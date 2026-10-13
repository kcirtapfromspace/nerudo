//! The Trunk Durable Object: the only writer of one trunk.
//!
//! Agents push to their own forks and ask to land. The alarm handler runs the landing
//! loop: gate each queued change (seal boundaries, allowances), pick a batch of changes
//! whose footprints don't overlap, apply them onto the tip over trees, check trunk
//! invariants, then compare-and-swap the trunk ref. An intent record is written before
//! the push and cleared in the same atomic write that records the landing, so a crash
//! anywhere is reconciled by comparing trunk with the task.
//!
//! Authority over trunk comes from `nerudo.toml` on trunk: its approvers sign seals and
//! allowances, and its quorums say how many must agree. The admin token creates a trunk
//! and can pause it, but can't approve anything; its one override, break-glass, can only
//! land a change to `nerudo.toml` and is marked on the landed commit.

use crate::ci::{self, Verdict};
use crate::host;
use crate::transport::FetchTransport;
use crate::types::{AdminNote, Attempt, ChangeRec, Config, Fork, LandReq, Landing, Meta, Seed, State as Phase, Stats};
use futures::channel::oneshot;
use futures::future::{select, Either};
use nerudo_core::log::GENESIS;
use nerudo_core::merge::{blobs_needed, change_bytes, edits, footprint, Ejection, MAX_CHANGE_BYTES};
use nerudo_core::quorum::{met, seal_problem, seal_trailers, tally, Tally, BREAK_GLASS};
use nerudo_core::rules::Typecheck;
use nerudo_core::{apply_change, auth, check_files, seal_boundaries, trunk_checks, Allowance, AllowanceSpec, Approver, ChangeInput, LogEntry, Rules, Seal, SealSpec, RULES_PATH};
use nerudo_git::{fetch, fetch_body, fetch_pack, fetch_with, ingest, ls_refs, push, write_pack, Commit, FetchOpts, FileEntry, Oid, RefUpdate, Remote, Signature, Store, MODE_BLOB};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Duration;
use timings::{Lap, Round};
use worker::*;

const BATCHES_PER_ALARM: usize = 50;
/// Times a page on a lazy trunk reads what it lacks and runs again, at most.
const UI_READ_ROUNDS: usize = 32;
/// The landing store is a cache of trunk: it gains the trees and files of each landing. When
/// it holds more than twice what one read of head's trees gave, and more than STORE_MIN_BYTES,
/// the next run starts again from head's trees. So its memory follows the size of trunk's
/// trees, not the number of landings.
const STORE_GROWTH: usize = 2;
const STORE_MIN_BYTES: usize = 32 << 20;
const ALARM_BUDGET_MS: f64 = 60_000.0;
/// Artifacts fork() fails with INTERNAL_ERROR when many agents fork one trunk at once
/// (seen at 6 and 8 concurrent, not at 4), so the object runs at most this many.
const FORKS_IN_FLIGHT: usize = 3;
/// A change whose commit can't be fetched from its fork for a reason that can pass (a lost
/// connection, HTTP 5xx: nerudo_git::Error::transient) stays queued. The gate tries again
/// after 1 s, 2 s, 4 s ... at most 30 s, and ejects it as fetch_failed after FETCH_ATTEMPTS
/// tries (about 7 minutes).
const FETCH_RETRY_MS: f64 = 1_000.0;
const FETCH_RETRY_MAX_MS: f64 = 30_000.0;
const FETCH_ATTEMPTS: u64 = 20;
/// Runs in a row that may end with a type check that gave no answer before such a timeout
/// counts as the tree failing the check (see `checked`). With the stall's backoff (1, 2, 4 s)
/// that is within a minute of the first one.
const CI_TIMEOUT_TRIES: u32 = 3;
/// Agents (forks) one trunk takes. Every page lists them, and the Branches view reads each
/// fork's refs; the shared agent token makes any name's first fork, so the number is bounded.
const MAX_AGENTS: usize = 500;
/// A seal must have this long left when the gate counts it: a batch takes time after the
/// gate (checks, trials), and the landed commit's time must still be before the seal's end,
/// or `landseq verify` refuses the landing the gate accepted.
const SEAL_SLACK_MS: i64 = 180_000;

mod agents;
mod app;
mod beaten;
mod branches;
mod compare;
mod context;
mod culprit;
mod decide;
mod git;
mod import;
mod metrics;
mod objects;
mod prefetch;
mod prinfo;
mod releases;
mod repo;
mod repush;
mod revisions;
mod search;
mod search_index;
mod tasks;
mod social;
mod stall;
mod timings;
mod ui;
mod views;

/// A user's one sign-in key, for the Hub (crate::hub).
pub(crate) use ui::{account_key, same as same_key};

/// Shared discovery validation/ranking, without exporting the Workers API module.
pub(crate) fn validate_search_query(query: &BTreeMap<String, String>) -> std::result::Result<(), String> {
    search::SearchOptions::parse(query).map(|_| ())
}

pub(crate) fn rank_search_name(name: &str, query: &str, fuzzy: bool, case: bool) -> Option<(i64, Vec<usize>)> {
    search::rank_name(name, query, fuzzy, case)
}

#[derive(Default)]
struct Mem {
    /// An exclusive reader currently holds the UI object store.
    ui_out: bool,
    ui_waiters: Vec<oneshot::Sender<()>>,
    /// The landing loop's view of trunk: one commit of trunk and its trees (no history), the
    /// commits it lands, and only the file contents that gates and landings read.
    store: Store,
    /// The bytes `store` held after its last read of head's trees (see STORE_GROWTH).
    store_floor: usize,
    /// Objects read one at a time since the last stats write: through the binding, and from
    /// this object's storage (trunk/objects.rs).
    object_reads: (u64, u64),
    /// Until when the gate reads changes through the binding, not by git (trunk/prefetch.rs).
    git_down_until: f64,
    /// The web UI's own copy of trunk, with history and contents, fetched when a page needs it.
    ui: Store,
    /// The trunk commit `ui` was last fetched at.
    ui_head: Option<Oid>,
    /// Composition is immutable for one commit and can be reused across viewers.
    digest: Option<(Oid, serde_json::Value)>,
    waiters: Vec<oneshot::Sender<()>>,
    busy: bool,
    /// fork() calls in flight, and requests waiting for a slot.
    forking: usize,
    fork_queue: VecDeque<oneshot::Sender<()>>,
    /// Agents whose first fork is being made: a second first ask for one gets 409.
    first_forks: BTreeSet<String>,
    /// This trunk's `{org}/{repo}` on the Hub, once found (see repo.rs).
    hub_name: Option<String>,
    /// The trunk read token plain git reads with (trunk/git.rs), and until when to use it.
    git_read: Option<(String, f64)>,
    /// Users whose sign-in key works here, or not, and until when to trust that (repo.rs).
    accounts: BTreeMap<String, (bool, f64)>,
    /// The rounds of the alarm that runs now (trunk/timings.rs), and the time its last
    /// landing record spent on events.
    rounds: Vec<timings::Round>,
    events_ms: f64,
    /// Change id -> revision, for the batch the landing loop applies and pushes now
    /// (trunk/revisions.rs). A new revision of these PRs waits.
    merging: BTreeMap<String, u32>,
}

/// One of the FORKS_IN_FLIGHT slots. Freed on drop, so a request canceled mid-fork can't
/// leak it; wakes the next waiter that is still there.
struct ForkSlot<'a>(&'a RefCell<Mem>);

impl Drop for ForkSlot<'_> {
    fn drop(&mut self) {
        let mut m = self.0.borrow_mut();
        m.forking -= 1;
        while let Some(tx) = m.fork_queue.pop_front() {
            if tx.send(()).is_ok() {
                break;
            }
        }
    }
}

/// The first fork of one agent, in progress. Only one caller gets an agent's first fork, and
/// with it the agent's key. Freed on drop.
struct FirstFork<'a>(&'a RefCell<Mem>, String);

impl Drop for FirstFork<'_> {
    fn drop(&mut self) {
        self.0.borrow_mut().first_forks.remove(&self.1);
    }
}

#[durable_object]
pub struct Trunk {
    state: State,
    env: Env,
    mem: RefCell<Mem>,
}

fn g(e: nerudo_git::Error) -> Error {
    Error::RustError(e.to_string())
}

fn now() -> f64 {
    Date::now().as_millis() as f64
}

fn bad(status: u16, msg: impl Into<String>) -> Result<Response> {
    Ok(Response::from_json(&serde_json::json!({ "error": msg.into() }))?.with_status(status))
}

/// `bad`, for a refusal before the handler reads the request body: read the body first (at
/// most 1 MiB of it). A reply sent before its body is read breaks the connection the body came
/// on, and the next request on that connection fails ("Network connection lost" in `wrangler
/// dev`, cf/scale/ops.md).
async fn refuse(req: &mut Request, status: u16, msg: impl Into<String>) -> Result<Response> {
    use futures::StreamExt;
    if let Ok(mut body) = req.stream() {
        let mut read = 0;
        while let Some(Ok(chunk)) = body.next().await {
            read += chunk.len();
            if read > 1 << 20 {
                break;
            }
        }
    }
    bad(status, msg)
}

#[derive(Deserialize)]
struct InitReq {
    #[serde(flatten)]
    cfg: Config,
    /// Create the trunk repo and push this as its first commit (Artifacts only).
    #[serde(default)]
    seed: Option<Seed>,
    /// Or start it from an existing repo's commit (Artifacts only); see trunk/import.rs.
    #[serde(default)]
    import: Option<import::ImportSpec>,
    /// No longer accepted here: approvers and the allowance requirement live in nerudo.toml.
    #[serde(default)]
    approvers: Option<serde_json::Value>,
    #[serde(default)]
    require_allowance: Option<serde_json::Value>,
    /// Fault hook: stop after the atomic landing record, when no intent remains.
    #[serde(default)]
    debug_crash_after_record: u32,
}

#[derive(Deserialize)]
struct ReasonReq {
    #[serde(default)]
    reason: String,
}

#[derive(Deserialize)]
struct BreakGlassReq {
    change_id: String,
    rev: u32,
    reason: String,
}

/// A queued change that passed the gate.
struct Ready {
    rec: ChangeRec,
    units: BTreeSet<String>,
    allowance: Option<String>,
    /// Seal or break-glass trailers for the landed commit.
    trailers: Vec<String>,
    /// It edits nerudo.toml, so it must land last in its batch.
    edits_rules: bool,
}

#[derive(Deserialize)]
struct ForkReq {
    agent: String,
}

#[derive(Deserialize)]
struct Signed<T> {
    spec: T,
    signature: String,
}

