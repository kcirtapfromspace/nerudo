//! nerudo: the human side of the sequencer.
//!
//!   nerudo keygen | pubkey
//!   nerudo allowance --trunk T --units 'targets/*,changes/*' --max 50 --ttl 2h
//!   nerudo seal --trunk T --change CID [--rev R] [--yes]
//!   nerudo inbox --trunk T                 (the PRs that wait for an approval your key can give)
//!   nerudo release --trunk T [--version vX.Y.Z] [--at SEQ|COMMIT] [--notes FILE] [--title T] [--prerelease] [--yes]
//!   nerudo status --trunk T
//!   nerudo verify --trunk T
//!   nerudo share --trunk T                                         (admin: read-only web link)
//!   nerudo invite --trunk T --user NAME                            (admin: a person's own web link)
//!   nerudo agent-key --trunk T --agent A [--rotate]                (admin: an agent's key; saves it in
//!                                           ~/.nerudo/agents/T/A. --rotate: a new key; the old one stops)
//!   nerudo pause --trunk T --reason R | resume --trunk T          (admin)
//!   nerudo break-glass --trunk T --change CID --rev R --reason R  (admin)
//!   nerudo repo --trunk T --as OWNER/REPO [--private] [--description D] [--upstream URL]
//!                                          (admin: name a trunk OWNER/REPO, an org's or a user's;
//!                                           public unless --private)
//!   nerudo org --org ORG [--title T] [--description D]            (admin)
//!   nerudo user --name NAME [--title T] [--key HEX|mine]          (admin: a user, one name on every
//!                                           repo; prints the sign-in link. --key mine: this machine's key)
//!   nerudo demo [--agents 6] [--changes 3] [--ci-ms 2000] [--crash-once] [--yes]
//!   nerudo replay --repo URL|PATH [--as ORG/REPO] [--private] [--commits 70] [--agents 16] [--retries 3] [--merges] [--yes]
//!                                          (a real repo's recent history, rebuilt by agents)
//!   nerudo mcp [--trunk T] [--agent A]     (MCP server for coding agents, on stdio)
//!   nerudo git-credential get              (a git credential helper for plain git push:
//!                                            git config credential.helper '!nerudo git-credential')
//!
//! `--trunk` takes a trunk id or a repo name, ORG/REPO. The CLI asks the Hub once for the
//! trunk id of a name (GET /v1/hub/repos/ORG/REPO); a private repo's name resolves only
//! with NERUDO_ADMIN_TOKEN, so agents on a private repo use its trunk id.
//!
//! The Worker URL comes from NERUDO_WORKER (default http://127.0.0.1:8787) and the bearer
//! token from NERUDO_TOKEN (or NERUDO_AGENT_TOKEN). `demo` and the admin commands need
//! NERUDO_ADMIN_TOKEN. The signing key lives in ~/.nerudo/key (owner-only); its public
//! half goes in the trunk's nerudo.toml as an [[approvers]] entry. The first fork of an
//! agent comes with the agent's own key; `mcp`, `demo`, `replay` and `git-credential` keep it
//! in ~/.nerudo/agents/<trunk>/<agent> (owner-only) and send it with that agent's requests.
//! The agent token alone acts for no agent that has a fork.
//!
//! `git-credential` answers git with NERUDO_GIT_USER (or NERUDO_AGENT) and a password: for
//! an agent, its agent key; for a person, NERUDO_GIT_TOKEN (their personal key) as it is.
//! The agent key comes from NERUDO_AGENT_KEY or the key file. Without one, the helper asks
//! for the first fork of the agent with NERUDO_GIT_TOKEN (or NERUDO_TOKEN,
//! NERUDO_AGENT_TOKEN) and keeps the key it returns, so git never gets the agent token. The
//! trunk is the one in git's `path=` (with credential.useHttpPath), else NERUDO_TRUNK. It
//! answers only for the protocol and host of NERUDO_WORKER (default http://127.0.0.1:8787)
//! or NERUDO_WEB, and plain http only for this machine.

mod context;
mod demo;
mod mcp;
mod release;
mod replay;

use ed25519_dalek::{Signer, SigningKey};
use nerudo_core::{AllowanceSpec, SealSpec};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64
}

fn key_path() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap()).join(".nerudo").join("key")
}

fn existing_key_path() -> PathBuf {
    existing_key_path_in(&PathBuf::from(std::env::var("HOME").unwrap()))
}

fn existing_key_path_in(home: &std::path::Path) -> PathBuf {
    let p = home.join(".nerudo/key");
    let old = home.join(".landseq/key");
    if p.exists() || !old.exists() { p } else { old }
}

/// NERUDO variables take precedence. Legacy environments keep working during migration.
pub(crate) fn env_value(name: &str) -> Result<String, std::env::VarError> {
    env_value_with(name, |key| std::env::var(key))
}

fn env_value_with(name: &str, read: impl Fn(&str) -> Result<String, std::env::VarError>) -> Result<String, std::env::VarError> {
    match read(name) {
        Ok(v) if !v.is_empty() => Ok(v),
        result => match name.strip_prefix("NERUDO_") {
            Some(suffix) => read(&format!("LANDSEQ_{suffix}")),
            None => result,
        },
    }
}

pub(crate) fn load_key() -> SigningKey {
    try_load_key().unwrap_or_else(|e| die(&e))
}

/// `load_key` for long-running callers (the MCP server): a missing or bad key is an error,
/// not an exit.
pub(crate) fn try_load_key() -> Result<SigningKey, String> {
    let raw = std::fs::read_to_string(existing_key_path()).map_err(|_| "no key: run `nerudo keygen` first".to_string())?;
    let raw = raw.trim();
    if raw.len() != 64 || !raw.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("bad key file".into());
    }
    let b: [u8; 32] = unhex(raw).try_into().map_err(|_| "bad key file".to_string())?;
    Ok(SigningKey::from_bytes(&b))
}

pub(crate) fn die(msg: &str) -> ! {
    eprintln!("nerudo: {msg}");
    std::process::exit(1)
}

