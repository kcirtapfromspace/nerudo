//! `nerudo mcp`: nerudo as tools for coding agents. It is an MCP server on stdio, so
//! Claude Code, Codex, Antigravity, Cursor and any other MCP client can use it. It runs
//! where the agent's checkout is: `land` pushes the agent's commit to the agent's own fork,
//! asks the sequencer to land it, and returns the receipt. The agent never pushes to trunk
//! and never opens a pull request.
//!
//! Environment: NERUDO_WORKER, NERUDO_TOKEN (or NERUDO_AGENT_TOKEN), NERUDO_TRUNK, and
//! optionally NERUDO_AGENT (the agent's fork name; default: the MCP client's name) and
//! NERUDO_REPO (the checkout; default: the current directory). `--trunk` and `--agent`
//! override the environment. The trunk can be a trunk id or a public repo's name, ORG/REPO.
//!
//! The first fork of an agent comes with the agent's own key. The server keeps it in
//! ~/.nerudo/agents/<trunk>/<agent> (owner-only) and sends it with each request, so the
//! sequencer knows which agent asks. NERUDO_AGENT_KEY overrides the file. An agent whose
//! fork exists has to send its key to get its fork again; without it, the admin runs
//! `nerudo agent-key --trunk T --agent A`.
//!
//! git needs no stored credentials: each git call gets the fork or trunk token through
//! GIT_CONFIG_* environment variables, never on its command line, and with credential
//! helpers cleared so that no token is saved.
//!
//! For a client that can show the person a form (MCP elicitation), `request_approval` asks the
//! person to approve a PR that waits for a seal. Only the person answers, and the seal is the one
//! `nerudo seal` signs (see the end of this file).

use crate::context;
use crate::Api;
use nerudo_core::{Rules, RULES_PATH};
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, Write};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// MCP revisions this server speaks; it answers with the client's revision when it knows it.
const PROTOCOLS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
/// Where `sync` puts trunk in the agent's checkout: `nerudo/main`.
const TRUNK_REF: &str = "refs/remotes/nerudo/main";

const INSTRUCTIONS: &str = "nerudo lands your commits on trunk. You never push to trunk and never open a pull request: \
the nerudo sequencer is the only writer of trunk, and it lands changes from many agents in batches, with one CI run per batch.

How to work:
0. Call `tasks`. Work comes as tasks (like T-12): what should change and why. `claim` the one you take, and pass \
its id as `task` to `land`; its landing closes the task. A task can ask for competing attempts: several agents each \
take one, and a person compares them and picks the one that ships. Do your best attempt alone; don't copy another's.
1. Call `sync`. Start each change from trunk: `git switch -c <change-name> nerudo/main`.
2. Call `rules` once. It tells you which files this trunk merges by rule, generates, numbers at landing, or protects with a human seal.
3. Call `in_flight`. It shows what the other agents' waiting changes touch. Prefer work that does not overlap.
4. Make one commit for the change. Put why in the commit message body: it lands on trunk with your change.
5. Call `land`. It pushes the commit to your own fork and waits for the receipt. Pass `context`: your plan, the \
decisions you made, the alternatives you rejected and why, and the task in your own words. It is kept, versioned, on \
your fork's nerudo/context branch, and the landed commit links to it, so the next agent knows what you were thinking. \
Never put secrets in it.

Before you change code that another agent changed recently, call `context` with that change's id: it shows the plan \
and decisions that agent recorded.

If the change is closed for a conflict, or ejected by CI, the receipt names the PRs that landed first and shows their \
plan. Call `sync`, rebuild your change on the new trunk (`git rebase nerudo/main`), keep their decision unless your task \
says otherwise, and call `land` again with the same change_id. If it is parked for a human seal, stop and tell the user: \
a human approver must run the `nerudo seal` command that `land` shows. Do not try to work around a seal.

Talk on the change, not around it: `comment` adds to a change's conversation, and `@name` or `@team` there notifies \
people (teams are the roles in nerudo.toml, for example @dba). Ask for a review with `request_review`. Call `mentions` \
to see who mentioned you or asked you for a review, and answer them with `comment`. `react` adds an emoji.";

#[derive(Clone)]
struct Fork {
    name: String,
    remote: String,
    auth: String,
    trunk: String,
    trunk_auth: String,
}

struct Server {
    trunk: String,
    agent: Option<String>,
    client: String,
    repo: String,
    fork: Option<Fork>,
    branch: Option<String>,
    /// Lines from the client. A thread reads stdin, so a tool can wait for an answer with a
    /// timeout.
    input: Receiver<String>,
    /// Messages that came while a tool waited for the person. They are handled next, in order.
    later: VecDeque<String>,
    /// Replies and requests to the client: stdout.
    out: Box<dyn Write>,
    ask: Ask,
    /// The agent's key, once the fork response gave it.
    key: Option<String>,
}

/// State for `request_approval`.
struct Ask {
    /// The client can show a form to the person: it declared MCP elicitation, and the
    /// protocol revision has it (2025-06-18 and later).
    form: bool,
    /// The number of the last request this server sent to the client.
    sent: u64,
    /// The PR with an open approval request.
    open: Option<String>,
    /// The id of the tools/call that is running, so that its cancellation stops a wait.
    call: Value,
    /// How long to wait for the person's answer.
    wait: Duration,
}

/// How long `request_approval` waits for the person.
const APPROVAL_WAIT: Duration = Duration::from_secs(300);
/// How long a seal from `request_approval` is good for, in minutes (as `nerudo seal` by default).
const APPROVAL_TTL_MIN: u32 = 30;

pub fn serve(f: &BTreeMap<String, String>) {
    let env = |k: &str| crate::env_value(k).ok().filter(|v| !v.is_empty());
    let (tx, input) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut s = Server {
        trunk: f.get("trunk").cloned().or_else(|| env("NERUDO_TRUNK").map(|t| crate::trunk_id(&t))).unwrap_or_default(),
        agent: f.get("agent").cloned().or_else(|| env("NERUDO_AGENT")),
        client: "agent".into(),
        repo: env("NERUDO_REPO").unwrap_or_else(|| ".".into()),
        fork: None,
        branch: None,
        input,
        later: VecDeque::new(),
        out: Box::new(std::io::stdout()),
        ask: Ask { form: false, sent: 0, open: None, call: Value::Null, wait: APPROVAL_WAIT },
        key: None,
    };
    s.run();
}

impl Server {
    /// Handle each message from the client, until it closes stdin. Messages that came during
    /// a wait go first.
    fn run(&mut self) {
        while let Some(line) = self.later.pop_front().or_else(|| self.input.recv().ok()) {
            if line.trim().is_empty() {
                continue;
            }
            let reply = match serde_json::from_str::<Value>(&line) {
                Ok(msg) => self.handle(&msg),
                Err(e) => Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": format!("parse error: {e}") } })),
            };
            if let Some(r) = reply {
                self.send(&r);
            }
        }
    }

    fn send(&mut self, msg: &Value) {
        let _ = writeln!(self.out, "{msg}");
        let _ = self.out.flush();
    }

    fn handle(&mut self, msg: &Value) -> Option<Value> {
        let method = msg["method"].as_str()?; // a response from the client: nothing to do
        let result = match method {
            "initialize" => Ok(self.initialize(&msg["params"])),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": self.listed_tools() })),
            "tools/call" => {
                self.ask.call = msg["id"].clone();
                Ok(self.call(&msg["params"]))
            }
            m if m.starts_with("notifications/") => return None,
            _ => Err(format!("method not found: {method}")),
        };
        let id = msg.get("id")?.clone();
        Some(match result {
            Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err(m) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": m } }),
        })
    }

