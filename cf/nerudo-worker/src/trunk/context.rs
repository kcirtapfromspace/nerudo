//! Agent context: the plan, the decisions and the rejected alternatives an agent records
//! with a change, so that later readers (people and other agents) know why, not only what.
//!
//! Two copies, two jobs:
//! - The agent's working copy is in its own fork, on an orphan branch `nerudo/context`,
//!   as `changes/<change>/rev-<n>.md`. The agent can rewrite that branch at will.
//! - The record is the sequencer's copy, in a repo only this object writes:
//!   `<trunk>--nerudo-context` in Artifacts. At land the object checks the agent's commit,
//!   then adds the file as `changes/<change>/rev-<n>/v<k>.md` in a new commit, with its
//!   source (fork@commit) in the front matter, and compare-and-swaps that repo's main. A
//!   file is added once and never changed; context sent again for the same revision is the
//!   next version (v2, v3...). Each version also gets a tag, context/<change>/rev-<n>/v<k>.
//!
//! The landed commit's `Context: <trunk>--nerudo-context@<commit>` trailer pins the
//! version the revision was sent with; the receipt keeps the fork pointer as provenance,
//! and `nerudo verify` checks that the two agree. Agents and the app only ever get read
//! tokens for the context repo. (The name can't be a fork's: agent names have no `-`.)
//!
//!   GET context/{change}[?rev=&v=]              agents' API and app API
//!   GET context-repo                            agents' API: its remote and a read token
//!   GET agent/{name}/context[?commit=&path=]    app API: an agent's working copy

use super::ui::agent_of;
use super::*;
use crate::types::HostCfg;
use serde_json::{json, Value};
use std::cell::Cell;
use std::collections::HashMap;

pub(super) const CONTEXT_BRANCH: &str = "refs/heads/nerudo/context";
pub(super) const LEGACY_CONTEXT_BRANCH: &str = "refs/heads/landseq/context";
pub(super) fn is_context_branch(name: &str) -> bool { name == CONTEXT_BRANCH || name == LEGACY_CONTEXT_BRANCH }
pub(super) const CONTEXT_TRAILER: &str = "Context";
/// The largest context file the object accepts at land and shows.
pub(super) const MAX_CONTEXT_BYTES: usize = 256 * 1024;
/// Commits of an agent's working copy that the app lists.
const HISTORY: usize = 50;
const COPY_MAIN: &str = "refs/heads/main";
/// Front-matter keys only the sequencer writes; an agent's own are dropped from the copy.
const OWN_KEYS: [&str; 3] = ["source:", "version:", "copied:"];

/// The repo this object keeps context in. Only the object holds its write token.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct ContextRepo {
    name: String,
    remote: String,
    authorization: String,
    created_ms: i64,
}

/// One version of a revision's context in the context repo.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Version {
    v: u32,
    repo: String,
    /// The context repo commit that added this version.
    commit: Oid,
    path: String,
    source_fork: String,
    source_commit: Oid,
    at_ms: i64,
}

thread_local! {
    /// Read tokens by repo name (forks and context repos): (Authorization, expiry in ms).
    static READ_TOKENS: RefCell<HashMap<String, (String, f64)>> = RefCell::new(HashMap::new());
    /// Context repo objects this isolate has seen, by repo name, with the tips it fetched.
    static OWNED: RefCell<HashMap<String, (Store, Vec<Oid>)>> = RefCell::new(HashMap::new());
    /// One copy at a time, so two lands don't race to create or push the context repo.
    static COPYING: Cell<bool> = const { Cell::new(false) };
}

struct CopyLock;

impl CopyLock {
    async fn take() -> CopyLock {
        while COPYING.with(|c| c.replace(true)) {
            Delay::from(Duration::from_millis(50)).await;
        }
        CopyLock
    }
}

impl Drop for CopyLock {
    fn drop(&mut self) {
        COPYING.with(|c| c.set(false));
    }
}

/// Where a change's context lives on an agent's working-copy branch.
pub(super) fn context_path(cid: &str, rev: u32) -> String {
    format!("changes/{cid}/rev-{rev}.md")
}

