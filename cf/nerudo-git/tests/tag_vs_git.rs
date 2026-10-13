//! An annotated tag written by nerudo-git is the object git writes, and git reads it back.
use nerudo_git::{Kind, Oid, Signature, Store, Tag};
use std::io::Write;
use std::process::{Command, Stdio};

fn git(dir: &std::path::Path, args: &[&str], input: Option<&[u8]>) -> String {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t.invalid")
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", "commit.gpgsign")
        .env("GIT_CONFIG_VALUE_0", "false")
        .env("GIT_CONFIG_KEY_1", "tag.gpgsign")
        .env("GIT_CONFIG_VALUE_1", "false")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(i) = input {
        child.stdin.take().unwrap().write_all(i).unwrap();
    }
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn tag_object_matches_git() {
    let d = std::env::temp_dir().join(format!("nerudo-git-tag-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    git(&d, &["init", "-q", "-b", "main"], None);
    std::fs::write(d.join("a.txt"), "a\n").unwrap();
    git(&d, &["add", "-A"], None);
    git(&d, &["commit", "-q", "-m", "first"], None);
    let head = git(&d, &["rev-parse", "HEAD"], None).trim().to_string();

    let tag = Tag {
        object: Oid::from_hex(&head).unwrap(),
        kind: Kind::Commit,
        name: "v0.1.0".into(),
        tagger: Some(Signature::new("you", "you@nerudo.invalid", 1_790_000_000)),
        message: "v0.1.0\n\n## Features\n- a thing\n\nReleased-By: you abc 1 sig\nRelease-Seq: 3\n".into(),
    };
    let mut store = Store::new();
    let oid = store.put_tag(&tag);
    // git mktag checks the object's format and writes it; the id must be the same.
    let theirs = git(&d, &["mktag"], Some(&tag.serialize())).trim().to_string();
    assert_eq!(oid.to_hex(), theirs);
    // And it reads back the same, trailers included.
    let back = store.tag(&oid).unwrap();
    assert_eq!(back, tag);
    assert_eq!(back.trailer("Release-Seq").as_deref(), Some("3"));
    assert_eq!(back.trailers("Released-By"), vec!["you abc 1 sig".to_string()]);
    assert_eq!(git(&d, &["cat-file", "-t", &theirs], None).trim(), "tag");
    let _ = std::fs::remove_dir_all(&d);
}
