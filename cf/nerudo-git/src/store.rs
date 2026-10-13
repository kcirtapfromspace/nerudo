use crate::error::{Error, Result};
use crate::object::{Commit, Kind, Object, Tag};
use crate::oid::Oid;
use crate::tree::{parse_tree, serialize_tree, TreeEntry, MODE_TREE};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// A file in a flattened tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub mode: u32,
    pub oid: Oid,
}

/// The files that differ between two trees: (path, entry in the first, entry in the second).
pub type Diff = Vec<(String, Option<FileEntry>, Option<FileEntry>)>;

/// In-memory object store. The sequencer keeps one per trunk. Objects are shared, so a
/// clone of a store (the web UI's view of trunk, say) costs a map, not a second copy of
/// every object.
#[derive(Default)]
pub struct Store {
    objects: HashMap<Oid, Arc<Object>>,
    /// The sum of the objects' sizes.
    bytes: usize,
    /// Objects asked for that are not here, with the kind asked for ("" for any), since the
    /// last `take_misses`. Code that reads one object at a time reads these and runs again,
    /// also where the code went on without them.
    misses: std::sync::Mutex<Vec<(Oid, &'static str)>>,
}

impl Clone for Store {
    fn clone(&self) -> Self {
        Store { objects: self.objects.clone(), bytes: self.bytes, misses: Default::default() }
    }
}

impl Store {
    pub fn new() -> Self {
        Self::default()
    }

    /// The bytes of object data held (not counting the map).
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.objects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    pub fn contains(&self, oid: &Oid) -> bool {
        self.objects.contains_key(oid)
    }

    pub fn insert(&mut self, obj: Object) -> Oid {
        let oid = obj.oid();
        let bytes = &mut self.bytes;
        self.objects.entry(oid).or_insert_with(|| {
            *bytes += obj.data.len();
            Arc::new(obj)
        });
        oid
    }

    /// Add every object of `other` that this store lacks, sharing it rather than copying it.
    pub fn absorb(&mut self, other: &Store) {
        for (oid, obj) in &other.objects {
            let bytes = &mut self.bytes;
            self.objects.entry(*oid).or_insert_with(|| {
                *bytes += obj.data.len();
                Arc::clone(obj)
            });
        }
    }

    pub fn put_blob(&mut self, data: &[u8]) -> Oid {
        self.insert(Object::new(Kind::Blob, data.to_vec()))
    }

    pub fn get(&self, oid: &Oid) -> Result<&Object> {
        self.objects.get(oid).map(|o| o.as_ref()).ok_or_else(|| self.miss(oid, ""))
    }

    fn miss(&self, oid: &Oid, kind: &'static str) -> Error {
        if let Ok(mut m) = self.misses.lock() {
            if !m.iter().any(|(o, _)| o == oid) {
                m.push((*oid, kind));
            }
        }
        if kind.is_empty() {
            Error::Missing(oid.to_hex())
        } else {
            Error::Missing(format!("{} ({kind})", oid.to_hex()))
        }
    }

    /// The objects asked for and not found since the last call, with the kind asked for.
    pub fn take_misses(&self) -> Vec<(Oid, &'static str)> {
        self.misses.lock().map(|mut m| std::mem::take(&mut *m)).unwrap_or_default()
    }

    fn get_kind(&self, oid: &Oid, kind: Kind) -> Result<&Object> {
        // The kind in a Missing error tells a caller that reads objects one at a time what to ask for.
        let o = self.objects.get(oid).map(|o| o.as_ref()).ok_or_else(|| self.miss(oid, kind.name()))?;
        if o.kind != kind {
            return Err(Error::Corrupt(format!("{oid} is a {}, expected {}", o.kind.name(), kind.name())));
        }
        Ok(o)
    }

    pub fn blob(&self, oid: &Oid) -> Result<&[u8]> {
        Ok(&self.get_kind(oid, Kind::Blob)?.data)
    }

    pub fn commit(&self, oid: &Oid) -> Result<Commit> {
        Commit::parse(&self.get_kind(oid, Kind::Commit)?.data)
    }

    pub fn tree(&self, oid: &Oid) -> Result<Vec<TreeEntry>> {
        parse_tree(&self.get_kind(oid, Kind::Tree)?.data)
    }

    pub fn tag(&self, oid: &Oid) -> Result<Tag> {
        Tag::parse(&self.get_kind(oid, Kind::Tag)?.data)
    }

