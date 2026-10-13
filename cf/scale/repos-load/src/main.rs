//! Open-loop load on many nerudo trunks at once, for cf/scale/repos.md.
//!
//!   repos-load setup --vars F --state F --count N [--prefix repos] [--conc 4] [--agents 2] [--digits 3]
//!       Make trunks PREFIX-000 .. PREFIX-(N-1) that the state file does not have yet:
//!       init with a seed (the Trunk object creates the Artifacts repo), then the forks of
//!       agents a0 and a1 (or a0 only with --agents 1), one trunk at a time on each of CONC
//!       threads. --digits sets the width of the number (cf/scale/10k.md uses 5).
//!   repos-load run --vars F --state F --k K --rate R --secs S --tag T --out F
//!       [--threads 256] [--poll 10] [--drain 900]
//!       Use the first K trunks. Each trunk gets R changes per second for S seconds, at
//!       fixed times that do not wait for earlier changes (open loop), sent by a0 and a1
//!       in turn. A change adds one new file, so no two changes overlap. Then stop
//!       admission and wait until every trunk's queue is empty. The output file holds
//!       every change, every stats poll and the landing log entries of the run.
//!
//! A change is built natively on the commit the fork was made from (no git subprocess, no
//! fetch per change): one push to the fork, then POST land with the agent's key. The run
//! stops admission early if the live nerudo Worker does not answer 200, if any request
//! gets HTTP 429, or if more than 5% of the changes in the last 60 s failed.
//!
//! The state file holds fork tokens and agent keys: keep it out of the repo.

use futures::executor::block_on;
use nerudo_git::{fetch, ls_refs, push, write_pack, Commit, FileEntry, Oid, RefUpdate, Remote, Signature, Store, Transport, MODE_BLOB};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const LIVE: &str = "https://nerudo.patrickdeutsch.workers.dev/";
const AGENTS: [&str; 2] = ["a0", "a1"];
const BUCKETS: u64 = 32;
const RULES: &str = "# Rules of a nerudo scale-test trunk (cf/scale/repos.md). Each change adds one new\n\
# file under changes/, so no two changes overlap.\n\
[footprint]\nunits = [\"targets/*\"]\n\n[seals]\ngovernance = [\"nerudo.toml\"]\n";

fn now_ms() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64() * 1000.0
}

fn die(msg: &str) -> ! {
    eprintln!("repos-load: {msg}");
    std::process::exit(1)
}

fn http() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(15))
            .timeout(Duration::from_secs(120))
            .max_idle_connections(4096)
            .max_idle_connections_per_host(2048)
            // workers.dev refuses some default user agents (error 1010).
            .user_agent("nerudo-scale-repos/0.1")
            .build()
    })
}

/// Hide tokens in text that goes to the output: Artifacts tokens and bearer values.
fn redact(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        let at = [rest.find("art_v"), rest.find("Bearer "), rest.find("lsa1.")].into_iter().flatten().min();
        let Some(i) = at else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let skip = if tail.starts_with("Bearer ") { 7 } else { 0 };
        let end = tail[skip..].find(|c: char| !(c.is_ascii_alphanumeric() || "_?=.-".contains(c))).map_or(tail.len(), |e| e + skip);
        out.push_str("***");
        rest = &tail[end..];
    }
}

/// git smart HTTP over ureq. Error statuses come back as (status, body).
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

/// The scale Worker's API. Ok((status, body)) for any HTTP answer, Err for no answer.
struct Api {
    worker: String,
    admin: String,
    agent: String,
}

impl Api {
    fn call(&self, method: &str, trunk: &str, path: &str, body: Option<&Value>, admin: bool, key: Option<&str>) -> Result<(u16, Value), String> {
        let url = format!("{}/v1/trunks/{trunk}/{path}", self.worker);
        let tok = if admin { &self.admin } else { &self.agent };
        let mut req = http().request(method, &url).set("Authorization", &format!("Bearer {tok}"));
        if let Some(k) = key {
            req = req.set("X-Nerudo-Agent-Key", k);
        }
        let resp = match body {
            Some(b) => req.set("Content-Type", "application/json").send_string(&b.to_string()),
            None => req.call(),
        };
        let resp = match resp {
            Ok(r) | Err(ureq::Error::Status(_, r)) => r,
            Err(e) => return Err(redact(&e.to_string())),
        };
        let status = resp.status();
        let text = resp.into_string().map_err(|e| e.to_string())?;
        let v = serde_json::from_str(&text).unwrap_or_else(|_| json!({ "raw": redact(&text[..text.len().min(400)]) }));
        Ok((status, v))
    }
}