impl DurableObject for Trunk {
    fn new(state: State, env: Env) -> Self {
        Self { state, env, mem: RefCell::new(Mem::default()) }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        let path = req.path();
        let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
        if parts.first() == Some(&"t") {
            let api = parts.get(2) == Some(&"api");
            let rest = if api { parts.get(3..) } else { parts.get(2..) }.unwrap_or_default();
            // On a lazy trunk a page reads the objects it lacks, then runs again
            // (trunk/objects.rs). Only reads run again; a write runs once.
            let lazy = req.method() == Method::Get && !(api && rest == ["search"]) && self.get::<bool>("lazy_trees").await?.unwrap_or(false);
            let mut round = 0;
            loop {
                let resp = if api { self.app(&mut req, rest).await } else { self.ui(&mut req, rest).await };
                let misses = self.mem.borrow().ui.take_misses();
                round += 1;
                if !lazy || misses.is_empty() || round >= UI_READ_ROUNDS {
                    return resp;
                }
                self.read_ui_misses(&misses).await?;
            }
        }
        let rest: Vec<&str> = parts.get(3..).map(|p| p.to_vec()).unwrap_or_default();
        // Plain git authenticates with HTTP Basic, not a bearer token (trunk/git.rs).
        if rest.first() == Some(&"git") {
            return self.git(&mut req, &rest[1..]).await;
        }
        let admin = self.authorized(&req, "NERUDO_ADMIN_TOKEN");
        let agent = admin || self.authorized(&req, "NERUDO_AGENT_TOKEN");
        // With an agent key, the request acts for that agent only (trunk/agents.rs).
        let caller = match agent {
            true => match self.caller(&req, admin).await? {
                Ok(c) => c,
                Err(e) => return refuse(&mut req, 403, e).await,
            },
            false => agents::Caller { admin, agent: None },
        };
        match (req.method(), rest.as_slice()) {
            (Method::Post, ["init"]) if admin => {
                let name = crate::request_header(&req, "x-nerudo-trunk")?;
                self.init(req.json().await?, name).await
            }
            (Method::Post, ["search-index"]) if admin => self.api_search_index().await,
            (Method::Get, ["search-index"]) if admin => self.api_search_index_status().await,
            (Method::Post, ["pause"]) if admin => self.pause(req.json().await?).await,
            (Method::Post, ["resume"]) if admin => self.resume().await,
            (Method::Post, ["break-glass"]) if admin => self.break_glass(req.json().await?).await,
            (Method::Post, ["visibility"]) if admin => self.set_visibility(req.json().await?).await,
            (Method::Post, ["share"]) if admin => self.share(&req).await,
            (Method::Post, ["aliases"]) if admin => self.set_alias(req.json().await?).await,
            (Method::Post, ["invite"]) if admin => {
                let url = req.url()?;
                self.invite(url, req.json().await?).await
            }
            (Method::Post, ["comments"]) if agent => self.api_comment(&caller, req.json().await?).await,
            (Method::Get, ["comments"]) if agent => self.api_comments(&req).await,
            (Method::Post, ["reviews"]) if agent => self.api_review(&caller, req.json().await?).await,
            (Method::Post, ["reactions"]) if agent => self.api_react(&caller, req.json().await?).await,
            (Method::Get, ["notes"]) if agent => self.api_notes(&caller, &req).await,
            (Method::Post, ["cleanup"]) if agent => self.api_cleanup(&caller, req.json().await?).await,
            (Method::Get, ["tasks"]) if agent => self.api_tasks(&req).await,
            (Method::Post, ["tasks"]) if agent => self.api_create_task(&caller, req.json().await?).await,
            (Method::Post, ["tasks", id, "claim"]) if agent => self.api_claim(&caller, id, req.json().await?).await,
            (Method::Get, ["trunk"]) if agent => self.trunk_info().await,
            (Method::Post, ["forks"]) if agent => self.api_fork(&caller, req.json().await?).await,
            (Method::Post, ["land"]) if agent => self.land(&caller, req.json().await?).await,
            (Method::Get, ["changes", cid]) if agent => self.change(cid, &req).await,
            // The admin, or an agent with its key for its own PR. The agent token alone names
            // no agent, and one agent must not close another's PR.
            (Method::Post, ["changes", cid, "close"]) if admin || caller.agent.is_some() => {
                self.close_change(&caller, cid, req.json().await.unwrap_or(serde_json::Value::Null)).await
            }
            (Method::Post, ["changes", _, "close"]) if agent => {
                refuse(&mut req, 403, "closing a PR needs the admin token, or the agent key of the agent that sent it").await
            }
            (Method::Get, ["agents", name, "key"]) if admin => self.api_agent_key(name, false).await,
            (Method::Post, ["agents", name, "key"]) if admin => self.api_agent_key(name, true).await,
            (Method::Get, ["context", cid]) if agent => self.api_context(cid, &req).await,
            (Method::Get, ["context-repo"]) if agent => self.api_context_repo().await,
            (Method::Post, ["allowances"]) if agent => self.add_allowance(req.json().await?).await,
            (Method::Post, ["seals"]) if agent => self.add_seal(req.json().await?).await,
            (Method::Get, ["releases"]) if agent => self.api_release_list().await,
            (Method::Get, ["releases", "draft"]) if agent => {
                self.warm_ui(|| self.api_release_draft(&req)).await?;
                self.api_release_draft(&req).await
            }
            (Method::Get, ["releases", tag]) if agent => self.api_release(tag).await,
            (Method::Post, ["releases"]) if agent => self.add_release(req.json().await?).await,
            (Method::Post, ["releases", "propose"]) if agent => self.propose_release(&caller, req.json().await?).await,
            (Method::Get, ["stats"]) if agent => self.stats().await,
            (Method::Get, ["timings"]) if agent => self.api_timings().await,
            (Method::Get, ["summary"]) if agent => self.summary().await,
            (Method::Get, ["log"]) if agent => self.log(&req).await,
            (Method::Get, ["queue"]) if agent => self.queue().await,
            (_, _) if !agent => refuse(&mut req, 401, "missing or wrong bearer token").await,
            _ => {
                let msg = format!("no route {} {path}", req.method());
                refuse(&mut req, 404, msg).await
            }
        }
    }

    async fn alarm(&self) -> Result<Response> {
        if std::mem::replace(&mut self.mem.borrow_mut().busy, true) {
            return Response::ok("busy");
        }
        // Idle indexing alarms must not hydrate the landing store. Recovery or
        // queued landings always run before one bounded background index chunk.
        let res = async {
            let queued = self.get::<Vec<String>>("q").await?.is_some_and(|q| !q.is_empty());
            let recovering = self.get::<Attempt>("attempt").await?.is_some();
            if queued || recovering { self.schedule().await } else { Ok(()) }
        }.await;
        if res.is_ok() {
            let indexing = async {
                let still_queued = self.get::<Vec<String>>("q").await?.is_some_and(|q| !q.is_empty());
                let paused = self.get::<AdminNote>("paused").await?.is_some();
                if !still_queued || paused { self.search_index_step().await?; }
                Ok::<(), Error>(())
            }.await;
            if let Err(e) = indexing {
                console_warn!("search index chunk will resume: {e}");
                let _ = self.ensure_alarm().await;
            }
        }
        self.mem.borrow_mut().busy = false;
        self.notify();
        // A failed run: the changes stay queued, and the next run starts after an interval
        // that doubles (trunk/stall.rs). The handler returns Ok, so that interval is the only
        // retry: the runtime does not also try a failed alarm again.
        match res {
            Ok(()) => {
                self.unstalled().await?;
                Response::ok("ok")
            }
            Err(e) => {
                self.metric_alarm_error(&e);
                let s = self.stalled(&e).await?;
                console_error!("landing loop: {e} ({}, {} failed, next in {} ms)", s.kind, s.attempts, s.next_ms - now() as i64);
                Response::ok("stalled")
            }
        }
    }
}

impl Trunk {
    // ---------------------------------------------------------------- storage helpers

