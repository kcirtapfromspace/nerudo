//! Trees and commits one at a time, for trunks too big to fetch whole (cf/scale/mono.md).
//!
//! Artifacts answers HTTP 500 to every git fetch of a repo with about 120,000 files in 19,000
//! directories, also for a commit with one changed file: the server walks the whole tree. Its
//! Workers binding still reads one tree (`readTree`, about 45 ms) or one commit
//! (`readCommit`). So when a git fetch of head's trees fails with HTTP 5xx, or gives more than
//! LAZY_TREES_BYTES, the trunk goes to "lazy" mode (kept in storage as `lazy_trees`): the
//! landing loop no longer fetches head's trees, and reads each tree when a gate or a landing
//! first needs it. A small trunk keeps one git fetch of head's trees: for it, that is faster
//! than one read for each tree. On any Artifacts trunk, an object that git did not give is
//! read through the binding.
//!
//! Each object read this way goes into this Durable Object's storage, so a new isolate reads
//! it from there. The storage keeps at most KEPT_MAX objects; then it starts again empty.
//!
//! The pure code (nerudo-core, the store) returns "missing object: <id> (<kind>)" when it
//! needs an object the store lacks. `fill` reads that object and runs the code again.

use super::*;
use crate::host::{reader, CommitInfo, RepoReader};
use crate::transport::base64;
use futures::StreamExt;
use nerudo_git::{Kind, Object, TreeEntry};

/// Objects kept in this object's storage, at most. More: the cache starts again empty.
const KEPT_MAX: u64 = 50_000;
/// Reads through the binding that run at once, for a page.
const READS_IN_FLIGHT: usize = 8;
/// Objects that one `fill` reads, at most.
const FILL_MAX: usize = 512;
/// Head's trees above this size put the trunk in lazy mode (about 25,000 files): a git fetch
/// of them takes seconds and stays in memory, while a landing reads only a few of them.
const LAZY_TREES_BYTES: usize = 1 << 20;

/// Where one run of the landing loop reads objects one at a time.
pub(super) struct Readers {
    /// The host is Artifacts, so the binding can read objects.
    artifacts: bool,
    /// Trunk's host (init reads objects before it stores the trunk's config).
    host: crate::types::HostCfg,
    /// The trunk is in lazy mode (see above).
    pub lazy: bool,
    trunk: Option<RepoReader>,
    /// Trunk's git remote, for what the binding does not read.
    pub(super) remote: Remote,
    forks: BTreeMap<String, RepoReader>,
    /// Objects read this run, by source: storage, binding, git.
    pub kept: u64,
    pub read: u64,
    pub git: u64,
}

impl Readers {
    /// Whether the binding can read objects (the host is Artifacts).
    pub(super) fn binding(&self) -> bool {
        self.artifacts
    }
}

/// The id and kind that a "missing object: <id> (<kind>)" error names.
pub(super) fn missing_in(detail: &str) -> Option<(Oid, String)> {
    // As Display ("missing object: <id> (tree)") or as Debug (Missing("<id> (tree)")).
    let start = ["missing object: ", "Missing(\""].iter().find_map(|m| detail.find(m).map(|i| i + m.len()))?;
    let rest = &detail[start..];
    let oid = Oid::from_hex(rest.get(..40)?).ok()?;
    let kind = rest[40..].strip_prefix(" (").and_then(|k| k.split(')').next()).unwrap_or("").to_string();
    Some((oid, kind))
}

/// Time zones from -12:00 to +14:00 in steps of 15 minutes, +0000 first.
fn zones() -> Vec<String> {
    let mut out = vec!["+0000".to_string()];
    for q in -48i32..=56 {
        if q != 0 {
            let m = q.abs() * 15;
            out.push(format!("{}{:02}{:02}", if q < 0 { '-' } else { '+' }, m / 60, m % 60));
        }
    }
    out
}

