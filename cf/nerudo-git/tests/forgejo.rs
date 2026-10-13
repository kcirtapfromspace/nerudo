//! Smart HTTP against a real server (the cluster's Forgejo). Skipped unless
//! NERUDO_FORGEJO_URL and NERUDO_FORGEJO_TOKEN are set.
use nerudo_git::{fetch, ls_refs, push, write_pack, FileEntry, RefUpdate, Remote, Signature, Store, Transport, MODE_BLOB};
use std::io::{Read, Write};
use std::net::TcpStream;

/// Minimal HTTP/1.1 client over a socket (http:// only), enough for tests.
struct Plain;

impl Transport for Plain {
    async fn request(&self, method: &str, url: &str, headers: &[(&str, String)], body: Option<Vec<u8>>) -> nerudo_git::Result<(u16, Vec<u8>)> {
        let rest = url.strip_prefix("http://").expect("http only");
        let (hostport, path) = rest.split_once('/').map(|(h, p)| (h, format!("/{p}"))).unwrap_or((rest, "/".into()));
        let mut s = TcpStream::connect(hostport).map_err(|e| nerudo_git::Error::Transport(e.to_string()))?;
        let body = body.unwrap_or_default();
        let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {hostport}\r\nConnection: close\r\nContent-Length: {}\r\n", body.len());
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        s.write_all(req.as_bytes()).unwrap();
        s.write_all(&body).unwrap();
        let mut raw = Vec::new();
        s.read_to_end(&mut raw).unwrap();
        let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8_lossy(&raw[..split]).to_string();
        let status: u16 = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        let mut body = raw[split + 4..].to_vec();
        if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
            let mut out = Vec::new();
            let mut i = 0;
            loop {
                let nl = body[i..].windows(2).position(|w| w == b"\r\n").unwrap() + i;
                let n = usize::from_str_radix(std::str::from_utf8(&body[i..nl]).unwrap().split(';').next().unwrap().trim(), 16).unwrap();
                if n == 0 { break; }
                out.extend_from_slice(&body[nl + 2..nl + 2 + n]);
                i = nl + 2 + n + 2;
            }
            body = out;
        }
        Ok((status, body))
    }
}

fn b64(s: &str) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let b = s.as_bytes();
    let mut o = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        o.push(T[(n >> 18) as usize & 63] as char);
        o.push(T[(n >> 12) as usize & 63] as char);
        o.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        o.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    o
}

#[test]
fn ls_refs_fetch_and_compare_and_swap_push() {
    let (Ok(base), Ok(token)) = (std::env::var("NERUDO_FORGEJO_URL"), std::env::var("NERUDO_FORGEJO_TOKEN")) else {
        eprintln!("skipped: set NERUDO_FORGEJO_URL / NERUDO_FORGEJO_TOKEN");
        return;
    };
    let repo = std::env::var("NERUDO_FORGEJO_REPO").expect("NERUDO_FORGEJO_REPO (owner/name, seeded with a main branch)");
    let remote = Remote::new(&format!("{base}/{repo}.git"), Some(format!("Basic {}", b64(&format!("bench:{token}")))));
    futures::executor::block_on(async {
        let t = Plain;
        let refs = ls_refs(&t, &remote, "refs/heads/").await.unwrap();
        let main = refs.iter().find(|(n, _)| n == "refs/heads/main").expect("main").1;

        let mut store = Store::new();
        let n = fetch(&t, &remote, &[main], &[], &mut store).await.unwrap();
        assert!(n > 0);
        let head = store.commit(&main).unwrap();
        let mut files = store.flatten(&head.tree).unwrap();

        // New commit on top of main.
        let blob = store.put_blob(format!("written by nerudo-git at {main}\n").as_bytes());
        files.insert(format!("nerudo/{}.txt", &main.to_hex()[..8]), FileEntry { mode: MODE_BLOB, oid: blob });
        let tree = store.write_tree(&files);
        let sig = Signature::new("nerudo", "nerudo@bench.invalid", 1_791_400_000);
        let c = nerudo_git::Commit { tree, parents: vec![main], author: sig.clone(), committer: sig, extra: vec![], message: "nerudo-git push test\n".into() };
        let new = store.put_commit(&c);
        let missing = store.missing_from(&new, &[main]).unwrap();
        assert!(missing.contains(&new) && missing.contains(&blob) && missing.contains(&tree));
        let pack = write_pack(&store.object_list(&missing).unwrap());

        let ok = push(&t, &remote, &[RefUpdate { name: "refs/heads/main".into(), old: main, new }], pack).await.unwrap();
        assert!(ok.all_ok(), "first push should land: {ok:?}");

        // A second writer that still believes main is at the old commit: its update must be
        // rejected, because the server's ref moved. This is the compare-and-swap.
        let blob2 = store.put_blob(b"a competing writer\n");
        let mut files2 = store.flatten(&head.tree).unwrap();
        files2.insert("nerudo/competing.txt".into(), FileEntry { mode: MODE_BLOB, oid: blob2 });
        let tree2 = store.write_tree(&files2);
        let sig2 = Signature::new("other", "other@bench.invalid", 1_791_400_001);
        let rival = store.put_commit(&nerudo_git::Commit { tree: tree2, parents: vec![main], author: sig2.clone(), committer: sig2, extra: vec![], message: "rival\n".into() });
        let pack2 = write_pack(&store.object_list(&store.missing_from(&rival, &[main]).unwrap()).unwrap());
        let stale = push(&t, &remote, &[RefUpdate { name: "refs/heads/main".into(), old: main, new: rival }], pack2).await.unwrap();
        assert!(!stale.all_ok(), "stale old-oid must be rejected: {stale:?}");
        eprintln!("CAS rejection reported by server: {:?}", stale.refs);

        let after = ls_refs(&t, &remote, "refs/heads/main").await.unwrap();
        assert_eq!(after[0].1, new);
    });
}