/// Where version `v` of a revision's context lives in the context repo.
fn copy_path(cid: &str, rev: u32, v: u32) -> String {
    format!("changes/{cid}/rev-{rev}/v{v}.md")
}

fn repo_name(cfg: &Config) -> String {
    let base = match &cfg.host {
        HostCfg::Artifacts { repo, .. } => repo.clone(),
        HostCfg::Forgejo { repo, .. } => repo.clone(),
    };
    format!("{base}--nerudo-context")
}

/// A tag name for a version, when the change id makes a valid ref (no `..`, no `.lock`).
fn tag(cid: &str, rev: u32, v: u32) -> Option<String> {
    let ok = !cid.contains("..") && !cid.starts_with('.') && !cid.ends_with(".lock") && !cid.ends_with('.');
    ok.then(|| format!("refs/tags/context/{cid}/rev-{rev}/v{v}"))
}

/// The copy's text: the agent's front matter (without keys only the sequencer writes), then
/// where it came from, which version it is, and when the sequencer copied it.
fn with_provenance(text: &[u8], source: &str, v: u32, at_ms: i64) -> Vec<u8> {
    let t = String::from_utf8_lossy(text);
    let ours = format!("source: {source}\nversion: v{v}\ncopied: {}\n", iso(at_ms));
    if let Some(rest) = t.strip_prefix("---\n") {
        if let Some(i) = rest.find("\n---\n") {
            let theirs: Vec<&str> = rest[..i].lines().filter(|l| !OWN_KEYS.iter().any(|k| l.starts_with(k))).collect();
            return format!("---\n{}\n{ours}---\n{}", theirs.join("\n"), &rest[i + 5..]).into_bytes();
        }
    }
    format!("---\n{ours}---\n\n{t}").into_bytes()
}

/// UTC time as 2026-10-11T16:30:00Z.
fn iso(ms: i64) -> String {
    let s = ms.div_euclid(1000);
    let (days, secs) = (s.div_euclid(86_400), s.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, secs % 3600 / 60, secs % 60)
}

/// One file at `commit`, or None when the commit has no such path.
fn read_file(store: &Store, commit: &Oid, path: &str) -> std::result::Result<Option<Vec<u8>>, String> {
    let c = store.commit(commit).map_err(|e| e.to_string())?;
    let files = store.flatten(&c.tree).map_err(|e| e.to_string())?;
    match files.get(path) {
        Some(e) => store.blob(&e.oid).map(|b| Some(b.to_vec())).map_err(|e| e.to_string()),
        None => Ok(None),
    }
}

/// Text for the app: UTF-8 (lossy), cut at MAX_CONTEXT_BYTES.
fn shown(bytes: &[u8]) -> (String, bool) {
    let cut = bytes.len() > MAX_CONTEXT_BYTES;
    (String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_CONTEXT_BYTES)]).into_owned(), cut)
}

/// The context repo's objects this isolate keeps, fetched up to `want` if needed.
async fn owned_store(remote: &Remote, repo: &str, want: Option<Oid>) -> std::result::Result<(Store, Vec<Oid>), String> {
    let (mut store, mut tips) = OWNED.with(|m| m.borrow_mut().remove(repo)).unwrap_or_default();
    if let Some(w) = want.filter(|w| !store.contains(w)) {
        let haves: Vec<Oid> = tips.iter().filter(|o| store.contains(o)).copied().collect();
        if let Err(e) = fetch(&FetchTransport::default(), remote, &[w], &haves, &mut store).await {
            OWNED.with(|m| m.borrow_mut().insert(repo.to_string(), (store, tips)));
            return Err(e.to_string());
        }
        tips.push(w);
    }
    Ok((store, tips))
}

fn keep(repo: &str, store: Store, tips: Vec<Oid>) {
    OWNED.with(|m| m.borrow_mut().insert(repo.to_string(), (store, tips)));
}

