//! The JSON API of the nerudo app (cf/nerudo-app). One snapshot for the shell, details on
//! demand, and actions. It takes the same keys as the pages: a share key reads, and a
//! personal key reads and acts. A public repo (repo.rs) reads with no key. Keys come in
//! the X-Nerudo-Key and X-Nerudo-User headers, or as ?k= and ?u=.
//!
//!   GET  state                     the shell: numbers, in flight, tasks, trunk, events
//!   GET  access                    the same read gate, without loading the snapshot
//!   GET  change/{id}[?rev=]        a change's life: why, diff, CI, seals, landing, thread
//!   GET  task/{id}               an task with its changes and thread
//!   GET  agent/{name}              an agent's fork, refs and changes
//!   GET  agent/{name}/context      an agent's context history (see context.rs)
//!   GET  context/{id}[?rev=&v=]    a change's context: its versions in the context repo
//!   GET  tree[?path=&at=]          files at the trunk tip or at a landing
//!   GET  search[?q=&mode=&...]    recursive paths or bounded text matches
//!   GET  digest[?at=]            language bytes, file types and manifest evidence
//!   GET  history[?path=&before=]   landings, newest first
//!   GET  task/{id}/compare         a task's competing attempts, side by side
//!   GET  releases                  releases, newest first, and open proposals (releases.rs)
//!   GET  release/draft[?at=&version=]  the next release: version, notes, what it signs
//!   GET  release/{tag}             one release
//!   GET  compare/{a}...{b}         files changed between two releases (or main)
//!   POST comment | review | react | cleanup | fork | tasks | task/{id} | task/{id}/pick

use super::tasks::Task;
use super::social::{normalize_emoji, Directory};
use super::ui::{account_key, agent_of, edit_script, same, share_key, valid_key, MAX_FILE_BYTES};
use super::*;
use serde_json::{json, Value};

type Flat = BTreeMap<String, FileEntry>;

/// Landings in the snapshot.
const LANDINGS: u64 = 80;

impl Trunk {
    pub(super) async fn app(&self, req: &mut Request, rest: &[&str]) -> Result<Response> {
        let url = req.url()?;
        let q: BTreeMap<String, String> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        let Some(cfg) = self.get::<Config>("cfg").await? else {
            return json_err(404, "unknown trunk");
        };
        let key = crate::request_header(&req, "x-nerudo-key")?.or_else(|| q.get("k").cloned()).filter(|k| valid_key(k)).unwrap_or_default();
        let mut user = crate::request_header(&req, "x-nerudo-user")?.or_else(|| q.get("u").cloned()).filter(|u| !u.is_empty());
        let dir = self.directory().await?;
        let person = match &user {
            Some(u) if dir.people.contains_key(u) => self.person_ok(&cfg.trunk, u, &key).await?,
            _ => false,
        };
        let ok = match &user {
            _ if person => true,
            // A signed-in user who is not a person here reads a public repo as anyone does.
            Some(u) if same(key.as_bytes(), account_key(&self.env, u)?.as_bytes()) && self.is_public().await? => {
                user = None;
                true
            }
            Some(_) => false,
            None => same(key.as_bytes(), share_key(&self.env, &cfg.trunk)?.as_bytes()) || self.is_public().await?,
        };
        if !ok {
            return json_err(403, "open this trunk with its share link or your personal link");
        }
        if req.method() == Method::Get && rest == ["access"] {
            return Response::from_json(&json!({ "person": person }));
        }
        let num = |k: &str| q.get(k).and_then(|v| v.parse::<u64>().ok());
        let resp = match (req.method(), rest) {
            (Method::Get, ["state"]) => self.app_state(&cfg, user.as_deref(), &dir).await,
            (Method::Get, ["change", cid]) => self.app_change(&cfg, cid, num("rev").map(|r| r as u32), &dir).await,
            (Method::Get, ["task", id]) => self.app_task(id, &dir).await,
            (Method::Get, ["task", id, "compare"]) => self.app_compare(&cfg, id).await,
            (Method::Get, ["agent", name]) => self.app_agent(&cfg, name).await,
            (Method::Get, ["agent", name, "context"]) => self.agent_context_json(&cfg, name, q.get("commit").map(String::as_str), q.get("path").map(String::as_str)).await,
            (Method::Get, ["context", cid]) => self.context_json(&cfg, cid, num("rev").map(|r| r as u32), num("v").map(|v| v as u32)).await,
            (Method::Get, ["branches"]) => self.app_branches(&cfg).await,
            (Method::Get, ["releases"]) => self.api_release_list().await,
            (Method::Get, ["release", "draft"]) => self.api_release_draft(req).await,
            (Method::Get, ["release", tag]) => self.api_release(tag).await,
            (Method::Get, ["compare", range]) => self.release_compare(&cfg, range).await,
            (Method::Get, ["tree"]) => self.app_tree(&cfg, q.get("path").map(String::as_str).unwrap_or(""), num("at")).await,
            (Method::Get, ["search"]) => self.app_discovery(&cfg, &q, false).await,
            (Method::Get, ["digest"]) => self.app_discovery(&cfg, &q, true).await,
            (Method::Get, ["history"]) => self.app_history(&cfg, q.get("path").map(String::as_str), num("before")).await,
            (Method::Post, action) => {
                let Some(me) = user else {
                    return json_err(403, "only a personal link can act");
                };
                let body: Value = req.json().await.unwrap_or(Value::Null);
                self.app_act(&me, action, body).await
            }
            _ => json_err(404, "no such API"),
        };
        self.renamed(resp?, &dir).await
    }