    fn initialize(&mut self, p: &Value) -> Value {
        if let Some(name) = p["clientInfo"]["name"].as_str() {
            self.client = name.to_string();
        }
        let asked = p["protocolVersion"].as_str().unwrap_or(PROTOCOLS[1]);
        let version = if PROTOCOLS.contains(&asked) { asked } else { PROTOCOLS[1] };
        self.ask.form = can_show_form(version, &p["capabilities"]);
        let instructions = if self.ask.form { format!("{INSTRUCTIONS}\n\n{APPROVAL_NOTE}") } else { INSTRUCTIONS.to_string() };
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "nerudo", "version": env!("CARGO_PKG_VERSION") },
            "instructions": instructions,
        })
    }

    fn call(&mut self, p: &Value) -> Value {
        let a = p.get("arguments").cloned().unwrap_or(json!({}));
        let r = match p["name"].as_str().unwrap_or("") {
            "land" => self.land(&a),
            "in_flight" => self.in_flight(),
            "status" => self.status(),
            "receipt" => self.receipt(&a),
            "sync" => self.sync(&a),
            "rules" => self.rules(&a),
            "comment" => self.comment(&a),
            "mentions" => self.mentions(),
            "request_review" => self.request_review(&a),
            "react" => self.react(&a),
            "cleanup" => self.cleanup(),
            "tasks" => self.tasks(),
            "claim" => self.claim(&a),
            "new_task" => self.new_task(&a),
            "context" => self.context(&a),
            "release_propose" => self.release_propose(&a),
            "request_approval" => self.request_approval(&a),
            other => Err(format!("unknown tool {other:?}")),
        };
        let (text, error) = match r {
            Ok(t) => (t, false),
            Err(e) => (e, true),
        };
        json!({ "content": [{ "type": "text", "text": text }], "isError": error })
    }

    // ------------------------------------------------------------------ plumbing

    fn api(&self) -> Result<Api, String> {
        if self.trunk.is_empty() {
            return Err("no trunk: set NERUDO_TRUNK in the MCP server's environment".into());
        }
        let token = ["NERUDO_TOKEN", "NERUDO_AGENT_TOKEN"]
            .iter()
            .find_map(|k| crate::env_value(k).ok().filter(|v| !v.is_empty()))
            .ok_or("no token: set NERUDO_TOKEN in the MCP server's environment")?;
        Ok(Api::with_token(&self.trunk, token).with_agent_key(self.agent_key()))
    }

    /// The agent's key: from the fork response, else NERUDO_AGENT_KEY, else the key file.
    fn agent_key(&self) -> Option<String> {
        self.key
            .clone()
            .or_else(|| crate::env_value("NERUDO_AGENT_KEY").ok().filter(|k| !k.is_empty()))
            .or_else(|| crate::load_agent_key(&self.trunk, &self.agent_name()))
    }

    fn agent_name(&self) -> String {
        let raw = self.agent.clone().unwrap_or_else(|| self.client.clone());
        let name: String = raw.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
        if name.is_empty() { "agent".into() } else { name }
    }

    /// The repo a tool works in: the one this server was started for, or a directory under
    /// it. Any other path is refused: a tool must not run git, or land a commit, in a
    /// checkout the user did not start the server in.
    fn repo(&self, a: &Value) -> Result<String, String> {
        let Some(given) = a["repo"].as_str().filter(|r| !r.is_empty()) else {
            return Ok(self.repo.clone());
        };
        let root = std::fs::canonicalize(&self.repo).map_err(|e| format!("{}: {e}", self.repo))?;
        let path = std::fs::canonicalize(given).map_err(|e| format!("repo {given:?}: {e}"))?;
        if !path.starts_with(&root) {
            return Err(format!("repo {given:?} is not under {}, the repo this server was started in", root.display()));
        }
        Ok(path.to_string_lossy().into_owned())
    }

    /// The agent's fork: created on first use, then reused.
    fn fork(&mut self) -> Result<Fork, String> {
        if let Some(f) = &self.fork {
            return Ok(f.clone());
        }
        let agent = self.agent_name();
        let f = match self.api()?.try_call("POST", "forks", Some(json!({ "agent": agent }))) {
            Ok(f) => f,
            Err(e) if e.starts_with("HTTP 403") => {
                // Mostly a fork from before agent keys, or a key that was rotated: say what to do.
                let why = e.split_once(": ").and_then(|(_, b)| serde_json::from_str::<Value>(b).ok()).and_then(|v| v["error"].as_str().map(str::to_string)).unwrap_or(e);
                let msg = format!(
                    "The sequencer refused the fork of agent {agent}: {why}\n\
                     nerudo mcp looks for the key of {agent} in NERUDO_AGENT_KEY, then in {}. \
                     `nerudo agent-key --trunk {} --agent {agent}` (admin) prints the key and saves it in that file on the machine where it runs. \
                     Tell the user; then call this tool again.",
                    crate::agent_key_path(&self.trunk, &agent).display(),
                    self.trunk
                );
                eprintln!("nerudo mcp: {msg}");
                return Err(msg);
            }
            Err(e) => return Err(e),
        };
        if crate::env_value("NERUDO_AGENT_KEY").ok().filter(|k| !k.is_empty()).is_none() {
            self.key = crate::keep_agent_key(&self.trunk, &agent, &f);
        } else {
            self.key = f["agent_key"].as_str().map(str::to_string);
        }
        let s = |k: &str| f[k].as_str().map(str::to_string).ok_or_else(|| format!("the fork response has no {k}"));
        let fork = Fork {
            name: s("name")?,
            remote: s("remote")?,
            auth: s("authorization")?,
            trunk: s("trunk")?,
            trunk_auth: f["trunk_authorization"].as_str().unwrap_or_default().to_string(),
        };
        self.fork = Some(fork.clone());
        Ok(fork)
    }

    fn branch(&mut self) -> Result<String, String> {
        if let Some(b) = &self.branch {
            return Ok(b.clone());
        }
        let info = self.api()?.try_call("GET", "trunk", None)?;
        let b = info["branch"].as_str().unwrap_or("refs/heads/main").to_string();
        self.branch = Some(b.clone());
        Ok(b)
    }

    /// Run git in `repo`. `auth` is an Authorization header value for the remote.
    fn git(&self, repo: &str, args: &[&str], auth: Option<&str>) -> Result<String, String> {
        let mut c = Command::new("git");
        c.current_dir(repo).args(args).env("GIT_TERMINAL_PROMPT", "0");
        let mut config = vec![("credential.helper".to_string(), String::new())];
        if let Some(a) = auth.filter(|a| !a.is_empty()) {
            config.push(("http.extraHeader".into(), format!("Authorization: {a}")));
        }
        c.env("GIT_CONFIG_COUNT", config.len().to_string());
        for (i, (k, v)) in config.iter().enumerate() {
            c.env(format!("GIT_CONFIG_KEY_{i}"), k).env(format!("GIT_CONFIG_VALUE_{i}"), v);
        }
        let out = c.output().map_err(|e| format!("cannot run git: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            Err(format!("git {}: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&out.stderr).trim()))
        }
    }

    /// Fetch trunk into `nerudo/main` in the checkout; returns the trunk tip.
    fn fetch_trunk(&mut self, repo: &str) -> Result<String, String> {
        let f = self.fork()?;
        let branch = self.branch()?;
        self.git(repo, &["fetch", "--quiet", &f.trunk, &format!("+{branch}:{TRUNK_REF}")], Some(&f.trunk_auth))?;
        self.git(repo, &["rev-parse", TRUNK_REF], None)
    }

    // ------------------------------------------------------------------ tools

    fn sync(&mut self, a: &Value) -> Result<String, String> {
        let repo = self.repo(a)?;
        let tip = self.fetch_trunk(&repo)?;
        let here = match self.git(&repo, &["rev-list", "--left-right", "--count", &format!("HEAD...{TRUNK_REF}")], None) {
            Ok(counts) => {
                let (ahead, behind) = counts.split_once('\t').unwrap_or(("0", "0"));
                format!(
                    "Your HEAD has {} that trunk does not, and trunk has {} that HEAD does not. \
                     A landed change is on trunk as a new commit (with the sequencer's trailers), so your own copy of it counts as ahead.",
                    plural(ahead, "commit"),
                    plural(behind, "commit")
                )
            }
            Err(_) => "Your checkout has no commit yet.".into(),
        };
        Ok(format!(
            "Trunk {} is at {} (nerudo/main in your checkout). {here}\n\
             Start a new change with `git switch -c <change-name> nerudo/main`; rebuild an ejected one with `git rebase nerudo/main`.",
            self.trunk,
            short(&tip)
        ))
    }

    fn rules(&mut self, a: &Value) -> Result<String, String> {
        let repo = self.repo(a)?;
        self.fetch_trunk(&repo)?;
        let text = self.git(&repo, &["show", &format!("{TRUNK_REF}:{RULES_PATH}")], None)
            .or_else(|_| self.git(&repo, &["show", &format!("{TRUNK_REF}:{}", nerudo_core::rules::LEGACY_RULES_PATH)], None))?;
        let r = Rules::parse(&text)?;
        let mut out = vec![format!("Rules of trunk {} ({RULES_PATH} on trunk; you cannot change them without a human seal):", self.trunk)];
        if !r.footprint.units.is_empty() {
            out.push(format!(
                "- Footprint units: {}. Changes in different units land in the same batch; changes in the same unit wait in order.",
                r.footprint.units.join(", ")
            ));
        }
        for c in &r.commutative {
            out.push(format!("- {}: add your lines freely; the sequencer merges appends from all agents, so it never conflicts.", c.path));
        }
        for d in &r.derived {
            out.push(format!("- {}: generated from {}/. Do not edit it by hand; the sequencer generates it again when your change lands.", d.path, d.source));
        }
        for al in &r.allocate {
            out.push(format!(
                "- {}/: name new files {}<name> (for example {}/{}add_users.sql), never with a number. The sequencer numbers them when they land.",
                al.dir, al.pending_prefix, al.dir, al.pending_prefix
            ));
        }
        if !r.seals.governance.is_empty() {
            out.push(format!("- Needs a human seal: edits to {}.", r.seals.governance.join(", ")));
        }
        if !r.seals.migrations.is_empty() {
            out.push(format!("- Needs a human seal: destructive SQL (drop, delete, truncate) in {}.", r.seals.migrations.join(", ")));
        }
        if let Some(t) = &r.ci.typecheck {
            out.push(format!("- CI type-checks the TypeScript with tsc ({}) on trunk with your change. Run your own type check first if you can.", t.tsconfig));
        }
        if r.allowances.required {
            out.push("- Ordinary changes need a human's allowance: a budget of landings. land tells you when the budget is spent.".into());
        }
        Ok(out.join("\n"))
    }

    /// Waiting changes, with what they touch and the first line of their plan, then the tasks
    /// that agents claimed and work on now. At most IN_FLIGHT of each.
    fn in_flight(&mut self) -> Result<String, String> {
        const IN_FLIGHT: usize = 10;
        let mine = self.fork().map(|f| f.name).unwrap_or_default();
        let api = self.api()?;
        let q = api.try_call("GET", "queue", None)?;
        let changes = q["changes"].as_array().cloned().unwrap_or_default();
        let mut out = vec![];
        if changes.is_empty() {
            out.push(format!("Nothing is waiting to land on trunk {}. Trunk is at {}.", self.trunk, short(q["head"].as_str().unwrap_or("?"))));
        } else {
            out.push(format!("{} changes are waiting to land on trunk {}:", changes.len(), self.trunk));
        }
        for c in changes.iter().take(IN_FLIGHT) {
            let units: Vec<&str> = c["footprint"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            let who = c["fork"].as_str().unwrap_or("?");
            let cid = c["change_id"].as_str().unwrap_or("?");
            out.push(format!(
                "- {}{cid} rev {} ({}){}: touches {}",
                c["number"].as_u64().map(|n| format!("#{n} ")).unwrap_or_default(),
                c["rev"],
                c["state"].as_str().unwrap_or("?"),
                if who == mine { ", yours" } else { "" },
                if units.is_empty() { "(not known yet)".to_string() } else { units.join(", ") }
            ));
            // The first line of its plan, when the agent sent context with it.
            let plan = api.try_call("GET", &format!("context/{cid}?rev={}", c["rev"]), None).ok().and_then(|x| x["text"].as_str().map(|t| context::plan_lines(t, 1)));
            if let Some(line) = plan.and_then(|l| l.into_iter().next()) {
                out.push(format!("  plan: {line}"));
            }
        }
        if changes.len() > IN_FLIGHT {
            out.push(format!("…and {} more.", changes.len() - IN_FLIGHT));
        }
        if !changes.is_empty() {
            out.push("Work that touches the same units waits behind these changes.".into());
        }
        let tasks = api.try_call("GET", "tasks", None).ok().and_then(|t| t.as_array().cloned()).unwrap_or_default();
        let claimed: Vec<&Value> = tasks
            .iter()
            .filter(|t| t["status"] == "in_progress" && (t["assignee"].is_string() || t["attempting"].as_array().is_some_and(|a| !a.is_empty())))
            .collect();
        if !claimed.is_empty() {
            out.push(format!("{} claimed tasks are in progress:", claimed.len()));
            for t in claimed.iter().take(IN_FLIGHT) {
                let by: Vec<&str> = match t["attempting"].as_array().filter(|a| !a.is_empty()) {
                    Some(a) => a.iter().filter_map(Value::as_str).collect(),
                    None => t["assignee"].as_str().into_iter().collect(),
                };
                out.push(format!("- {} {} (claimed by {})", t["id"].as_str().unwrap_or("?"), t["title"].as_str().unwrap_or(""), by.join(", ")));
            }
            if claimed.len() > IN_FLIGHT {
                out.push(format!("…and {} more.", claimed.len() - IN_FLIGHT));
            }
        }
        Ok(out.join("\n"))
    }

    fn status(&mut self) -> Result<String, String> {
        let api = self.api()?;
        let info = api.try_call("GET", "trunk", None)?;
        let stats = api.try_call("GET", "stats", None)?;
        let n = |k: &str| stats[k].as_u64().unwrap_or(0);
        let seq = n("seq");
        let log = api.try_call("GET", &format!("log?since={}", seq.saturating_sub(8)), None)?;
        let mut out = vec![
            format!("Trunk {} is at {}.", self.trunk, short(info["head"].as_str().unwrap_or("?"))),
            format!(
                "Landed {} in {} with {}. Ejected: {} conflicts, {} by CI, {} other. Waiting for a human: {} seals, {} allowances.",
                plural(&n("landed").to_string(), "change"),
                plural(&n("batches").to_string(), "batch"),
                plural(&n("ci_runs").to_string(), "CI run"),
                n("ejected_conflict"),
                n("ejected_ci"),
                n("ejected_check") + n("ejected_other"),
                n("awaiting_seal"),
                n("awaiting_allowance")
            ),
            "Latest landings:".into(),
        ];
        for e in log.as_array().into_iter().flatten().rev() {
            out.push(format!(
                "- seq {} {} rev {}, batch {}, trunk commit {}",
                e["seq"],
                e["change_id"].as_str().unwrap_or("?"),
                e["rev"],
                e["batch"],
                short(e["commit_out"].as_str().unwrap_or("?"))
            ));
        }
        Ok(out.join("\n"))
    }

    fn receipt(&mut self, a: &Value) -> Result<String, String> {
        let cid = valid_id(a["change_id"].as_str().ok_or("change_id is required")?, "change_id")?;
        let path = match a["rev"].as_u64() {
            Some(r) => format!("changes/{cid}?rev={r}"),
            None => format!("changes/{cid}"),
        };
        let api = self.api()?;
        let rec = api.try_call("GET", &path, None)?;
        Ok(describe_with(&self.trunk, &rec, Some(&api)))
    }

    fn land(&mut self, a: &Value) -> Result<String, String> {
        let repo = self.repo(a)?;
        let head = self.git(&repo, &["rev-parse", "HEAD"], None)?;
        let tip = self.fetch_trunk(&repo)?;
        let base = self
            .git(&repo, &["merge-base", "HEAD", TRUNK_REF], None)
            .map_err(|_| "HEAD shares no history with trunk. Call sync, then start from nerudo/main.".to_string())?;
        if base == head {
            return Err("Nothing to land: HEAD is already on trunk. Commit your change first.".into());
        }
        let cid = match a["change_id"].as_str() {
            Some(c) => c.to_string(),
            None => self.default_change_id(&repo, &head),
        };
        valid_id(&cid, "change_id")?;
        if cid.contains("..") {
            return Err(format!("change_id {cid:?} has \"..\", which git refuses in a ref name. A git push opened that PR: push its branch with git to send a new revision."));
        }
        let api = self.api()?;
        // A merged change takes no new revision; a new commit for a waiting change is its next
        // revision, and replaces the waiting one (cf/scale/backlog.md, T-8 and T-9).
        let mut replaces = None;
        let rev = match api.try_call("GET", &format!("changes/{cid}"), None) {
            Ok(rec) if !rec.is_null() => match rec["state"].as_str() {
                Some("landed") => return Err(format!("{cid} merged as seq {}. A merged change takes no new revision: land your new work with a new change_id.", rec["seq"])),
                Some("ejected") => rec["req"]["rev"].as_u64().unwrap_or(1) + 1,
                Some(_) if rec["req"]["commit"].as_str() == Some(head.as_str()) => {
                    return Ok(format!("{cid} is already waiting with this commit:\n{}", describe_with(&self.trunk, &rec, Some(&api))))
                }
                Some(_) => {
                    replaces = rec["req"]["rev"].as_u64();
                    replaces.unwrap_or(0) + 1
                }
                None => 1,
            },
            _ => 1,
        };
        let f = self.fork()?;
        let task = a["task"].as_str().filter(|i| !i.is_empty());
        // The context goes to the fork's nerudo/context branch in the same push as the change.
        let mut refspecs = vec![format!("+HEAD:refs/heads/land/{cid}-{rev}")];
        let mut context_commit = None;
        if let Some(text) = a["context"].as_str().filter(|t| !t.trim().is_empty()) {
            let at = crate::now_ms();
            let meta = context::Meta { change_id: &cid, rev: rev as u32, agent: &self.agent_name(), task, commit: &head, base: &base, at_ms: at };
            let secrets = self.secrets(&f);
            let doc = context::document(&meta, text, &secrets);
            let c = context::commit_with_git(&repo, &f.remote, &f.auth, &self.agent_name(), &context::path(&cid, rev as u32), &doc, at)?;
            refspecs.push(format!("{c}:{}", context::BRANCH));
            context_commit = Some(c);
        }
        let mut push = vec!["push", "--quiet", f.remote.as_str()];
        push.extend(refspecs.iter().map(String::as_str));
        self.git(&repo, &push, Some(&f.auth))?;
        let mut req = json!({ "change_id": cid, "rev": rev, "fork": f.name, "commit": head, "base": base });
        if let Some(task) = task {
            req["task"] = json!(task);
        }
        if let Some(c) = &context_commit {
            req["context_commit"] = json!(c);
        }
        if let Err(e) = api.try_call("POST", "land", Some(req)) {
            // A refusal of the revision says why in words (merged, merging, older): give those.
            let words = e.strip_prefix("HTTP 409: ").and_then(|b| serde_json::from_str::<Value>(b).ok()).and_then(|v| v["error"].as_str().map(str::to_string));
            return Err(words.unwrap_or(e));
        }
        let wait = a["wait_s"].as_u64().unwrap_or(50).min(600);
        let end = Instant::now() + Duration::from_secs(wait);
        let rec = loop {
            let left = end.saturating_duration_since(Instant::now()).as_secs();
            let rec = api.try_call("GET", &format!("changes/{cid}?rev={rev}&wait={}", left.clamp(1, 30)), None)?;
            if rec["state"] != "queued" || left == 0 {
                break rec;
            }
        };
        let from = if base == tip { String::new() } else { format!(" It was built on {}, which is behind trunk {}.", short(&base), short(&tip)) };
        let kept = match &context_commit {
            Some(c) => format!(" Your context is kept as {} on your fork's nerudo/context branch.", short(c)),
            None => String::new(),
        };
        let replaced = match replaces {
            Some(r) => format!(" It replaces rev {r}, which was waiting and does not merge."),
            None => String::new(),
        };
        Ok(format!("Asked to land {} as {cid} rev {rev}.{replaced}{from}{kept}\n{}", short(&head), describe_with(&self.trunk, &rec, Some(&api))))
    }

    /// Every credential this process holds, so that none of them can end up in a context file.
    fn secrets(&self, f: &Fork) -> Vec<String> {
        let mut s: Vec<String> = std::env::vars().filter(|(k, _)| {
            (k.starts_with("NERUDO_") || k.starts_with("LANDSEQ_")) && (k.ends_with("TOKEN") || k.ends_with("KEY"))
        }).map(|(_, v)| v).collect();
        s.extend(self.agent_key());
        for a in [&f.auth, &f.trunk_auth] {
            s.push(a.clone());
            if let Some(t) = a.strip_prefix("Bearer ") {
                s.push(t.to_string());
            }
        }
        s.retain(|x| x.len() >= 8);
        s
    }

    /// What another agent (or this one) recorded about a change: its plan and decisions.
    fn context(&mut self, a: &Value) -> Result<String, String> {
        let cid = valid_id(a["change_id"].as_str().ok_or("change_id is required")?, "change_id")?;
        let path = match a["rev"].as_u64() {
            Some(r) => format!("context/{cid}?rev={r}"),
            None => format!("context/{cid}"),
        };
        Ok(context::describe(&self.api()?.try_call("GET", &path, None)?))
    }

    fn comment(&mut self, a: &Value) -> Result<String, String> {
        let cid = a["change_id"].as_str().ok_or("change_id is required")?;
        let body = a["body"].as_str().ok_or("body is required")?;
        self.fork()?; // an agent speaks under the name of its fork
        let c = self.api()?.try_call("POST", "comments", Some(json!({ "change_id": cid, "body": body, "as": self.agent_name() })))?;
        Ok(format!("Commented on {cid} as {} (comment {}).", self.agent_name(), c["id"].as_str().unwrap_or("?")))
    }

    fn mentions(&mut self) -> Result<String, String> {
        self.fork()?;
        let me = self.agent_name();
        let notes = self.api()?.try_call("GET", &format!("notes?who={me}"), None)?;
        let notes = notes.as_array().cloned().unwrap_or_default();
        if notes.is_empty() {
            return Ok(format!("Nobody has mentioned {me} or asked it for a review on trunk {}.", self.trunk));
        }
        let mut out = vec![format!("For {me} on trunk {}, newest first:", self.trunk)];
        for n in notes.iter().take(20) {
            let what = match n["kind"].as_str().unwrap_or("") {
                "review_request" => "asked you for a review of".to_string(),
                "team_mention" => format!("mentioned @{} on", n["team"].as_str().unwrap_or("?")),
                _ => "mentioned you on".to_string(),
            };
            let text = n["text"].as_str().filter(|t| !t.is_empty()).map(|t| format!(": {t}")).unwrap_or_default();
            out.push(format!("- {} {what} {}{text}", n["from"].as_str().unwrap_or("?"), n["change_id"].as_str().unwrap_or("?")));
        }
        out.push("Answer with comment on that change.".into());
        Ok(out.join("\n"))
    }

    fn request_review(&mut self, a: &Value) -> Result<String, String> {
        let cid = a["change_id"].as_str().ok_or("change_id is required")?;
        let reviewer = a["reviewer"].as_str().ok_or("reviewer is required")?;
        self.fork()?;
        let r = self.api()?.try_call("POST", "reviews", Some(json!({ "change_id": cid, "reviewer": reviewer, "as": self.agent_name() })))?;
        Ok(format!("Asked {} to review {cid}. They see it in their inbox.", r["requested"].as_str().unwrap_or(reviewer)))
    }

    fn react(&mut self, a: &Value) -> Result<String, String> {
        let cid = a["change_id"].as_str().ok_or("change_id is required")?;
        let emoji = a["emoji"].as_str().ok_or("emoji is required")?;
        self.fork()?;
        let mut body = json!({ "change_id": cid, "emoji": emoji, "as": self.agent_name() });
        if let Some(c) = a["comment"].as_str() {
            body["comment"] = json!(c);
        }
        let r = self.api()?.try_call("POST", "reactions", Some(body))?;
        let on = r["on"].as_bool().unwrap_or(false);
        Ok(format!("{} {} on {cid}.", if on { "Added" } else { "Took back" }, r["emoji"].as_str().unwrap_or(emoji)))
    }

    fn tasks(&mut self) -> Result<String, String> {
        self.fork()?;
        let me = self.agent_name();
        let list = self.api()?.try_call("GET", &format!("tasks?for={me}"), None)?;
        let list = list.as_array().cloned().unwrap_or_default();
        if list.is_empty() {
            return Ok(format!("No open tasks on trunk {}.", self.trunk));
        }
        let mut out = vec![format!("Open tasks on trunk {}, yours first:", self.trunk)];
        for i in list.iter().take(30) {
            let who = i["assignee"].as_str().map(|a| if a == me { "you".to_string() } else { a.to_string() }).unwrap_or_else(|| "nobody".into());
            let prio = ["", " (urgent)", " (high)", " (medium)", " (low)"].get(i["priority"].as_u64().unwrap_or(0) as usize).copied().unwrap_or("");
            out.push(format!("- {} [{}]{prio} {} (assigned to {who})", i["id"].as_str().unwrap_or("?"), i["status"].as_str().unwrap_or("?").replace('_', " "), i["title"].as_str().unwrap_or("")));
            let attempts = i["attempts"].as_u64().unwrap_or(1);
            if attempts >= 2 {
                let taken: Vec<&str> = i["attempting"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                let pick = i["pick"].as_str().filter(|p| !p.is_empty()).unwrap_or("person");
                out.push(format!(
                    "  wants {attempts} competing attempts ({} taken{}); {}",
                    taken.len(),
                    if taken.is_empty() { String::new() } else { format!(": {}", taken.join(", ")) },
                    if pick == "person" { "a person picks one".to_string() } else { format!("the {pick} policy picks one") }
                ));
            }
            if let Some(body) = i["body"].as_str().filter(|b| !b.is_empty()) {
                let short: String = body.chars().take(300).collect();
                out.push(format!("  {}", short.replace('\n', " ")));
            }
        }
        out.push("Claim one with claim, then land your change with task set to its id.".into());
        Ok(out.join("\n"))
    }

    fn claim(&mut self, a: &Value) -> Result<String, String> {
        let id = valid_id(a["task"].as_str().ok_or("task is required")?, "task")?;
        self.fork()?;
        let i = self.api()?.try_call("POST", &format!("tasks/{id}/claim"), Some(json!({ "as": self.agent_name() })))?;
        let attempts = i["attempts"].as_u64().unwrap_or(1);
        if attempts >= 2 {
            let me = self.agent_name();
            let k = i["attempting"].as_array().into_iter().flatten().position(|a| a.as_str() == Some(me.as_str())).map(|p| p + 1).unwrap_or(0);
            return Ok(format!(
                "You took attempt {k} of {attempts} on {id}: {}\n{}\nOther agents attempt it too. Land your attempt with task: {id}; \
                 it gets a trial on trunk and waits until a person picks one attempt to ship.",
                i["title"].as_str().unwrap_or(""),
                i["body"].as_str().unwrap_or("")
            ));
        }
        Ok(format!(
            "{id} is yours and in progress: {}\n{}\nWhen you land the change, pass task: {id}.",
            i["title"].as_str().unwrap_or(""),
            i["body"].as_str().unwrap_or("")
        ))
    }

    fn new_task(&mut self, a: &Value) -> Result<String, String> {
        let title = a["title"].as_str().ok_or("title is required")?;
        self.fork()?;
        let mut body = json!({ "title": title, "body": a["body"].as_str().unwrap_or(""), "as": self.agent_name() });
        if let Some(p) = a["priority"].as_u64() {
            body["priority"] = json!(p);
        }
        for k in ["attempts", "pick"] {
            if !a[k].is_null() {
                body[k] = a[k].clone();
            }
        }
        let i = self.api()?.try_call("POST", "tasks", Some(body))?;
        Ok(format!("Wrote {}: {}. People see it in the app.", i["id"].as_str().unwrap_or("?"), i["title"].as_str().unwrap_or("")))
    }

    /// Ask the approvers for a release. They see it in their inbox and sign it with
    /// `nerudo release`; agents never sign.
    fn release_propose(&mut self, a: &Value) -> Result<String, String> {
        self.fork()?; // the agent proposes with its key
        let mut body = json!({ "by": self.agent_name() });
        for k in ["version", "title", "notes"] {
            if let Some(v) = a[k].as_str().filter(|v| !v.is_empty()) {
                body[k] = json!(v);
            }
        }
        let r = self.api()?.try_call("POST", "releases/propose", Some(body))?;
        let told: Vec<&str> = r["notified"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        Ok(format!(
            "Proposed {} at landing {} ({} PRs). {} can sign it with `nerudo release --trunk {}`; the sequencer cuts the tag then.",
            r["proposed"].as_str().unwrap_or("?"),
            r["seq"],
            r["prs"],
            if told.is_empty() { "An approver".to_string() } else { told.join(", ") },
            self.trunk
        ))
    }

    fn cleanup(&mut self) -> Result<String, String> {
        self.fork()?;
        let r = self.api()?.try_call("POST", "cleanup", Some(json!({ "as": self.agent_name() })))?;
        Ok(format!("Deleted {} stale refs from your fork: their PRs merged or closed, or a later revision replaced them.", r["deleted"]))
    }

    /// The branch name, when it names the change; else the agent and the commit.
    fn default_change_id(&self, repo: &str, head: &str) -> String {
        let branch = self.git(repo, &["symbolic-ref", "--short", "-q", "HEAD"], None).unwrap_or_default();
        let clean: String = branch.chars().map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '-' }).take(64).collect();
        if !clean.is_empty() && !["main", "master", "trunk"].contains(&clean.as_str()) {
            clean
        } else {
            format!("{}-{}", self.agent_name(), short(head))
        }
    }
}

