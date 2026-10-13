//! `nerudo replay`: a real repo's recent history, rebuilt by many agents at once.
//!
//! 1. Start: main begins at an older upstream commit X, as one root commit with X's tree and
//!    a nerudo.toml. A public repo is imported into Artifacts with the binding's import();
//!    a repo Artifacts can't reach (a private one, or a local clone) goes up as a git pack.
//! 2. Replay: N agents take the upstream commits after X, oldest first. For each, an agent
//!    syncs main, applies the commit's diff onto it (three-way, with the sequencer's own
//!    merge), keeps the upstream subject and body (the why), pushes to its own fork and asks
//!    to land. When another landing conflicts with it, the agent rebuilds it on the new main.
//!    Merges are skipped, unless --merges: then each merge on the first-parent line is one PR,
//!    the diff from its first parent, as a squash merge would land it.
//!    A commit from a GitHub PR, "subject (#N)", starts where the PR's branch started: the
//!    newest main commit in refs/pull/N/head, once its replay has merged. The agent builds
//!    the diff on that commit and the sequencer moves it onto main, so PRs that were open at
//!    the same time upstream are open at the same time here. A diff that doesn't apply there
//!    starts from main as it is. `--no-branch-points` starts every commit from main.
//! 3. Open PRs (`--open N`, with `--yes`): a rules PR makes every PR wait for one
//!    maintainer's approval, as upstream review does. Then the N most recently updated open
//!    PRs of the GitHub upstream (by people, not drafts) go up, each from where its branch
//!    started, and wait for that approval. A PR whose branch started before the replayed
//!    range is left out. `--maintainers A,B` also hands the repo to its real maintainers in
//!    that rules PR: they replace the approvers, with no key until they join nerudo.
//! 4. Check: `nerudo verify` from outside, with a read-only token.
//!
//! The trunk gets a name on the Hub, `--as ORG/REPO` (by default the owner and name of an
//! https upstream, in lower case), public unless `--private`. Its id is
//! `{org}-{repo}-{unix seconds}` unless `--trunk` gives one.
//!
//! Upstream commits are read from a local clone with the git CLI; nothing upstream is
//! pushed anywhere but through the sequencer.

use crate::{die, env_or, hex, http, key_exists, keygen, load_key, my_name, now_ms, register, repo_name, seal, Api};
use futures::executor::block_on;
use nerudo_core::merge::Ejection;
use nerudo_core::{apply_change, ChangeInput, LogEntry, Rules, RULES_PATH};
use nerudo_git::{fetch, ls_refs, parse_pack, push, write_pack, Commit, FileEntry, Oid, RefUpdate, Remote, Signature, Store, Transport, MODE_BLOB};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const MAIN: &str = "refs/heads/main";
const KEY_SLOT: &str = "<your nerudo pubkey>";
const NAME_SLOT: &str = "<your name>";
const AGENTS_RULES: &str = include_str!("../../fixtures/replay/agents.toml");
const HONO_RULES: &str = include_str!("../../fixtures/replay/hono.toml");
const NERUDO_RULES: &str = include_str!("../../fixtures/replay/nerudo.toml");
/// Subjects of commits a replay skips by default: releases and version bumps, which a
/// release tool writes, not an agent.
const SKIP: &[&str] = &["chore(release)", "chore(deps)"];

/// git smart HTTP over the CLI's HTTP client.
struct Https;

impl Transport for Https {
    async fn request(&self, method: &str, url: &str, headers: &[(&str, String)], body: Option<Vec<u8>>) -> nerudo_git::Result<(u16, Vec<u8>)> {
        let mut req = http().request(method, url);
        for (k, v) in headers {
            req = req.set(k, v);
        }
        let resp = match body {
            Some(b) => req.send_bytes(&b),
            None => req.call(),
        };
        let resp = match resp {
            Ok(r) | Err(ureq::Error::Status(_, r)) => r,
            Err(e) => return Err(nerudo_git::Error::Transport(e.to_string())),
        };
        let status = resp.status();
        let mut out = Vec::new();
        resp.into_reader().read_to_end(&mut out).map_err(|e| nerudo_git::Error::Transport(e.to_string()))?;
        Ok((status, out))
    }
}

struct Out {
    t0: Instant,
    color: bool,
}