impl Trunk {
    /// A read-only Authorization for `repo` (a fork or the context repo), kept 9 minutes.
    async fn reader(&self, cfg: &Config, repo: &str, fork: Option<&Fork>) -> Result<String> {
        let at = now();
        let cached = READ_TOKENS.with(|m| m.borrow().get(repo).filter(|(_, until)| *until > at).map(|(a, _)| a.clone()));
        if let Some(a) = cached {
            return Ok(a);
        }
        let a = match fork {
            Some(f) => host::fork_read_authorization(&self.env, &cfg.host, f).await?,
            None => host::repo_read_authorization(&self.env, &cfg.host, repo).await?,
        };
        let until = at + (host::READ_TOKEN_TTL_S - 60.0) * 1000.0;
        READ_TOKENS.with(|m| m.borrow_mut().insert(repo.to_string(), (a.clone(), until)));
        Ok(a)
    }

    /// The context repo, created the first time a change brings context. None on a host
    /// where the object can't own a repo (the local Forgejo stand-in).
    async fn context_repo(&self, cfg: &Config) -> Result<std::result::Result<Option<ContextRepo>, String>> {
        if let Some(r) = self.get::<ContextRepo>("ctxrepo").await? {
            return Ok(Ok(Some(r)));
        }
        if !matches!(cfg.host, HostCfg::Artifacts { .. }) {
            return Ok(Ok(None));
        }
        let name = repo_name(cfg);
        match host::create_owned_repo(&self.env, &cfg.host, &name).await {
            Ok((remote, authorization)) => {
                let r = ContextRepo { name, remote, authorization, created_ms: now() as i64 };
                self.put_all(vec![("ctxrepo".into(), serde_json::to_string(&r)?)]).await?;
                Ok(Ok(Some(r)))
            }
            Err(e) => Ok(Err(format!("cannot create the context repo {name}: {e}"))),
        }
    }

    /// At land: check the agent's context commit (in its fork, with this change's file, at
    /// most MAX_CONTEXT_BYTES), then keep an immutable copy in the context repo. Err(what is
    /// wrong) for a 400. Sending the same context again finds the version it made before.
    pub(super) async fn keep_context(&self, fork: &Fork, req: &LandReq) -> Result<std::result::Result<(), String>> {
        let Some(commit) = req.context_commit else {
            return Ok(Ok(()));
        };
        let Some(cfg) = self.get::<Config>("cfg").await? else {
            return Ok(Err("trunk is not set up".into()));
        };
        let remote = Remote::new(&fork.remote, Some(self.reader(&cfg, &fork.name, Some(fork)).await?));
        let mut store = Store::new();
        if let Err(e) = fetch(&FetchTransport::default(), &remote, &[commit], &[], &mut store).await {
            return Ok(Err(format!("context_commit {} is not in fork {}: {e}", commit.to_hex(), fork.name)));
        }
        let path = context_path(&req.change_id, req.rev);
        let bytes = match read_file(&store, &commit, &path) {
            Ok(Some(b)) if b.len() <= MAX_CONTEXT_BYTES => b,
            Ok(Some(_)) => return Ok(Err(format!("{path} is larger than {} KiB", MAX_CONTEXT_BYTES / 1024))),
            Ok(None) => return Ok(Err(format!("context_commit {} has no {path}", commit.to_hex()))),
            Err(e) => return Ok(Err(format!("context_commit {}: {e}", commit.to_hex()))),
        };
        self.copy_context(&cfg, req, &bytes).await
    }