/// The git object of a commit that the binding gave as fields. The binding leaves out the time
/// zones and the message's last newline, so this tries each zone and both messages until the
/// object's id is `oid`. None: no candidate has that id (a signed commit, say).
fn commit_object(info: &CommitInfo, oid: &Oid) -> Option<Object> {
    let tree = Oid::from_hex(&info.tree).ok()?;
    let parents: Vec<Oid> = info.parents.iter().map(|p| Oid::from_hex(p)).collect::<std::result::Result<_, _>>().ok()?;
    let messages = [format!("{}\n", info.message), info.message.clone()];
    let zones = zones();
    let sig = |p: &(String, String, i64), tz: &str| Signature { name: p.0.clone(), email: p.1.clone(), when: p.2, tz: tz.to_string() };
    let attempt = |m: &str, az: &str, cz: &str| -> Option<Object> {
        let c = Commit { tree, parents: parents.clone(), author: sig(&info.author, az), committer: sig(&info.committer, cz), extra: vec![], message: m.to_string() };
        let o = Object::new(Kind::Commit, c.serialize());
        (o.oid() == *oid).then_some(o)
    };
    // The same zone for both first, then a committer at +0000 (the sequencer's own commits),
    // then every pair.
    for m in &messages {
        if let Some(o) = zones.iter().find_map(|z| attempt(m, z, z)) {
            return Some(o);
        }
        if let Some(o) = zones.iter().find_map(|z| attempt(m, z, "+0000")) {
            return Some(o);
        }
    }
    messages.iter().find_map(|m| zones.iter().find_map(|a| zones.iter().find_map(|c| attempt(m, a, c))))
}

/// The tree object for entries the binding gave, if its id is `oid`.
fn tree_object(entries: Vec<(u32, String, String)>, oid: &Oid) -> Option<Object> {
    let mut list: Vec<TreeEntry> = entries.into_iter().map(|(mode, name, h)| Some(TreeEntry { mode, name, oid: Oid::from_hex(&h).ok()? })).collect::<Option<_>>()?;
    let o = Object::new(Kind::Tree, nerudo_git::serialize_tree(&mut list));
    (o.oid() == *oid).then_some(o)
}

impl Trunk {
    /// Readers for one run of the landing loop: trunk's repo, and each fork's when first needed.
    pub(super) async fn readers(&self, cfg: &Config) -> Result<Readers> {
        let remote = Remote::new(&cfg.remote, Some(cfg.authorization.clone()));
        let artifacts = matches!(cfg.host, crate::types::HostCfg::Artifacts { .. });
        let lazy = artifacts && self.get::<bool>("lazy_trees").await?.unwrap_or(false);
        Ok(Readers { artifacts, lazy, host: cfg.host.clone(), trunk: None, remote, forks: BTreeMap::new(), kept: 0, read: 0, git: 0 })
    }

    /// Put the trunk in lazy mode: its head's trees are too big for a git fetch.
    pub(super) async fn go_lazy(&self, rd: &mut Readers) -> Result<()> {
        rd.lazy = true;
        console_warn!("objects: trunk's trees are too big to fetch whole; they now come one at a time");
        self.put("lazy_trees", &true).await
    }