impl Out {
    fn paint(&self, code: &str, s: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
    fn at(&self, s: &str) {
        println!("{:>6.1}s  {s}", self.t0.elapsed().as_secs_f64());
    }
    fn more(&self, s: &str) {
        println!("         {s}");
    }
    fn section(&self, s: &str) {
        println!("\n{}", self.paint("1", &format!("== {s}")));
    }
}

fn git<T>(what: &str, r: nerudo_git::Result<T>) -> T {
    r.unwrap_or_else(|e| die(&format!("{what}: {e}")))
}

fn flag<T: std::str::FromStr>(f: &BTreeMap<String, String>, k: &str, default: T) -> T {
    f.get(k).map(|v| v.parse().unwrap_or_else(|_| die(&format!("--{k}: bad value {v:?}")))).unwrap_or(default)
}

/// Run git in `dir` with no credential helper (nothing here needs credentials).
fn gitc(dir: &Path, args: &[&str]) -> Vec<u8> {
    gitc_in(dir, args, None)
}

fn gitc_in(dir: &Path, args: &[&str], stdin: Option<&[u8]>) -> Vec<u8> {
    run_git(dir, args, stdin).unwrap_or_else(|e| die(&e))
}

fn run_git(dir: &Path, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    cmd.env("GIT_CONFIG_COUNT", "1").env("GIT_CONFIG_KEY_0", "credential.helper").env("GIT_CONFIG_VALUE_0", "").env("GIT_TERMINAL_PROMPT", "0");
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap_or_else(|e| die(&format!("git: {e}")));
    let writer = stdin.map(|data| {
        let mut w = child.stdin.take().expect("stdin");
        let data = data.to_vec();
        std::thread::spawn(move || {
            let _ = w.write_all(&data);
        })
    });
    let out = child.wait_with_output().unwrap_or_else(|e| die(&format!("git: {e}")));
    if let Some(w) = writer {
        let _ = w.join();
    }
    if !out.status.success() {
        return Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(out.stdout)
}

fn text(b: Vec<u8>) -> String {
    String::from_utf8_lossy(&b).trim().to_string()
}

/// One upstream commit to rebuild.
#[derive(Clone)]
struct Job {
    /// Its place in upstream order.
    idx: usize,
    cid: String,
    commit: Oid,
    parent: Oid,
    subject: String,
    message: String,
    author: Signature,
    /// The paths it changes.
    paths: BTreeSet<String>,
    /// An earlier upstream commit on the same paths that hasn't finished: this one doesn't
    /// apply until that one has landed (or failed).
    wait_for: Option<usize>,
    /// The next revision to send.
    rev: u32,
    /// Where the upstream PR's branch started. None: start from main as it is.
    fork: Option<Fork>,
}

/// The main commit an upstream PR's branch started from.
#[derive(Clone, Copy)]
enum Fork {
    /// The start commit, or an older one.
    Start,
    /// An earlier job's commit.
    Job(usize),
}

/// Which job an agent takes next. An agent takes any job (agents don't line up), but a job
/// that doesn't apply on main waits for the earlier job on the same paths it depends on.
#[derive(Default)]
struct Sched {
    /// Jobs no agent holds, oldest first.
    pending: VecDeque<Job>,
    /// Every job that hasn't finished (held or not), by index: the paths it changes.
    open: BTreeMap<usize, BTreeSet<String>>,
}

enum Take {
    Job(Box<Job>),
    Wait,
    Done,
}

impl Sched {
    fn take(&mut self) -> Take {
        if self.pending.is_empty() {
            return Take::Done;
        }
        let open = &self.open;
        match self.pending.iter().position(|j| j.wait_for.is_none_or(|w| !open.contains_key(&w))) {
            Some(i) => Take::Job(Box::new(self.pending.remove(i).expect("index in range"))),
            None => Take::Wait,
        }
    }

    /// Put a job back to wait for `blocker`, in upstream order.
    fn defer(&mut self, mut job: Job, blocker: usize) {
        job.wait_for = Some(blocker);
        let at = self.pending.iter().position(|j| j.idx > job.idx).unwrap_or(self.pending.len());
        self.pending.insert(at, job);
    }

    /// The latest earlier unfinished job that changes any of `job`'s paths.
    fn blocker(&self, job: &Job) -> Option<usize> {
        self.open.range(..job.idx).rev().find(|(_, p)| !p.is_disjoint(&job.paths)).map(|(k, _)| *k)
    }
}

struct Shared {
    out: Out,
    trunk: String,
    token: String,
    rules: Rules,
    retries: u32,
    yes: bool,
    /// Upstream commits plus main as the agents have fetched it.
    store: Mutex<Store>,
    /// Main commits already in `store`, for incremental fetches.
    known: Mutex<Vec<Oid>>,
    reader: Mutex<Option<Remote>>,
    sched: Mutex<Sched>,
    /// Change id of each job, by index.
    cids: Vec<String>,
    /// Times a job waited for an earlier one on the same paths.
    waits: AtomicUsize,
    /// change id -> subject, for the batch lines.
    subjects: Mutex<BTreeMap<String, String>>,
    landed: Mutex<BTreeSet<String>>,
    ejected_conflict: AtomicUsize,
    ejected_other: AtomicUsize,
    rebuilt: AtomicUsize,
    gave_up: Mutex<Vec<String>>,
    parked: Mutex<Vec<String>>,
    sealed: AtomicUsize,
    already: AtomicUsize,
    printed: AtomicU64,
    first_submit: Mutex<Option<Instant>>,
    /// Main's first commit, which the start commit became.
    root: Oid,
    /// The commit each job's replay merged as, by job index.
    outs: Mutex<BTreeMap<usize, Oid>>,
    /// Jobs that started where their upstream PR's branch started.
    branched: AtomicUsize,
}

pub fn run(f: &BTreeMap<String, String>) {
    let repo = f.get("repo").cloned().unwrap_or_else(|| die("--repo is required: an https:// git URL, or a path to a local clone"));
    let commits: usize = flag(f, "commits", 70).clamp(1, 500);
    let agents: usize = flag(f, "agents", 16).clamp(1, 64);
    let retries: u32 = flag(f, "retries", 3).clamp(0, 10);
    let branch = f.get("branch").cloned().unwrap_or_else(|| "main".into());
    let yes = f.contains_key("yes");
    let open: usize = flag(f, "open", 0).clamp(0, 30);
    if open > 0 && !yes {
        die("--open needs --yes: the rules PR that makes PRs wait for approval needs your seal");
    }
    let maintainers: Vec<String> = f.get("maintainers").map(|m| m.split(',').map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).collect()).unwrap_or_default();
    if !maintainers.is_empty() && open == 0 {
        die("--maintainers goes with --open: the rules PR hands the repo over");
    }
    if let Some(bad) = maintainers.iter().find(|n| !n.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))) {
        die(&format!("--maintainers: {bad:?} is not a name like octocat"));
    }
    let worker = env_or(&["NERUDO_WORKER"], Some("http://127.0.0.1:8787"));
    let admin = env_or(&["NERUDO_ADMIN_TOKEN"], None);
    let token = env_or(&["NERUDO_TOKEN", "NERUDO_AGENT_TOKEN"], None);
    let out = Out { t0: Instant::now(), color: std::io::stdout().is_terminal() };

    // 0. The upstream: a local clone, and the commits to replay.
    let local = Path::new(&repo).is_dir();
    let name = repo.trim_end_matches('/').trim_end_matches(".git").rsplit('/').next().unwrap_or("repo").to_string();
    let (org, short) = match f.get("as") {
        Some(n) => repo_name(n),
        None if local => die("--as ORG/REPO is required for a local clone: it is the repo's name on the hub"),
        None => repo_name(&default_name(&repo)),
    };
    let trunk = match f.get("trunk") {
        Some(t) if t.contains('/') => die("--trunk is the new trunk's id; name the repo with --as ORG/REPO"),
        Some(t) => t.clone(),
        None => trunk_for(&org, &short, now_ms() / 1000),
    };
    let (dir, tip_rev, source) = if local {
        let dir = PathBuf::from(&repo);
        let tip = f.get("rev").cloned().unwrap_or_else(|| "HEAD".into());
        let label = text(gitc(&dir, &["rev-parse", "--show-toplevel"])).rsplit('/').next().unwrap_or("repo").to_string();
        (dir, tip, format!("{label} (local clone)"))
    } else {
        if !repo.starts_with("https://") {
            die("--repo: use an https:// URL (Artifacts imports public https remotes) or a local path");
        }
        let owner_name = repo.trim_end_matches('/').trim_end_matches(".git").splitn(4, '/').nth(3).unwrap_or(&name).to_string();
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        let dir = f.get("clone").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(format!("{home}/.cache/nerudo/replay/{}", owner_name.replace('/', "-"))));
        let depth = (commits + 2).to_string();
        if dir.join(".git").is_dir() {
            gitc(&dir, &["fetch", "-q", "--depth", &depth, "origin", &branch]);
            (dir, f.get("rev").cloned().unwrap_or_else(|| "FETCH_HEAD".into()), owner_name)
        } else {
            std::fs::create_dir_all(dir.parent().unwrap_or(Path::new("."))).unwrap_or_else(|e| die(&format!("{}: {e}", dir.display())));
            gitc(Path::new("."), &["clone", "-q", "--single-branch", "--branch", &branch, "--depth", &depth, &repo, &dir.to_string_lossy()]);
            (dir, f.get("rev").cloned().unwrap_or_else(|| "HEAD".into()), owner_name)
        }
    };
    let tip = text(gitc(&dir, &["rev-parse", &format!("{tip_rev}^{{commit}}")]));
    let mut chain: Vec<String> = text(gitc(&dir, &["rev-list", "--first-parent", &format!("--max-count={}", commits + 1), &tip])).lines().map(str::to_string).collect();
    chain.reverse();
    if chain.len() < 2 {
        die("the upstream has fewer than two commits to start from");
    }
    let start = chain[0].clone();
    let merges = f.contains_key("merges");
    let mut skip: Vec<String> = if f.contains_key("no-skip") { vec![] } else { SKIP.iter().map(|s| s.to_string()).collect() };
    if let Some(more) = f.get("skip") {
        skip.extend(more.split('|').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()));
    }
    let mut jobs = vec![];
    let mut skipped = vec![];
    let mut cids = BTreeSet::new();
    for c in &chain[1..] {
        let meta = gitc(&dir, &["log", "-1", "--format=%P%x00%an%x00%ae%x00%at%x00%B", c]);
        let meta = String::from_utf8_lossy(&meta).into_owned();
        let parts: Vec<&str> = meta.splitn(5, '\0').collect();
        let parents: Vec<&str> = parts[0].split_whitespace().collect();
        let message = parts.get(4).map(|m| m.trim_end().to_string() + "\n").unwrap_or_default();
        let subject = message.lines().next().unwrap_or("").to_string();
        let lower = subject.to_lowercase();
        // A merge on the first-parent line is a branch that was merged: with --merges it is
        // one PR, the diff from its first parent (a squash merge).
        if parents.len() != 1 && !(merges && parents.len() > 1) {
            skipped.push(format!("{}  {subject}  (a merge)", &c[..7]));
            continue;
        }
        if !skip.is_empty() && (skip.iter().any(|s| lower.starts_with(s.as_str())) || is_release(&lower)) {
            skipped.push(format!("{}  {subject}  (a release)", &c[..7]));
            continue;
        }
        let mut cid = pr_number(&subject).map(|n| format!("pr{n}")).unwrap_or_else(|| format!("c{}", &c[..7]));
        if !cids.insert(cid.clone()) {
            cid = format!("{cid}-{}", &c[..7]);
            cids.insert(cid.clone());
        }
        let when: i64 = parts.get(3).and_then(|t| t.trim().parse().ok()).unwrap_or(0);
        let changed = gitc(&dir, &["diff-tree", "-r", "--no-renames", "--name-only", "-z", parents[0], c]);
        let paths: BTreeSet<String> = String::from_utf8_lossy(&changed).split('\0').filter(|p| !p.is_empty()).map(str::to_string).collect();
        jobs.push(Job {
            idx: jobs.len(),
            paths,
            wait_for: None,
            cid,
            commit: Oid::from_hex(c).unwrap_or_else(|_| die("bad oid from git")),
            parent: Oid::from_hex(parents[0]).unwrap_or_else(|_| die("bad oid from git")),
            subject,
            message,
            author: Signature::new(parts.get(1).copied().unwrap_or("upstream"), parts.get(2).copied().unwrap_or("upstream@invalid"), when),
            rev: 1,
            fork: None,
        });
    }
    let github = !local && repo.starts_with("https://github.com/");
    if github && !f.contains_key("no-branch-points") {
        branch_points(&repo, &dir, &chain, &mut jobs);
    }
    // The job that replays each upstream commit, by its place in `chain`.
    let at: BTreeMap<String, usize> = chain.iter().enumerate().map(|(i, c)| (c.clone(), i)).collect();
    let job_at: BTreeMap<usize, usize> = jobs.iter().filter_map(|j| at.get(&j.commit.to_hex()).map(|i| (*i, j.idx))).collect();

    println!("{}", out.paint("1", &format!("nerudo replay: {source} on {worker}, repo {org}/{short}, trunk {trunk}")));
    println!("  upstream  {} at {}, {} commits after {} ({} skipped)", dir.display(), &tip[..7], jobs.len(), &start[..7], skipped.len());
    for s in skipped.iter().take(12) {
        println!("  skip      {s}");
    }

    // The upstream objects the agents need: the start commit's whole tree, then everything
    // the replayed commits add.
    let objects = |args: &[&str]| -> Vec<u8> {
        let list = gitc(&dir, args);
        let oids: String = String::from_utf8_lossy(&list).lines().filter_map(|l| l.split_whitespace().next()).map(|o| format!("{o}\n")).collect();
        gitc_in(&dir, &["pack-objects", "--stdout", "-q"], Some(oids.as_bytes()))
    };
    let start_pack = objects(&["rev-list", "--objects", "--no-walk", &start]);
    let later_pack = objects(&["rev-list", "--objects", &tip, &format!("^{start}")]);
    let mut store = Store::new();
    for p in [&start_pack, &later_pack] {
        for o in git("read the upstream pack", parse_pack(p, Some(&store))) {
            store.insert(o);
        }
    }
    let start_oid = Oid::from_hex(&start).unwrap_or_else(|_| die("bad start oid"));
    let start_files = git("read the start tree", store.flatten(&git("read the start commit", store.commit(&start_oid)).tree));
    let bytes: usize = start_files.values().filter_map(|e| store.blob(&e.oid).ok()).map(<[u8]>::len).sum();
    println!("  start     {}  {} files, {:.1} MB; packs {:.1} MB + {:.1} MB", &start[..7], start_files.len(), bytes as f64 / 1e6, start_pack.len() as f64 / 1e6, later_pack.len() as f64 / 1e6);

    // 1. Rules and the trunk.
    if !key_exists() {
        keygen(false);
        println!("created your approver key at ~/.nerudo/key");
    }
    let approver = hex(&load_key().verifying_key().to_bytes());
    let rules_text = match f.get("rules") {
        Some(p) => std::fs::read_to_string(p).unwrap_or_else(|e| die(&format!("--rules {p}: {e}"))),
        None if name.contains("agents") => AGENTS_RULES.to_string(),
        None if name.contains("hono") => HONO_RULES.to_string(),
        None => NERUDO_RULES.to_string(),
    }
    .replace(KEY_SLOT, &approver)
    .replace(NAME_SLOT, &my_name());
    let rules = Rules::parse(&rules_text).unwrap_or_else(|e| die(&format!("rules: {e}")));

    out.section("1. Start main from the upstream");
    let mut import = json!({ "commit": start, "rules": rules_text, "source": source, "branch": branch, "depth": commits + 2 });
    if local || f.contains_key("pack") {
        println!("Artifacts can't import a local clone, so the start commit's tree goes up as a git pack.");
        import["pack"] = json!(base64(&start_pack));
    } else {
        println!("Artifacts imports {repo} (the binding's import()), and the Trunk Durable Object");
        println!("makes main from it: one root commit with the start tree and a nerudo.toml.");
        import["url"] = json!(repo);
    }
    // --ci-ms: how long each batch's CI takes. PRs that arrive meanwhile share the next batch.
    let init = json!({ "trunk": trunk, "host": { "kind": "artifacts", "binding": "ARTIFACTS", "repo": trunk }, "ci_ms": flag(f, "ci-ms", 0).min(30_000), "import": import });
    let t_init = Instant::now();
    let created = slow_post(&worker, &trunk, &admin, "init", &init);
    let s = |v: &Value| v.as_str().unwrap_or("?").to_string();
    let root = Oid::from_hex(&s(&created["head"])).unwrap_or_else(|_| die("init returned no head"));
    println!("  main      {}  in {:.1} s; trunk {}", &s(&created["head"])[..7], t_init.elapsed().as_secs_f64(), s(&created["remote"]));
    let admin_api = Api::with_token(&trunk, admin.clone());
    let stats = admin_api.call("GET", "stats", None);
    println!("  memory    {} after init; {} objects cached", mb(&stats["wasm_memory_bytes"]), stats["objects_cached"]);
    let public = !f.contains_key("private");
    let upstream = (!local).then(|| repo.trim_end_matches('/').to_string());
    let description = f.get("description").cloned().or_else(|| upstream.as_deref().and_then(github_description));
    let web = match register(&trunk, &org, &short, public, description.as_deref(), upstream.as_deref()) {
        Ok(link) => {
            println!("  repo      {org}/{short} ({})  {link}", if public { "public" } else { "private" });
            Some(link)
        }
        Err(e) => {
            println!("  (the hub did not take the name {org}/{short}: {e})");
            None
        }
    };
    let mut share_key = String::new();
    if let Ok(board) = admin_api.try_call("POST", "share", Some(json!({}))) {
        let url = board["url"].as_str().unwrap_or("?");
        share_key = url.split("k=").nth(1).and_then(|r| r.split('&').next()).unwrap_or_default().to_string();
        println!("  board     {url}   (read-only)");
    }
    if let Ok(mine) = admin_api.try_call("POST", "invite", Some(json!({ "user": my_name() }))) {
        println!("  you       {}", mine["url"].as_str().unwrap_or("?"));
    }

    // 2. The agents.
    out.section(&format!("2. {agents} agents rebuild {} upstream commits", jobs.len()));
    println!("Each agent takes the next upstream commit, applies its diff onto main as it is now,");
    println!("keeps the upstream message (its why), pushes to its own fork and asks to land.");
    println!();
    let planned = jobs.len();
    let sh = Shared {
        out: Out { t0: Instant::now(), ..out },
        trunk: trunk.clone(),
        token: token.clone(),
        rules,
        retries,
        yes,
        store: Mutex::new(store),
        known: Mutex::new(vec![]),
        reader: Mutex::default(),
        subjects: Mutex::new(jobs.iter().map(|j| (j.cid.clone(), j.subject.clone())).collect()),
        cids: jobs.iter().map(|j| j.cid.clone()).collect(),
        sched: Mutex::new(Sched { open: jobs.iter().map(|j| (j.idx, j.paths.clone())).collect(), pending: jobs.into_iter().collect() }),
        waits: AtomicUsize::new(0),
        landed: Mutex::default(),
        ejected_conflict: AtomicUsize::new(0),
        ejected_other: AtomicUsize::new(0),
        rebuilt: AtomicUsize::new(0),
        gave_up: Mutex::default(),
        parked: Mutex::default(),
        sealed: AtomicUsize::new(0),
        already: AtomicUsize::new(0),
        printed: AtomicU64::new(0),
        first_submit: Mutex::default(),
        root,
        outs: Mutex::default(),
        branched: AtomicUsize::new(0),
    };
    let done = AtomicBool::new(false);
    let api = Api::with_token(&trunk, token);
    let mut wall = 0.0;
    std::thread::scope(|scope| {
        let watcher = scope.spawn(|| watch(&sh, &done));
        let workers: Vec<_> = (1..=agents)
            .map(|n| {
                let sh = &sh;
                // Forks are made a few at a time by the sequencer; stagger the asks a little.
                std::thread::sleep(Duration::from_millis(50));
                scope.spawn(move || agent(sh, n))
            })
            .collect();
        for w in workers {
            w.join().unwrap_or_else(|_| die("an agent thread panicked"));
        }
        wall = sh.first_submit.lock().unwrap().map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);
        let seq = api.call("GET", "stats", None)["seq"].as_u64().unwrap_or(0);
        while sh.printed.load(Ordering::SeqCst) < seq {
            std::thread::sleep(Duration::from_millis(100));
        }
        done.store(true, Ordering::SeqCst);
        watcher.join().unwrap_or_else(|_| die("the batch watcher panicked"));
    });

    let waiting = if open > 0 && github { open_prs(&sh, &repo, &dir, &chain, &job_at, open, &maintainers) } else { 0 };

    // 4. Numbers, then a check from outside. Open the file tree once the way the web app
    // does, so the object also holds the UI's copy of main and the memory number counts it.
    if !share_key.is_empty() {
        let url = format!("{}/t/{trunk}/api/tree?path=", worker.trim_end_matches('/'));
        if let Err(e) = http().get(&url).set("X-Nerudo-Key", &share_key).call() {
            println!("  (the web app's tree did not load: {e})");
        }
    }
    let stats = api.call("GET", "stats", None);
    let n = |k: &str| stats[k].as_u64().unwrap_or(0);
    let landed = sh.landed.lock().unwrap().len();
    sh.out.section("Numbers");
    println!("  agents      {agents}, each with its own Artifacts fork");
    println!("  commits     {planned} replayed from {source} ({} skipped as merges or releases)", skipped.len());
    println!("  merged      {landed} of {planned} in {} batches; {:.0} s from first ask to last agent done ({:.1}/min)", n("batches"), wall, landed as f64 * 60.0 / wall.max(0.1));
    println!("  conflicts   {} closed by a conflict on main and rebuilt by their agent ({} rebuilds)", sh.ejected_conflict.load(Ordering::SeqCst), sh.rebuilt.load(Ordering::SeqCst));
    println!("  waits       {} times an agent's commit did not apply yet and waited for an earlier one on the same files", sh.waits.load(Ordering::SeqCst));
    println!("  branches    {} started where their upstream PR's branch started; the rest started from main as it was", sh.branched.load(Ordering::SeqCst));
    if sh.ejected_other.load(Ordering::SeqCst) > 0 {
        println!("  closed      {} for another reason", sh.ejected_other.load(Ordering::SeqCst));
    }
    if sh.already.load(Ordering::SeqCst) > 0 {
        println!("  empty       {} already on main (no change left to land)", sh.already.load(Ordering::SeqCst));
    }
    if open > 0 {
        println!("  open        {waiting} upstream PRs wait for a maintainer's approval");
    }
    let parked = sh.parked.lock().unwrap().clone();
    println!("  approvals   {} sealed by you; {} still waiting{}", sh.sealed.load(Ordering::SeqCst), parked.len(), if parked.is_empty() { String::new() } else { format!(": {}", parked.join(", ")) });
    let gave_up = sh.gave_up.lock().unwrap().clone();
    if !gave_up.is_empty() {
        println!("  needs you   {} that could not land (each depends on a commit that did not land, or closed {} times): {}", gave_up.len(), retries + 1, gave_up.join(", "));
    }
    println!("  memory      {} of WebAssembly memory in the Trunk object; {} objects in the sequencer's store, {} in the UI's", mb(&stats["wasm_memory_bytes"]), n("objects_cached"), n("ui_objects_cached"));
    println!("  git calls   {} by the sequencer", n("git_requests"));
    if let Some(link) = &web {
        println!("  repo        {org}/{short}  {link}");
    }
    println!();
    if !f.contains_key("no-verify") && !crate::demo::verify(&api, &trunk) {
        die("trunk check failed");
    }
}

