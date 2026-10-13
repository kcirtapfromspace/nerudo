//! Agent forks. On Cloudflare each agent gets an Artifacts fork; locally, Forgejo forks
//! stand in. Either way the agent only ever writes its own repo, and only the Durable
//! Object holds a token that can write trunk.

use crate::transport::{basic, FetchTransport};
use crate::types::{Fork, HostCfg};
use nerudo_git::Transport;
use std::time::Duration;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use worker::{console_warn, Delay, Env, EnvBinding, Error, Result};

/// Agent tokens (fork write, trunk read) last a day; the agent asks for a new fork after that.
const AGENT_TOKEN_TTL_S: f64 = 86_400.0;
/// The trunk write token is minted at init. Refreshing it is not implemented yet.
const TRUNK_TOKEN_TTL_S: f64 = 30.0 * 86_400.0;

/// A trunk name the object accepts. On Artifacts, forks are named `<trunk>--<agent>` and
/// agent names have no `-`, so a trunk name without `--` keeps every fork name unique to
/// one (trunk, agent) pair and distinct from every trunk name.
pub fn valid_trunk_repo(host: &HostCfg) -> bool {
    match host {
        HostCfg::Forgejo { .. } => true,
        HostCfg::Artifacts { repo, .. } => {
            !repo.is_empty() && !repo.contains("--") && repo.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        }
    }
}