    async fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let raw: Option<String> = self.state.storage().get(key).await?;
        match raw.as_deref() {
            None | Some("null") => Ok(None),
            Some(s) => Ok(Some(serde_json::from_str(s)?)),
        }
    }

    /// Write several keys in one atomic storage operation.
    async fn put_all(&self, items: Vec<(String, String)>) -> Result<()> {
        let obj = js_sys::Object::new();
        for (k, v) in items {
            js_sys::Reflect::set(&obj, &k.into(), &v.into())?;
        }
        self.state.storage().put_multiple_raw(obj).await
    }

    async fn put<T: Serialize>(&self, key: &str, v: &T) -> Result<()> {
        self.put_all(vec![(key.to_string(), serde_json::to_string(v)?)]).await
    }

    async fn cfg(&self) -> Result<Config> {
        self.get("cfg").await?.ok_or_else(|| Error::RustError("trunk not initialized: POST /init".into()))
    }

    async fn rules(&self) -> Result<Rules> {
        self.get("rules").await?.ok_or_else(|| Error::RustError("no rules".into()))
    }

    fn authorized(&self, req: &Request, secret: &str) -> bool {
        crate::bearer_ok(&self.env, req, secret)
    }

    fn notify(&self) {
        for w in self.mem.borrow_mut().waiters.drain(..) {
            let _ = w.send(());
        }
    }

    async fn ensure_alarm(&self) -> Result<()> {
        if self.state.storage().get_alarm().await?.is_none() {
            self.state.storage().set_alarm(Duration::from_millis(25)).await?;
        }
        Ok(())
    }

    async fn fork_slot(&self) -> ForkSlot<'_> {
        loop {
            if self.mem.borrow().forking < FORKS_IN_FLIGHT {
                self.mem.borrow_mut().forking += 1;
                return ForkSlot(&self.mem);
            }
            let (tx, rx) = oneshot::channel();
            self.mem.borrow_mut().fork_queue.push_back(tx);
            let _ = rx.await;
        }
    }

    fn take_store(&self) -> Store {
        std::mem::take(&mut self.mem.borrow_mut().store)
    }

    fn put_store(&self, store: Store) {
        // The UI does not share this store: it lacks history and most file contents.
        self.mem.borrow_mut().store = store;
    }

    // ---------------------------------------------------------------- routes

    /// `name`: the name this object was reached by (the router sets X-Nerudo-Trunk), which
    /// the trunk id must be: keys, seals and links are scoped by the id, so two objects with
    /// one id would share them.
    async fn init(&self, r: InitReq, name: Option<String>) -> Result<Response> {
        let InitReq { mut cfg, seed, import, approvers, require_allowance, debug_crash_after_record } = r;
        if let Some(n) = name.filter(|n| *n != cfg.trunk) {
            return bad(400, format!("trunk {:?} is not the name of this object ({n:?}); POST /v1/trunks/{}/init with trunk {:?}", cfg.trunk, cfg.trunk, cfg.trunk));
        }
        if seed.is_some() && import.is_some() {
            return bad(400, "send a seed or an import, not both");
        }
        if approvers.is_some() || require_allowance.is_some() {
            return bad(400, "approvers and require_allowance live in nerudo.toml on trunk ([[approvers]], [allowances] required)");
        }
        if self.mem.borrow().busy {
            return bad(409, "landing loop is running");
        }
        if !host::valid_trunk_repo(&cfg.host) {
            return bad(400, "trunk repo name must be [A-Za-z0-9_-]+ without \"--\" (fork names are <trunk>--<agent>)");
        }
        // The id goes into every link and key of the trunk: the same shape the router takes.
        if !crate::hub::valid_trunk(&cfg.trunk) {
            return bad(400, "trunk must be 1 to 100 of [A-Za-z0-9._-]");
        }
        // Once a trunk exists, init can only change how it runs, never what it is or who
        // approves for it; that lives in nerudo.toml on trunk.
        if let Some(old) = self.get::<Config>("cfg").await? {
            if seed.is_some() || import.is_some() {
                return bad(409, "trunk already initialized; a seed or an import only creates new trunks");
            }
            let same_host = serde_json::to_value(&old.host)? == serde_json::to_value(&cfg.host)?;
            if old.trunk != cfg.trunk || old.branch != cfg.branch || !same_host || !(cfg.remote.is_empty() || cfg.remote == old.remote) {
                return bad(409, "trunk already initialized: re-init can change ci_ms and batch_cap and re-mint the write token, not the trunk, host or branch");
            }
            cfg.remote = old.remote;
        }
        let mut creation_token = None;
        if let Some(seed) = &seed {
            if let Some(p) = seed.files.keys().find(|p| !valid_path(p)) {
                return bad(400, format!("bad seed path {p:?}"));
            }
            // Check the rules before making a repo, so a bad seed doesn't leave one behind.
            match nerudo_core::rules::rules_entry(&seed.files).map_err(Error::RustError)?.map(|(_, t)| Rules::parse(t)) {
                None => return bad(400, format!("the seed needs a {RULES_PATH}")),
                Some(Err(e)) => return bad(400, e),
                Some(Ok(_)) => {}
            }
            let branch = cfg.branch.strip_prefix("refs/heads/").unwrap_or(&cfg.branch).to_string();
            let (remote, token) = match host::create_trunk(&self.env, &cfg.host, &branch).await {
                Ok(x) => x,
                Err(e) => return bad(409, format!("create trunk repo: {e}")),
            };
            cfg.remote = remote;
            let mut store = self.take_store();
            let pushed = push_seed(&mut store, &cfg, seed, &token).await;
            self.put_store(store);
            if let Err(e) = pushed {
                return bad(502, format!("seed push: {e}"));
            }
            creation_token = Some(token);
        }
        if let Some(imp) = &import {
            match self.import_trunk(&mut cfg, imp).await? {
                Ok(token) => creation_token = Some(token),
                Err((status, msg)) => return bad(status, msg),
            }
        }
        if cfg.remote.is_empty() {
            return bad(400, "remote is required, or send a seed to create the trunk");
        }
        if cfg.authorization.is_empty() {
            match host::trunk_write_authorization(&self.env, &cfg.host).await? {
                Some(a) => cfg.authorization = a,
                None => return bad(400, "authorization is required for this host"),
            }
        }
        if let Some(token) = creation_token {
            if let Err(e) = host::revoke(&self.env, &cfg.host, &token).await {
                console_warn!("revoking the trunk creation token failed: {e:?}");
            }
        }
        let t = FetchTransport::default();
        let remote = Remote::new(&cfg.remote, Some(cfg.authorization.clone()));
        let refs = ls_refs(&t, &remote, &cfg.branch).await.map_err(g)?;
        let Some(head) = refs.iter().find(|(n, _)| n == &cfg.branch).map(|(_, o)| *o) else {
            return bad(400, format!("{} not found on trunk", cfg.branch));
        };
        let mut store = self.take_store();
        // On Artifacts, head's commit and the root tree alone (trunk/objects.rs); elsewhere head
        // and all its trees. Then nerudo.toml.
        let mut rd = self.readers(&cfg).await?;
        let rules = match self.load_commit(&mut rd, &t, &mut store, head, None).await {
            Ok(()) => match self.fill(&mut rd, None, &t, &mut store, |s| s.commit(&head).and_then(|c| nerudo_core::rules::rules_file(s, &c.tree))).await {
                Ok(_) => load_rules(&t, &remote, &mut store, &head).await,
                Err(e) => Err(e),
            },
            Err(e) => Err(e),
        };
        let objects = store.len();
        self.mem.borrow_mut().store_floor = store.bytes();
        self.put_store(store);
        let rules = rules?;
        let mut meta: Meta = self.get("meta").await?.unwrap_or_default();
        if meta.prev_hash.is_empty() {
            meta.prev_hash = GENESIS.into();
        }
        meta.head = Some(head);
        self.put_all(vec![
            ("cfg".into(), serde_json::to_string(&cfg)?),
            ("rules".into(), serde_json::to_string(&rules)?),
            ("rules_head".into(), serde_json::to_string(&head)?),
            ("debug_crash_after_record".into(), debug_crash_after_record.to_string()),
            ("meta".into(), serde_json::to_string(&meta)?),
            ("search_dirty".into(), serde_json::to_string(&search_index::Dirty { head, seq: meta.seq })?),
        ])
        .await?;
        self.notify_search_dirty(head, meta.seq);
        self.ensure_alarm().await?;
        let approvers: Vec<&str> = rules.approvers.iter().map(|a| a.name.as_str()).collect();
        Response::from_json(&serde_json::json!({ "head": head, "remote": cfg.remote, "rules_digest": rules.digest(), "approvers": approvers, "objects": objects }))
    }

    async fn pause(&self, r: ReasonReq) -> Result<Response> {
        self.cfg().await?;
        let note = AdminNote { reason: one_line(&r.reason), at_ms: now() as i64 };
        self.put("paused", &note).await?;
        Response::from_json(&serde_json::json!({ "paused": note }))
    }

    async fn resume(&self) -> Result<Response> {
        self.put_all(vec![("paused".into(), "null".into())]).await?;
        self.ensure_alarm().await?;
        Response::from_json(&serde_json::json!({ "paused": null }))
    }

    /// The admin's one override, for when the approvers can't reach a quorum any more. It
    /// lands a parked change only if that change edits nothing but nerudo.toml, and the
    /// landed commit says so in a `Break-Glass:` trailer.
    async fn break_glass(&self, r: BreakGlassReq) -> Result<Response> {
        let key = format!("{}:{}", r.change_id, r.rev);
        let Some(rec) = self.get::<ChangeRec>(&format!("c:{key}")).await? else {
            return bad(404, "no such change");
        };
        if rec.state != Phase::AwaitingSeal {
            return bad(409, "break-glass only applies to a change waiting for seals");
        }
        let reason = one_line(&r.reason);
        if reason.is_empty() {
            return bad(400, "say why: the reason goes on trunk");
        }
        self.put(&format!("bg:{key}"), &AdminNote { reason, at_ms: now() as i64 }).await?;
        let woken = self.requeue(Phase::AwaitingSeal, |k| k == key).await?;
        self.ensure_alarm().await?;
        Response::from_json(&serde_json::json!({ "break_glass": key, "requeued": woken }))
    }

    /// Where trunk is, with a token that can only read it: enough to check every landing
    /// from outside (`nerudo verify`).
    async fn trunk_info(&self) -> Result<Response> {
        let Some(cfg) = self.get::<Config>("cfg").await? else {
            return bad(404, "no such trunk");
        };
        let authorization = host::trunk_read_authorization(&self.env, &cfg.host).await?;
        let meta: Meta = self.get("meta").await?.unwrap_or_default();
        Response::from_json(&serde_json::json!({
            "trunk": cfg.trunk, "remote": cfg.remote, "branch": cfg.branch, "authorization": authorization, "head": meta.head,
        }))
    }

    /// `POST .../forks {agent, agent_key?}`. A new agent gets its fork and its key. An agent
    /// that has a fork gets it again only with its key (header or body), or for the admin.
    /// The agent token can't take a person's name or a person's fork.
    async fn api_fork(&self, caller: &agents::Caller, body: serde_json::Value) -> Result<Response> {
        let agent = body["agent"].as_str().unwrap_or_default().to_string();
        if !host::valid_agent(&agent) {
            return bad(400, "agent must be [A-Za-z0-9_]+");
        }
        let mut me = caller.agent.clone();
        if let (None, Some(k)) = (&me, body["agent_key"].as_str().filter(|k| !k.is_empty())) {
            match self.key_agent(k).await? {
                Ok(a) => me = Some(a),
                Err(e) => return bad(403, e),
            }
        }
        if let Some(me) = me.as_deref().filter(|me| *me != agent) {
            return bad(403, format!("your agent key is for {me}, not {agent}. An agent gets only its own fork."));
        }
        let known = self.get::<Fork>(&format!("fa:{agent}")).await?.is_some();
        if !caller.admin && me.is_none() {
            if self.is_person(&agent).await? {
                return bad(403, format!("{agent} is a person on this trunk, or the name of a person's fork. Use another agent name."));
            }
            if known {
                let trunk = self.cfg().await?.trunk;
                return bad(403, format!(
                    "agent {agent} has a fork on trunk {trunk} already. Only its agent key ({}), or the admin token, gets that fork again. \
                     nerudo mcp keeps the key in ~/.nerudo/agents/{trunk}/{agent}. No key? {}",
                    agents::KEY_HEADER,
                    agents::remedy(&trunk, &agent)
                ));
            }
        }
        if !known && self.list_prefix("fa:").await?.len() >= MAX_AGENTS {
            return bad(403, format!("this trunk has {MAX_AGENTS} agents already; no new agent can fork it"));
        }
        // Two first asks for one name: only one makes the fork and gets the key.
        let _first = match known {
            true => None,
            false => match self.mem.borrow_mut().first_forks.insert(agent.clone()) {
                true => Some(FirstFork(&self.mem, agent.clone())),
                false => return bad(409, format!("the fork of {agent} is being made now; ask again in a few seconds")),
            },
        };
        self.fork(ForkReq { agent }).await
    }

    /// Make the agent's fork, or give its fork a new token. The callers check who may.
    async fn fork(&self, r: ForkReq) -> Result<Response> {
        let cfg = self.cfg().await?;
        if !host::valid_agent(&r.agent) {
            return bad(400, "agent must be [A-Za-z0-9_]+");
        }
        let known = self.get::<Fork>(&format!("fa:{}", r.agent)).await?;
        let slot = self.fork_slot().await;
        let res = host::fork(&self.env, &cfg.host, &r.agent, known).await;
        drop(slot);
        let f = match res {
            Ok(f) => f,
            Err(e) => return bad(502, format!("fork failed: {e}")),
        };
        let trunk_authorization = host::trunk_read_authorization(&self.env, &cfg.host).await?;
        let rec = serde_json::to_string(&f)?;
        self.put_all(vec![(format!("f:{}", f.name), rec.clone()), (format!("fa:{}", r.agent), rec)]).await?;
        let meta: Meta = self.get("meta").await?.unwrap_or_default();
        let agent_key = ui::agent_key(&self.env, &cfg.trunk, &r.agent, self.key_gen(&r.agent).await?)?;
        Response::from_json(&serde_json::json!({
            "name": f.name, "remote": f.remote, "authorization": f.authorization,
            "trunk": cfg.remote, "trunk_authorization": trunk_authorization, "base": meta.head, "agent_key": agent_key,
        }))
    }

    async fn land(&self, caller: &agents::Caller, req: LandReq) -> Result<Response> {
        if !valid_change_id(&req.change_id) {
            return bad(400, "change_id must be 1 to 64 of [A-Za-z0-9._-]");
        }
        // A PR belongs to the fork that sent revision 1. Only that fork sends its next revisions.
        if let Some(rev) = self.get::<u32>(&format!("latest:{}", req.change_id)).await? {
            if let Some(first) = self.get::<ChangeRec>(&format!("c:{}:{rev}", req.change_id)).await?.filter(|r| r.req.fork != req.fork) {
                return bad(403, format!("{} is a PR from fork {}. Only that fork can send its next revision. Send your work as another change_id.", prinfo::pr_label(&first), first.req.fork));
            }
        }
        // With its key, an agent sends PRs from its own fork only. The agent token alone sends
        // no PR: each fork is an agent's or a person's, and only their key acts for them.
        if let Some(me) = caller.agent.as_deref() {
            if self.caller_fork(caller).await?.as_deref() != Some(req.fork.as_str()) {
                return bad(403, format!("your agent key is for {me}, and {} is not its fork", req.fork));
            }
        } else if !caller.admin {
            if let Some(owner) = self.fork_owner(&req.fork).await? {
                if self.is_person(&owner).await? {
                    return bad(403, format!("{} is the fork of a person on this trunk. The agent token sends PRs only from the forks of agents.", req.fork));
                }
                return bad(403, format!("{} is the fork of {owner}. {}", req.fork, self.needs_key(&owner).await?));
            }
            if self.get::<Fork>(&format!("f:{}", req.fork)).await?.is_some() {
                return bad(403, format!("a PR from {} needs the agent key of its agent in {}", req.fork, agents::KEY_HEADER));
            }
        }
        let key = req.key();
        if let Some(rec) = self.get::<ChangeRec>(&format!("c:{key}")).await? {
            // A revision is one commit: the same revision with another commit is not a retry.
            if rec.req.commit != req.commit {
                return revisions::refused("taken", format!("revision {} of {} is commit {}. Send another commit as the next revision.", req.rev, prinfo::pr_label(&rec), rec.req.commit.to_hex()));
            }
            // Idempotent on (change_id, rev); context sent again for it is its next version.
            if let Some(fork) = self.get::<Fork>(&format!("f:{}", req.fork)).await?.filter(|f| f.name == rec.req.fork) {
                if let Err(e) = self.keep_context(&fork, &req).await? {
                    return bad(400, e);
                }
            }
            return Response::from_json(&rec);
        }
        let Some(fork) = self.get::<Fork>(&format!("f:{}", req.fork)).await? else {
            return bad(400, format!("unknown fork {}; POST /forks first", req.fork));
        };
        // A merged change id takes no new revision, revisions only go forward, and a PR in the
        // batch being pushed waits (trunk/revisions.rs). Checked before the context is kept,
        // and again after its fetches.
        if let Some((kind, words)) = self.revision_refusal(&req).await? {
            return revisions::refused(kind, words);
        }
        if let Err(e) = self.keep_context(&fork, &req).await? {
            return bad(400, e);
        }
        if let Some(id) = &req.task {
            if self.task(id).await?.is_none() {
                return bad(400, format!("unknown task {id}"));
            }
        }
        // From here to the write, storage only: no other request runs in between.
        if let Some(rec) = self.get::<ChangeRec>(&format!("c:{key}")).await? {
            // The same revision, sent again while this request kept its context.
            if rec.req.commit == req.commit {
                return Response::from_json(&rec);
            }
            return revisions::refused("taken", format!("revision {} of {} is commit {}. Send another commit as the next revision.", req.rev, prinfo::pr_label(&rec), rec.req.commit.to_hex()));
        }
        if let Some((kind, words)) = self.revision_refusal(&req).await? {
            return revisions::refused(kind, words);
        }
        let rec = ChangeRec {
            req: req.clone(),
            state: Phase::Queued,
            seq: None,
            trunk_commit: None,
            reason: None,
            seal_reasons: vec![],
            allowance: None,
            sealed_by: vec![],
            submitted_ms: now() as i64,
            queued_ms: None,
            landed_ms: None,
            batch: None,
            allocated: vec![],
            footprint: vec![],
            ci_runs: 0,
            subject: String::new(),
            decisions: vec![],
            number: None,
            size: None,
        };
        let (number, numbering) = self.pr_number(&req.change_id).await?;
        let rec = ChangeRec { number: Some(number), ..rec };
        let mut queue: Vec<String> = self.get("q").await?.unwrap_or_default();
        queue.push(key.clone());
        let mut stats: Stats = self.get("stats").await?.unwrap_or_default();
        stats.submitted += 1;
        let mut writes = vec![
            (format!("c:{key}"), serde_json::to_string(&rec)?),
            (format!("latest:{}", req.change_id), req.rev.to_string()),
            ("q".into(), serde_json::to_string(&queue)?),
            ("stats".into(), serde_json::to_string(&stats)?),
        ];
        writes.extend(numbering);
        self.put_all(writes).await?;
        self.opened(&req, number).await?;
        if let Some(id) = &rec.req.task {
            self.task_sent(id, &rec.req.change_id).await?;
        }
        self.ensure_alarm().await?;
        Response::from_json(&rec)
    }

    async fn change(&self, cid: &str, req: &Request) -> Result<Response> {
        let url = req.url()?;
        let q: BTreeMap<String, String> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        let rev: u32 = match q.get("rev").and_then(|r| r.parse().ok()) {
            Some(r) => r,
            None => match self.get::<u32>(&format!("latest:{cid}")).await? {
                Some(r) => r,
                None => return bad(404, format!("unknown change {cid}")),
            },
        };
        let wait_ms = q.get("wait").and_then(|w| w.parse::<f64>().ok()).unwrap_or(0.0).min(60.0) * 1000.0;
        let deadline = now() + wait_ms;
        loop {
            let Some(rec) = self.get::<ChangeRec>(&format!("c:{cid}:{rev}")).await? else {
                return bad(404, format!("unknown change {cid}:{rev}"));
            };
            let left = deadline - now();
            if rec.state != Phase::Queued || left <= 0.0 {
                return self.receipt(&rec).await;
            }
            let (tx, rx) = oneshot::channel();
            self.mem.borrow_mut().waiters.push(tx);
            let timer = Delay::from(Duration::from_millis(left as u64));
            if let Either::Right(_) = select(rx, timer).await {
                return match self.get::<ChangeRec>(&format!("c:{cid}:{rev}")).await? {
                    Some(rec) => self.receipt(&rec).await,
                    None => Response::from_json(&serde_json::Value::Null),
                };
            }
        }
    }

    async fn add_allowance(&self, s: Signed<AllowanceSpec>) -> Result<Response> {
        if !valid_change_id(&s.spec.id) {
            return bad(400, "an allowance id is 1 to 64 of [A-Za-z0-9._-]");
        }
        let cfg = self.cfg().await?;
        let rules = self.rules().await?;
        let who = match check_signed(&cfg, &rules, &s.spec.approver, &s.spec.trunk, &s.spec.rules_digest, &s.spec.signing_bytes(), &s.signature) {
            Ok(a) => a,
            Err(e) => return bad(403, e),
        };
        if !who.eligible(&rules.allowances.roles) {
            return bad(403, format!("{} has no role that may sign allowances ({})", who.name, rules.allowances.roles.join(", ")));
        }
        let key = format!("a:{}", s.spec.id);
        if self.get::<Allowance>(&key).await?.is_some() {
            return bad(409, "allowance id already used");
        }
        let a = Allowance { spec: s.spec, signature: s.signature, reserved: 0, spent: 0 };
        // Changes parked for want of an allowance get another look.
        let woken = self.requeue(Phase::AwaitingAllowance, |_| true).await?;
        self.put(&key, &a).await?;
        self.ensure_alarm().await?;
        Response::from_json(&serde_json::json!({ "allowance": a.spec.id, "requeued": woken }))
    }

    /// Store one approver's seal. Each approver has one slot per change, so sealing twice
    /// renews it but still counts once. The change goes back on the queue only once the
    /// seals meet every quorum it needs; until then its receipt shows the tally.
    async fn add_seal(&self, s: Signed<SealSpec>) -> Result<Response> {
        let cfg = self.cfg().await?;
        let rules = self.rules().await?;
        let who = match check_signed(&cfg, &rules, &s.spec.approver, &s.spec.trunk, &s.spec.rules_digest, &s.spec.signing_bytes(), &s.signature) {
            Ok(a) => a.name.clone(),
            Err(e) => return bad(403, e),
        };
        let key = format!("{}:{}", s.spec.change_id, s.spec.rev);
        let Some(mut rec) = self.get::<ChangeRec>(&format!("c:{key}")).await? else {
            return bad(404, "no such change");
        };
        if rec.req.commit.to_hex() != s.spec.commit {
            return bad(409, "seal covers a different commit than the one submitted");
        }
        if matches!(rec.state, Phase::Landed | Phase::Ejected) {
            return bad(409, format!("change is already {:?}", rec.state).to_lowercase());
        }
        if !rec.seal_reasons.is_empty() && rec.seal_reasons != s.spec.reasons {
            return bad(409, "seal names different reasons than the change has");
        }
        let reasons = s.spec.reasons.clone();
        let cid = s.spec.change_id.clone();
        self.put(&format!("s:{key}:{}", s.spec.approver), &Seal { spec: s.spec, signature: s.signature }).await?;
        self.change_event("approved", format!("{who} approved with a signature: {}", reasons.join("; ")), &cid, Some(&who)).await?;
        let tallies = self.tally_for(&rules, &rec, &reasons).await?.1;
        let complete = met(&tallies);
        let mut woken = 0;
        if complete {
            woken = self.requeue(Phase::AwaitingSeal, |k| k == key).await?;
            self.ensure_alarm().await?;
        } else if rec.state == Phase::AwaitingSeal {
            rec.reason = Some(serde_json::json!({ "kind": "needs_seal", "reasons": reasons, "quorum": tallies }));
            self.put(&format!("c:{key}"), &rec).await?;
        }
        Response::from_json(&serde_json::json!({ "sealed": key, "by": who, "quorum": tallies, "complete": complete, "requeued": woken }))
    }

    /// The seals that count for this change now, and its tally against the trunk's quorums.
    async fn tally_for(&self, rules: &Rules, rec: &ChangeRec, reasons: &[String]) -> Result<(Vec<Seal>, Vec<Tally>)> {
        let input = input_of(&rec.req);
        let mut seals = vec![];
        for (_, v) in self.list_prefix(&format!("s:{}:", rec.req.key())).await? {
            if let Ok(s) = serde_json::from_str::<Seal>(&v) {
                if seal_problem(rules, &s.spec, &input, reasons, now() as i64 + SEAL_SLACK_MS).is_none() {
                    seals.push(s);
                }
            }
        }
        let specs: Vec<SealSpec> = seals.iter().map(|s| s.spec.clone()).collect();
        let tallies = tally(rules, reasons, &specs);
        Ok((seals, tallies))
    }

    /// Move parked changes back onto the queue.
    async fn requeue(&self, from: Phase, pick: impl Fn(&str) -> bool) -> Result<usize> {
        let parked: Vec<String> = self.get(&parked_key(&from)).await?.unwrap_or_default();
        let (wake, keep): (Vec<String>, Vec<String>) = parked.into_iter().partition(|k| pick(k));
        if wake.is_empty() {
            return Ok(0);
        }
        let mut queue: Vec<String> = self.get("q").await?.unwrap_or_default();
        let mut items = vec![];
        for k in &wake {
            if let Some(mut rec) = self.get::<ChangeRec>(&format!("c:{k}")).await? {
                rec.state = Phase::Queued;
                rec.reason = None;
                rec.queued_ms = Some(now() as i64);
                items.push((format!("c:{k}"), serde_json::to_string(&rec)?));
                queue.push(k.clone());
            }
        }
        items.push(("q".into(), serde_json::to_string(&queue)?));
        items.push((parked_key(&from), serde_json::to_string(&keep)?));
        self.put_all(items).await?;
        Ok(wake.len())
    }

    async fn stats(&self) -> Result<Response> {
        let stats: Stats = self.get("stats").await?.unwrap_or_default();
        let meta: Meta = self.get("meta").await?.unwrap_or_default();
        let queue: Vec<String> = self.get("q").await?.unwrap_or_default();
        let mut v = serde_json::to_value(&stats)?;
        v["head"] = serde_json::to_value(meta.head)?;
        v["seq"] = meta.seq.into();
        v["log_head_hash"] = meta.prev_hash.into();
        v["queued"] = queue.len().into();
        v["objects_cached"] = self.mem.borrow().store.len().into();
        v["store_bytes"] = self.mem.borrow().store.bytes().into();
        v["ui_objects_cached"] = self.mem.borrow().ui.len().into();
        // Linear memory of this isolate's WebAssembly instance. It only grows, so it is the peak.
        #[cfg(target_arch = "wasm32")]
        {
            v["wasm_memory_bytes"] = (core::arch::wasm32::memory_size(0) as u64 * 65_536).into();
        }
        v["paused"] = serde_json::to_value(self.get::<AdminNote>("paused").await?)?;
        if let Some(r) = self.get::<Rules>("rules").await? {
            v["rules_digest"] = r.digest().into();
            v["approvers"] = r.approvers.iter().map(|a| a.name.clone()).collect::<Vec<_>>().into();
        }
        Response::from_json(&v)
    }

    /// Changes that haven't landed yet: queued, and parked for a human. Each carries its
    /// footprint once the sequencer has gated it, so agents can see what in-flight work
    /// touches before they start.
    async fn queue(&self) -> Result<Response> {
        let queue: Vec<String> = self.get("q").await?.unwrap_or_default();
        let mut keys: Vec<String> = queue;
        for k in self.parked_keys().await? {
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
        let mut out = vec![];
        for k in keys {
            if let Some(r) = self.get::<ChangeRec>(&format!("c:{k}")).await? {
                // In the batch being applied and pushed now: a new revision of it waits.
                let merging = self.mem.borrow().merging.get(&r.req.change_id) == Some(&r.req.rev);
                out.push(serde_json::json!({
                    "change_id": r.req.change_id, "rev": r.req.rev, "state": r.state, "fork": r.req.fork,
                    "footprint": r.footprint, "submitted_ms": r.submitted_ms, "seal_reasons": r.seal_reasons, "reason": r.reason,
                    "number": r.number, "size": r.size, "merging": merging,
                }));
            }
        }
        let meta: Meta = self.get("meta").await?.unwrap_or_default();
        // Why the queue waits, when the landing loop failed (trunk/stall.rs).
        let stall = self.stall().await?;
        Response::from_json(&serde_json::json!({ "head": meta.head, "changes": out, "stall": stall }))
    }

    async fn log(&self, req: &Request) -> Result<Response> {
        let since: u64 = req.url()?.query_pairs().find(|(k, _)| k == "since").and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
        let meta: Meta = self.get("meta").await?.unwrap_or_default();
        let mut out = vec![];
        for seq in since + 1..=meta.seq.min(since + 500) {
            if let Some(e) = self.get::<LogEntry>(&format!("log:{seq:012}")).await? {
                out.push(e);
            }
        }
        Response::from_json(&out)
    }

    // ---------------------------------------------------------------- the landing loop

    /// Read and parse the authority at an exact Git head. Missing objects can be loaded,
    /// but any unavailable or invalid rules stop the landing loop.
    async fn rules_at(&self, rd: &mut objects::Readers, t: &FetchTransport, store: &mut Store, head: Oid) -> Result<Rules> {
        self.fill(rd, None, t, store, |s| read_rules(s, &head)).await?
    }

    async fn note_authority(&self, active: &Rules) -> Result<()> {
        if let Some(old) = self.get::<Rules>("rules").await? {
            if old.digest() != active.digest() {
                self.note_renames(&old, active).await?;
            }
        }
        Ok(())
    }

    /// Reservations are provisional until publication. A rejected or deferred candidate
    /// can leave real capacity available: wake eligible waiters in the same durable record.
    async fn wake_allowances(
        &self, rules: &Rules, budget: &BTreeMap<String, Allowance>, done: &BTreeSet<String>,
        queue: &mut Vec<String>, items: &mut Vec<(String, String)>,
    ) -> Result<()> {
        let parked: Vec<String> = self.get(&parked_key(&Phase::AwaitingAllowance)).await?.unwrap_or_default();
        let mut available = budget.clone();
        // A grant can arrive during the gate's fetches, before that gate parks a waiter.
        // Keep this batch's updated charges, and include grants absent from its snapshot.
        if !parked.is_empty() && rules.allowances.required {
            for (key, value) in self.list_prefix("a:").await? {
                if !available.contains_key(&key) {
                    if let Ok(a) = serde_json::from_str::<Allowance>(&value) {
                        available.insert(key, a);
                    }
                }
            }
        }
        let digests = rules.digests();
        let mut keep = vec![];
        for key in parked {
            if done.contains(&key) { continue; }
            let Some(mut rec) = self.get::<ChangeRec>(&format!("c:{key}")).await? else { continue };
            let units = rec.footprint.iter().cloned().collect();
            if rec.state == Phase::AwaitingAllowance
                && (!rules.allowances.required || available.values().any(|a| a.covers(&units, now() as i64, &digests)))
            {
                rec.state = Phase::Queued;
                rec.reason = None;
                if !queue.contains(&key) { queue.push(key.clone()); }
                items.push((format!("c:{key}"), serde_json::to_string(&rec)?));
            } else {
                keep.push(key);
            }
        }
        items.push((parked_key(&Phase::AwaitingAllowance), serde_json::to_string(&keep)?));
        Ok(())
    }

    async fn schedule(&self) -> Result<()> {
        let cfg = self.cfg().await?;
        let t = FetchTransport::default();
        let trunk = Remote::new(&cfg.remote, Some(cfg.authorization.clone()));
        let mut store = self.take_store();
        let res = self.schedule_with(&cfg, &t, &trunk, &mut store).await;
        let floor = self.mem.borrow().store_floor;
        if store.bytes() > (STORE_GROWTH * floor).max(STORE_MIN_BYTES) {
            store = Store::new();
        }
        self.put_store(store);
        let mut stats: Stats = self.get("stats").await?.unwrap_or_default();
        stats.git_requests += t.requests.get();
        let (read, kept) = std::mem::take(&mut self.mem.borrow_mut().object_reads);
        stats.object_reads += read;
        stats.objects_from_storage += kept;
        self.flush_timings(vec![("stats".into(), serde_json::to_string(&stats)?)]).await?;
        res
    }

    async fn schedule_with(&self, cfg: &Config, t: &FetchTransport, trunk: &Remote, store: &mut Store) -> Result<()> {
        let started = now();
        let mut meta: Meta = self.get("meta").await?.unwrap_or_default();
        let mut head = meta.head.ok_or_else(|| Error::RustError("no trunk head".into()))?;
        // On Artifacts, objects come one at a time when needed (trunk/objects.rs).
        let mut rd = self.readers(cfg).await?;
        if !store.contains(&head) {
            self.load_commit(&mut rd, t, store, head, None).await?;
            self.mem.borrow_mut().store_floor = store.bytes();
        }
        if let Some(att) = self.get::<Attempt>("attempt").await? {
            head = self.reconcile(cfg, t, trunk, store, att, &mut meta).await?;
        }
        let mut head_ms = now() - started;
        for _ in 0..BATCHES_PER_ALARM {
            if now() - started > ALARM_BUDGET_MS {
                self.state.storage().set_alarm(Duration::from_millis(10)).await?;
                return Ok(());
            }
            // Repair old metadata/rules gaps and external moves before any gate. A failed
            // authority read never permits a candidate to use the previous head's rules.
            if self.get::<Oid>("rules_head").await? != Some(head) {
                let active = self.rules_at(&mut rd, t, store, head).await?;
                self.note_authority(&active).await?;
                self.put_all(vec![
                    ("rules".into(), serde_json::to_string(&active)?),
                    ("rules_head".into(), serde_json::to_string(&head)?),
                ]).await?;
            }
            // Stage timings of this round (trunk/timings.rs).
            let round_start = now();
            let mut lap = Lap::start();
            let (requests0, rx0) = (t.requests.get(), t.rx_bytes.get());
            let mut tm = Round { alarm_ms: started as i64, at_ms: round_start as i64, head_ms: std::mem::take(&mut head_ms), ..Round::default() };
            let queue: Vec<String> = self.get("q").await?.unwrap_or_default();
            // A newer revision closes the older ones, on a paused trunk too (trunk/revisions.rs).
            let queue = self.supersede(queue).await?;
            if queue.is_empty() || self.get::<AdminNote>("paused").await?.is_some() {
                return Ok(()); // resume sets a new alarm
            }
            let rules = self.rules().await?;
            let mut recs: Vec<ChangeRec> = vec![];
            for k in &queue {
                if let Some(r) = self.get::<ChangeRec>(&format!("c:{k}")).await? {
                    recs.push(r);
                }
            }
            tm.queued = queue.len();
            tm.oldest_ms = recs.iter().map(|r| now() - metrics::queued_since(r)).fold(0.0, f64::max);

            // Gate every queued change: objects present, seal boundaries and quorum, allowance.
            let mut ready: Vec<Ready> = vec![];
            let mut parked: Vec<ChangeRec> = vec![];
            let mut ejected: Vec<ChangeRec> = vec![];
            let mut budget: BTreeMap<String, Allowance> = BTreeMap::new();
            if rules.allowances.required {
                for (k, v) in self.list_prefix("a:").await? {
                    if let Ok(a) = serde_json::from_str::<Allowance>(&v) {
                        budget.insert(k, a);
                    }
                }
            }
            tm.load_ms = lap.take();
            // Fetch what the gate needs: by fork, several forks at once (trunk/prefetch.rs).
            let mut prefetched = self.prefetch(&mut rd, t, store, &rules, head, &recs).await?;
            tm.fetched = prefetched.requests;
            tm.fetch_max_ms = prefetched.max_ms;
            let prefetch_ms = lap.take();
            tm.fetch_ms = prefetch_ms;
            // The earliest time, from now, that a change waiting to fetch again is due.
            let mut retry_in: Option<f64> = None;
            // Changes whose PR size waits for base files: (key, base, commit).
            let mut no_size: Vec<(String, Oid, Oid)> = vec![];
            for mut rec in recs {
                let retry = fetch_retry(&rec);
                let input = input_of(&rec.req);
                // The base first: an older trunk commit. On Artifacts its commit alone (its trees
                // come when needed); elsewhere one commit and the trees that differ from head, so
                // that with it as a have, the fork sends only what the change adds.
                if !store.contains(&rec.req.base) {
                    let f0 = now();
                    tm.fetched += 1;
                    if rd.lazy {
                        self.resolve(&mut rd, Some(&rec.req.fork), t, store, rec.req.base, "commit").await?;
                    } else if let Err(e) = fetch_with(t, trunk, &[rec.req.base], &[head], FetchOpts::TREES, store).await {
                        if e.transient() {
                            return Err(g(e));
                        }
                        // Not on trunk: the fork sends it below, or the change is ejected as missing.
                    }
                    tm.fetch_ms += now() - f0;
                }
                let lacks_commit = !store.contains(&rec.req.commit);
                let ours: Vec<Oid> = if lacks_commit {
                    vec![]
                } else {
                    self.fill(&mut rd, Some(&rec.req.fork), t, store, |s| blobs_needed(s, &rules, &head, &input)).await?.map(|(o, _)| o).unwrap_or_default()
                };
                if lacks_commit || !ours.is_empty() {
                    if let Some((_, next_ms)) = retry {
                        if next_ms > now() {
                            retry_in = Some(retry_in.unwrap_or(f64::MAX).min(next_ms - now()));
                            continue;
                        }
                    }
                    let Some(fork) = self.get::<Fork>(&format!("f:{}", rec.req.fork)).await? else {
                        ejected.push(eject(rec, serde_json::json!({ "kind": "unknown_fork" })));
                        continue;
                    };
                    let remote = Remote::new(&fork.remote, Some(fork.authorization.clone()));
                    // The change's commit, then the files its gate and landing read from its side,
                    // fetched by id. The prefetch asked for both; this is what it could not get.
                    // On Artifacts, a commit that git cannot fetch comes through the binding.
                    let f0 = now();
                    let fetched: nerudo_git::Result<usize> = match prefetched.errors.remove(&rec.req.commit) {
                        // Its own request failed in the prefetch: that was its fetch this round
                        // (on Artifacts, the binding reads it next).
                        Some(e) if !rd.binding() => Err(e),
                        _ => {
                            let mut fetched = Ok(0);
                            if !store.contains(&rec.req.commit) {
                                tm.fetched += 1;
                                if !rd.lazy {
                                    fetched = fetch_change(t, &remote, rec.req.commit, &[head, rec.req.base], store).await;
                                }
                                if rd.binding() && !store.contains(&rec.req.commit) {
                                    fetched = match self.resolve(&mut rd, Some(&rec.req.fork), t, store, rec.req.commit, "commit").await {
                                        Ok(true) => Ok(1),
                                        Ok(false) => fetched.and(Err(nerudo_git::Error::Missing(format!("{} (commit)", rec.req.commit)))),
                                        Err(e) => Err(nerudo_git::Error::Transport(e.to_string())),
                                    };
                                }
                            }
                            if fetched.is_ok() {
                                let ours = self.fill(&mut rd, Some(&rec.req.fork), t, store, |s| blobs_needed(s, &rules, &head, &input)).await?.map(|(o, _)| o).unwrap_or_default();
                                if !ours.is_empty() {
                                    tm.fetched += 1;
                                    fetched = fetch_with(t, &remote, &ours, &[], FetchOpts::default(), store).await;
                                }
                            }
                            fetched
                        }
                    };
                    tm.fetch_ms += now() - f0;
                    tm.fetch_max_ms = tm.fetch_max_ms.max(now() - f0);
                    if let Err(e) = fetched {
                        let attempts = retry.map_or(1, |(a, _)| a + 1);
                        if !e.transient() || attempts >= FETCH_ATTEMPTS {
                            ejected.push(eject(rec, serde_json::json!({ "kind": "fetch_failed", "detail": e.to_string(), "attempts": attempts })));
                            continue;
                        }
                        // The fork or the network may come back: a blip must not close the PR.
                        let delay = (FETCH_RETRY_MS * 2f64.powi(attempts as i32 - 1)).min(FETCH_RETRY_MAX_MS);
                        console_warn!("gate: fetch_retry {} attempt {attempts}, next in {delay} ms: {e}", rec.req.key());
                        rec.reason = Some(serde_json::json!({ "kind": "fetch_retry", "detail": e.to_string(), "attempts": attempts, "next_ms": (now() + delay) as i64 }));
                        self.put(&format!("c:{}", rec.req.key()), &rec).await?;
                        let mut stats: Stats = self.get("stats").await?.unwrap_or_default();
                        stats.fetch_retries += 1;
                        self.put("stats", &stats).await?;
                        tm.fetch_retries += 1;
                        retry_in = Some(retry_in.unwrap_or(f64::MAX).min(delay));
                        continue;
                    }
                }
                if retry.is_some() {
                    rec.reason = None;
                }
                // Head's side: the files head moved on the edited paths, nerudo.toml, and the
                // directories the rules regenerate. Trunk has them.
                if let Ok((_, theirs)) = self.fill(&mut rd, Some(&rec.req.fork), t, store, |s| blobs_needed(s, &rules, &head, &input)).await? {
                    if !theirs.is_empty() {
                        let f0 = now();
                        tm.fetched += 1;
                        let fetched = fetch_with(t, trunk, &theirs, &[], FetchOpts::default(), store).await;
                        tm.fetch_ms += now() - f0;
                        if let Err(e) = fetched {
                            if e.transient() {
                                return Err(g(e));
                            }
                        }
                    }
                }
                let units = match footprint(store, &rules, &input) {
                    Ok(u) => u,
                    Err(e) => {
                        ejected.push(eject(rec, serde_json::to_value(&e)?));
                        continue;
                    }
                };
                rec.footprint = units.iter().cloned().collect();
                if rec.subject.is_empty() {
                    rec.subject = store.commit(&rec.req.commit).map(|c| c.message.lines().next().unwrap_or("").to_string()).unwrap_or_default();
                }
                if rec.size.is_none() {
                    // None while a base file it counts is not here: the landing loop reads those
                    // while it pushes, or before it parks the change.
                    rec.size = prinfo::change_size(store, &rec.req.base, &rec.req.commit);
                    if rec.size.is_none() {
                        no_size.push((rec.req.key(), rec.req.base, rec.req.commit));
                    }
                    self.put(&format!("c:{}", rec.req.key()), &rec).await?;
                }
                let paths = match edits(store, &input) {
                    Ok(p) => p,
                    Err(e) => {
                        ejected.push(eject(rec, serde_json::to_value(&e)?));
                        continue;
                    }
                };
                // What the change adds must fit in the object beside trunk, now and at landing.
                let bytes = change_bytes(store, &input).unwrap_or(0);
                if bytes > MAX_CHANGE_BYTES {
                    ejected.push(eject(rec, serde_json::json!({
                        "kind": "too_big", "detail": format!("the change adds {bytes} bytes of files; a change may add at most {MAX_CHANGE_BYTES}"), "bytes": bytes,
                    })));
                    continue;
                }
                let edits_rules = paths.iter().any(|p| nerudo_core::rules::is_rules_path(p));
                let reasons = match seal_boundaries(store, &rules, &input) {
                    Ok(r) => r,
                    // A file it must read is not here: the same ejection as a missing tree.
                    Err(detail) => {
                        ejected.push(eject(rec, serde_json::to_value(Ejection::Missing { detail })?));
                        continue;
                    }
                };
                if reasons.iter().any(|r| r.chars().any(char::is_control)) {
                    // Reasons become commit trailers; a path can't be allowed to forge one.
                    ejected.push(eject(rec, serde_json::json!({ "kind": "bad_path", "detail": "a changed path contains a control character" })));
                    continue;
                }
                // A decision model ([decide]) can add one reason: a review, or a signature.
                let mut reasons = reasons;
                if let Some(r) = self.decide_risk(&rules, &mut rec, store, &input, &paths, &reasons).await {
                    reasons.push(r);
                }
                if !reasons.is_empty() {
                    let (seals, tallies) = self.tally_for(&rules, &rec, &reasons).await?;
                    let mut trailers = None;
                    let mut refused = None;
                    if met(&tallies) {
                        trailers = Some(seal_trailers(&rules, &rec.req.commit, &reasons, &seals));
                        rec.sealed_by = tallies.iter().flat_map(|t| t.have.iter().cloned()).collect::<BTreeSet<_>>().into_iter().collect();
                    } else if let Some(bg) = self.get::<AdminNote>(&format!("bg:{}", rec.req.key())).await? {
                        if paths.iter().all(|p| nerudo_core::rules::is_rules_path(p)) {
                            trailers = Some(vec![format!("{BREAK_GLASS}: {}", bg.reason)]);
                        } else {
                            refused = Some(format!("break-glass only lands a change that edits {RULES_PATH} alone"));
                        }
                    }
                    let Some(trailers) = trailers else {
                        rec.state = Phase::AwaitingSeal;
                        rec.seal_reasons = reasons.clone();
                        rec.reason = Some(serde_json::json!({ "kind": "needs_seal", "reasons": reasons, "quorum": tallies, "break_glass_refused": refused }));
                        parked.push(rec);
                        continue;
                    };
                    rec.seal_reasons = reasons;
                    ready.push(Ready { rec, units, allowance: None, trailers, edits_rules });
                    continue;
                }
                if rules.allowances.required {
                    let digests = rules.digests();
                    let pick = budget.iter_mut().find(|(_, a)| a.covers(&units, now() as i64, &digests)).map(|(k, a)| {
                        a.reserved += 1;
                        k.clone()
                    });
                    match pick {
                        Some(k) => ready.push(Ready { rec, units, allowance: Some(k), trailers: vec![], edits_rules }),
                        None => {
                            rec.state = Phase::AwaitingAllowance;
                            rec.reason = Some(serde_json::json!({ "kind": "needs_allowance", "units": units }));
                            parked.push(rec);
                        }
                    }
                } else {
                    ready.push(Ready { rec, units, allowance: None, trailers: vec![], edits_rules });
                }
            }
            tm.parked = parked.len();
            if !parked.is_empty() {
                // A parked PR shows its size to the people who approve it.
                prinfo::fill_sizes(t, trunk, store, &mut parked.iter_mut().collect::<Vec<_>>()).await;
                self.park(&rules, &parked).await?;
            }
            tm.gate_ms = lap.take() - (tm.fetch_ms - prefetch_ms);
            if let Some(ms) = retry_in.filter(|_| ready.is_empty() && ejected.is_empty() && parked.is_empty()) {
                // Only changes that wait to fetch again are left: wake when the first is due.
                self.state.storage().set_alarm(Duration::from_millis(ms.ceil() as u64)).await?;
                self.round_done(tm, round_start);
                return Ok(());
            }
            // A task's competing attempts get a trial and wait for a pick instead of merging.
            let ready = self.compare_divert(&rules, store, head, ready, &mut budget, &mut ejected).await?;
            tm.divert_ms = lap.take();

            // Batch: footprint-disjoint, in arrival order; a change that overlaps an earlier
            // deferred one also waits, so conflicting changes keep their order. A change to
            // nerudo.toml lands last in its batch: every change before it was gated under
            // the rules at its parent commit, and the next batch is gated under the new ones.
            let mut taken = BTreeSet::new();
            let mut blocked = BTreeSet::new();
            let mut batch = vec![];
            let mut released: Vec<String> = vec![];
            let mut closed = false;
            // Changes that wait for a later batch keep their footprint on record, so agents
            // can see what in-flight work touches (GET .../queue).
            let mut waiting: Vec<(String, String)> = vec![];
            for r in ready {
                if !closed && batch.len() < cfg.batch_cap && r.units.is_disjoint(&taken) && r.units.is_disjoint(&blocked) {
                    taken.extend(r.units.iter().cloned());
                    closed = r.edits_rules;
                    batch.push((r.rec, r.allowance, r.trailers));
                } else {
                    waiting.push((format!("c:{}", r.rec.req.key()), serde_json::to_string(&r.rec)?));
                    blocked.extend(r.units);
                    if let Some(a) = r.allowance {
                        released.push(a);
                    }
                }
            }
            tm.batched = batch.len();
            tm.waiting = waiting.len();
            if !waiting.is_empty() {
                self.put_all(waiting).await?;
            }
            for a in released {
                if let Some(al) = budget.get_mut(&a) {
                    al.reserved -= 1;
                }
            }

            // From here until the batch is recorded, a new revision of these PRs waits; a member
            // whose PR got a newer revision in this round is left out (trunk/revisions.rs).
            let (batch, _merging) = self.fence(batch, &mut budget).await?;
            tm.batched = batch.len();
            tm.pick_ms = lap.take();

            // Apply over trees, checking trunk invariants after each change.
            let committer = Signature::new("nerudo", "nerudo@nerudo.invalid", (now() / 1000.0) as i64);
            let mut tip = head;
            let mut landed: Vec<(ChangeRec, Landing)> = vec![];
            for (mut rec, allowance, trailers) in batch {
                let input = ChangeInput { trailers: self.context_trailers(trailers, &rec.req).await?, ..input_of(&rec.req) };
                // A file the landing reads that the gate did not fetch (head moved under the
                // batch): fetch it from trunk and apply again.
                let mut outcome = apply_change(store, &rules, tip, &input, &committer);
                for _ in 0..MISSING_TRIES {
                    let Err(Ejection::Missing { detail }) = &outcome else { break };
                    let Some((oid, kind)) = objects::missing_in(detail) else { break };
                    if !self.resolve(&mut rd, Some(&rec.req.fork), t, store, oid, &kind).await? {
                        break;
                    }
                    outcome = apply_change(store, &rules, tip, &input, &committer);
                }
                match outcome {
                    Ok(applied) => {
                        let files = self.fill(&mut rd, None, t, store, |s| check_files(s, &rules, &applied.tree)).await?;
                        let checked = files.map_err(|e| e.to_string()).and_then(|files| trunk_checks(store, &rules, &files));
                        if let Err(detail) = checked {
                            if let Some(al) = allowance.as_ref().and_then(|a| budget.get_mut(a)) {
                                al.reserved -= 1;
                            }
                            ejected.push(eject(rec, serde_json::json!({ "kind": "check_failed", "detail": detail })));
                            continue;
                        }
                        tip = applied.commit;
                        rec.allocated = applied.allocated.clone();
                        let l = Landing {
                            key: rec.req.key(),
                            commit_in: rec.req.commit,
                            commit_out: applied.commit,
                            allocated: applied.allocated,
                            allowance: allowance.clone(),
                            sealed_by: rec.sealed_by.clone(),
                        };
                        rec.allowance = allowance;
                        landed.push((rec, l));
                    }
                    Err(e) => {
                        if let Some(al) = allowance.as_ref().and_then(|a| budget.get_mut(a)) {
                            al.reserved -= 1;
                        }
                        // A conflict names the landings that beat it (trunk/beaten.rs).
                        if matches!(e, Ejection::Conflict { .. }) {
                            self.load_history(t, trunk, store, tip, rec.req.base).await;
                        }
                        let reason = self.apply_ejection(&rules, store, tip, &rec, e).await?;
                        ejected.push(eject(rec, reason));
                    }
                }
            }

            tm.apply_ms = lap.take();
            tm.landed = landed.len();
            tm.eject(&ejected);
            if landed.is_empty() {
                self.commit(&mut meta, None, vec![], ejected, &budget, &queue, 0, None).await?;
                tm.record_ms = lap.take();
                tm.events_ms = self.mem.borrow().events_ms;
                tm.requests = t.requests.get() - requests0;
                tm.rx_bytes = t.rx_bytes.get() - rx0;
                self.round_done(tm, round_start);
                continue;
            }

            // One CI run per batch: the type check from nerudo.toml, if any, then the
            // simulated duration that stands for the rest of a test suite.
            let mut ci_runs = 1;
            if let Some(tc) = &rules.ci.typecheck {
                self.typecheck_files(&mut rd, t, store, &tip).await?;
                tm.ci = "passed".into();
                if let Verdict::Failed(reason) = self.checked(store, &tip, tc).await? {
                    ci_runs += 1;
                    tm.ci = "failed".into();
                    self.typecheck_files(&mut rd, t, store, &head).await?;
                    if !self.checked(store, &head, tc).await?.passed() {
                        // Trunk itself fails, so the failure says nothing about these changes.
                        // Eject nobody; a later batch that makes trunk pass can still land.
                        console_warn!("trunk {head} fails the type check; {} changes wait", landed.len());
                        self.state.storage().set_alarm(Duration::from_secs(30)).await?;
                        tm.ci = "trunk_red".into();
                        tm.ci_ms = lap.take();
                        tm.landed = 0;
                        tm.eject(&[]);
                        self.round_done(tm, round_start);
                        return Ok(());
                    }
                    // Each change is one commit on the previous: halve the range between a
                    // prefix that passes and one that fails (trunk/culprit.rs) to find the
                    // change that broke the build. Land the changes before it; the changes
                    // after it stay queued and are applied again on the new trunk.
                    let mut search = culprit::Search::new(landed.len());
                    let mut reason = reason;
                    while let Some(i) = search.next() {
                        ci_runs += 1;
                        self.typecheck_files(&mut rd, t, store, &landed[i].1.commit_out).await?;
                        match self.checked(store, &landed[i].1.commit_out, tc).await? {
                            Verdict::Passed => search.report(i, true),
                            Verdict::Failed(r) => {
                                search.report(i, false);
                                reason = r;
                            }
                        }
                    }
                    let i = search.culprit();
                    let (rec, _) = landed.split_off(i).swap_remove(0);
                    tip = landed.last().map(|(_, l)| l.commit_out).unwrap_or(head);
                    // The receipt names the landings that moved main under it (trunk/beaten.rs).
                    self.load_history(t, trunk, store, tip, rec.req.base).await;
                    let reason = self.main_moved(store, tip, &rec, reason).await?;
                    ejected.push(eject(rec, reason));
                    if landed.is_empty() {
                        tm.eject(&ejected);
                        self.commit(&mut meta, None, vec![], ejected, &budget, &queue, ci_runs, None).await?;
                        tm.ci_ms = lap.take();
                        tm.landed = 0;
                        self.round_done(tm, round_start);
                        continue;
                    }
                }
            }
            if cfg.ci_ms > 0 {
                Delay::from(Duration::from_millis(cfg.ci_ms)).await;
            }
            tm.ci_ms = lap.take();

            let batch_no = meta.batches + 1;
            let attempt = Attempt { batch: batch_no, start: head, tip, landed: landed.iter().map(|(_, l)| l.clone()).collect() };
            self.put("attempt", &attempt).await?;
            // Only what the batch changed: never a walk of trunk's whole tree or history.
            let objs = store.new_objects(&tip, &head).map_err(g)?;
            let pack = write_pack(&store.object_list(&objs).map_err(g)?);
            tm.pack_ms = lap.take();
            tm.pack_bytes = pack.len();
            // The base files that PR sizes still count, read from trunk while the push runs.
            let size_files: BTreeSet<Oid> = no_size.iter().flat_map(|(_, b, c)| prinfo::size_files(store, b, c)).collect();
            let size_body = fetch_body(&size_files.into_iter().collect::<Vec<_>>(), &[], FetchOpts::default(), store);
            let update = [RefUpdate { name: cfg.branch.clone(), old: head, new: tip }];
            let pushing = push(t, trunk, &update, pack);
            let (report, sizes) = match size_body {
                Some(body) => {
                    let (r, p) = futures::future::join(pushing, fetch_pack(t, trunk, body)).await;
                    (r, Some(p))
                }
                None => (pushing.await, None),
            };
            let report = report.map_err(g)?;
            if let Some(Err(e)) = sizes.map(|p| p.and_then(|p| ingest(&p, store))) {
                console_warn!("PR sizes: the files could not be read: {e}");
            }
            for (rec, _) in landed.iter_mut().filter(|(r, _)| r.size.is_none()) {
                rec.size = prinfo::change_size(store, &rec.req.base, &rec.req.commit);
            }
            tm.push_ms = lap.take();
            if report.all_ok() && cfg.debug_crash_after_push > 0 {
                // Test hook: die between the push and the landing record, as a real crash would.
                let mut c = cfg.clone();
                c.debug_crash_after_push -= 1;
                self.put("cfg", &c).await?;
                return Err(Error::RustError("simulated crash after push".into()));
            }
            let landed_n = landed.len();
            // A push that main refused records nothing: these changes are gated again.
            tm.eject(if report.all_ok() { &ejected } else { &[] });
            if report.all_ok() {
                tm.waits_ms = landed.iter().map(|(r, _)| now() - metrics::queued_since(r)).collect();
                let active = self.rules_at(&mut rd, t, store, tip).await?;
                self.note_authority(&active).await?;
                self.commit(&mut meta, Some((batch_no, tip)), landed, ejected, &budget, &queue, ci_runs, Some(&active)).await?;
                // The changes that wait for a later batch keep the sizes read with the push.
                self.store_sizes(store, &no_size, &attempt).await?;
                if rd.lazy {
                    // A new isolate reads the new head from storage, not through the binding.
                    self.keep_commit(store, &tip).await?;
                }
                head = tip;
            } else {
                // Someone moved trunk without us: never overwrite. Re-read it and retry.
                let mut stats: Stats = self.get("stats").await?.unwrap_or_default();
                stats.external_moves += 1;
                let refs = ls_refs(t, trunk, &cfg.branch).await.map_err(g)?;
                let actual = refs.iter().find(|(n, _)| n == &cfg.branch).map(|(_, o)| *o).unwrap_or(head);
                self.load_commit(&mut rd, t, store, actual, Some(head)).await?;
                meta.head = Some(actual);
                self.put_all(vec![
                    ("meta".into(), serde_json::to_string(&meta)?),
                    ("search_dirty".into(), serde_json::to_string(&search_index::Dirty { head: actual, seq: meta.seq })?),
                    ("stats".into(), serde_json::to_string(&stats)?),
                    ("attempt".into(), "null".into()),
                ])
                .await?;
                self.notify_search_dirty(actual, meta.seq);
                console_warn!("trunk moved outside the sequencer: {report:?}");
                head = actual;
            }
            tm.record_ms = lap.take();
            tm.events_ms = self.mem.borrow().events_ms;
            tm.batch = report.all_ok().then_some(batch_no);
            tm.landed = if report.all_ok() { landed_n } else { 0 };
            tm.requests = t.requests.get() - requests0;
            tm.rx_bytes = t.rx_bytes.get() - rx0;
            self.round_done(tm, round_start);
            self.notify();
        }
        // Batch limit reached with work left: continue in a fresh alarm.
        self.state.storage().set_alarm(Duration::from_millis(10)).await?;
        Ok(())
    }

    /// Record a batch: log entries, receipts, head, queue, allowance charges and stats in one
    /// atomic write, which also clears the intent record. The rules and their Git head go
    /// in that same write, so recovery cannot leave a recorded landing paired with old
    /// approvers or quorums. This also lets older stored state repair its authority marker.
    #[allow(clippy::too_many_arguments)]
    async fn commit(
        &self,
        meta: &mut Meta,
        landed_tip: Option<(u64, Oid)>,
        landed: Vec<(ChangeRec, Landing)>,
        ejected: Vec<ChangeRec>,
        budget: &BTreeMap<String, Allowance>,
        _queue_seen: &[String],
        ci_runs: u64,
        authority: Option<&Rules>,
    ) -> Result<()> {
        let mut stats: Stats = self.get("stats").await?.unwrap_or_default();
        let mut items: Vec<(String, String)> = vec![];
        let ts = now() as i64;
        let landed_recs: Vec<ChangeRec> = landed.iter().map(|(r, l)| ChangeRec { trunk_commit: Some(l.commit_out), ..r.clone() }).collect();
        let ejected_recs: Vec<ChangeRec> = ejected.clone();
        let batch_no = landed_tip.map(|(b, _)| b);
        let mut budget = budget.clone();
        let done: BTreeSet<String> = landed.iter().map(|(r, _)| r.req.key()).chain(ejected.iter().map(|r| r.req.key())).collect();
        if let Some((batch, tip)) = landed_tip {
            for (mut rec, l) in landed {
                meta.seq += 1;
                let e = LogEntry::new(meta.seq, &rec.req.change_id, rec.req.rev, &l.commit_in.to_hex(), &l.commit_out.to_hex(), batch, ts, &meta.prev_hash);
                meta.prev_hash = e.hash.clone();
                items.push((format!("log:{:012}", e.seq), serde_json::to_string(&e)?));
                rec.state = Phase::Landed;
                rec.seq = Some(meta.seq);
                rec.trunk_commit = Some(l.commit_out);
                rec.landed_ms = Some(ts);
                rec.batch = Some(batch);
                rec.ci_runs = ci_runs;
                items.push((format!("c:{}", rec.req.key()), serde_json::to_string(&rec)?));
                if let Some(a) = l.allowance.as_ref().and_then(|a| budget.get_mut(a)) {
                    a.reserved -= 1;
                    a.spent += 1;
                }
                stats.landed += 1;
            }
            meta.head = Some(tip);
            meta.batches = batch;
            stats.batches += 1;
            stats.pushes += 1;
            stats.batch_sizes.push(items.iter().filter(|(k, _)| k.starts_with("log:")).count() as u32);
        }
        stats.ci_runs += ci_runs;
        for rec in ejected {
            match rec.reason.as_ref().and_then(|r| r["kind"].as_str()) {
                Some("conflict") => stats.ejected_conflict += 1,
                Some("check_failed") => stats.ejected_check += 1,
                Some("ci_failed") | Some("ci_crashed") | Some("ci_timeout") => stats.ejected_ci += 1,
                Some("not_picked") => stats.not_picked += 1,
                _ => stats.ejected_other += 1,
            }
            items.push((format!("c:{}", rec.req.key()), serde_json::to_string(&rec)?));
        }
        for (k, a) in budget.iter_mut() {
            a.reserved = 0; // reservations never outlive a batch
            items.push((k.clone(), serde_json::to_string(a)?));
        }
        // Re-read the queue: agents may have submitted while we were fetching and pushing.
        let queue: Vec<String> = self.get("q").await?.unwrap_or_default();
        let parked = self.parked_keys().await?;
        let mut queue: Vec<String> = queue.into_iter().filter(|k| !done.contains(k) && !parked.contains(k)).collect();
        let active = match authority { Some(r) => r.clone(), None => self.rules().await? };
        self.wake_allowances(&active, &budget, &done, &mut queue, &mut items).await?;
        items.push(("q".into(), serde_json::to_string(&queue)?));
        items.push(("meta".into(), serde_json::to_string(meta)?));
        items.push(("stats".into(), serde_json::to_string(&stats)?));
        items.push(("attempt".into(), "null".into()));
        if let Some((_, head)) = landed_tip {
            let active = authority.ok_or_else(|| Error::RustError("a landing record needs rules from its Git head".into()))?;
            items.push(("rules".into(), serde_json::to_string(active)?));
            items.push(("rules_head".into(), serde_json::to_string(&head)?));
            items.push(("search_dirty".into(), serde_json::to_string(&search_index::Dirty { head, seq: meta.seq })?));
        }
        let crash = self.get::<u32>("debug_crash_after_record").await?.unwrap_or(0);
        if landed_tip.is_some() && crash > 0 {
            items.push(("debug_crash_after_record".into(), (crash - 1).to_string()));
        }
        self.put_all(items).await?;
        if landed_tip.is_some() && crash > 0 {
            return Err(Error::RustError("simulated crash after landing record".into()));
        }
        if let Some((_, head)) = landed_tip {
            self.notify_search_dirty(head, meta.seq);
            // Background scheduling is best effort; the durable outbox survives
            // a failed alarm write and is retried on the next request or landing.
            let _ = self.ensure_alarm().await;
        }
        let t0 = now();
        let res = self.after_batch(batch_no, &landed_recs, &ejected_recs, ci_runs).await;
        self.mem.borrow_mut().events_ms = now() - t0;
        res
    }

    /// The verdict of the type check of `commit`. A check that gives no answer fails the run
    /// (the loop stalls and tries again, trunk/stall.rs) until CI_TIMEOUT_TRIES runs in a row
    /// ended that way: then the tree is what takes too long, not the CI Worker, and the
    /// timeout is a failing verdict, so the culprit search ejects the change that brought it.
    /// Head is checked first on a failing batch: a trunk that takes too long ejects nobody.
    async fn checked(&self, store: &Store, commit: &Oid, tc: &Typecheck) -> Result<Verdict> {
        match ci::typecheck(&self.env, store, commit, tc).await {
            Err(e) if timed_out(&e.to_string(), self.stall().await?.as_ref()) => Ok(Verdict::Failed(serde_json::json!({
                "kind": "ci_timeout", "check": "typecheck",
                "detail": format!("the type check gave no answer in {} s, {CI_TIMEOUT_TRIES} runs in a row: the tree is too much for it", ci::TIMEOUT_MS / 1000),
            }))),
            other => other,
        }
    }

    /// `POST .../changes/{cid}/close {as?, reason?}` (admin, or an agent with its key for its
    /// own PR): close an open PR that waits for a person (an approval, an allowance or a
    /// pick). `as` names whom the admin closes it for, like its author. A queued PR merges or
    /// closes on its own in seconds, so it can't be closed, except by the admin while the
    /// trunk is paused: the way out when the landing loop cannot get past it.
    async fn close_change(&self, caller: &agents::Caller, cid: &str, body: serde_json::Value) -> Result<Response> {
        let Some(rev) = self.get::<u32>(&format!("latest:{cid}")).await? else {
            return bad(404, format!("unknown change {cid}"));
        };
        let key = format!("{cid}:{rev}");
        let Some(rec) = self.get::<ChangeRec>(&format!("c:{key}")).await? else {
            return bad(404, format!("unknown change {cid}"));
        };
        let mut by = body["as"].as_str().filter(|a| !a.is_empty()).unwrap_or("an admin").to_string();
        if let (false, Some(me)) = (caller.admin, caller.agent.as_deref()) {
            if self.caller_fork(caller).await?.as_deref() != Some(rec.req.fork.as_str()) {
                return bad(403, format!("{cid} is not a PR of {me}. An agent closes only its own PRs."));
            }
            by = me.to_string();
        }
        let phase = rec.state.clone();
        // A queued PR normally merges or closes on its own in seconds. When the landing loop
        // cannot get past it (a check that never answers on its tree, say), the admin takes it
        // out of the queue: with the trunk paused and no run in progress, so no run has it in
        // a batch that would write its record again.
        let queued_by_admin = phase == Phase::Queued && caller.admin && self.get::<AdminNote>("paused").await?.is_some() && !self.mem.borrow().busy;
        if !matches!(phase, Phase::AwaitingSeal | Phase::AwaitingAllowance | Phase::AwaitingPick) && !queued_by_admin {
            return bad(409, format!("{cid} does not wait for a person: only such a PR can be closed. The admin can close a queued PR while the trunk is paused (POST .../pause)"));
        }
        let by = by.chars().take(64).collect::<String>();
        let note: String = body["reason"].as_str().map(social::clean).unwrap_or_default().chars().take(300).collect();
        let list_key = if phase == Phase::Queued { "q".to_string() } else { parked_key(&phase) };
        let list: Vec<String> = self.get::<Vec<String>>(&list_key).await?.unwrap_or_default().into_iter().filter(|k| *k != key).collect();
        let mut stats: Stats = self.get("stats").await?.unwrap_or_default();
        stats.ejected_other += 1;
        let rec = eject(rec, serde_json::json!({ "kind": "closed", "by": by, "detail": note }));
        self.put_all(vec![
            (format!("c:{key}"), serde_json::to_string(&rec)?),
            (list_key, serde_json::to_string(&list)?),
            ("stats".into(), serde_json::to_string(&stats)?),
        ])
        .await?;
        let why = if note.is_empty() { String::new() } else { format!(": {note}") };
        self.event("ejected", format!("{by} closed {cid}{why}"), Some(cid), rec.req.task.as_deref(), Some(&by)).await?;
        self.notify();
        Response::from_json(&rec)
    }

    async fn park(&self, rules: &Rules, recs: &[ChangeRec]) -> Result<()> {
        let mut seal: Vec<String> = self.get(&parked_key(&Phase::AwaitingSeal)).await?.unwrap_or_default();
        let mut allow: Vec<String> = self.get(&parked_key(&Phase::AwaitingAllowance)).await?.unwrap_or_default();
        let mut stats: Stats = self.get("stats").await?.unwrap_or_default();
        let mut items = vec![];
        // Changes parked for a seal for the first time ask their quorums' teams to review.
        let mut newly_sealable: Vec<ChangeRec> = vec![];
        for r in recs {
            let k = r.req.key();
            // A seal that arrived while the gate was fetching found the change still queued and
            // left it there; don't park it on a stale tally. It's gated again next round.
            if r.state == Phase::AwaitingSeal && met(&self.tally_for(rules, r, &r.seal_reasons).await?.1) {
                continue;
            }
            match r.state {
                Phase::AwaitingSeal if !seal.contains(&k) => {
                    seal.push(k.clone());
                    stats.awaiting_seal += 1;
                    newly_sealable.push(r.clone());
                }
                Phase::AwaitingAllowance if !allow.contains(&k) => {
                    allow.push(k.clone());
                    stats.awaiting_allowance += 1;
                }
                _ => {}
            }
            items.push((format!("c:{k}"), serde_json::to_string(r)?));
        }
        let all: BTreeSet<&String> = seal.iter().chain(allow.iter()).collect();
        let queue: Vec<String> = self.get::<Vec<String>>("q").await?.unwrap_or_default().into_iter().filter(|k| !all.contains(k)).collect();
        items.push((parked_key(&Phase::AwaitingSeal), serde_json::to_string(&seal)?));
        items.push((parked_key(&Phase::AwaitingAllowance), serde_json::to_string(&allow)?));
        items.push(("q".into(), serde_json::to_string(&queue)?));
        items.push(("stats".into(), serde_json::to_string(&stats)?));
        self.put_all(items).await?;
        for r in &newly_sealable {
            self.auto_review(r).await?;
            self.event("parked", format!("{} needs approval: {}", prinfo::pr_label(r), r.seal_reasons.join("; ")), Some(&r.req.change_id), r.req.task.as_deref(), None).await?;
        }
        self.notify();
        Ok(())
    }

    async fn parked_keys(&self) -> Result<BTreeSet<String>> {
        let a: Vec<String> = self.get(&parked_key(&Phase::AwaitingSeal)).await?.unwrap_or_default();
        let b: Vec<String> = self.get(&parked_key(&Phase::AwaitingAllowance)).await?.unwrap_or_default();
        let c: Vec<String> = self.get(&parked_key(&Phase::AwaitingPick)).await?.unwrap_or_default();
        Ok(a.into_iter().chain(b).chain(c).collect())
    }

    async fn list_prefix(&self, prefix: &str) -> Result<Vec<(String, String)>> {
        let map = self.state.storage().list_with_options(ListOptions::new().prefix(prefix)).await?;
        let mut out = vec![];
        for entry in map.entries() {
            let pair: js_sys::Array = entry?.into();
            if let (Some(k), Some(v)) = (pair.get(0).as_string(), pair.get(1).as_string()) {
                if v != "null" {
                    out.push((k, v));
                }
            }
        }
        Ok(out)
    }

    /// An intent record survived a crash: find out from trunk whether the push happened.
    async fn reconcile(&self, cfg: &Config, t: &FetchTransport, trunk: &Remote, store: &mut Store, att: Attempt, meta: &mut Meta) -> Result<Oid> {
        let refs = ls_refs(t, trunk, &cfg.branch).await.map_err(g)?;
        let actual = refs.iter().find(|(n, _)| n == &cfg.branch).map(|(_, o)| *o).unwrap_or(att.start);
        // A push with no answer can still land later: send the same push again, never a new
        // batch from the same start (trunk/repush.rs).
        let actual = self.push_again(cfg, t, trunk, store, &att, actual).await?;
        let mut stats: Stats = self.get("stats").await?.unwrap_or_default();
        stats.recovered_attempts += 1;
        self.put("stats", &stats).await?;
        if actual == att.tip {
            // The push landed; record it exactly as the success path would have.
            let mut landed = vec![];
            for l in &att.landed {
                if let Some(mut rec) = self.get::<ChangeRec>(&format!("c:{}", l.key)).await? {
                    let mut l = l.clone();
                    // Current intents name the storage key; older ones could name the bare
                    // allowance id. Normalize before reserving and charging that same key.
                    l.allowance = l.allowance.as_deref().map(|id| {
                        if id.starts_with("a:") { id.to_string() } else { format!("a:{id}") }
                    });
                    rec.allocated = l.allocated.clone();
                    rec.allowance = l.allowance.clone();
                    rec.sealed_by = l.sealed_by.clone();
                    landed.push((rec, l));
                }
            }
            let mut budget = BTreeMap::new();
            for (k, v) in self.list_prefix("a:").await? {
                if let Ok(mut a) = serde_json::from_str::<Allowance>(&v) {
                    a.reserved += landed.iter().filter(|(_, l)| l.allowance.as_deref() == Some(k.as_str())).count() as u32;
                    budget.insert(k, a);
                }
            }
            let mut rd = self.readers(cfg).await?;
            if !store.contains(&actual) {
                self.load_commit(&mut rd, t, store, actual, Some(att.start)).await?;
            }
            let active = self.rules_at(&mut rd, t, store, actual).await?;
            self.note_authority(&active).await?;
            self.commit(meta, Some((att.batch, att.tip)), landed, vec![], &budget, &[], 1, Some(&active)).await?;
            Ok(actual)
        } else {
            // The push didn't happen (or trunk moved elsewhere): the changes stay queued.
            if actual != att.start {
                let mut rd = self.readers(cfg).await?;
                self.load_commit(&mut rd, t, store, actual, Some(att.start)).await?;
            }
            meta.head = Some(actual);
            self.put_all(vec![("meta".into(), serde_json::to_string(meta)?), ("attempt".into(), "null".into()),
                ("search_dirty".into(), serde_json::to_string(&search_index::Dirty { head: actual, seq: meta.seq })?)]).await?;
            self.notify_search_dirty(actual, meta.seq);
            Ok(actual)
        }
    }
}