/// ORG/REPO from an https upstream: its owner and name, in lower case, as org and repo
/// names allow.
fn default_name(url: &str) -> String {
    let path = url.trim_end_matches('/').trim_end_matches(".git").splitn(4, '/').nth(3).unwrap_or_default().to_lowercase();
    let owner: String = path.split('/').next().unwrap_or_default().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let name: String = path.rsplit('/').next().unwrap_or_default().chars().map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '-' }).collect();
    format!("{}/{name}", owner.trim_start_matches('-'))
}

/// A new trunk's id for ORG/REPO, as an Artifacts repo name allows it ([a-z0-9_-], no "--").
fn trunk_for(org: &str, repo: &str, secs: i64) -> String {
    let mut id: String = format!("{org}-{repo}-{secs}").to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '-' }).collect();
    while id.contains("--") {
        id = id.replace("--", "-");
    }
    id.trim_matches('-').to_string()
}

/// The description GitHub has for an https://github.com/OWNER/NAME upstream, if it answers.
fn github_description(url: &str) -> Option<String> {
    let path = url.strip_prefix("https://github.com/")?.trim_end_matches('/').trim_end_matches(".git");
    let r = http().get(&format!("https://api.github.com/repos/{path}")).set("Accept", "application/vnd.github+json").timeout(Duration::from_secs(10)).call().ok()?;
    let v: Value = serde_json::from_str(&r.into_string().ok()?).ok()?;
    v["description"].as_str().map(|d| d.trim().to_string()).filter(|d| !d.is_empty())
}

