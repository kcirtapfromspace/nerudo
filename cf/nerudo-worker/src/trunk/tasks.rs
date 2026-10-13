//! Tasks: the work, as people (and agents) write it down, in the style of Linear issues.
//! An task says what should change and why. An agent claims it, lands changes for it,
//! and the landings move it on:
//!
//!   backlog / todo  --claim-->  in_progress  --a change is sent-->  landing
//!   landing  --the change lands, and no other change for it waits-->  done
//!   landing  --the change is ejected-->  in_progress (back to the agent)
//!
//! Also the activity log that the app shows as a live feed.
//!
//! Storage: `in:{id}` an task, `in_seq` the last number, `ev:{ms}{rand}` an event.

use super::*;
use serde_json::{json, Value};

pub(super) const STATUSES: [&str; 6] = ["backlog", "todo", "in_progress", "landing", "done", "canceled"];
/// Task ids are this prefix and a number: T-1, T-2, ...
const PREFIX: &str = "T";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Task {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub status: String,
    /// 0 none, 1 urgent, 2 high, 3 medium, 4 low (as Linear).
    #[serde(default)]
    pub priority: u8,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    pub created_by: String,
    pub created_ms: i64,
    pub updated_ms: i64,
    /// Change ids sent for this task, oldest first.
    #[serde(default)]
    pub changes: Vec<String>,
    /// How many competing attempts the task wants. 0 or 1: one agent, one PR, which merges
    /// as usual. 2 or more: agents attempt it side by side and one attempt is picked.
    #[serde(default)]
    pub attempts: u8,
    /// Who picks among the attempts: "person" (the default), or a policy (compare::POLICIES).
    #[serde(default)]
    pub pick: String,
    /// The agents that took an attempt, in order.
    #[serde(default)]
    pub attempting: Vec<String>,
    /// The attempt that was picked, and who picked it.
    #[serde(default)]
    pub picked: Option<String>,
    #[serde(default)]
    pub picked_by: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Event {
    pub at_ms: i64,
    /// landed | ejected | parked | comment | review | task
    pub kind: String,
    pub text: String,
    #[serde(default)]
    pub change: Option<String>,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub who: Option<String>,
}

fn stamp() -> String {
    let ms = now() as i64;
    let rand = (js_sys::Math::random() * 65536.0) as u32;
    format!("{ms:013}{rand:04x}")
}

impl Trunk {
    // ------------------------------------------------------------------ activity

    pub(super) async fn event(&self, kind: &str, text: String, change: Option<&str>, task: Option<&str>, who: Option<&str>) -> Result<()> {
        let e = Event { at_ms: now() as i64, kind: kind.into(), text, change: change.map(str::to_string), task: task.map(str::to_string), who: who.map(str::to_string) };
        let key = stamp();
        // A PR's own timeline (prinfo.rs) reads `ce:{change}:`. A batch's landing names only
        // its first change, so after_batch writes one "merged" line per PR instead.
        if let Some(cid) = change.filter(|_| kind != "landed") {
            self.put(&format!("ce:{cid}:{key}"), &e).await?;
        }
        self.put(&format!("ev:{key}"), &e).await
    }

    /// A line on one PR's timeline only, not in the trunk's activity feed.
    pub(super) async fn change_event(&self, kind: &str, text: String, change: &str, who: Option<&str>) -> Result<()> {
        let e = Event { at_ms: now() as i64, kind: kind.into(), text, change: Some(change.to_string()), task: None, who: who.map(str::to_string) };
        self.put(&format!("ce:{change}:{}", stamp()), &e).await
    }

    /// The newest events, newest first.
    pub(super) async fn events(&self, limit: u32) -> Result<Vec<Event>> {
        let map = self.state.storage().list_with_options(ListOptions::new().prefix("ev:").reverse(true).limit(limit as usize)).await?;
        let mut out = vec![];
        for entry in map.entries() {
            let pair: js_sys::Array = entry?.into();
            if let Some(v) = pair.get(1).as_string() {
                if let Ok(e) = serde_json::from_str::<Event>(&v) {
                    out.push(e);
                }
            }
        }
        Ok(out)
    }

    /// After a batch: one event for the landing, one per ejection, and the tasks moved on.
    pub(super) async fn after_batch(&self, batch: Option<u64>, landed: &[ChangeRec], ejected: &[ChangeRec], ci_runs: u64) -> Result<()> {
        if let Some(b) = batch.filter(|_| !landed.is_empty()) {
            let ids: Vec<&str> = landed.iter().map(|r| r.req.change_id.as_str()).collect();
            let names: Vec<String> = landed.iter().map(super::prinfo::pr_label).collect();
            let runs = if ci_runs == 1 { "1 CI run".to_string() } else { format!("{ci_runs} CI runs") };
            let text = format!("batch {b} merged {} with {runs}: {}", if ids.len() == 1 { "1 PR".into() } else { format!("{} PRs", ids.len()) }, names.join(", "));
            self.event("landed", text, ids.first().copied(), None, None).await?;
            for r in landed {
                let by = if r.sealed_by.is_empty() { String::new() } else { format!(", approved by {}", r.sealed_by.join(", ")) };
                let sha = r.trunk_commit.map(|o| o.to_hex()[..7].to_string()).unwrap_or_default();
                self.change_event("merged", format!("merged as {sha} in batch {b} with {runs}{by}"), &r.req.change_id, None).await?;
            }
        }
        for r in ejected {
            let why = match r.reason.as_ref().and_then(|v| v["kind"].as_str()) {
                Some("ci_failed") => {
                    let d = r.reason.as_ref().and_then(|v| v["diagnostics"].get(0).cloned()).unwrap_or(Value::Null);
                    format!("CI: {} {}:{}", d["code"].as_str().unwrap_or("?"), d["file"].as_str().unwrap_or("?"), d["line"])
                }
                Some(k) => k.replace('_', " "),
                None => "ejected".into(),
            };
            self.event("ejected", format!("{} rev {} closed ({why})", super::prinfo::pr_label(r), r.req.rev), Some(&r.req.change_id), r.req.task.as_deref(), None).await?;
        }
        for r in landed {
            if let Some(id) = &r.req.task {
                self.task_after_landing(id).await?;
            }
        }
        for r in ejected {
            let not_picked = r.reason.as_ref().and_then(|v| v["kind"].as_str()) == Some("not_picked");
            if let Some(id) = r.req.task.as_ref().filter(|_| !not_picked) {
                self.set_status_if(id, &["landing"], "in_progress", "nerudo").await?;
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------ tasks

    pub(super) async fn tasks(&self) -> Result<Vec<Task>> {
        Ok(self.list_prefix("tk:").await?.into_iter().filter_map(|(_, v)| serde_json::from_str(&v).ok()).collect())
    }

    pub(super) async fn task(&self, id: &str) -> Result<Option<Task>> {
        self.get(&format!("tk:{id}")).await
    }

    pub(super) async fn save_task(&self, i: &Task) -> Result<()> {
        self.put(&format!("tk:{}", i.id), i).await
    }

    /// A person, a team (@team) or an agent, as the trunk knows them; None: not known.
    async fn known(&self, who: &str) -> Result<Option<String>> {
        let dir = self.directory().await?;
        Ok(match dir.resolve(who) {
            Some(super::social::Who::Team(t)) => Some(format!("@{t}")),
            Some(super::social::Who::Person(p)) | Some(super::social::Who::Agent(p)) => Some(p),
            None => None,
        })
    }

    pub(super) async fn create_task(&self, by: &str, v: &Value) -> Result<std::result::Result<Task, String>> {
        let title = super::social::clean(v["title"].as_str().unwrap_or_default());
        let title: String = title.lines().next().unwrap_or_default().chars().take(200).collect();
        if title.is_empty() {
            return Ok(Err("an task needs a title".into()));
        }
        let assignee = match v["assignee"].as_str().filter(|a| !a.is_empty()) {
            Some(a) => match self.known(a).await? {
                Some(k) => Some(k),
                None => return Ok(Err(format!("nobody is called {a} on this trunk"))),
            },
            None => None,
        };
        let status = v["status"].as_str().unwrap_or(if assignee.is_some() { "todo" } else { "backlog" });
        if !STATUSES.contains(&status) {
            return Ok(Err(format!("status must be one of {}", STATUSES.join(", "))));
        }
        let (attempts, pick) = match super::compare::attempts_of(v) {
            Ok(x) => x,
            Err(e) => return Ok(Err(e)),
        };
        let n: u64 = self.get("tk_seq").await?.unwrap_or(0) + 1;
        self.put("tk_seq", &n).await?;
        let at = now() as i64;
        let i = Task {
            id: format!("{PREFIX}-{n}"),
            title,
            body: super::social::clean(v["body"].as_str().unwrap_or_default()),
            status: status.into(),
            priority: v["priority"].as_u64().unwrap_or(0).min(4) as u8,
            assignee: assignee.clone(),
            labels: labels(v),
            created_by: by.into(),
            created_ms: at,
            updated_ms: at,
            changes: vec![],
            attempts: attempts.unwrap_or(1),
            pick: pick.unwrap_or_default(),
            attempting: vec![],
            picked: None,
            picked_by: None,
        };
        self.save_task(&i).await?;
        self.event("task", format!("{by} wrote {} {}", i.id, i.title), None, Some(&i.id), Some(by)).await?;
        if let Some(a) = &assignee {
            self.notify_assignee(&i, a, by).await?;
        }
        Ok(Ok(i))
    }

    /// Change the fields given in `v`: title, body, status, priority, assignee, labels.
    pub(super) async fn update_task(&self, id: &str, by: &str, v: &Value) -> Result<std::result::Result<Task, String>> {
        let Some(mut i) = self.task(id).await? else {
            return Ok(Err(format!("unknown task {id}")));
        };
        if let Some(t) = v["title"].as_str() {
            let t: String = super::social::clean(t).lines().next().unwrap_or_default().chars().take(200).collect();
            if t.is_empty() {
                return Ok(Err("an task needs a title".into()));
            }
            i.title = t;
        }
        if let Some(b) = v["body"].as_str() {
            i.body = super::social::clean(b);
        }
        if let Some(p) = v["priority"].as_u64() {
            i.priority = p.min(4) as u8;
        }
        if v.get("attempts").is_some() || v.get("pick").is_some() {
            if i.picked.is_some() {
                return Ok(Err(format!("{id} already has a picked attempt")));
            }
            match super::compare::attempts_of(v) {
                Ok((a, p)) => {
                    if let Some(a) = a {
                        i.attempts = a;
                    }
                    if let Some(p) = p {
                        i.pick = p;
                    }
                }
                Err(e) => return Ok(Err(e)),
            }
        }
        if v.get("labels").is_some() {
            i.labels = labels(v);
        }
        if let Some(a) = v.get("assignee") {
            match a.as_str().filter(|a| !a.is_empty()) {
                None => i.assignee = None,
                Some(a) => match self.known(a).await? {
                    Some(k) => {
                        if i.assignee.as_ref() != Some(&k) {
                            self.notify_assignee(&i, &k, by).await?;
                        }
                        i.assignee = Some(k);
                    }
                    None => return Ok(Err(format!("nobody is called {a} on this trunk"))),
                },
            }
        }
        if let Some(s) = v["status"].as_str() {
            if !STATUSES.contains(&s) {
                return Ok(Err(format!("status must be one of {}", STATUSES.join(", "))));
            }
            if s != i.status {
                self.event("task", format!("{by} moved {} to {}", i.id, s.replace('_', " ")), None, Some(&i.id), Some(by)).await?;
            }
            i.status = s.into();
        }
        i.updated_ms = now() as i64;
        self.save_task(&i).await?;
        Ok(Ok(i))
    }

    async fn notify_assignee(&self, i: &Task, assignee: &str, by: &str) -> Result<()> {
        let dir = self.directory().await?;
        let members = match assignee.strip_prefix('@') {
            Some(t) => dir.team(t),
            None => vec![assignee.to_string()],
        };
        for m in members.into_iter().filter(|m| m != by) {
            let n = super::social::Note { kind: "assigned".into(), change_id: i.id.clone(), from: by.into(), team: assignee.strip_prefix('@').map(str::to_string), text: i.title.clone(), at_ms: now() as i64 };
            self.put(&format!("nt:{m}:{}", stamp()), &n).await?;
        }
        Ok(())
    }

    /// An agent takes an task: it is assigned to the agent and in progress.
    pub(super) async fn claim(&self, id: &str, agent: &str) -> Result<std::result::Result<Task, String>> {
        let Some(i) = self.task(id).await? else {
            return Ok(Err(format!("unknown task {id}")));
        };
        if matches!(i.status.as_str(), "done" | "canceled") {
            return Ok(Err(format!("{id} is {}", i.status)));
        }
        if i.attempts >= 2 {
            return self.take_attempt(i, agent).await;
        }
        if let Some(other) = i.assignee.as_ref().filter(|a| *a != agent && !a.starts_with('@')) {
            if self.directory().await?.agents.contains(other) && i.status == "in_progress" {
                return Ok(Err(format!("{id} is in progress with {other}")));
            }
        }
        let r = self.update_task(id, agent, &json!({ "assignee": agent, "status": "in_progress" })).await?;
        if r.is_ok() {
            self.event("task", format!("{agent} claimed {id}"), None, Some(id), Some(agent)).await?;
        }
        Ok(r)
    }

    /// A change was sent for task `id`: link it, and the task is landing.
    pub(super) async fn task_sent(&self, id: &str, cid: &str) -> Result<()> {
        if let Some(mut i) = self.task(id).await? {
            if !i.changes.iter().any(|c| c == cid) {
                i.changes.push(cid.into());
            }
            if !matches!(i.status.as_str(), "done" | "canceled") {
                i.status = "landing".into();
            }
            i.updated_ms = now() as i64;
            self.save_task(&i).await?;
        }
        Ok(())
    }

    /// A change for task `id` landed. The task is done when no other change for it waits.
    async fn task_after_landing(&self, id: &str) -> Result<()> {
        let Some(i) = self.task(id).await? else { return Ok(()) };
        for cid in &i.changes {
            if let Some(rev) = self.get::<u32>(&format!("latest:{cid}")).await? {
                if let Some(r) = self.get::<ChangeRec>(&format!("c:{cid}:{rev}")).await? {
                    if matches!(r.state, Phase::Queued | Phase::AwaitingSeal | Phase::AwaitingAllowance | Phase::AwaitingPick) {
                        return Ok(());
                    }
                }
            }
        }
        if self.set_status_if(id, &["landing", "in_progress", "todo", "backlog"], "done", "nerudo").await? {
            self.event("task", format!("{id} is done: its changes landed"), None, Some(id), None).await?;
        }
        Ok(())
    }

    /// Move an task to `to` when it is in one of `from`. Returns whether it moved.
    async fn set_status_if(&self, id: &str, from: &[&str], to: &str, _by: &str) -> Result<bool> {
        let Some(mut i) = self.task(id).await? else { return Ok(false) };
        if !from.contains(&i.status.as_str()) {
            return Ok(false);
        }
        i.status = to.into();
        i.updated_ms = now() as i64;
        self.save_task(&i).await?;
        Ok(true)
    }

    // ------------------------------------------------------------------ API for agents

    /// `GET .../tasks?for=<agent>`: the tasks for that agent first, then the open ones.
    pub(super) async fn api_tasks(&self, req: &Request) -> Result<Response> {
        let who = req.url()?.query_pairs().find(|(k, _)| k == "for").map(|(_, v)| v.into_owned());
        let mut all = self.tasks().await?;
        all.retain(|i| !matches!(i.status.as_str(), "done" | "canceled"));
        all.sort_by_key(|i| (i.assignee.as_deref() != who.as_deref(), if i.priority == 0 { 5 } else { i.priority }, i.created_ms));
        Response::from_json(&all)
    }

    /// `POST .../tasks {title, body, priority, assignee, labels, as}`: an agent writes one.
    pub(super) async fn api_create_task(&self, caller: &super::agents::Caller, v: Value) -> Result<Response> {
        let who = match self.acting_agent(caller, "as", v["as"].as_str().unwrap_or_default()).await? {
            Ok(w) => w,
            Err((status, e)) => return bad(status, e),
        };
        match self.create_task(&who, &v).await? {
            Ok(i) => Response::from_json(&i),
            Err(e) => bad(400, e),
        }
    }

    /// `POST .../tasks/{id}/claim {as}`.
    pub(super) async fn api_claim(&self, caller: &super::agents::Caller, id: &str, v: Value) -> Result<Response> {
        let who = match self.acting_agent(caller, "as", v["as"].as_str().unwrap_or_default()).await? {
            Ok(w) => w,
            Err((status, e)) => return bad(status, e),
        };
        match self.claim(id, &who).await? {
            Ok(i) => Response::from_json(&i),
            Err(e) => bad(409, e),
        }
    }
}

fn labels(v: &Value) -> Vec<String> {
    let raw: Vec<String> = match &v["labels"] {
        Value::Array(a) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        Value::String(s) => s.split(',').map(str::to_string).collect(),
        _ => vec![],
    };
    let mut out: Vec<String> = vec![];
    for l in raw {
        let l: String = l.trim().chars().filter(|c| c.is_alphanumeric() || "-_ ".contains(*c)).take(32).collect();
        if !l.is_empty() && !out.contains(&l) {
            out.push(l);
        }
    }
    out.truncate(8);
    out
}