/// A receipt, in words an agent can act on.
#[allow(dead_code)]
fn describe(trunk: &str, rec: &Value) -> String {
    describe_with(trunk, rec, None)
}

/// Plan lines shown for each landing that beat a change or moved main under it.
const PLAN_LINES: usize = 4;
/// Conflict text shown per file, and files shown.
const HUNK_CHARS: usize = 1500;
const HUNK_FILES: usize = 2;

/// "PR #9 (a02, "price() needs a currency")" for an entry of `beaten_by` or `moved`.
fn prior_name(p: &Value) -> String {
    let cid = p["change_id"].as_str().unwrap_or("?");
    let pr = p["number"].as_u64().map(|n| format!("PR #{n}")).unwrap_or_else(|| cid.to_string());
    let subject = p["subject"].as_str().unwrap_or("");
    let subject = subject.strip_prefix(&format!("{cid}: ")).unwrap_or(subject);
    format!("{pr} ({}, \"{subject}\")", p["agent"].as_str().unwrap_or("?"))
}

/// What each landing in `list` planned, read from its pinned context (GET context/{change}).
/// A landing without context, or one whose context can't be read, shows no plan.
fn plans(api: Option<&Api>, list: &[Value]) -> String {
    let mut out = String::new();
    for p in list.iter().take(3) {
        let (Some(api), Some(_), Some(cid)) = (api, p["context"].as_str(), p["change_id"].as_str()) else { continue };
        let path = match p["rev"].as_u64().filter(|r| *r > 0) {
            Some(r) => format!("context/{cid}?rev={r}"),
            None => format!("context/{cid}"),
        };
        let Some(text) = api.try_call("GET", &path, None).ok().and_then(|c| c["text"].as_str().map(str::to_string)) else { continue };
        let lines = context::plan_lines(&text, PLAN_LINES);
        if !lines.is_empty() {
            let who = p["number"].as_u64().map(|n| format!("PR #{n}")).unwrap_or_else(|| cid.to_string());
            out.push_str(&format!("\nIts plan ({who}, read it all with context change_id {cid}):\n  {}", lines.join("\n  ")));
        }
    }
    out
}

