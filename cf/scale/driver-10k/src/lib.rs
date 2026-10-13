//! Load on 1,000 to 10,000 nerudo trunks, sent from inside Cloudflare (cf/scale/10k.md).
//!
//! One computer cannot send this load: the edge blocks one IP near 2,000 requests/s. This
//! Worker has one Durable Object class, `Shard`. Each shard object sends the changes of a few
//! trunks (its seats) at fixed times that do not wait for earlier changes (open loop). A
//! change is one commit pushed to the agent's fork (2 git requests to Artifacts), then
//! `POST land` to the Worker under test through the service binding TARGET. Each seat sends
//! its changes one after the other, as one agent does. After the last change, the shard waits
//! until the queues of its trunks are empty, reads their landing logs, and keeps a summary:
//! histograms, and one row for each trunk.
//!
//!   POST /fan/{start|stop|status|report|kick}  {"shards": {"<name>": <body>, ...}}
//!       Send <body> to each named shard object, and answer {"<name>": {status, body}}.
//!       Each request needs `Authorization: Bearer <DRIVER_KEY>`.
//!
//! A shard sends only from t0 to t0 + secs of its plan. It stops by itself at the first
//! HTTP 429, and when more than 5% of its changes in the last 60 s failed.
//!
//! Bindings: SHARD (this class), TARGET (service: the Worker under test). Secrets:
//! DRIVER_KEY, AGENT (the agent token of the Worker under test).

use futures::future::join_all;
use futures::stream::{self, StreamExt};
use nerudo_git::{fetch, push, write_pack, Commit, FileEntry, Oid, RefUpdate, Remote, Signature, Store, Transport, MODE_BLOB};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;
use wasm_bindgen::JsValue;
use worker::*;

const OPS: [&str; 5] = ["start", "stop", "status", "report", "kick"];
/// A change adds changes/NN/<id>.txt, NN = n mod BUCKETS.
const BUCKETS: u32 = 32;
/// Records in one storage value.
const CHUNK: usize = 1000;
/// Requests at once from one shard for its reads (stats, logs).
const READS: usize = 6;

fn now() -> f64 {
    js_sys::Date::now()
}

fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

fn key_ok(env: &Env, req: &Request) -> bool {
    let Ok(want) = env.secret("DRIVER_KEY").map(|s| s.to_string()) else {
        return false;
    };
    let got = req.headers().get("Authorization").ok().flatten().unwrap_or_default();
    let got = got.strip_prefix("Bearer ").unwrap_or_default();
    !want.is_empty() && same(got.as_bytes(), want.as_bytes())
}

/// Hide tokens in text that goes to the output: Artifacts tokens and bearer values.
fn redact(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        let at = [rest.find("art_v"), rest.find("Bearer "), rest.find("lsa1.")].into_iter().flatten().min();
        let Some(i) = at else {
            out.push_str(rest);
            return out.chars().take(400).collect();
        };
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let skip = if tail.starts_with("Bearer ") { 7 } else { 0 };
        let end = tail[skip..].find(|c: char| !(c.is_ascii_alphanumeric() || "_?=.-".contains(c))).map_or(tail.len(), |e| e + skip);
        out.push_str("***");
        rest = &tail[end..];
    }
}

#[event(fetch)]
async fn main(mut req: Request, env: Env, _ctx: Context) -> Result<Response> {
    if !key_ok(&env, &req) {
        return Response::error("forbidden", 403);
    }
    let path = req.path();
    let op = path.trim_matches('/').strip_prefix("fan/").unwrap_or_default().to_string();
    if req.method() != Method::Post || !OPS.contains(&op.as_str()) {
        return Response::error("POST /fan/{start|stop|status|report|kick}", 404);
    }
    let body: Value = req.json().await?;
    let ns = env.durable_object("SHARD")?;
    let (ns, op) = (&ns, op.as_str());
    let empty = serde_json::Map::new();
    let shards = body["shards"].as_object().unwrap_or(&empty);
    let calls = shards.iter().map(|(name, b)| async move {
        let res: Result<Value> = async {
            let stub = ns.id_from_name(name)?.get_stub()?;
            let mut init = RequestInit::new();
            init.with_method(Method::Post).with_body(Some(JsValue::from_str(&b.to_string())));
            let mut resp = stub.fetch_with_request(Request::new_with_init(&format!("https://shard/{op}"), &init)?).await?;
            let status = resp.status_code();
            let text = resp.text().await?;
            Ok(json!({ "status": status, "body": serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text)) }))
        }
        .await;
        (name.clone(), res.unwrap_or_else(|e| json!({ "status": 0, "error": redact(&e.to_string()) })))
    });
    let out: Vec<(String, Value)> = stream::iter(calls).buffer_unordered(32).collect().await;
    Response::from_json(&Value::Object(out.into_iter().collect()))
}