    pub fn put_tag(&mut self, t: &Tag) -> Oid {
        self.insert(Object::new(Kind::Tag, t.serialize()))
    }

    pub fn put_commit(&mut self, c: &Commit) -> Oid {
        self.insert(Object::new(Kind::Commit, c.serialize()))
    }

    /// Flatten a tree into "path/to/file" -> entry. Gitlinks are kept as entries.
    pub fn flatten(&self, tree: &Oid) -> Result<BTreeMap<String, FileEntry>> {
        let mut out = BTreeMap::new();
        self.flatten_into(tree, "", &mut out, 0)?;
        Ok(out)
    }

    fn flatten_into(&self, tree: &Oid, prefix: &str, out: &mut BTreeMap<String, FileEntry>, depth: usize) -> Result<()> {
        let depth = deeper(depth)?;
        for e in self.tree(tree)? {
            let path = if prefix.is_empty() { e.name.clone() } else { format!("{prefix}/{}", e.name) };
            if e.is_tree() {
                self.flatten_into(&e.oid, &path, out, depth)?;
            } else {
                out.insert(path, FileEntry { mode: e.mode, oid: e.oid });
            }
        }
        Ok(())
    }

    /// The file at `path` under `tree`: None when no file has that path (a directory is not a
    /// file). Reads only the trees on the path.
    pub fn file(&self, tree: &Oid, path: &str) -> Result<Option<FileEntry>> {
        let mut cur = *tree;
        let mut parts = path.split('/').peekable();
        while let Some(part) = parts.next() {
            let Some(e) = self.tree(&cur)?.into_iter().find(|e| e.name == part) else { return Ok(None) };
            match (parts.peek().is_none(), e.is_tree()) {
                (true, false) => return Ok(Some(FileEntry { mode: e.mode, oid: e.oid })),
                (false, true) => cur = e.oid,
                _ => return Ok(None),
            }
        }
        Ok(None)
    }

    /// The files under directory `dir` of `tree`, keyed by their full paths, as `flatten` gives
    /// them. Empty when `dir` is not a directory. Reads only that directory's trees.
    pub fn flatten_dir(&self, tree: &Oid, dir: &str) -> Result<BTreeMap<String, FileEntry>> {
        let mut out = BTreeMap::new();
        let dir = dir.trim_end_matches('/');
        if dir.is_empty() {
            self.flatten_into(tree, "", &mut out, 0)?;
            return Ok(out);
        }
        let mut cur = *tree;
        for part in dir.split('/') {
            match self.tree(&cur)?.into_iter().find(|e| e.name == part && e.is_tree()) {
                Some(e) => cur = e.oid,
                None => return Ok(out),
            }
        }
        self.flatten_into(&cur, dir, &mut out, 0)?;
        Ok(out)
    }

    /// The files that differ between trees `a` and `b` (None: no tree), sorted by path, with the
    /// entry on each side: the same as comparing `flatten(a)` with `flatten(b)`. It reads only
    /// the subtrees whose ids differ, so its cost follows the size of the difference.
    pub fn diff(&self, a: Option<&Oid>, b: Option<&Oid>) -> Result<Diff> {
        let mut out = vec![];
        self.diff_into(a.copied(), b.copied(), "", &mut out, 0)?;
        out.sort_by(|x, y| x.0.cmp(&y.0));
        Ok(out)
    }

    fn diff_into(&self, a: Option<Oid>, b: Option<Oid>, prefix: &str, out: &mut Diff, depth: usize) -> Result<()> {
        if a == b {
            return Ok(());
        }
        let depth = deeper(depth)?;
        let entries = |t: Option<Oid>| -> Result<Vec<TreeEntry>> { t.map(|t| self.tree(&t)).transpose().map(Option::unwrap_or_default) };
        let mut names: BTreeMap<String, (Option<TreeEntry>, Option<TreeEntry>)> = BTreeMap::new();
        for e in entries(a)? {
            let name = e.name.clone();
            names.entry(name).or_default().0 = Some(e);
        }
        for e in entries(b)? {
            let name = e.name.clone();
            names.entry(name).or_default().1 = Some(e);
        }
        for (name, (ea, eb)) in names {
            if ea == eb {
                continue;
            }
            let path = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
            let file = |e: &Option<TreeEntry>| e.as_ref().filter(|e| !e.is_tree()).map(|e| FileEntry { mode: e.mode, oid: e.oid });
            let dir = |e: &Option<TreeEntry>| e.as_ref().filter(|e| e.is_tree()).map(|e| e.oid);
            let (fa, fb) = (file(&ea), file(&eb));
            if fa != fb {
                out.push((path.clone(), fa, fb));
            }
            self.diff_into(dir(&ea), dir(&eb), &path, out, depth)?;
        }
        Ok(())
    }

