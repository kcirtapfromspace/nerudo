//! One fetch for several commits (`fetch_body`, `fetch_pack`, `ingest`), against the real
//! `git upload-pack` in protocol v2, as the sequencer's gate uses it for one fork.
use nerudo_git::{fetch, fetch_body, fetch_pack, ingest, FetchOpts, Oid, Remote, Store, Transport};
use std::cell::Cell;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t.invalid")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "commit.gpgsign")
        .env("GIT_CONFIG_VALUE_0", "false")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Smart HTTP without HTTP: each POST to git-upload-pack runs `git upload-pack
/// --stateless-rpc` on the repo, as `git http-backend` does.
struct Local {
    repo: PathBuf,
    requests: Cell<u32>,
}

impl Transport for Local {
    async fn request(&self, method: &str, url: &str, _headers: &[(&str, String)], body: Option<Vec<u8>>) -> nerudo_git::Result<(u16, Vec<u8>)> {
        assert_eq!((method, url.ends_with("/git-upload-pack")), ("POST", true));
        self.requests.set(self.requests.get() + 1);
        let mut child = Command::new("git")
            .args(["upload-pack", "--stateless-rpc", "."])
            .current_dir(&self.repo)
            .env("GIT_PROTOCOL", "version=2")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&body.unwrap_or_default()).unwrap();
        let out = child.wait_with_output().unwrap();
        Ok((200, out.stdout))
    }
}

fn oid(hex: &str) -> Oid {
    Oid::from_hex(hex).unwrap()
}

/// A repo with a base commit and one branch per change, each adding its own file.
fn repo(name: &str, changes: usize) -> (PathBuf, Oid, Vec<Oid>) {
    let d = std::env::temp_dir().join(format!("nerudo-git-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    git(&d, &["init", "-q", "-b", "main"]);
    std::fs::write(d.join("README.md"), "base\n").unwrap();
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-q", "-m", "base"]);
    let base = oid(&git(&d, &["rev-parse", "HEAD"]));
    let mut tips = vec![];
    for i in 0..changes {
        git(&d, &["switch", "-q", "-c", &format!("land/c{i}"), "main"]);
        std::fs::create_dir_all(d.join("changes")).unwrap();
        std::fs::write(d.join("changes").join(format!("c{i}.txt")), format!("c{i}\n")).unwrap();
        git(&d, &["add", "-A"]);
        git(&d, &["commit", "-q", "-m", &format!("c{i}")]);
        tips.push(oid(&git(&d, &["rev-parse", "HEAD"])));
    }
    git(&d, &["switch", "-q", "main"]);
    (d, base, tips)
}

#[test]
fn one_request_fetches_every_commit_of_a_fork() {
    let (dir, base, tips) = repo("fetch-many", 5);
    let t = Local { repo: dir.clone(), requests: Cell::new(0) };
    let remote = Remote::new("http://fork.invalid/repo.git", None);
    let mut store = Store::default();
    futures::executor::block_on(fetch(&t, &remote, &[base], &[], &mut store)).unwrap();
    assert_eq!(t.requests.get(), 1);

    let body = fetch_body(&tips, &[base], FetchOpts::default(), &store).expect("the store lacks the tips");
    let pack = futures::executor::block_on(fetch_pack(&t, &remote, body)).unwrap();
    let n = ingest(&pack, &mut store).unwrap();
    assert_eq!(t.requests.get(), 2, "one request for all five commits");
    for tip in &tips {
        let c = store.commit(tip).unwrap();
        assert_eq!(c.parents, vec![base]);
        assert!(store.flatten(&c.tree).unwrap().len() == 2, "README.md and its own change file");
    }
    // The base was a have: the pack holds each change's commit, root tree, changes/ tree and
    // blob, not the base's objects.
    assert_eq!(n, 5 * 4);
    assert!(fetch_body(&tips, &[base], FetchOpts::default(), &store).is_none(), "nothing left to fetch");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_commit_the_fork_lacks_fails_the_whole_request() {
    let (dir, base, tips) = repo("fetch-missing", 2);
    let t = Local { repo: dir.clone(), requests: Cell::new(0) };
    let remote = Remote::new("http://fork.invalid/repo.git", None);
    let mut store = Store::default();
    futures::executor::block_on(fetch(&t, &remote, &[base], &[], &mut store)).unwrap();
    let missing = oid("0123456789abcdef0123456789abcdef01234567");
    let body = fetch_body(&[tips[0], missing, tips[1]], &[base], FetchOpts::default(), &store).unwrap();
    assert!(futures::executor::block_on(fetch_pack(&t, &remote, body)).is_err());
    assert!(!store.contains(&tips[0]) && !store.contains(&tips[1]));
    // So the gate asks again one commit at a time, and the good ones still arrive.
    for tip in &tips {
        futures::executor::block_on(fetch(&t, &remote, &[*tip], &[base], &mut store)).unwrap();
        assert!(store.contains(tip));
    }
    let _ = std::fs::remove_dir_all(dir);
}