    /// Records keep the name a person had when they were written. Show a former name as the
    /// person's name now (social.rs `aliases`), so an old `you` does not read as the viewer.
    async fn renamed(&self, mut resp: Response, dir: &Directory) -> Result<Response> {
        let json = resp.headers().get("content-type")?.is_some_and(|c| c.starts_with("application/json"));
        if resp.status_code() != 200 || !json {
            return Ok(resp);
        }
        let aliases = self.aliases(dir).await?;
        if aliases.is_empty() {
            return Ok(resp);
        }
        let mut v: Value = resp.json().await?;
        rename_people(&mut v, &aliases);
        Ok(Response::from_json(&v)?.with_headers(resp.headers().clone()))
    }

    fn change_json(trunk: &str, r: &ChangeRec, comments: usize) -> Value {
        json!({
            "change_id": r.req.change_id, "rev": r.req.rev, "state": r.state, "agent": agent_of(trunk, &r.req.fork),
            "subject": r.subject, "footprint": r.footprint, "submitted_ms": r.submitted_ms, "landed_ms": r.landed_ms,
            "seq": r.seq, "batch": r.batch, "ci_runs": r.ci_runs, "task": r.req.task, "seal_reasons": r.seal_reasons,
            "reason": r.reason, "sealed_by": r.sealed_by, "allocated": r.allocated, "comments": comments, "decisions": r.decisions,
            "context_commit": r.req.context_commit, "number": r.number, "size": r.size, "base": r.req.base, "branch": r.req.branch,
        })
    }

