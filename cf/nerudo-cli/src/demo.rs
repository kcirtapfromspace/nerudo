//! `nerudo demo`: a trunk on Cloudflare, a few agents landing through it, and a check of
//! the result. Everything goes through the deployed Worker and Artifacts.
//!
//! 1. Create: the Trunk Durable Object creates the trunk repo in Artifacts, pushes the
//!    seed, and mints its own write token. Nothing else ever holds one.
//! 2. Land: each agent gets its own Artifacts fork, builds changes on the latest trunk with
//!    nerudo-git, pushes them to its fork and asks to land. The sequencer lands changes
//!    whose footprints don't overlap together, with one CI run per batch. CI is a real type
//!    check of src/ (tsc from ts-rust, in the nerudo-ci Worker) plus simulated time. Two
//!    agents edit src/ from the same old trunk; whichever lands second fails the check.
//! 3. Seal: one change carries irreversible SQL, so it waits for a human's signed seal.
//!    The seed's nerudo.toml lists your key as the trunk's only approver.
//! 4. Check: fetch trunk with a read-only token, run the trunk checks on every commit,
//!    verify every seal against the approvers at its parent, find every landed change
//!    exactly once, and verify the landing log's hash chain.

use crate::{context, die, env_or, hex, http, key_exists, keygen, load_key, my_name, now_ms, seal, Api};
use futures::executor::block_on;
use nerudo_core::log::{verify_chain, GENESIS};
use nerudo_core::merge::digest_index;
use nerudo_core::{trunk_checks, verify_landing, LogEntry, Rules, RULES_PATH};
use nerudo_git::{fetch, ls_refs, push, write_pack, Commit, FileEntry, Oid, RefUpdate, Remote, Signature, Store, Transport, MODE_BLOB};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{IsTerminal, Read};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const RULES: &str = include_str!("../../fixtures/demo-nerudo.toml");
/// Where the fixture's approver key goes: the demo enrolls the key of whoever runs it.
const KEY_SLOT: &str = "<your nerudo pubkey>";
const NAME_SLOT: &str = "<your name>";
const MAIN: &str = "refs/heads/main";
const TARGETS: u64 = 12;
const LINES: u64 = 12;
const DROP: &str = "alter table users drop column legacy_flag;";
const TSCONFIG: &str = r#"{
  "compilerOptions": { "strict": true, "target": "es2022", "module": "es2022", "lib": ["es2022"], "skipLibCheck": true },
  "include": ["src"]
}
"#;
const PRICE_1: &str = "export function price(cents: number): string {\n  return (cents / 100).toFixed(2);\n}\n";
const PRICE_2: &str = "export function price(cents: number, currency: string): string {\n  return `${(cents / 100).toFixed(2)} ${currency}`;\n}\n";

/// git smart HTTP over the CLI's HTTP client. Error statuses come back as (status, body).
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

/// What an agent does in one change. Deterministic from (agent, index).
struct Plan {
    cid: String,
    target: u64,
    line: u64,
    dep: bool,
    migration: Option<String>,
}

impl Plan {
    fn new(agent: usize, index: usize, irreversible: bool) -> Plan {
        let h = format!("{agent}:{index}").bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
        let cid = format!("a{agent:02}c{index}");
        let migration = if irreversible {
            Some(format!("-- {cid}: the column is unused now\n{DROP}\n"))
        } else if (h >> 24) % 4 == 0 {
            Some(format!("-- {cid}\nalter table users add column c_{cid} int;\n"))
        } else {
            None
        };
        Plan { cid, target: h % TARGETS, line: (h >> 8) % LINES, dep: (h >> 16) % 3 == 0, migration }
    }

    fn summary(&self) -> String {
        let mut s = format!("edit t{:02} line {}, changelog", self.target, self.line);
        if self.dep {
            s.push_str(", new dependency");
        }
        if let Some(m) = &self.migration {
            s.push_str(if m.contains(DROP) { ", migration that DROPS a column" } else { ", migration" });
        }
        s
    }
}

struct Shared {
    out: Out,
    trunk: String,
    token: String,
    rules: Rules,
    /// change id -> what it does, for the batch lines.
    summaries: Mutex<BTreeMap<String, String>>,
    /// change ids with a landed receipt (the only proof of landing).
    landed: Mutex<BTreeSet<String>>,
    parked: Mutex<Option<(String, u32)>>,
    /// Ejections for a conflict (the agent rebuilt), and the other ejections by kind.
    ejections: Mutex<u32>,
    other_ejections: Mutex<BTreeMap<String, u32>>,
    /// A read-only trunk remote, from the first fork the sequencer handed out.
    reader: Mutex<Option<Remote>>,
    /// Highest seq the batch watcher has printed.
    printed: AtomicU64,
    /// When the first change was submitted; throughput is measured from here.
    first_submit: Mutex<Option<Instant>>,
    /// The trunk's first commit. The two TypeScript changes start from it.
    seed: Oid,
    /// (agent that changes price(), agent that adds a caller), when there are 2+ agents.
    ts_pair: Option<(usize, usize)>,
    /// Agent name -> the task it claims and lands for.
    tasks: BTreeMap<String, String>,
    /// The task with competing attempts, and the agents (by number) that attempt it.
    compare: Option<(String, Vec<usize>)>,
}

/// The competing attempts: agents format prices for receipts, each their own way. The
/// third attempt passes a string to toFixed(), so its trial fails the type check.
const FORMATS: [(&str, &str, &str); 3] = [
    (
        "format prices with toFixed(2)",
        "Receipts need two decimals. toFixed(2) does it in one line and works for every currency we sell in.",
        "export const format = (n: number): string => n.toFixed(2);\n",
    ),
    (
        "format prices with Intl.NumberFormat",
        "Receipts go to customers in many countries. Intl.NumberFormat writes the amount and the currency the way each reader expects.",
        "export function format(n: number, currency = 'EUR'): string {\n  return new Intl.NumberFormat('en', { style: 'currency', currency }).format(n);\n}\n",
    ),
    (
        "format prices with two decimals",
        "Two decimals, given as text so that the call reads like the receipt.",
        "export const format = (n: number): string => n.toFixed('2');\n",
    ),
];

/// What each attempt records as context: its own plan, decisions and rejected alternatives.
const FORMAT_PLANS: [&str; 3] = [
    "## Plan\nformat() returns the amount with two decimals, from toFixed(2).\n\n\
     ## Decisions\n- One line, no dependency: receipts only need the number.\n\
     - The currency stays where it is printed today, next to the amount.\n\n\
     ## Rejected\n- Intl.NumberFormat: more than receipts need, and its output differs between runtimes.\n",
    "## Plan\nformat() writes the amount and the currency with Intl.NumberFormat, for the reader's locale.\n\n\
     ## Decisions\n- The currency is a parameter, 'EUR' by default, as price() callers pass today.\n\
     - Locale 'en' for now; the receipt page can pass its own later.\n\n\
     ## Rejected\n- toFixed(2) and a suffix: it is wrong for currencies without cents.\n",
    "## Plan\nformat() gives two decimals, with the digits as text so the call reads like the receipt.\n\n\
     ## Decisions\n- toFixed('2'): the same two decimals, written as the receipt shows them.\n\n\
     ## Rejected\n- A formatting library: one function does not need one.\n",
];

/// One attempt at the receipts task: src/format.ts, written this attempt's way.
fn format_attempt(store: &mut Store, base: Oid, cid: &str, agent: &str, k: usize) -> Oid {
    let (summary, why, text) = FORMATS[k];
    let tree = git("read base", store.commit(&base)).tree;
    let mut files = git("read base tree", store.flatten(&tree));
    put(store, &mut files, "src/format.ts", text);
    put(store, &mut files, &format!("changes/{cid}.txt"), &format!("{cid}\n"));
    let changelog = read(store, &files, "CHANGELOG.md") + &format!("- {cid}: {summary}\n");
    put(store, &mut files, "CHANGELOG.md", &changelog);
    commit_files(store, base, &files, agent, &format!("{cid}: {summary}\n\n{why}\n"))
}