    /// Add `bytes` as the next version of (change, revision) in the context repo.
    async fn copy_context(&self, cfg: &Config, req: &LandReq, bytes: &[u8]) -> Result<std::result::Result<(), String>> {
        let Some(source) = req.context_commit else {
            return Ok(Ok(()));
        };
        let _lock = CopyLock::take().await;
        let key = format!("cv:{}:{}", req.change_id, req.rev);
        let mut versions: Vec<Version> = self.get(&key).await?.unwrap_or_default();
        if versions.iter().any(|v| v.source_commit == source && v.source_fork == req.fork) {
            return Ok(Ok(()));
        }
        let repo = match self.context_repo(cfg).await? {
            Ok(Some(r)) => r,
            Ok(None) => return Ok(Ok(())), // no owned repo on this host: the trailer points at the fork
            Err(e) => return Ok(Err(e)),
        };
        let remote = Remote::new(&repo.remote, Some(repo.authorization.clone()));
        let t = FetchTransport::default();
        let v = versions.iter().map(|x| x.v).max().unwrap_or(0) + 1;
        let path = copy_path(&req.change_id, req.rev, v);
        let at = now() as i64;
        let text = with_provenance(bytes, &format!("{}@{}", req.fork, source.to_hex()), v, at);
        let mut last = String::new();
        for _ in 0..3 {
            let tip = match ls_refs(&t, &remote, COPY_MAIN).await {
                Ok(r) => r.into_iter().find(|(n, _)| n == COPY_MAIN).map(|(_, o)| o),
                Err(e) => return Ok(Err(format!("cannot read the context repo: {e}"))),
            };
            let (mut store, tips) = match owned_store(&remote, &repo.name, tip).await {
                Ok(s) => s,
                Err(e) => return Ok(Err(format!("cannot read the context repo: {e}"))),
            };
            let mut files = match tip {
                Some(tp) => store.commit(&tp).and_then(|c| store.flatten(&c.tree)).map_err(g)?,
                None => BTreeMap::new(),
            };
            if files.contains_key(&path) {
                keep(&repo.name, store, tips);
                return Ok(Err(format!("{path} is already in the context repo")));
            }
            files.insert(path.clone(), FileEntry { mode: MODE_BLOB, oid: store.put_blob(&text) });
            let tree = store.write_tree(&files);
            let sig = Signature::new("nerudo", "nerudo@nerudo.invalid", at / 1000);
            let message = format!(
                "context: {} rev {} v{v}\n\nSource: {}@{}\nChange-Id: {}\nRevision: {}\n",
                req.change_id,
                req.rev,
                req.fork,
                source.to_hex(),
                req.change_id,
                req.rev
            );
            let commit = store.put_commit(&Commit { tree, parents: tip.into_iter().collect(), author: sig.clone(), committer: sig, extra: vec![], message });
            let stops: Vec<Oid> = tip.into_iter().collect();
            let objects = store.missing_from(&commit, &stops).and_then(|m| store.object_list(&m).map(|l| write_pack(&l))).map_err(g)?;
            let mut updates = vec![RefUpdate { name: COPY_MAIN.into(), old: tip.unwrap_or(Oid::ZERO), new: commit }];
            if let Some(name) = tag(&req.change_id, req.rev, v) {
                updates.push(RefUpdate { name, old: Oid::ZERO, new: commit });
            }
            match push(&t, &remote, &updates, objects).await {
                Ok(report) if report.all_ok() => {
                    let mut tips = tips;
                    tips.push(commit);
                    keep(&repo.name, store, tips);
                    versions.push(Version { v, repo: repo.name.clone(), commit, path, source_fork: req.fork.clone(), source_commit: source, at_ms: at });
                    self.put_all(vec![(key, serde_json::to_string(&versions)?)]).await?;
                    return Ok(Ok(()));
                }
                Ok(report) => last = format!("{:?}", report.refs),
                Err(e) => last = e.to_string(),
            }
            keep(&repo.name, store, tips);
        }
        Ok(Err(format!("cannot add the context to the context repo: {last}")))
    }

    /// The landed commit's trailers, with the link to the version of the context that this
    /// revision was sent with: the sequencer's copy, or (no owned repo) the agent's fork.
    pub(super) async fn context_trailers(&self, mut trailers: Vec<String>, req: &LandReq) -> Result<Vec<String>> {
        let Some(source) = req.context_commit else {
            return Ok(trailers);
        };
        let versions: Vec<Version> = self.get(&format!("cv:{}:{}", req.change_id, req.rev)).await?.unwrap_or_default();
        // Repo names and object ids carry no spaces or newlines, so the line can't forge
        // another trailer.
        trailers.push(match versions.iter().find(|v| v.source_commit == source && v.source_fork == req.fork) {
            Some(v) => format!("{CONTEXT_TRAILER}: {}@{}", v.repo, v.commit.to_hex()),
            None => format!("{CONTEXT_TRAILER}: {}@{}", req.fork, source.to_hex()),
        });
        Ok(trailers)
    }