    /// Write `tree` (None: an empty tree) with `edits` applied: path -> Some(file) to add or
    /// replace a file, None to remove it. It writes only the trees on edited paths, and the
    /// result is the tree that `write_tree` makes from the flattened tree with the edits.
    /// A directory that loses its last file goes. A path that would be a file and a directory
    /// at once is an error.
    pub fn edit_tree(&mut self, tree: Option<&Oid>, edits: &BTreeMap<String, Option<FileEntry>>) -> Result<Oid> {
        let edits: Vec<(&str, Option<FileEntry>)> = edits.iter().map(|(p, e)| (p.as_str(), *e)).collect();
        match self.edit_node(tree.copied(), &edits, 0)? {
            Some(t) => Ok(t),
            None => Ok(self.insert(Object::new(Kind::Tree, vec![]))),
        }
    }

    fn edit_node(&mut self, tree: Option<Oid>, edits: &[(&str, Option<FileEntry>)], depth: usize) -> Result<Option<Oid>> {
        let depth = deeper(depth)?;
        let mut entries: BTreeMap<String, TreeEntry> = match tree {
            Some(t) => self.tree(&t)?.into_iter().map(|e| (e.name.clone(), e)).collect(),
            None => BTreeMap::new(),
        };
        // Removals first, then directories, then files: a file can replace a directory, or a
        // directory a file, in one edit.
        let mut nested: BTreeMap<&str, Vec<(&str, Option<FileEntry>)>> = BTreeMap::new();
        let mut files = vec![];
        for (path, edit) in edits {
            match (path.split_once('/'), edit) {
                (Some((first, rest)), _) => nested.entry(first).or_default().push((rest, *edit)),
                (None, Some(f)) => files.push((*path, *f)),
                (None, None) => {
                    if entries.get(*path).is_some_and(|e| !e.is_tree()) {
                        entries.remove(*path);
                    }
                }
            }
        }
        for (name, sub) in nested {
            let existing = match entries.get(name) {
                Some(e) if e.is_tree() => Some(e.oid),
                Some(_) if sub.iter().any(|(_, e)| e.is_some()) => return Err(Error::Corrupt(format!("{name} is a file and a directory"))),
                _ => None,
            };
            if existing.is_none() && sub.iter().all(|(_, e)| e.is_none()) {
                continue; // removing files from a directory that is not there
            }
            match self.edit_node(existing, &sub, depth)? {
                Some(oid) => {
                    entries.insert(name.to_string(), TreeEntry { mode: MODE_TREE, name: name.to_string(), oid });
                }
                None => {
                    entries.remove(name);
                }
            }
        }
        for (name, f) in files {
            if entries.get(name).is_some_and(TreeEntry::is_tree) {
                return Err(Error::Corrupt(format!("{name} is a directory and a file")));
            }
            entries.insert(name.to_string(), TreeEntry { mode: f.mode, name: name.to_string(), oid: f.oid });
        }
        if entries.is_empty() {
            return Ok(None);
        }
        let mut list: Vec<TreeEntry> = entries.into_values().collect();
        Ok(Some(self.insert(Object::new(Kind::Tree, serialize_tree(&mut list)))))
    }

    /// The objects to push to move a remote from commit `stop` to commit `tip`: the commits
    /// after `stop`, and for each, the trees and files on the paths it changed from its parent.
    /// Unlike `missing_from`, it never walks the trees that did not change, so its cost follows
    /// the size of the change, not of the repo. An object on a changed path that this store
    /// does not hold is left out: the store got every object of a change except the ones the
    /// remote said it has, and the store made the rest (merges, new trees, commits).
    pub fn new_objects(&self, tip: &Oid, stop: &Oid) -> Result<Vec<Oid>> {
        let mut out = std::collections::HashSet::new();
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![*tip];
        while let Some(c) = stack.pop() {
            if c == *stop || !out.insert(c) {
                continue;
            }
            let commit = self.commit(&c)?;
            let parent = commit.parents.first().map(|p| self.commit(p)).transpose()?;
            self.changed_objects(parent.map(|p| p.tree), commit.tree, &mut out, &mut seen, 0)?;
            stack.extend(commit.parents);
        }
        let mut v: Vec<Oid> = out.into_iter().collect();
        v.sort();
        Ok(v)
    }