fn flags(args: &[String]) -> BTreeMap<String, String> {
    let mut f = BTreeMap::new();
    let mut it = args.iter();
    while let Some(k) = it.next() {
        let Some(k) = k.strip_prefix("--") else { die(&format!("unexpected argument {k}")) };
        f.insert(k.to_string(), it.next().cloned().unwrap_or_default());
    }
    f
}

fn need<'a>(f: &'a BTreeMap<String, String>, k: &str) -> &'a str {
    f.get(k).map(String::as_str).unwrap_or_else(|| die(&format!("--{k} is required")))
}

fn num<T: std::str::FromStr>(f: &BTreeMap<String, String>, k: &str, default: T) -> T {
    f.get(k).and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn api_from(path: &str) -> Api {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| die(&format!("{path}: {e}")));
    let vars: BTreeMap<&str, &str> = text.lines().filter_map(|l| l.split_once('=')).collect();
    let get = |k: &str| vars.get(k).map(|v| v.trim().to_string()).unwrap_or_else(|| die(&format!("{path} has no {k}")));
    Api { worker: get("NERUDO_WORKER").trim_end_matches('/').to_string(), admin: get("NERUDO_ADMIN_TOKEN"), agent: get("NERUDO_AGENT_TOKEN") }
}

/// Write a file only its owner can read (the state file holds tokens).
fn write_private(path: &str, text: &str) {
    let tmp = format!("{path}.tmp");
    let mut fh = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp).unwrap_or_else(|e| die(&format!("{tmp}: {e}")));
    fh.write_all(text.as_bytes()).unwrap();
    drop(fh);
    std::fs::rename(&tmp, path).unwrap();
}

/// Run `f` on every item with `threads` threads; results in item order.
fn par_map<T: Sync, R: Send>(items: &[T], threads: usize, f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let out: Mutex<Vec<Option<R>>> = Mutex::new((0..items.len()).map(|_| None).collect());
    std::thread::scope(|s| {
        for _ in 0..threads.max(1).min(items.len().max(1)) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                if i >= items.len() {
                    break;
                }
                let r = f(&items[i]);
                out.lock().unwrap()[i] = Some(r);
            });
        }
    });
    out.into_inner().unwrap().into_iter().map(Option::unwrap).collect()
}

// ------------------------------------------------------------------ setup

fn seed_files() -> Value {
    let mut files = serde_json::Map::new();
    files.insert("nerudo.toml".into(), RULES.into());
    files.insert("README.md".into(), "# nerudo scale-test trunk\n\nEvery commit after the seed was landed by the nerudo sequencer.\n".into());
    files.insert("changes/.keep".into(), "".into());
    for t in 0..12 {
        let text: String = (0..12).map(|i| format!("t{t:02} line {i}\n")).collect();
        files.insert(format!("targets/t{t:02}/src.txt"), text.into());
    }
    Value::Object(files)
}