    /// The version of a revision's context that the revision was sent with, and the context
    /// repo commit that added it. None: no context, or no copy (a host without an owned repo).
    pub(super) async fn sent_version(&self, req: &LandReq) -> Result<Option<(u32, Oid)>> {
        let Some(source) = req.context_commit else {
            return Ok(None);
        };
        let versions: Vec<Version> = self.get(&format!("cv:{}:{}", req.change_id, req.rev)).await?.unwrap_or_default();
        Ok(versions.iter().find(|v| v.source_commit == source && v.source_fork == req.fork).map(|v| (v.v, v.commit)))
    }

    /// The context repo commit the landed commit pins, from its Context: trailer.
    async fn pinned(&self, cfg: &Config, rec: &ChangeRec) -> Result<Option<Oid>> {
        let (Phase::Landed, Some(out)) = (&rec.state, rec.trunk_commit) else {
            return Ok(None);
        };
        let meta: Meta = self.get("meta").await?.unwrap_or_default();
        let store = self.ui_store(cfg, meta.head).await?;
        let pin = store.commit(&out).ok().and_then(|c| c.trailer(CONTEXT_TRAILER)).and_then(|t| t.rsplit_once('@').and_then(|(_, h)| Oid::from_hex(h).ok()));
        self.ui_store_back(store);
        Ok(pin)
    }

    /// Agents' API: GET context/{change}[?rev=&v=].
    pub(super) async fn api_context(&self, cid: &str, req: &Request) -> Result<Response> {
        let Some(cfg) = self.get::<Config>("cfg").await? else {
            return bad(404, "unknown trunk");
        };
        let url = req.url()?;
        let q = |k: &str| url.query_pairs().find(|(n, _)| n == k).and_then(|(_, v)| v.parse().ok());
        self.context_json(&cfg, cid, q("rev"), q("v")).await
    }

    /// Agents' API: GET context-repo, for `nerudo verify`: its remote and a read token.
    pub(super) async fn api_context_repo(&self) -> Result<Response> {
        let Some(cfg) = self.get::<Config>("cfg").await? else {
            return bad(404, "unknown trunk");
        };
        let Some(r) = self.get::<ContextRepo>("ctxrepo").await? else {
            return bad(404, "no change has brought context yet");
        };
        let authorization = self.reader(&cfg, &r.name, None).await?;
        Response::from_json(&json!({ "name": r.name, "remote": r.remote, "branch": COPY_MAIN, "authorization": authorization }))
    }