    fn changed_objects(&self, old: Option<Oid>, new: Oid, out: &mut std::collections::HashSet<Oid>, seen: &mut std::collections::HashSet<(Option<Oid>, Oid)>, depth: usize) -> Result<()> {
        // What goes depends on the pair: the same tree under another old tree can bring
        // other objects, so a tree already sent is still compared again.
        if old == Some(new) || !self.contains(&new) || !seen.insert((old, new)) {
            return Ok(());
        }
        let depth = deeper(depth)?;
        out.insert(new);
        let before: HashMap<String, TreeEntry> = match old.filter(|o| self.contains(o)) {
            Some(o) => self.tree(&o)?.into_iter().map(|e| (e.name.clone(), e)).collect(),
            None => HashMap::new(),
        };
        for e in self.tree(&new)? {
            let was = before.get(&e.name);
            if was.is_some_and(|w| w.oid == e.oid) {
                continue;
            }
            if e.is_tree() {
                self.changed_objects(was.filter(|w| w.is_tree()).map(|w| w.oid), e.oid, out, seen, depth)?;
            } else if e.mode != crate::tree::MODE_GITLINK && self.contains(&e.oid) {
                out.insert(e.oid);
            }
        }
        Ok(())
    }

    /// Build (and store) the tree objects for a flattened file map; returns the root tree.
    pub fn write_tree(&mut self, files: &BTreeMap<String, FileEntry>) -> Oid {
        let mut root: Node = Node::default();
        for (path, entry) in files {
            let mut node = &mut root;
            let mut parts = path.split('/').peekable();
            while let Some(part) = parts.next() {
                if parts.peek().is_none() {
                    node.files.insert(part.to_string(), *entry);
                } else {
                    node = node.dirs.entry(part.to_string()).or_default();
                }
            }
        }
        self.write_node(&root, 0)
    }

    fn write_node(&mut self, node: &Node, depth: usize) -> Oid {
        // The paths came from this process (a seed, an import, a flattened tree), so a
        // depth past the limit is a bug here, not input to refuse: the subtree is left out.
        let Ok(depth) = deeper(depth) else { return self.insert(Object::new(Kind::Tree, vec![])) };
        let mut entries: Vec<TreeEntry> = node
            .files
            .iter()
            .map(|(name, e)| TreeEntry { mode: e.mode, name: name.clone(), oid: e.oid })
            .collect();
        for (name, child) in &node.dirs {
            let oid = self.write_node(child, depth);
            entries.push(TreeEntry { mode: MODE_TREE, name: name.clone(), oid });
        }
        self.insert(Object::new(Kind::Tree, serialize_tree(&mut entries)))
    }

    /// Every object reachable from `tip` that is not reachable from any `stop` commit.
    /// Used to build the pack for a push: the remote already has everything under `stop`.
    pub fn missing_from(&self, tip: &Oid, stop: &[Oid]) -> Result<Vec<Oid>> {
        let mut have = std::collections::HashSet::new();
        for s in stop {
            if self.contains(s) {
                self.reach_commit(s, &mut have, None)?;
            }
        }
        let mut out = std::collections::HashSet::new();
        self.reach_commit(tip, &mut out, Some(&have))?;
        let mut v: Vec<Oid> = out.into_iter().filter(|o| !have.contains(o)).collect();
        v.sort();
        Ok(v)
    }

    fn reach_commit(
        &self,
        start: &Oid,
        seen: &mut std::collections::HashSet<Oid>,
        stop: Option<&std::collections::HashSet<Oid>>,
    ) -> Result<()> {
        let mut stack = vec![*start];
        while let Some(c) = stack.pop() {
            if stop.is_some_and(|s| s.contains(&c)) || !seen.insert(c) {
                continue;
            }
            let commit = match self.commit(&c) {
                Ok(c) => c,
                // A shallow or partial history ends here; the remote has the rest.
                Err(Error::Missing(_)) if stop.is_none() => continue,
                Err(e) => return Err(e),
            };
            self.reach_tree(&commit.tree, seen, stop, 0)?;
            stack.extend(commit.parents);
        }
        Ok(())
    }