    async fn app_state(&self, cfg: &Config, me: Option<&str>, dir: &Directory) -> Result<Response> {
        let meta: Meta = self.get("meta").await?.unwrap_or_default();
        let stats: Stats = self.get("stats").await?.unwrap_or_default();
        let talk = self.comment_counts().await?;
        let n = |cid: &str| talk.get(cid).copied().unwrap_or(0);
        // Open PRs, with their place in the merge queue (1 is next).
        let queue: Vec<String> = self.get("q").await?.unwrap_or_default();
        let pending: Vec<Value> = self
            .pending()
            .await?
            .iter()
            .map(|r| {
                let mut v = Self::change_json(&cfg.trunk, r, n(&r.req.change_id));
                if let Some(i) = queue.iter().position(|k| *k == r.req.key()) {
                    v["position"] = json!(i + 1);
                    v["queue_len"] = json!(queue.len());
                }
                v
            })
            .collect();

        let mut landings = vec![];
        let from = meta.seq.saturating_sub(LANDINGS) + 1;
        // State polling reads durable records. Older subjects may use an already cached
        // commit, but must never fetch the whole repository before the page can open.
        for seq in (from..=meta.seq).rev() {
            if let Some(e) = self.get::<LogEntry>(&format!("log:{seq:012}")).await? {
                let mut r = self.rec(&e.change_id, e.rev).await?;
                if let Some(rec) = r.as_mut().filter(|x| x.subject.is_empty()) {
                    rec.subject = Oid::from_hex(&e.commit_out).ok().and_then(|o| self.mem.borrow().ui.commit(&o).ok()).map(|c| c.message.lines().next().unwrap_or("").to_string()).unwrap_or_default();
                }
                landings.push(json!({
                    "seq": e.seq, "batch": e.batch, "change_id": e.change_id, "rev": e.rev, "commit_out": e.commit_out,
                    "landed_ms": e.landed_at_ms, "agent": r.as_ref().map(|r| agent_of(&cfg.trunk, &r.req.fork).to_string()),
                    "subject": r.as_ref().map(|r| r.subject.clone()).unwrap_or_default(),
                    "sealed_by": r.as_ref().map(|r| r.sealed_by.clone()).unwrap_or_default(),
                    "allocated": r.as_ref().map(|r| r.allocated.clone()).unwrap_or_default(),
                    "ci_runs": r.as_ref().map(|r| r.ci_runs).unwrap_or(1), "task": r.as_ref().and_then(|r| r.req.task.clone()),
                    "comments": n(&e.change_id), "number": r.as_ref().and_then(|r| r.number), "size": r.as_ref().and_then(|r| r.size),
                    "base": r.as_ref().map(|r| r.req.base),
                }));
            }
        }

        let mut ejected: Vec<ChangeRec> = self
            .list_prefix("c:")
            .await?
            .into_iter()
            .filter_map(|(_, v)| serde_json::from_str::<ChangeRec>(&v).ok())
            .filter(|r| r.state == Phase::Ejected)
            .collect();
        ejected.sort_by_key(|r| std::cmp::Reverse(r.submitted_ms));
        let ejected: Vec<Value> = ejected.iter().take(30).map(|r| Self::change_json(&cfg.trunk, r, n(&r.req.change_id))).collect();

        let mut tasks = self.tasks().await?;
        tasks.sort_by_key(|i| std::cmp::Reverse(i.updated_ms));
        let tasks: Vec<Value> = tasks.iter().map(|i| task_json(i, n(&i.id))).collect();

        let needs_me = match me {
            Some(user) => {
                let roles = dir.people.get(user).cloned().unwrap_or_default();
                let mut reviews = vec![];
                for r in self.all_reviews().await? {
                    let mine = r.reviewer == user || r.reviewer.strip_prefix('@').is_some_and(|t| roles.iter().any(|x| x == t));
                    if !mine {
                        continue;
                    }
                    if let Some(c) = self.latest_rec(&r.change_id).await? {
                        if matches!(c.state, Phase::Queued | Phase::AwaitingSeal | Phase::AwaitingAllowance) {
                            reviews.push(json!({ "request": r, "change": Self::change_json(&cfg.trunk, &c, n(&c.req.change_id)) }));
                        }
                    }
                }
                json!({ "reviews": reviews, "notes": self.notes(user, 30).await? })
            }
            None => Value::Null,
        };

        let people: Vec<Value> = dir.people.iter().map(|(name, roles)| json!({ "name": name, "roles": roles })).collect();
        Response::from_json(&json!({
            "trunk": cfg.trunk, "branch": cfg.branch, "head": meta.head, "seq": meta.seq,
            "me": me.map(|m| json!({ "name": m, "roles": dir.people.get(m).cloned().unwrap_or_default() })),
            "people": people, "teams": dir.teams(), "agents": dir.agents,
            "stats": stats, "pending": pending, "landings": landings, "ejected": ejected, "tasks": tasks,
            "needs_me": needs_me, "events": self.events(60).await?, "releases": self.release_marks().await?,
        }))
    }

    async fn thread_json(&self, thread: &str) -> Result<Value> {
        let mut comments = vec![];
        for c in self.comments(thread).await? {
            let reactions = self.reactions(&format!("m.{}", c.id)).await?;
            comments.push(json!({ "comment": c, "reactions": reactions }));
        }
        Ok(json!({ "comments": comments, "reactions": self.reactions(&format!("c.{thread}")).await? }))
    }