/// git smart HTTP over the Workers `fetch` API.
struct FetchTransport;

impl Transport for FetchTransport {
    async fn request(&self, method: &str, url: &str, headers: &[(&str, String)], body: Option<Vec<u8>>) -> nerudo_git::Result<(u16, Vec<u8>)> {
        let err = |e: worker::Error| nerudo_git::Error::Transport(e.to_string());
        let h = Headers::new();
        for (k, v) in headers {
            h.set(k, v).map_err(err)?;
        }
        let mut init = RequestInit::new();
        init.with_method(Method::from(method.to_string())).with_headers(h);
        if let Some(b) = body {
            init.with_body(Some(js_sys::Uint8Array::from(b.as_slice()).into()));
        }
        let req = Request::new_with_init(url, &init).map_err(err)?;
        let mut resp = Fetch::Request(req).send().await.map_err(err)?;
        let status = resp.status_code();
        let body = resp.bytes().await.map_err(err)?;
        Ok((status, body))
    }
}

/// One agent of one trunk, as the controller sends it. `g` is the trunk's number in the run.
#[derive(Clone, Serialize, Deserialize)]
struct SeatCfg {
    g: u32,
    trunk: String,
    fork: String,
    remote: String,
    authorization: String,
    key: String,
    base: String,
}

fn d_drain() -> f64 {
    300.0
}

fn d_poll() -> f64 {
    10.0
}

fn d_prep() -> f64 {
    15.0
}

#[derive(Clone, Serialize, Deserialize)]
struct Plan {
    tag: String,
    /// Admission is from t0_ms (epoch ms) to t0_ms + secs.
    t0_ms: f64,
    /// Changes each second, for each trunk.
    rate: f64,
    secs: f64,
    /// Trunks in the whole run. Trunk g starts at g/k of one period, so the starts of all
    /// trunks are spread evenly.
    k: u32,
    seats: Vec<SeatCfg>,
    #[serde(default = "d_drain")]
    drain_s: f64,
    #[serde(default = "d_poll")]
    poll_s: f64,
    /// The first shard wakes this long before t0 to fetch its seeds and read stats; the
    /// others wake later, spread up to 15 s before t0, so the seed fetches do not all come
    /// at once.
    #[serde(default = "d_prep")]
    prep_s: f64,
}

/// One change: seat, n, due and start (ms after t0), push ms, land ms, submitted_ms (the
/// server's time; 0 = not accepted), error kind ("" = none).
#[derive(Clone, Serialize, Deserialize)]
struct Rec(u16, u32, i64, i64, i32, i32, i64, String);

/// Kinds that are not a failed request: the shard did not send the change.
fn unsent(kind: &str) -> bool {
    kind == "not_sent" || kind == "lost"
}

struct Seat {
    remote: Remote,
    base: Oid,
    agent: String,
    seed: Store,
    files: BTreeMap<String, FileEntry>,
}

#[derive(Default)]
struct Run {
    tag: String,
    recs: Vec<Rec>,
    samples: BTreeMap<String, Vec<String>>,
    polls: Vec<Value>,
}

impl Run {
    fn sample(&mut self, kind: &str, detail: String) {
        let s = self.samples.entry(kind.to_string()).or_default();
        if s.len() < 3 {
            s.push(redact(&detail));
        }
    }
}

#[durable_object]
pub struct Shard {
    state: State,
    env: Env,
    run: RefCell<Option<Run>>,
    stop: RefCell<Option<String>>,
    /// An alarm of this object is running now.
    busy: Cell<bool>,
}

impl DurableObject for Shard {
    fn new(state: State, env: Env) -> Self {
        Self { state, env, run: RefCell::new(None), stop: RefCell::new(None), busy: Cell::new(false) }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        let path = req.path();
        let body: Value = req.json().await.unwrap_or(Value::Null);
        let tag = body["tag"].as_str().unwrap_or_default().to_string();
        match path.trim_matches('/') {
            "start" => self.start(body).await,
            "stop" => {
                let reason = body["reason"].as_str().unwrap_or("stopped by the controller").to_string();
                self.stop.borrow_mut().get_or_insert(reason.clone());
                self.state.storage().put(&format!("stop:{tag}"), &reason).await?;
                Response::from_json(&json!({ "stopped": reason }))
            }
            "status" => self.status(&tag).await,
            "report" => match self.state.storage().get::<Value>(&format!("report:{tag}")).await? {
                Some(r) => Response::from_json(&r),
                None => Response::from_json(&json!({ "phase": self.phase(&tag).await?, "report": null })),
            },
            // A run whose alarm was lost: stop sending and write the report.
            "kick" => {
                let phase = self.phase(&tag).await?;
                if (phase == "running" || phase == "draining") && !self.busy.get() {
                    self.state.storage().put(&format!("phase:{tag}"), "draining").await?;
                    self.state.storage().set_alarm(10i64).await?;
                }
                Response::from_json(&json!({ "phase": phase }))
            }
            _ => Response::error("no route", 404),
        }
    }

