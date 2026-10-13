//! The sequencer holds trunk's trees but few file contents (filter blob:none). With the
//! files `blobs_needed` names, gates and landings give what they give with every file.
use nerudo_core::merge::{blobs_needed, digest_index, edits, footprint};
use nerudo_core::{apply_change, check_files, seal_boundaries, trunk_checks, ChangeInput, Ejection, Rules};
use nerudo_git::{Commit, FileEntry, Kind, Object, Oid, Signature, Store, MODE_BLOB};
use std::collections::BTreeMap;

const RULES: &str = include_str!("../../fixtures/bench-nerudo.toml");

fn sig(n: &str) -> Signature {
    Signature::new(n, &format!("{n}@bench.invalid"), 1_791_400_000)
}

fn seed(store: &mut Store) -> Oid {
    let mut texts: Vec<(String, String)> = vec![
        ("nerudo.toml".into(), RULES.into()),
        ("targets/t00/src.txt".into(), "a\nb\nc\nd\ne\n".into()),
        ("targets/t01/src.txt".into(), "x\ny\nz\n".into()),
        ("CHANGELOG.md".into(), "# Changelog\n".into()),
        ("deps/base.txt".into(), "base==1.0\n".into()),
        ("migrations/0001_init.sql".into(), "create table users(id int);\n".into()),
        ("tests/test_money.py".into(), "import unittest\n\nclass T(unittest.TestCase):\n    def test_one(self):\n        self.assertEqual(1, 1)\n".into()),
        ("ci/check.py".into(), "print('ok')\n".into()),
    ];
    texts.extend((0..50).map(|i| (format!("big/pkg{i:02}/file.txt"), format!("untouched {i}\n"))));
    let mut files: BTreeMap<String, FileEntry> = texts.iter().map(|(p, t)| (p.clone(), FileEntry { mode: MODE_BLOB, oid: store.put_blob(t.as_bytes()) })).collect();
    let lock = digest_index(store, &files, "deps").unwrap();
    files.insert("deps.lock".into(), FileEntry { mode: MODE_BLOB, oid: store.put_blob(lock.as_bytes()) });
    let tree = store.write_tree(&files);
    store.put_commit(&Commit { tree, parents: vec![], author: sig("seed"), committer: sig("seed"), extra: vec![], message: "seed\n".into() })
}

fn change(store: &mut Store, base: Oid, id: &str, edits: &[(&str, Option<&str>)]) -> ChangeInput {
    let mut flat = store.flatten(&store.commit(&base).unwrap().tree).unwrap();
    for (p, c) in edits {
        match c {
            Some(t) => {
                let oid = store.put_blob(t.as_bytes());
                flat.insert(p.to_string(), FileEntry { mode: MODE_BLOB, oid });
            }
            None => {
                flat.remove(*p);
            }
        }
    }
    let t = store.write_tree(&flat);
    let c = Commit { tree: t, parents: vec![base], author: sig(id), committer: sig(id), extra: vec![], message: format!("{id}: change\n") };
    ChangeInput { change_id: id.into(), rev: 1, commit: store.put_commit(&c), base, trailers: vec![] }
}

/// Every commit and tree `tip` reaches, and no file contents: a fetch with filter blob:none.
fn trees_of(full: &Store, into: &mut Store, tip: &Oid) {
    for oid in full.missing_from(tip, &[]).unwrap() {
        let o = full.get(&oid).unwrap();
        if o.kind != Kind::Blob {
            into.insert(Object::new(o.kind, o.data.clone()));
        }
    }
}

fn copy(full: &Store, into: &mut Store, oids: &[Oid]) {
    for oid in oids {
        let o = full.get(oid).unwrap();
        into.insert(Object::new(o.kind, o.data.clone()));
    }
}