/// The conflict text of a receipt, a few files, each cut.
fn hunks(reason: &Value) -> String {
    let Some(map) = reason["hunks"].as_object().filter(|m| !m.is_empty()) else { return String::new() };
    let mut out = String::from("\nConflict (<<<<<<< main, then this change):");
    for (path, text) in map.iter().take(HUNK_FILES) {
        let text = text.as_str().unwrap_or("");
        let cut: String = text.chars().take(HUNK_CHARS).collect();
        let more = if cut.len() < text.len() { "\n[cut]" } else { "" };
        out.push_str(&format!("\n--- {path}\n{}{more}", cut.trim_end()));
    }
    out
}

/// A receipt, in words an agent can act on. With an API, it reads the plans of the PRs that
/// beat this one, or that moved main under it.
fn describe_with(trunk: &str, rec: &Value, api: Option<&Api>) -> String {
    let cid = rec["req"]["change_id"].as_str().unwrap_or("?");
    let rev = &rec["req"]["rev"];
    let reason = &rec["reason"];
    // The PR number people see in the web app, when the sequencer gave one.
    let pr = rec["number"].as_u64().map(|n| format!("PR #{n} ({cid})")).unwrap_or_else(|| cid.to_string());
    match rec["state"].as_str().unwrap_or("?") {
        "landed" => format!(
            "LANDED: {pr} rev {rev} is on trunk as {} (seq {}, batch {}, {} in that batch).\n\
             Call sync before your next change.",
            short(rec["trunk_commit"].as_str().unwrap_or("?")),
            rec["seq"],
            rec["batch"],
            plural(&rec["ci_runs"].as_u64().unwrap_or(1).to_string(), "CI run")
        ),
        "queued" => format!("QUEUED: {pr} rev {rev} is waiting for a batch. Call receipt with change_id {cid} to follow it."),
        "awaiting_seal" => {
            let why: Vec<&str> = rec["seal_reasons"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            format!(
                "PARKED FOR A HUMAN: {pr} rev {rev} needs a seal: {}.\n\
                 Stop here and tell the user. An approver must run: nerudo seal --trunk {trunk} --change {cid} --rev {rev}\n\
                 It lands in the next batch after enough approvers seal it. Do not try to work around the seal.",
                why.join("; ")
            )
        }
        "awaiting_allowance" => format!(
            "PARKED FOR A HUMAN: {pr} rev {rev} needs an allowance for {}. Tell the user: an approver must sign one with \
             nerudo allowance --trunk {trunk} --units '<units>' --max <n>",
            reason["units"]
        ),
        "ejected" => match reason["kind"].as_str().unwrap_or("?") {
            "conflict" => {
                let paths: Vec<&str> = reason["paths"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                let by: Vec<Value> = reason["beaten_by"].as_array().cloned().unwrap_or_default();
                let what = if by.is_empty() {
                    format!("CLOSED: conflict. {pr} rev {rev} edits lines that changed on trunk in {}.", paths.join(", "))
                } else {
                    let names: Vec<String> = by.iter().take(3).map(prior_name).collect();
                    format!("CLOSED: conflict with {} in {}. {pr} rev {rev} edits the same lines.", names.join(" and "), paths.join(", "))
                };
                format!(
                    "{what}{}{}\n\
                     Call sync, rebuild the change on nerudo/main (git rebase nerudo/main), and call land again with change_id {cid}. \
                     Keep their decision unless your task says otherwise.",
                    plans(api, &by),
                    hunks(reason)
                )
            }
            "ci_failed" => {
                let errors: Vec<String> = reason["diagnostics"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|d| {
                        let s = |k: &str| d[k].as_str().map(str::to_string).unwrap_or_else(|| d[k].to_string());
                        format!("  {} {}:{} {}", s("code"), s("file"), s("line"), s("text"))
                    })
                    .collect();
                let moved: Vec<Value> = reason["moved"].as_array().cloned().unwrap_or_default();
                let why = if moved.is_empty() {
                    "It may pass on its own base: another change that landed first can break it.".to_string()
                } else {
                    let names: Vec<String> = moved
                        .iter()
                        .map(|m| {
                            let at: Vec<&str> = m["paths"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                            if at.is_empty() { prior_name(m) } else { format!("{} in {}", prior_name(m), at.join(", ")) }
                        })
                        .collect();
                    format!("Main moved under it since its base: {}.{}", names.join("; "), plans(api, &moved))
                };
                format!(
                    "EJECTED BY CI: {pr} rev {rev} fails the type check on trunk ({}):\n{}\n{why}\n\
                     Call sync, rebase on nerudo/main, fix the errors, and call land again with change_id {cid}. \
                     Keep their decision unless your task says otherwise.",
                    plural(&reason["errors"].to_string(), "error"),
                    errors.join("\n")
                )
            }
            "ci_crashed" => format!(
                "EJECTED BY CI: the type checker crashed on trunk with {cid} rev {rev}: {}. Simplify the change \
                 (for example, very deep expressions) and call land again with change_id {cid}.",
                reason["detail"]
            ),
            "not_picked" => format!(
                "CLOSED, NOT PICKED: {pr} rev {rev} was one of the attempts for {}, and {} was picked to ship. \
                 Nothing to fix. Call cleanup to delete its branch, then take other work.",
                reason["task"].as_str().unwrap_or("its task"),
                reason["picked"].as_str().unwrap_or("another attempt")
            ),
            "superseded" => format!(
                "REPLACED: {pr} rev {rev} does not merge: rev {} of the same PR replaced it. \
                 Nothing to fix. Call receipt with change_id {cid} to follow the newer revision.",
                reason["rev"]
            ),
            kind => format!("EJECTED ({kind}): {cid} rev {rev}: {reason}. Fix the change and call land again with change_id {cid}."),
        },
        "awaiting_pick" => {
            let task = reason["task"].as_str().unwrap_or("its task");
            let verdict = match reason["passed"].as_bool() {
                Some(true) => "its trial passed the checks on trunk as it is now",
                Some(false) => "its trial FAILED the type check on trunk; a person can't pick it as it is",
                None => "its trial has no check verdict",
            };
            format!(
                "WAITING FOR A PICK: {cid} rev {rev} is one of {} competing attempts for {task}, and {verdict}. \
                 A person compares the attempts in the app and picks one; the picked one merges and the others close. \
                 Stop here and take other work. Call receipt with change_id {cid} to follow it.",
                reason["attempts"]
            )
        }
        other => format!("{cid} rev {rev}: {other} {reason}"),
    }
}

/// "1 commit", "3 commits", "2 batches".
fn plural(n: &str, noun: &str) -> String {
    match n {
        "1" => format!("1 {noun}"),
        _ if noun.ends_with("ch") => format!("{n} {noun}es"),
        _ => format!("{n} {noun}s"),
    }
}

fn short(oid: &str) -> &str {
    &oid[..oid.len().min(8)]
}

fn tools() -> Value {
    let repo = json!({ "type": "string", "description": "Path of the git checkout. Default: the server's working directory." });
    json!([
        {
            "name": "land",
            "description": "Land the commit at HEAD on trunk. Pushes it to your own fork, asks the nerudo sequencer to land it, \
                            and waits for the receipt. The result says LANDED, PARKED FOR A HUMAN, CLOSED (a conflict) or EJECTED \
                            with the reason and what to do next. Commit first; put why in the commit message body.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "change_id": { "type": "string", "description": "A stable name for this change, [A-Za-z0-9._-]. Reuse it when you land again after an ejection, or to replace a waiting revision with a new commit. A merged change_id takes no new revision. Default: the current branch name." },
                    "task": { "type": "string", "description": "The task this change is for, like T-12. Its landing moves the task on." },
                    "context": { "type": "string", "description": "Markdown: your plan, the decisions you made, the alternatives you rejected and why, and the task in your own words. Kept, versioned, on your fork's nerudo/context branch; the landed commit links to it. Secrets are redacted, but leave them out." },
                    "wait_s": { "type": "integer", "description": "Seconds to wait for the receipt (default 50)." },
                    "repo": repo,
                },
            },
        },
        {
            "name": "in_flight",
            "description": "What other agents' changes are waiting to land, which footprint units they touch and the first \
                            line of their plan, and which tasks agents claimed. Call it before you start, and prefer work that \
                            does not overlap.",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "status",
            "description": "Trunk's tip, the latest landings, and counts of landings, CI runs and ejections.",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "receipt",
            "description": "The receipt of one change: landed, queued, parked for a human, or ejected with the reason.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "change_id": { "type": "string" },
                    "rev": { "type": "integer", "description": "Default: the latest revision." },
                },
                "required": ["change_id"],
            },
        },
        {
            "name": "sync",
            "description": "Fetch trunk into nerudo/main in the checkout, and say how far HEAD is ahead and behind.",
            "inputSchema": { "type": "object", "properties": { "repo": repo } },
        },
        {
            "name": "comment",
            "description": "Add a comment to a change's conversation. @name or @team in the body notifies people, teams \
                            (roles in nerudo.toml) and agents; :shortcodes: become emoji.",
            "inputSchema": {
                "type": "object",
                "properties": { "change_id": { "type": "string" }, "body": { "type": "string" } },
                "required": ["change_id", "body"],
            },
        },
        {
            "name": "mentions",
            "description": "Who mentioned you or asked you for a review, newest first.",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "request_review",
            "description": "Ask a person, a team (@team) or another agent to review a change. Use it when a change needs \
                            someone who knows the area, for example @dba for SQL.",
            "inputSchema": {
                "type": "object",
                "properties": { "change_id": { "type": "string" }, "reviewer": { "type": "string", "description": "A name, or @team." } },
                "required": ["change_id", "reviewer"],
            },
        },
        {
            "name": "react",
            "description": "Add an emoji reaction to a change, or to one comment on it; call it again to take it back.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "change_id": { "type": "string" },
                    "emoji": { "type": "string", "description": "An emoji, or a :shortcode: like :rocket:." },
                    "comment": { "type": "string", "description": "A comment id, to react to that comment." },
                },
                "required": ["change_id", "emoji"],
            },
        },
        {
            "name": "tasks",
            "description": "The open tasks (work items, like T-12): yours first, then the rest, with status, priority and why.",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "claim",
            "description": "Take a task: it is assigned to you and in progress. For a task that wants competing attempts, take one of its attempts. Then land your change with task set to its id.",
            "inputSchema": { "type": "object", "properties": { "task": { "type": "string" } }, "required": ["task"] },
        },
        {
            "name": "new_task",
            "description": "Write down work that should happen, for people or other agents to pick up: a follow-up you found.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "body": { "type": "string", "description": "What should change, and why." },
                    "priority": { "type": "integer", "description": "1 urgent, 2 high, 3 medium, 4 low." },
                    "attempts": { "type": "integer", "description": "2 to 5: several agents attempt it side by side, and one attempt is picked to ship. Default 1." },
                    "pick": { "type": "string", "enum": ["person", "smallest", "first"], "description": "Who picks among the attempts. Default: a person." },
                },
                "required": ["title"],
            },
        },
        {
            "name": "context",
            "description": "The context an agent recorded with a change: its plan, decisions and rejected alternatives. Read it \
                            before you change code another agent changed recently, or when its change conflicts with yours: \
                            a receipt that closes your change names the PRs that beat it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "change_id": { "type": "string" },
                    "rev": { "type": "integer", "description": "Default: the latest revision." },
                },
                "required": ["change_id"],
            },
        },
        {
            "name": "release_propose",
            "description": "Propose a release of trunk: the sequencer drafts the next version and its notes from what merged, \
                            and the approvers find the proposal in their inbox. Only a person signs a release; you can't cut one.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "version": { "type": "string", "description": "Like v0.3.0. Default: the next version from the merged PRs' subjects." },
                    "title": { "type": "string" },
                    "notes": { "type": "string", "description": "What you want the approvers to know about this release." },
                },
            },
        },
        {
            "name": "cleanup",
            "description": "Delete the stale branches in your fork (land/* refs, and branches pushed with plain git): their PR merged or closed, or a later revision replaced their ejected one.",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "rules",
            "description": "This trunk's landing rules in plain words: files merged by rule, generated files, files numbered at \
                            landing, paths that need a human seal, and the CI check.",
            "inputSchema": { "type": "object", "properties": { "repo": repo } },
        },
    ])
}