    async fn alarm(&self) -> Result<Response> {
        let storage = self.state.storage();
        let Some(tag) = storage.get::<String>("cur").await? else {
            return Response::ok("idle");
        };
        let Some(plan) = storage.get::<Plan>(&format!("plan:{tag}")).await? else {
            return Response::ok("no plan");
        };
        if self.busy.replace(true) {
            return Response::ok("busy");
        }
        let res = async {
            match self.phase(&tag).await?.as_str() {
                "planned" => self.admit(&plan).await,
                // The admission alarm of this run stopped before its end. Send nothing more.
                "running" => {
                    storage.put(&format!("phase:{tag}"), "draining").await?;
                    storage.put(&format!("restarted:{tag}"), &now()).await?;
                    self.drain(&plan).await
                }
                "draining" => self.drain(&plan).await,
                _ => Ok(()),
            }
        }
        .await;
        self.busy.set(false);
        // Never fail the alarm: the runtime would run it again, and send changes twice.
        if let Err(e) = res {
            console_error!("shard {tag}: {e}");
            storage.put(&format!("fail:{tag}"), &redact(&e.to_string())).await?;
            storage.put(&format!("phase:{tag}"), "failed").await?;
        }
        Response::ok("ok")
    }
}

impl Shard {
    async fn phase(&self, tag: &str) -> Result<String> {
        Ok(self.state.storage().get::<String>(&format!("phase:{tag}")).await?.unwrap_or_else(|| "none".into()))
    }

    async fn start(&self, body: Value) -> Result<Response> {
        let plan: Plan = serde_json::from_value(body)?;
        let storage = self.state.storage();
        if let Some(cur) = storage.get::<String>("cur").await? {
            let phase = self.phase(&cur).await?;
            if cur == plan.tag {
                return Response::from_json(&json!({ "phase": phase, "again": true }));
            }
            if ["planned", "running", "draining"].contains(&phase.as_str()) {
                return Response::error(format!("run {cur} is {phase}"), 409);
            }
        }
        if plan.seats.is_empty() || plan.rate <= 0.0 || plan.secs <= 0.0 || plan.k == 0 {
            return Response::error("a plan needs seats, rate, secs and k", 400);
        }
        storage.put(&format!("plan:{}", plan.tag), &plan).await?;
        storage.put(&format!("phase:{}", plan.tag), "planned").await?;
        storage.put("cur", &plan.tag).await?;
        *self.stop.borrow_mut() = None;
        // Wake before t0: fetch the seeds and read the stats before the first change.
        let spread = (plan.prep_s - 15.0).max(0.0) * (plan.seats[0].g % plan.k) as f64 / plan.k as f64;
        storage.set_alarm((plan.t0_ms - (plan.prep_s - spread) * 1000.0 - now()).max(10.0) as i64).await?;
        Response::from_json(&json!({ "phase": "planned", "seats": plan.seats.len() }))
    }

    async fn status(&self, tag: &str) -> Result<Response> {
        let phase = self.phase(tag).await?;
        let stop = self.stop.borrow().clone();
        if let Some(run) = self.run.borrow().as_ref().filter(|r| r.tag == tag) {
            let mut c = counters(&run.recs, now());
            c["phase"] = phase.into();
            c["stop"] = json!(stop);
            c["queued"] = run.polls.last().map(|p| p["q"].clone()).unwrap_or(Value::Null);
            return Response::from_json(&c);
        }
        let saved = self.state.storage().get::<Value>(&format!("prog:{tag}")).await?.unwrap_or(json!({}));
        let mut c = saved;
        c["phase"] = phase.into();
        c["stop"] = json!(stop);
        Response::from_json(&c)
    }

    /// GET a path of the Worker under test, with the agent token, through the binding.
    async fn target(&self, method: &str, path: &str, body: Option<&Value>, key: Option<&str>) -> Result<(u16, Value)> {
        let h = Headers::new();
        h.set("Authorization", &format!("Bearer {}", self.env.secret("AGENT")?))?;
        if let Some(k) = key {
            h.set("X-Nerudo-Agent-Key", k)?;
        }
        let mut init = RequestInit::new();
        if let Some(b) = body {
            h.set("Content-Type", "application/json")?;
            init.with_body(Some(JsValue::from_str(&b.to_string())));
        }
        init.with_method(Method::from(method.to_string())).with_headers(h);
        let mut resp = self.env.service("TARGET")?.fetch(format!("https://target/v1/trunks/{path}"), Some(init)).await?;
        let status = resp.status_code();
        let text = resp.text().await?;
        let v = serde_json::from_str(&text).unwrap_or_else(|_| json!({ "raw": redact(&text) }));
        Ok((status, v))
    }