/// `error` is a type check that gave no answer, and the loop's last CI_TIMEOUT_TRIES runs
/// ended with one too (the stall record, trunk/stall.rs): the tree, not the CI Worker.
fn timed_out(error: &str, stall: Option<&stall::Stall>) -> bool {
    error.starts_with(ci::UNAVAILABLE)
        && error.contains(ci::TIMED_OUT)
        && stall.is_some_and(|s| s.kind == "ci_unavailable" && s.attempts >= CI_TIMEOUT_TRIES && s.detail.contains(ci::TIMED_OUT))
}

fn parked_key(p: &Phase) -> String {
    match p {
        Phase::AwaitingSeal => "parked:seal".into(),
        Phase::AwaitingAllowance => "parked:allowance".into(),
        Phase::AwaitingPick => "parked:pick".into(),
        _ => "parked:none".into(),
    }
}

/// The tries so far and the time of the next one, for a change that waits to fetch again.
fn fetch_retry(rec: &ChangeRec) -> Option<(u64, f64)> {
    let r = rec.reason.as_ref().filter(|r| r["kind"] == "fetch_retry")?;
    Some((r["attempts"].as_u64().unwrap_or(1), r["next_ms"].as_f64().unwrap_or(0.0)))
}

fn eject(mut rec: ChangeRec, reason: serde_json::Value) -> ChangeRec {
    rec.state = Phase::Ejected;
    rec.reason = Some(reason);
    rec
}