fn setup_repo(api: &Api, name: &str, agents: &[&str]) -> Value {
    let t0 = now_ms();
    let body = json!({
        "trunk": name,
        "host": { "kind": "artifacts", "binding": "ARTIFACTS", "repo": name },
        "seed": { "files": seed_files(), "message": "seed\n" },
    });
    let mut init = api.call("POST", name, "init", Some(&body), true, None);
    // No answer at all (a reset connection from the pool): the request did not reach init.
    // An init that did reach it answers 409 on the next try, and the trunk is then a failure.
    if init.is_err() {
        std::thread::sleep(Duration::from_secs(2));
        init = api.call("POST", name, "init", Some(&body), true, None);
    }
    let init_ms = now_ms() - t0;
    match &init {
        Ok((200, v)) if v.get("head").is_some() => {}
        other => return json!({ "trunk": name, "ok": false, "init_ms": init_ms, "error": format!("init: {other:?}") }),
    }
    let mut forks = vec![];
    for &agent in agents {
        let t1 = now_ms();
        let mut errors = vec![];
        let mut fork = None;
        for attempt in 0..3 {
            match api.call("POST", name, "forks", Some(&json!({ "agent": agent })), false, None) {
                Ok((200, v)) => {
                    fork = Some(v);
                    break;
                }
                Ok((s, v)) => errors.push(format!("HTTP {s}: {}", redact(&v.to_string()))),
                Err(e) => errors.push(e),
            }
            std::thread::sleep(Duration::from_secs(2 << attempt));
        }
        let fork_ms = now_ms() - t1;
        let Some(f) = fork else {
            return json!({ "trunk": name, "ok": false, "init_ms": init_ms, "error": format!("fork {agent}"), "fork_errors": errors });
        };
        let s = |k: &str| f[k].as_str().unwrap_or_default().to_string();
        // The fork's main is the commit every change of this agent is built on.
        let remote = Remote::new(&s("remote"), Some(s("authorization")));
        let base = match block_on(ls_refs(&Https, &remote, "refs/heads/main")) {
            Ok(refs) => refs.into_iter().find(|(n, _)| n == "refs/heads/main").map(|(_, o)| o.to_hex()),
            Err(e) => return json!({ "trunk": name, "ok": false, "error": format!("fork {agent} refs: {}", redact(&e.to_string())) }),
        };
        forks.push(json!({
            "agent": agent, "name": s("name"), "remote": s("remote"), "authorization": s("authorization"),
            "agent_key": s("agent_key"), "base": base, "fork_ms": fork_ms, "fork_errors": errors,
        }));
    }
    json!({ "trunk": name, "ok": true, "init_ms": init_ms, "forks": forks })
}

fn setup(f: &BTreeMap<String, String>) {
    let api = api_from(need(f, "vars"));
    let path = need(f, "state").to_string();
    let count: usize = num(f, "count", 1);
    let prefix = f.get("prefix").cloned().unwrap_or_else(|| "repos".into());
    let conc: usize = num(f, "conc", 4);
    let state: Vec<Value> = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let have: Vec<String> = state.iter().map(|r| r["trunk"].as_str().unwrap_or_default().to_string()).collect();
    let agents = &AGENTS[..num(f, "agents", AGENTS.len()).clamp(1, AGENTS.len())];
    let digits: usize = num(f, "digits", 3);
    let todo: Vec<String> = (0..count).map(|i| format!("{prefix}-{i:0digits$}")).filter(|n| !have.contains(n)).collect();
    println!("setup: {} trunks to make, {} threads", todo.len(), conc);
    let state = Mutex::new(state);
    let t0 = now_ms();
    par_map(&todo, conc, |name| {
        let r = setup_repo(&api, name, agents);
        let ok = r["ok"].as_bool().unwrap_or(false);
        let forks: Vec<String> = r["forks"].as_array().into_iter().flatten().map(|f| format!("{:.1}s", f["fork_ms"].as_f64().unwrap_or(0.0) / 1000.0)).collect();
        println!("  {name}: {} init {:.1}s forks {:?} {}", if ok { "ok" } else { "FAILED" }, r["init_ms"].as_f64().unwrap_or(0.0) / 1000.0, forks, r.get("error").map(|e| e.to_string()).unwrap_or_default());
        let mut st = state.lock().unwrap();
        st.push(r);
        st.sort_by(|a, b| a["trunk"].as_str().cmp(&b["trunk"].as_str()));
        write_private(&path, &serde_json::to_string_pretty(&*st).unwrap());
    });
    println!("setup: done in {:.0}s", (now_ms() - t0) / 1000.0);
}

// ------------------------------------------------------------------ run

/// One agent of one trunk, ready to build changes.
struct Seat {
    repo: usize,
    trunk: String,
    agent: String,
    fork_name: String,
    fork: Remote,
    key: String,
    base: Oid,
    seed: Store,
    seed_files: BTreeMap<String, FileEntry>,
}

struct Job {
    seat: usize,
    n: u64,
    due: f64,
}