    async fn app_change(&self, cfg: &Config, cid: &str, rev: Option<u32>, dir: &Directory) -> Result<Response> {
        let cid = &self.resolve_pr(cid).await?;
        let latest = self.get::<u32>(&format!("latest:{cid}")).await?;
        let Some(rev) = rev.or(latest) else {
            return json_err(404, "unknown change");
        };
        let Some(r) = self.rec(cid, rev).await? else {
            return json_err(404, "unknown change");
        };
        let mut revisions = vec![];
        for n in 1..=latest.unwrap_or(rev) {
            if let Some(x) = self.rec(cid, n).await? {
                revisions.push(json!({ "rev": n, "state": x.state, "kind": x.reason.as_ref().and_then(|v| v["kind"].as_str().map(str::to_string)), "context": x.req.context_commit.is_some() }));
            }
        }
        let meta: Meta = self.get("meta").await?.unwrap_or_default();
        let mut store = self.ui_store(cfg, meta.head).await?;
        let mut landing = Value::Null;
        let (why, files) = if let (Phase::Landed, Some(seq)) = (&r.state, r.seq) {
            match self.get::<LogEntry>(&format!("log:{seq:012}")).await? {
                Some(e) => {
                    let out = Oid::from_hex(&e.commit_out).map_err(g)?;
                    match store.commit(&out) {
                        Ok(c) => {
                            let parent = c.parents.first().map(|p| store.commit(p).map(|p| p.tree)).transpose().map_err(g)?;
                            let (before, after) = super::views::changed_files(&store, parent.as_ref(), &c.tree).map_err(g)?;
                            landing = json!({
                                "seq": seq, "batch": e.batch, "commit_out": e.commit_out, "commit_in": e.commit_in, "landed_ms": e.landed_at_ms,
                                "author": format!("{} <{}>", c.author.name, c.author.email),
                                "seal_reasons": c.trailers("Seal-Reason"), "sealed_by": c.trailers("Sealed-By"),
                                "change_commit": c.trailer("Change-Commit"), "break_glass": c.trailer("Break-Glass"),
                            });
                            (c.body().to_string(), diff_json(&store, &before, &after))
                        }
                        Err(_) => (String::new(), Value::Null),
                    }
                }
                None => (String::new(), Value::Null),
            }
        } else {
            if !store.contains(&r.req.commit) {
                if let Some(fork) = self.get::<Fork>(&format!("f:{}", r.req.fork)).await? {
                    let t = FetchTransport::default();
                    let remote = Remote::new(&fork.remote, Some(fork.authorization.clone()));
                    let haves: Vec<Oid> = meta.head.into_iter().chain([r.req.base]).collect();
                    let _ = fetch_with(&t, &remote, &[r.req.commit], &haves, FetchOpts { any_haves: true, ..FetchOpts::default() }, &mut store).await;
                }
            }
            match (store.commit(&r.req.commit), store.commit(&r.req.base)) {
                (Ok(c), Ok(base)) => {
                    let (before, after) = super::views::changed_files(&store, Some(&base.tree), &c.tree).map_err(g)?;
                    (c.body().to_string(), diff_json(&store, &before, &after))
                }
                _ => (String::new(), Value::Null),
            }
        };
        let info = self.pr_info(cfg, &r, &landing, &store).await?;
        self.ui_store_back(store);

        let mut r = r;
        if r.subject.is_empty() {
            let store = self.ui_store(cfg, meta.head).await?;
            let commit = data_commit(&r, &landing);
            r.subject = commit.and_then(|o| store.commit(&o).ok()).map(|c| c.message.lines().next().unwrap_or("").to_string()).unwrap_or_default();
            self.ui_store_back(store);
        }
        let quorum = r.reason.as_ref().map(|v| v["quorum"].clone()).unwrap_or(Value::Null);
        let have: Vec<String> = quorum.as_array().into_iter().flatten().flat_map(|t| t["have"].as_array().cloned().unwrap_or_default()).filter_map(|v| v.as_str().map(str::to_string)).chain(r.sealed_by.clone()).collect();
        let open = matches!(r.state, Phase::Queued | Phase::AwaitingSeal | Phase::AwaitingAllowance | Phase::AwaitingPick);
        let reviews: Vec<Value> = self
            .reviews(cid)
            .await?
            .into_iter()
            .map(|x| {
                let status = if have.contains(&x.reviewer) { "sealed" } else if open { "requested" } else { "closed" };
                json!({ "request": x, "status": status })
            })
            .collect();
        let task = match &r.req.task {
            Some(id) => self.task(id).await?.map(|i| json!({ "id": i.id, "title": i.title, "status": i.status })),
            None => None,
        };
        let _ = dir;
        let mut out = json!({
            "change": Self::change_json(&cfg.trunk, &r, 0), "revisions": revisions, "why": why, "files": files, "landing": landing,
            "quorum": quorum, "reviews": reviews, "task": task, "thread": self.thread_json(cid).await?,
            "seal_command": format!("nerudo seal --trunk {} --change {} --rev {}", cfg.trunk, cid, r.req.rev),
        });
        // A git host's PR page: timeline, commits, checks, size, and the branches (prinfo.rs).
        if let (Some(o), Value::Object(extra)) = (out.as_object_mut(), info) {
            o.extend(extra);
        }
        Response::from_json(&out)
    }