fn input_of(r: &LandReq) -> ChangeInput {
    ChangeInput { change_id: r.change_id.clone(), rev: r.rev, commit: r.commit, base: r.base, trailers: vec![] }
}

fn read_rules(store: &Store, head: &Oid) -> Result<Rules> {
    let tree = store.commit(head).map_err(g)?.tree;
    let entry = nerudo_core::rules::rules_file(store, &tree).map_err(g)?.ok_or_else(|| Error::RustError(format!("trunk has no {RULES_PATH}")))?;
    let text = String::from_utf8_lossy(store.blob(&entry.oid).map_err(g)?).into_owned();
    Rules::parse(&text).map_err(Error::RustError)
}

/// `read_rules`, fetching nerudo.toml from `remote` when the store has only the trees.
async fn load_rules(t: &FetchTransport, remote: &Remote, store: &mut Store, head: &Oid) -> Result<Rules> {
    let tree = store.commit(head).map_err(g)?.tree;
    if let Some(e) = nerudo_core::rules::rules_file(store, &tree).map_err(g)? {
        fetch_with(t, remote, &[e.oid], &[], FetchOpts::default(), store).await.map_err(g)?;
    }
    read_rules(store, head)
}

/// A change's commit from its fork, with the trees and files it adds. If the fork refuses, or
/// its pack has a delta on a file the store lacks, the commit and trees only (the gate then
/// fetches the files it reads by id).
async fn fetch_change(t: &FetchTransport, fork: &Remote, commit: Oid, haves: &[Oid], store: &mut Store) -> nerudo_git::Result<usize> {
    match fetch_with(t, fork, &[commit], haves, FetchOpts::default(), store).await {
        Err(e) if !e.transient() => fetch_with(t, fork, &[commit], haves, FetchOpts { no_blobs: true, ..FetchOpts::default() }, store).await,
        other => other,
    }
}