fn mb(v: &Value) -> String {
    match v.as_u64() {
        Some(b) => format!("{:.0} MB", b as f64 / 1_048_576.0),
        None => "n/a".into(),
    }
}

/// A release or version bump: "release …", "chore(adapters): release …", "Version Packages",
/// "v4.9.1". `subject` is lower case.
fn is_release(subject: &str) -> bool {
    // Drop a conventional-commit prefix like "chore(adapters): ".
    let rest = match subject.split_once(": ") {
        Some((kind, rest)) if !kind.contains(' ') => rest,
        _ => subject,
    };
    let version = |w: &str| {
        let w = w.trim_start_matches('v');
        w.split('.').count() >= 3 && w.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
    };
    ["release", "version packages", "bump version", "update changesets", "publish"].iter().any(|p| rest.starts_with(p))
        || rest.split_whitespace().next().is_some_and(version)
}

/// Where each job's upstream PR branched from main: the newest main commit in the history of
/// GitHub's refs/pull/N/head. The PR heads go into a clone of their own, fetched shallow, so
/// the main clone keeps its history. A PR whose branch point isn't in the replayed range
/// (or that GitHub doesn't have) starts from main as it is.
fn branch_points(repo: &str, dir: &Path, chain: &[String], jobs: &mut [Job]) {
    let prs: Vec<(usize, u64)> = jobs.iter().filter_map(|j| pr_number(&j.subject).map(|n| (j.idx, n))).collect();
    if prs.is_empty() {
        return;
    }
    let heads = match pr_heads(repo, dir, &prs.iter().map(|(_, n)| *n).collect::<Vec<_>>()) {
        Ok(h) => h,
        Err(e) => {
            println!("  (no PR branch points: {e})");
            return;
        }
    };
    let at: BTreeMap<&str, usize> = chain.iter().enumerate().map(|(i, c)| (c.as_str(), i)).collect();
    let job_at: BTreeMap<usize, usize> = jobs.iter().filter_map(|j| at.get(j.commit.to_hex().as_str()).map(|i| (*i, j.idx))).collect();
    let mut found = 0;
    let mut depth = 0;
    for (idx, n) in prs {
        let own = at.get(jobs[idx].commit.to_hex().as_str()).copied().unwrap_or(0);
        let Some(point) = newest_main(&heads, n, &at, own) else { continue };
        // A branch point the replay skipped (a merge or a release) starts from the job before it.
        jobs[idx].fork = Some(match job_at.range(..=point).next_back() {
            Some((_, j)) => Fork::Job(*j),
            None => Fork::Start,
        });
        found += 1;
        depth += own - point;
    }
    println!("  branches  {found} PRs branched from a main commit in range, on average {:.1} commits before they merged", depth as f64 / found.max(1) as f64);
}