/// One HTTP client for the Worker API and git: pooled connections, a User-Agent workers.dev
/// accepts, and a timeout longer than the receipt long-poll.
pub(crate) fn http() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(15))
            .timeout(Duration::from_secs(120))
            .user_agent(concat!("nerudo-cli/", env!("CARGO_PKG_VERSION")))
            .build()
    })
}

pub(crate) fn env_or(keys: &[&str], default: Option<&str>) -> String {
    keys.iter()
        .find_map(|k| crate::env_value(k).ok().filter(|v| !v.is_empty()))
        .or_else(|| default.map(str::to_string))
        .unwrap_or_else(|| die(&format!("set {}", keys.join(" or "))))
}

/// Your name as the approver in the nerudo.toml that `demo` and `replay` write:
/// NERUDO_USER, or your login name, in lower case with `-` for other characters. It has the
/// form of a user name on the Hub, so `nerudo user --name` can make it your user later.
pub(crate) fn my_name() -> String {
    name_of(&["NERUDO_USER", "USER", "USERNAME"].iter().find_map(|k| crate::env_value(k).ok().filter(|v| !v.is_empty())).unwrap_or_default())
}

fn name_of(raw: &str) -> String {
    let mapped: String = raw.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let mut name = mapped.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    name.truncate(39);
    let name = name.trim_end_matches('-');
    if name.is_empty() { "maintainer".into() } else { name.into() }
}

pub(crate) struct Api {
    base: String,
    token: String,
    /// The agent's own key (X-Nerudo-Agent-Key): the sequencer then checks that each
    /// request acts for that agent only.
    agent_key: Option<String>,
}

impl Api {
    pub(crate) fn new(trunk: &str) -> Api {
        Api::with_token(trunk, env_or(&["NERUDO_TOKEN", "NERUDO_AGENT_TOKEN"], None))
    }

    pub(crate) fn with_token(trunk: &str, token: String) -> Api {
        let worker = env_or(&["NERUDO_WORKER"], Some("http://127.0.0.1:8787"));
        Api { base: format!("{}/v1/trunks/{trunk}", worker.trim_end_matches('/')), token, agent_key: None }
    }

    /// The Hub (`/v1/hub/...`): repo and org names.
    pub(crate) fn hub(token: String) -> Api {
        let worker = env_or(&["NERUDO_WORKER"], Some("http://127.0.0.1:8787"));
        Api { base: format!("{}/v1/hub", worker.trim_end_matches('/')), token, agent_key: None }
    }

    /// This API, as one agent: each request sends the agent's key.
    pub(crate) fn with_agent_key(mut self, key: Option<String>) -> Api {
        self.agent_key = key.filter(|k| !k.is_empty());
        self
    }

    pub(crate) fn call(&self, method: &str, path: &str, body: Option<Value>) -> Value {
        self.try_call(method, path, body).unwrap_or_else(|e| die(&e))
    }