    /// A change's context for one revision: its versions in the context repo (which one the
    /// landed commit pins, and which one the revision was sent with) and the text of one.
    pub(super) async fn context_json(&self, cfg: &Config, cid: &str, rev: Option<u32>, want: Option<u32>) -> Result<Response> {
        let Some(latest) = self.get::<u32>(&format!("latest:{cid}")).await? else {
            return bad(404, format!("unknown change {cid}"));
        };
        let rev = rev.unwrap_or(latest);
        let Some(rec) = self.get::<ChangeRec>(&format!("c:{cid}:{rev}")).await? else {
            return bad(404, format!("unknown change {cid} rev {rev}"));
        };
        let mut revisions = vec![];
        for n in 1..=latest {
            if let Some(x) = self.get::<ChangeRec>(&format!("c:{cid}:{n}")).await? {
                if let Some(c) = x.req.context_commit {
                    revisions.push(json!({ "rev": n, "commit": c, "state": x.state }));
                }
            }
        }
        let versions: Vec<Version> = self.get(&format!("cv:{cid}:{rev}")).await?.unwrap_or_default();
        let pin = self.pinned(cfg, &rec).await?;
        let sent = |v: &Version| Some(v.source_commit) == rec.req.context_commit && v.source_fork == rec.req.fork;
        let mut out = json!({
            "change_id": cid, "rev": rev, "agent": agent_of(&cfg.trunk, &rec.req.fork), "fork": rec.req.fork,
            "branch": CONTEXT_BRANCH, "revisions": revisions, "commit": rec.req.context_commit, "text": Value::Null,
            "repo": versions.first().map(|v| v.repo.clone()),
            "versions": versions.iter().map(|v| json!({
                "v": v.v, "commit": v.commit, "path": v.path, "source_fork": v.source_fork, "source_commit": v.source_commit,
                "at_ms": v.at_ms, "pinned": Some(v.commit) == pin, "sent": sent(v),
            })).collect::<Vec<_>>(),
        });
        let chosen = want
            .and_then(|w| versions.iter().find(|v| v.v == w))
            .or_else(|| versions.iter().find(|v| Some(v.commit) == pin))
            .or_else(|| versions.iter().find(|v| sent(v)))
            .or(versions.last());
        let Some(v) = chosen else {
            return self.fork_context(cfg, &rec, out).await;
        };
        out["v"] = json!(v.v);
        out["path"] = json!(v.path);
        out["immutable"] = json!(true);
        let auth = self.reader(cfg, &v.repo, None).await?;
        let Some(repo) = self.get::<ContextRepo>("ctxrepo").await? else {
            out["error"] = json!("the context repo is not recorded");
            return Response::from_json(&out);
        };
        let remote = Remote::new(&repo.remote, Some(auth));
        match owned_store(&remote, &v.repo, Some(v.commit)).await {
            Ok((store, tips)) => {
                match read_file(&store, &v.commit, &v.path) {
                    Ok(Some(b)) => {
                        let (text, cut) = shown(&b);
                        out["text"] = json!(text);
                        out["truncated"] = json!(cut);
                        out["size"] = json!(b.len());
                    }
                    Ok(None) => out["error"] = json!(format!("{} is not in {}", v.path, v.commit.to_hex())),
                    Err(e) => out["error"] = json!(e),
                }
                keep(&v.repo, store, tips);
            }
            Err(e) => out["error"] = json!(format!("cannot read the context repo: {e}")),
        }
        Response::from_json(&out)
    }

    /// No copy (a host without an owned repo): read the revision's file from the fork.
    async fn fork_context(&self, cfg: &Config, rec: &ChangeRec, mut out: Value) -> Result<Response> {
        let Some(commit) = rec.req.context_commit else {
            return Response::from_json(&out);
        };
        let path = context_path(&rec.req.change_id, rec.req.rev);
        out["path"] = json!(path);
        out["immutable"] = json!(false);
        let Some(fork) = self.get::<Fork>(&format!("f:{}", rec.req.fork)).await? else {
            out["error"] = json!("the fork is gone");
            return Response::from_json(&out);
        };
        let remote = Remote::new(&fork.remote, Some(self.reader(cfg, &fork.name, Some(&fork)).await?));
        let mut store = Store::new();
        if let Err(e) = fetch(&FetchTransport::default(), &remote, &[commit], &[], &mut store).await {
            out["error"] = json!(format!("cannot read the context from the fork: {e}"));
            return Response::from_json(&out);
        }
        match read_file(&store, &commit, &path) {
            Ok(Some(b)) => {
                let (text, cut) = shown(&b);
                out["text"] = json!(text);
                out["truncated"] = json!(cut);
                out["size"] = json!(b.len());
            }
            Ok(None) => out["error"] = json!(format!("{path} is not in the context commit")),
            Err(e) => out["error"] = json!(e),
        }
        Response::from_json(&out)
    }