#[test]
fn a_store_with_trees_and_the_needed_files_lands_the_same_commit() {
    let mut full = Store::new();
    let rules = Rules::parse(RULES).unwrap();
    let root = seed(&mut full);
    // Trunk moves after b's base: a edits the first line of t00 and appends to CHANGELOG.
    let a = change(&mut full, root, "a01", &[("targets/t00/src.txt", Some("A\nb\nc\nd\ne\n")), ("CHANGELOG.md", Some("# Changelog\n- a01\n"))]);
    let head = apply_change(&mut full, &rules, root, &a, &sig("nerudo")).unwrap().commit;
    // b, on the old base: the last line of t00 (a line merge), a test, a dependency, a
    // pending migration, and the changelog.
    let b = change(&mut full, root, "b01", &[
        ("targets/t00/src.txt", Some("a\nb\nc\nd\nE\n")),
        ("tests/test_money.py", Some("import unittest\n\nclass T(unittest.TestCase):\n    def test_one(self):\n        self.assertEqual(1, 2)\n")),
        ("deps/b01.txt", Some("b01==1.0\n")),
        ("migrations/NEXT_b01.sql", Some("drop table users;\n")),
        ("CHANGELOG.md", Some("# Changelog\n- b01\n")),
    ]);

    // What the sequencer holds: head's trees, the base's trees, the change's trees.
    let mut part = Store::new();
    trees_of(&full, &mut part, &head);
    trees_of(&full, &mut part, &b.commit);
    assert_eq!(footprint(&part, &rules, &b).unwrap(), footprint(&full, &rules, &b).unwrap(), "the footprint needs trees only");
    assert_eq!(edits(&part, &b).unwrap(), edits(&full, &b).unwrap());

    // Without the files, a gate is an error, never a pass, and a merge is never a conflict.
    assert!(seal_boundaries(&part, &rules, &b).is_err(), "a ratchet must not pass on a file it cannot read");
    let mut scratch = part.clone();
    let e = apply_change(&mut scratch, &rules, head, &b, &sig("nerudo")).unwrap_err();
    assert!(matches!(e, Ejection::Missing { .. }), "{e:?}");

    let (ours, theirs) = blobs_needed(&part, &rules, &head, &b).unwrap();
    copy(&full, &mut part, &ours);
    copy(&full, &mut part, &theirs);
    assert_eq!(blobs_needed(&part, &rules, &head, &b).unwrap(), (vec![], vec![]));
    // Only what the change reads: not the 50 untouched packages.
    assert!(ours.len() + theirs.len() < 20, "{} files", ours.len() + theirs.len());

    assert_eq!(seal_boundaries(&part, &rules, &b).unwrap(), seal_boundaries(&full, &rules, &b).unwrap());
    let want = apply_change(&mut full, &rules, head, &b, &sig("nerudo")).unwrap();
    let got = apply_change(&mut part, &rules, head, &b, &sig("nerudo")).unwrap();
    assert_eq!(got.commit, want.commit, "the same landed commit, byte for byte");
    assert_eq!((got.allocated, got.appended, got.regenerated), (want.allocated, want.appended, want.regenerated));

    let all = full.flatten(&full.commit(&want.commit).unwrap().tree).unwrap();
    let some = check_files(&part, &rules, &got.tree).unwrap();
    assert!(some.len() < all.len() / 2, "{} of {} files", some.len(), all.len());
    assert_eq!(trunk_checks(&part, &rules, &some), trunk_checks(&full, &rules, &all));

    // The push: what the remote at head lacks, from the partial store.
    let pushed = part.new_objects(&got.commit, &head).unwrap();
    let missing = full.missing_from(&want.commit, &[head]).unwrap();
    assert!(missing.iter().all(|o| pushed.contains(o)), "the pack holds every object trunk lacks");
}

#[test]
fn check_files_names_a_file_it_cannot_read() {
    let mut full = Store::new();
    let rules = Rules::parse(RULES).unwrap();
    let root = seed(&mut full);
    let mut part = Store::new();
    trees_of(&full, &mut part, &root);
    let tree = part.commit(&root).unwrap().tree;
    let Err(nerudo_git::Error::Missing(_)) = check_files(&part, &rules, &tree) else { panic!("no missing file named") };
}

#[test]
fn a_plain_edit_on_head_needs_no_base_file() {
    // The usual change: built on head, it edits a file that no rule reads. The landing takes
    // the change's file as it is, so the base's version is never read.
    let mut full = Store::new();
    let rules = Rules::parse(RULES).unwrap();
    let root = seed(&mut full);
    let c = change(&mut full, root, "c01", &[("big/pkg07/file.txt", Some("edited 7\n")), ("targets/t01/src.txt", None)]);
    let mut part = Store::new();
    trees_of(&full, &mut part, &root);
    trees_of(&full, &mut part, &c.commit);
    let tree = full.commit(&c.commit).unwrap().tree;
    let edited = full.file(&tree, "big/pkg07/file.txt").unwrap().unwrap();
    let before = full.file(&full.commit(&root).unwrap().tree, "big/pkg07/file.txt").unwrap().unwrap();
    let (ours, theirs) = blobs_needed(&part, &rules, &root, &c).unwrap();
    assert!(ours.contains(&edited.oid), "the change's own file is pushed, so it is needed");
    assert!(!ours.contains(&before.oid), "the base's version of a plain edit is not read");
    copy(&full, &mut part, &ours);
    copy(&full, &mut part, &theirs);
    let want = apply_change(&mut full, &rules, root, &c, &sig("nerudo")).unwrap();
    let got = apply_change(&mut part, &rules, root, &c, &sig("nerudo")).unwrap();
    assert_eq!(got.commit, want.commit);
    assert!(!part.contains(&before.oid));
}