    /// `call` for long-running callers (the MCP server): an HTTP error is returned, not fatal.
    /// A 503 with Retry-After (an object of the Worker is overloaded, and it did not run the
    /// request) is sent again after that time, at most BUSY_TRIES times.
    pub(crate) fn try_call(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value, String> {
        let mut tries = 0;
        loop {
            let mut req = http().request(method, &format!("{}/{path}", self.base)).set("Authorization", &format!("Bearer {}", self.token));
            if let Some(k) = &self.agent_key {
                req = req.set(AGENT_KEY_HEADER, k);
            }
            let res = match &body {
                Some(b) => req.set("Content-Type", "application/json").send_string(&b.to_string()),
                None => req.call(),
            };
            return match res {
                Ok(r) => Ok(serde_json::from_str(&r.into_string().unwrap_or_default()).unwrap_or(Value::Null)),
                Err(ureq::Error::Status(503, r)) if tries < BUSY_TRIES && r.header("retry-after").is_some() => {
                    tries += 1;
                    std::thread::sleep(busy_wait(r.header("retry-after"), tries));
                    continue;
                }
                Err(ureq::Error::Status(code, r)) => Err(format!("HTTP {code}: {}", r.into_string().unwrap_or_default())),
                Err(e) => Err(e.to_string()),
            };
        }
    }

    /// The whole landing log. The sequencer gives at most 500 entries for each request, so
    /// ask again after the last entry until a page is empty.
    pub(crate) fn landing_log(&self) -> Vec<nerudo_core::LogEntry> {
        let mut log: Vec<nerudo_core::LogEntry> = vec![];
        loop {
            let since = log.last().map_or(0, |e| e.seq);
            let page: Vec<nerudo_core::LogEntry> = serde_json::from_value(self.call("GET", &format!("log?since={since}"), None)).unwrap_or_default();
            if page.last().is_none_or(|e| e.seq <= since) {
                return log;
            }
            log.extend(page);
        }
    }
}

fn args() -> (String, BTreeMap<String, String>) {
    let mut it = std::env::args().skip(1);
    let cmd = it.next().unwrap_or_default();
    let mut flags = BTreeMap::new();
    let rest: Vec<String> = it.collect();
    let mut i = 0;
    while i < rest.len() {
        let k = rest[i].trim_start_matches("--").to_string();
        if rest.get(i + 1).is_some_and(|v| !v.starts_with("--")) {
            flags.insert(k, rest[i + 1].clone());
            i += 2;
        } else {
            flags.insert(k, "true".into());
            i += 1;
        }
    }
    (cmd, flags)
}

/// A trunk id for `--trunk`: the id itself, or the trunk of a repo name ORG/REPO.
pub(crate) fn trunk_id(given: &str) -> String {
    let Some((org, repo)) = given.split_once('/') else {
        return given.to_string();
    };
    let hub = Api::hub(crate::env_value("NERUDO_ADMIN_TOKEN").unwrap_or_default());
    match hub.try_call("GET", &format!("repos/{org}/{repo}"), None) {
        Ok(v) => v["trunk"].as_str().map(str::to_string).unwrap_or_else(|| die(&format!("{given}: the hub sent no trunk"))),
        Err(e) if e.starts_with("HTTP 404") => die(&format!("no repo {given} (a private repo's name needs NERUDO_ADMIN_TOKEN; or give its trunk id)")),
        Err(e) => die(&format!("{given}: {e}")),
    }
}

/// ORG/REPO, checked as the Hub checks it.
pub(crate) fn repo_name(name: &str) -> (String, String) {
    let ok_org = |o: &str| !o.is_empty() && o.len() <= 39 && !o.starts_with('-') && o.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    let ok_repo = |r: &str| !r.is_empty() && r.len() <= 100 && r != "." && r != ".." && !r.ends_with(".git") && r.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    match name.split_once('/') {
        Some((o, r)) if ok_org(o) && ok_repo(r) => (o.to_string(), r.to_string()),
        _ => die(&format!("{name:?}: give ORG/REPO; an org is a-z, 0-9 and -, a repo is A-Z, a-z, 0-9, ., _ and -")),
    }
}

/// Name a trunk ORG/REPO on the Hub (admin). Returns the repo's web link: by name for a
/// public repo, else its share link.
pub(crate) fn register(trunk: &str, org: &str, repo: &str, public: bool, description: Option<&str>, upstream: Option<&str>) -> Result<String, String> {
    let admin = env_or(&["NERUDO_ADMIN_TOKEN"], None);
    let mut body = json!({ "trunk": trunk, "public": public });
    if let Some(d) = description {
        body["description"] = json!(d);
    }
    if let Some(u) = upstream {
        body["upstream"] = json!(u);
    }
    Api::hub(admin.clone()).try_call("PUT", &format!("repos/{org}/{repo}"), Some(body))?;
    let share = Api::with_token(trunk, admin).try_call("POST", "share", Some(json!({})))?;
    let link = share["app"].as_str().or(share["url"].as_str()).ok_or("the share response has no url")?;
    // A public repo reads without the key.
    Ok(if public { link.split('?').next().unwrap_or(link).to_string() } else { link.to_string() })
}

fn ttl_ms(s: &str) -> i64 {
    let (n, unit) = s.split_at(s.len() - 1);
    let n: i64 = n.parse().unwrap_or_else(|_| die("ttl like 30m, 2h, 1d"));
    n * match unit {
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => die("ttl unit must be m, h or d"),
    }
}

/// Write a new signing key to ~/.nerudo/key (owner-only).
pub(crate) fn keygen(force: bool) -> SigningKey {
    let p = key_path();
    if existing_key_path().exists() && !force {
        die(&format!("{} exists (use --force to replace)", existing_key_path().display()));
    }
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    let k = SigningKey::generate(&mut rand_core::OsRng);
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create(true).truncate(true);
    // Owner-only from the first byte: the key is never world-readable, however short the moment.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut open, 0o600);
    let mut file = open.open(&p).unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    file.write_all(hex(&k.to_bytes()).as_bytes()).unwrap();
    k
}

pub(crate) fn key_exists() -> bool {
    existing_key_path().exists()
}

/// The header that carries an agent's own key.
const AGENT_KEY_HEADER: &str = "X-Nerudo-Agent-Key";
/// Tries again after a 503 with Retry-After.
const BUSY_TRIES: u32 = 3;

/// The wait before try `tries` + 1 after a 503: the Retry-After of the Worker (at most 10 s),
/// times the try, plus a random 0 to 500 ms so that many clients do not all come back at once.
fn busy_wait(retry_after: Option<&str>, tries: u32) -> Duration {
    use rand_core::RngCore;
    let s = retry_after.and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(1).clamp(1, 10);
    Duration::from_secs(s * u64::from(tries)) + Duration::from_millis(u64::from(rand_core::OsRng.next_u32() % 500))
}

/// Where an agent's key lives: ~/.nerudo/agents/<trunk>/<agent> (owner-only).
pub(crate) fn agent_key_path(trunk: &str, agent: &str) -> PathBuf {
    let part = |s: &str| -> String { s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect() };
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".nerudo").join("agents").join(part(trunk)).join(part(agent))
}

pub(crate) fn load_agent_key(trunk: &str, agent: &str) -> Option<String> {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    load_agent_key_in(&home, trunk, agent)
}

fn load_agent_key_in(home: &std::path::Path, trunk: &str, agent: &str) -> Option<String> {
    let part = |s: &str| -> String { s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect() };
    let p = home.join(".nerudo/agents").join(part(trunk)).join(part(agent));
    let old = home.join(".landseq").join(p.strip_prefix(home.join(".nerudo")).ok()?);
    // A present but unreadable/empty new file must not resurrect an older credential.
    let path = if p.exists() { p } else { old };
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Write the agent's key file, owner-only. Returns its path.
pub(crate) fn save_agent_key(trunk: &str, agent: &str, key: &str) -> std::io::Result<PathBuf> {
    let p = agent_key_path(trunk, agent);
    let dir = p.parent().expect("a parent");
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    for d in [dir, dir.parent().expect("a parent")] {
        std::fs::set_permissions(d, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    }
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut open, 0o600);
    let mut file = open.open(&p)?;
    #[cfg(unix)]
    std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    file.write_all(key.as_bytes())?;
    Ok(p)
}

/// Keep the agent key that a `POST /forks` response carries, in the agent's key file.
/// Returns the key, to send with the agent's next requests.
pub(crate) fn keep_agent_key(trunk: &str, agent: &str, fork: &Value) -> Option<String> {
    let key = fork["agent_key"].as_str().filter(|k| !k.is_empty())?.to_string();
    if load_agent_key(trunk, agent).as_deref() != Some(key.as_str()) {
        if let Err(e) = save_agent_key(trunk, agent, &key) {
            eprintln!("nerudo: cannot save the key of agent {agent}: {e}");
        }
    }
    Some(key)
}

/// Show why a change needs a human, ask, and sign a seal for that exact commit. None: the
/// human said no.
pub(crate) fn seal(api: &Api, trunk: &str, cid: &str, rev: Option<&str>, yes: bool, ttl: &str) -> Option<Value> {
    let key = load_key();
    let p = seal_fetch(api, cid, rev).unwrap_or_else(|e| die(&e));
    show(&p);
    if !yes && !confirm("Seal this exact commit? [y/N] ") {
        return None;
    }
    let out = seal_post(api, &seal_sign(trunk, &p, &key, ttl)).unwrap_or_else(|e| die(&e));
    for l in tally_lines(&out["quorum"]) {
        println!("  {l}");
    }
    Some(out)
}

/// What a person reads before signing a seal: the commit, why it needs a person, and the
/// size of the change.
fn show(p: &Pending) {
    println!("change {} rev {} commit {}", p.change_id(), p.rev(), p.commit());
    if let Some(n) = p.rec["number"].as_u64() {
        let subject: String = p.rec["subject"].as_str().unwrap_or_default().chars().map(|c| if c.is_control() { ' ' } else { c }).take(200).collect();
        println!("  PR #{n}: {subject}");
    }
    for r in p.reasons() {
        println!("  needs a human: {r}");
    }
    for l in diff_lines(&p.rec, ".") {
        println!("  {l}");
    }
}

fn confirm(question: &str) -> bool {
    print!("{question}");
    std::io::stdout().flush().unwrap();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).unwrap();
    matches!(line.trim(), "y" | "Y" | "yes")
}