    fn reach_tree(
        &self,
        tree: &Oid,
        seen: &mut std::collections::HashSet<Oid>,
        stop: Option<&std::collections::HashSet<Oid>>,
        depth: usize,
    ) -> Result<()> {
        if stop.is_some_and(|s| s.contains(tree)) || !seen.insert(*tree) {
            return Ok(());
        }
        let depth = deeper(depth)?;
        for e in self.tree(tree)? {
            if e.is_tree() {
                self.reach_tree(&e.oid, seen, stop, depth)?;
            } else if e.mode != crate::tree::MODE_GITLINK && !stop.is_some_and(|s| s.contains(&e.oid)) {
                seen.insert(e.oid);
            }
        }
        Ok(())
    }

    /// Keep only the objects in `keep` and free the rest.
    pub fn retain(&mut self, keep: &std::collections::HashSet<Oid>) {
        self.objects.retain(|k, _| keep.contains(k));
        self.bytes = self.objects.values().map(|o| o.data.len()).sum();
        self.objects.shrink_to_fit();
    }

    pub fn object_list(&self, oids: &[Oid]) -> Result<Vec<&Object>> {
        oids.iter().map(|o| self.get(o)).collect()
    }
}

#[derive(Default)]
struct Node {
    files: BTreeMap<String, FileEntry>,
    dirs: BTreeMap<String, Node>,
}

/// How deep a tree may nest. Every walk of a tree here recurses once for each level, on a
/// stack of about 1 MB in WebAssembly, where an overflow takes the whole sequencer down;
/// no real repository comes near this, and a crafted one is refused as corrupt.
pub const MAX_TREE_DEPTH: usize = 256;

/// One level deeper, or the error for a tree that nests past MAX_TREE_DEPTH.
fn deeper(depth: usize) -> Result<usize> {
    if depth >= MAX_TREE_DEPTH {
        return Err(Error::Corrupt(format!("tree nests deeper than {MAX_TREE_DEPTH} levels")));
    }
    Ok(depth + 1)
}

#[cfg(test)]
mod depth_tests {
    use super::*;

    /// A tree `a/a/.../a/f` with `levels` directories, made from objects, as a remote could
    /// send it (write_tree would refuse to go that deep).
    fn deep(store: &mut Store, levels: usize) -> Oid {
        let blob = store.put_blob(b"x");
        let mut cur = store.insert(Object::new(Kind::Tree, serialize_tree(&mut [TreeEntry { mode: crate::tree::MODE_BLOB, name: "f".into(), oid: blob }])));
        for _ in 0..levels {
            cur = store.insert(Object::new(Kind::Tree, serialize_tree(&mut [TreeEntry { mode: MODE_TREE, name: "a".into(), oid: cur }])));
        }
        cur
    }

    #[test]
    fn a_tree_that_nests_too_deep_is_corrupt_not_a_stack_overflow() {
        let mut s = Store::new();
        let ok = deep(&mut s, MAX_TREE_DEPTH - 1);
        assert_eq!(s.flatten(&ok).unwrap().len(), 1);
        let bad = deep(&mut s, 100_000);
        let e = s.flatten(&bad).unwrap_err().to_string();
        assert!(e.contains("nests deeper"), "{e}");
        assert!(s.diff(None, Some(&bad)).unwrap_err().to_string().contains("nests deeper"));
        assert!(s.diff(Some(&ok), Some(&bad)).unwrap_err().to_string().contains("nests deeper"));
        let mut seen = std::collections::HashSet::new();
        assert!(s.reach_tree(&bad, &mut seen, None, 0).unwrap_err().to_string().contains("nests deeper"));
        let edits: BTreeMap<String, Option<FileEntry>> = [(format!("{}g", "a/".repeat(100_000)), Some(FileEntry { mode: crate::tree::MODE_BLOB, oid: s.put_blob(b"y") }))].into_iter().collect();
        assert!(s.edit_tree(Some(&bad), &edits).unwrap_err().to_string().contains("nests deeper"));
        assert!(s.edit_tree(None, &edits).unwrap_err().to_string().contains("nests deeper"));
    }
}