    async fn app_task(&self, id: &str, _dir: &Directory) -> Result<Response> {
        let Some(i) = self.task(id).await? else {
            return json_err(404, "unknown task");
        };
        let cfg = self.cfg().await?;
        let mut changes = vec![];
        for cid in &i.changes {
            if let Some(r) = self.latest_rec(cid).await? {
                changes.push(Self::change_json(&cfg.trunk, &r, 0));
            }
        }
        Response::from_json(&json!({ "task": task_json(&i, 0), "changes": changes, "thread": self.thread_json(id).await? }))
    }

    async fn app_agent(&self, cfg: &Config, name: &str) -> Result<Response> {
        let Some(fork) = self.get::<Fork>(&format!("fa:{name}")).await? else {
            return json_err(404, "unknown agent");
        };
        let (fork, refs) = self.fork_refs(cfg, name, fork).await?;
        let refs = match refs {
            Ok(r) => {
                let mut rows = vec![];
                for (n, oid) in r {
                    let row = self.ref_row(&cfg.branch, &fork.name, n, oid).await?;
                    rows.push(json!({ "name": row.name, "oid": row.oid, "change": row.change.as_ref().map(|c| json!({ "change_id": c.0, "rev": c.1, "state": c.2 })), "stale": row.stale }));
                }
                json!({ "ok": rows })
            }
            Err(e) => json!({ "error": e }),
        };
        let mut changes: Vec<ChangeRec> = self
            .list_prefix("c:")
            .await?
            .into_iter()
            .filter_map(|(_, v)| serde_json::from_str::<ChangeRec>(&v).ok())
            .filter(|r| agent_of(&cfg.trunk, &r.req.fork) == name)
            .collect();
        changes.sort_by_key(|r| std::cmp::Reverse(r.submitted_ms));
        let changes: Vec<Value> = changes.iter().take(40).map(|r| Self::change_json(&cfg.trunk, r, 0)).collect();
        Response::from_json(&json!({ "agent": name, "fork": fork.name, "remote": fork.remote, "refs": refs, "changes": changes, "notes": self.notes(name, 20).await? }))
    }

    /// The repo's branches: main, then every agent's fork with the branches in it.
    async fn app_branches(&self, cfg: &Config) -> Result<Response> {
        let meta: Meta = self.get("meta").await?.unwrap_or_default();
        let mut forks = vec![];
        for b in self.branches(cfg).await? {
            let refs = match b.refs {
                Ok(rows) => json!({ "ok": rows.iter().map(|row| json!({ "name": row.name, "oid": row.oid, "change": row.change.as_ref().map(|c| json!({ "change_id": c.0, "rev": c.1, "state": c.2 })), "stale": row.stale })).collect::<Vec<_>>() }),
                Err(e) => json!({ "error": e }),
            };
            forks.push(json!({ "agent": b.agent, "fork": b.fork.name, "refs": refs }));
        }
        Response::from_json(&json!({ "branch": cfg.branch, "head": meta.head, "seq": meta.seq, "forks": forks }))
    }

