//! Apply one change onto the trunk tip, over trees, without a worktree.
//!
//! Exclusive paths merge at file level: if trunk and the change both changed a path to
//! different content, that's a conflict and the change is ejected (the agent regenerates).
//! Special paths are never taken from the change: commutative files are appended to,
//! pending allocations are numbered now, and derived files are regenerated.

use crate::rules::Rules;
use nerudo_git::{Commit, FileEntry, Oid, Signature, Store, MODE_BLOB, MODE_EXEC, MODE_GITLINK, MODE_LINK};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChangeInput {
    pub change_id: String,
    pub rev: u32,
    /// The agent's commit (in the store).
    pub commit: Oid,
    /// The commit the agent built on, an ancestor of its tip (in the store).
    pub base: Oid,
    /// Extra trailer lines for the landed commit (the seals it landed under).
    #[serde(default)]
    pub trailers: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Ejection {
    /// Trunk and the change edited the same paths differently since the change's base.
    /// `hunks`: per path, the conflicting lines with git's markers (see `hunks_of`).
    /// `beaten_by`: the landings that changed those paths first. The sequencer fills it in;
    /// receipts from before these fields have neither.
    Conflict {
        paths: Vec<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        hunks: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        beaten_by: Vec<Prior>,
    },
    /// The change commit doesn't sit on the base it claims.
    BadBase { detail: String },
    /// The change adds a path that can't be on trunk (see `path_problem`), or an entry
    /// whose mode is not a file's.
    BadPath { detail: String },
    /// A trunk invariant (unique numbering, derived consistency, ...) fails after applying.
    CheckFailed { detail: String },
    /// Objects needed to apply the change are missing (fetch problem).
    Missing { detail: String },
}

/// A landing on trunk since a change's base that bears on it: one that changed the same
/// unit first (a conflict), or one that moved trunk under it (a CI failure).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Prior {
    pub change_id: String,
    #[serde(default)]
    pub rev: u32,
    #[serde(default)]
    pub number: Option<u64>,
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub subject: String,
    /// The commit on trunk.
    pub commit: String,
    /// Its `Context:` trailer (`<repo>@<commit>`), when it landed with context.
    #[serde(default)]
    pub context: Option<String>,
    /// The paths or units it shares with the change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

/// A receipt is one stored value, so it keeps the conflict text of a few files, each cut.
pub const HUNK_FILES: usize = 4;
pub const HUNK_BYTES: usize = 4096;
/// Lines kept around each conflict.
const HUNK_CONTEXT: usize = 2;

#[derive(Clone, Debug)]
pub struct Applied {
    pub commit: Oid,
    pub tree: Oid,
    /// Footprint units the change writes (exclusive paths only).
    pub footprint: BTreeSet<String>,
    /// What the rules did, for receipts and stats.
    pub allocated: Vec<String>,
    pub appended: Vec<String>,
    pub regenerated: Vec<String>,
}

/// Footprint of a change: units of the non-special paths it touches relative to its base.
pub fn footprint(store: &Store, rules: &Rules, change: &ChangeInput) -> Result<BTreeSet<String>, Ejection> {
    Ok(changes(store, change)?.into_iter().map(|(p, ..)| p).filter(|p| !rules.is_special(p)).map(|p| rules.unit_of(&p)).collect())
}

type Flat = BTreeMap<String, FileEntry>;

/// The files a change edits: (path, the file at its base, the file in the change).
pub type Changes = nerudo_git::Diff;

/// Paths the change edits relative to its base.
pub fn edits(store: &Store, change: &ChangeInput) -> Result<Vec<String>, Ejection> {
    Ok(changes(store, change)?.into_iter().map(|(p, ..)| p).collect())
}

fn missing(e: nerudo_git::Error) -> Ejection {
    Ejection::Missing { detail: e.to_string() }
}

/// Maximum distinct commits inspected to prove a change's base. The bound also caps the
/// pending walk: an untrusted merge commit cannot enqueue an unbounded number of parents.
pub const MAX_BASE_COMMITS: usize = 2_000;

/// Prove the claimed base before using its tree. Plain Git branches can contain several
/// commits; their complete delta is tip-versus-base, while approvals still cover the
/// original tip. Missing history remains Missing so lazy object readers can fill it.
fn check_base(store: &Store, change: &ChangeInput) -> Result<(), Ejection> {
    let bad = |detail| Ejection::BadBase { detail };
    if change.commit == change.base {
        return Err(bad("the change tip is its base".into()));
    }
    let mut seen = BTreeSet::from([change.commit]);
    let mut queue = VecDeque::from([change.commit]);
    let mut absent = None;
    while let Some(oid) = queue.pop_front() {
        let commit = match store.commit(&oid) {
            Ok(c) => c,
            Err(e) => {
                // Another known path may prove ancestry without loading this parent.
                absent.get_or_insert_with(|| missing(e));
                continue;
            }
        };
        if commit.parents.contains(&change.base) {
            return Ok(());
        }
        for parent in commit.parents {
            if seen.insert(parent) {
                if seen.len() > MAX_BASE_COMMITS {
                    return Err(bad(format!("base ancestry exceeds {MAX_BASE_COMMITS} commits; rebase the branch on main")));
                }
                queue.push_back(parent);
            }
        }
    }
    Err(absent.unwrap_or_else(|| bad(format!("{} is not an ancestor of {}", change.base, change.commit))))
}

/// The files the change edits relative to its base, sorted by path. It reads only the trees
/// that differ between the two commits, never the whole tree. A change that adds a path
/// trunk can't hold (`path_problem`), or an entry with a mode that is not a file's, is
/// refused here, so nothing that lands, gates or measures a change ever sees such a path.
pub fn changes(store: &Store, change: &ChangeInput) -> Result<Changes, Ejection> {
    check_base(store, change)?;
    let c = store.commit(&change.commit).map_err(missing)?;
    let base = store.commit(&change.base).map_err(missing)?;
    // A tree the store refuses (nested past what any repository has) is the change's doing.
    let diff = store.diff(Some(&base.tree), Some(&c.tree)).map_err(|e| match e {
        nerudo_git::Error::Corrupt(detail) => Ejection::BadPath { detail },
        other => missing(other),
    })?;
    for (p, _, ours) in &diff {
        // A deletion takes a path off trunk: fine whatever the path is.
        let Some(e) = ours else { continue };
        if let Some(detail) = path_problem(p) {
            return Err(Ejection::BadPath { detail });
        }
        if ![MODE_BLOB, MODE_EXEC, MODE_LINK, MODE_GITLINK].contains(&e.mode) {
            return Err(Ejection::BadPath { detail: format!("{p}: mode {:o} is not a file mode", e.mode) });
        }
    }
    Ok(diff)
}

/// The most file content one change may add to trunk, in bytes: the sum of the files it
/// edits, as they are in the change. The sequencer holds a change's files in memory to gate
/// and land it, beside trunk's trees and the files it reads on head's side.
pub const MAX_CHANGE_BYTES: usize = 32 << 20;

/// The bytes of the files `change` edits, as they are in the change: what landing it holds
/// in memory. A file whose content the store lacks counts as nothing (it comes later, or
/// its absence ejects the change elsewhere).
pub fn change_bytes(store: &Store, change: &ChangeInput) -> Result<usize, Ejection> {
    Ok(changes(store, change)?.iter().filter_map(|(_, _, c)| c.as_ref()).filter(|e| e.mode != MODE_GITLINK).map(|e| store.blob(&e.oid).map(<[u8]>::len).unwrap_or(0)).sum())
}

/// The longest path and the longest name trunk takes: what most file systems check out
/// (PATH_MAX and NAME_MAX).
pub const MAX_PATH_BYTES: usize = 4096;
pub const MAX_NAME_BYTES: usize = 255;

/// Why a path can't be on trunk, or None. git refuses to check out a path with an empty
/// component, `.`, `..` or `.git` (in any case, and as NTFS and HFS+ spell it), so one such
/// path on trunk would leave every clone of it failing; a backslash or a control character
/// in a name reads as another path on some systems, and as another line in a commit
/// trailer. Every change is held to this at its base's diff (`changes`); seeds are too.
pub fn path_problem(path: &str) -> Option<String> {
    if path.len() > MAX_PATH_BYTES {
        return Some(format!("a path of {} bytes is longer than {MAX_PATH_BYTES}", path.len()));
    }
    if path.split('/').count() > nerudo_git::MAX_TREE_DEPTH {
        return Some(format!("a path nests deeper than {} directories", nerudo_git::MAX_TREE_DEPTH));
    }
    for part in path.split('/') {
        if part.is_empty() {
            return Some(format!("{path:?}: an empty path component"));
        }
        if part.len() > MAX_NAME_BYTES {
            return Some(format!("{path:?}: a name longer than {MAX_NAME_BYTES} bytes"));
        }
        if part == "." || part == ".." {
            return Some(format!("{path:?}: a {part} component"));
        }
        if is_dot_git(part) {
            return Some(format!("{path:?}: a .git component"));
        }
        if part.contains('\\') {
            return Some(format!("{path:?}: a backslash in a name"));
        }
        if part.chars().any(char::is_control) {
            return Some(format!("{path:?}: a control character in a name"));
        }
    }
    None
}

/// `.git` as git's own `verify_path` refuses it: in any case, with the trailing dots and
/// spaces NTFS drops, with the code points HFS+ ignores taken out, and as the NTFS short
/// name `git~1`.
fn is_dot_git(name: &str) -> bool {
    let plain: String = name.chars().filter(|c| !is_hfs_ignorable(*c)).collect();
    let trimmed = plain.trim_end_matches(['.', ' ']);
    trimmed.eq_ignore_ascii_case(".git") || trimmed.eq_ignore_ascii_case("git~1")
}

fn is_hfs_ignorable(c: char) -> bool {
    matches!(c, '\u{200c}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{206a}'..='\u{206f}' | '\u{feff}')
}

/// The file contents that gating and landing `change` on `head` read, as blob ids that `store`
/// does not hold yet. The trees of the three commits must be in the store. Two lists: the
/// change's side (its base and its commit, which its fork holds), and head's side (trunk).
/// Change side: each edited file in the change (a landing pushes it), and the file at the
/// base only where something reads it: a 3-way merge (head moved the file), a commutative
/// file, a seal rule on tests or migrations, or a decision model. Head side: each edited file
/// on head where head moved it since the base, head's nerudo.toml, commutative files, and
/// the source directory of each derived file (it is regenerated at every landing).
/// `prinfo::change_size` may want more base files; it fetches them on its own.
pub fn blobs_needed(store: &Store, rules: &Rules, head: &Oid, change: &ChangeInput) -> Result<(Vec<Oid>, Vec<Oid>), Ejection> {
    let changes = changes(store, change)?;
    let tree = store.commit(head).map_err(missing)?.tree;
    let paths: Vec<String> = changes.iter().map(|(p, ..)| p.clone()).collect();
    let decided = rules.decide.as_ref().is_some_and(|d| d.asks(&paths));
    let mut ours: Vec<FileEntry> = vec![];
    let mut theirs: Vec<FileEntry> = vec![];
    for (p, b, c) in &changes {
        ours.extend(c.iter().copied());
        let h = store.file(&tree, p).map_err(missing)?;
        let commutative = rules.commutative.iter().any(|r| &r.path == p);
        let merged = h != *b && h != *c && !rules.is_special(p);
        if merged || commutative || decided || crate::gates::reads_base(rules, p) {
            ours.extend(b.iter().copied());
        }
        if h != *b || commutative {
            theirs.extend(h);
        }
    }
    theirs.extend(crate::rules::rules_file(store, &tree).map_err(missing)?);
    for d in &rules.derived {
        theirs.extend(store.flatten_dir(&tree, &d.source).map_err(missing)?.into_values());
    }
    let absent = |s: Vec<FileEntry>| -> Vec<Oid> {
        let ids: BTreeSet<Oid> = s.into_iter().filter(|e| e.mode != MODE_GITLINK && !store.contains(&e.oid)).map(|e| e.oid).collect();
        ids.into_iter().collect()
    };
    let ours = absent(ours);
    let theirs: Vec<Oid> = absent(theirs).into_iter().filter(|o| !ours.contains(o)).collect();
    Ok((ours, theirs))
}

fn text(store: &Store, e: Option<&FileEntry>) -> Result<String, Ejection> {
    match e {
        None => Ok(String::new()),
        Some(e) => Ok(String::from_utf8_lossy(store.blob(&e.oid).map_err(missing)?).into_owned()),
    }
}

/// Head's tree with a landing's edits on top. Reads look at the edits, then at the tree, so a
/// landing reads only the paths it touches and the directories its rules name.
struct Overlay {
    tree: Oid,
    edits: BTreeMap<String, Option<FileEntry>>,
}

impl Overlay {
    fn get(&self, store: &Store, path: &str) -> Result<Option<FileEntry>, Ejection> {
        match self.edits.get(path) {
            Some(e) => Ok(*e),
            None => store.file(&self.tree, path).map_err(missing),
        }
    }

    fn set(&mut self, path: &str, e: Option<FileEntry>) {
        self.edits.insert(path.to_string(), e);
    }

    /// The files under `dir`, by full path.
    fn dir(&self, store: &Store, dir: &str) -> Result<Flat, Ejection> {
        let mut files = store.flatten_dir(&self.tree, dir).map_err(missing)?;
        let prefix = format!("{}/", dir.trim_end_matches('/'));
        for (p, e) in self.edits.range(prefix.clone()..).take_while(|(p, _)| p.starts_with(&prefix)) {
            match e {
                Some(e) => files.insert(p.clone(), *e),
                None => files.remove(p),
            };
        }
        Ok(files)
    }
}

pub fn apply_change(
    store: &mut Store,
    rules: &Rules,
    head: Oid,
    change: &ChangeInput,
    committer: &Signature,
) -> Result<Applied, Ejection> {
    let changes = changes(store, change)?;
    let base: Flat = changes.iter().filter_map(|(p, b, _)| b.map(|e| (p.clone(), e))).collect();
    let ours: Flat = changes.iter().filter_map(|(p, _, c)| c.map(|e| (p.clone(), e))).collect();
    let changed: Vec<String> = changes.into_iter().map(|(p, ..)| p).collect();
    let head_tree = store.commit(&head).map_err(missing)?.tree;
    let mut result = Overlay { tree: head_tree, edits: BTreeMap::new() };

    // 1. Exclusive paths: three-way. Whole-file when only one side changed; line-level
    //    (diff3) when both did; a conflict only when the same lines changed differently.
    let mut conflicts = vec![];
    let mut hunks = BTreeMap::new();
    for p in changed.iter().filter(|p| !rules.is_special(p)) {
        let (b, h, c) = (base.get(p), result.get(store, p)?, ours.get(p));
        if h.as_ref() == b {
            result.set(p, c.copied());
        } else if h.as_ref() != c {
            // A side whose content is not here is a fetch problem, not a conflict.
            if let Some(e) = [b, h.as_ref(), c].into_iter().flatten().find(|e| e.mode != MODE_GITLINK && !store.contains(&e.oid)) {
                return Err(Ejection::Missing { detail: nerudo_git::Error::Missing(e.oid.to_hex()).to_string() });
            }
            match line_merge(store, b, h.as_ref(), c) {
                Ok(merged) => {
                    let mode = c.map(|e| e.mode).unwrap_or(MODE_BLOB);
                    let oid = store.put_blob(merged.as_bytes());
                    result.set(p, Some(FileEntry { mode, oid }));
                }
                Err(marked) => {
                    if let Some(m) = marked.filter(|_| hunks.len() < HUNK_FILES) {
                        hunks.insert(p.clone(), hunks_of(&m));
                    }
                    conflicts.push(p.clone());
                }
            }
        }
    }
    if !conflicts.is_empty() {
        return Err(Ejection::Conflict { paths: conflicts, hunks, beaten_by: vec![] });
    }

    // 2. Commutative files: append the lines the change appended.
    let mut appended = vec![];
    for rule in &rules.commutative {
        if !changed.contains(&rule.path) {
            continue;
        }
        let before = text(store, base.get(&rule.path))?;
        let after = text(store, ours.get(&rule.path))?;
        let added: Vec<&str> = match after.strip_prefix(before.as_str()) {
            Some(suffix) => suffix.lines().collect(),
            // Not a pure append: take the lines the change has that its base didn't.
            None => {
                let mut pool: Vec<&str> = before.lines().collect();
                after
                    .lines()
                    .filter(|l| match pool.iter().position(|x| x == l) {
                        Some(i) => {
                            pool.remove(i);
                            false
                        }
                        None => true,
                    })
                    .collect()
            }
        };
        if added.is_empty() {
            continue;
        }
        let mut merged = text(store, result.get(store, &rule.path)?.as_ref())?;
        if !merged.is_empty() && !merged.ends_with('\n') {
            merged.push('\n');
        }
        for l in &added {
            merged.push_str(l);
            merged.push('\n');
        }
        let oid = store.put_blob(merged.as_bytes());
        result.set(&rule.path, Some(FileEntry { mode: MODE_BLOB, oid }));
        appended.push(rule.path.clone());
    }

    // 3. Allocations: number pending files on top of everything already on trunk.
    let mut allocated = vec![];
    for p in changed.iter() {
        let Some((rule, name)) = rules.pending(p) else { continue };
        let Some(entry) = ours.get(p) else { continue };
        if base.contains_key(p) {
            continue; // only newly added pending files are allocated
        }
        let prefix = format!("{}/", rule.dir);
        let next = result
            .dir(store, &rule.dir)?
            .keys()
            .filter_map(|k| k.strip_prefix(&prefix))
            .filter_map(|f| f.split('_').next()?.parse::<u64>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        let path = format!("{}/{next:0w$}_{name}", rule.dir, w = rule.width);
        result.set(&path, Some(*entry));
        allocated.push(path);
    }

    // 4. Derived files: regenerate from the merged tree.
    let mut regenerated = vec![];
    for d in &rules.derived {
        let content = digest_index(store, &result.dir(store, &d.source)?, &d.source)?;
        let oid = store.put_blob(content.as_bytes());
        if result.get(store, &d.path)?.map(|e| e.oid) != Some(oid) {
            regenerated.push(d.path.clone());
        }
        result.set(&d.path, Some(FileEntry { mode: MODE_BLOB, oid }));
    }

    // 5. One linear commit per change: the agent stays author, the sequencer commits.
    let theirs = store.commit(&change.commit).map_err(missing)?;
    // The agent's message stays: its subject, and its body (why), above the sequencer's own
    // trailer paragraph. Trailers are read from the last paragraph only, so nothing the
    // agent wrote can pass for a Change-Id, Sealed-By or Break-Glass trailer.
    let subject = theirs.message.lines().next().unwrap_or("change").to_string();
    let body = theirs.message.split_once('\n').map(|(_, r)| r.trim()).unwrap_or("");
    let tree = store.edit_tree(Some(&head_tree), &result.edits).map_err(|e| Ejection::CheckFailed { detail: e.to_string() })?;
    let mut message = format!("{subject}\n\n");
    if !body.is_empty() {
        message.push_str(body);
        message.push_str("\n\n");
    }
    message.push_str(&format!("Change-Id: {}\nRevision: {}\n", change.change_id, change.rev));
    for t in &change.trailers {
        message.push_str(t);
        message.push('\n');
    }
    message.push_str("Landed-By: nerudo\n");
    let commit = Commit { tree, parents: vec![head], author: theirs.author.clone(), committer: committer.clone(), extra: vec![], message };
    let oid = store.put_commit(&commit);
    let footprint = changed.iter().filter(|p| !rules.is_special(p)).map(|p| rules.unit_of(p)).collect();
    Ok(Applied { commit: oid, tree, footprint, allocated, appended, regenerated })
}

/// diff3 over UTF-8 text. Err(Some(the file with conflict markers)) when the same lines
/// changed differently; Err(None) when a side is missing or not UTF-8 text.
fn line_merge(store: &Store, b: Option<&FileEntry>, h: Option<&FileEntry>, c: Option<&FileEntry>) -> Result<String, Option<String>> {
    let get = |e: Option<&FileEntry>| e.and_then(|e| store.blob(&e.oid).ok()).and_then(|d| std::str::from_utf8(d).ok()).map(str::to_string);
    let (Some(base), Some(head), Some(ours)) = (get(b), get(h), get(c)) else {
        return Err(None);
    };
    diffy::merge(&base, &head, &ours).map_err(Some)
}

/// The conflicts in a file that diffy marked, each with HUNK_CONTEXT lines around it, cut at
/// HUNK_BYTES. diffy calls trunk "ours" (it merges onto trunk) and the change "theirs"; the
/// markers say `main` and `this change` instead, so the agent reads them the right way round.
pub fn hunks_of(marked: &str) -> String {
    let lines: Vec<&str> = marked.lines().collect();
    let mut keep = vec![false; lines.len()];
    let mut start = None;
    for (i, l) in lines.iter().enumerate() {
        if l.starts_with("<<<<<<<") {
            start = Some(i);
        } else if l.starts_with(">>>>>>>") {
            if let Some(s) = start.take() {
                let (from, to) = (s.saturating_sub(HUNK_CONTEXT), (i + HUNK_CONTEXT).min(lines.len() - 1));
                keep[from..=to].iter_mut().for_each(|k| *k = true);
            }
        }
    }
    let mut out = String::new();
    let mut gap = false;
    for (i, l) in lines.iter().enumerate() {
        if !keep[i] {
            gap = true;
            continue;
        }
        if gap && !out.is_empty() {
            out.push_str("…\n");
        }
        gap = false;
        let l = match *l {
            m if m.starts_with("<<<<<<< ours") => "<<<<<<< main",
            m if m.starts_with("||||||| original") => "||||||| base",
            m if m.starts_with(">>>>>>> theirs") => ">>>>>>> this change",
            m => m,
        };
        out.push_str(l);
        out.push('\n');
    }
    if out.len() > HUNK_BYTES {
        let mut cut = HUNK_BYTES;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push_str("\n[cut]\n");
    }
    out
}

/// The "digest-index" generator: lines of `source/*.txt` sorted by file name, under a
/// SHA-256 digest of those lines. Byte-identical to the bench repo's `ci/gen_lock.py`.
pub fn digest_index(store: &Store, files: &Flat, source: &str) -> Result<String, Ejection> {
    use sha2::{Digest, Sha256};
    let prefix = format!("{}/", source.trim_end_matches('/'));
    let mut names: Vec<(&String, &FileEntry)> = files
        .iter()
        .filter(|(k, _)| k.strip_prefix(&prefix).is_some_and(|n| !n.contains('/') && n.ends_with(".txt")))
        .collect();
    names.sort_by(|a, b| a.0.cmp(b.0));
    let mut lines = vec![];
    for (_, e) in names {
        lines.push(String::from_utf8_lossy(store.blob(&e.oid).map_err(missing)?).trim().to_string());
    }
    let digest: String = Sha256::digest(lines.join("\n").as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    let mut out = format!("# digest: {digest}\n");
    for l in lines {
        out.push_str(&l);
        out.push('\n');
    }
    Ok(out)
}