// ------------------------------------------------------------------ request_approval
//
// The agent asks; only the person answers. The tool sends the client an MCP elicitation
// (`elicitation/create`): a form with the PR and one box, approve. Only an accept with the
// box ticked signs, with the key in ~/.nerudo/key and the same code as `nerudo seal`.

const APPROVAL_NOTE: &str = "This client can show the user a form. When `land` parks a PR for a human seal and the user \
is here, you can call `request_approval` with its change_id: the user sees the PR and approves or declines it. Only the \
user answers; you never sign. If the user declines, do not ask again unless the user tells you to.";

/// Whether the client can show a form: MCP elicitation in form mode. 2025-06-18 added
/// elicitation. In 2025-11-25, an empty capability means form mode, and `url` alone does
/// not include it.
fn can_show_form(version: &str, caps: &Value) -> bool {
    let Some(e) = caps.get("elicitation").and_then(Value::as_object) else {
        return false;
    };
    match version {
        "2025-06-18" => true,
        "2025-11-25" => e.is_empty() || e.contains_key("form"),
        _ => false,
    }
}

/// What the person answered.
#[derive(Debug, PartialEq)]
enum Answer {
    Approved,
    /// Why not. Nothing is signed.
    NotApproved(String),
}

impl Server {
    /// The tools, with `request_approval` only for a client that can show a form.
    fn listed_tools(&self) -> Value {
        let mut t = tools();
        if self.ask.form {
            if let Some(list) = t.as_array_mut() {
                list.push(approval_tool());
            }
        }
        t
    }