    async fn app_tree(&self, cfg: &Config, path: &str, at: Option<u64>) -> Result<Response> {
        let path = path.trim_matches('/');
        let meta: Meta = self.get("meta").await?.unwrap_or_default();
        let Some(head) = meta.head else {
            return json_err(404, "trunk has no commit");
        };
        let commit = match at {
            Some(seq) => match self.get::<LogEntry>(&format!("log:{seq:012}")).await? {
                Some(e) => Oid::from_hex(&e.commit_out).map_err(g)?,
                None => return json_err(404, "unknown landing"),
            },
            None => head,
        };
        let store = self.ui_store(cfg, Some(head)).await?;
        let tree = store.commit(&commit).map_err(g)?.tree;
        let out = match super::views::path_view(&store, &tree, path).map_err(g)? {
            Some(super::views::PathView::File(e)) => {
                let bytes = store.blob(&e.oid).map_err(g)?;
                let text = (bytes.len() <= MAX_FILE_BYTES).then(|| std::str::from_utf8(bytes).ok()).flatten();
                json!({ "kind": "file", "path": path, "size": bytes.len(), "text": text })
            }
            None => {
                self.ui_store_back(store);
                return json_err(404, "no such path");
            }
            Some(super::views::PathView::Dir(dirs, files)) => {
            let mut entries: BTreeMap<String, bool> = BTreeMap::new();
            for d in dirs {
                entries.insert(d, true);
            }
            for f in files {
                entries.insert(f, false);
            }
            let mut list: Vec<Value> = entries.into_iter().map(|(name, dir)| json!({ "name": name, "dir": dir })).collect();
            list.sort_by_key(|v| (!v["dir"].as_bool().unwrap_or(false), v["name"].as_str().unwrap_or("").to_string()));
            json!({ "kind": "dir", "path": path, "entries": list })
            }
        };
        self.ui_store_back(store);
        let mut out = out;
        out["commit"] = json!(commit);
        out["at"] = json!(at);
        Response::from_json(&out)
    }

    /// Search and composition use the same read gate as tree. The store is returned
    /// before propagating an object error, so one failed read cannot empty the cache.
    async fn app_discovery(&self, cfg: &Config, q: &BTreeMap<String, String>, composition: bool) -> Result<Response> {
        let options = if composition { None } else {
            match super::search::SearchOptions::parse(q) {
                Ok(options) => Some(options),
                Err(error) => return json_err(400, &error),
            }
        };
        let at = match q.get("at") {
            Some(value) => match value.parse::<u64>() {
                Ok(seq) if seq > 0 => Some(seq),
                _ => return json_err(400, "at must be a positive landing number"),
            },
            None => None,
        };
        let meta: Meta = self.get("meta").await?.unwrap_or_default();
        let Some(head) = meta.head else { return json_err(404, "trunk has no commit"); };
        let commit = match at {
            Some(seq) => match self.get::<LogEntry>(&format!("log:{seq:012}")).await? {
                Some(entry) => Oid::from_hex(&entry.commit_out).map_err(g)?,
                None => return json_err(404, "unknown landing"),
            },
            None => head,
        };
        if let Some(options) = &options {
            let mut out = self.indexed_search(options, commit, at.unwrap_or(meta.seq), head, at.is_some()).await?;
            out["at"] = json!(at);
            let mut response = Response::from_json(&out)?;
            response.headers_mut().set("cache-control", "private, no-store")?;
            return Ok(response);
        }
        let cached = if composition { self.mem.borrow().digest.as_ref().filter(|(oid, _)| *oid == commit).map(|(_, value)| value.clone()) } else { None };
        let mut out = if let Some(value) = cached { value } else {
            let store = self.ui_store(cfg, Some(head)).await?;
            let result = (|| -> Result<Value> {
                let flat = store.flatten(&store.commit(&commit).map_err(g)?.tree).map_err(g)?;
                Ok(match &options {
                    Some(options) => super::search::search(&store, &flat, options),
                    None => super::search::digest(&store, &flat),
                })
            })();
            self.ui_store_back(store);
            let value = result?;
            // A lazy read records missing blobs for the outer request retry. Cache
            // only a complete digest, or a retry would return this partial value.
            if composition && value["skipped_files"].as_u64() == Some(0) { self.mem.borrow_mut().digest = Some((commit, value.clone())); }
            value
        };
        out["commit"] = json!(commit);
        out["at"] = json!(at);
        let mut response = Response::from_json(&out)?;
        response.headers_mut().set("cache-control", "no-store")?;
        Ok(response)
    }

