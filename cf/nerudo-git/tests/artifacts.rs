//! Compare-and-swap on a live Cloudflare Artifacts repo. Skipped unless
//! NERUDO_ARTIFACTS_REMOTE (https smart-HTTP URL of a repo with a `main` branch) and
//! NERUDO_ARTIFACTS_TOKEN (write-scoped token) are set.
use nerudo_git::{fetch, ls_refs, push, write_pack, Commit, FileEntry, Oid, RefUpdate, Remote, Signature, Store, Transport, MODE_BLOB};
use std::io::Read;
use std::sync::{Arc, Barrier};

/// HTTPS over ureq. 4xx/5xx come back as (status, body), like the Worker's fetch transport.
struct Https;

impl Transport for Https {
    async fn request(&self, method: &str, url: &str, headers: &[(&str, String)], body: Option<Vec<u8>>) -> nerudo_git::Result<(u16, Vec<u8>)> {
        let mut req = ureq::request(method, url);
        for (k, v) in headers {
            req = req.set(k, v);
        }
        let resp = match body {
            Some(b) => req.send_bytes(&b),
            None => req.call(),
        };
        let resp = match resp {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(e) => return Err(nerudo_git::Error::Transport(e.to_string())),
        };
        let status = resp.status();
        let mut out = Vec::new();
        resp.into_reader().read_to_end(&mut out).map_err(|e| nerudo_git::Error::Transport(e.to_string()))?;
        Ok((status, out))
    }
}

const MAIN: &str = "refs/heads/main";

fn remote() -> Option<Remote> {
    let (Ok(url), Ok(token)) = (std::env::var("NERUDO_ARTIFACTS_REMOTE"), std::env::var("NERUDO_ARTIFACTS_TOKEN")) else {
        return None;
    };
    Some(Remote::new(&url, Some(format!("Bearer {token}"))))
}

async fn head(remote: &Remote) -> Oid {
    let refs = ls_refs(&Https, remote, MAIN).await.unwrap();
    refs.iter().find(|(n, _)| n == MAIN).expect("main").1
}

/// A new commit on `parent` adding one file, plus the pack the server needs for it.
fn child(store: &mut Store, parent: Oid, base: Oid, name: &str) -> (Oid, Vec<u8>) {
    let tree = store.commit(&parent).unwrap().tree;
    let mut files = store.flatten(&tree).unwrap();
    let blob = store.put_blob(format!("{name}\n").as_bytes());
    files.insert(format!("cas/{name}.txt"), FileEntry { mode: MODE_BLOB, oid: blob });
    let tree = store.write_tree(&files);
    let sig = Signature::new(name, "cas@bench.invalid", 1_791_400_000);
    let c = store.put_commit(&Commit { tree, parents: vec![parent], author: sig.clone(), committer: sig, extra: vec![], message: format!("{name}\n") });
    let pack = write_pack(&store.object_list(&store.missing_from(&c, &[base]).unwrap()).unwrap());
    (c, pack)
}

#[test]
fn artifacts_compare_and_swap() {
    let Some(remote) = remote() else {
        eprintln!("skipped: set NERUDO_ARTIFACTS_REMOTE / NERUDO_ARTIFACTS_TOKEN");
        return;
    };
    futures::executor::block_on(async {
        let start = head(&remote).await;
        let mut store = Store::new();
        fetch(&Https, &remote, &[start], &[], &mut store).await.unwrap();

        // 1. A push from the head we read lands.
        let (a, pack) = child(&mut store, start, start, "a");
        let r = push(&Https, &remote, &[RefUpdate { name: MAIN.into(), old: start, new: a }], pack).await.unwrap();
        assert!(r.all_ok(), "push from the current head must land: {r:?}");
        assert_eq!(head(&remote).await, a);

        // 2. A writer still holding `start` pushes a sibling of `a`: stale and non-fast-forward.
        let (rival, pack) = child(&mut store, start, start, "rival");
        let r = push(&Https, &remote, &[RefUpdate { name: MAIN.into(), old: start, new: rival }], pack).await.unwrap();
        assert!(!r.all_ok(), "stale non-fast-forward push must be rejected: {r:?}");
        eprintln!("case 2 (stale, non-fast-forward): {:?}", r.refs);
        assert_eq!(head(&remote).await, a);

        // 3. Stale old value, but the new commit descends from the real head. Only a server
        //    that checks the old value refuses this; one that checks fast-forward accepts it.
        let (b, pack) = child(&mut store, a, start, "b");
        let r = push(&Https, &remote, &[RefUpdate { name: MAIN.into(), old: start, new: b }], pack).await.unwrap();
        let after = head(&remote).await;
        eprintln!("case 3 (stale old value, fast-forward): ok={} refs={:?} head_is_b={}", r.all_ok(), r.refs, after == b);
        assert_eq!(r.all_ok(), after == b, "the report must match what happened to the ref");
        if !r.all_ok() {
            eprintln!("case 3 verdict: server checks the old value (true compare-and-swap)");
        } else {
            eprintln!("case 3 verdict: server accepted a stale old value (fast-forward check only)");
        }
    });
}

#[test]
fn artifacts_concurrent_writers_exactly_one_wins() {
    let Some(remote) = remote() else {
        eprintln!("skipped: set NERUDO_ARTIFACTS_REMOTE / NERUDO_ARTIFACTS_TOKEN");
        return;
    };
    const N: usize = 8;
    let base = futures::executor::block_on(head(&remote));
    let gate = Arc::new(Barrier::new(N));
    let threads: Vec<_> = (0..N)
        .map(|i| {
            let (remote, gate) = (remote.clone(), gate.clone());
            std::thread::spawn(move || {
                futures::executor::block_on(async {
                    let mut store = Store::new();
                    fetch(&Https, &remote, &[base], &[], &mut store).await.unwrap();
                    let (c, pack) = child(&mut store, base, base, &format!("racer-{i}"));
                    gate.wait();
                    let r = push(&Https, &remote, &[RefUpdate { name: MAIN.into(), old: base, new: c }], pack).await;
                    (c, r)
                })
            })
        })
        .collect();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    let winners: Vec<Oid> = results.iter().filter(|(_, r)| matches!(r, Ok(r) if r.all_ok())).map(|(c, _)| *c).collect();
    for (c, r) in &results {
        eprintln!("racer {}: {:?}", &c.to_hex()[..8], r.as_ref().map(|r| &r.refs));
    }
    let after = futures::executor::block_on(head(&remote));
    assert_eq!(winners.len(), 1, "exactly one of {N} racers holding the same old value may land");
    assert_eq!(after, winners[0], "trunk must be the winner's commit");
}