/// `nerudo inbox`: each PR that waits for an approval that this machine's key can give.
/// For each one: what `seal` shows, then y/N. A yes signs through the same path as `seal`.
fn inbox(api: &Api, trunk: &str, ttl: &str) {
    let key = load_key();
    let (name, roles) = approver_of(api, &hex(&key.verifying_key().to_bytes())).unwrap_or_else(|e| die(&e));
    let q = api.call("GET", "queue", None);
    let mine: Vec<&Value> = q["changes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["state"] == "awaiting_seal" && seal_cannot_help(&c["reason"]["quorum"], &name, &roles).is_none())
        .collect();
    if mine.is_empty() {
        println!("Nothing on {trunk} waits for an approval that {name} can give.");
        return;
    }
    println!("{} PR{} on {trunk} wait for an approval that {name} can give.", mine.len(), if mine.len() == 1 { "" } else { "s" });
    let mut signed = 0;
    for c in &mine {
        println!();
        let cid = c["change_id"].as_str().unwrap_or_default();
        let p = match seal_fetch(api, cid, Some(&c["rev"].to_string())) {
            Ok(p) => p,
            Err(e) => {
                println!("{e}");
                continue;
            }
        };
        show(&p);
        if !confirm("Seal this exact commit? [y/N] ") {
            println!("  not sealed");
            continue;
        }
        match seal_post(api, &seal_sign(trunk, &p, &key, ttl)) {
            Ok(out) => {
                signed += 1;
                for l in tally_lines(&out["quorum"]) {
                    println!("  {l}");
                }
            }
            Err(e) => println!("  the sequencer refused the seal: {e}"),
        }
    }
    println!("\n{name} sealed {signed} of {}.", mine.len());
}

/// The approver whose public key is `key`: their name and roles (`GET summary`).
pub(crate) fn approver_of(api: &Api, key: &str) -> Result<(String, Vec<String>), String> {
    let s = api.try_call("GET", "summary", None)?;
    let name = s["keys"]
        .as_object()
        .and_then(|m| m.iter().find(|(_, k)| k.as_str() == Some(key)).map(|(n, _)| n.clone()))
        .ok_or_else(|| format!("the key on this machine ({}…) is not an approver in nerudo.toml", &key[..key.len().min(12)]))?;
    let roles = strs(&s["people"][&name]);
    Ok((name, roles))
}

/// Why a seal from `name` can't help a change with this quorum tally. None: it can.
pub(crate) fn seal_cannot_help(quorum: &Value, name: &str, roles: &[String]) -> Option<String> {
    let open: Vec<&Value> = quorum.as_array().into_iter().flatten().filter(|t| missing(t) > 0).collect();
    let counts = |t: &&Value| {
        let r = strs(&t["roles"]);
        r.is_empty() || r.iter().any(|x| roles.contains(x))
    };
    let mine: Vec<&Value> = open.iter().copied().filter(counts).collect();
    if open.is_empty() {
        None
    } else if mine.is_empty() {
        Some(format!("{name}'s roles ({}) do not count for it", roles.join(", ")))
    } else if mine.iter().all(|t| strs(&t["have"]).iter().any(|h| h == name)) {
        Some(format!("{name} approved it already"))
    } else {
        None
    }
}

/// Approvals that a quorum tally still needs.
pub(crate) fn missing(t: &Value) -> u64 {
    t["need"].as_u64().unwrap_or(0).saturating_sub(t["have"].as_array().map_or(0, Vec::len) as u64)
}

pub(crate) fn strs(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect()
}

// A seal in three steps, shared by `nerudo seal` and the MCP tool `request_approval`:
// fetch what the person approves, sign exactly that, and send it. None of them prints or
// exits, because the MCP server uses stdout for JSON-RPC.

/// A change that waits for a seal, as the approver sees it before signing.
pub(crate) struct Pending {
    /// The receipt (`GET changes/{id}`).
    pub rec: Value,
    /// The digest of the rules on trunk. The seal binds it.
    pub rules_digest: String,
}

impl Pending {
    pub(crate) fn change_id(&self) -> &str {
        self.rec["req"]["change_id"].as_str().unwrap_or_default()
    }

    pub(crate) fn rev(&self) -> u32 {
        self.rec["req"]["rev"].as_u64().unwrap_or(1) as u32
    }

    pub(crate) fn commit(&self) -> &str {
        self.rec["req"]["commit"].as_str().unwrap_or_default()
    }

    pub(crate) fn reasons(&self) -> Vec<String> {
        self.rec["seal_reasons"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default()
    }
}

/// Fetch: the receipt of a change that waits for a seal, and the rules digest to sign.
pub(crate) fn seal_fetch(api: &Api, cid: &str, rev: Option<&str>) -> Result<Pending, String> {
    let path = match rev {
        Some(r) => format!("changes/{cid}?rev={r}"),
        None => format!("changes/{cid}"),
    };
    let rec = api.try_call("GET", &path, None)?;
    if rec["state"] != "awaiting_seal" {
        return Err(format!("{cid} is {}, not awaiting a seal", rec["state"]));
    }
    let stats = api.try_call("GET", "stats", None)?;
    Ok(Pending { rec, rules_digest: stats["rules_digest"].as_str().unwrap_or_default().to_string() })
}

/// Sign: a seal for this exact commit, its reasons and the rules digest, with `key`.
/// Returns the body for `POST seals`.
pub(crate) fn seal_sign(trunk: &str, p: &Pending, key: &SigningKey, ttl: &str) -> Value {
    let spec = SealSpec {
        trunk: trunk.to_string(),
        change_id: p.change_id().to_string(),
        rev: p.rev(),
        commit: p.commit().to_string(),
        rules_digest: p.rules_digest.clone(),
        reasons: p.reasons(),
        expires_at_ms: now_ms() + ttl_ms(ttl),
        approver: hex(&key.verifying_key().to_bytes()),
    };
    let sig = hex(&key.sign(&spec.signing_bytes()).to_bytes());
    json!({ "spec": spec, "signature": sig })
}

/// Post: send the signed seal. The answer has the quorum tally and `complete`.
pub(crate) fn seal_post(api: &Api, signed: &Value) -> Result<Value, String> {
    api.try_call("POST", "seals", Some(signed.clone()))
}

/// One line for each quorum: "governance: 1 of 2 maintainer (alice)".
pub(crate) fn tally_lines(quorum: &Value) -> Vec<String> {
    let mut out = vec![];
    for t in quorum.as_array().into_iter().flatten() {
        let have: Vec<&str> = t["have"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        let roles: Vec<&str> = t["roles"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        let who = if roles.is_empty() { "approvers".to_string() } else { roles.join("/") };
        out.push(format!("{}: {} of {} {who} ({})", t["kind"].as_str().unwrap_or("?"), have.len(), t["need"], if have.is_empty() { "none yet".into() } else { have.join(", ") }));
    }
    out
}

/// The size of a change. The sequencer counts the files and lines; the file list comes from
/// git in `repo` when that checkout has the change's commit. The API gives no file list.
pub(crate) fn diff_lines(rec: &Value, repo: &str) -> Vec<String> {
    let s = &rec["size"];
    let mut out = vec![match s["files"].as_u64() {
        Some(n) => format!("changes: {n} file{}, +{} −{} (counted by the sequencer)", if n == 1 { "" } else { "s" }, s["adds"], s["dels"]),
        None => "changes: the sequencer did not count this PR".to_string(),
    }];
    let (base, commit) = (rec["req"]["base"].as_str().unwrap_or_default(), rec["req"]["commit"].as_str().unwrap_or_default());
    let oid = |s: &str| s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit());
    let numstat = Some(())
        .filter(|_| oid(base) && oid(commit))
        .and_then(|_| {
            std::process::Command::new("git")
                .current_dir(repo)
                .args(["diff", "--numstat", "--no-renames", base, commit, "--"])
                .env("GIT_TERMINAL_PROMPT", "0")
                .stderr(std::process::Stdio::null())
                .output()
                .ok()
        })
        .filter(|o| o.status.success());
    let Some(numstat) = numstat else {
        out.push(format!("files: not available here (this checkout does not have commit {}). The PR page in the app shows the diff.", &commit[..commit.len().min(12)]));
        return out;
    };
    // git quotes a path with unusual characters, so a path can't add a line of its own.
    let files: Vec<String> = String::from_utf8_lossy(&numstat.stdout)
        .lines()
        .filter_map(|l| {
            let mut f = l.splitn(3, '\t');
            let (a, d, path) = (f.next()?, f.next()?, f.next()?);
            Some(if a == "-" { format!("  {path} (binary)") } else { format!("  {path} +{a} −{d}") })
        })
        .collect();
    out.push("files (git diff in the local checkout):".to_string());
    out.extend(files.iter().take(20).cloned());
    if files.len() > 20 {
        out.push(format!("  and {} more files", files.len() - 20));
    }
    out
}

/// A git credential helper (`git config credential.helper '!nerudo git-credential'`). git
/// sends `protocol=`, `host=` and (with credential.useHttpPath) `path=` lines on stdin;
/// `get` answers with `username=` and `password=`. `store` and `erase` do nothing. Only
/// those two lines go to stdout; what went wrong goes to stderr, which git shows.
fn git_credential() {
    let mut input = String::new();
    let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input);
    if std::env::args().nth(2).as_deref() != Some("get") {
        return;
    }
    let var = |keys: &[&str]| keys.iter().find_map(|k| crate::env_value(k).ok().filter(|v| !v.is_empty()));
    let mut ours = vec![var(&["NERUDO_WORKER"]).unwrap_or_else(|| "http://127.0.0.1:8787".into())];
    ours.extend(var(&["NERUDO_WEB"]));
    let (Some(user), Some(token)) = (var(&["NERUDO_GIT_USER", "NERUDO_AGENT"]), var(&["NERUDO_GIT_TOKEN", "NERUDO_TOKEN", "NERUDO_AGENT_TOKEN"])) else {
        return;
    };
    if !credential_for(&input, &ours) {
        return;
    }
    if let Some(password) = git_password(&input, &user, &token) {
        println!("username={user}\npassword={password}");
    }
}

/// The password for `user`: an agent's key, or `token` as it is (a person's key, or a key).
/// None: no password; the reason is on stderr.
fn git_password(input: &str, user: &str, token: &str) -> Option<String> {
    let agent = !user.is_empty() && user.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !agent || token.starts_with("lsa1.") {
        return Some(token.to_string());
    }
    if let Some(k) = crate::env_value("NERUDO_AGENT_KEY").ok().filter(|k| k.starts_with(&format!("lsa1.{user}."))) {
        return Some(k);
    }
    // Without the trunk there is no key file to read. The token goes as it is: a person's
    // key, or the agent token, which the sequencer takes only for the first push of an agent.
    let Some(trunk) = credential_trunk(input) else {
        return Some(token.to_string());
    };
    if let Some(k) = load_agent_key(&trunk, user) {
        return Some(k);
    }
    let say = |e: &str| eprintln!("nerudo git-credential: {user} on {trunk}: {e}");
    match Api::with_token(&trunk, token.to_string()).try_call("POST", "forks", Some(json!({ "agent": user }))) {
        Ok(f) => {
            let key = keep_agent_key(&trunk, user, &f);
            if key.is_none() {
                say("the fork response has no agent key");
            }
            key
        }
        // Not the agent token: a person's key (a person can have a name like an agent's).
        Err(e) if e.starts_with("HTTP 401") => Some(token.to_string()),
        Err(e) => {
            let why = e.split_once(": ").and_then(|(_, b)| serde_json::from_str::<Value>(b).ok()).and_then(|v| v["error"].as_str().map(str::to_string));
            say(&why.unwrap_or(e));
            None
        }
    }
}

/// The trunk of git's request: from `path=` (`v1/trunks/T/git`, `v1/repos/ORG/REPO/git` or
/// `ORG/REPO.git`), else NERUDO_TRUNK. The Hub gives the trunk of a name ORG/REPO.
fn credential_trunk(input: &str) -> Option<String> {
    let path = input.lines().find_map(|l| l.strip_prefix("path=")).unwrap_or("").trim_matches('/');
    let parts: Vec<&str> = path.split('/').collect();
    let given = match parts.as_slice() {
        ["v1", "trunks", t, ..] => Some(t.to_string()),
        ["v1", "repos", org, repo, ..] => Some(format!("{org}/{repo}")),
        [org, repo] => Some(format!("{org}/{}", repo.strip_suffix(".git").unwrap_or(repo))),
        _ => None,
    };
    let given = given.or_else(|| crate::env_value("NERUDO_TRUNK").ok()).filter(|t| !t.is_empty())?;
    let Some((org, repo)) = given.split_once('/') else {
        return Some(given);
    };
    let hub = Api::hub(crate::env_value("NERUDO_ADMIN_TOKEN").unwrap_or_default());
    match hub.try_call("GET", &format!("repos/{org}/{repo}"), None) {
        Ok(v) => v["trunk"].as_str().map(str::to_string),
        Err(e) => {
            eprintln!("nerudo git-credential: no trunk for {given} ({e}); set NERUDO_TRUNK to its trunk id");
            None
        }
    }
}

/// Whether git's request names one of `ours` (the Worker and the web app): the same
/// protocol and host (with its port). Plain http only to this machine, so a helper set for
/// every host never sends the token anywhere else.
fn credential_for(input: &str, ours: &[String]) -> bool {
    let field = |k: &str| input.lines().find_map(|l| l.strip_prefix(k)).unwrap_or("");
    let (protocol, host) = (field("protocol="), field("host="));
    let name = match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().map(|h| format!("[{h}]")).unwrap_or_default(),
        None => host.rsplit_once(':').map_or(host, |(h, _)| h).to_string(),
    };
    if protocol != "https" && !(protocol == "http" && ["127.0.0.1", "localhost", "[::1]"].contains(&name.as_str())) {
        return false;
    }
    ours.iter().any(|url| match url.split_once("://") {
        Some((scheme, rest)) => scheme == protocol && rest.split('/').next() == Some(host),
        None => false,
    })
}

fn main() {
    let (cmd, mut f) = args();
    // demo and replay make a new trunk; every other command takes a name for --trunk too.
    if !matches!(cmd.as_str(), "demo" | "replay") {
        if let Some(t) = f.get("trunk").filter(|t| t.contains('/')).cloned() {
            f.insert("trunk".into(), trunk_id(&t));
        }
    }
    let need = |k: &str| f.get(k).cloned().unwrap_or_else(|| die(&format!("--{k} is required")));
    match cmd.as_str() {
        "keygen" => {
            let k = keygen(f.contains_key("force"));
            println!("{}", hex(&k.verifying_key().to_bytes()));
            eprintln!("public key above; add it to the trunk's nerudo.toml as an [[approvers]] entry");
        }
        "pubkey" => println!("{}", hex(&load_key().verifying_key().to_bytes())),
        "allowance" => {
            let trunk = need("trunk");
            let api = Api::new(&trunk);
            let key = load_key();
            let stats = api.call("GET", "stats", None);
            let spec = AllowanceSpec {
                id: f.get("id").cloned().unwrap_or_else(|| format!("al-{}", now_ms())),
                trunk: trunk.clone(),
                units: need("units").split(',').map(|s| s.trim().to_string()).collect(),
                max_landings: need("max").parse().unwrap_or_else(|_| die("--max must be a number")),
                expires_at_ms: now_ms() + ttl_ms(&f.get("ttl").cloned().unwrap_or_else(|| "2h".into())),
                rules_digest: stats["rules_digest"].as_str().unwrap_or_else(|| die("worker did not report rules_digest")).to_string(),
                approver: hex(&key.verifying_key().to_bytes()),
            };
            let sig = hex(&key.sign(&spec.signing_bytes()).to_bytes());
            let out = api.call("POST", "allowances", Some(json!({ "spec": spec, "signature": sig })));
            println!("{out}");
        }
        "seal" => {
            let trunk = need("trunk");
            let ttl = f.get("ttl").cloned().unwrap_or_else(|| "30m".into());
            match seal(&Api::new(&trunk), &trunk, &need("change"), f.get("rev").map(String::as_str), f.contains_key("yes"), &ttl) {
                Some(out) => println!("{out}"),
                None => die("not sealed"),
            }
        }
        "inbox" => {
            let trunk = need("trunk");
            inbox(&Api::new(&trunk), &trunk, &f.get("ttl").cloned().unwrap_or_else(|| "30m".into()));
        }
        "release" => {
            let trunk = need("trunk");
            let o = release::Options {
                version: f.get("version").cloned(),
                at: f.get("at").cloned(),
                notes_file: f.get("notes").cloned(),
                title: f.get("title").cloned(),
                prerelease: f.contains_key("prerelease"),
                yes: f.contains_key("yes"),
                ttl: f.get("ttl").cloned().unwrap_or_else(|| "30m".into()),
            };
            if release::release(&Api::new(&trunk), &trunk, &o).is_none() {
                die("not released");
            }
        }
        "status" => println!("{}", serde_json::to_string_pretty(&Api::new(&need("trunk")).call("GET", "stats", None)).unwrap()),
        "verify" => {
            let trunk = need("trunk");
            if !demo::verify(&Api::new(&trunk), &trunk) {
                die("trunk check failed");
            }
        }
        "invite" => {
            let admin = Api::with_token(&need("trunk"), env_or(&["NERUDO_ADMIN_TOKEN"], None));
            let out = admin.call("POST", "invite", Some(json!({ "user": need("user") })));
            println!("{}", out["app"].as_str().or(out["url"].as_str()).unwrap_or_else(|| die("the invite response has no url")));
        }
        "share" => {
            let admin = Api::with_token(&need("trunk"), env_or(&["NERUDO_ADMIN_TOKEN"], None));
            let out = admin.call("POST", "share", Some(json!({})));
            println!("{}", out["app"].as_str().or(out["url"].as_str()).unwrap_or_else(|| die("the share response has no url")));
        }
        "pause" | "resume" => {
            let admin = Api::with_token(&need("trunk"), env_or(&["NERUDO_ADMIN_TOKEN"], None));
            let body = json!({ "reason": f.get("reason").cloned().unwrap_or_default() });
            println!("{}", admin.call("POST", &cmd, Some(body)));
        }
        "break-glass" => {
            let admin = Api::with_token(&need("trunk"), env_or(&["NERUDO_ADMIN_TOKEN"], None));
            let rev: u32 = need("rev").parse().unwrap_or_else(|_| die("--rev must be a number"));
            println!("{}", admin.call("POST", "break-glass", Some(json!({ "change_id": need("change"), "rev": rev, "reason": need("reason") }))));
        }
        "repo" => {
            let (org, repo) = repo_name(&need("as"));
            let trunk = need("trunk");
            let public = !f.contains_key("private");
            match register(&trunk, &org, &repo, public, f.get("description").map(String::as_str), f.get("upstream").map(String::as_str)) {
                Ok(link) => {
                    println!("{org}/{repo} is trunk {trunk} ({})", if public { "public" } else { "private" });
                    println!("{link}");
                }
                Err(e) => die(&e),
            }
        }
        "org" => {
            let org = need("org");
            let mut body = json!({});
            for k in ["title", "description"] {
                if let Some(v) = f.get(k) {
                    body[k] = json!(v);
                }
            }
            println!("{}", Api::hub(env_or(&["NERUDO_ADMIN_TOKEN"], None)).call("PUT", &format!("orgs/{org}"), Some(body)));
        }
        "user" => {
            // A user is one name across every repo; the sign-in link opens them all.
            let name = need("name");
            let mut body = json!({});
            if let Some(t) = f.get("title") {
                body["title"] = json!(t);
            }
            match f.get("key").map(String::as_str) {
                Some("mine") => body["key"] = json!(hex(&load_key().verifying_key().to_bytes())),
                Some(k) => body["key"] = json!(k),
                None => {}
            }
            let hub = Api::hub(env_or(&["NERUDO_ADMIN_TOKEN"], None));
            let u = hub.call("PUT", &format!("users/{name}"), Some(body));
            let key = u["key"].as_str().map(|k| format!(", key {}…", &k[..12.min(k.len())])).unwrap_or_default();
            println!("user {name}{key}");
            let link = hub.call("POST", &format!("users/{name}/invite"), Some(json!({})));
            println!("sign-in link (it opens every repo that lists {name}; give it only to {name}):");
            println!("{}", link["url"].as_str().unwrap_or("?"));
        }
        "agent-key" => {
            // The admin gives an agent its key again: for a fork made before agent keys, or
            // a key that is lost. --rotate makes a new key; the old one stops working.
            let (trunk, agent) = (need("trunk"), need("agent"));
            if agent.is_empty() || !agent.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                die("--agent must be [A-Za-z0-9_]+");
            }
            let admin = Api::with_token(&trunk, env_or(&["NERUDO_ADMIN_TOKEN"], None));
            let path = format!("agents/{agent}/key");
            let rotate = f.contains_key("rotate");
            let out = if rotate { admin.call("POST", &path, Some(json!({}))) } else { admin.call("GET", &path, None) };
            let key = out["agent_key"].as_str().unwrap_or_else(|| die("the response has no agent_key"));
            match save_agent_key(&trunk, &agent, key) {
                Ok(p) => eprintln!("saved in {} (owner-only); nerudo mcp on this machine sends it", p.display()),
                Err(e) => eprintln!("nerudo: cannot save it in {}: {e}", agent_key_path(&trunk, &agent).display()),
            }
            eprintln!("for an agent on another machine: put the key in that file there, or in NERUDO_AGENT_KEY");
            if rotate {
                eprintln!("the old key of {agent} no longer works");
            }
            println!("{key}");
        }
        "demo" => demo::run(&f),
        "replay" => replay::run(&f),
        "mcp" => mcp::serve(&f),
        "git-credential" => git_credential(),
        _ => die("usage: nerudo keygen | pubkey | allowance --trunk T --units U --max N [--ttl 2h] | seal --trunk T --change C [--rev R] [--yes] | inbox --trunk T | release --trunk T [--version vX.Y.Z] [--at SEQ|COMMIT] [--notes FILE] [--prerelease] [--yes] | status --trunk T | verify --trunk T | share --trunk T | invite --trunk T --user NAME | agent-key --trunk T --agent A [--rotate] | pause --trunk T --reason R | resume --trunk T | break-glass --trunk T --change C --rev R --reason R | repo --trunk T --as OWNER/REPO [--private] [--description D] [--upstream URL] | org --org ORG [--title T] [--description D] | user --name NAME [--title T] [--key HEX|mine] | demo [--agents N] [--changes N] [--ci-ms MS] [--crash-once] [--yes] | replay --repo URL|PATH [--as ORG/REPO] [--private] [--commits 70] [--agents 16] [--merges] [--ci-ms MS] [--open N --maintainers A,B] [--yes] | mcp [--trunk T] [--agent A] | git-credential get  (--trunk takes a trunk id or ORG/REPO)"),
    }
}