pub fn valid_agent(agent: &str) -> bool {
    !agent.is_empty() && agent.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `known`: the fork this object already made for the agent, if any.
pub async fn fork(env: &Env, host: &HostCfg, agent: &str, known: Option<Fork>) -> Result<Fork> {
    match (host, known) {
        (HostCfg::Forgejo { api, owner, repo, token }, _) => forgejo_fork(api, owner, repo, token, agent).await,
        (HostCfg::Artifacts { binding, .. }, Some(known)) => {
            let handle = call(&env.get_binding::<Binding>(binding)?.0, "get", &[known.name.clone().into()]).await?;
            Ok(Fork { authorization: write_token(&handle).await?, ..known })
        }
        (HostCfg::Artifacts { binding, repo }, None) => artifacts_fork(env, binding, repo, agent).await,
    }
}

/// The trunk write credential, minted by the Durable Object itself. None: the host needs
/// it supplied at init.
pub async fn trunk_write_authorization(env: &Env, host: &HostCfg) -> Result<Option<String>> {
    match host {
        HostCfg::Forgejo { .. } => Ok(None),
        HostCfg::Artifacts { binding, repo } => Ok(Some(bearer(env, binding, repo, "write", TRUNK_TOKEN_TTL_S).await?)),
    }
}

/// Create the trunk repo. Returns its remote and the creation token (an Authorization
/// header) for the first push; the caller revokes it once it has minted its own.
pub async fn create_trunk(env: &Env, host: &HostCfg, branch: &str) -> Result<(String, String)> {
    match host {
        HostCfg::Forgejo { .. } => Err(Error::RustError("creating a trunk from a seed needs the Artifacts host".into())),
        HostCfg::Artifacts { binding, repo } => {
            let ns = env.get_binding::<Binding>(binding)?;
            let opts = js_sys::Object::new();
            js_sys::Reflect::set(&opts, &"setDefaultBranch".into(), &branch.into())?;
            let created = call(&ns.0, "create", &[repo.into(), opts.into()]).await?;
            Ok((field(&created, "remote")?, format!("Bearer {}", field(&created, "token")?)))
        }
    }
}

/// Import a public HTTPS git repo (GitHub, say) into Artifacts as `name`, read-only, at
/// `depth` commits of `branch`. Returns its remote and the import token (an Authorization
/// header). The import can still be running when this returns; the caller retries reads.
pub async fn import_repo(env: &Env, host: &HostCfg, name: &str, url: &str, branch: &str, depth: u32) -> Result<(String, String)> {
    let HostCfg::Artifacts { binding, .. } = host else {
        return Err(Error::RustError("importing a repo needs the Artifacts host".into()));
    };
    let ns = env.get_binding::<Binding>(binding)?;
    let source = js_sys::Object::new();
    js_sys::Reflect::set(&source, &"url".into(), &url.into())?;
    js_sys::Reflect::set(&source, &"branch".into(), &branch.into())?;
    js_sys::Reflect::set(&source, &"depth".into(), &JsValue::from_f64(depth as f64))?;
    let opts = js_sys::Object::new();
    js_sys::Reflect::set(&opts, &"readOnly".into(), &JsValue::TRUE)?;
    js_sys::Reflect::set(&opts, &"description".into(), &format!("upstream of {url}").into())?;
    let target = js_sys::Object::new();
    js_sys::Reflect::set(&target, &"name".into(), &name.into())?;
    js_sys::Reflect::set(&target, &"opts".into(), &opts)?;
    let params = js_sys::Object::new();
    js_sys::Reflect::set(&params, &"source".into(), &source)?;
    js_sys::Reflect::set(&params, &"target".into(), &target)?;
    let imported = call(&ns.0, "import", &[params.into()]).await?;
    let token = js_sys::Reflect::get(&imported, &"token".into())?;
    let token = match token.as_string() {
        Some(t) => t,
        None => field(&token, "plaintext")?,
    };
    Ok((field(&imported, "remote")?, format!("Bearer {token}")))
}

/// Revoke a token on the trunk repo (the creation token, once the object has its own).
pub async fn revoke(env: &Env, host: &HostCfg, authorization: &str) -> Result<()> {
    if let HostCfg::Artifacts { binding, repo } = host {
        let ns = env.get_binding::<Binding>(binding)?;
        let handle = call(&ns.0, "get", &[repo.into()]).await?;
        let token = authorization.strip_prefix("Bearer ").unwrap_or(authorization);
        call(&handle, "revokeToken", &[token.into()]).await?;
    }
    Ok(())
}

/// A read-only trunk credential for an agent, so it can fetch trunk but never push to it.
pub async fn trunk_read_authorization(env: &Env, host: &HostCfg) -> Result<Option<String>> {
    match host {
        HostCfg::Forgejo { .. } => Ok(None),
        HostCfg::Artifacts { binding, repo } => Ok(Some(bearer(env, binding, repo, "read", AGENT_TOKEN_TTL_S).await?)),
    }
}

async fn forgejo_fork(api: &str, owner: &str, repo: &str, token: &str, agent: &str) -> Result<Fork> {
    let t = FetchTransport::default();
    let name = format!("{repo}-{agent}");
    let hdrs = vec![("Authorization", format!("token {token}")), ("Content-Type", "application/json".to_string())];
    let body = serde_json::json!({ "name": name }).to_string().into_bytes();
    let url = format!("{api}/repos/{owner}/{repo}/forks");
    let gerr = |e: nerudo_git::Error| Error::RustError(e.to_string());
    let (status, resp) = t.request("POST", &url, &hdrs, Some(body)).await.map_err(gerr)?;
    let repo_json: serde_json::Value = if status == 201 || status == 202 {
        serde_json::from_slice(&resp)?
    } else {
        // Already forked (409) or forking refused: reuse or create an empty repo the agent fills.
        let (s, r) = t.request("GET", &format!("{api}/repos/{owner}/{name}"), &hdrs, None).await.map_err(gerr)?;
        if s == 200 {
            serde_json::from_slice(&r)?
        } else {
            let create = serde_json::json!({ "name": name, "default_branch": "main" }).to_string().into_bytes();
            let (s, r) = t.request("POST", &format!("{api}/user/repos"), &hdrs, Some(create)).await.map_err(gerr)?;
            if s != 201 {
                return Err(Error::RustError(format!("forgejo fork {name}: HTTP {s}: {}", String::from_utf8_lossy(&r))));
            }
            serde_json::from_slice(&r)?
        }
    };
    let remote = repo_json["clone_url"].as_str().unwrap_or_default().to_string();
    Ok(Fork { name, remote, authorization: basic(owner, token) })
}

/// One Artifacts repo, read through the binding: one tree, or one commit, at a time. Artifacts
/// answers these for a repo whose trees are too big for a git fetch (cf/scale/mono.md).
pub struct RepoReader {
    /// The repo stub; disposed when the reader drops (Rpc).
    handle: Rpc,
}

/// A commit as the binding gives it: no time zones, no extra headers, and the message without
/// its last newline. `objects.rs` makes the git object from it and checks its id.
pub struct CommitInfo {
    pub tree: String,
    pub parents: Vec<String>,
    pub message: String,
    pub author: (String, String, i64),
    pub committer: (String, String, i64),
}

/// A reader for `repo` (Artifacts only; None on other hosts).
pub async fn reader(env: &Env, host: &HostCfg, repo: &str) -> Result<Option<RepoReader>> {
    let HostCfg::Artifacts { binding, .. } = host else { return Ok(None) };
    let ns = env.get_binding::<Binding>(binding)?;
    Ok(Some(RepoReader { handle: call(&ns.0, "get", &[repo.into()]).await? }))
}

impl RepoReader {
    /// The entries of one tree: (mode, name, id). None: the repo has no such tree.
    pub async fn tree(&self, oid: &str) -> Result<Option<Vec<(u32, String, String)>>> {
        let v = call(&self.handle, "readTree", &[oid.into()]).await?;
        if v.is_null() || v.is_undefined() {
            return Ok(None);
        }
        let arr: js_sys::Array = (*v).clone().unchecked_into();
        let mut out = Vec::with_capacity(arr.length() as usize);
        for e in arr.iter() {
            let mode = u32::from_str_radix(&field(&e, "mode")?, 8).map_err(|_| Error::RustError("artifacts: bad tree mode".into()))?;
            out.push((mode, field(&e, "name")?, field(&e, "hash")?));
        }
        Ok(Some(out))
    }

    /// One commit. None: the repo has no such commit.
    pub async fn commit(&self, oid: &str) -> Result<Option<CommitInfo>> {
        let v = call(&self.handle, "readCommit", &[oid.into()]).await?;
        if v.is_null() || v.is_undefined() {
            return Ok(None);
        }
        let person = |k: &str, at: &str| -> Result<(String, String, i64)> {
            let p = js_sys::Reflect::get(&v, &k.into())?;
            let when = js_sys::Reflect::get(&v, &at.into())?.as_f64().ok_or_else(|| Error::RustError(format!("artifacts: missing {at}")))?;
            Ok((field(&p, "name")?, field(&p, "email")?, when as i64))
        };
        let parents: js_sys::Array = js_sys::Reflect::get(&v, &"parents".into())?.unchecked_into();
        Ok(Some(CommitInfo {
            tree: field(&v, "treeHash")?,
            parents: parents.iter().filter_map(|p| p.as_string()).collect(),
            message: field(&v, "message")?,
            author: person("author", "authoredAt")?,
            committer: person("committer", "committedAt")?,
        }))
    }
}

/// The Artifacts binding has no workers-rs wrapper; call it as a JS object from Rust.
struct Binding(JsValue);

impl EnvBinding for Binding {
    const TYPE_NAME: &'static str = "";
    fn get(val: JsValue) -> Result<Self> {
        Ok(Binding(val))
    }
}

impl JsCast for Binding {
    fn instanceof(_: &JsValue) -> bool {
        true
    }
    fn unchecked_from_js(val: JsValue) -> Self {
        Binding(val)
    }
    fn unchecked_from_js_ref(val: &JsValue) -> &Self {
        // SAFETY: Binding is a transparent wrapper around JsValue.
        unsafe { &*(val as *const JsValue as *const Self) }
    }
}

impl AsRef<JsValue> for Binding {
    fn as_ref(&self) -> &JsValue {
        &self.0
    }
}

impl From<Binding> for JsValue {
    fn from(b: Binding) -> Self {
        b.0
    }
}

/// Bindings are RPC stubs: every property, `apply` included, reads as a remote method. So
/// invoke through `Reflect.apply`, never `Function.prototype.apply` on the stub.
async fn call(obj: &JsValue, method: &str, args: &[JsValue]) -> Result<Rpc> {
    let f: js_sys::Function = js_sys::Reflect::get(obj, &method.into())?.unchecked_into();
    let arr = js_sys::Array::new();
    for a in args {
        arr.push(a);
    }
    let p = js_sys::Reflect::apply(&f, obj, &arr)?;
    Ok(Rpc(JsFuture::from(js_sys::Promise::from(p)).await?))
}

/// The result of a binding call. A result that is not disposed keeps its RPC session open,
/// and the Trunk object cannot leave memory while it waits: Cloudflare bills its duration
/// for up to 15 minutes after the call (cf/scale/10k.md). Dropping the result disposes it.
struct Rpc(JsValue);

impl std::ops::Deref for Rpc {
    type Target = JsValue;
    fn deref(&self) -> &JsValue {
        &self.0
    }
}

impl Drop for Rpc {
    fn drop(&mut self) {
        if !(self.0.is_object() || self.0.is_function()) {
            return;
        }
        let dispose = js_sys::Reflect::get(&js_sys::global(), &"Symbol".into())
            .and_then(|s| js_sys::Reflect::get(&s, &"dispose".into()))
            .and_then(|sym| js_sys::Reflect::get(&self.0, &sym));
        if let Ok(Ok(f)) = dispose.map(|f| f.dyn_into::<js_sys::Function>()) {
            let _ = js_sys::Reflect::apply(&f, &self.0, &js_sys::Array::new());
        }
    }
}

fn field(v: &JsValue, k: &str) -> Result<String> {
    js_sys::Reflect::get(v, &k.into())?.as_string().ok_or_else(|| Error::RustError(format!("artifacts: missing {k}")))
}

async fn bearer(env: &Env, binding: &str, repo: &str, scope: &str, ttl_s: f64) -> Result<String> {
    let ns = env.get_binding::<Binding>(binding)?;
    let handle = call(&ns.0, "get", &[repo.into()]).await?;
    let token = call(&handle, "createToken", &[scope.into(), JsValue::from_f64(ttl_s)]).await?;
    Ok(format!("Bearer {}", field(&token, "plaintext")?))
}

async fn write_token(repo: &JsValue) -> Result<String> {
    let token = call(repo, "createToken", &["write".into(), JsValue::from_f64(AGENT_TOKEN_TTL_S)]).await?;
    Ok(format!("Bearer {}", field(&token, "plaintext")?))
}

/// Make a new fork for the agent and mint its write token. A token is only ever minted for
/// a fork this call created: a name that already exists is skipped, never reused, so no
/// agent can be handed a repo some other trunk (or anyone else) made. fork() sometimes
/// fails with INTERNAL_ERROR when several agents fork at once, and the half-made repo it
/// leaves never becomes ready; the next name is tried instead.
async fn artifacts_fork(env: &Env, binding: &str, repo: &str, agent: &str) -> Result<Fork> {
    let ns = env.get_binding::<Binding>(binding)?;
    let handle = call(&ns.0, "get", &[repo.into()]).await?;
    let opts = js_sys::Object::new();
    js_sys::Reflect::set(&opts, &"defaultBranchOnly".into(), &JsValue::TRUE)?;
    let mut last = String::new();
    for attempt in 0..4 {
        let name = match attempt {
            0 => format!("{repo}--{agent}"),
            n => format!("{repo}--{agent}-{n}"),
        };
        // fork() returns {name, remote} but no token.
        let remote = match call(&handle, "fork", &[name.clone().into(), opts.clone().into()]).await {
            Ok(forked) => field(&forked, "remote")?,
            Err(e) => {
                console_warn!("artifacts fork {name}: {e}");
                last = format!("{name}: {e}");
                Delay::from(Duration::from_millis(500)).await;
                continue;
            }
        };
        // get() throws until the fork is ready.
        for _ in 0..40 {
            if let Ok(fork) = call(&ns.0, "get", &[name.clone().into()]).await {
                return Ok(Fork { name, remote, authorization: write_token(&fork).await? });
            }
            Delay::from(Duration::from_millis(250)).await;
        }
        console_warn!("artifacts fork {name}: not ready after 10 s");
        last = format!("{name}: not ready after 10 s");
    }
    Err(Error::RustError(format!("artifacts fork for {agent} failed; last: {last}")))
}

/// Make repo `to` as a fork of repo `from` (Artifacts only), and mint a write token for it.
/// Returns its remote and that token (an Authorization header). Artifacts copies the objects
/// itself, so no file passes through the caller.
pub async fn fork_repo(env: &Env, host: &HostCfg, from: &str, to: &str) -> Result<(String, String)> {
    let HostCfg::Artifacts { binding, .. } = host else {
        return Err(Error::RustError("a fork needs the Artifacts host".into()));
    };
    let ns = env.get_binding::<Binding>(binding)?;
    let handle = call(&ns.0, "get", &[from.into()]).await?;
    let opts = js_sys::Object::new();
    js_sys::Reflect::set(&opts, &"defaultBranchOnly".into(), &JsValue::TRUE)?;
    let forked = call(&handle, "fork", &[to.into(), opts.into()]).await?;
    let remote = field(&forked, "remote")?;
    // get() throws until the fork is ready; a big repo takes a minute or more.
    for _ in 0..600 {
        if let Ok(repo) = call(&ns.0, "get", &[to.into()]).await {
            return Ok((remote, write_token(&repo).await?));
        }
        Delay::from(Duration::from_millis(500)).await;
    }
    Err(Error::RustError(format!("fork {to}: not ready after 300 s")))
}

/// A short read-only credential for an agent's fork, for the object's own reads (the
/// agent's context). Forgejo forks keep the token the object already holds.
pub async fn fork_read_authorization(env: &Env, host: &HostCfg, fork: &Fork) -> Result<String> {
    match host {
        HostCfg::Forgejo { .. } => Ok(fork.authorization.clone()),
        HostCfg::Artifacts { binding, .. } => bearer(env, binding, &fork.name, "read", READ_TOKEN_TTL_S).await,
    }
}

/// The object keeps a fork read token for 9 of its 10 minutes.
pub const READ_TOKEN_TTL_S: f64 = 600.0;

/// Create a repo the object owns besides trunk (the context repo): only the object can
/// write it. Returns its remote and a write token the object keeps; the creation token is
/// revoked. A name that already exists is refused, never adopted: the object only writes
/// repos it made.
pub async fn create_owned_repo(env: &Env, host: &HostCfg, name: &str) -> Result<(String, String)> {
    let HostCfg::Artifacts { binding, .. } = host else {
        return Err(Error::RustError("a repo the sequencer owns needs the Artifacts host".into()));
    };
    let ns = env.get_binding::<Binding>(binding)?;
    let opts = js_sys::Object::new();
    js_sys::Reflect::set(&opts, &"setDefaultBranch".into(), &"main".into())?;
    let created = call(&ns.0, "create", &[name.into(), opts.into()]).await?;
    let remote = field(&created, "remote")?;
    let creation = field(&created, "token")?;
    let mut write = Err(Error::RustError(format!("{name}: no write token")));
    for _ in 0..20 {
        write = bearer(env, binding, name, "write", TRUNK_TOKEN_TTL_S).await;
        if write.is_ok() {
            break;
        }
        Delay::from(Duration::from_millis(250)).await;
    }
    let write = write?;
    let handle = call(&ns.0, "get", &[name.into()]).await?;
    if let Err(e) = call(&handle, "revokeToken", &[creation.into()]).await {
        console_warn!("{name}: revoking the creation token failed: {e:?}");
    }
    Ok((remote, write))
}

/// A short read-only credential for a repo the object owns (the context repo).
pub async fn repo_read_authorization(env: &Env, host: &HostCfg, name: &str) -> Result<String> {
    match host {
        HostCfg::Artifacts { binding, .. } => bearer(env, binding, name, "read", READ_TOKEN_TTL_S).await,
        HostCfg::Forgejo { .. } => Err(Error::RustError("a repo the sequencer owns needs the Artifacts host".into())),
    }
}