/// The TypeScript part of the demo. Two agents edit src/ from the seed, an old checkout:
/// one changes price() to take a currency and updates the callers it can see; the other
/// adds a caller written against the price() it can see. Each passes the type check on its
/// own base. Whichever lands second fails it on trunk, is ejected, and is built again.
#[derive(Clone, Copy)]
enum TsEdit {
    Currency,
    Caller,
}

impl TsEdit {
    fn for_agent(sh: &Shared, n: usize) -> Option<TsEdit> {
        match sh.ts_pair {
            Some((a, _)) if a == n => Some(TsEdit::Currency),
            Some((_, b)) if b == n => Some(TsEdit::Caller),
            _ => None,
        }
    }

    fn summary(self) -> &'static str {
        match self {
            TsEdit::Currency => "price() takes a currency; update its callers",
            TsEdit::Caller => "add src/cart.ts, a caller of price()",
        }
    }

    /// The commit body: why the agent made the change. It lands on trunk with it.
    fn why(self) -> &'static str {
        match self {
            TsEdit::Currency => "Prices now show in more than one currency, so price() must say which one. Every caller passes it.",
            TsEdit::Caller => "The cart page needs a price for each line. It calls price() as price() is defined on this checkout.",
        }
    }

    fn build(self, store: &mut Store, base: Oid, cid: &str, agent: &str) -> Oid {
        let tree = git("read base", store.commit(&base)).tree;
        let mut files = git("read base tree", store.flatten(&tree));
        match self {
            TsEdit::Currency => {
                put(store, &mut files, "src/price.ts", PRICE_2);
                let callers: Vec<String> = files.keys().filter(|p| p.starts_with("src/") && p.ends_with(".ts") && *p != "src/price.ts").cloned().collect();
                for p in callers {
                    let text = add_currency(&read(store, &files, &p));
                    put(store, &mut files, &p, &text);
                }
            }
            TsEdit::Caller => {
                let call = if read(store, &files, "src/price.ts").contains("currency") { "price(n * 100, 'EUR')" } else { "price(n * 100)" };
                put(store, &mut files, "src/cart.ts", &format!("import {{ price }} from './price';\nexport const line = (n: number) => {call};\n"));
            }
        }
        put(store, &mut files, &format!("changes/{cid}.txt"), &format!("{cid}\n"));
        let changelog = read(store, &files, "CHANGELOG.md") + &format!("- {cid}: {}\n", self.summary());
        put(store, &mut files, "CHANGELOG.md", &changelog);
        commit_files(store, base, &files, agent, &format!("{cid}: {}\n\n{}\n", self.summary(), self.why()))
    }
}