    /// GET stats of each trunk of the plan: one compact row each.
    async fn poll_stats(&self, plan: &Plan) -> Vec<Value> {
        stream::iter(plan.seats.iter().map(|s| async move {
            match self.target("GET", &format!("{}/stats", s.trunk), None, None).await {
                Ok((200, v)) => {
                    let n = |k: &str| v[k].as_u64().unwrap_or(0);
                    let ejected = n("ejected_conflict") + n("ejected_check") + n("ejected_other") + n("ejected_ci") + n("not_picked");
                    json!({
                        "submitted": n("submitted"), "landed": n("landed"), "queued": n("queued"), "batches": n("batches"),
                        "ejected": ejected, "git_requests": n("git_requests"), "fetch_retries": n("fetch_retries"),
                        "external_moves": n("external_moves"), "seq": n("seq"), "wasm_bytes": n("wasm_memory_bytes"),
                    })
                }
                Ok((code, v)) => json!({ "error": format!("HTTP {code}"), "detail": redact(&v.to_string()) }),
                Err(e) => json!({ "error": redact(&e.to_string()) }),
            }
        }))
        .buffered(READS)
        .collect()
        .await
    }

    async fn checkpoint(&self, tag: &str) {
        let (chunks, samples, polls, prog) = {
            let run = self.run.borrow();
            let Some(run) = run.as_ref() else { return };
            let chunks: Vec<String> = run.recs.chunks(CHUNK).map(|c| serde_json::to_string(c).unwrap_or_default()).collect();
            (chunks, json!(run.samples), json!(run.polls), counters(&run.recs, now()))
        };
        let storage = self.state.storage();
        let mut res = Ok(());
        for (i, c) in chunks.iter().enumerate() {
            res = res.and(storage.put(&format!("recs:{tag}:{i}"), c).await);
        }
        res = res.and(storage.put(&format!("nrecs:{tag}"), chunks.len()).await);
        res = res.and(storage.put(&format!("samples:{tag}"), &samples).await);
        res = res.and(storage.put(&format!("polls:{tag}"), &polls).await);
        res = res.and(storage.put(&format!("prog:{tag}"), &prog).await);
        if let Err(e) = res {
            console_error!("checkpoint {tag}: {e}");
        }
    }

    async fn seed(&self, s: &SeatCfg) -> std::result::Result<Seat, String> {
        let remote = Remote::new(&s.remote, Some(s.authorization.clone()));
        let base = Oid::from_hex(&s.base).map_err(|e| format!("base: {e}"))?;
        let mut seed = Store::new();
        // Three tries, 1 s and 2 s apart: Artifacts sometimes answers 503.
        let mut got = Err(String::new());
        for wait in [0u64, 1, 2] {
            Delay::from(Duration::from_secs(wait)).await;
            got = fetch(&FetchTransport, &remote, &[base], &[], &mut seed).await.map_err(|e| format!("seed fetch: {e}"));
            if got.is_ok() {
                break;
            }
        }
        got?;
        let tree = seed.commit(&base).map_err(|e| e.to_string())?.tree;
        let files = seed.flatten(&tree).map_err(|e| e.to_string())?;
        let agent = s.fork.rsplit("--").next().unwrap_or("a0").to_string();
        Ok(Seat { remote, base, agent, seed, files })
    }