fn seats_of(repos: &[Value]) -> Vec<Seat> {
    let mut want = vec![];
    for (i, r) in repos.iter().enumerate() {
        for f in r["forks"].as_array().into_iter().flatten() {
            want.push((i, r["trunk"].as_str().unwrap().to_string(), f.clone()));
        }
    }
    par_map(&want, 32, |(i, trunk, f)| {
        let s = |k: &str| f[k].as_str().unwrap_or_default().to_string();
        let fork = Remote::new(&s("remote"), Some(s("authorization")));
        let base = Oid::from_hex(&s("base")).unwrap_or_else(|_| die(&format!("{trunk}: no base")));
        let mut store = Store::new();
        if let Err(e) = block_on(fetch(&Https, &fork, &[base], &[], &mut store)) {
            die(&format!("{trunk} {}: fetch of the seed failed: {}", s("agent"), redact(&e.to_string())));
        }
        let tree = store.commit(&base).unwrap().tree;
        let seed_files = store.flatten(&tree).unwrap();
        Seat { repo: *i, trunk: trunk.clone(), agent: s("agent"), fork_name: s("name"), fork, key: s("agent_key"), base, seed: store, seed_files }
    })
}

/// Push one change to the seat's fork and ask to land it.
fn send(api: &Api, tag: &str, s: &Seat, job: &Job, t0: f64) -> Value {
    let start = now_ms();
    let cid = format!("{tag}-{}-{:05}", s.agent, job.n);
    let mut store = s.seed.clone();
    let mut files = s.seed_files.clone();
    let text = format!("{cid}\n");
    files.insert(format!("changes/{:02}/{cid}.txt", job.n % BUCKETS), FileEntry { mode: MODE_BLOB, oid: store.put_blob(text.as_bytes()) });
    let tree = store.write_tree(&files);
    let sig = Signature::new(&s.agent, &format!("{}@agents.invalid", s.agent), (start / 1000.0) as i64);
    let commit = store.put_commit(&Commit { tree, parents: vec![s.base], author: sig.clone(), committer: sig, extra: vec![], message: format!("{cid}: scale-test change\n") });
    let objs = store.missing_from(&commit, &[s.base]).unwrap();
    let pack = write_pack(&store.object_list(&objs).unwrap());
    let mut rec = json!({
        "repo": s.repo, "agent": s.agent, "n": job.n, "cid": cid,
        "due": job.due, "start": start - t0,
    });
    let update = RefUpdate { name: format!("refs/heads/land/{cid}-1"), old: Oid::ZERO, new: commit };
    let pushed = block_on(push(&Https, &s.fork, &[update], pack));
    rec["push_ms"] = (now_ms() - start).into();
    match pushed {
        Ok(r) if r.all_ok() => {}
        Ok(r) => {
            rec["error"] = "push_refused".into();
            rec["detail"] = redact(&format!("{:?}", r.refs)).into();
            return rec;
        }
        Err(e) => {
            rec["error"] = match &e {
                nerudo_git::Error::Status(code, _) => format!("push_http_{code}"),
                nerudo_git::Error::Transport(_) => "push_transport".into(),
                _ => "push_protocol".into(),
            }
            .into();
            rec["detail"] = redact(&e.to_string()).into();
            return rec;
        }
    }
    let l0 = now_ms();
    let body = json!({ "change_id": cid, "rev": 1, "fork": s.fork_name, "commit": commit.to_hex(), "base": s.base.to_hex() });
    let landed = api.call("POST", &s.trunk, "land", Some(&body), false, Some(&s.key));
    rec["land_ms"] = (now_ms() - l0).into();
    match landed {
        Ok((200, v)) => {
            rec["submitted_ms"] = v["submitted_ms"].clone();
            rec["state"] = v["state"].clone();
        }
        Ok((code, v)) => {
            rec["error"] = format!("land_http_{code}").into();
            rec["detail"] = redact(&v.to_string()).into();
        }
        Err(e) => {
            rec["error"] = "land_transport".into();
            rec["detail"] = e.into();
        }
    }
    rec
}