/// GitHub's refs/pull/N/head for each N, fetched shallow into a clone next to `dir` (as
/// refs/pr/N), so the main clone keeps its history.
fn pr_heads(repo: &str, dir: &Path, numbers: &[u64]) -> Result<PathBuf, String> {
    let heads = PathBuf::from(format!("{}-prs", dir.to_string_lossy().trim_end_matches('/')));
    if !heads.join("HEAD").is_file() {
        std::fs::create_dir_all(&heads).map_err(|e| format!("{}: {e}", heads.display()))?;
        run_git(&heads, &["init", "-q", "--bare"], None)?;
    }
    let specs: Vec<String> = numbers.iter().map(|n| format!("+refs/pull/{n}/head:refs/pr/{n}")).collect();
    let mut args = vec!["fetch", "-q", "--depth=60", "--no-tags", repo];
    args.extend(specs.iter().map(String::as_str));
    run_git(&heads, &args, None)?;
    Ok(heads)
}

/// The newest commit of `chain` (by its place in it, below `below`) in PR N's history.
fn newest_main(heads: &Path, n: u64, at: &BTreeMap<&str, usize>, below: usize) -> Option<usize> {
    let list = run_git(heads, &["rev-list", "--max-count=400", &format!("refs/pr/{n}")], None).ok()?;
    String::from_utf8_lossy(&list).lines().filter_map(|o| at.get(o).copied()).filter(|i| *i < below).max()
}

