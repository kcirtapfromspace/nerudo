//! Cross-check the pack reader/writer against the real git CLI.
use nerudo_git::{parse_pack, write_pack, Kind, Object, Oid, Store};
use std::io::Write;
use std::process::{Command, Stdio};

fn git(dir: &std::path::Path, args: &[&str]) -> String {
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
    String::from_utf8(out.stdout).unwrap()
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("nerudo-git-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A repo whose history makes git emit deltas (a large file edited a few times).
fn repo_with_history(name: &str) -> std::path::PathBuf {
    let d = tmp(name);
    git(&d, &["init", "-q", "-b", "main"]);
    let big: String = (0..4000).map(|i| format!("line {i} of a reasonably long file\n")).collect();
    for rev in 0..4 {
        std::fs::create_dir_all(d.join("src/nested")).unwrap();
        std::fs::write(d.join("src/nested/big.txt"), format!("{big}rev {rev}\n")).unwrap();
        std::fs::write(d.join("README.md"), format!("readme {rev}\n")).unwrap();
        git(&d, &["add", "-A"]);
        git(&d, &["commit", "-q", "-m", &format!("rev {rev}")]);
    }
    d
}

#[test]
fn reads_git_pack_with_ofs_and_ref_deltas() {
    let d = repo_with_history("read");
    for flags in [vec!["--delta-base-offset"], vec![]] {
        // --delta-base-offset gives OFS_DELTA; without it git uses REF_DELTA.
        let revs = git(&d, &["rev-list", "--objects", "main"]);
        let mut args = vec!["pack-objects", "--stdout", "-q"];
        args.extend(flags.iter().copied());
        let mut child = Command::new("git").args(&args).current_dir(&d).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(revs.as_bytes()).unwrap();
        let pack = child.wait_with_output().unwrap().stdout;
        let objects = parse_pack(&pack, None).expect("parse git pack");
        let expected: Vec<&str> = revs.lines().map(|l| &l[..40]).collect();
        assert_eq!(objects.len(), expected.len());
        let mut store = Store::new();
        for o in objects {
            store.insert(o);
        }
        for e in &expected {
            assert!(store.contains(&Oid::from_hex(e).unwrap()), "missing {e} ({flags:?})");
        }
        // Flatten HEAD's tree and compare with git ls-tree.
        let head = Oid::from_hex(&git(&d, &["rev-parse", "main"])).unwrap();
        let files = store.flatten(&store.commit(&head).unwrap().tree).unwrap();
        let ls = git(&d, &["ls-tree", "-r", "main"]);
        assert_eq!(files.len(), ls.lines().count());
        for line in ls.lines() {
            let (meta, path) = line.split_once('\t').unwrap();
            let oid = meta.split_whitespace().nth(2).unwrap();
            assert_eq!(files[path].oid.to_hex(), oid);
        }
    }
}

#[test]
fn written_pack_is_accepted_by_git_and_trees_round_trip() {
    let d = repo_with_history("write");
    // Read everything, rebuild the head tree from its flat form, and check the ids match.
    let revs = git(&d, &["rev-list", "--objects", "main"]);
    let mut child = Command::new("git").args(["pack-objects", "--stdout", "-q", "--delta-base-offset"]).current_dir(&d)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(revs.as_bytes()).unwrap();
    let mut store = Store::new();
    for o in parse_pack(&child.wait_with_output().unwrap().stdout, None).unwrap() {
        store.insert(o);
    }
    let head = Oid::from_hex(&git(&d, &["rev-parse", "main"])).unwrap();
    let tree = store.commit(&head).unwrap().tree;
    let flat = store.flatten(&tree).unwrap();
    assert_eq!(store.write_tree(&flat), tree, "write_tree must reproduce git's tree id");

    // A brand-new commit, packed by us, must pass `git index-pack --strict` in a fresh repo.
    let blob = store.put_blob(b"hello from nerudo\n");
    let mut flat2 = flat.clone();
    flat2.insert("added/by/nerudo.txt".into(), nerudo_git::FileEntry { mode: nerudo_git::MODE_BLOB, oid: blob });
    let t2 = store.write_tree(&flat2);
    let mut c = store.commit(&head).unwrap();
    c.tree = t2;
    c.parents = vec![head];
    c.message = "nerudo commit\n\nChange-Id: abc123\n".into();
    let c2 = store.put_commit(&c);
    let all = store.missing_from(&c2, &[]).unwrap();
    let objs: Vec<&Object> = store.object_list(&all).unwrap();
    let pack = write_pack(&objs);
    let fresh = tmp("fresh");
    git(&fresh, &["init", "-q", "--bare"]);
    let mut ip = Command::new("git").args(["index-pack", "--strict", "--stdin"]).current_dir(&fresh)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    ip.stdin.take().unwrap().write_all(&pack).unwrap();
    let out = ip.wait_with_output().unwrap();
    assert!(out.status.success(), "index-pack: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(git(&fresh, &["cat-file", "-t", &c2.to_hex()]).trim(), "commit");
    let msg = git(&fresh, &["log", "-1", "--format=%B", &c2.to_hex()]);
    assert!(msg.contains("Change-Id: abc123"));
    assert_eq!(store.commit(&c2).unwrap().trailer("Change-Id").as_deref(), Some("abc123"));
    // Our blob id matches git's.
    let mut hb = Command::new("git").args(["hash-object", "--stdin"]).current_dir(&fresh).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    hb.stdin.take().unwrap().write_all(b"x").unwrap();
    let gid = String::from_utf8(hb.wait_with_output().unwrap().stdout).unwrap();
    assert_eq!(Object::new(Kind::Blob, b"x".to_vec()).oid().to_hex(), gid.trim());
}