/// GET stats of every trunk: one compact row each.
fn poll(api: &Api, trunks: &[String]) -> Vec<Value> {
    par_map(trunks, 16, |t| match api.call("GET", t, "stats", None, false, None) {
        Ok((200, v)) => {
            let n = |k: &str| v[k].as_u64().unwrap_or(0);
            let ejected = n("ejected_conflict") + n("ejected_check") + n("ejected_other") + n("ejected_ci") + n("not_picked");
            json!({
                "submitted": n("submitted"), "landed": n("landed"), "queued": n("queued"), "batches": n("batches"),
                "ejected": ejected, "git_requests": n("git_requests"), "fetch_retries": n("fetch_retries"),
                "external_moves": n("external_moves"), "seq": n("seq"), "objects": n("objects_cached"),
                "wasm_bytes": n("wasm_memory_bytes"), "awaiting": n("awaiting_seal") + n("awaiting_allowance"),
            })
        }
        Ok((code, v)) => json!({ "error": format!("HTTP {code}"), "detail": redact(&v.to_string()) }),
        Err(e) => json!({ "error": e }),
    })
}

fn live_status() -> u16 {
    match http().get(LIVE).timeout(Duration::from_secs(20)).call() {
        Ok(r) => r.status(),
        Err(ureq::Error::Status(c, _)) => c,
        Err(_) => 0,
    }
}