/// Add `'EUR'` to each one-argument call of price() in `text`.
fn add_currency(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("price(") {
        let (before, after) = rest.split_at(i + "price(".len());
        out.push_str(before);
        let mut depth = 1;
        let Some(end) = after.char_indices().find_map(|(j, c)| {
            depth += match c { '(' => 1, ')' => -1, _ => 0 };
            (depth == 0).then_some(j)
        }) else {
            rest = after;
            break;
        };
        out.push_str(&after[..end]);
        if !after[..end].contains(',') {
            out.push_str(", 'EUR'");
        }
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

fn flag<T: std::str::FromStr>(f: &BTreeMap<String, String>, k: &str, default: T) -> T {
    f.get(k).map(|v| v.parse().unwrap_or_else(|_| die(&format!("--{k}: bad value {v:?}")))).unwrap_or(default)
}

fn short(oid: &str) -> &str {
    &oid[..oid.len().min(7)]
}

fn git<T>(what: &str, r: nerudo_git::Result<T>) -> T {
    r.unwrap_or_else(|e| die(&format!("{what}: {e}")))
}

fn put(store: &mut Store, files: &mut BTreeMap<String, FileEntry>, path: &str, text: &str) {
    files.insert(path.to_string(), FileEntry { mode: MODE_BLOB, oid: store.put_blob(text.as_bytes()) });
}

fn read(store: &Store, files: &BTreeMap<String, FileEntry>, path: &str) -> String {
    files.get(path).and_then(|e| store.blob(&e.oid).ok()).map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default()
}

fn regenerate(store: &mut Store, files: &mut BTreeMap<String, FileEntry>, rules: &Rules) {
    for d in &rules.derived {
        let text = digest_index(store, files, &d.source).unwrap_or_else(|e| die(&format!("{}: {e:?}", d.path)));
        put(store, files, &d.path, &text);
    }
}

fn seed(rules: &Rules, rules_text: &str) -> BTreeMap<String, String> {
    let mut text: BTreeMap<String, String> = BTreeMap::new();
    text.insert(RULES_PATH.into(), rules_text.into());
    text.insert("README.md".into(), "# nerudo demo trunk\n\nEvery commit after the seed was landed by the nerudo sequencer.\n".into());
    for t in 0..TARGETS {
        text.insert(format!("targets/t{t:02}/src.txt"), (0..LINES).map(|i| format!("t{t:02} line {i}\n")).collect());
    }
    text.insert("deps/base.txt".into(), "base==1.0\n".into());
    text.insert("migrations/0001_init.sql".into(), "create table users(id int, legacy_flag int);\n".into());
    text.insert("changes/.keep".into(), String::new());
    text.insert("CHANGELOG.md".into(), "# Changelog\n".into());
    text.insert("tsconfig.json".into(), TSCONFIG.into());
    text.insert("src/price.ts".into(), PRICE_1.into());
    text.insert("src/index.ts".into(), "import { price } from './price';\nexport const total = price(250);\n".into());
    let mut store = Store::new();
    let mut files = BTreeMap::new();
    for (p, t) in &text {
        put(&mut store, &mut files, p, t);
    }
    regenerate(&mut store, &mut files, rules);
    for d in &rules.derived {
        text.insert(d.path.clone(), read(&store, &files, &d.path));
    }
    text
}

/// The agent's commit for `plan`, built on `base` the way a coding agent would edit files.
fn build(store: &mut Store, base: Oid, plan: &Plan, agent: &str, rules: &Rules) -> Oid {
    let tree = git("read base", store.commit(&base)).tree;
    let mut files = git("read base tree", store.flatten(&tree));
    let target = format!("targets/t{:02}/src.txt", plan.target);
    let mut lines: Vec<String> = read(store, &files, &target).lines().map(str::to_string).collect();
    lines[plan.line as usize] = format!("t{:02} line {} (changed by {})", plan.target, plan.line, plan.cid);
    put(store, &mut files, &target, &(lines.join("\n") + "\n"));
    put(store, &mut files, &format!("changes/{}.txt", plan.cid), &format!("{}\n", plan.cid));
    let changelog = read(store, &files, "CHANGELOG.md") + &format!("- {}: {}\n", plan.cid, plan.summary());
    put(store, &mut files, "CHANGELOG.md", &changelog);
    if plan.dep {
        put(store, &mut files, &format!("deps/{}.txt", plan.cid), &format!("dep-{}==1.0\n", plan.cid));
        regenerate(store, &mut files, rules);
    }
    if let Some(sql) = &plan.migration {
        put(store, &mut files, &format!("migrations/NEXT_{}.sql", plan.cid), sql);
    }
    // Only the change that needs a human explains itself; the rest are routine edits.
    let why = if plan.migration.as_deref().is_some_and(|m| m.contains(DROP)) {
        "\nlegacy_flag has had no reader since the flags moved to their own table. Dropping it cannot be undone, so it needs a seal.\n"
    } else {
        ""
    };
    commit_files(store, base, &files, agent, &format!("{}: {}\n{why}", plan.cid, plan.summary()))
}

/// POST to the app API of a trunk as a person (a personal link's key).
fn app_post(worker: &str, trunk: &str, key: &str, user: &str, path: &str, body: &Value) -> Result<Value, String> {
    let url = format!("{}/t/{trunk}/api/{path}", worker.trim_end_matches('/'));
    match http().post(&url).set("X-Nerudo-Key", key).set("X-Nerudo-User", user).set("Content-Type", "application/json").send_string(&body.to_string()) {
        Ok(r) => serde_json::from_str(&r.into_string().unwrap_or_default()).map_err(|e| e.to_string()),
        Err(ureq::Error::Status(code, r)) => Err(format!("HTTP {code}: {}", r.into_string().unwrap_or_default())),
        Err(e) => Err(e.to_string()),
    }
}

/// GET from the app API of a trunk as a person.
fn app_get(worker: &str, trunk: &str, key: &str, user: &str, path: &str) -> Result<Value, String> {
    let url = format!("{}/t/{trunk}/api/{path}", worker.trim_end_matches('/'));
    match http().get(&url).set("X-Nerudo-Key", key).set("X-Nerudo-User", user).call() {
        Ok(r) => serde_json::from_str(&r.into_string().unwrap_or_default()).map_err(|e| e.to_string()),
        Err(ureq::Error::Status(code, r)) => Err(format!("HTTP {code}: {}", r.into_string().unwrap_or_default())),
        Err(e) => Err(e.to_string()),
    }
}

/// A person compares the competing attempts, as the app's Compare tab shows them, and picks
/// one. With --yes, the attempt that passed and changes the fewest lines.
fn pick_attempt(sh: &Shared, api: &Api, worker: &str, trunk: &str, key: &str, id: &str, yes: bool) {
    sh.out.section(&format!("3b. A person compares the attempts for {id} and picks one"));
    println!("Each attempt got a trial on trunk as it is now: applied, checked and type-checked, but not");
    println!("pushed. Nothing merged. The app's Compare tab shows the attempts side by side.\n");
    let me = my_name();
    let cmp = match app_get(worker, trunk, key, &me, &format!("task/{id}/compare")) {
        Ok(v) => v,
        Err(e) => return println!("cannot read the attempts: {e}"),
    };
    let attempts: Vec<Value> = cmp["attempts"].as_array().cloned().unwrap_or_default().into_iter().filter(|a| a["state"] == "awaiting_pick").collect();
    if attempts.is_empty() {
        return println!("no attempt is waiting for a pick.");
    }
    let mut best: Option<(u64, String)> = None;
    for a in &attempts {
        let t = &a["trial"];
        let cid = a["change_id"].as_str().unwrap_or("?");
        let verdict = match t["passed"].as_bool() {
            Some(true) => sh.out.paint("32", "checks passed"),
            Some(false) => {
                let d = &t["ci"]["diagnostics"][0];
                sh.out.paint("31", &format!("checks FAILED: {} {}", d["code"].as_str().unwrap_or("?"), d["text"].as_str().unwrap_or("")))
            }
            None => "no check verdict".to_string(),
        };
        let (adds, dels) = (t["adds"].as_u64().unwrap_or(0), t["dels"].as_u64().unwrap_or(0));
        let subject = a["subject"].as_str().unwrap_or("");
        let subject = subject.strip_prefix(&format!("{cid}: ")).unwrap_or(subject);
        println!("  {cid:<8} {:<4} {} files +{adds} −{dels}  {subject}", a["agent"].as_str().unwrap_or("?"), t["files"]);
        println!("           {verdict}");
        // Its plan, beside its trial: the version of its context it was sent with.
        if a["context_commit"].is_string() {
            let v = a["context_version"].as_u64().map(|v| format!("&v={v}")).unwrap_or_default();
            let plan = app_get(worker, trunk, key, &me, &format!("context/{cid}?rev={}{v}", a["rev"])).ok();
            if let Some(line) = plan.and_then(|c| c["text"].as_str().map(|t| context::plan_lines(t, 1))).and_then(|l| l.into_iter().next()) {
                println!("           plan: {line}");
            }
        }
        if t["passed"].as_bool() != Some(false) && best.as_ref().is_none_or(|(n, _)| adds + dels < *n) {
            best = Some((adds + dels, cid.to_string()));
        }
    }
    let Some((_, suggested)) = best else {
        return println!("\nno attempt passed its checks: a person writes the task again, or asks for another attempt.");
    };
    let pick = if yes {
        suggested
    } else {
        print!("\npick which attempt? [{suggested}] ");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        let line = line.trim();
        if line.is_empty() { suggested } else { line.to_string() }
    };
    match app_post(worker, trunk, key, &me, &format!("task/{id}/pick"), &json!({ "change": pick })) {
        Ok(r) => {
            let closed: Vec<&str> = r["closed"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            println!("\nyou picked {pick}: it goes back on the queue and merges with its own CI run.");
            if !closed.is_empty() {
                println!("closed as not picked: {} (their branches are stale; cleanup deletes them)", closed.join(", "));
            }
            let rev = attempts.iter().find(|a| a["change_id"] == pick.as_str()).and_then(|a| a["rev"].as_u64()).unwrap_or(1) as u32;
            let rec = receipt(api, &pick, rev);
            if rec["state"] == "landed" {
                sh.landed.lock().unwrap().insert(pick.clone());
                let seq = rec["seq"].as_u64().unwrap_or(0);
                while sh.printed.load(Ordering::SeqCst) < seq {
                    std::thread::sleep(Duration::from_millis(100));
                }
            } else {
                println!("{pick} did not merge: {} {}", rec["state"], rec["reason"]);
            }
        }
        Err(e) => println!("\nthe pick was refused: {e}"),
    }
}

fn commit_files(store: &mut Store, base: Oid, files: &BTreeMap<String, FileEntry>, agent: &str, message: &str) -> Oid {
    let tree = store.write_tree(files);
    let sig = Signature::new(agent, &format!("{agent}@agents.invalid"), now_ms() / 1000);
    store.put_commit(&Commit { tree, parents: vec![base], author: sig.clone(), committer: sig, extra: vec![], message: message.to_string() })
}

fn head(remote: &Remote) -> Oid {
    head_of(remote, MAIN)
}

fn head_of(remote: &Remote, branch: &str) -> Oid {
    let refs = git("read trunk refs", block_on(ls_refs(&Https, remote, branch)));
    refs.into_iter().find(|(n, _)| n == branch).map(|(_, o)| o).unwrap_or_else(|| die(&format!("trunk has no {branch}")))
}

/// Long-poll the receipt until the change leaves the queue.
fn receipt(api: &Api, cid: &str, rev: u32) -> Value {
    loop {
        let rec = api.call("GET", &format!("changes/{cid}?rev={rev}&wait=30"), None);
        if !rec.is_null() && rec["state"] != "queued" {
            return rec;
        }
    }
}

fn agent(sh: &Shared, n: usize, changes: usize, irreversible_last: bool) {
    let name = format!("a{n:02}");
    let api = Api::with_token(&sh.trunk, sh.token.clone());
    let f = api.call("POST", "forks", Some(json!({ "agent": name })));
    // The agent's own key: its claims and landings are checked against its name and fork.
    let api = api.with_agent_key(crate::keep_agent_key(&sh.trunk, &name, &f));
    let s = |k: &str| f[k].as_str().unwrap_or_else(|| die(&format!("fork response has no {k}"))).to_string();
    let fork = Remote::new(&s("remote"), Some(s("authorization")));
    let trunk = Remote::new(&s("trunk"), f["trunk_authorization"].as_str().map(str::to_string));
    sh.reader.lock().unwrap().get_or_insert_with(|| trunk.clone());
    sh.out.at(&format!("{name}  got fork {}  (token writes its fork only; trunk token is read-only)", s("name")));

    // What the fork already has, so each push sends only what's new to it.
    let fork_has: Vec<Oid> = git("read fork refs", block_on(ls_refs(&Https, &fork, "refs/heads/"))).into_iter().map(|(_, o)| o).collect();
    let mut seat = Seat { name: name.clone(), api, fork_name: s("name"), fork, trunk, store: Store::new(), fork_has, seen: vec![], context_tip: None };
    // An agent with an task claims it first: the task is its work, and its landing closes it.
    let task = sh.tasks.get(&name).cloned();
    if let Some(id) = &task {
        if seat.api.try_call("POST", &format!("tasks/{id}/claim"), Some(json!({ "as": name }))).is_ok() {
            sh.out.at(&format!("{name}  claimed {}", sh.out.paint("36", id)));
        }
    }
    if let Some(ts) = TsEdit::for_agent(sh, n) {
        let cid = format!("{name}ts");
        if !submit(sh, &mut seat, &cid, ts.summary(), Some(sh.seed), task.as_deref(), |store, base| ts.build(store, base, &cid, &name)) {
            return;
        }
    }
    for i in 1..=changes {
        let plan = Plan::new(n, i, irreversible_last && i == changes);
        // The column drop is the irreversible agent's task.
        let for_task = if plan.migration.as_deref().is_some_and(|m| m.contains(DROP)) { task.as_deref() } else { None };
        if !submit(sh, &mut seat, &plan.cid, &plan.summary(), None, for_task, |store, base| build(store, base, &plan, &name, &sh.rules)) {
            return;
        }
    }
    // Last, an attempt at the task that wants competing attempts, if this agent is one of them.
    if let Some((id, who)) = &sh.compare {
        if let Some(k) = who.iter().position(|&w| w == n) {
            if seat.api.try_call("POST", &format!("tasks/{id}/claim"), Some(json!({ "as": name }))).is_ok() {
                sh.out.at(&format!("{name}  took attempt {} of {} on {}", k + 1, who.len(), sh.out.paint("36", id)));
            }
            let cid = format!("{name}fmt");
            submit(sh, &mut seat, &cid, FORMATS[k].0, None, Some(id), |store, base| format_attempt(store, base, &cid, &name, k));
        }
    }
}

/// One agent: its fork, its read-only view of trunk, and what it has fetched and pushed.
struct Seat {
    name: String,
    api: Api,
    fork_name: String,
    fork: Remote,
    trunk: Remote,
    store: Store,
    fork_has: Vec<Oid>,
    seen: Vec<Oid>,
    /// The tip of the fork's nerudo/context branch, once the agent has written context.
    context_tip: Option<Oid>,
}

/// The plan of the landing that beat a change or moved main under it, from the receipt of the
/// ejected revision: "#7 (a01)" and the first lines of its pinned context.
fn plan_of_winner(api: &Api, rec: &Value) -> Option<(String, Vec<String>)> {
    let r = &rec["reason"];
    let list = r["beaten_by"].as_array().filter(|l| !l.is_empty()).or_else(|| r["moved"].as_array())?;
    let p = list.iter().find(|p| p["context"].is_string())?;
    let cid = p["change_id"].as_str()?;
    let text = api.try_call("GET", &format!("context/{cid}?rev={}", p["rev"]), None).ok()?["text"].as_str()?.to_string();
    let who = format!("{} ({})", p["number"].as_u64().map(|n| format!("#{n}")).unwrap_or_else(|| cid.to_string()), p["agent"].as_str().unwrap_or("?"));
    Some((who, context::plan_lines(&text, 4)))
}

/// "#7 a01ts (a01)" for each landing that beat a change or moved main under it.
fn winners(list: &Value) -> String {
    let names: Vec<String> = list
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| format!("{} {} ({})", p["number"].as_u64().map(|n| format!("#{n}")).unwrap_or_default(), p["change_id"].as_str().unwrap_or("?"), p["agent"].as_str().unwrap_or("?")))
        .collect();
    names.join(", ")
}

/// What some of the demo's agents record as context with a change: the TypeScript pair, the
/// column drop, the competing attempts, and every rebuild after an ejection (what went wrong,
/// and the plan of the change that landed first, which the rebuild keeps).
fn context_text(cid: &str, summary: &str, rev: u32, last: Option<&Value>, winner: Option<&(String, Vec<String>)>) -> Option<String> {
    let mut text = String::new();
    if let Some(k) = FORMATS.iter().position(|f| f.0 == summary) {
        text.push_str(FORMAT_PLANS[k]);
    } else if summary == TsEdit::Currency.summary() {
        text.push_str(
            "## Plan\nprice() takes the currency as a second argument, and every caller passes it.\n\n\
             ## Decisions\n- Callers in src/ pass 'EUR' for now: it is the only currency in use.\n\
             - Change the signature, not the behaviour: amounts stay in cents.\n\n\
             ## Rejected\n- A default `currency = 'EUR'`: callers that should choose a currency would never notice.\n\
             - A second function priceIn(): two ways to format a price, and the old one stays the easy one.\n",
        );
    } else if summary == TsEdit::Caller.summary() {
        text.push_str(
            "## Plan\nAdd src/cart.ts with line(n), the price of n units, for the cart page.\n\n\
             ## Decisions\n- Call price() as it is declared on my checkout.\n\n\
             ## Rejected\n- Format in the cart itself: price() already owns formatting.\n",
        );
    } else if summary.contains("DROPS") {
        text.push_str(
            "## Plan\nDrop users.legacy_flag.\n\n\
             ## Decisions\n- Checked: nothing has read the column since the flags moved to their own table.\n\
             - One migration, no backfill: the data has a copy in the flags table.\n\n\
             ## Rejected\n- Keep the column and stop writing it: a dead column that the next reader must understand.\n\n\
             ## Risk\nIrreversible. nerudo.toml makes it wait for a maintainer's seal.\n",
        );
    }
    if let Some(rec) = last.filter(|_| rev > 1) {
        let r = &rec["reason"];
        let why = match r["kind"].as_str().unwrap_or("") {
            "conflict" => format!("it edited lines that changed on trunk ({})", r["paths"]),
            "ci_failed" => {
                let d = &r["diagnostics"][0];
                let at = |k: &str| d[k].as_str().map(str::to_string).unwrap_or_else(|| d[k].to_string());
                format!("the type check failed on trunk: {} {}:{} {}", at("code"), at("file"), at("line"), at("text"))
            }
            other => other.replace('_', " "),
        };
        let why = why.trim_end_matches('.');
        text.push_str(&format!(
            "\n## Revision {rev}\nRevision {} was ejected: {why}. A change that landed first moved trunk under it. \
             This revision is built again on the new trunk; the plan is the same.\n",
            rev - 1
        ));
        if let Some((who, lines)) = winner.filter(|(_, l)| !l.is_empty()) {
            text.push_str(&format!("\n## What landed first\n{who} landed first. Its plan:\n\n"));
            for l in lines {
                text.push_str(&format!("> {l}\n"));
            }
            text.push_str("\nThis revision keeps that decision and builds on it.\n");
        }
    }
    (!text.is_empty()).then(|| format!("# {cid}: {summary}\n\n{}", text.trim_start()))
}

/// Build one change, push it to the agent's fork and ask to land it. `old_base` builds the
/// first revision on an older trunk commit instead of the latest. After an ejection for a
/// conflict or by CI, build it again on the new trunk. Returns false when the change is
/// parked for a human: the agent stops there.
fn submit(sh: &Shared, a: &mut Seat, cid: &str, summary: &str, old_base: Option<Oid>, task: Option<&str>, build: impl Fn(&mut Store, Oid) -> Oid) -> bool {
    sh.summaries.lock().unwrap().insert(cid.to_string(), summary.to_string());
    let name = a.name.clone();
    let mut rev = 1;
    let mut last: Option<Value> = None;
    // The plan of the change that beat the last revision, which the next one quotes.
    let mut winner: Option<(String, Vec<String>)> = None;
    loop {
        // Fetch the tip, not an old commit by id: the old base is in its history.
        let tip = head(&a.trunk);
        if !a.store.contains(&tip) {
            git("fetch trunk", block_on(fetch(&Https, &a.trunk, &[tip], &a.seen, &mut a.store)));
            a.seen = vec![tip];
        }
        let base = match old_base {
            Some(b) if rev == 1 => b,
            _ => tip,
        };
        let commit = build(&mut a.store, base);
        // Context, when the agent records some, goes to the fork's nerudo/context branch in
        // the same push as the change.
        let context = context_text(cid, summary, rev, last.as_ref(), winner.as_ref()).map(|text| {
            let at = now_ms();
            let meta = context::Meta { change_id: cid, rev, agent: &name, task, commit: &commit.to_hex(), base: &base.to_hex(), at_ms: at };
            let doc = context::document(&meta, &text, &[sh.token.clone()]);
            context::commit_native(&mut a.store, a.context_tip, &context::path(cid, rev), &doc, &name, at)
        });
        let stops: Vec<Oid> = a.fork_has.iter().filter(|o| a.store.contains(o)).copied().collect();
        let mut wanted = git("pack", a.store.missing_from(&commit, &stops));
        let mut updates = vec![RefUpdate { name: format!("refs/heads/land/{cid}-{rev}"), old: Oid::ZERO, new: commit }];
        if let Some(c) = context {
            for o in git("pack", a.store.missing_from(&c, &stops)) {
                if !wanted.contains(&o) {
                    wanted.push(o);
                }
            }
            updates.push(RefUpdate { name: context::BRANCH.into(), old: a.context_tip.unwrap_or(Oid::ZERO), new: c });
        }
        let pack = write_pack(&git("pack", a.store.object_list(&wanted)));
        let report = git("push to fork", block_on(push(&Https, &a.fork, &updates, pack)));
        if !report.all_ok() {
            die(&format!("{name}: push to its fork refused: {:?}", report.refs));
        }
        a.fork_has.push(commit);
        if let Some(c) = context {
            a.fork_has.push(c);
            a.context_tip = Some(c);
        }
        let mut req = json!({ "change_id": cid, "rev": rev, "fork": a.fork_name, "commit": commit, "base": base });
        if let Some(id) = task {
            req["task"] = json!(id);
        }
        if let Some(c) = context {
            req["context_commit"] = json!(c);
        }
        sh.first_submit.lock().unwrap().get_or_insert_with(Instant::now);
        a.api.call("POST", "land", Some(req));
        let from = if base == tip { String::new() } else { format!(" (from the seed {})", short(&base.to_hex())) };
        sh.out.at(&format!("{name}  {cid}  pushed to its fork, asked to land: {}{from}", sh.out.paint("2", summary)));
        let rec = receipt(&a.api, cid, rev);
        last = Some(rec.clone());
        if rec["state"] == "ejected" {
            winner = plan_of_winner(&a.api, &rec);
        }
        match rec["state"].as_str().unwrap_or_default() {
            "landed" => {
                sh.landed.lock().unwrap().insert(cid.to_string());
                return true;
            }
            "awaiting_seal" => {
                let why: Vec<&str> = rec["seal_reasons"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                sh.out.at(&format!("{name}  {cid}  {}: {}", sh.out.paint("33", "parked, needs a human seal"), why.join("; ")));
                *sh.parked.lock().unwrap() = Some((cid.to_string(), rev));
                return false;
            }
            "awaiting_pick" => {
                let verdict = match rec["reason"]["passed"].as_bool() {
                    Some(true) => "its trial passed the type check",
                    Some(false) => "its trial FAILED the type check",
                    None => "its trial has no check verdict",
                };
                sh.out.at(&format!("{name}  {cid}  {}: {verdict}", sh.out.paint("33", "waits for a pick")));
                return true;
            }
            "ejected" if rec["reason"]["kind"] == "conflict" && rev < 5 => {
                *sh.ejections.lock().unwrap() += 1;
                let by = winners(&rec["reason"]["beaten_by"]);
                sh.out.at(&format!(
                    "{name}  {cid}  {} on {}{}: rebuilding on the new trunk as rev {}",
                    sh.out.paint("31", "ejected, conflict"),
                    rec["reason"]["paths"],
                    if by.is_empty() { String::new() } else { format!(", beaten by {by}") },
                    rev + 1
                ));
                rev += 1;
            }
            "ejected" if rec["reason"]["kind"] == "ci_failed" && rev < 5 => {
                let d = &rec["reason"]["diagnostics"][0];
                let at = |k: &str| d[k].as_str().map(str::to_string).unwrap_or_else(|| d[k].to_string());
                sh.out.at(&format!(
                    "{name}  {cid}  {} {} {}:{} {}",
                    sh.out.paint("31", "ejected by CI:"),
                    at("code"),
                    at("file"),
                    at("line"),
                    at("text")
                ));
                let moved = winners(&rec["reason"]["moved"]);
                if !moved.is_empty() {
                    sh.out.more(&format!("main moved: {moved}{}", winner.as_ref().map(|(w, _)| format!("; the rebuild quotes {w}'s plan")).unwrap_or_default()));
                }
                sh.out.more(&format!("it passed on its own base, not on trunk: rebuilding on the new trunk as rev {}", rev + 1));
                rev += 1;
            }
            other => {
                // Not a conflict: the summary names these by kind.
                let kind = if other == "ejected" { rec["reason"]["kind"].as_str().unwrap_or("ejected").to_string() } else { other.to_string() };
                *sh.other_ejections.lock().unwrap().entry(kind.replace('_', " ")).or_default() += 1;
                sh.out.at(&format!("{name}  {cid}  {}: {}", sh.out.paint("31", other), rec["reason"]));
                return true;
            }
        }
    }
}

/// Print each batch as the sequencer records it: the log is written atomically per batch.
fn watch(sh: &Shared, done: &AtomicBool) {
    let api = Api::with_token(&sh.trunk, sh.token.clone());
    let mut since = 0;
    loop {
        let finished = done.load(Ordering::SeqCst);
        let log: Vec<LogEntry> = serde_json::from_value(api.call("GET", &format!("log?since={since}"), None)).unwrap_or_default();
        let mut batches: BTreeMap<u64, Vec<&LogEntry>> = BTreeMap::new();
        for e in &log {
            batches.entry(e.batch).or_default().push(e);
        }
        for (b, entries) in batches {
            let (first, last) = (entries[0].seq, entries[entries.len() - 1].seq);
            let seqs = if first == last { format!("seq {first}") } else { format!("seq {first}–{last}") };
            let n = entries.len();
            let what = if n == 1 { "1 change".to_string() } else { format!("{n} changes") };
            let recs: Vec<Value> = entries.iter().map(|e| api.call("GET", &format!("changes/{}?rev={}", e.change_id, e.rev), None)).collect();
            let runs = match recs[0]["ci_runs"].as_u64().unwrap_or(1) {
                1 => "1 CI run".to_string(),
                r => format!("{r} CI runs"),
            };
            sh.out.at(&sh.out.paint(
                "32;1",
                &format!("▶ batch {b} landed: {what}, {runs}, {seqs}, trunk now {}", short(&entries[n - 1].commit_out)),
            ));
            for (e, rec) in entries.iter().zip(&recs) {
                let allocated: Vec<&str> = rec["allocated"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                let mut line = format!("{}  {}", e.change_id, sh.summaries.lock().unwrap().get(&e.change_id).cloned().unwrap_or_default());
                if !allocated.is_empty() {
                    line.push_str(&sh.out.paint("36", &format!("  → numbered {}", allocated.join(", "))));
                }
                sh.out.more(&line);
            }
        }
        if let Some(e) = log.last() {
            since = e.seq;
            sh.printed.store(since, Ordering::SeqCst);
        }
        if finished {
            return;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

pub fn run(f: &BTreeMap<String, String>) {
    let agents: usize = flag(f, "agents", 6).clamp(1, 16);
    let changes: usize = flag(f, "changes", 3).clamp(1, 10);
    let ci_ms: u64 = flag(f, "ci-ms", 2000);
    let crash = f.contains_key("crash-once");
    let yes = f.contains_key("yes");
    let worker = env_or(&["NERUDO_WORKER"], Some("http://127.0.0.1:8787"));
    let admin = env_or(&["NERUDO_ADMIN_TOKEN"], None);
    let token = env_or(&["NERUDO_TOKEN", "NERUDO_AGENT_TOKEN"], None);
    let trunk = f.get("trunk").cloned().unwrap_or_else(|| format!("demo-{}", now_ms() / 1000));
    let out = Out { t0: Instant::now(), color: std::io::stdout().is_terminal() };

    println!("{}", out.paint("1", &format!("nerudo demo: trunk {trunk} on {worker}")));
    if !key_exists() {
        keygen(false);
        println!("created your approver key at ~/.nerudo/key");
    }
    let approver = hex(&load_key().verifying_key().to_bytes());
    let me = my_name();
    let rules_text = RULES.replace(KEY_SLOT, &approver).replace(NAME_SLOT, &me);
    let rules = Rules::parse(&rules_text).unwrap_or_else(|e| die(&format!("demo rules: {e}")));

    out.section("1. Create the trunk");
    println!("The Trunk Durable Object creates the repo in Artifacts, pushes the seed, and mints its");
    println!("own write token. Agents and humans only ever get tokens that can't write trunk.");
    let init = json!({
        "trunk": trunk,
        "host": { "kind": "artifacts", "binding": "ARTIFACTS", "repo": trunk },
        "ci_ms": ci_ms,
        "debug_crash_after_push": if crash { 1 } else { 0 },
        "seed": { "files": seed(&rules, &rules_text), "message": "seed: nerudo demo trunk\n" },
    });
    let created = Api::with_token(&trunk, admin).call("POST", "init", Some(init));
    let s = |v: &Value| v.as_str().unwrap_or("?").to_string();
    println!("  trunk     {}", s(&created["remote"]));
    println!("  seed      {}   rules digest {}", short(&s(&created["head"])), &s(&created["rules_digest"])[..12.min(s(&created["rules_digest"]).len())]);
    println!("  approver  {me}, key {}… (your name and key in nerudo.toml on trunk)", &approver[..16]);
    if crash {
        println!("  {}", out.paint("33", "crash-once: the sequencer will die right after its first push, before recording it"));
    }
    let ts_pair = match agents {
        1 => None,
        2 => Some((1, 2)),
        _ => Some((1, 3)),
    };
    let admin_api = Api::with_token(&trunk, env_or(&["NERUDO_ADMIN_TOKEN"], None));
    if let Ok(board) = admin_api.try_call("POST", "share", Some(json!({}))) {
        println!("  board     {}   (read-only)", board["app"].as_str().or(board["url"].as_str()).unwrap_or("?"));
    }
    let mut task_ids: BTreeMap<String, String> = BTreeMap::new();
    let mut compare: Option<(String, Vec<usize>)> = None;
    let mut you_key: Option<String> = None;
    if let Ok(mine) = admin_api.try_call("POST", "invite", Some(json!({ "user": me }))) {
        let url = mine["app"].as_str().or(mine["url"].as_str()).unwrap_or("?").to_string();
        println!("  you       {url}   (your link: comment, react, review)");
        let key = url.split("k=").nth(1).and_then(|r| r.split('&').next()).unwrap_or_default().to_string();
        you_key = Some(key.clone());
        // The work, as a person writes it down: tasks that agents claim and landings close.
        let wanted: Vec<(Option<String>, Value)> = vec![
            (ts_pair.map(|(a, _)| format!("a{a:02}")), json!({ "title": "Show prices in more than one currency", "priority": 2, "labels": "pricing",
                "body": "Customers outside the euro area see EUR. price() should take the currency, and every caller must pass one." })),
            (ts_pair.map(|(_, b)| format!("a{b:02}")), json!({ "title": "Show a price for each line in the cart", "priority": 3, "labels": "cart",
                "body": "The cart lists items without a price per line. Add a line total that uses price()." })),
            ((agents >= 2).then(|| "a02".to_string()), json!({ "title": "Drop the unused legacy_flag column", "priority": 4, "labels": "database",
                "body": "Nothing reads users.legacy_flag since the flags moved to their own table. Dropping it can't be undone, so it needs a seal." })),
            (None, json!({ "title": "Rate-limit price lookups", "priority": 0, "status": "backlog", "labels": "pricing",
                "body": "Later: a burst of cart updates calls price() for every line." })),
        ];
        for (agent, mut body) in wanted {
            if body.get("status").is_none() {
                body["status"] = json!("todo");
            }
            match app_post(&worker, &trunk, &key, &me, "tasks", &body) {
                Ok(i) => {
                    let id = i["id"].as_str().unwrap_or("?").to_string();
                    println!("  task    {}  {}", out.paint("36", &id), i["title"].as_str().unwrap_or(""));
                    if let Some(a) = agent {
                        task_ids.insert(a, id);
                    }
                }
                Err(e) => println!("  task    not written: {e}"),
            }
        }
        // One task asks for competing attempts: agents a04 onward each try it their own way.
        let attempting: Vec<usize> = (4..=agents).take(FORMATS.len()).collect();
        if attempting.len() >= 2 {
            let body = json!({ "title": "Format prices on receipts", "priority": 3, "labels": "receipts", "status": "todo",
                "attempts": attempting.len(),
                "body": "Receipts print raw numbers. Add format() in src/format.ts. Several agents attempt it; compare the attempts and ship one." });
            match app_post(&worker, &trunk, &key, &me, "tasks", &body) {
                Ok(i) => {
                    let id = i["id"].as_str().unwrap_or("?").to_string();
                    println!("  task    {}  {}  ({} competing attempts)", out.paint("36", &id), i["title"].as_str().unwrap_or(""), attempting.len());
                    compare = Some((id, attempting));
                }
                Err(e) => println!("  task    not written: {e}"),
            }
        }
    }

    let seed_head = Oid::from_hex(&s(&created["head"])).unwrap_or_else(|_| die("init returned no seed commit"));
    let planned = agents * changes + if ts_pair.is_some() { 2 } else { 0 } + if compare.is_some() { 1 } else { 0 };

    out.section(&format!("2. {agents} agents land {changes} changes each (CI per batch: type check + {:.1} s)", ci_ms as f64 / 1000.0));
    println!("Each agent pushes to its own fork and asks to land. Changes on different targets");
    println!("share a batch and one CI run; the changelog, lockfile and migration numbers are");
    println!("merged by rule, so they never conflict. CI type-checks src/ with tsc (ts-rust, as");
    println!("WebAssembly) in the nerudo-ci Worker, then waits the simulated time for the rest.");
    if let Some((a, b)) = ts_pair {
        println!("a{a:02} and a{b:02} also edit src/ from the seed, an old checkout: a{a:02} gives price() a");
        println!("currency, a{b:02} adds a caller of the price() it sees. Each passes the check on its own.");
    }
    if let Some((id, who)) = &compare {
        let names: Vec<String> = who.iter().map(|n| format!("a{n:02}")).collect();
        println!("{} each attempt {id} their own way. Each attempt gets a trial on trunk (the type", names.join(", "));
        println!("check as a dry run) and waits: nothing merges until a person picks one.");
    }
    println!();
    let sh = Shared {
        out: Out { t0: Instant::now(), ..out },
        trunk: trunk.clone(),
        token: token.clone(),
        rules,
        summaries: Mutex::default(),
        landed: Mutex::default(),
        parked: Mutex::default(),
        ejections: Mutex::default(),
        other_ejections: Mutex::default(),
        reader: Mutex::default(),
        printed: AtomicU64::new(0),
        first_submit: Mutex::default(),
        seed: seed_head,
        ts_pair,
        tasks: task_ids,
        compare,
    };
    let done = AtomicBool::new(false);
    let api = Api::with_token(&trunk, token);
    std::thread::scope(|scope| {
        let watcher = scope.spawn(|| watch(&sh, &done));
        let workers: Vec<_> = (1..=agents).map(|n| {
            let sh = &sh;
            scope.spawn(move || agent(sh, n, changes, n == 2.min(agents)))
        }).collect();
        for w in workers {
            w.join().unwrap_or_else(|_| die("an agent thread panicked"));
        }
        let land = (sh.first_submit.lock().unwrap().map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0), sh.landed.lock().unwrap().len());
        // Receipts reach agents before the watcher's next poll; let it print every batch first.
        let seq = api.call("GET", "stats", None)["seq"].as_u64().unwrap_or(0);
        while sh.printed.load(Ordering::SeqCst) < seq {
            std::thread::sleep(Duration::from_millis(100));
        }

        let parked = sh.parked.lock().unwrap().clone();
        if let Some((cid, rev)) = parked {
            sh.out.section("3. A human seals the irreversible change");
            println!("Everything else has landed. {cid} waits until someone with an enrolled key signs a");
            println!("seal for this exact commit. Same as `nerudo seal --trunk {trunk} --change {cid}`.\n");
            match seal(&api, &trunk, &cid, Some(&rev.to_string()), yes, "30m") {
                Some(out) if out["complete"] != true => println!("sealed, but the quorum isn't met yet: {cid} stays parked.\n"),
                Some(_) => {
                    println!("sealed with your key; it lands in the sequencer's next batch.\n");
                    let rec = receipt(&api, &cid, rev);
                    if rec["state"] == "landed" {
                        sh.landed.lock().unwrap().insert(cid.clone());
                        let seq = rec["seq"].as_u64().unwrap_or(0);
                        while sh.printed.load(Ordering::SeqCst) < seq {
                            std::thread::sleep(Duration::from_millis(100));
                        }
                    }
                }
                None => println!("not sealed: {cid} stays parked. Seal it later with nerudo seal --trunk {trunk} --change {cid}"),
            }
        }

        if let (Some((id, _)), Some(key)) = (&sh.compare, &you_key) {
            pick_attempt(&sh, &api, &worker, &trunk, key, id, yes);
        }

        sh.out.section("3c. You cut the first release");
        println!("The sequencer drafts the release from what merged: the next version from the PR subjects,");
        println!("and the notes. You sign it with your key; the sequencer pushes the tag. Same as");
        println!("`nerudo release --trunk {trunk}`.\n");
        let o = crate::release::Options { version: None, at: None, notes_file: None, title: None, prerelease: false, yes, ttl: "30m".into() };
        if crate::release::release(&api, &trunk, &o).is_none() {
            println!("not released: release later with nerudo release --trunk {trunk}");
        }
        done.store(true, Ordering::SeqCst);
        watcher.join().unwrap_or_else(|_| die("the batch watcher panicked"));
        check(&sh, &api, &trunk, planned, land);
    });
}

/// Trunk as anyone with a read token sees it.
struct Fetched {
    store: Store,
    /// First-parent history, root first.
    chain: Vec<Oid>,
    tip: Oid,
}

fn fetch_trunk(remote: &Remote, branch: &str) -> Fetched {
    let tip = head_of(remote, branch);
    let mut store = Store::new();
    git("fetch trunk", block_on(fetch(&Https, remote, &[tip], &[], &mut store)));
    let mut chain = vec![tip];
    while let Some(p) = git("walk trunk", store.commit(chain.last().unwrap())).parents.first().copied() {
        chain.push(p);
    }
    chain.reverse();
    Fetched { store, chain, tip }
}

/// What a check of every trunk commit found.
#[derive(Default)]
struct Audit {
    /// Commits that fail the trunk checks under their own nerudo.toml.
    broken: Vec<String>,
    /// Landings that needed a seal (or carried one) and verify.
    sealed: Vec<String>,
    /// Landings whose seals don't verify or don't meet the quorum at their parent.
    unsealed: Vec<String>,
    /// Change-Id -> times it appears on trunk.
    ids: BTreeMap<String, usize>,
}

/// Check every commit: the trunk checks under its own rules, and its seals under the
/// approvers and quorums in its parent's nerudo.toml.
fn audit(t: &Fetched, trunk: &str) -> Audit {
    let mut a = Audit::default();
    for (i, c) in t.chain.iter().enumerate() {
        let commit = git("read commit", t.store.commit(c));
        let files = git("read tree", t.store.flatten(&commit.tree));
        let checked = Rules::parse(&read(&t.store, &files, RULES_PATH)).and_then(|rules| trunk_checks(&t.store, &rules, &files));
        if let Err(e) = checked {
            a.broken.push(format!("{}: {e}", short(&c.to_hex())));
        }
        if let Some(id) = commit.trailer("Change-Id") {
            *a.ids.entry(id).or_default() += 1;
        }
        if i == 0 {
            continue;
        }
        match verify_landing(&t.store, trunk, &t.chain[i - 1], c) {
            Ok(l) if l.break_glass.is_some() => a.sealed.push(format!("{} break-glass: {}", short(&c.to_hex()), l.break_glass.unwrap_or_default())),
            Ok(l) if !l.kinds.is_empty() || !l.signers.is_empty() => {
                a.sealed.push(format!("{} {}, sealed by {}", short(&c.to_hex()), l.kinds.join(" + "), l.signers.join(", ")))
            }
            Ok(_) => {}
            Err(e) => a.unsealed.push(format!("{}: {e}", short(&c.to_hex()))),
        }
    }
    a
}

/// Every Context: trailer on trunk resolves to an immutable copy in the context repo whose
/// source is the fork@commit in the change's receipt.
fn check_context(out: &Out, api: &Api, t: &Fetched) -> bool {
    let (n, bad) = context::verify_trailers(&Https, api, &t.store, &t.chain);
    if n == 0 && bad.is_empty() {
        return mark(out, true, "no landing carries agent context".into());
    }
    let ok = mark(
        out,
        bad.is_empty(),
        format!("{n} Context: trailers resolve to immutable copies in the context repo; each copy's source is its receipt's fork@commit"),
    );
    for b in &bad {
        println!("      {b}");
    }
    ok
}

fn mark(out: &Out, good: bool, s: String) -> bool {
    println!("  {} {s}", if good { out.paint("32", "✓") } else { out.paint("31", "✗") });
    good
}

fn print_audit(out: &Out, t: &Fetched, a: &Audit) -> bool {
    let n = t.chain.len();
    let checks = mark(out, a.broken.is_empty(), format!("{n} commits on trunk (seed included); {} pass the trunk checks", n - a.broken.len()));
    for b in &a.broken {
        println!("      {b}");
    }
    let what = match a.sealed.len() {
        0 => "no landing needed a seal".to_string(),
        1 => "1 landing needed a seal; its Sealed-By trailers verify against the approvers in nerudo.toml at its parent".to_string(),
        k => format!("{k} landings needed seals; their Sealed-By trailers verify against the approvers in nerudo.toml at each parent"),
    };
    let seals = mark(out, a.unsealed.is_empty(), if a.unsealed.is_empty() { what } else { format!("{} landings lack the seals their rules require", a.unsealed.len()) });
    for s in a.sealed.iter().chain(&a.unsealed) {
        println!("      {s}");
    }
    checks && seals
}

/// The sequencer's landing log against trunk: an intact hash chain whose last entry is
/// the trunk tip, with every entry's commit on trunk.
fn check_log(out: &Out, api: &Api, t: &Fetched, stats: &Value) -> bool {
    let log = api.landing_log();
    let chain_ok = verify_chain(GENESIS, &log).is_ok();
    let ends = log.last().map(|e| (e.hash.clone(), e.commit_out.clone())).unwrap_or_default();
    let on_trunk: BTreeSet<String> = t.chain.iter().map(Oid::to_hex).collect();
    let log_ok = chain_ok && ends.0 == stats["log_head_hash"].as_str().unwrap_or_default() && ends.1 == t.tip.to_hex() && log.iter().all(|e| on_trunk.contains(&e.commit_out));
    mark(out, log_ok, format!("landing log: {} entries, hash chain intact, ends at {} = trunk tip {}", log.len(), &ends.0[..12.min(ends.0.len())], short(&t.tip.to_hex())))
}

/// Every release tag on trunk: it points at a trunk commit, its notes match their digest,
/// and its Released-By signatures meet the release quorum in nerudo.toml at that commit.
fn check_releases(out: &Out, remote: &Remote, trunk: &str, t: &Fetched) -> bool {
    use crate::release::{check_tags, Checked};
    let checked = check_tags(&Https, remote, trunk, &t.store, &t.chain);
    let releases: Vec<String> = checked
        .iter()
        .filter_map(|c| match c {
            Checked::Release(r) => Some(format!("{} at {} (landing {}), signed by {}", r.tag, short(&r.commit), r.seq, r.signers.join(", "))),
            _ => None,
        })
        .collect();
    let bad: Vec<String> = checked.iter().filter_map(|c| if let Checked::Bad(n, e) = c { Some(format!("{n}: {e}")) } else { None }).collect();
    let other = checked.iter().filter(|c| matches!(c, Checked::Other)).count();
    if releases.is_empty() && bad.is_empty() {
        return true;
    }
    let good = mark(
        out,
        bad.is_empty(),
        format!(
            "{} release tag{}: signatures verify against the approvers in nerudo.toml at the released commit{}",
            releases.len(),
            if releases.len() == 1 { "" } else { "s" },
            if other > 0 { format!(" ({other} other tags not checked)") } else { String::new() }
        ),
    );
    for r in releases.iter().chain(&bad) {
        println!("      {r}");
    }
    good
}

/// `nerudo verify`: check a trunk from outside with a read-only token.
pub fn verify(api: &Api, trunk: &str) -> bool {
    let info = api.call("GET", "trunk", None);
    let authorization = info["authorization"].as_str().map(str::to_string).or_else(|| crate::env_value("NERUDO_TRUNK_AUTHORIZATION").ok());
    let remote = Remote::new(info["remote"].as_str().unwrap_or_else(|| die("no trunk remote")), authorization);
    let out = Out { t0: Instant::now(), color: std::io::stdout().is_terminal() };
    println!("{}", out.paint("1", &format!("nerudo verify: trunk {trunk}, fetched with a read-only token")));
    let t = fetch_trunk(&remote, info["branch"].as_str().unwrap_or(MAIN));
    let a = audit(&t, trunk);
    let good = print_audit(&out, &t, &a);
    let dupes: Vec<&String> = a.ids.iter().filter(|(_, n)| **n > 1).map(|(id, _)| id).collect();
    let once = mark(&out, dupes.is_empty(), format!("{} landed changes, each on trunk once (Change-Id trailer)", a.ids.len()));
    let pinned = check_context(&out, api, &t);
    let released = check_releases(&out, &remote, trunk, &t);
    let stats = api.call("GET", "stats", None);
    good && once && pinned && released && check_log(&out, api, &t, &stats)
}

/// Check the trunk from the outside: what's on Artifacts, not what the sequencer says.
/// `land`: (seconds from first submit until the agents were done, changes landed by then).
fn check(sh: &Shared, api: &Api, trunk: &str, planned: usize, land: (f64, usize)) {
    sh.out.section("4. Check trunk (fetched from Artifacts with a read-only token)");
    let remote = sh.reader.lock().unwrap().clone().unwrap_or_else(|| die("no trunk reader"));
    let t = fetch_trunk(&remote, MAIN);
    let a = audit(&t, trunk);
    let landed = sh.landed.lock().unwrap().clone();
    let tip_files = git("read tip", t.store.flatten(&git("read tip", t.store.commit(&t.tip)).tree));
    let missing: Vec<&String> = landed.iter().filter(|c| a.ids.get(*c) != Some(&1) || !tip_files.contains_key(&format!("changes/{c}.txt"))).collect();
    let migrations: Vec<&str> = tip_files.keys().filter_map(|k| k.strip_prefix("migrations/")).collect();

    let audited = print_audit(&sh.out, &t, &a);
    mark(&sh.out, missing.is_empty(), format!("{} changes with a landed receipt; each is on trunk exactly once (Change-Id trailer)", landed.len()));
    let numbers: BTreeSet<&str> = migrations.iter().filter_map(|m| m.split('_').next()).collect();
    mark(&sh.out, numbers.len() == migrations.len(), format!("migrations numbered at landing, no duplicates: {}", migrations.join(" ")));
    check_context(&sh.out, api, &t);
    let released = check_releases(&sh.out, &remote, trunk, &t);
    let stats = api.call("GET", "stats", None);
    let log_ok = check_log(&sh.out, api, &t, &stats) && released;
    let (store, chain) = (&t.store, &t.chain);

    println!("\n  trunk, newest first:");
    for c in chain.iter().rev().take(6) {
        let commit = git("read commit", store.commit(c));
        println!("    {}  {}", short(&c.to_hex()), commit.message.lines().next().unwrap_or_default());
    }

    sh.out.section("Numbers");
    let n = |k: &str| stats[k].as_u64().unwrap_or(0);
    let per = |a: u64| if n("landed") > 0 { a as f64 / n("landed") as f64 } else { 0.0 };
    println!(
        "  landed      {} of {planned} in {} batches; {:.0} s from first submit to last agent done ({:.1}/min, human seal excluded)",
        n("landed"),
        n("batches"),
        land.0,
        land.1 as f64 * 60.0 / land.0.max(0.1)
    );
    println!("  CI runs     {} for {} changes ({:.2} per change); agents ran none themselves", n("ci_runs"), n("landed"), per(n("ci_runs")));
    match *sh.ejections.lock().unwrap() {
        0 => println!("  ejected     0 for conflicts"),
        e => println!("  ejected     {e} for conflicts (the receipt named the PR that beat it; the agent rebuilt on the new trunk and resubmitted)"),
    }
    let others = sh.other_ejections.lock().unwrap().clone();
    if !others.is_empty() {
        let kinds: Vec<String> = others.iter().map(|(k, n)| format!("{n} {k}")).collect();
        println!("  closed      {} for other reasons: {}", others.values().sum::<u32>(), kinds.join(", "));
    }
    if n("ejected_ci") > 0 {
        println!(
            "  CI caught   {} change{} that passed on its own base but not on trunk (the agent rebuilt it on the new trunk)",
            n("ejected_ci"),
            if n("ejected_ci") == 1 { "" } else { "s" }
        );
    }
    println!("  git calls   {} by the sequencer ({:.1} per landed change)", n("git_requests"), per(n("git_requests")));
    if n("recovered_attempts") > 0 {
        println!("  recovered   {} push the sequencer crashed after; reconciled from trunk, nothing landed twice", n("recovered_attempts"));
    }
    if !audited || !missing.is_empty() || !log_ok {
        die("trunk check failed");
    }
}

#[cfg(test)]
mod tests {
    use super::add_currency;

    #[test]
    fn add_currency_adds_the_argument_once() {
        assert_eq!(add_currency("x = price(250);\n"), "x = price(250, 'EUR');\n");
        assert_eq!(add_currency("f(price(n * (1 + 2)))"), "f(price(n * (1 + 2), 'EUR'))");
        assert_eq!(add_currency("price(1, 'EUR') + price(2)"), "price(1, 'EUR') + price(2, 'EUR')");
        assert_eq!(add_currency("import { price } from './price';\n"), "import { price } from './price';\n");
        assert_eq!(add_currency("price(oops"), "price(oops");
    }
}