    fn request_approval(&mut self, a: &Value) -> Result<String, String> {
        let cid = a["change"].as_str().filter(|c| !c.is_empty()).ok_or("change is required: the change_id of the PR")?.to_string();
        if !self.ask.form {
            return Err(format!(
                "This client can't show the user a form (it did not declare MCP elicitation). Tell the user to run: nerudo seal --trunk {} --change {cid}",
                self.trunk
            ));
        }
        if self.ask.open.as_deref() == Some(cid.as_str()) {
            return Err(format!("An approval request for {cid} is open already. Wait for the user's answer."));
        }
        let api = self.api()?;
        let rev = a["rev"].as_u64().map(|r| r.to_string());
        let p = crate::seal_fetch(&api, &cid, rev.as_deref()).map_err(|e| format!("Not asking: {e}. Only a PR that waits for a person's approval can be approved."))?;
        let key = crate::try_load_key().map_err(|e| format!("Not asking: {e}. The user signs with the key on this machine."))?;
        let (name, roles) = crate::approver_of(&api, &crate::hex(&key.verifying_key().to_bytes())).map_err(|e| format!("Not asking: {e}, so the sequencer refuses its seal."))?;
        let pr = pr_name(&p.rec);
        // Don't ask a person whose seal can't help.
        let quorum = &p.rec["reason"]["quorum"];
        if let Some(why) = crate::seal_cannot_help(quorum, &name, &roles) {
            return Err(format!("Not asking: {why}. {pr} waits for: {}.", crate::tally_lines(quorum).join("; ")));
        }
        let message = approval_message(&self.trunk, &p, &name, &roles, &crate::diff_lines(&p.rec, &self.repo));
        self.ask.open = Some(cid.clone());
        let answer = self.ask_person(&cid, &message);
        self.ask.open = None;
        if let Answer::NotApproved(why) = answer {
            return Ok(format!("NOT APPROVED: {why}. Nothing was signed, and {pr} still waits for a person. Do not ask again unless the user tells you to."));
        }
        let out = crate::seal_post(&api, &crate::seal_sign(&self.trunk, &p, &key, &format!("{APPROVAL_TTL_MIN}m")))
            .map_err(|e| format!("The user approved, but the sequencer refused the seal: {e}"))?;
        let next = if out["complete"] == true {
            "The approvals are complete: it merges in the next batch. Call receipt to follow it."
        } else {
            "It waits for more approvals."
        };
        Ok(format!(
            "APPROVED: {name} signed a seal for {pr} rev {}, commit {}. It is the same signature as nerudo seal; nerudo verify checks it.\n{}\n{next}",
            p.rev(),
            short(p.commit()),
            crate::tally_lines(&out["quorum"]).join("\n")
        ))
    }