    /// Send all changes of the plan, at their times.
    async fn admit(&self, plan: &Plan) -> Result<()> {
        let tag = &plan.tag;
        let storage = self.state.storage();
        storage.put(&format!("phase:{tag}"), "running").await?;
        *self.run.borrow_mut() = Some(Run { tag: tag.clone(), ..Default::default() });
        if let Some(r) = storage.get::<String>(&format!("stop:{tag}")).await? {
            self.stop.borrow_mut().get_or_insert(r);
        }
        let seats: Vec<std::result::Result<Seat, String>> = stream::iter(plan.seats.iter().map(|s| self.seed(s))).buffered(READS).collect().await;
        let pre = self.poll_stats(plan).await;
        storage.put(&format!("pre:{tag}"), &pre).await?;
        let period = 1000.0 / plan.rate;
        let end = plan.secs * 1000.0;
        let finished = Cell::new(0usize);
        let loops = seats.iter().enumerate().map(|(i, seat)| {
            let finished = &finished;
            async move {
                let cfg = &plan.seats[i];
                let phase = period * (cfg.g % plan.k) as f64 / plan.k as f64;
                let mut n = 0u32;
                loop {
                    let due = phase + n as f64 * period;
                    if due >= end {
                        break;
                    }
                    let wait = plan.t0_ms + due - now();
                    if wait > 0.0 {
                        Delay::from(Duration::from_millis(wait as u64)).await;
                    }
                    let stopped = self.stop.borrow().is_some();
                    let (rec, detail) = match seat {
                        _ if stopped => (Rec(i as u16, n, due as i64, 0, 0, 0, 0, "not_sent".into()), None),
                        Err(e) => (Rec(i as u16, n, due as i64, due as i64, 0, 0, 0, "no_seed".into()), Some(e.clone())),
                        Ok(s) => self.send(plan, s, cfg, i, n, due).await,
                    };
                    if rec.7.ends_with("_429") {
                        self.stop.borrow_mut().get_or_insert(format!("HTTP 429 ({})", rec.7));
                    }
                    if let Some(run) = self.run.borrow_mut().as_mut() {
                        if let Some(d) = detail {
                            run.sample(&rec.7, d);
                        }
                        run.recs.push(rec);
                    }
                    n += 1;
                }
                finished.set(finished.get() + 1);
            }
        });
        let monitor = async {
            let mut next = plan.t0_ms + plan.poll_s * 1000.0;
            while finished.get() < seats.len() {
                if now() >= next {
                    next += plan.poll_s * 1000.0;
                    let rows = self.poll_stats(plan).await;
                    let q: u64 = rows.iter().map(|r| r["queued"].as_u64().unwrap_or(0)).sum();
                    let qmax = rows.iter().map(|r| r["queued"].as_u64().unwrap_or(0)).max().unwrap_or(0);
                    let errs = rows.iter().filter(|r| r.get("error").is_some()).count();
                    let c = {
                        let mut run = self.run.borrow_mut();
                        let run = run.as_mut().expect("set above");
                        run.polls.push(json!({ "t": (now() - plan.t0_ms) as i64, "q": q, "qmax": qmax, "stats_errors": errs }));
                        counters(&run.recs, now())
                    };
                    let (recent, bad) = (c["recent"].as_u64().unwrap_or(0), c["recent_err"].as_u64().unwrap_or(0));
                    if recent >= 20 && bad * 20 > recent {
                        self.stop.borrow_mut().get_or_insert(format!("{bad} of {recent} changes failed in the last 60 s"));
                    }
                    self.checkpoint(tag).await;
                }
                Delay::from(Duration::from_millis(250)).await;
            }
        };
        futures::join!(join_all(loops), monitor);
        let stop = self.stop.borrow().clone();
        if let Some(r) = stop {
            storage.put(&format!("stop:{tag}"), &r).await?;
        }
        storage.put(&format!("sent_end:{tag}"), &now()).await?;
        self.checkpoint(tag).await;
        storage.put(&format!("phase:{tag}"), "draining").await?;
        storage.set_alarm(10i64).await?;
        Ok(())
    }

    /// Push one change to the seat's fork and ask to land it.
    async fn send(&self, plan: &Plan, s: &Seat, cfg: &SeatCfg, i: usize, n: u32, due: f64) -> (Rec, Option<String>) {
        let start = now();
        let mut rec = Rec(i as u16, n, due as i64, (start - plan.t0_ms) as i64, 0, 0, 0, String::new());
        let cid = format!("{}-{:05}-{n:05}", plan.tag, cfg.g);
        let mut store = s.seed.clone();
        let mut files = s.files.clone();
        let blob = store.put_blob(format!("{cid}\n").as_bytes());
        files.insert(format!("changes/{:02}/{cid}.txt", n % BUCKETS), FileEntry { mode: MODE_BLOB, oid: blob });
        let tree = store.write_tree(&files);
        let sig = Signature::new(&s.agent, &format!("{}@agents.invalid", s.agent), (start / 1000.0) as i64);
        let commit = store.put_commit(&Commit { tree, parents: vec![s.base], author: sig.clone(), committer: sig, extra: vec![], message: format!("{cid}: scale-test change\n") });
        let pack = match store.missing_from(&commit, &[s.base]).and_then(|o| store.object_list(&o).map(|l| write_pack(&l))) {
            Ok(p) => p,
            Err(e) => {
                rec.7 = "build".into();
                return (rec, Some(e.to_string()));
            }
        };
        let update = RefUpdate { name: format!("refs/heads/land/{cid}-1"), old: Oid::ZERO, new: commit };
        let pushed = push(&FetchTransport, &s.remote, &[update], pack).await;
        rec.4 = (now() - start) as i32;
        let failed = match pushed {
            Ok(r) if r.all_ok() => None,
            Ok(r) => {
                let d = format!("{:?}", r.refs);
                Some((if d.contains("busy") { "push_busy".to_string() } else { "push_refused".to_string() }, d))
            }
            Err(e) => {
                let d = e.to_string();
                let kind = match &e {
                    _ if d.contains("busy") => "push_busy".to_string(),
                    nerudo_git::Error::Status(code, _) => format!("push_http_{code}"),
                    nerudo_git::Error::Transport(_) => "push_transport".into(),
                    _ => "push_protocol".into(),
                };
                Some((kind, d))
            }
        };
        if let Some((kind, d)) = failed {
            rec.7 = kind;
            return (rec, Some(d));
        }
        let l0 = now();
        let body = json!({ "change_id": cid, "rev": 1, "fork": cfg.fork, "commit": commit.to_hex(), "base": s.base.to_hex() });
        let landed = self.target("POST", &format!("{}/land", cfg.trunk), Some(&body), Some(&cfg.key)).await;
        rec.5 = (now() - l0) as i32;
        match landed {
            Ok((200, v)) => {
                rec.6 = v["submitted_ms"].as_i64().unwrap_or(0);
                if rec.6 == 0 {
                    rec.7 = "land_no_receipt".into();
                    return (rec, Some(v.to_string()));
                }
                (rec, None)
            }
            Ok((code, v)) => {
                let d = v.to_string();
                let low = d.to_lowercase();
                rec.7 = match code {
                    429 => "land_http_429".into(),
                    _ if low.contains("overloaded") => "land_do_overloaded".into(),
                    _ if low.contains("memory") => "land_memory".into(),
                    _ => format!("land_http_{code}"),
                };
                (rec, Some(d))
            }
            Err(e) => {
                rec.7 = "land_transport".into();
                (rec, Some(e.to_string()))
            }
        }
    }

