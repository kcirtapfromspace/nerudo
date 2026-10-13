//! The Hub: names for trunks, as on a git host. A repo is `{owner}/{repo}`, and the owner is
//! an org or a user; its trunk id (the Trunk Durable Object's name and the Artifacts repo's
//! name) stays opaque, and the Hub maps one to the other. Orgs and users share one set of
//! names. Teams are not stored: a team is a role in a repo's nerudo.toml, so an org's teams
//! are the union over its repos, read from each trunk's summary.
//!
//! A user is one name across every repo: the approvers in each nerudo.toml with that name
//! (and, when the user has a key, that key) are this user. One sign-in key (the account key)
//! opens every repo the user is a person on. The approvers and their keys stay in
//! nerudo.toml, which is what seals verify against; the Hub only binds a name to a key.
//!
//! One Hub object (`id_from_name("hub")`) keeps the names in Durable Object key-value storage:
//!
//!   org:{org}           {org, title, description, created_ms}
//!   user:{user}         {user, title, key, created_ms}
//!   repo:{owner}/{repo} {org, repo, trunk, description, upstream, public, created_ms}
//!   trunk:{trunk}       "{owner}/{repo}"
//!
//! The object answers only the front Worker, on paths outside /v1, so a client can't reach
//! it. The front Worker (the functions below `Hub`) serves /v1/hub/..., the
//! /v1/repos/{org}/{repo}/... alias, and git by name (/{org}/{repo}.git/...). Without the
//! admin token or a signed-in repo member, only public repos exist.

use futures::{future::join_all, stream, StreamExt};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use worker::*;

/// Names an org can't have: top-level paths of the Worker and of the web app.
const RESERVED: &[&str] = &["t", "v1", "app", "api", "explore", "orgs", "settings", "login", "new", "assets", "static", "_next", "favicon.ico", "robots.txt"];
/// Repos whose summary an org page reads, at most.
const MAX_SUMMARIES: usize = 50;
const ORG_RULE: &str = "an org or user name is 1 to 39 of a-z, 0-9 and -, and starts with a letter or a digit";
const REPO_RULE: &str = "a repo name is 1 to 100 of A-Z, a-z, 0-9, ., _ and -, is not . or .., and does not end in .git";

pub(crate) fn valid_org(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty() && b.len() <= 39 && b[0] != b'-' && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-') && !RESERVED.contains(&s)
}

