//! The tree operations that read only what changed (diff, edit_tree, new_objects, file,
//! flatten_dir) give the same results as the ones that flatten the whole tree.
use nerudo_git::{Commit, FileEntry, Kind, Object, Oid, Signature, Store, MODE_BLOB, MODE_EXEC, MODE_GITLINK};
use std::collections::{BTreeMap, BTreeSet, HashSet};

type Flat = BTreeMap<String, FileEntry>;

/// xorshift: the same cases on every run, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Names that sort differently as files and as directories ("a.b" < "a/" < "a0").
const NAMES: [&str; 7] = ["a", "a.b", "a-b", "a0", "b", "c", "z"];

fn random_path(r: &mut Rng) -> String {
    let depth = 1 + r.below(3);
    (0..depth).map(|_| NAMES[r.below(NAMES.len())]).collect::<Vec<_>>().join("/")
}

/// A path can go in a tree if no file is a directory of it, and it is no directory of a file.
fn fits(files: &Flat, path: &str) -> bool {
    !files.keys().any(|k| k == path || path.starts_with(&format!("{k}/")) || k.starts_with(&format!("{path}/")))
}

fn blob(store: &mut Store, r: &mut Rng) -> FileEntry {
    let mode = [MODE_BLOB, MODE_BLOB, MODE_EXEC][r.below(3)];
    FileEntry { mode, oid: store.put_blob(format!("content {}\n", r.next()).as_bytes()) }
}

fn random_flat(store: &mut Store, r: &mut Rng, n: usize) -> Flat {
    let mut files = Flat::new();
    for _ in 0..n {
        let p = random_path(r);
        if fits(&files, &p) {
            let e = blob(store, r);
            files.insert(p, e);
        }
    }
    files
}

/// Edits that keep the tree valid: changed and removed files, new files, a directory that
/// becomes a file and a file that becomes a directory.
fn random_edits(store: &mut Store, r: &mut Rng, files: &Flat) -> BTreeMap<String, Option<FileEntry>> {
    let mut after = files.clone();
    let mut edits = BTreeMap::new();
    for p in files.keys() {
        match r.below(6) {
            0 => {
                after.remove(p);
                edits.insert(p.clone(), None);
            }
            1 => {
                let e = blob(store, r);
                after.insert(p.clone(), e);
                edits.insert(p.clone(), Some(e));
            }
            _ => {}
        }
    }
    for _ in 0..4 {
        let p = random_path(r);
        // A directory that becomes a file: remove what is under it first.
        if r.below(3) == 0 {
            let under: Vec<String> = after.keys().filter(|k| k.starts_with(&format!("{p}/"))).cloned().collect();
            for k in under {
                after.remove(&k);
                edits.insert(k, None);
            }
        }
        // A file that becomes a directory.
        if r.below(3) == 0 {
            if let Some((k, _)) = after.iter().find(|(k, _)| p.starts_with(&format!("{k}/"))).map(|(k, v)| (k.clone(), *v)) {
                after.remove(&k);
                edits.insert(k, None);
            }
        }
        if fits(&after, &p) {
            let e = blob(store, r);
            after.insert(p.clone(), e);
            edits.insert(p, Some(e));
        }
    }
    edits
}

fn apply(files: &Flat, edits: &BTreeMap<String, Option<FileEntry>>) -> Flat {
    let mut out = files.clone();
    for (p, e) in edits {
        match e {
            Some(e) => out.insert(p.clone(), *e),
            None => out.remove(p),
        };
    }
    out
}

fn flat_diff(a: &Flat, b: &Flat) -> Vec<(String, Option<FileEntry>, Option<FileEntry>)> {
    let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    keys.into_iter().filter(|k| a.get(*k) != b.get(*k)).map(|k| (k.clone(), a.get(k).copied(), b.get(k).copied())).collect()
}

#[test]
fn diff_is_the_flattened_difference() {
    let mut r = Rng(0x9e3779b97f4a7c15);
    for case in 0..400 {
        let mut store = Store::new();
        let a = { let n = 1 + r.below(30); random_flat(&mut store, &mut r, n) };
        let b = apply(&a, &random_edits(&mut store, &mut r, &a));
        let (ta, tb) = (store.write_tree(&a), store.write_tree(&b));
        assert_eq!(store.diff(Some(&ta), Some(&tb)).unwrap(), flat_diff(&a, &b), "case {case}");
        assert_eq!(store.diff(None, Some(&tb)).unwrap(), flat_diff(&Flat::new(), &b), "case {case} from nothing");
        assert_eq!(store.diff(Some(&ta), Some(&ta)).unwrap(), vec![], "case {case} same tree");
    }
}

#[test]
fn edit_tree_is_write_tree_of_the_edited_files() {
    let mut r = Rng(0x2545f4914f6cdd1d);
    for case in 0..400 {
        let mut store = Store::new();
        let a = { let n = 1 + r.below(30); random_flat(&mut store, &mut r, n) };
        let edits = random_edits(&mut store, &mut r, &a);
        let ta = store.write_tree(&a);
        let want = store.write_tree(&apply(&a, &edits));
        assert_eq!(store.edit_tree(Some(&ta), &edits).unwrap(), want, "case {case}");
        // Every file removed: the empty tree, as write_tree makes it.
        let all: BTreeMap<String, Option<FileEntry>> = a.keys().map(|k| (k.clone(), None)).collect();
        assert_eq!(store.edit_tree(Some(&ta), &all).unwrap(), store.write_tree(&Flat::new()), "case {case} all removed");
    }
}

