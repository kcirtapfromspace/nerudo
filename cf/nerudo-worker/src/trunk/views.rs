//! What the web UI and the app API read of trunk, without flattening a whole tree: the files
//! that differ between two trees, and one path. On a big trunk the UI's store reads trees one
//! at a time (trunk/objects.rs), so a view must read only the trees on the paths it shows.

use nerudo_git::{FileEntry, Oid, Result, Store};
use std::collections::{BTreeMap, BTreeSet};

pub(super) type Flat = BTreeMap<String, FileEntry>;

/// The files that differ between tree `before` (None: no tree) and tree `after`, as two maps
/// of only those files: what `diff_json` and `diff_trees` compare.
pub(super) fn changed_files(store: &Store, before: Option<&Oid>, after: &Oid) -> Result<(Flat, Flat)> {
    let mut a = Flat::new();
    let mut b = Flat::new();
    for (p, x, y) in store.diff(before, Some(after))? {
        if let Some(x) = x {
            a.insert(p.clone(), x);
        }
        if let Some(y) = y {
            b.insert(p, y);
        }
    }
    Ok((a, b))
}

/// What one path of a tree is.
pub(super) enum PathView {
    File(FileEntry),
    /// A directory: the names of its directories, then of its files.
    Dir(BTreeSet<String>, Vec<String>),
}

/// The file or directory at `path` under `tree` ("" is the root). None: no such path. Reads
/// only the trees on the path. A directory shows its subdirectories and files, as a listing
/// of the flattened tree under it does.
pub(super) fn path_view(store: &Store, tree: &Oid, path: &str) -> Result<Option<PathView>> {
    if let Some(f) = (!path.is_empty()).then(|| store.file(tree, path)).transpose()?.flatten() {
        return Ok(Some(PathView::File(f)));
    }
    let mut cur = *tree;
    for part in path.split('/').filter(|p| !p.is_empty()) {
        match store.tree(&cur)?.into_iter().find(|e| e.name == part && e.is_tree()) {
            Some(e) => cur = e.oid,
            None => return Ok(None),
        }
    }
    let mut dirs = BTreeSet::new();
    let mut files = vec![];
    for e in store.tree(&cur)? {
        if e.is_tree() {
            dirs.insert(e.name);
        } else {
            files.push(e.name);
        }
    }
    files.sort();
    if dirs.is_empty() && files.is_empty() {
        return Ok(None);
    }
    Ok(Some(PathView::Dir(dirs, files)))
}

/// The file at `path` in commit `c`'s tree, and in its first parent's (None where it is not).
pub(super) fn file_at(store: &Store, c: &nerudo_git::Commit, path: &str) -> Result<(Option<Oid>, Option<Oid>)> {
    let now = store.file(&c.tree, path)?.map(|e| e.oid);
    let was = match c.parents.first() {
        Some(p) => store.file(&store.commit(p)?.tree, path)?.map(|e| e.oid),
        None => None,
    };
    Ok((now, was))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nerudo_git::MODE_BLOB;

    fn flat(store: &mut Store, paths: &[&str]) -> Flat {
        paths.iter().map(|p| (p.to_string(), FileEntry { mode: MODE_BLOB, oid: store.put_blob(p.as_bytes()) })).collect()
    }

    #[test]
    fn a_path_view_lists_what_the_flattened_tree_lists() {
        let mut s = Store::new();
        let files = flat(&mut s, &["README.md", "src/a.rs", "src/b/c.rs", "src/b.rs", "docs/x/y.md"]);
        let t = s.write_tree(&files);
        let Some(PathView::Dir(dirs, names)) = path_view(&s, &t, "src").unwrap() else { panic!("src is a directory") };
        assert_eq!(dirs.into_iter().collect::<Vec<_>>(), vec!["b"]);
        assert_eq!(names, vec!["a.rs", "b.rs"]);
        let Some(PathView::Dir(dirs, names)) = path_view(&s, &t, "").unwrap() else { panic!("the root") };
        assert_eq!((dirs.len(), names), (2, vec!["README.md".to_string()]));
        assert!(matches!(path_view(&s, &t, "src/b/c.rs").unwrap(), Some(PathView::File(_))));
        assert!(path_view(&s, &t, "nope").unwrap().is_none());
        assert!(path_view(&s, &t, "README.md/x").unwrap().is_none());
    }

    #[test]
    fn changed_files_are_the_flattened_difference() {
        let mut s = Store::new();
        let a = flat(&mut s, &["x/1", "x/2", "y"]);
        let mut b = a.clone();
        b.remove("x/2");
        b.insert("z/3".into(), FileEntry { mode: MODE_BLOB, oid: s.put_blob(b"new") });
        let (ta, tb) = (s.write_tree(&a), s.write_tree(&b));
        let (before, after) = changed_files(&s, Some(&ta), &tb).unwrap();
        assert_eq!(before.keys().collect::<Vec<_>>(), vec!["x/2"]);
        assert_eq!(after.keys().collect::<Vec<_>>(), vec!["z/3"]);
    }
}