pub(crate) fn valid_repo(s: &str) -> bool {
    !s.is_empty() && s.len() <= 100 && s != "." && s != ".." && !s.ends_with(".git") && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

/// A trunk id as a name can point to it.
pub(crate) fn valid_trunk(s: &str) -> bool {
    !s.is_empty() && s.len() <= 100 && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct Org {
    pub org: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub created_ms: i64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct User {
    pub user: String,
    #[serde(default)]
    pub title: String,
    /// The user's approver key (ed25519, hex), as nerudo.toml lists it. A repo whose
    /// approver of this name has another key names someone else.
    #[serde(default)]
    pub key: Option<String>,
    pub created_ms: i64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct Repo {
    pub org: String,
    pub repo: String,
    pub trunk: String,
    #[serde(default)]
    pub description: String,
    /// The repo it replays, like "https://github.com/cloudflare/agents".
    #[serde(default)]
    pub upstream: Option<String>,
    pub public: bool,
    pub created_ms: i64,
}

impl Repo {
    fn name(&self) -> String {
        format!("{}/{}", self.org, self.repo)
    }
}

#[durable_object]
pub struct Hub {
    state: State,
}

impl DurableObject for Hub {
    fn new(state: State, _env: Env) -> Self {
        Self { state }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        let path = req.path();
        let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
        match (req.method(), parts.as_slice()) {
            (Method::Post, ["search-candidates"]) => {
                let params: BTreeMap<String, String> = req.json().await?;
                let options = match crate::hub_search::Options::parse(&params) { Ok(options) => options, Err(error) => return err(400, error) };
                let orgs: Vec<Org> = self.list("org:").await?;
                let users: Vec<User> = self.list("user:").await?;
                let repos: Vec<Repo> = self.list("repo:").await?;
                let trunks = repos.iter().filter(|repo| options.accepts_repo(repo)).map(|repo| repo.trunk.clone()).collect();
                let shortlist = crate::hub_index::candidates(&self.state.storage().sql(), &trunks, &options)?;
                Response::from_json(&json!({ "orgs":orgs, "users":users, "repos":repos, "file_candidates":shortlist.files, "pending":shortlist.pending }))
            }
            (Method::Put, ["search-index", trunk]) => {
                let publication: crate::hub_index::Publication = req.json().await?;
                if let Err(error) = publication.validate(trunk) { return err(400,error); }
                let accepted = crate::hub_index::publish(&self.state.storage().sql(), &publication)?;
                Response::from_json(&json!({ "accepted":accepted }))
            }
            (Method::Post, ["search-index", trunk, "dirty"]) => {
                let body: Value = req.json().await?;
                let head = body["head"].as_str().unwrap_or_default();
                let seq = body["seq"].as_u64().unwrap_or(0);
                if !valid_trunk(trunk) || !crate::hub_index::valid_head(head) || seq > 9_007_199_254_740_991 { return err(400,"Invalid search index generation."); }
                crate::hub_index::dirty(&self.state.storage().sql(), trunk, head, seq)?;
                Response::from_json(&json!({ "accepted":true }))
            }
            (Method::Get, ["orgs"]) => {
                let mut orgs: Vec<Org> = self.list("org:").await?;
                if orgs.iter().any(|o| o.org == "nerudo") { orgs.retain(|o| o.org != "landseq"); }
                let users: Vec<User> = self.list("user:").await?;
                let repos: Vec<Repo> = self.list("repo:").await?;
                Response::from_json(&json!({ "orgs": orgs, "users": users, "repos": repos }))
            }
            (Method::Get, ["org", org]) => {
                if let Some(new) = crate::brand::owner_alias(org) {
                    if let Some(o) = self.get::<Org>(&format!("org:{new}")).await? { return found(Some(o)); }
                }
                found(self.get::<Org>(&format!("org:{org}")).await?)
            }
            (Method::Get, ["user", user]) => found(self.get::<User>(&format!("user:{user}")).await?),
            (Method::Put, ["user", user]) => {
                if self.get::<Org>(&format!("org:{user}")).await?.is_some() {
                    return err(409, format!("{user} is an org; orgs and users share names"));
                }
                let body: Value = req.json().await?;
                let mut u = self.get::<User>(&format!("user:{user}")).await?.unwrap_or_else(|| User { user: user.to_string(), created_ms: now(), ..Default::default() });
                if let Some(t) = body["title"].as_str() {
                    u.title = t.to_string();
                }
                match &body["key"] {
                    Value::String(k) => u.key = Some(k.clone()),
                    Value::Null if body.get("key").is_some() => u.key = None,
                    _ => {}
                }
                self.put(&format!("user:{user}"), &u).await?;
                Response::from_json(&u)
            }
            (Method::Put, ["org", org]) => {
                if self.get::<User>(&format!("user:{org}")).await?.is_some() {
                    return err(409, format!("{org} is a user; orgs and users share names"));
                }
                let body: Value = req.json().await?;
                let mut o = self.get::<Org>(&format!("org:{org}")).await?.unwrap_or_else(|| Org { org: org.to_string(), created_ms: now(), ..Default::default() });
                if let Some(t) = body["title"].as_str() {
                    o.title = t.to_string();
                }
                if let Some(d) = body["description"].as_str() {
                    o.description = d.to_string();
                }
                self.put(&format!("org:{org}"), &o).await?;
                Response::from_json(&o)
            }
            (Method::Get, ["repos", org]) => Response::from_json(&self.list::<Repo>(&format!("repo:{org}/")).await?),
            (Method::Get, ["repo", org, repo]) => {
                if let Some((owner, name)) = crate::brand::repo_alias(org, repo) {
                    if let Some(r) = self.get::<Repo>(&format!("repo:{owner}/{name}")).await? { return found(Some(r)); }
                }
                found(self.get::<Repo>(&format!("repo:{org}/{repo}")).await?)
            }
            (Method::Get, ["trunk", trunk]) => match self.get::<String>(&format!("trunk:{trunk}")).await? {
                Some(name) => found(self.get::<Repo>(&format!("repo:{name}")).await?),
                None => found::<Repo>(None),
            },
            (Method::Put, ["repo", _, _]) => {
                let r: Repo = req.json().await?;
                let name = r.name();
                let mut gone = vec![];
                // The name moves to another trunk: the old trunk has no name any more.
                let mut unnamed = vec![];
                if let Some(old) = self.get::<Repo>(&format!("repo:{name}")).await?.filter(|o| o.trunk != r.trunk) {
                    if self.get::<String>(&format!("trunk:{}", old.trunk)).await?.as_deref() == Some(name.as_str()) {
                        gone.push(format!("trunk:{}", old.trunk));
                        unnamed.push(old.trunk);
                    }
                }
                // The trunk had another name: it is renamed, and the old name is free.
                let renamed_from = self.get::<String>(&format!("trunk:{}", r.trunk)).await?.filter(|n| *n != name);
                if let Some(n) = &renamed_from {
                    gone.push(format!("repo:{n}"));
                }
                let mut items = vec![(format!("repo:{name}"), serde_json::to_string(&r)?), (format!("trunk:{}", r.trunk), serde_json::to_string(&name)?)];
                // A new owner is an org, unless a user has the name.
                if self.get::<Org>(&format!("org:{}", r.org)).await?.is_none() && self.get::<User>(&format!("user:{}", r.org)).await?.is_none() {
                    items.push((format!("org:{}", r.org), serde_json::to_string(&Org { org: r.org.clone(), created_ms: r.created_ms, ..Default::default() })?));
                }
                self.put_all(items).await?;
                if !gone.is_empty() {
                    self.state.storage().delete_multiple(gone).await?;
                }
                Response::from_json(&json!({ "repo": r, "unnamed": unnamed, "renamed_from": renamed_from }))
            }
            (Method::Delete, ["repo", org, repo]) => {
                let name = format!("{org}/{repo}");
                let Some(r) = self.get::<Repo>(&format!("repo:{name}")).await? else {
                    return found::<Repo>(None);
                };
                let mut gone = vec![format!("repo:{name}")];
                if self.get::<String>(&format!("trunk:{}", r.trunk)).await?.as_deref() == Some(name.as_str()) {
                    gone.push(format!("trunk:{}", r.trunk));
                }
                self.state.storage().delete_multiple(gone).await?;
                Response::from_json(&r)
            }
            _ => Response::error("no such hub route", 404),
        }
    }
}

impl Hub {
    async fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let raw: Option<String> = self.state.storage().get(key).await?;
        Ok(raw.and_then(|s| serde_json::from_str(&s).ok()))
    }

    async fn put<T: Serialize>(&self, key: &str, v: &T) -> Result<()> {
        self.put_all(vec![(key.to_string(), serde_json::to_string(v)?)]).await
    }

    /// Write several keys in one atomic storage operation.
    async fn put_all(&self, items: Vec<(String, String)>) -> Result<()> {
        let obj = js_sys::Object::new();
        for (k, v) in items {
            js_sys::Reflect::set(&obj, &k.into(), &v.into())?;
        }
        self.state.storage().put_multiple_raw(obj).await
    }

    async fn list<T: DeserializeOwned>(&self, prefix: &str) -> Result<Vec<T>> {
        let map = self.state.storage().list_with_options(ListOptions::new().prefix(prefix)).await?;
        let mut out = vec![];
        for entry in map.entries() {
            let pair: js_sys::Array = entry?.into();
            if let Some(v) = pair.get(1).as_string().and_then(|v| serde_json::from_str(&v).ok()) {
                out.push(v);
            }
        }
        Ok(out)
    }
}

fn found<T: Serialize>(v: Option<T>) -> Result<Response> {
    match v {
        Some(v) => Response::from_json(&v),
        None => Response::error("not found", 404),
    }
}

fn now() -> i64 {
    Date::now().as_millis() as i64
}

fn err(status: u16, msg: impl Into<String>) -> Result<Response> {
    Ok(Response::from_json(&json!({ "error": msg.into() }))?.with_status(status))
}

/// One line of text from a request: no control characters, trimmed, at most `max` characters.
fn line(s: &str, max: usize) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>().trim().chars().take(max).collect()
}

// -------------------------------------------------------------------- the front Worker

/// Ask the Hub object. 404 comes back as None.
pub(crate) async fn hub<T: DeserializeOwned>(env: &Env, method: Method, path: &str, body: Option<&Value>) -> Result<Option<T>> {
    let stub = env.durable_object("HUB")?.id_from_name("hub")?.get_stub()?;
    let mut init = RequestInit::new();
    init.with_method(method).with_body(body.map(|b| b.to_string().into()));
    let mut resp = stub.fetch_with_request(Request::new_with_init(&format!("https://hub.internal/{path}"), &init)?).await?;
    match resp.status_code() {
        404 => Ok(None),
        200 => Ok(Some(resp.json().await?)),
        s => Err(Error::RustError(format!("hub {path}: HTTP {s}: {}", resp.text().await.unwrap_or_default()))),
    }
}

/// The repo with this name, public or not.
pub(crate) async fn lookup(env: &Env, org: &str, repo: &str) -> Result<Option<Repo>> {
    if !valid_org(org) || !valid_repo(repo) {
        return Ok(None);
    }
    hub(env, Method::Get, &format!("repo/{org}/{repo}"), None).await
}

/// The name of a trunk, public or not.
pub(crate) async fn name_of(env: &Env, trunk: &str) -> Result<Option<Repo>> {
    if !valid_trunk(trunk) {
        return Ok(None);
    }
    hub(env, Method::Get, &format!("trunk/{trunk}"), None).await
}

/// Call a trunk's API as the admin, from inside Cloudflare.
async fn trunk_call(env: &Env, trunk: &str, method: Method, path: &str, body: Option<&Value>) -> Result<(u16, Value)> {
    let headers = Headers::new();
    headers.set("authorization", &format!("Bearer {}", crate::secret(env, "NERUDO_ADMIN_TOKEN")?))?;
    let mut init = RequestInit::new();
    init.with_method(method).with_headers(headers).with_body(body.map(|b| b.to_string().into()));
    let req = Request::new_with_init(&format!("https://trunk.internal/v1/trunks/{trunk}/{path}"), &init)?;
    let mut resp = env.durable_object("TRUNK")?.id_from_name(trunk)?.get_stub()?.fetch_with_request(req).await?;
    Ok((resp.status_code(), resp.json().await.unwrap_or(Value::Null)))
}

/// A trunk's summary, or null when it does not answer.
async fn summary(env: &Env, trunk: &str) -> Value {
    match trunk_call(env, trunk, Method::Get, "summary", None).await {
        Ok((200, v)) => v,
        _ => Value::Null,
    }
}

/// RepoView: the record and the trunk's summary.
async fn view(env: &Env, r: &Repo) -> Result<Value> {
    let mut v = serde_json::to_value(r)?;
    v["summary"] = summary(env, &r.trunk).await;
    Ok(v)
}

/// Discovery uses the same person check as opening the repo. A profile name or repo
/// owner alone grants nothing; share and per-repo keys are not account sign-in keys.
struct Viewer {
    admin: bool,
    account: Option<(String, String)>,
}

impl Viewer {
    async fn from_request(env: &Env, req: &Request) -> Result<Self> {
        let name = crate::request_header(&req, "x-nerudo-user")?.unwrap_or_default();
        let key = crate::request_header(&req, "x-nerudo-key")?.unwrap_or_default();
        let account = if valid_org(&name)
            && crate::trunk::same_key(key.as_bytes(), crate::trunk::account_key(env, &name)?.as_bytes())
            && user(env, &name).await?.is_some()
        {
            Some((name, key))
        } else {
            None
        };
        Ok(Self { admin: crate::bearer_ok(env, req, "NERUDO_ADMIN_TOKEN"), account })
    }

    async fn can_see(&self, env: &Env, r: &Repo) -> bool {
        if self.admin || r.public {
            return true;
        }
        let Some((name, key)) = &self.account else { return false };
        let check = async {
            let headers = Headers::new();
            headers.set("x-nerudo-user", name)?;
            headers.set("x-nerudo-key", key)?;
            let mut init = RequestInit::new();
            init.with_method(Method::Get).with_headers(headers);
            let req = Request::new_with_init(&format!("https://trunk.internal/t/{}/api/access", r.trunk), &init)?;
            let mut resp = env.durable_object("TRUNK")?.id_from_name(&r.trunk)?.get_stub()?.fetch_with_request(req).await?;
            Ok::<bool, Error>(resp.status_code() == 200 && resp.json::<Value>().await?["person"] == true)
        };
        check.await.unwrap_or(false)
    }

    async fn repos(&self, env: &Env, repos: Vec<Repo>) -> Vec<Repo> {
        // Bound simultaneous cross-object checks, and preserve the directory's ordering.
        stream::iter(repos.into_iter().map(|r| async move {
            self.can_see(env, &r).await.then_some(r)
        })).buffered(16).filter_map(|r| async move { r }).collect().await
    }
}

/// `/v1/hub/...`; `rest` is the path after /v1/hub.
pub async fn api(mut req: Request, env: &Env, rest: &[&str]) -> Result<Response> {
    let viewer = Viewer::from_request(env, &req).await?;
    let admin = viewer.admin;
    let mut response = match (req.method(), rest) {
        (Method::Get, ["orgs"]) => orgs(env, &viewer).await,
        (Method::Get, ["search"]) => global_search(env, &viewer, &req).await,
        // An owner's page: an org, or a user's profile.
        (Method::Get, ["orgs", owner]) => match user(env, owner).await? {
            Some(u) => user_view(env, &u, &viewer).await,
            None => org_view(env, owner, &viewer).await,
        },
        (Method::Get, ["users", name]) => match user(env, name).await? {
            Some(u) => user_view(env, &u, &viewer).await,
            None => err(404, format!("unknown user {name}")),
        },
        (Method::Get, ["me"]) => me(env, &req).await,
        (Method::Get, ["repos", org, repo]) => match lookup(env, org, repo).await? {
            Some(r) if viewer.can_see(env, &r).await => Response::from_json(&view(env, &r).await?),
            _ => err(404, format!("unknown repo {org}/{repo}")),
        },
        (Method::Get, ["trunks", trunk]) => match name_of(env, trunk).await? {
            Some(r) if viewer.can_see(env, &r).await => Response::from_json(&view(env, &r).await?),
            _ => err(404, format!("trunk {trunk} has no name")),
        },
        (Method::Put, ["orgs", org]) if admin => {
            let body: Value = req.json().await.unwrap_or(Value::Null);
            put_org(env, org, &body).await
        }
        (Method::Put, ["repos", org, repo]) if admin => {
            let body: Value = req.json().await.unwrap_or(Value::Null);
            put_repo(env, org, repo, &body).await
        }
        (Method::Delete, ["repos", org, repo]) if admin => delete_repo(env, org, repo).await,
        (Method::Put, ["users", name]) if admin => {
            let body: Value = req.json().await.unwrap_or(Value::Null);
            put_user(env, name, &body).await
        }
        (Method::Post, ["users", name, "invite"]) if admin => invite(env, name).await,
        (Method::Put | Method::Delete | Method::Post, _) if !admin => err(401, "missing or wrong bearer token"),
        _ => err(404, "no such route; see GET /v1/hub/orgs"),
    }?;
    response.headers_mut().set("cache-control", "private, no-store")?;
    response.headers_mut().set("vary", "X-Nerudo-User, X-Nerudo-Key, Authorization")?;
    Ok(response)
}

/// Every visible repo, plus owner counts computed only from those visible repos.
async fn orgs(env: &Env, viewer: &Viewer) -> Result<Response> {
    let all: Value = hub(env, Method::Get, "orgs", None).await?.unwrap_or(Value::Null);
    let orgs: Vec<Org> = serde_json::from_value(all["orgs"].clone()).unwrap_or_default();
    let users: Vec<User> = serde_json::from_value(all["users"].clone()).unwrap_or_default();
    let repos: Vec<Repo> = serde_json::from_value(all["repos"].clone()).unwrap_or_default();
    let repos = viewer.repos(env, repos).await;
    let count = |owner: &str| repos.iter().filter(|r| r.org == owner).count();
    let mut out = vec![];
    for o in orgs {
        let n = count(&o.org);
        if viewer.admin || n > 0 {
            out.push(json!({ "org": o.org, "title": o.title, "description": o.description, "repos": n }));
        }
    }
    // Every user has a profile, with or without repos of their own.
    let users: Vec<Value> = users.iter().map(|u| json!({ "user": u.user, "title": u.title, "repos": count(&u.user) })).collect();
    let repos: Vec<Value> = repos.iter().map(|r| {
        let mut v = serde_json::to_value(r).expect("repo is serializable");
        v["summary"] = Value::Null;
        v
    }).collect();
    Response::from_json(&json!({ "orgs": out, "users": users, "repos": repos }))
}

/// One field searches the public directory and only repositories this viewer can
/// currently read. No administrator credential is forwarded to content searches.
async fn global_search(env: &Env, viewer: &Viewer, req: &Request) -> Result<Response> {
    use crate::hub_search::{self as search, Outcome};
    let params: BTreeMap<String, String> = req.url()?.query_pairs().map(|(key, value)| (key.into_owned(), value.into_owned())).collect();
    let mut options = match search::Options::parse(&params) { Ok(options) => options, Err(error) => return err(400, error) };
    let scope = format!("{}:{}", viewer.admin, viewer.account.as_ref().map(|(name, _)| name.as_str()).unwrap_or("anonymous"));
    let cursor = match search::cursor(params.get("cursor").map(String::as_str), &options, &scope) { Ok(cursor) => cursor, Err(error) => return err(400, error) };
    // The Hub's durable postings select repositories without opening Git packs.
    // The shortlist is internal and grants no access to its private entries.
    let all: Value = match hub(env, Method::Post, "search-candidates", Some(&serde_json::to_value(&params)?)).await? {
        Some(directory) => directory,
        // During a code rollout a newly updated Worker can reach the previous
        // Hub implementation. Retain complete coverage until it has the index.
        None => {
            let mut directory: Value = hub(env, Method::Get, "orgs", None).await?.unwrap_or(Value::Null);
            let trunks: Vec<Value> = directory["repos"].as_array().into_iter().flatten().filter_map(|repo| repo.get("trunk").cloned()).collect();
            directory["file_candidates"] = json!(trunks);
            directory["pending"] = directory["file_candidates"].clone();
            directory
        }
    };
    let orgs: Vec<Org> = serde_json::from_value(all["orgs"].clone()).unwrap_or_default();
    let users: Vec<User> = serde_json::from_value(all["users"].clone()).unwrap_or_default();
    let candidates: Vec<Repo> = serde_json::from_value(all["repos"].clone()).unwrap_or_default();
    let file_candidates: BTreeSet<String> = serde_json::from_value(all["file_candidates"].clone()).unwrap_or_default();
    let pending: BTreeSet<String> = serde_json::from_value(all["pending"].clone()).unwrap_or_default();
    let mut relevant = options.metadata_candidates(&orgs, &users, &candidates);
    relevant.extend(file_candidates.clone());
    options.set_file_candidates(file_candidates);
    // Private permission checks are also bounded and timed. Failed checks are
    // invisible: their names, counts and errors never enter the aggregation.
    let checks: Vec<_> = stream::iter(candidates.iter().filter(|r| options.accepts_repo(r) && relevant.contains(&r.trunk)).map(|repo| async move {
        match search_within(search_can_see(env, viewer, repo)).await {
            Some(Ok(true)) => (Some(search::repo_name(repo)), false),
            Some(Ok(false)) => (None, false),
            Some(Err(_)) | None => (None, true),
        }
    })).buffered(16).collect().await;
    let directory_incomplete = checks.iter().any(|(_, incomplete)| *incomplete);
    let allowed: BTreeSet<_> = checks.into_iter().filter_map(|(name, _)| name).collect();
    let repos = search::authorized_repos(&candidates, &allowed, &options);
    let page = search::batch(&repos, &options, &cursor);
    let outcomes: BTreeMap<_, _> = stream::iter(page.iter().map(|repo| {
        let options = &options;
        async move {
            let outcome = search_within(search_repo(env, viewer, repo, &options.params)).await.unwrap_or(Outcome::Failed);
            (search::repo_name(repo), outcome)
        }
    })).buffered(search::CONCURRENCY).collect().await;
    let mut result = search::aggregate(&orgs, &users, &repos, &page, &outcomes, &options, &cursor, &scope);
    search::mark_directory_incomplete(&mut result, directory_incomplete);
    let index_pending = repos.iter().any(|repo| options.file_candidate(repo) && match outcomes.get(&search::repo_name(repo)) {
        Some(Outcome::Denied) => false,
        Some(Outcome::Search(response)) => pending.contains(&repo.trunk)
            || response["stats"]["index_pending"].as_bool().unwrap_or(false)
            || response["stats"]["index_stale"].as_bool().unwrap_or(false),
        _ => pending.contains(&repo.trunk),
    });
    result["stats"]["index_pending"] = json!(index_pending);
    if index_pending { result["stats"]["partial"] = json!(true); }
    Response::from_json(&result)
}

async fn search_can_see(env: &Env, viewer: &Viewer, repo: &Repo) -> Result<bool> {
    if viewer.admin || repo.public { return Ok(true); }
    let Some((name, key)) = &viewer.account else { return Ok(false); };
    let headers = Headers::new();
    headers.set("x-nerudo-user", name)?;
    headers.set("x-nerudo-key", key)?;
    let mut init = RequestInit::new();
    init.with_method(Method::Get).with_headers(headers);
    let request = Request::new_with_init(&format!("https://trunk.internal/t/{}/api/access", repo.trunk), &init)?;
    let mut response = env.durable_object("TRUNK")?.id_from_name(&repo.trunk)?.get_stub()?.fetch_with_request(request).await?;
    match response.status_code() {
        200 => Ok(response.json::<Value>().await?["person"] == true),
        403 | 404 => Ok(false),
        _ => Err(Error::RustError("repository permission check unavailable".into())),
    }
}

async fn search_within<F: std::future::Future<Output = T>, T>(future: F) -> Option<T> {
    use futures::future::{select, Either};
    let future = std::pin::pin!(future);
    let timer = std::pin::pin!(Delay::from(std::time::Duration::from_millis(crate::hub_search::TIMEOUT_MS)));
    match select(future, timer).await { Either::Left((value, _)) => Some(value), Either::Right(_) => None }
}

async fn search_repo(env: &Env, viewer: &Viewer, repo: &Repo, params: &BTreeMap<String, String>) -> crate::hub_search::Outcome {
    use crate::hub_search::Outcome;
    let call = async {
        let mut url = Url::parse(&format!("https://trunk.internal/t/{}/api/search", repo.trunk)).map_err(|e| Error::RustError(e.to_string()))?;
        url.query_pairs_mut().extend_pairs(params);
        let request = |identity: bool| -> Result<Request> {
            let headers = Headers::new();
            if identity {
                if let Some((name, key)) = &viewer.account { headers.set("x-nerudo-user", name)?; headers.set("x-nerudo-key", key)?; }
            }
            let mut init = RequestInit::new();
            init.with_method(Method::Get).with_headers(headers);
            Request::new_with_init(url.as_str(), &init)
        };
        let stub = env.durable_object("TRUNK")?.id_from_name(&repo.trunk)?.get_stub()?;
        let mut response = stub.fetch_with_request(request(true)?).await?;
        // Public fallback sends no identity and still traverses the trunk's current
        // read gate; a repo made private between calls continues to refuse access.
        if response.status_code() == 403 && repo.public && viewer.account.is_some() {
            response = stub.fetch_with_request(request(false)?).await?;
        }
        match response.status_code() {
            403 | 404 => Ok::<Outcome, Error>(Outcome::Denied),
            200 => {
                let value: Value = response.json().await?;
                Ok(if value["files"].is_array() && value["commit"].is_string() { Outcome::Search(value) } else { Outcome::Failed })
            }
            _ => Ok(Outcome::Failed),
        }
    };
    call.await.unwrap_or(Outcome::Failed)
}

/// The user with this name.
pub(crate) async fn user(env: &Env, name: &str) -> Result<Option<User>> {
    if !valid_org(name) {
        return Ok(None);
    }
    hub(env, Method::Get, &format!("user/{name}"), None).await
}

/// A user's profile: the repos they own, and every repo that lists them as a person (an
/// approver with their name, or with their key under another name), with the orgs and
/// teams (roles) that come from those repos.
async fn user_view(env: &Env, u: &User, viewer: &Viewer) -> Result<Response> {
    let all: Value = hub(env, Method::Get, "orgs", None).await?.unwrap_or(Value::Null);
    let orgs: Vec<Org> = serde_json::from_value(all["orgs"].clone()).unwrap_or_default();
    let repos: Vec<Repo> = serde_json::from_value(all["repos"].clone()).unwrap_or_default();
    let repos = viewer.repos(env, repos).await;
    let summaries = join_all(repos.iter().enumerate().map(|(i, r)| async move {
        if i < MAX_SUMMARIES { summary(env, &r.trunk).await } else { Value::Null }
    })).await;
    let mut owned = vec![];
    let mut on = vec![];
    let mut teams: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    let mut member_of: BTreeSet<String> = BTreeSet::new();
    for (r, s) in repos.iter().zip(summaries) {
        let keys = s["keys"].as_object();
        let key_of = |name: &str| keys.and_then(|k| k.get(name)).and_then(Value::as_str).map(str::to_string);
        // The name this repo knows the user by: their own, or another with their key.
        let local = match (&u.key, s["people"].get(&u.user)) {
            (_, Some(_)) => Some(u.user.clone()),
            (Some(k), None) => s["people"].as_object().into_iter().flatten().map(|(n, _)| n.clone()).find(|n| key_of(n).as_deref() == Some(k.as_str())),
            (None, None) => None,
        };
        if let Some(local) = local {
            let roles: Vec<String> = s["people"][&local].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
            // Same key: this is the user. Another key: someone else with the same name. No key:
            // the repo lists the name of someone who has not joined nerudo.
            let verified = match (&u.key, key_of(&local)) {
                (Some(k), Some(theirs)) => Some(theirs == *k),
                _ => None,
            };
            // Teams and orgs belong to orgs; a user's own repo has neither.
            if verified != Some(false) && orgs.iter().any(|o| o.org == r.org) {
                for role in &roles {
                    teams.entry((r.org.clone(), role.clone())).or_default().insert(r.repo.clone());
                }
                member_of.insert(r.org.clone());
            }
            on.push(json!({ "org": r.org, "repo": r.repo, "roles": roles, "as": (local != u.user).then_some(local), "verified": verified }));
        }
        if r.org == u.user {
            let mut v = serde_json::to_value(r)?;
            v["summary"] = s;
            owned.push(v);
        }
    }
    Response::from_json(&json!({
        "kind": "user", "user": u.user, "title": u.title, "created_ms": u.created_ms, "key": u.key,
        "repos": owned, "on": on,
        "orgs": orgs.iter().filter(|o| member_of.contains(&o.org)).map(|o| json!({ "org": o.org, "title": o.title })).collect::<Vec<_>>(),
        "teams": teams.into_iter().map(|((org, team), repos)| json!({ "org": org, "team": team, "repos": repos })).collect::<Vec<_>>(),
    }))
}

/// `PUT /v1/hub/users/{name} {title?, key?}` (admin). `key` is the user's approver key,
/// 64 hex characters, or null.
async fn put_user(env: &Env, name: &str, body: &Value) -> Result<Response> {
    if let Some(p) = org_problem(name) {
        return err(400, p);
    }
    let mut patch = json!({});
    if let Some(t) = body["title"].as_str() {
        patch["title"] = json!(line(t, 100));
    }
    match &body["key"] {
        Value::String(k) if k.len() == 64 && k.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) => patch["key"] = json!(k),
        Value::String(_) => return err(400, "key: 64 lowercase hex characters (nerudo pubkey)"),
        Value::Null if body.get("key").is_some() => patch["key"] = Value::Null,
        _ => {}
    }
    pass(env, Method::Put, &format!("user/{name}"), &patch).await
}

/// Ask the Hub object and answer with what it says, its status too (a 409 stays a 409).
async fn pass(env: &Env, method: Method, path: &str, body: &Value) -> Result<Response> {
    let stub = env.durable_object("HUB")?.id_from_name("hub")?.get_stub()?;
    let mut init = RequestInit::new();
    init.with_method(method).with_body(Some(body.to_string().into()));
    let mut resp = stub.fetch_with_request(Request::new_with_init(&format!("https://hub.internal/{path}"), &init)?).await?;
    let status = resp.status_code();
    Ok(Response::from_json(&resp.json::<Value>().await?)?.with_status(status))
}

/// `POST /v1/hub/users/{name}/invite` (admin): the user's sign-in link for the web app. The
/// key in it opens every repo that lists the user as a person.
async fn invite(env: &Env, name: &str) -> Result<Response> {
    if user(env, name).await?.is_none() {
        return err(404, format!("unknown user {name}: make it with PUT /v1/hub/users/{name}"));
    }
    let key = crate::trunk::account_key(env, name)?;
    let web = crate::variable(env, "NERUDO_WEB").map(|v| v.to_string()).unwrap_or_default();
    let web = web.trim().trim_end_matches('/');
    Response::from_json(&json!({ "user": name, "url": format!("{web}/login?u={name}&k={key}") }))
}

/// `GET /v1/hub/me` with X-Nerudo-User and X-Nerudo-Key: the signed-in user, or 401.
async fn me(env: &Env, req: &Request) -> Result<Response> {
    let name = crate::request_header(&req, "x-nerudo-user")?.unwrap_or_default();
    let key = crate::request_header(&req, "x-nerudo-key")?.unwrap_or_default();
    match user(env, &name).await? {
        Some(u) if crate::trunk::same_key(key.as_bytes(), crate::trunk::account_key(env, &name)?.as_bytes()) => {
            Response::from_json(&json!({ "user": u.user, "title": u.title }))
        }
        _ => err(401, "sign in with your sign-in link"),
    }
}

/// One org: its repos with their summaries, and its teams, people and agents (the union
/// over its repos).
async fn org_view(env: &Env, org: &str, viewer: &Viewer) -> Result<Response> {
    let o: Option<Org> = if valid_org(org) { hub(env, Method::Get, &format!("org/{org}"), None).await? } else { None };
    let Some(o) = o else {
        return err(404, format!("unknown org {org}"));
    };
    let org = o.org.as_str();
    let repos = viewer.repos(env, hub::<Vec<Repo>>(env, Method::Get, &format!("repos/{org}"), None).await?.unwrap_or_default()).await;
    if !viewer.admin && repos.is_empty() {
        return err(404, format!("unknown org {org}"));
    }
    let summaries = join_all(repos.iter().enumerate().map(|(i, r)| async move {
        if i < MAX_SUMMARIES {
            summary(env, &r.trunk).await
        } else {
            Value::Null
        }
    }))
    .await;
    // People whose name is a user's link to a profile.
    let all: Value = hub(env, Method::Get, "orgs", None).await?.unwrap_or(Value::Null);
    let users: BTreeSet<String> = all["users"].as_array().into_iter().flatten().filter_map(|u| u["user"].as_str()).map(str::to_string).collect();
    let mut teams: BTreeMap<String, (BTreeSet<String>, BTreeSet<String>)> = BTreeMap::new();
    let mut people: BTreeMap<String, (BTreeSet<String>, BTreeSet<String>)> = BTreeMap::new();
    let mut agents: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    // People with a key in at least one repo have joined nerudo.
    let mut joined: BTreeSet<String> = BTreeSet::new();
    let mut views = vec![];
    for (r, s) in repos.iter().zip(summaries) {
        joined.extend(s["keys"].as_object().into_iter().flatten().map(|(n, _)| n.clone()));
        for (name, roles) in s["people"].as_object().into_iter().flatten() {
            let p = people.entry(name.clone()).or_default();
            p.1.insert(r.repo.clone());
            for role in roles.as_array().into_iter().flatten().filter_map(Value::as_str) {
                p.0.insert(role.to_string());
                let t = teams.entry(role.to_string()).or_default();
                t.0.insert(name.clone());
                t.1.insert(r.repo.clone());
            }
        }
        for a in s["agents"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            agents.entry(a.to_string()).or_default().insert(r.repo.clone());
        }
        let mut v = serde_json::to_value(r)?;
        v["summary"] = s;
        views.push(v);
    }
    Response::from_json(&json!({
        "kind": "org", "org": o.org, "title": o.title, "description": o.description, "created_ms": o.created_ms, "repos": views,
        "teams": teams.into_iter().map(|(team, (members, repos))| json!({ "team": team, "members": members, "repos": repos })).collect::<Vec<_>>(),
        "people": people.into_iter().map(|(name, (roles, repos))| json!({ "name": name, "roles": roles, "repos": repos, "user": users.contains(&name), "joined": joined.contains(&name) })).collect::<Vec<_>>(),
        "agents": agents.into_iter().map(|(name, repos)| json!({ "name": name, "repos": repos })).collect::<Vec<_>>(),
    }))
}

fn org_problem(org: &str) -> Option<String> {
    if RESERVED.contains(&org) {
        Some(format!("{org:?} is a reserved name"))
    } else if !valid_org(org) {
        Some(format!("{org:?}: {ORG_RULE}"))
    } else {
        None
    }
}

/// `PUT /v1/hub/orgs/{org} {title?, description?}` (admin).
async fn put_org(env: &Env, org: &str, body: &Value) -> Result<Response> {
    if let Some(p) = org_problem(org) {
        return err(400, p);
    }
    let mut patch = json!({});
    if let Some(t) = body["title"].as_str() {
        patch["title"] = json!(line(t, 100));
    }
    if let Some(d) = body["description"].as_str() {
        patch["description"] = json!(line(d, 300));
    }
    pass(env, Method::Put, &format!("org/{org}"), &patch).await
}

/// `PUT /v1/hub/repos/{org}/{repo} {trunk, description?, upstream?, public?}` (admin). A
/// field left out keeps its value; a new repo is public.
async fn put_repo(env: &Env, org: &str, repo: &str, body: &Value) -> Result<Response> {
    if let Some(p) = org_problem(org) {
        return err(400, p);
    }
    if !valid_repo(repo) {
        return err(400, format!("{repo:?}: {REPO_RULE}"));
    }
    let trunk = body["trunk"].as_str().unwrap_or_default();
    if !valid_trunk(trunk) {
        return err(400, "trunk: give the id of an initialized trunk");
    }
    // Renaming preserves metadata and creation time from the same trunk's former name.
    let old = match lookup(env, org, repo).await? {
        Some(r) => Some(r),
        None => name_of(env, trunk).await?,
    };
    let public = match &body["public"] {
        Value::Bool(b) => *b,
        Value::Null => old.as_ref().is_none_or(|o| o.public),
        _ => return err(400, "public: true or false"),
    };
    let description = match &body["description"] {
        Value::String(d) => line(d, 300),
        _ if body.get("description").is_some() => String::new(),
        _ => old.as_ref().map(|o| o.description.clone()).unwrap_or_default(),
    };
    let upstream = match &body["upstream"] {
        Value::String(u) if u.is_empty() => None,
        Value::String(u) if u.starts_with("https://") && u.len() <= 300 && !u.chars().any(|c| c.is_whitespace() || c.is_control()) => Some(u.clone()),
        Value::String(_) => return err(400, "upstream: an https:// URL"),
        _ if body.get("upstream").is_some() => None,
        _ => old.as_ref().and_then(|o| o.upstream.clone()),
    };
    // The name moves to another trunk: the trunk that loses it is private before anything
    // else happens, so a failure further on leaves nothing public by mistake.
    let mut warnings: Vec<String> = vec![];
    if let Some(o) = old.as_ref().filter(|o| o.trunk != trunk && o.public) {
        match trunk_call(env, &o.trunk, Method::Post, "visibility", Some(&json!({ "public": false }))).await {
            Ok((200, _)) => {}
            Ok((s, v)) => return err(502, format!("trunk {} (losing the name) said HTTP {s}: {v}", o.trunk)),
            Err(e) => return err(502, format!("trunk {} (losing the name) did not answer: {e}", o.trunk)),
        }
    }
    match trunk_call(env, trunk, Method::Post, "visibility", Some(&json!({ "public": public }))).await? {
        (200, _) => {}
        (404, _) => return err(404, format!("unknown trunk {trunk}: init it first")),
        (s, v) => return err(502, format!("trunk {trunk} said HTTP {s}: {v}")),
    }
    let r = Repo {
        org: org.to_string(),
        repo: repo.to_string(),
        trunk: trunk.to_string(),
        description,
        upstream,
        public,
        created_ms: old.as_ref().map(|o| o.created_ms).unwrap_or_else(now),
    };
    let out: Value = hub(env, Method::Put, &format!("repo/{org}/{repo}"), Some(&serde_json::to_value(&r)?)).await?.unwrap_or(Value::Null);
    // The memo of names forgets this name, the trunk's old name, and each trunk that lost it.
    let mut keys = vec![format!("repo:{}", r.name()), format!("trunk:{}", r.trunk)];
    keys.extend(out["renamed_from"].as_str().map(|old| format!("repo:{old}")));
    keys.extend(out["unnamed"].as_array().into_iter().flatten().filter_map(Value::as_str).map(|t| format!("trunk:{t}")));
    crate::names::forget(env, &keys).await;
    // A trunk that lost its name is private again; a trunk that did not answer is said so.
    for t in out["unnamed"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        match trunk_call(env, t, Method::Post, "visibility", Some(&json!({ "public": false }))).await {
            Ok((200, _)) => {}
            Ok((s, v)) => warnings.push(format!("trunk {t} lost the name but is still public: HTTP {s}: {v}")),
            Err(e) => warnings.push(format!("trunk {t} lost the name but may still be public: {e}")),
        }
    }
    // An import may have finished its local index before acquiring a Hub name.
    // This schedules publication/backfill only; it returns no indexed content.
    if let Err(error) = trunk_call(env, trunk, Method::Post, "search-index", Some(&json!({}))).await {
        console_warn!("search index backfill scheduling failed: {error}");
    }
    let mut v = view(env, &r).await?;
    v["renamed_from"] = out["renamed_from"].clone();
    if !warnings.is_empty() {
        v["warnings"] = json!(warnings);
    }
    Response::from_json(&v)
}

/// `DELETE /v1/hub/repos/{org}/{repo}` (admin). The trunk stays, private and without a name.
async fn delete_repo(env: &Env, org: &str, repo: &str) -> Result<Response> {
    let r = if valid_org(org) && valid_repo(repo) { hub::<Repo>(env, Method::Delete, &format!("repo/{org}/{repo}"), None).await? } else { None };
    let Some(r) = r else {
        return err(404, format!("unknown repo {org}/{repo}"));
    };
    crate::names::forget(env, &[format!("repo:{}", r.name()), format!("trunk:{}", r.trunk)]).await;
    let mut v = json!({ "deleted": r.name(), "trunk": r.trunk });
    match trunk_call(env, &r.trunk, Method::Post, "visibility", Some(&json!({ "public": false }))).await {
        Ok((200, _)) => {}
        Ok((s, w)) => v["warnings"] = json!([format!("trunk {} is still public: HTTP {s}: {w}", r.trunk)]),
        Err(e) => v["warnings"] = json!([format!("trunk {} may still be public: {e}", r.trunk)]),
    }
    Response::from_json(&v)
}

/// A request by name, sent on to its trunk as `/v1/trunks/{trunk}/{rest}`: the
/// `/v1/repos/{org}/{repo}/...` alias, and git (`git` true), which also gets
/// `X-Nerudo-Repo: {org}/{repo}`. The body streams through. A read finds the trunk in this
/// isolate's memo of names when it can (names.rs).
pub async fn forward(req: Request, env: &Env, org: &str, repo: &str, rest: &str, git: bool) -> Result<Response> {
    let read = crate::names::is_read(&req.method(), rest, &req.url()?);
    let Some(trunk) = crate::names::trunk_of(env, org, repo, read).await? else {
        if git {
            let headers = Headers::new();
            headers.set("content-type", "text/plain; charset=utf-8")?;
            return Ok(Response::ok("repository not found\n")?.with_status(404).with_headers(headers));
        }
        return err(404, format!("unknown repo {org}/{repo}"));
    };
    let mut url = req.url()?;
    url.set_path(&format!("/v1/trunks/{trunk}/{rest}"));
    let fwd = resend(&req, url.as_str())?;
    fwd.headers().set("x-nerudo-trunk", &trunk)?;
    if git {
        fwd.headers().set("x-nerudo-repo", &format!("{org}/{repo}"))?;
    }
    env.durable_object("TRUNK")?.id_from_name(&trunk)?.get_stub()?.fetch_with_request(fwd).await
}

/// `req` again at `url`: the same method and headers, and the body as a stream (never read
/// here). It leaves out X-Nerudo-Repo and X-Nerudo-Trunk, which only the router sets.
pub(crate) fn resend(req: &Request, url: &str) -> Result<Request> {
    let headers = Headers::new();
    for (k, v) in req.headers().entries() {
        if !["x-nerudo-repo", "x-landseq-repo", "x-nerudo-trunk", "x-landseq-trunk"].iter().any(|h| k.eq_ignore_ascii_case(h)) {
            headers.set(&k, &v)?;
        }
    }
    let mut init = RequestInit::new();
    init.with_method(req.method()).with_headers(headers).with_body(req.inner().body().map(Into::into));
    Request::new_with_init(url, &init)
}

/// `GET /v1/hub/repos/{org}/{repo}` and `GET /v1/hub/trunks/{trunk}` without the admin token,
/// as `api` answers them, but with the record from the memo of names (names.rs), so that a
/// page view does not ask the Hub. The trunk's summary is read now, and the answer is the
/// repo's view only when the trunk says now that the repo is public. A public repo has one
/// view for all viewers. The trunk changes before the Hub in `put_repo`, so a repo made
/// private is not shown from an old record. None: ask `api`, as before. A trunk that does
/// not answer gives 503, not a second try through the Hub.
pub(crate) async fn cached_view(req: &Request, env: &Env, rest: &[&str]) -> Result<Option<Response>> {
    if req.method() != Method::Get || crate::bearer_ok(env, req, "NERUDO_ADMIN_TOKEN") {
        return Ok(None);
    }
    let (key, record, missing) = match rest {
        ["repos", org, repo] => (format!("repo:{org}/{repo}"), crate::names::by_name(env, org, repo).await?, format!("unknown repo {org}/{repo}")),
        ["trunks", trunk] => (format!("trunk:{trunk}"), crate::names::by_trunk(env, trunk).await?, format!("trunk {trunk} has no name")),
        _ => return Ok(None),
    };
    let signed_in = crate::request_header(req, "x-nerudo-user")?.is_some() || crate::request_header(req, "x-nerudo-key")?.is_some();
    let mut resp = match (shown(record.as_ref(), signed_in), record) {
        (Shown::No, _) => err(404, missing)?,
        (Shown::IfTrunkPublic, Some(r)) => match trunk_call(env, &r.trunk, Method::Get, "summary", None).await {
            Ok((200, s)) if s["public"] == true => {
                let mut v = serde_json::to_value(&r)?;
                v["summary"] = s;
                Response::from_json(&v)?
            }
            // The trunk says it is private: the record is old.
            Ok((200, _)) => {
                crate::names::forget(env, &[key]).await;
                return Ok(None);
            }
            // The trunk answered without its summary: `api` gives the view with no summary.
            Ok(_) => return Ok(None),
            // The trunk did not answer (it can be over its rate). Asking `api` would put the
            // load of this repo on the Hub too.
            Err(_) => {
                let mut resp = err(503, format!("trunk {} did not answer; try again", r.trunk))?;
                resp.headers_mut().set("retry-after", "1")?;
                resp
            }
        },
        _ => return Ok(None),
    };
    resp.headers_mut().set("cache-control", "private, no-store")?;
    resp.headers_mut().set("vary", "X-Nerudo-User, X-Nerudo-Key, Authorization")?;
    Ok(Some(resp))
}

/// What `cached_view` does with a record from the memo, for a viewer without the admin token.
#[derive(Debug, PartialEq)]
enum Shown {
    /// The repo's view, if the trunk says now that it is public.
    IfTrunkPublic,
    /// 404: no such name, or a private repo and a viewer who is not signed in.
    No,
    /// `api` decides: a private repo can be open to a signed-in person on it.
    Ask,
}

fn shown(record: Option<&Repo>, signed_in: bool) -> Shown {
    match record {
        None => Shown::No,
        Some(r) if r.public => Shown::IfTrunkPublic,
        Some(_) if signed_in => Shown::Ask,
        Some(_) => Shown::No,
    }
}

#[cfg(test)]
mod tests {
    use super::{shown, Repo, Shown};

    #[test]
    fn a_public_record_is_shown_only_if_the_trunk_agrees_and_a_private_one_never() {
        let public = Repo { org: "o".into(), repo: "r".into(), trunk: "t".into(), public: true, ..Default::default() };
        let private = Repo { public: false, ..public.clone() };
        // A public repo: the same view for every viewer, after the trunk says it is public.
        assert_eq!(shown(Some(&public), false), Shown::IfTrunkPublic);
        assert_eq!(shown(Some(&public), true), Shown::IfTrunkPublic);
        // A private repo: 404 with no sign-in; a signed-in viewer goes to `api`.
        assert_eq!(shown(Some(&private), false), Shown::No);
        assert_eq!(shown(Some(&private), true), Shown::Ask);
        // No such name: 404 for all (the admin never comes here).
        assert_eq!(shown(None, false), Shown::No);
        assert_eq!(shown(None, true), Shown::No);
    }
}