    /// App API: an agent's working copy (its fork's nerudo/context branch), newest first,
    /// and one file from it. The agent can rewrite this branch; the record is the context repo.
    pub(super) async fn agent_context_json(&self, cfg: &Config, name: &str, at: Option<&str>, path: Option<&str>) -> Result<Response> {
        let Some(fork) = self.get::<Fork>(&format!("fa:{name}")).await? else {
            return bad(404, "unknown agent");
        };
        let t = FetchTransport::default();
        let remote = Remote::new(&fork.remote, Some(self.reader(cfg, &fork.name, Some(&fork)).await?));
        let record = self.get::<ContextRepo>("ctxrepo").await?.map(|r| r.name);
        let mut out = json!({ "agent": name, "fork": fork.name, "branch": CONTEXT_BRANCH, "tip": Value::Null, "commits": [], "working_copy": true, "record": record });
        let refs = match ls_refs(&t, &remote, "refs/heads/").await {
            Ok(r) => r,
            Err(e) => {
                out["error"] = json!(format!("cannot read the fork: {e}"));
                return Response::from_json(&out);
            }
        };
        let current = refs.iter().find(|(n, _)| n == CONTEXT_BRANCH).or_else(|| refs.iter().find(|(n, _)| n == LEGACY_CONTEXT_BRANCH));
        let Some((branch, tip)) = current else {
            return Response::from_json(&out);
        };
        let tip = *tip;
        out["branch"] = json!(branch);
        out["tip"] = json!(tip);
        let mut store = Store::new();
        if let Err(e) = fetch(&t, &remote, &[tip], &[], &mut store).await {
            out["error"] = json!(format!("cannot read the context branch: {e}"));
            return Response::from_json(&out);
        }
        let mut commits = vec![];
        let mut cur = Some(tip);
        while let Some(oid) = cur.filter(|o| store.contains(o)) {
            if commits.len() >= HISTORY {
                break;
            }
            let c = store.commit(&oid).map_err(g)?;
            let now_files = store.flatten(&c.tree).map_err(g)?;
            let before = match c.parents.first().and_then(|p| store.commit(p).ok()) {
                Some(p) => store.flatten(&p.tree).map_err(g)?,
                None => BTreeMap::new(),
            };
            let changed: Vec<&String> = now_files.iter().filter(|(p, e)| before.get(*p) != Some(*e)).map(|(p, _)| p).collect();
            commits.push(json!({
                "commit": oid, "subject": c.message.lines().next().unwrap_or(""), "at_ms": c.committer.when * 1000, "paths": changed,
            }));
            cur = c.parents.first().copied();
        }
        out["commits"] = json!(commits);
        if let Some(path) = path {
            let at = match at {
                Some(h) => match Oid::from_hex(h) {
                    Ok(o) if store.contains(&o) => o,
                    _ => return bad(404, "that commit is not on this agent's context branch"),
                },
                None => tip,
            };
            match read_file(&store, &at, path) {
                Ok(Some(b)) => {
                    let (text, cut) = shown(&b);
                    out["file"] = json!({ "commit": at, "path": path, "text": text, "truncated": cut, "size": b.len() });
                }
                Ok(None) => return bad(404, format!("{path} is not at {}", at.to_hex())),
                Err(e) => return bad(500, e),
            }
        }
        Response::from_json(&out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provenance_replaces_the_agents_own_keys() {
        let doc = b"---\nchange: x1\nsource: forged@0000\nagent: a1\n---\n\n# Plan\n";
        let out = String::from_utf8(with_provenance(doc, "f--a1@abcd", 2, 1_791_734_418_000)).unwrap();
        assert_eq!(out, "---\nchange: x1\nagent: a1\nsource: f--a1@abcd\nversion: v2\ncopied: 2026-10-11T16:00:18Z\n---\n\n# Plan\n");
        let bare = String::from_utf8(with_provenance(b"just text", "f@1", 1, 0)).unwrap();
        assert!(bare.starts_with("---\nsource: f@1\nversion: v1\n"));
        assert_eq!(tag("a..b", 1, 1), None);
        assert_eq!(tag("x1", 2, 3).as_deref(), Some("refs/tags/context/x1/rev-2/v3"));
    }
}