/// Times a landing reads an object it lacks and tries again.
const MISSING_TRIES: usize = 64;

/// A seed path trunk can hold (the same rule as for every change: `merge::path_problem`).
fn valid_path(p: &str) -> bool {
    nerudo_core::merge::path_problem(p).is_none()
}

/// Write the seed as a root commit and push it to the new, empty trunk.
async fn push_seed(store: &mut Store, cfg: &Config, seed: &Seed, authorization: &str) -> Result<Oid> {
    let mut files = BTreeMap::new();
    for (path, text) in &seed.files {
        files.insert(path.clone(), FileEntry { mode: MODE_BLOB, oid: store.put_blob(text.as_bytes()) });
    }
    let tree = store.write_tree(&files);
    let sig = Signature::new("nerudo", "nerudo@trunk.invalid", (now() / 1000.0) as i64);
    let c = store.put_commit(&Commit { tree, parents: vec![], author: sig.clone(), committer: sig, extra: vec![], message: seed.message.clone() });
    let pack = write_pack(&store.object_list(&store.missing_from(&c, &[]).map_err(g)?).map_err(g)?);
    let remote = Remote::new(&cfg.remote, Some(authorization.to_string()));
    let update = [RefUpdate { name: cfg.branch.clone(), old: Oid::ZERO, new: c }];
    let report = push(&FetchTransport::default(), &remote, &update, pack).await.map_err(g)?;
    if !report.all_ok() {
        return Err(Error::RustError(format!("refused: {:?}", report.refs)));
    }
    Ok(c)
}

/// Check a signed seal or allowance: the signer is an approver in nerudo.toml on trunk,
/// and the signature covers this trunk and these rules.
fn check_signed<'r>(cfg: &Config, rules: &'r Rules, approver: &str, trunk: &str, digest: &str, msg: &[u8], sig: &str) -> std::result::Result<&'r Approver, String> {
    let a = rules.approver(approver).ok_or("signer is not an approver in nerudo.toml on trunk")?;
    if trunk != cfg.trunk {
        return Err("signed for a different trunk".into());
    }
    if !rules.accepts(digest) {
        return Err("signed against different landing rules".into());
    }
    auth::verify(approver, msg, sig)?;
    Ok(a)
}

/// Admin reasons go on trunk as a trailer: one line, no control characters, bounded.
fn one_line(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>().trim().chars().take(200).collect()
}

/// A change id: 1 to 64 of [A-Za-z0-9._-]. land refuses any other id, and a fork ref whose
/// name carries another id is not read as a change (agents name their own refs).
pub(crate) fn valid_change_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}