fn run(f: &BTreeMap<String, String>) {
    let api = Arc::new(api_from(need(f, "vars")));
    let state: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(need(f, "state")).unwrap()).unwrap();
    let k: usize = num(f, "k", 1);
    let rate: f64 = num(f, "rate", 0.5);
    let secs: f64 = num(f, "secs", 300.0);
    let threads: usize = num(f, "threads", 256);
    let poll_s: f64 = num(f, "poll", 10.0);
    let drain_s: f64 = num(f, "drain", 900.0);
    let tag = need(f, "tag").to_string();
    let out = need(f, "out").to_string();
    let repos: Vec<Value> = state.into_iter().filter(|r| r["ok"].as_bool() == Some(true)).take(k).collect();
    if repos.len() < k {
        die(&format!("the state file has only {} ready trunks", repos.len()));
    }
    let trunks: Vec<String> = repos.iter().map(|r| r["trunk"].as_str().unwrap().to_string()).collect();
    let live0 = live_status();
    if live0 != 200 {
        die(&format!("the live Worker answers {live0}; no load"));
    }
    println!("run {tag}: K={k} rate={rate}/s per trunk for {secs}s, {threads} threads");
    let seats = Arc::new(seats_of(&repos));
    // Each trunk has the same number of agents (setup --agents).
    let per_repo = (seats.len() / k).max(1);
    println!("  {} seats ready", seats.len());
    let pre = poll(&api, &trunks);
    if let Some(bad) = pre.iter().find(|p| p.get("error").is_some()) {
        die(&format!("stats before the run: {bad}"));
    }

    // The arrival schedule: each trunk every 1/rate s, trunks spread evenly over that period.
    let per = 1000.0 / rate;
    let mut events: Vec<(f64, usize, u64)> = vec![];
    for r in 0..k {
        let phase = per * r as f64 / k as f64;
        let mut j = 0u64;
        while phase + j as f64 * per < secs * 1000.0 {
            events.push((phase + j as f64 * per, r, j));
            j += 1;
        }
    }
    events.sort_by(|a, b| a.0.total_cmp(&b.0));

    let results: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(vec![]));
    let polls: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(vec![]));
    let health: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(vec![json!({ "t": 0.0, "status": live0 })]));
    let abort: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let stop = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let backlog = Arc::new(AtomicUsize::new(0));
    let t0 = now_ms();

    // Workers.
    let (tx, rx) = mpsc::channel::<Job>();
    let rx = Arc::new(Mutex::new(rx));
    let mut workers = vec![];
    for _ in 0..threads {
        let (rx, api, seats, results, stop, tag, backlog) = (rx.clone(), api.clone(), seats.clone(), results.clone(), stop.clone(), tag.clone(), backlog.clone());
        workers.push(std::thread::spawn(move || loop {
            let job = match rx.lock().unwrap().recv() {
                Ok(j) => j,
                Err(_) => break,
            };
            backlog.fetch_sub(1, Ordering::SeqCst);
            if stop.load(Ordering::SeqCst) {
                let s = &seats[job.seat];
                results.lock().unwrap().push(json!({ "repo": s.repo, "agent": s.agent, "n": job.n, "due": job.due, "error": "not_sent_aborted" }));
                continue;
            }
            let rec = send(&api, &tag, &seats[job.seat], &job, t0);
            results.lock().unwrap().push(rec);
        }));
    }

    // Monitor: stats every poll_s, the live Worker every 60 s, the error rate and 429s.
    let monitor = {
        let (api, trunks, results, polls, health, abort, stop, done, backlog) =
            (api.clone(), trunks.clone(), results.clone(), polls.clone(), health.clone(), abort.clone(), stop.clone(), done.clone(), backlog.clone());
        std::thread::spawn(move || {
            let mut next_live = 60_000.0;
            while !done.load(Ordering::SeqCst) {
                let t = now_ms() - t0;
                let rows = poll(&api, &trunks);
                polls.lock().unwrap().push(json!({ "t": t, "backlog": backlog.load(Ordering::SeqCst), "repos": rows }));
                if t >= next_live {
                    let s = live_status();
                    health.lock().unwrap().push(json!({ "t": t, "status": s }));
                    if s != 200 {
                        abort.lock().unwrap().get_or_insert(format!("the live Worker answered {s}"));
                        stop.store(true, Ordering::SeqCst);
                    }
                    next_live += 60_000.0;
                }
                {
                    let res = results.lock().unwrap();
                    let now = now_ms() - t0;
                    let recent: Vec<&Value> = res.iter().filter(|r| r["start"].as_f64().is_some_and(|s| s > now - 60_000.0)).collect();
                    let failed = recent.iter().filter(|r| r.get("error").is_some()).count();
                    let throttled = res.iter().any(|r| r["error"].as_str().is_some_and(|e| e.ends_with("_429")));
                    if throttled {
                        abort.lock().unwrap().get_or_insert("HTTP 429".into());
                        stop.store(true, Ordering::SeqCst);
                    }
                    if recent.len() >= 20 && failed * 20 > recent.len() {
                        abort.lock().unwrap().get_or_insert(format!("{failed} of {} changes failed in the last 60 s", recent.len()));
                        stop.store(true, Ordering::SeqCst);
                    }
                }
                let spent = now_ms() - t0 - t;
                std::thread::sleep(Duration::from_millis((poll_s * 1000.0 - spent).max(500.0) as u64));
            }
        })
    };

    // Admission: open loop on the schedule above.
    let mut offered = 0usize;
    for (due, r, j) in &events {
        let wait = t0 + due - now_ms();
        if wait > 0.0 {
            std::thread::sleep(Duration::from_micros((wait * 1000.0) as u64));
        }
        if stop.load(Ordering::SeqCst) {
            break;
        }
        offered += 1;
        backlog.fetch_add(1, Ordering::SeqCst);
        tx.send(Job { seat: r * per_repo + (*j as usize % per_repo), n: *j, due: *due }).unwrap();
    }
    let admission_end = now_ms() - t0;
    drop(tx);
    for w in workers {
        w.join().unwrap();
    }
    let sent_end = now_ms() - t0;
    println!("  admission closed at {:.0}s: offered {offered}; last change sent at {:.0}s", admission_end / 1000.0, sent_end / 1000.0);

    // Drain: every trunk's queue empty, and every accepted change counted as submitted.
    let accepted: Vec<u64> = {
        let res = results.lock().unwrap();
        (0..k).map(|r| res.iter().filter(|x| x["repo"].as_u64() == Some(r as u64) && x.get("submitted_ms").is_some()).count() as u64).collect()
    };
    let mut drained_at = None;
    loop {
        let last = polls.lock().unwrap().last().cloned();
        if let Some(p) = last.filter(|p| p["t"].as_f64().unwrap_or(0.0) > sent_end) {
            let rows = p["repos"].as_array().unwrap();
            let clear = rows.iter().zip(&pre).zip(&accepted).all(|((row, pre), acc)| {
                row.get("error").is_none() && row["queued"] == 0 && row["submitted"].as_u64().unwrap_or(0) >= pre["submitted"].as_u64().unwrap_or(0) + acc
            });
            if clear {
                drained_at = p["t"].as_f64();
                break;
            }
        }
        if now_ms() - t0 - sent_end > drain_s * 1000.0 {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    done.store(true, Ordering::SeqCst);
    monitor.join().unwrap();
    let end = now_ms() - t0;
    match drained_at {
        Some(t) => println!("  drained: all queues empty at {:.0}s ({:.0}s after the last send)", t / 1000.0, (t - sent_end) / 1000.0),
        None => println!("  NOT drained after {drain_s}s"),
    }

    // The landing log of the run, and the receipt of each accepted change that did not land.
    let pre_seq: Vec<u64> = pre.iter().map(|p| p["seq"].as_u64().unwrap_or(0)).collect();
    let idx: Vec<usize> = (0..k).collect();
    let logs: Vec<Value> = par_map(&idx, 16, |&r| {
        let mut since = pre_seq[r];
        let mut out = vec![];
        loop {
            match api.call("GET", &trunks[r], &format!("log?since={since}"), None, false, None) {
                Ok((200, Value::Array(page))) => {
                    let n = page.len();
                    for e in page {
                        since = e["seq"].as_u64().unwrap_or(since);
                        out.push(json!([e["change_id"], e["seq"], e["batch"], e["landed_at_ms"]]));
                    }
                    if n < 500 {
                        break;
                    }
                }
                other => {
                    out.push(json!({ "error": format!("{other:?}") }));
                    break;
                }
            }
        }
        Value::Array(out)
    });
    let mut results = std::mem::take(&mut *results.lock().unwrap());
    let landed: std::collections::HashSet<(usize, String)> = logs
        .iter()
        .enumerate()
        .flat_map(|(r, l)| l.as_array().unwrap().iter().filter_map(move |e| e[0].as_str().map(|c| (r, c.to_string()))))
        .collect();
    let missing: Vec<usize> = results
        .iter()
        .enumerate()
        .filter(|(_, x)| x.get("submitted_ms").is_some() && !landed.contains(&(x["repo"].as_u64().unwrap() as usize, x["cid"].as_str().unwrap().to_string())))
        .map(|(i, _)| i)
        .collect();
    let receipts: Vec<Value> = par_map(&missing, 32, |&i| {
        let x = &results[i];
        match api.call("GET", &trunks[x["repo"].as_u64().unwrap() as usize], &format!("changes/{}", x["cid"].as_str().unwrap()), None, false, None) {
            Ok((200, v)) => json!({ "state": v["state"], "reason": v["reason"], "landed_ms": v["landed_ms"] }),
            other => json!({ "error": format!("{other:?}") }),
        }
    });
    for (i, r) in missing.iter().zip(receipts) {
        results[*i]["receipt"] = r;
    }
    results.sort_by(|a, b| a["due"].as_f64().unwrap_or(0.0).total_cmp(&b["due"].as_f64().unwrap_or(0.0)));

    let report = json!({
        "tag": tag, "k": k, "rate_per_repo": rate, "secs": secs, "threads": threads, "poll_s": poll_s,
        "t0_ms": t0, "admission_end": admission_end, "sent_end": sent_end, "drained_at": drained_at, "end": end,
        "offered": offered, "aborted": *abort.lock().unwrap(), "trunks": trunks,
        "pre": pre, "polls": *polls.lock().unwrap(), "health": *health.lock().unwrap(),
        "changes": results, "logs": logs,
    });
    std::fs::write(&out, serde_json::to_string(&report).unwrap()).unwrap();
    let ok = report["changes"].as_array().unwrap().iter().filter(|x| x.get("submitted_ms").is_some()).count();
    let n_landed: usize = logs.iter().map(|l| l.as_array().unwrap().len()).sum();
    println!("  offered {offered}, accepted {ok}, landed {n_landed}; aborted: {}; wrote {out}", report["aborted"]);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else { die("usage: repos-load setup|run --flags (see the top of src/main.rs)") };
    let f = flags(&args[1..]);
    match cmd.as_str() {
        "setup" => setup(&f),
        "run" => run(&f),
        _ => die(&format!("unknown command {cmd}")),
    }
}