    async fn app_history(&self, cfg: &Config, path: Option<&str>, before: Option<u64>) -> Result<Response> {
        let meta: Meta = self.get("meta").await?.unwrap_or_default();
        let store = self.ui_store(cfg, meta.head).await?;
        let top = before.unwrap_or(meta.seq + 1).saturating_sub(1).min(meta.seq);
        let (limit, floor) = if path.is_some() { (200, top.saturating_sub(400)) } else { (60, 0) };
        let mut out = vec![];
        let mut seq = top;
        while seq > floor && out.len() < limit {
            if let Some(e) = self.get::<LogEntry>(&format!("log:{seq:012}")).await? {
                let commit = Oid::from_hex(&e.commit_out).ok().and_then(|o| store.commit(&o).ok());
                let touched = match (path, &commit) {
                    (None, _) => true,
                    (Some(p), Some(c)) => {
                        let (now, was) = super::views::file_at(&store, c, p).unwrap_or_default();
                        now != was
                    }
                    _ => false,
                };
                if touched {
                    out.push(json!({
                        "seq": e.seq, "batch": e.batch, "change_id": e.change_id, "rev": e.rev, "landed_ms": e.landed_at_ms,
                        "subject": commit.as_ref().map(|c| c.message.lines().next().unwrap_or("").to_string()).unwrap_or_default(),
                        "author": commit.as_ref().map(|c| c.author.name.clone()).unwrap_or_default(),
                    }));
                }
            }
            seq -= 1;
        }
        self.ui_store_back(store);
        Response::from_json(&json!({ "path": path, "landings": out, "more": path.is_none() && seq > 0, "next_before": seq + 1 }))
    }

    async fn app_act(&self, me: &str, action: &[&str], v: Value) -> Result<Response> {
        let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
        let done = |r: std::result::Result<Value, String>| match r {
            Ok(x) => Response::from_json(&x),
            Err(e) => json_err(400, &e),
        };
        match action {
            ["comment"] => done(self.add_comment(&s("thread"), me, false, &s("body")).await?.map(|c| json!(c))),
            ["review"] => done(self.add_review_request(&s("change"), &s("reviewer"), me).await?.map(|r| json!({ "requested": r }))),
            ["react"] => {
                let thread = s("thread");
                let Some(emoji) = normalize_emoji(&s("emoji")) else {
                    return json_err(400, "give one emoji, or a :shortcode: like :rocket:");
                };
                if self.get::<u32>(&format!("latest:{thread}")).await?.is_none() && self.task(&thread).await?.is_none() {
                    return json_err(400, "unknown change or task");
                }
                let target = match self.reaction_target(&thread, v["comment"].as_str()).await? {
                    Ok(t) => t,
                    Err(e) => return json_err(400, &e),
                };
                let on = self.toggle_reaction(&target, &emoji, me).await?;
                Response::from_json(&json!({ "emoji": emoji, "on": on }))
            }
            ["cleanup"] => {
                let cfg = self.cfg().await?;
                done(self.cleanup(&cfg, &s("agent")).await?.map(|n| json!({ "deleted": n })))
            }
            ["fork"] => {
                let agent: String = me.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
                let mut resp = self.fork(ForkReq { agent: agent.clone() }).await?;
                let f: Value = resp.json().await.unwrap_or(Value::Null);
                match f["name"].as_str() {
                    Some(name) => Response::from_json(&json!({ "agent": agent, "fork": name, "remote": f["remote"] })),
                    None => json_err(400, &f.to_string()),
                }
            }
            ["tasks"] => done(self.create_task(me, &v).await?.map(|i| json!(i))),
            ["task", id] => done(self.update_task(id, me, &v).await?.map(|i| json!(i))),
            ["task", id, "pick"] => done(self.pick(id, &s("change"), me).await?),
            _ => json_err(404, "no such action"),
        }
    }
}

/// The commit that carries a change's subject: on trunk once it landed, else the agent's.
fn data_commit(r: &ChangeRec, landing: &Value) -> Option<Oid> {
    landing["commit_out"].as_str().and_then(|h| Oid::from_hex(h).ok()).or(Some(r.req.commit))
}

fn task_json(i: &Task, comments: usize) -> Value {
    let mut v = json!(i);
    v["comments"] = json!(comments);
    v
}