    /// Send the client an elicitation and wait for the answer. Other messages that come in
    /// the meantime wait in `later`. Three kinds are answered at once: a ping, a second request
    /// for the same PR (refused), and the cancellation of this tool call.
    fn ask_person(&mut self, cid: &str, message: &str) -> Answer {
        self.ask.sent += 1;
        let id = json!(format!("nerudo-approval-{}", self.ask.sent));
        let schema = json!({
            "type": "object",
            "properties": {
                "approve": { "type": "boolean", "title": "Approve and sign", "description": "Sign a seal for this exact commit with your key.", "default": false },
            },
            "required": ["approve"],
        });
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": "elicitation/create", "params": { "message": message, "requestedSchema": schema } }));
        let close = json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": id, "reason": "nerudo stopped waiting" } });
        let end = Instant::now() + self.ask.wait;
        loop {
            let line = match self.input.recv_timeout(end.saturating_duration_since(Instant::now())) {
                Ok(l) => l,
                Err(RecvTimeoutError::Timeout) => {
                    // Close the form. An answer that comes later signs nothing.
                    self.send(&close);
                    return Answer::NotApproved(format!("the user did not answer in {} seconds", self.ask.wait.as_secs()));
                }
                Err(RecvTimeoutError::Disconnected) => return Answer::NotApproved("the client closed the connection".into()),
            };
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                self.later.push_back(line);
                continue;
            };
            match msg["method"].as_str() {
                None if msg["id"] == id => return answer_of(&msg),
                Some("ping") if msg.get("id").is_some() => self.send(&json!({ "jsonrpc": "2.0", "id": msg["id"], "result": {} })),
                Some("notifications/cancelled") if !self.ask.call.is_null() && msg["params"]["requestId"] == self.ask.call => {
                    self.send(&close);
                    return Answer::NotApproved("the client cancelled the tool call".into());
                }
                Some("tools/call") if msg["params"]["name"] == "request_approval" && msg["params"]["arguments"]["change"] == cid => {
                    let text = format!("An approval request for {cid} is open already. Wait for the user's answer.");
                    self.send(&json!({ "jsonrpc": "2.0", "id": msg["id"], "result": { "content": [{ "type": "text", "text": text }], "isError": true } }));
                }
                _ => self.later.push_back(line),
            }
        }
    }
}

/// Only an accept with `approve` set to true approves.
fn answer_of(msg: &Value) -> Answer {
    if !msg["error"].is_null() {
        return Answer::NotApproved(format!("the client answered with an error: {}", msg["error"]["message"].as_str().unwrap_or("?")));
    }
    let r = &msg["result"];
    match r["action"].as_str() {
        Some("accept") if r["content"]["approve"].as_bool() == Some(true) => Answer::Approved,
        Some("accept") => Answer::NotApproved("the user did not tick Approve".into()),
        Some("decline") => Answer::NotApproved("the user declined".into()),
        Some("cancel") => Answer::NotApproved("the user closed the form without an answer".into()),
        _ => Answer::NotApproved("the client's answer has no action that nerudo knows".into()),
    }
}