    /// Wait until the queues are empty, read the landing logs, and write the report.
    async fn drain(&self, plan: &Plan) -> Result<()> {
        let tag = &plan.tag;
        let storage = self.state.storage();
        let (mut recs, samples, polls) = match self.run.borrow_mut().take() {
            Some(r) if r.tag == *tag => (r.recs, json!(r.samples), json!(r.polls)),
            _ => (vec![], Value::Null, Value::Null),
        };
        let (samples, polls) = if recs.is_empty() {
            let chunks = storage.get::<usize>(&format!("nrecs:{tag}")).await?.unwrap_or(0);
            for c in 0..chunks {
                if let Some(t) = storage.get::<String>(&format!("recs:{tag}:{c}")).await? {
                    recs.extend(serde_json::from_str::<Vec<Rec>>(&t).unwrap_or_default());
                }
            }
            (storage.get::<Value>(&format!("samples:{tag}")).await?.unwrap_or(json!({})), storage.get::<Value>(&format!("polls:{tag}")).await?.unwrap_or(json!([])))
        } else {
            (samples, polls)
        };
        // Changes of the schedule that have no record: the admission stopped before them.
        let period = 1000.0 / plan.rate;
        let end = plan.secs * 1000.0;
        let have: std::collections::HashSet<(u16, u32)> = recs.iter().map(|r| (r.0, r.1)).collect();
        for (i, cfg) in plan.seats.iter().enumerate() {
            let phase = period * (cfg.g % plan.k) as f64 / plan.k as f64;
            let mut n = 0u32;
            while phase + n as f64 * period < end {
                if !have.contains(&(i as u16, n)) {
                    recs.push(Rec(i as u16, n, (phase + n as f64 * period) as i64, 0, 0, 0, 0, "lost".into()));
                }
                n += 1;
            }
        }
        let pre = match storage.get::<Vec<Value>>(&format!("pre:{tag}")).await? {
            Some(p) => p,
            None => self.poll_stats(plan).await,
        };
        let mut accepted = vec![0u64; plan.seats.len()];
        for r in recs.iter().filter(|r| r.6 > 0) {
            accepted[r.0 as usize] += 1;
        }
        let sent_end = storage.get::<f64>(&format!("sent_end:{tag}")).await?.unwrap_or_else(now);
        let mut drained = None;
        let mut post;
        loop {
            post = self.poll_stats(plan).await;
            let clear = post.iter().zip(&pre).zip(&accepted).all(|((row, pre), acc)| {
                row.get("error").is_none() && row["queued"] == 0 && row["submitted"].as_u64().unwrap_or(0) >= pre["submitted"].as_u64().unwrap_or(0) + acc
            });
            if clear {
                drained = Some(now());
                break;
            }
            if now() - sent_end > plan.drain_s * 1000.0 {
                break;
            }
            Delay::from(Duration::from_secs(2)).await;
        }
        // The landing log of the run: (change id) -> (batch, landed_at_ms).
        let logs: Vec<std::result::Result<Vec<(String, u64, i64)>, String>> = stream::iter(plan.seats.iter().zip(&pre).map(|(s, p)| {
            let mut since = p["seq"].as_u64().unwrap_or(0);
            async move {
                let mut out = vec![];
                loop {
                    match self.target("GET", &format!("{}/log?since={since}", s.trunk), None, None).await {
                        Ok((200, Value::Array(page))) => {
                            let n = page.len();
                            for e in page {
                                since = e["seq"].as_u64().unwrap_or(since);
                                out.push((e["change_id"].as_str().unwrap_or_default().to_string(), e["batch"].as_u64().unwrap_or(0), e["landed_at_ms"].as_i64().unwrap_or(0)));
                            }
                            if n < 500 {
                                return Ok(out);
                            }
                        }
                        Ok((code, v)) => return Err(format!("log: HTTP {code}: {}", redact(&v.to_string()))),
                        Err(e) => return Err(format!("log: {}", redact(&e.to_string()))),
                    }
                }
            }
        }))
        .buffered(READS)
        .collect()
        .await;
        let report = summarize(plan, &recs, &pre, &post, &logs, drained, sent_end, samples, polls, storage.get::<String>(&format!("stop:{tag}")).await?);
        let missing: Vec<(usize, String)> = report["missing"].as_array().into_iter().flatten().filter_map(|m| Some((m["seat"].as_u64()? as usize, m["cid"].as_str()?.to_string()))).take(20).collect();
        let receipts: Vec<Value> = stream::iter(missing.iter().map(|(i, cid)| async move {
            match self.target("GET", &format!("{}/changes/{cid}", plan.seats[*i].trunk), None, None).await {
                Ok((200, v)) => json!({ "cid": cid, "state": v["state"], "reason": v["reason"], "landed_ms": v["landed_ms"] }),
                Ok((code, v)) => json!({ "cid": cid, "error": format!("HTTP {code}"), "detail": redact(&v.to_string()) }),
                Err(e) => json!({ "cid": cid, "error": redact(&e.to_string()) }),
            }
        }))
        .buffered(READS)
        .collect()
        .await;
        let mut report = report;
        report["receipts"] = receipts.into();
        if let Some(t) = storage.get::<f64>(&format!("restarted:{tag}")).await? {
            report["restarted_at"] = (t - plan.t0_ms).into();
        }
        storage.put(&format!("report:{tag}"), &report).await?;
        storage.put(&format!("phase:{tag}"), "done").await?;
        Ok(())
    }
}