#[test]
fn edit_tree_refuses_a_file_that_is_a_directory() {
    let mut store = Store::new();
    let e = FileEntry { mode: MODE_BLOB, oid: store.put_blob(b"x") };
    let t = store.write_tree(&Flat::from([("a/b".to_string(), e)]));
    assert!(store.edit_tree(Some(&t), &BTreeMap::from([("a".to_string(), Some(e))])).is_err());
    let t = store.write_tree(&Flat::from([("a".to_string(), e)]));
    assert!(store.edit_tree(Some(&t), &BTreeMap::from([("a/b".to_string(), Some(e))])).is_err());
}

#[test]
fn file_and_flatten_dir_read_one_path() {
    let mut r = Rng(0xd1b54a32d192ed03);
    for _ in 0..100 {
        let mut store = Store::new();
        let a = { let n = 1 + r.below(30); random_flat(&mut store, &mut r, n) };
        let t = store.write_tree(&a);
        for _ in 0..10 {
            let p = random_path(&mut r);
            assert_eq!(store.file(&t, &p).unwrap(), a.get(&p).copied(), "{p}");
            let prefix = format!("{p}/");
            let want: Flat = a.iter().filter(|(k, _)| k.starts_with(&prefix)).map(|(k, v)| (k.clone(), *v)).collect();
            assert_eq!(store.flatten_dir(&t, &p).unwrap(), want, "{p}/");
        }
        assert_eq!(store.flatten_dir(&t, "").unwrap(), a);
    }
}

fn commit(store: &mut Store, tree: Oid, parents: Vec<Oid>, n: i64) -> Oid {
    let s = Signature::new("t", "t@t.invalid", 1_700_000_000 + n);
    store.put_commit(&Commit { tree, parents, author: s.clone(), committer: s, extra: vec![], message: format!("c{n}\n") })
}

#[test]
fn new_objects_holds_what_the_remote_lacks() {
    let mut r = Rng(0xbf58476d1ce4e5b9);
    for case in 0..200 {
        let mut store = Store::new();
        let mut files = { let n = 1 + r.below(30); random_flat(&mut store, &mut r, n) };
        let t = store.write_tree(&files);
        let mut head = commit(&mut store, t, vec![], 0);
        // A gitlink rides along as a file; it is never an object to send.
        files.insert("sub".into(), FileEntry { mode: MODE_GITLINK, oid: Oid::from_hex(&"1".repeat(40)).unwrap() });
        let stop = head;
        for n in 1..=1 + r.below(4) {
            files = apply(&files, &random_edits(&mut store, &mut r, &files));
            let t = store.write_tree(&files);
            head = commit(&mut store, t, vec![head], n as i64);
        }
        let want: HashSet<Oid> = store.missing_from(&head, &[stop]).unwrap().into_iter().collect();
        let got: HashSet<Oid> = store.new_objects(&head, &stop).unwrap().into_iter().collect();
        assert!(want.is_subset(&got), "case {case}: new_objects lacks {:?}", want.difference(&got).collect::<Vec<_>>());
        let reach: HashSet<Oid> = store.missing_from(&head, &[]).unwrap().into_iter().collect();
        assert!(got.is_subset(&reach), "case {case}: new_objects sends what tip does not reach");
    }
}

#[test]
fn new_objects_leaves_out_what_the_store_lacks() {
    // The sequencer holds trunk's trees but not its files (filter blob:none). A change that
    // keeps a file it never read must not need that file to be pushed.
    let mut full = Store::new();
    let kept = FileEntry { mode: MODE_BLOB, oid: full.put_blob(b"kept, never fetched\n") };
    let edited = FileEntry { mode: MODE_BLOB, oid: full.put_blob(b"old\n") };
    let base = Flat::from([("dir/kept.txt".to_string(), kept), ("dir/edited.txt".to_string(), edited)]);
    let t0 = full.write_tree(&base);
    let c0 = commit(&mut full, t0, vec![], 0);
    let mut trees_only = Store::new();
    for oid in full.missing_from(&c0, &[]).unwrap() {
        let o = full.get(&oid).unwrap();
        if o.kind != Kind::Blob {
            trees_only.insert(Object::new(o.kind, o.data.clone()));
        }
    }
    let new = FileEntry { mode: MODE_BLOB, oid: trees_only.put_blob(b"new\n") };
    let t1 = trees_only.edit_tree(Some(&t0), &BTreeMap::from([("dir/edited.txt".to_string(), Some(new))])).unwrap();
    let c1 = commit(&mut trees_only, t1, vec![c0], 1);
    let got: BTreeSet<Oid> = trees_only.new_objects(&c1, &c0).unwrap().into_iter().collect();
    let dir = trees_only.file(&t1, "dir/edited.txt").unwrap().unwrap();
    assert_eq!(dir.oid, new.oid);
    assert!(got.contains(&c1) && got.contains(&t1) && got.contains(&new.oid));
    assert!(!got.contains(&kept.oid) && !got.contains(&edited.oid) && !got.contains(&c0));
    assert_eq!(got.len(), 4, "commit, root tree, dir tree, new file");
}

#[test]
fn a_store_names_what_it_lacks_also_when_the_caller_goes_on() {
    // Code that turns a missing object into "no file" still leaves its id and kind behind,
    // so a caller that reads objects one at a time knows what to read.
    let mut full = Store::new();
    let e = FileEntry { mode: MODE_BLOB, oid: full.put_blob(b"x\n") };
    let t = full.write_tree(&Flat::from([("a/b.txt".to_string(), e)]));
    let empty = Store::new();
    assert_eq!(empty.file(&t, "a/b.txt").ok().flatten(), None);
    assert_eq!(empty.take_misses(), vec![(t, "tree")]);
    assert!(empty.take_misses().is_empty(), "taken once");
    assert!(full.blob(&e.oid).is_ok() && full.take_misses().is_empty());
}