#[cfg(test)]
mod tests {
    use super::credential_for;

    #[test]
    fn the_new_environment_wins_and_the_previous_environment_still_works() {
        let values = std::collections::BTreeMap::from([("NERUDO_WORKER", "new"), ("LANDSEQ_WORKER", "old"), ("LANDSEQ_TOKEN", "token")]);
        let read = |k: &str| values.get(k).map(|v| v.to_string()).ok_or(std::env::VarError::NotPresent);
        assert_eq!(super::env_value_with("NERUDO_WORKER", read).unwrap(), "new");
        assert_eq!(super::env_value_with("NERUDO_TOKEN", read).unwrap(), "token");
        assert!(super::env_value_with("NERUDO_UNKNOWN", read).is_err());
    }

    #[test]
    fn existing_keys_are_read_without_replacing_identity() {
        let home = std::env::temp_dir().join(format!("nerudo-key-migration-{}-{}", std::process::id(), super::now_ms()));
        let old = home.join(".landseq/key");
        let new = home.join(".nerudo/key");
        let agent = home.join(".landseq/agents/test/alice");
        std::fs::create_dir_all(agent.parent().unwrap()).unwrap();
        std::fs::write(&old, "synthetic old key").unwrap();
        std::fs::write(&agent, "synthetic agent key").unwrap();
        assert_eq!(super::existing_key_path_in(&home), old);
        assert_eq!(super::load_agent_key_in(&home, "test", "alice").as_deref(), Some("synthetic agent key"));
        std::fs::create_dir_all(new.parent().unwrap()).unwrap();
        std::fs::write(&new, "synthetic new key").unwrap();
        assert_eq!(super::existing_key_path_in(&home), new);
        let new_agent = home.join(".nerudo/agents/test/alice");
        std::fs::create_dir_all(new_agent.parent().unwrap()).unwrap();
        std::fs::write(&new_agent, "").unwrap();
        assert!(super::load_agent_key_in(&home, "test", "alice").is_none());
        assert_eq!(std::fs::read_to_string(old).unwrap(), "synthetic old key");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn a_busy_worker_is_asked_again_later_each_time() {
        use super::busy_wait;
        use std::time::Duration;
        for (after, tries, min) in [(Some("1"), 1, 1), (Some("2"), 2, 4), (Some("60"), 1, 10), (None, 3, 3), (Some("x"), 1, 1), (Some("0"), 1, 1)] {
            let w = busy_wait(after, tries);
            assert!(w >= Duration::from_secs(min) && w < Duration::from_secs(min) + Duration::from_millis(500), "{after:?} {tries}: {w:?}");
        }
    }

    fn ask(protocol: &str, host: &str) -> String {
        format!("protocol={protocol}\nhost={host}\n\n")
    }

    #[test]
    fn git_credential_answers_only_its_own_hosts() {
        // No NERUDO_WORKER or NERUDO_WEB: only the loopback default.
        let default = vec!["http://127.0.0.1:8787".to_string()];
        assert!(credential_for(&ask("http", "127.0.0.1:8787"), &default));
        assert!(!credential_for(&ask("http", "127.0.0.1:8788"), &default));
        assert!(!credential_for(&ask("https", "github.com"), &default));
        let ours = vec!["https://nerudo.example.workers.dev".to_string(), "https://web.example.dev/".to_string()];
        assert!(credential_for(&ask("https", "nerudo.example.workers.dev"), &ours));
        assert!(credential_for(&ask("https", "web.example.dev"), &ours));
        assert!(!credential_for(&ask("https", "github.com"), &ours));
        assert!(!credential_for(&ask("http", "nerudo.example.workers.dev"), &ours));
        // Plain http to a host that is not this machine: never, even when configured.
        let plain = vec!["http://192.168.1.5:8787".to_string(), "http://[::1]:8787".to_string()];
        assert!(!credential_for(&ask("http", "192.168.1.5:8787"), &plain));
        assert!(credential_for(&ask("http", "[::1]:8787"), &plain));
        assert!(!credential_for("", &default));
    }

    #[test]
    fn git_credential_finds_the_trunk_in_the_path() {
        let ask = |path: &str| format!("protocol=https\nhost=w.example.dev\npath={path}\n\n");
        assert_eq!(super::credential_trunk(&ask("v1/trunks/scn-git-1/git")).as_deref(), Some("scn-git-1"));
        assert_eq!(super::credential_trunk(&ask("/v1/trunks/t1/git/")).as_deref(), Some("t1"));
    }

    #[test]
    fn git_credential_gives_a_person_key_or_an_agent_key_as_it_is() {
        // No network: these never ask the sequencer.
        assert_eq!(super::git_password("", "lee-ann", "personal").as_deref(), Some("personal"));
        assert_eq!(super::git_password("", "a00", "lsa1.a00.0.ff").as_deref(), Some("lsa1.a00.0.ff"));
    }

    #[test]
    fn an_agent_key_file_stays_under_its_directory() {
        let p = super::agent_key_path("../../etc", "a/../b");
        let s = p.to_string_lossy();
        assert!(s.ends_with("/.nerudo/agents/______etc/a____b"), "{s}");
        assert!(!p.components().any(|c| c == std::path::Component::ParentDir));
        assert!(super::agent_key_path("scn-x-1", "a00").ends_with(".nerudo/agents/scn-x-1/a00"));
    }

    #[test]
    fn a_login_name_becomes_an_approver_name() {
        assert_eq!(super::name_of("Patrick D."), "patrick-d");
        assert_eq!(super::name_of("dev_user"), "dev-user");
        assert_eq!(super::name_of("--"), "maintainer");
        assert_eq!(super::name_of(""), "maintainer");
        assert_eq!(super::name_of(&"a".repeat(50)).len(), 39);
    }
}