/// Counts for the status of a run.
fn counters(recs: &[Rec], now_ms: f64) -> Value {
    let mut errors: BTreeMap<String, u64> = BTreeMap::new();
    let (mut sent, mut accepted, mut unsent_n, mut lag_max, mut recent, mut recent_err) = (0u64, 0u64, 0u64, 0i64, 0u64, 0u64);
    let mut last_start = 0i64;
    for r in recs {
        if unsent(&r.7) {
            unsent_n += 1;
            continue;
        }
        sent += 1;
        if r.6 > 0 {
            accepted += 1;
        }
        if !r.7.is_empty() {
            *errors.entry(r.7.clone()).or_default() += 1;
        }
        lag_max = lag_max.max(r.3 - r.2);
        last_start = last_start.max(r.3);
    }
    // "Recent" is the last 60 s before the newest send (the shard's clock is the t0 offset).
    // A seat with no seed sends nothing, so it does not count here.
    for r in recs.iter().filter(|r| !unsent(&r.7) && r.7 != "no_seed" && r.3 > last_start - 60_000) {
        recent += 1;
        if !r.7.is_empty() {
            recent_err += 1;
        }
    }
    let _ = now_ms;
    json!({ "sent": sent, "accepted": accepted, "not_sent": unsent_n, "errors": errors, "lag_max": lag_max, "recent": recent, "recent_err": recent_err })
}

fn bump(h: &mut BTreeMap<i64, u64>, v: i64, width: i64) {
    *h.entry(v.max(0) / width * width).or_default() += 1;
}

fn pct(sorted: &[i64], p: f64) -> Value {
    if sorted.is_empty() {
        return Value::Null;
    }
    let i = ((p / 100.0) * (sorted.len() - 1) as f64).round() as usize;
    sorted[i.min(sorted.len() - 1)].into()
}