/// What the person reads before answering. The sequencer's facts first; the agent's subject
/// is marked as the agent's.
fn approval_message(trunk: &str, p: &crate::Pending, name: &str, roles: &[String], diff: &[String]) -> String {
    let mut m = vec![
        format!("Approve {} on {trunk}?", pr_name(&p.rec)),
        String::new(),
        "A coding agent asks you to approve this PR. If you tick Approve and accept, nerudo signs a seal with the key on this \
         machine (~/.nerudo/key). It is the same signature as `nerudo seal`, and `nerudo verify` checks it on main."
            .to_string(),
        String::new(),
        "Why it needs a person (from the sequencer):".to_string(),
    ];
    m.extend(p.reasons().iter().map(|r| format!("- {}", clean(r))));
    m.push("Approvals:".into());
    let quorum = &p.rec["reason"]["quorum"];
    for (t, line) in quorum.as_array().into_iter().flatten().zip(crate::tally_lines(quorum)) {
        let n = crate::missing(t);
        m.push(if n > 0 { format!("- {line}: {n} more needed") } else { format!("- {line}: complete") });
    }
    m.push(format!("You sign as: {name}{}", if roles.is_empty() { String::new() } else { format!(" ({})", roles.join(", ")) }));
    m.push(String::new());
    let subject = clean(p.rec["subject"].as_str().unwrap_or_default());
    m.push(format!("Subject (the agent wrote it): {}", if subject.is_empty() { "(none)" } else { &subject }));
    m.push(format!("Commit: {} (revision {})", p.commit(), p.rev()));
    m.push(format!("Rules digest: {}", p.rules_digest));
    m.extend(diff.iter().cloned());
    m.push(String::new());
    m.push(format!("The seal covers this commit only, and it expires in {APPROVAL_TTL_MIN} minutes."));
    m.join("\n")
}

/// "PR #12 (g1)", or the change id for a record from before PR numbers.
fn pr_name(rec: &Value) -> String {
    let cid = rec["req"]["change_id"].as_str().unwrap_or("?");
    rec["number"].as_u64().map(|n| format!("PR #{n} ({cid})")).unwrap_or_else(|| cid.to_string())
}

/// Text from the agent, on one line: no control characters, at most 200 characters.
fn clean(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).take(200).collect()
}

fn approval_tool() -> Value {
    json!({
        "name": "request_approval",
        "description": "Ask the user to approve a PR that waits for a person (PARKED FOR A HUMAN). The client shows the user a \
                        form: the PR, why it needs a person, the approvals it has, and its size. Only the user answers. An approval \
                        is signed with the user's key on this machine, the same signature as nerudo seal; you never sign. If the \
                        user declines, do not ask again unless the user tells you to.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "change": { "type": "string", "description": "The change_id of the PR." },
                "rev": { "type": "integer", "description": "Default: the latest revision." },
            },
            "required": ["change"],
        },
    })
}


/// An id that goes into a Worker URL path: 1 to 64 of [A-Za-z0-9._-], as the Worker takes a
/// change id. Anything else (a `/`, a `?`, `..`) would make another request.
fn valid_id<'a>(id: &'a str, what: &str) -> Result<&'a str, String> {
    if id.is_empty() || id.len() > 64 || !id.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) {
        return Err(format!("{what} {id:?} must be 1 to 64 of [A-Za-z0-9._-]"));
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// What the server wrote to the client.
    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Sink {
        fn sent(&self) -> Vec<Value> {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
        }
    }

    /// A server whose client already sent `lines`. Keep the sender: dropping it closes the input.
    fn server(lines: &[Value], wait_ms: u64) -> (Server, Sink, mpsc::Sender<String>) {
        let (tx, input) = mpsc::channel();
        for l in lines {
            tx.send(l.to_string()).unwrap();
        }
        let sink = Sink::default();
        let s = Server {
            trunk: "t".into(),
            agent: None,
            client: "test".into(),
            repo: ".".into(),
            fork: None,
            branch: None,
            input,
            later: VecDeque::new(),
            out: Box::new(sink.clone()),
            ask: Ask { form: true, sent: 0, open: None, call: json!(7), wait: Duration::from_millis(wait_ms) },
            key: None,
        };
        (s, sink, tx)
    }

    fn names(tools: &Value) -> Vec<String> {
        tools.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn the_tool_is_listed_only_for_a_client_that_can_show_a_form() {
        for (version, caps, listed) in [
            ("2025-06-18", json!({ "elicitation": {} }), true),
            ("2025-06-18", json!({}), false),
            ("2025-11-25", json!({ "elicitation": {} }), true),
            ("2025-11-25", json!({ "elicitation": { "form": {}, "url": {} } }), true),
            ("2025-11-25", json!({ "elicitation": { "url": {} } }), false),
            ("2025-03-26", json!({ "elicitation": {} }), false),
        ] {
            let (mut s, _, _tx) = server(&[], 10);
            s.ask.form = false;
            let init = s.handle(&json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": { "protocolVersion": version, "capabilities": caps } })).unwrap();
            assert_eq!(init["result"]["protocolVersion"], version);
            let list = s.handle(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })).unwrap();
            assert_eq!(names(&list["result"]["tools"]).contains(&"request_approval".to_string()), listed, "{version} {caps}");
            assert_eq!(init["result"]["instructions"].as_str().unwrap().contains("request_approval"), listed);
        }
    }

    #[test]
    fn only_accept_with_approve_true_approves() {
        let r = |v: Value| answer_of(&json!({ "jsonrpc": "2.0", "id": "x", "result": v }));
        assert_eq!(r(json!({ "action": "accept", "content": { "approve": true } })), Answer::Approved);
        for no in [
            json!({ "action": "accept", "content": { "approve": false } }),
            json!({ "action": "accept", "content": { "approve": "true" } }),
            json!({ "action": "accept", "content": {} }),
            json!({ "action": "accept" }),
            json!({ "action": "decline", "content": { "approve": true } }),
            json!({ "action": "cancel" }),
            json!({ "action": "approve", "content": { "approve": true } }),
            json!({}),
        ] {
            assert!(matches!(r(no.clone()), Answer::NotApproved(_)), "{no}");
        }
        let err = answer_of(&json!({ "jsonrpc": "2.0", "id": "x", "error": { "code": -32601, "message": "no" }, "result": { "action": "accept", "content": { "approve": true } } }));
        assert!(matches!(err, Answer::NotApproved(_)));
    }

    #[test]
    fn a_wait_keeps_other_messages_for_later() {
        let accept = json!({ "jsonrpc": "2.0", "id": "nerudo-approval-1", "result": { "action": "accept", "content": { "approve": true } } });
        let lines = [
            json!({ "jsonrpc": "2.0", "method": "notifications/roots/list_changed" }),
            json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "status", "arguments": {} } }),
            json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "request_approval", "arguments": { "change": "g1" } } }),
            json!({ "jsonrpc": "2.0", "id": "nerudo-approval-0", "result": { "action": "accept", "content": { "approve": true } } }),
            accept,
        ];
        let (mut s, sink, _tx) = server(&lines, 5_000);
        assert_eq!(s.ask_person("g1", "Approve PR #1 (g1)?"), Answer::Approved);
        let sent = sink.sent();
        assert_eq!(sent[0]["method"], "elicitation/create");
        assert_eq!(sent[0]["id"], "nerudo-approval-1");
        assert_eq!(sent[0]["params"]["message"], "Approve PR #1 (g1)?");
        assert_eq!(sent[0]["params"]["requestedSchema"]["required"], json!(["approve"]));
        assert_eq!(sent[0]["params"]["requestedSchema"]["properties"]["approve"]["type"], "boolean");
        assert_eq!((sent[1]["id"].clone(), sent[1]["result"].clone()), (json!(1), json!({})), "a ping is answered at once");
        assert_eq!((sent[2]["id"].clone(), sent[2]["result"]["isError"].clone()), (json!(3), json!(true)), "one open request per PR");
        assert_eq!(sent.len(), 3);
        let later: Vec<Value> = s.later.iter().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(later, vec![lines[0].clone(), lines[2].clone(), lines[4].clone()], "kept, in order");
    }

    #[test]
    fn no_answer_in_time_closes_the_form_and_signs_nothing() {
        let (mut s, sink, _tx) = server(&[], 50);
        assert!(matches!(s.ask_person("g1", "?"), Answer::NotApproved(w) if w.contains("did not answer")));
        let sent = sink.sent();
        assert_eq!(sent[1]["method"], "notifications/cancelled");
        assert_eq!(sent[1]["params"]["requestId"], sent[0]["id"]);
        // The client cancels the tool call, or goes away: no approval either.
        let (mut s, _, _tx) = server(&[json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": 7 } })], 5_000);
        assert!(matches!(s.ask_person("g1", "?"), Answer::NotApproved(w) if w.contains("cancelled")));
        let (mut s, _, tx) = server(&[], 5_000);
        drop(tx);
        assert!(matches!(s.ask_person("g1", "?"), Answer::NotApproved(w) if w.contains("closed")));
    }
}