/// nerudo.toml with an approval rule for every path, next to its governance lines.
fn with_approval(rules: &str) -> Option<String> {
    let mut out = vec![];
    let (mut table, mut paths, mut quorum) = ("", false, false);
    for line in rules.lines() {
        out.push(line.to_string());
        if line.starts_with('[') {
            table = line.trim();
        } else if line.starts_with("governance =") && table == "[seals]" {
            out.push("# Every PR waits for one maintainer's approval, as upstream review does.".into());
            out.push(r#"approval = ["**"]"#.into());
            paths = true;
        } else if line.starts_with("governance =") && table == "[seals.require]" {
            out.push(r#"approval = { roles = ["maintainer"], count = 1 }"#.into());
            quorum = true;
        }
    }
    (paths && quorum).then(|| out.join("\n") + "\n")
}

/// nerudo.toml with `maintainers` as its only approvers: no key (they have not joined), the
/// maintainer role. The approvers come last in the replay's rules files.
fn handed_over(rules: &str, maintainers: &[String]) -> String {
    let head = rules.find("[[approvers]]").map(|i| &rules[..i]).unwrap_or(rules);
    let mut out = head.trim_end().to_string();
    out += "\n\n# The upstream maintainers. They have not joined nerudo, so they have no key here and\n# can't approve yet. When one joins, an admin adds their key (nerudo break-glass).\n";
    for m in maintainers {
        out += &format!("\n[[approvers]]\nname = \"{m}\"\nroles = [\"maintainer\"]\n");
    }
    out
}

/// Push `c` to the seat's fork and ask to land it; returns its receipt once it leaves the queue.
fn send(a: &mut Seat, cid: &str, c: Oid, base: Oid, pack: Vec<u8>, branch: Option<&str>) -> Value {
    let update = RefUpdate { name: format!("refs/heads/land/{cid}-1"), old: Oid::ZERO, new: c };
    let report = git("push to fork", block_on(push(&Https, &a.fork, &[update], pack)));
    if !report.all_ok() {
        die(&format!("{}: push to its fork refused: {:?}", a.name, report.refs));
    }
    a.fork_has.push(c);
    let mut req = json!({ "change_id": cid, "rev": 1, "fork": a.fork_name, "commit": c, "base": base });
    if let Some(b) = branch {
        req["branch"] = json!(b);
    }
    if let Err(e) = a.api.try_call("POST", "land", Some(req)) {
        die(&format!("{cid}: land refused: {e}"));
    }
    receipt(&a.api, cid, 1)
}

/// 3. Open PRs: a rules PR makes every PR wait for a maintainer's approval, then the newest
/// open upstream PRs go up from where their branches started. Returns how many wait.
fn open_prs(sh: &Shared, repo: &str, dir: &Path, chain: &[String], job_at: &BTreeMap<usize, usize>, want: usize, maintainers: &[String]) -> usize {
    sh.out.section(&format!("3. Up to {want} open upstream PRs wait for a maintainer"));
    let mut seats: Vec<(Seat, Remote)> = (1..=4).map(|n| seat(sh, format!("a{n:02}"))).collect();

    // The rules PR, sealed by you: it edits nerudo.toml.
    let (a, reader) = &mut seats[0];
    let tip = sync_main(sh, reader);
    let built = {
        let mut store = sh.store.lock().unwrap();
        let mut files = git("read main", store.flatten(&git("read main", store.commit(&tip)).tree));
        let old = files.get(RULES_PATH).and_then(|e| store.blob(&e.oid).ok()).map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();
        with_approval(&old).map(|new| if maintainers.is_empty() { new } else { handed_over(&new, maintainers) }).map(|new| {
            files.insert(RULES_PATH.into(), FileEntry { mode: MODE_BLOB, oid: store.put_blob(new.as_bytes()) });
            let tree = store.write_tree(&files);
            let me = Signature::new(&a.name, &format!("{}@agents.invalid", a.name), now_ms() / 1000);
            let mut message = "nerudo.toml: every PR waits for one maintainer's approval\n\nUpstream, a maintainer reviews each PR before it merges. From this commit on,\nnerudo holds every PR until one maintainer approves it.\n".to_string();
            if !maintainers.is_empty() {
                message += &format!("\nThe maintainers are the upstream maintainers: {}. They have not joined nerudo,\nso they have no key here yet. The approver that ran the replay leaves.\n", maintainers.join(", "));
            }
            let c = store.put_commit(&Commit { tree, parents: vec![tip], author: me.clone(), committer: me, extra: vec![], message });
            let stops: Vec<Oid> = a.fork_has.iter().filter(|o| store.contains(o)).copied().collect();
            (c, write_pack(&git("pack", store.missing_from(&c, &stops).and_then(|m| store.object_list(&m)))))
        })
    };
    let Some((c, pack)) = built else {
        println!("  (nerudo.toml has no [seals] governance lines to add the approval rule to; no open PRs)");
        return 0;
    };
    let cid = "require-approval";
    let mut rec = send(a, cid, c, tip, pack, None);
    if rec["state"] == "awaiting_seal" && seal(&a.api, &sh.trunk, cid, Some("1"), true, "30m").is_some() {
        rec = receipt(&a.api, cid, 1);
    }
    if rec["state"] != "landed" {
        println!("  the rules PR did not merge ({}); no open PRs", rec["state"]);
        return 0;
    }
    sh.out.at(&format!("{}  {cid}  merged: every PR now waits for one maintainer's approval", a.name));

    // The upstream's open PRs, newest first.
    let Some(path) = repo.strip_prefix("https://github.com/").map(|p| p.trim_end_matches('/').trim_end_matches(".git")) else { return 0 };
    let url = format!("https://api.github.com/repos/{path}/pulls?state=open&sort=updated&direction=desc&per_page=60");
    let list: Vec<Value> = match http().get(&url).set("Accept", "application/vnd.github+json").timeout(Duration::from_secs(20)).call() {
        Ok(r) => serde_json::from_str(&r.into_string().unwrap_or_default()).unwrap_or_default(),
        Err(e) => {
            println!("  (GitHub did not list the open PRs: {e})");
            return 0;
        }
    };
    // People's PRs, ready for review: no drafts, no bots' dependency bumps.
    let list: Vec<&Value> = list.iter().filter(|p| p["draft"] != true && p["user"]["type"] != "Bot" && p["number"].is_u64()).collect();
    let heads = match pr_heads(repo, dir, &list.iter().filter_map(|p| p["number"].as_u64()).collect::<Vec<_>>()) {
        Ok(h) => h,
        Err(e) => {
            println!("  (no PR heads: {e})");
            return 0;
        }
    };
    let at: BTreeMap<&str, usize> = chain.iter().enumerate().map(|(i, c)| (c.as_str(), i)).collect();
    let tip = sync_main(sh, &seats[0].1);
    let mut waiting = 0;
    for p in list {
        if waiting == want {
            break;
        }
        let n = p["number"].as_u64().unwrap_or(0);
        let title = p["title"].as_str().unwrap_or("").trim();
        let Some(point) = newest_main(&heads, n, &at, chain.len()) else {
            sh.out.at(&format!("#{n}  left out: its branch started before the replayed range"));
            continue;
        };
        let from = chain[point].as_str();
        // Its branch point as the replay merged it.
        let on = match job_at.range(..=point).next_back() {
            Some((_, j)) => sh.outs.lock().unwrap().range(..=*j).next_back().map(|(_, o)| *o).unwrap_or(sh.root),
            None => sh.root,
        };
        let Ok(head) = run_git(&heads, &["rev-parse", &format!("refs/pr/{n}")], None).map(text) else { continue };
        let objects = run_git(&heads, &["rev-list", "--objects", &format!("refs/pr/{n}"), &format!("^{from}")], None).and_then(|list| {
            let oids: String = String::from_utf8_lossy(&list).lines().filter_map(|l| l.split_whitespace().next()).map(|o| format!("{o}\n")).collect();
            run_git(&heads, &["pack-objects", "--stdout", "-q"], Some(oids.as_bytes()))
        });
        let (Ok(objects), Ok(head), Ok(base)) = (objects, Oid::from_hex(&head), Oid::from_hex(from)) else { continue };
        let cid = format!("pr{n}");
        let (a, _) = &mut seats[waiting % 4];
        let built = {
            let mut store = sh.store.lock().unwrap();
            for o in git("read the PR pack", parse_pack(&objects, Some(&store))) {
                store.insert(o);
            }
            let me = Signature::new(&a.name, &format!("{}@agents.invalid", a.name), now_ms() / 1000);
            // The PR as one commit on its branch point, as a squash merge would land it.
            let upstream = git("read the PR head", store.commit(&head));
            let squash = store.put_commit(&Commit { parents: vec![base], ..upstream.clone() });
            let input = ChangeInput { change_id: cid.clone(), rev: 1, commit: squash, base, trailers: vec![] };
            // Where it branched; or, when its diff doesn't apply there (the replay merged the
            // commits around it in another order), main as it is now.
            let mut tries = [on, tip].into_iter().filter_map(|o| apply_change(&mut store, &sh.rules, o, &input, &me).ok().map(|there| (o, there)));
            match tries.next() {
                Some((on, there)) if there.tree != git("read the branch point", store.commit(&on)).tree => {
                    let author = upstream.author.clone();
                    let body: String = p["body"].as_str().unwrap_or("").replace('\r', "").trim().chars().take(3000).collect();
                    let html = p["html_url"].as_str().unwrap_or("");
                    let message = format!("{title} (#{n})\n\n{}Upstream-PR: {html}\n", if body.is_empty() { String::new() } else { format!("{body}\n\n") });
                    let c = store.put_commit(&Commit { tree: there.tree, parents: vec![on], author, committer: me, extra: vec![], message });
                    let stops: Vec<Oid> = a.fork_has.iter().filter(|o| store.contains(o)).copied().collect();
                    Some((c, on, write_pack(&git("pack", store.missing_from(&c, &stops).and_then(|m| store.object_list(&m))))))
                }
                _ => None,
            }
        };
        let Some((c, on, pack)) = built else {
            sh.out.at(&format!("#{n}  left out: its diff doesn't apply where its branch started, or on main"));
            continue;
        };
        sh.subjects.lock().unwrap().insert(cid.clone(), format!("{title} (#{n})"));
        let rec = send(a, &cid, c, on, pack, p["head"]["label"].as_str());
        let from = match on == tip {
            true => "from main as it is now".to_string(),
            false => format!("branched {} commits before the tip", chain.len() - 1 - point),
        };
        sh.out.at(&format!("{}  {cid}  {}: {title} ({from})", a.name, rec["state"].as_str().unwrap_or("?").replace('_', " ")));
        if rec["state"] == "awaiting_seal" {
            waiting += 1;
        }
    }
    waiting
}

/// The commit a job's first revision starts from: where its upstream PR branched, as the
/// replay merged it (or the nearest earlier commit that has merged).
fn fork_base(sh: &Shared, job: &Job) -> Option<Oid> {
    let outs = sh.outs.lock().unwrap();
    match job.fork? {
        Fork::Start => Some(sh.root),
        Fork::Job(j) => Some(outs.range(..=j).next_back().map(|(_, o)| *o).unwrap_or(sh.root)),
    }
}

/// "fix: something (#2519)" -> 2519.
fn pr_number(subject: &str) -> Option<u64> {
    let s = subject.trim_end();
    let inner = s.strip_suffix(')')?;
    let at = inner.rfind("(#")?;
    inner[at + 2..].parse().ok()
}

/// POST to the Worker with a long timeout: an import of a big repo takes a while.
fn slow_post(worker: &str, trunk: &str, token: &str, path: &str, body: &Value) -> Value {
    let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(15)).timeout(Duration::from_secs(900)).build();
    let url = format!("{}/v1/trunks/{trunk}/{path}", worker.trim_end_matches('/'));
    match agent.post(&url).set("Authorization", &format!("Bearer {token}")).set("Content-Type", "application/json").send_string(&body.to_string()) {
        Ok(r) => serde_json::from_str(&r.into_string().unwrap_or_default()).unwrap_or(Value::Null),
        Err(ureq::Error::Status(code, r)) => die(&format!("{path}: HTTP {code}: {}", r.into_string().unwrap_or_default())),
        Err(e) => die(&format!("{path}: {e}")),
    }
}

fn base64(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(A[(n >> 18) as usize & 63] as char);
        out.push(A[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { A[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { A[n as usize & 63] as char } else { '=' });
    }
    out
}

fn head_of(remote: &Remote) -> Oid {
    let refs = git("read main", block_on(ls_refs(&Https, remote, MAIN)));
    refs.into_iter().find(|(n, _)| n == MAIN).map(|(_, o)| o).unwrap_or_else(|| die("main is missing"))
}

/// Long-poll the receipt until the change leaves the queue.
fn receipt(api: &Api, cid: &str, rev: u32) -> Value {
    loop {
        match api.try_call("GET", &format!("changes/{cid}?rev={rev}&wait=30"), None) {
            Ok(rec) if !rec.is_null() && rec["state"] != "queued" => return rec,
            Ok(_) => {}
            Err(e) => {
                eprintln!("receipt {cid}: {e}; asking again");
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

/// What an agent knows of its fork, and its read-only view of main.
struct Seat {
    name: String,
    api: Api,
    fork_name: String,
    fork: Remote,
    fork_has: Vec<Oid>,
}

/// An agent's fork (made, or its token renewed) and its read-only view of main. The first
/// fork comes with the agent's key; a later one needs it, and each request sends it.
fn seat(sh: &Shared, name: String) -> (Seat, Remote) {
    let api = Api::with_token(&sh.trunk, sh.token.clone()).with_agent_key(crate::load_agent_key(&sh.trunk, &name));
    let f = api.call("POST", "forks", Some(json!({ "agent": name })));
    let api = Api::with_token(&sh.trunk, sh.token.clone()).with_agent_key(crate::keep_agent_key(&sh.trunk, &name, &f));
    let s = |k: &str| f[k].as_str().unwrap_or_else(|| die(&format!("fork response has no {k}"))).to_string();
    let fork = Remote::new(&s("remote"), Some(s("authorization")));
    let reader = Remote::new(&s("trunk"), f["trunk_authorization"].as_str().map(str::to_string));
    sh.reader.lock().unwrap().get_or_insert_with(|| reader.clone());
    let fork_has: Vec<Oid> = git("read fork refs", block_on(ls_refs(&Https, &fork, "refs/heads/"))).into_iter().map(|(_, o)| o).collect();
    sh.out.at(&format!("{name}  got fork {}", s("name")));
    (Seat { name, api, fork_name: s("name"), fork, fork_has }, reader)
}

fn agent(sh: &Shared, n: usize) {
    let (mut seat, reader) = seat(sh, format!("a{n:02}"));
    loop {
        let next = sh.sched.lock().unwrap().take();
        match next {
            Take::Done => return,
            Take::Wait => std::thread::sleep(Duration::from_millis(250)),
            Take::Job(job) => work(sh, &mut seat, &reader, *job),
        }
    }
}

/// Run one job to its end, or put it back to wait for the earlier job it depends on.
fn work(sh: &Shared, a: &mut Seat, reader: &Remote, job: Job) {
    let idx = job.idx;
    let mut job = job;
    let mut tries_without_blocker = 0;
    loop {
        let Some((back, why)) = attempt(sh, a, reader, job) else { break };
        job = back;
        let mut s = sh.sched.lock().unwrap();
        match s.blocker(&job) {
            Some(k) => {
                sh.waits.fetch_add(1, Ordering::SeqCst);
                sh.out.at(&format!("{}  {}  does not apply on main yet ({why}); waits for {}", a.name, job.cid, sh.cids[k]));
                s.defer(job, k);
                return;
            }
            // Nothing earlier on these paths is still open, but main may have moved since
            // the try: try once more, then give up.
            None if tries_without_blocker == 0 => {
                tries_without_blocker += 1;
                drop(s);
            }
            None => {
                drop(s);
                sh.gave_up.lock().unwrap().push(job.cid.clone());
                sh.out.at(&format!("{}  {}  {}: {why}", a.name, job.cid, sh.out.paint("31", "needs a human: it depends on a commit that did not land")));
                break;
            }
        }
    }
    sh.sched.lock().unwrap().open.remove(&idx);
}

/// Bring main into the shared store; returns its tip.
fn sync_main(sh: &Shared, reader: &Remote) -> Oid {
    let tip = head_of(reader);
    let mut store = sh.store.lock().unwrap();
    if !store.contains(&tip) {
        let mut known = sh.known.lock().unwrap();
        git("fetch main", block_on(fetch(&Https, reader, &[tip], &known, &mut store)));
        known.push(tip);
    }
    tip
}

/// Rebuild one upstream commit on main, push it, ask to land, and follow it to the end.
/// Some((job, why)): it doesn't apply on main as it is now.
fn attempt(sh: &Shared, a: &mut Seat, reader: &Remote, mut job: Job) -> Option<(Job, String)> {
    loop {
        let base = sync_main(sh, reader);
        let committer = Signature::new(&a.name, &format!("{}@agents.invalid", a.name), now_ms() / 1000);
        let built = {
            let mut store = sh.store.lock().unwrap();
            let input = ChangeInput { change_id: job.cid.clone(), rev: job.rev, commit: job.commit, base: job.parent, trailers: vec![] };
            match apply_change(&mut store, &sh.rules, base, &input, &committer) {
                Ok(applied) => {
                    let main_tree = git("read main", store.commit(&base)).tree;
                    if applied.tree == main_tree {
                        None
                    } else {
                        // It applies on main. The first revision starts where the upstream PR's
                        // branch started, if the diff applies there too.
                        let at = (job.rev == 1).then(|| fork_base(sh, &job)).flatten().filter(|o| *o != base && store.contains(o));
                        let (on, tree) = match at.map(|o| (o, apply_change(&mut store, &sh.rules, o, &input, &committer))) {
                            Some((o, Ok(there))) if there.tree != git("read the branch point", store.commit(&o)).tree => (o, there.tree),
                            _ => (base, applied.tree),
                        };
                        let c = store.put_commit(&Commit { tree, parents: vec![on], author: job.author.clone(), committer, extra: vec![], message: job.message.clone() });
                        let stops: Vec<Oid> = a.fork_has.iter().filter(|o| store.contains(o)).copied().collect();
                        let pack = write_pack(&git("pack", store.missing_from(&c, &stops).and_then(|m| store.object_list(&m))));
                        Some(Ok((c, on, pack)))
                    }
                }
                Err(Ejection::Conflict { paths, .. }) => Some(Err(paths.join(", "))),
                Err(e) => Some(Err(format!("{e:?}"))),
            }
        };
        let (commit, on, pack) = match built {
            None => {
                sh.already.fetch_add(1, Ordering::SeqCst);
                sh.out.at(&format!("{}  {}  already on main, nothing to land", a.name, job.cid));
                return None;
            }
            Some(Ok(x)) => x,
            // It doesn't apply on main yet: an earlier commit it builds on hasn't landed.
            Some(Err(why)) => return Some((job, why)),
        };
        let update = RefUpdate { name: format!("refs/heads/land/{}-{}", job.cid, job.rev), old: Oid::ZERO, new: commit };
        let report = git("push to fork", block_on(push(&Https, &a.fork, &[update], pack)));
        if !report.all_ok() {
            die(&format!("{}: push to its fork refused: {:?}", a.name, report.refs));
        }
        a.fork_has.push(commit);
        sh.first_submit.lock().unwrap().get_or_insert_with(Instant::now);
        let req = json!({ "change_id": job.cid, "rev": job.rev, "fork": a.fork_name, "commit": commit, "base": on });
        if let Err(e) = a.api.try_call("POST", "land", Some(req)) {
            sh.out.at(&format!("{}  {}  land refused: {e}", a.name, job.cid));
            sh.gave_up.lock().unwrap().push(job.cid.clone());
            return None;
        }
        let from = if on == base {
            String::new()
        } else {
            sh.branched.fetch_add(1, Ordering::SeqCst);
            format!(" (from main at {}, where its PR branched)", &on.to_hex()[..7])
        };
        sh.out.at(&format!("{}  {}  asked to land{from}: {}", a.name, job.cid, sh.out.paint("2", &job.subject)));
        let mut rec = receipt(&a.api, &job.cid, job.rev);
        if rec["state"] == "awaiting_seal" || rec["state"] == "awaiting_allowance" {
            let why: Vec<&str> = rec["seal_reasons"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            sh.out.at(&format!("{}  {}  {}: {}", a.name, job.cid, sh.out.paint("33", "parked, needs a human seal"), why.join("; ")));
            if sh.yes && rec["state"] == "awaiting_seal" && seal(&a.api, &sh.trunk, &job.cid, Some(&job.rev.to_string()), true, "30m").is_some() {
                sh.sealed.fetch_add(1, Ordering::SeqCst);
                rec = receipt(&a.api, &job.cid, job.rev);
            } else {
                sh.parked.lock().unwrap().push(job.cid.clone());
                return None;
            }
        }
        match rec["state"].as_str().unwrap_or_default() {
            "landed" => {
                sh.landed.lock().unwrap().insert(job.cid.clone());
                if let Some(o) = rec["trunk_commit"].as_str().and_then(|h| Oid::from_hex(h).ok()) {
                    sh.outs.lock().unwrap().insert(job.idx, o);
                }
                return None;
            }
            "ejected" => {
                let conflict = rec["reason"]["kind"] == "conflict";
                if conflict {
                    sh.ejected_conflict.fetch_add(1, Ordering::SeqCst);
                } else {
                    sh.ejected_other.fetch_add(1, Ordering::SeqCst);
                }
                if job.rev > sh.retries {
                    sh.gave_up.lock().unwrap().push(job.cid.clone());
                    sh.out.at(&format!("{}  {}  {}: {}", a.name, job.cid, sh.out.paint("31", "closed again, giving up"), rec["reason"]));
                    return None;
                }
                sh.out.at(&format!(
                    "{}  {}  {} {}: rebuilding on the new main as rev {}",
                    a.name,
                    job.cid,
                    sh.out.paint("31", if conflict { "closed, conflict on" } else { "closed:" }),
                    if conflict { rec["reason"]["paths"].to_string() } else { rec["reason"].to_string() },
                    job.rev + 1
                ));
                sh.rebuilt.fetch_add(1, Ordering::SeqCst);
                job.rev += 1;
            }
            other => {
                sh.out.at(&format!("{}  {}  {other}: {}", a.name, job.cid, rec["reason"]));
                sh.gave_up.lock().unwrap().push(job.cid.clone());
                return None;
            }
        }
    }
}

/// Print each batch as the sequencer records it.
fn watch(sh: &Shared, done: &AtomicBool) {
    let api = Api::with_token(&sh.trunk, sh.token.clone());
    let mut since = 0;
    loop {
        let finished = done.load(Ordering::SeqCst);
        let log: Vec<LogEntry> = api.try_call("GET", &format!("log?since={since}"), None).ok().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
        let mut batches: BTreeMap<u64, Vec<&LogEntry>> = BTreeMap::new();
        for e in &log {
            batches.entry(e.batch).or_default().push(e);
        }
        for (b, entries) in batches {
            let (first, last) = (entries[0].seq, entries[entries.len() - 1].seq);
            let seqs = if first == last { format!("#{first}") } else { format!("#{first}–{last}") };
            let what = if entries.len() == 1 { "1 PR".to_string() } else { format!("{} PRs", entries.len()) };
            sh.out.at(&sh.out.paint("32;1", &format!("▶ batch {b} merged: {what}, {seqs}, main now {}", &entries[entries.len() - 1].commit_out[..7])));
            for e in &entries {
                sh.out.more(&format!("{}  {}", e.change_id, sh.subjects.lock().unwrap().get(&e.change_id).cloned().unwrap_or_default()));
            }
        }
        if let Some(e) = log.last() {
            since = e.seq;
            sh.printed.store(since, Ordering::SeqCst);
        }
        if finished {
            return;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
}

#[cfg(test)]
mod tests {
    use super::{base64, default_name, handed_over, is_release, pr_number, trunk_for, with_approval, HONO_RULES, KEY_SLOT, NAME_SLOT};
    use nerudo_core::Rules;

    #[test]
    fn the_approval_rule_goes_next_to_governance() {
        let hono = HONO_RULES.replace(KEY_SLOT, &"ab".repeat(32)).replace(NAME_SLOT, "you");
        let rules = Rules::parse(&with_approval(&hono).expect("hono's rules have governance lines")).unwrap();
        assert_eq!(rules.seals.approval, vec!["**"]);
        assert_eq!(rules.seals.require["approval"].count, 1);
        assert!(with_approval("[footprint]\nunits = []\n").is_none());
        let over = Rules::parse(&handed_over(&with_approval(&hono).unwrap(), &["yusukebe".into()])).unwrap();
        assert_eq!(over.approvers.iter().map(|a| (a.name.as_str(), a.key.as_str())).collect::<Vec<_>>(), vec![("yusukebe", "")]);
    }

    #[test]
    fn releases() {
        assert!(is_release("chore(adapters): release @hono/bun@1.0.1"));
        assert!(is_release("release agents@0.28.0 & @cloudflare/think@0.20.2 (#2537)"));
        assert!(is_release("version packages (#2461)"));
        assert!(is_release("v4.9.10"));
        assert!(is_release("chore: update changesets for release"));
        assert!(!is_release("fix(jsx): keep the key (#5500)"));
        assert!(!is_release("docs(readme): remove the deepwiki badge (#5548)"));
    }

    #[test]
    fn names() {
        assert_eq!(default_name("https://github.com/cloudflare/agents"), "cloudflare/agents");
        assert_eq!(default_name("https://github.com/HonoJS/hono.git/"), "honojs/hono");
        assert_eq!(trunk_for("cloudflare", "agents", 1791740371), "cloudflare-agents-1791740371");
        assert_eq!(trunk_for("nerudo", "my.repo--x", 7), "nerudo-my-repo-x-7");
    }

    #[test]
    fn pr_numbers_and_base64() {
        assert_eq!(pr_number("fix(examples): stream all of it (#2519)"), Some(2519));
        assert_eq!(pr_number("Release agents@0.28.0"), None);
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
    }
}