/// The report of one shard for one run: histograms (ms, 5 ms buckets unless stated) and one
/// row for each trunk. The controller adds the histograms of all shards.
#[allow(clippy::too_many_arguments)]
fn summarize(
    plan: &Plan,
    recs: &[Rec],
    pre: &[Value],
    post: &[Value],
    logs: &[std::result::Result<Vec<(String, u64, i64)>, String>],
    drained: Option<f64>,
    sent_end: f64,
    samples: Value,
    polls: Value,
    stop: Option<String>,
) -> Value {
    let t0 = plan.t0_ms as i64;
    let w0 = t0 + 30_000;
    let w1 = t0 + (plan.secs * 1000.0) as i64;
    let mut lat: BTreeMap<i64, u64> = BTreeMap::new();
    let mut lat_min: BTreeMap<i64, BTreeMap<i64, u64>> = BTreeMap::new();
    let (mut push_h, mut land_h, mut lag_h): (BTreeMap<i64, u64>, BTreeMap<i64, u64>, BTreeMap<i64, u64>) = Default::default();
    let mut landed_s: BTreeMap<i64, u64> = BTreeMap::new();
    let mut batch_sizes: BTreeMap<i64, u64> = BTreeMap::new();
    let mut errors: BTreeMap<String, u64> = BTreeMap::new();
    let (mut first_sub, mut last_sub) = (i64::MAX, 0i64);
    let (mut offered, mut sent, mut accepted, mut landed, mut unsent_n) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut missing = vec![];
    let mut trunks = vec![];
    for (i, cfg) in plan.seats.iter().enumerate() {
        let log: HashMap<&str, (u64, i64)> = match &logs[i] {
            Ok(l) => l.iter().map(|(c, b, t)| (c.as_str(), (*b, *t))).collect(),
            Err(_) => HashMap::new(),
        };
        let mut batches: HashMap<u64, u64> = HashMap::new();
        for (b, _) in log.values() {
            *batches.entry(*b).or_default() += 1;
        }
        for (_, size) in batches {
            *batch_sizes.entry(size as i64).or_default() += 1;
        }
        let mut lats = vec![];
        let (mut t_off, mut t_acc, mut t_land, mut t_win, mut t_err) = (0u64, 0u64, 0u64, 0u64, 0u64);
        for r in recs.iter().filter(|r| r.0 as usize == i) {
            offered += 1;
            t_off += 1;
            if unsent(&r.7) {
                unsent_n += 1;
                *errors.entry(r.7.clone()).or_default() += 1;
                continue;
            }
            sent += 1;
            bump(&mut lag_h, r.3 - r.2, 5);
            if r.4 > 0 {
                bump(&mut push_h, r.4 as i64, 5);
            }
            if r.5 > 0 {
                bump(&mut land_h, r.5 as i64, 5);
            }
            if !r.7.is_empty() {
                t_err += 1;
                *errors.entry(r.7.clone()).or_default() += 1;
            }
            if r.6 == 0 {
                continue;
            }
            accepted += 1;
            t_acc += 1;
            first_sub = first_sub.min(r.6);
            last_sub = last_sub.max(r.6);
            let cid = format!("{}-{:05}-{:05}", plan.tag, cfg.g, r.1);
            match log.get(cid.as_str()) {
                Some((_, at)) => {
                    landed += 1;
                    t_land += 1;
                    let l = at - r.6;
                    lats.push(l);
                    bump(&mut lat, l, 5);
                    bump(lat_min.entry(r.2 / 60_000).or_default(), l, 10);
                    *landed_s.entry((at - t0).div_euclid(1000)).or_default() += 1;
                    if *at >= w0 && *at < w1 {
                        t_win += 1;
                    }
                }
                None => missing.push(json!({ "seat": i, "cid": cid })),
            }
        }
        lats.sort_unstable();
        let d = |k: &str| post[i][k].as_i64().unwrap_or(0) - pre[i][k].as_i64().unwrap_or(0);
        trunks.push(json!({
            "trunk": cfg.trunk, "g": cfg.g, "offered": t_off, "errors": t_err, "accepted": t_acc, "landed": t_land,
            "in_window": t_win, "p50": pct(&lats, 50.0), "p99": pct(&lats, 99.0),
            "landed_delta": d("landed"), "git_requests": d("git_requests"), "batches": d("batches"), "fetch_retries": d("fetch_retries"),
            "ejected": d("ejected"), "external_moves": d("external_moves"), "seq": post[i]["seq"], "wasm_bytes": post[i]["wasm_bytes"],
            "log_error": logs[i].as_ref().err(), "stats_error": post[i].get("error"),
        }));
    }
    json!({
        "tag": plan.tag, "t0_ms": t0, "rate": plan.rate, "secs": plan.secs, "k": plan.k, "seats": plan.seats.len(),
        "offered": offered, "sent": sent, "accepted": accepted, "landed": landed, "not_sent": unsent_n,
        "errors": errors, "samples": samples, "stop": stop,
        "first_submitted_ms": if first_sub == i64::MAX { Value::Null } else { first_sub.into() },
        "last_submitted_ms": if last_sub == 0 { Value::Null } else { last_sub.into() },
        "sent_end": (sent_end - plan.t0_ms) as i64, "drained_at": drained.map(|d| (d - plan.t0_ms) as i64),
        "lat": lat, "lat_min": lat_min, "push": push_h, "land": land_h, "lag": lag_h, "landed_s": landed_s, "batch_sizes": batch_sizes,
        "polls": polls, "trunks": trunks, "missing": missing,
    })
}