    async fn trunk_reader<'a>(&self, rd: &'a mut Readers) -> Result<Option<&'a RepoReader>> {
        if rd.artifacts && rd.trunk.is_none() {
            if let crate::types::HostCfg::Artifacts { repo, .. } = &rd.host {
                rd.trunk = reader(&self.env, &rd.host, repo).await?;
            }
        }
        Ok(rd.trunk.as_ref())
    }

    async fn fork_reader<'a>(&self, rd: &'a mut Readers, fork: &str) -> Result<Option<&'a RepoReader>> {
        if !rd.artifacts {
            return Ok(None);
        }
        if !rd.forks.contains_key(fork) {
            // Fork names are the Artifacts repo names (host.rs).
            if let Some(r) = reader(&self.env, &rd.host, fork).await? {
                rd.forks.insert(fork.to_string(), r);
            }
        }
        Ok(rd.forks.get(fork))
    }

    /// Put object `oid` in `store`. `kind` is "tree", "commit", "blob" or "" (unknown). In
    /// order: this object's storage; the binding, from trunk's repo and then `fork`'s; git,
    /// from trunk. Ok(false): no source has it.
    pub(super) async fn resolve(&self, rd: &mut Readers, fork: Option<&str>, t: &FetchTransport, store: &mut Store, oid: Oid, kind: &str) -> Result<bool> {
        if store.contains(&oid) {
            return Ok(true);
        }
        let key = format!("o:{oid}");
        if let Some(v) = self.state.storage().get::<String>(&key).await? {
            let k = match v.get(..1) {
                Some("t") => Some(Kind::Tree),
                Some("c") => Some(Kind::Commit),
                _ => None,
            };
            if let (Some(k), Some(data)) = (k, super::import::base64_decode(&v[1..])) {
                let o = Object::new(k, data);
                if o.oid() == oid {
                    store.insert(o);
                    rd.kept += 1;
                    self.mem.borrow_mut().object_reads.1 += 1;
                    return Ok(true);
                }
            }
        }
        if rd.artifacts && kind != "blob" {
            let mut sources: Vec<String> = vec![String::new()];
            sources.extend(fork.map(str::to_string));
            for src in sources {
                let r = if src.is_empty() { self.trunk_reader(rd).await? } else { self.fork_reader(rd, &src).await? };
                let Some(r) = r else { continue };
                let hex = oid.to_hex();
                let got = match kind {
                    "commit" => r.commit(&hex).await?.and_then(|c| commit_object(&c, &oid)),
                    "tree" => {
                        let e = r.tree(&hex).await?;
                        let n = e.as_ref().map(|e| e.len());
                        let o = e.and_then(|e| tree_object(e, &oid));
                        if o.is_none() {
                            console_warn!("objects: readTree {hex} gave {n:?} entries, not this tree");
                        }
                        o
                    }
                    _ => match r.tree(&hex).await? {
                        Some(e) => tree_object(e, &oid),
                        None => r.commit(&hex).await?.and_then(|c| commit_object(&c, &oid)),
                    },
                };
                if let Some(o) = got {
                    let tag = if o.kind == Kind::Tree { "t" } else { "c" };
                    self.keep(&key, &format!("{tag}{}", base64(&o.data))).await?;
                    store.insert(o);
                    rd.read += 1;
                    self.mem.borrow_mut().object_reads.0 += 1;
                    return Ok(true);
                }
            }
        }
        // Git: a file by id, or (other hosts) a tree or a commit with its trees.
        rd.git += 1;
        let opts = if kind == "blob" { FetchOpts::default() } else { FetchOpts::TREES };
        match fetch_with(t, &rd.remote, &[oid], &[], opts, store).await {
            Ok(_) => Ok(store.contains(&oid)),
            Err(e) if e.transient() && !rd.artifacts => Err(g(e)),
            Err(_) => Ok(false),
        }
    }

    /// Run `f` until it needs no object the store lacks, reading each one it names with
    /// `resolve` (at most FILL_MAX). Returns `f`'s last result.
    pub(super) async fn fill<T, E: std::fmt::Debug>(
        &self,
        rd: &mut Readers,
        fork: Option<&str>,
        t: &FetchTransport,
        store: &mut Store,
        mut f: impl FnMut(&Store) -> std::result::Result<T, E>,
    ) -> Result<std::result::Result<T, E>> {
        for _ in 0..FILL_MAX {
            let out = f(store);
            let Err(e) = &out else { return Ok(out) };
            let Some((oid, kind)) = missing_in(&format!("{e:?}")) else { return Ok(out) };
            if !self.resolve(rd, fork, t, store, oid, &kind).await? {
                return Ok(out);
            }
        }
        Ok(f(store))
    }

    /// Put commit `oid` (trunk's head, or a commit trunk moved to) in `store`. On Artifacts the
    /// commit alone; elsewhere with its trees, by git (`have`: a commit the store has).
    pub(super) async fn load_commit(&self, rd: &mut Readers, t: &FetchTransport, store: &mut Store, oid: Oid, have: Option<Oid>) -> Result<()> {
        if rd.lazy && self.resolve(rd, None, t, store, oid, "commit").await? {
            return Ok(());
        }
        let haves: Vec<Oid> = have.into_iter().collect();
        let before = store.bytes();
        match fetch_with(t, &rd.remote, &[oid], &haves, FetchOpts::TREES, store).await {
            Ok(_) => {
                if rd.artifacts && have.is_none() && store.bytes() - before > LAZY_TREES_BYTES {
                    self.go_lazy(rd).await?;
                }
                Ok(())
            }
            // Artifacts cannot send trees this big: from now on, one at a time.
            Err(nerudo_git::Error::Status(s, _)) if s >= 500 && rd.artifacts => {
                self.go_lazy(rd).await?;
                if self.resolve(rd, None, t, store, oid, "commit").await? {
                    Ok(())
                } else {
                    Err(Error::RustError(format!("cannot read trunk commit {oid}")))
                }
            }
            Err(e) => Err(g(e)),
        }
    }

    /// The type check reads every TypeScript and JSON file of the tree: read the trees and
    /// fetch the files the store lacks. Its cost follows the size of those files, not of the change.
    pub(super) async fn typecheck_files(&self, rd: &mut Readers, t: &FetchTransport, store: &mut Store, commit: &Oid) -> Result<()> {
        let flat = self.fill(rd, None, t, store, |s| s.commit(commit).and_then(|c| s.flatten(&c.tree))).await?.map_err(g)?;
        let want: BTreeSet<Oid> = flat
            .into_iter()
            .filter(|(p, e)| crate::ci::EXTENSIONS.iter().any(|x| p.ends_with(x)) && e.mode != nerudo_git::MODE_GITLINK && !store.contains(&e.oid))
            .map(|(_, e)| e.oid)
            .collect();
        let want: Vec<Oid> = want.into_iter().collect();
        for chunk in want.chunks(2000) {
            fetch_with(t, &rd.remote, chunk, &[], FetchOpts::default(), store).await.map_err(g)?;
        }
        Ok(())
    }

    /// Read into the UI's copy of trunk the objects a page missed: from storage, files by
    /// one git request, and trees and commits through the binding, READS_IN_FLIGHT at once.
    pub(super) async fn read_ui_misses(&self, misses: &[(Oid, &'static str)]) -> Result<()> {
        let cfg = self.cfg().await?;
        let mut rd = self.readers(&cfg).await?;
        let t = FetchTransport::default();
        let mut store = self.index_store().await?;
        let out = self.resolve_many(&mut rd, &t, &mut store, misses).await;
        drop(store);
        out
    }

    /// On a lazy trunk, run `read` (a read of the UI's copy of trunk) until it misses nothing,
    /// reading what it missed each time (at most UI_READ_ROUNDS times). Then a write that
    /// reads the same objects finds them. Elsewhere it does nothing.
    pub(super) async fn warm_ui<F, Fut, T>(&self, mut read: F) -> Result<()>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        if !self.get::<bool>("lazy_trees").await?.unwrap_or(false) {
            return Ok(());
        }
        for _ in 0..super::UI_READ_ROUNDS {
            let _ = read().await;
            let misses = self.mem.borrow().ui.take_misses();
            if misses.is_empty() {
                break;
            }
            self.read_ui_misses(&misses).await?;
        }
        Ok(())
    }

    /// `resolve` for many objects of trunk at once.
    pub(super) async fn resolve_many(&self, rd: &mut Readers, t: &FetchTransport, store: &mut Store, misses: &[(Oid, &'static str)]) -> Result<()> {
        let mut left: Vec<(Oid, &'static str)> = vec![];
        let wanted: Vec<(Oid, &'static str)> = misses.iter().filter(|(o, _)| !store.contains(o)).copied().collect();
        for (oid, kind) in wanted {
            if let Some(v) = self.state.storage().get::<String>(&format!("o:{oid}")).await? {
                let k = match v.get(..1) {
                    Some("t") => Some(Kind::Tree),
                    Some("c") => Some(Kind::Commit),
                    _ => None,
                };
                if let (Some(k), Some(data)) = (k, super::import::base64_decode(&v[1..])) {
                    let o = Object::new(k, data);
                    if o.oid() == oid {
                        store.insert(o);
                        continue;
                    }
                }
            }
            left.push((oid, kind));
        }
        let blobs: Vec<Oid> = left.iter().filter(|(_, k)| *k == "blob").map(|(o, _)| *o).collect();
        if !blobs.is_empty() {
            if let Err(e) = fetch_with(t, &rd.remote, &blobs, &[], FetchOpts::default(), store).await {
                console_warn!("objects: files for a page: {e}");
            }
        }
        let others: Vec<(Oid, &'static str)> = left.into_iter().filter(|(_, k)| *k != "blob").collect();
        if others.is_empty() {
            return Ok(());
        }
        let Some(r) = self.trunk_reader(rd).await? else {
            for (oid, kind) in others {
                self.resolve(rd, None, t, store, oid, kind).await?;
            }
            return Ok(());
        };
        let reads: Vec<Option<Object>> = futures::stream::iter(others.iter().map(|(oid, kind)| async move {
            let hex = oid.to_hex();
            match *kind {
                "commit" => r.commit(&hex).await.ok().flatten().and_then(|c| commit_object(&c, oid)),
                "tree" => r.tree(&hex).await.ok().flatten().and_then(|e| tree_object(e, oid)),
                _ => match r.tree(&hex).await.ok().flatten() {
                    Some(e) => tree_object(e, oid),
                    None => r.commit(&hex).await.ok().flatten().and_then(|c| commit_object(&c, oid)),
                },
            }
        }))
        .buffered(READS_IN_FLIGHT)
        .collect()
        .await;
        for o in reads.into_iter().flatten() {
            let tag = if o.kind == Kind::Tree { "t" } else { "c" };
            self.keep(&format!("o:{}", o.oid()), &format!("{tag}{}", base64(&o.data))).await?;
            store.insert(o);
            self.mem.borrow_mut().object_reads.0 += 1;
        }
        Ok(())
    }

    /// Keep an object in storage, and empty the cache when it holds KEPT_MAX objects.
    async fn keep(&self, key: &str, value: &str) -> Result<()> {
        let count: u64 = self.get("o_count").await?.unwrap_or(0);
        if count >= KEPT_MAX {
            loop {
                let map = self.state.storage().list_with_options(ListOptions::new().prefix("o:").limit(512)).await?;
                let keys: Vec<String> = map.keys().into_iter().filter_map(|k| k.ok().and_then(|k| k.as_string())).collect();
                if keys.is_empty() {
                    break;
                }
                self.state.storage().delete_multiple(keys).await?;
            }
            self.put_all(vec![(key.to_string(), value.to_string()), ("o_count".into(), "1".into())]).await
        } else {
            self.put_all(vec![(key.to_string(), value.to_string()), ("o_count".into(), (count + 1).to_string())]).await
        }
    }

    /// Keep a commit the sequencer made (a new head), so that a new isolate reads it from storage.
    pub(super) async fn keep_commit(&self, store: &Store, oid: &Oid) -> Result<()> {
        if let Ok(o) = store.get(oid) {
            self.keep(&format!("o:{oid}"), &format!("c{}", base64(&o.data))).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tree_from_the_binding_has_its_id() {
        // The root tree of a test repo, as readTree gives it.
        let e = |m: &str, n: &str, h: &str| (u32::from_str_radix(m, 8).unwrap(), n.to_string(), h.to_string());
        let entries = vec![
            e("100644", "CHANGELOG.md", "825c32f0d03d98995ebe3e6d797f14daf2df51d9"),
            e("100644", "README.md", "fccd80d65247b686a4e11d822f5000cc69dc0489"),
            e("40000", "ci", "777710f3abf7695c4ebab327d070e7274a4df637"),
            e("40000", "deps", "bc3b2da54c524b6114aabf0cd4833b38cc384815"),
            e("100644", "deps.lock", "476f5eaa5349c65d9b8ea80cc10ec8d62f90a03a"),
            e("100644", "landseq.toml", "17c28d110c5692c1f50342ae2796153bc2731e68"),
            e("40000", "migrations", "ae9f92dea493ccc32c7d734a4016643be7d3b45a"),
            e("40000", "svc", "64059ed0a56a140b6591580daf1175e4f52250e2"),
        ];
        let oid = Oid::from_hex("adf9a370a1a41de44c4cbcd3e400c4e4121df686").unwrap();
        let o = tree_object(entries, &oid);
        assert!(o.is_some());
    }

    #[test]
    fn a_missing_object_names_its_id_and_kind() {
        let id = "adf9a370a1a41de44c4cbcd3e400c4e4121df686";
        let want = Some((Oid::from_hex(id).unwrap(), "tree".to_string()));
        assert_eq!(missing_in(&format!("{:?}", nerudo_git::Error::Missing(format!("{id} (tree)")))), want);
        assert_eq!(missing_in(&nerudo_git::Error::Missing(format!("{id} (tree)")).to_string()), want);
        let e = nerudo_core::Ejection::Missing { detail: nerudo_git::Error::Missing(format!("{id} (tree)")).to_string() };
        assert_eq!(missing_in(&format!("{e:?}")), want);
        assert_eq!(missing_in("missing object: abc"), None);
    }
}