/// Changed files between two trees, with line diffs and three lines of context. Each
/// line is [op, old line, new line, text]; a skipped stretch is ["…"].
pub(super) fn diff_json(store: &Store, before: &Flat, after: &Flat) -> Value {
    let paths: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    let mut files = vec![];
    for p in paths {
        let (a, b) = (before.get(p), after.get(p));
        if a.map(|e| e.oid) == b.map(|e| e.oid) {
            continue;
        }
        let status = match (a, b) {
            (None, _) => "added",
            (_, None) => "deleted",
            _ => "changed",
        };
        let text = |e: Option<&FileEntry>| -> Option<String> {
            match e {
                None => Some(String::new()),
                Some(e) => store.blob(&e.oid).ok().filter(|b| b.len() <= MAX_FILE_BYTES).and_then(|b| String::from_utf8(b.to_vec()).ok()),
            }
        };
        let (x, y) = (text(a), text(b));
        let (Some(x), Some(y)) = (x, y) else {
            files.push(json!({ "path": p, "status": status, "binary": true, "lines": [] }));
            continue;
        };
        let xa: Vec<&str> = x.lines().collect();
        let ya: Vec<&str> = y.lines().collect();
        let ops = edit_script(&xa, &ya);
        let near: Vec<bool> = (0..ops.len()).map(|i| ops[i.saturating_sub(3)..(i + 4).min(ops.len())].iter().any(|(k, _)| *k != ' ')).collect();
        let (mut i, mut j) = (0u32, 0u32);
        let (mut adds, mut dels) = (0, 0);
        let mut lines: Vec<Value> = vec![];
        let mut gap = false;
        for (idx, (k, line)) in ops.iter().enumerate() {
            let (o, n) = match k {
                ' ' => {
                    i += 1;
                    j += 1;
                    (Some(i), Some(j))
                }
                '-' => {
                    i += 1;
                    dels += 1;
                    (Some(i), None)
                }
                _ => {
                    j += 1;
                    adds += 1;
                    (None, Some(j))
                }
            };
            if !near[idx] {
                gap = true;
                continue;
            }
            if gap {
                lines.push(json!(["…"]));
                gap = false;
            }
            lines.push(json!([k.to_string(), o, n, line]));
        }
        if gap {
            lines.push(json!(["…"]));
        }
        files.push(json!({ "path": p, "status": status, "binary": false, "adds": adds, "dels": dels, "lines": lines }));
    }
    json!(files)
}

/// The fields of the app API that hold the name of a person or an agent.
const PERSON_FIELDS: &[&str] = &["sealed_by", "have", "created_by", "picked_by", "assignee", "who", "author", "reviewer", "by", "from"];

/// Replace former names (old → new) in the person fields, in a `Sealed-By` trailer (its first
/// word is the name), and in the sentences the sequencer writes about a person: an event or
/// timeline line starts with its `who`, and a check says "signed by …" or "approved by …".
/// Text that people and agents wrote (comments, notes, context) stays as written.
fn rename_people(v: &mut Value, aliases: &BTreeMap<String, String>) {
    match v {
        Value::Object(m) => {
            let who = m.get("who").and_then(Value::as_str).map(str::to_string);
            for (k, x) in m.iter_mut() {
                if PERSON_FIELDS.contains(&k.as_str()) {
                    rename_names(x, aliases);
                } else {
                    rename_people(x, aliases);
                }
            }
            for field in ["text", "detail"] {
                if field == "text" && who.is_none() {
                    continue;
                }
                if let Some(Value::String(s)) = m.get_mut(field) {
                    *s = rename_sentence(s, who.as_deref(), aliases);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| rename_people(x, aliases)),
        _ => {}
    }
}

fn rename_names(v: &mut Value, aliases: &BTreeMap<String, String>) {
    match v {
        Value::String(s) => {
            let (name, rest) = s.split_once(' ').unwrap_or((s.as_str(), ""));
            if let Some(new) = aliases.get(name) {
                *s = if rest.is_empty() { new.clone() } else { format!("{new} {rest}") };
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| rename_names(x, aliases)),
        _ => rename_people(v, aliases),
    }
}

fn rename_sentence(s: &str, who: Option<&str>, aliases: &BTreeMap<String, String>) -> String {
    let mut out = s.to_string();
    if let Some((old, new)) = who.and_then(|w| aliases.get_key_value(w)) {
        if let Some(rest) = out.strip_prefix(&format!("{old} ")) {
            out = format!("{new} {rest}");
        }
    }
    for (old, new) in aliases {
        for verb in ["signed by", "approved by"] {
            for end in [",", ".", ")", ";"] {
                out = out.replace(&format!("{verb} {old}{end}"), &format!("{verb} {new}{end}"));
            }
            if let Some(head) = out.strip_suffix(&format!("{verb} {old}")) {
                out = format!("{head}{verb} {new}");
            }
        }
    }
    out
}

fn json_err(status: u16, msg: &str) -> Result<Response> {
    Ok(Response::from_json(&json!({ "error": msg }))?.with_status(status))
}
