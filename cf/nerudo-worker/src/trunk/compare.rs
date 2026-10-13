//! Competing attempts: compare several PRs for one task, and ship one.
//!
//! A task can ask for N attempts (`attempts: 3`). Up to N agents claim it, and each lands
//! a PR with `task: T-n`. Such a PR does not merge on its own. When it passes the gate,
//! the sequencer runs a trial: it applies the PR to main as main is now, runs the trunk
//! checks and the type check on the result, and keeps the result (a trial). Nothing is
//! pushed. The PR then waits for a pick:
//!
//!   queued  --gate + trial-->  awaiting_pick  --picked-->  queued  -->  landed
//!                                             --another attempt picked-->  ejected (not_picked)
//!
//! A person picks in the app (POST task/{id}/pick), or the task's pick policy picks once
//! every attempt is in. The picked PR goes back on the queue and merges as usual, with
//! its own CI run on main as main is then. The other attempts close as not picked, and
//! their branches become stale.
//!
//! Storage: `tr:{task}:{change}` the latest trial of one attempt; `parked:pick` the keys
//! of the changes waiting for a pick.

use super::app::diff_json;
use super::tasks::Task;
use super::*;
use serde_json::{json, Value};

/// Who picks among a task's attempts.
///   person    a person with a personal link picks in the app (the default)
///   smallest  once every attempt is in: the passing attempt that changes the fewest lines
///   first     once every attempt is in: the passing attempt that was sent first
/// A decision model is one more policy: see `choose`.
pub(super) const POLICIES: [&str; 3] = ["person", "smallest", "first"];

/// The most attempts a task can ask for.
const MAX_ATTEMPTS: u64 = 5;

/// The trial of one attempt: what main would be with this PR alone, and what CI said.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Trial {
    pub change_id: String,
    pub rev: u32,
    pub at_ms: i64,
    /// Main when the trial ran, and the commit main would get.
    pub main: Oid,
    pub preview: Oid,
    /// true: the type check passed; false: it failed; None: no type check is configured, or
    /// main itself fails it, so the result says nothing about this attempt.
    pub passed: Option<bool>,
    /// The CI failure (the same shape as a ci_failed ejection), or why there was no verdict.
    #[serde(default)]
    pub ci: Value,
    /// Files changed against main, and lines added and deleted.
    pub files: u32,
    pub adds: u32,
    pub dels: u32,
}

/// `attempts` and `pick` from a task body. None: not given.
pub(super) fn attempts_of(v: &Value) -> std::result::Result<(Option<u8>, Option<String>), String> {
    let attempts = match &v["attempts"] {
        Value::Null => None,
        a => match a.as_u64().or_else(|| a.as_str().and_then(|s| s.parse().ok())) {
            Some(n) if (1..=MAX_ATTEMPTS).contains(&n) => Some(n as u8),
            _ => return Err(format!("attempts must be 1 to {MAX_ATTEMPTS}")),
        },
    };
    let pick = match v["pick"].as_str().filter(|p| !p.is_empty()) {
        None => None,
        Some(p) if POLICIES.contains(&p) => Some(p.to_string()),
        Some(p) => return Err(format!("pick must be one of {} (not {p:?})", POLICIES.join(", "))),
    };
    Ok((attempts, pick))
}

/// The pick policy of a task, "person" when none is set.
pub(super) fn policy(t: &Task) -> &str {
    if t.pick.is_empty() {
        "person"
    } else {
        &t.pick
    }
}

impl Trunk {
    /// An agent takes one of a task's attempts.
    pub(super) async fn take_attempt(&self, mut t: Task, agent: &str) -> Result<std::result::Result<Task, String>> {
        if t.picked.is_some() {
            return Ok(Err(format!("{} already has a picked attempt", t.id)));
        }
        if !t.attempting.iter().any(|a| a == agent) {
            if t.attempting.len() >= t.attempts as usize {
                return Ok(Err(format!("{} has all {} of its attempts taken: {}", t.id, t.attempts, t.attempting.join(", "))));
            }
            t.attempting.push(agent.to_string());
            self.event("task", format!("{agent} took attempt {} of {} on {}", t.attempting.len(), t.attempts, t.id), None, Some(&t.id), Some(agent)).await?;
        }
        if matches!(t.status.as_str(), "backlog" | "todo") {
            t.status = "in_progress".into();
        }
        t.updated_ms = now() as i64;
        self.save_task(&t).await?;
        Ok(Ok(t))
    }

    /// In the gate: a PR for a task that wants attempts doesn't merge on its own. While the
    /// task has no pick, it gets a trial and waits; once another attempt is picked, it
    /// closes. The picked attempt, and every other PR, stays in `ready`.
    pub(super) async fn compare_divert(
        &self,
        rules: &Rules,
        store: &mut Store,
        head: Oid,
        ready: Vec<Ready>,
        budget: &mut BTreeMap<String, Allowance>,
        ejected: &mut Vec<ChangeRec>,
    ) -> Result<Vec<Ready>> {
        let mut keep = vec![];
        let mut waiting: Vec<(ChangeRec, Trial)> = vec![];
        for r in ready {
            let task = match &r.rec.req.task {
                Some(id) => self.task(id).await?.filter(|t| t.attempts >= 2),
                None => None,
            };
            let Some(task) = task else {
                keep.push(r);
                continue;
            };
            let release = |budget: &mut BTreeMap<String, Allowance>, a: &Option<String>| {
                if let Some(al) = a.as_ref().and_then(|a| budget.get_mut(a)) {
                    al.reserved -= 1;
                }
            };
            match &task.picked {
                Some(p) if *p == r.rec.req.change_id => keep.push(r),
                Some(p) => {
                    release(budget, &r.allowance);
                    ejected.push(eject(r.rec, json!({ "kind": "not_picked", "task": task.id, "picked": p })));
                }
                None => {
                    release(budget, &r.allowance);
                    match self.trial(rules, store, head, &r).await? {
                        Ok(trial) => {
                            let mut rec = r.rec;
                            rec.state = Phase::AwaitingPick;
                            rec.ci_runs = if trial.ci.is_null() { 0 } else { 1 };
                            rec.reason = Some(json!({ "kind": "awaiting_pick", "task": task.id, "attempts": task.attempts, "passed": trial.passed }));
                            waiting.push((rec, trial));
                        }
                        Err(reason) => ejected.push(eject(r.rec, reason)),
                    }
                }
            }
        }
        if !waiting.is_empty() {
            self.park_pick(waiting).await?;
        }
        Ok(keep)
    }

    /// Apply one attempt to main as main is now, check it, and type-check it. Err: the
    /// attempt can't be applied or breaks a trunk check; it is ejected as any PR would be.
    async fn trial(&self, rules: &Rules, store: &mut Store, head: Oid, r: &Ready) -> Result<std::result::Result<Trial, Value>> {
        let committer = Signature::new("nerudo", "nerudo@nerudo.invalid", (now() / 1000.0) as i64);
        let input = ChangeInput { trailers: r.trailers.clone(), ..input_of(&r.rec.req) };
        let applied = match apply_change(store, rules, head, &input, &committer) {
            Ok(a) => a,
            Err(e) => return Ok(Err(self.apply_ejection(rules, store, head, &r.rec, e).await?)),
        };
        let checked = check_files(store, rules, &applied.tree).map_err(|e| e.to_string()).and_then(|files| trunk_checks(store, rules, &files));
        if let Err(detail) = checked {
            return Ok(Err(json!({ "kind": "check_failed", "detail": detail })));
        }
        if rules.ci.typecheck.is_some() {
            // The landing store holds few file contents; the type check reads them all.
            let cfg = self.cfg().await?;
            let t = FetchTransport::default();
            let mut rd = self.readers(&cfg).await?;
            self.typecheck_files(&mut rd, &t, store, &applied.commit).await?;
            self.typecheck_files(&mut rd, &t, store, &head).await?;
        }
        let (passed, ci) = match &rules.ci.typecheck {
            None => (None, Value::Null),
            Some(tc) => match ci::typecheck(&self.env, store, &applied.commit, tc).await? {
                Verdict::Passed => (Some(true), json!({ "kind": "ci_passed" })),
                Verdict::Failed(reason) => {
                    if ci::typecheck(&self.env, store, &head, tc).await?.passed() {
                        (Some(false), reason)
                    } else {
                        (None, json!({ "kind": "main_fails", "detail": "main itself fails the type check, so this trial says nothing" }))
                    }
                }
            },
        };
        let diff = store.diff(Some(&store.commit(&head).map_err(g)?.tree), Some(&applied.tree)).map_err(g)?;
        let before: BTreeMap<String, FileEntry> = diff.iter().filter_map(|(p, a, _)| a.map(|e| (p.clone(), e))).collect();
        let after: BTreeMap<String, FileEntry> = diff.iter().filter_map(|(p, _, b)| b.map(|e| (p.clone(), e))).collect();
        let (files, adds, dels) = line_counts(store, &before, &after);
        Ok(Ok(Trial {
            change_id: r.rec.req.change_id.clone(),
            rev: r.rec.req.rev,
            at_ms: now() as i64,
            main: head,
            preview: applied.commit,
            passed,
            ci,
            files,
            adds,
            dels,
        }))
    }

    async fn park_pick(&self, waiting: Vec<(ChangeRec, Trial)>) -> Result<()> {
        let mut parked: Vec<String> = self.get(&parked_key(&Phase::AwaitingPick)).await?.unwrap_or_default();
        let mut items = vec![];
        let mut tasks = BTreeSet::new();
        for (rec, trial) in &waiting {
            let k = rec.req.key();
            if !parked.contains(&k) {
                parked.push(k.clone());
            }
            let task = rec.req.task.clone().unwrap_or_default();
            items.push((format!("c:{k}"), serde_json::to_string(rec)?));
            items.push((format!("tr:{task}:{}", rec.req.change_id), serde_json::to_string(trial)?));
            tasks.insert(task);
        }
        let set: BTreeSet<&String> = parked.iter().collect();
        let queue: Vec<String> = self.get::<Vec<String>>("q").await?.unwrap_or_default().into_iter().filter(|k| !set.contains(k)).collect();
        items.push((parked_key(&Phase::AwaitingPick), serde_json::to_string(&parked)?));
        items.push(("q".into(), serde_json::to_string(&queue)?));
        self.put_all(items).await?;
        for (rec, trial) in &waiting {
            let verdict = match trial.passed {
                Some(true) => "its checks passed",
                Some(false) => "its checks failed",
                None => "no check verdict",
            };
            let task = rec.req.task.as_deref().unwrap_or_default();
            self.event("parked", format!("{} waits for a pick on {task}: {verdict}, {} files +{} −{}", rec.req.change_id, trial.files, trial.adds, trial.dels), Some(&rec.req.change_id), Some(task), None)
                .await?;
        }
        self.notify();
        for t in tasks {
            self.maybe_auto_pick(&t).await?;
        }
        Ok(())
    }

    /// The attempts of a task: each change for it, its latest record, and its trial.
    pub(super) async fn attempts(&self, t: &Task) -> Result<Vec<(ChangeRec, Option<Trial>)>> {
        let mut out = vec![];
        for cid in &t.changes {
            if let Some(r) = self.latest_rec(cid).await? {
                let trial = self.get::<Trial>(&format!("tr:{}:{cid}", t.id)).await?;
                out.push((r, trial));
            }
        }
        Ok(out)
    }

    /// Ship one attempt: it goes back on the queue and merges; the others close.
    pub(super) async fn pick(&self, id: &str, cid: &str, by: &str) -> Result<std::result::Result<Value, String>> {
        let Some(mut t) = self.task(id).await? else {
            return Ok(Err(format!("unknown task {id}")));
        };
        if t.attempts < 2 {
            return Ok(Err(format!("{id} doesn't ask for competing attempts")));
        }
        if let Some(p) = &t.picked {
            return Ok(Err(format!("{id} already shipped {p}")));
        }
        let attempts = self.attempts(&t).await?;
        let Some((winner, trial)) = attempts.iter().find(|(r, _)| r.req.change_id == cid) else {
            return Ok(Err(format!("{cid} is not an attempt for {id}")));
        };
        if winner.state != Phase::AwaitingPick {
            return Ok(Err(format!("{cid} is not waiting for a pick (it is {:?})", winner.state)));
        }
        if trial.as_ref().and_then(|x| x.passed) == Some(false) {
            return Ok(Err(format!("{cid} failed its checks; pick an attempt that passed, or ask for another attempt")));
        }
        t.picked = Some(cid.to_string());
        t.picked_by = Some(by.to_string());
        t.updated_ms = now() as i64;
        self.save_task(&t).await?;

        let winner_key = winner.req.key();
        // The losers: every other attempt that is still open. A later revision of one, still
        // queued, closes when the gate sees it.
        let mut parked: Vec<String> = self.get(&parked_key(&Phase::AwaitingPick)).await?.unwrap_or_default();
        let mut stats: Stats = self.get("stats").await?.unwrap_or_default();
        let mut closed = vec![];
        let mut items = vec![];
        for (r, _) in &attempts {
            if r.req.change_id == cid || !matches!(r.state, Phase::AwaitingPick) {
                continue;
            }
            let k = r.req.key();
            parked.retain(|x| *x != k);
            let rec = eject(r.clone(), json!({ "kind": "not_picked", "task": id, "picked": cid }));
            items.push((format!("c:{k}"), serde_json::to_string(&rec)?));
            stats.not_picked += 1;
            closed.push(r.req.change_id.clone());
        }
        items.push((parked_key(&Phase::AwaitingPick), serde_json::to_string(&parked)?));
        items.push(("stats".into(), serde_json::to_string(&stats)?));
        self.put_all(items).await?;
        let woken = self.requeue(Phase::AwaitingPick, |k| k == winner_key).await?;
        let text = if closed.is_empty() { format!("{by} picked {cid} for {id}") } else { format!("{by} picked {cid} for {id}; closed {}", closed.join(", ")) };
        self.event("task", text, Some(cid), Some(id), Some(by)).await?;
        for c in &closed {
            self.event("ejected", format!("{c} closed: {cid} was picked for {id}"), Some(c), Some(id), None).await?;
        }
        self.ensure_alarm().await?;
        self.notify();
        Ok(Ok(json!({ "task": id, "picked": cid, "requeued": woken, "closed": closed })))
    }

    /// When the task's policy picks by itself and every attempt is in, pick.
    async fn maybe_auto_pick(&self, id: &str) -> Result<()> {
        let Some(t) = self.task(id).await? else { return Ok(()) };
        let policy = policy(&t).to_string();
        if t.picked.is_some() || policy == "person" {
            return Ok(());
        }
        let attempts = self.attempts(&t).await?;
        let waiting: Vec<&(ChangeRec, Option<Trial>)> = attempts.iter().filter(|(r, _)| r.state == Phase::AwaitingPick).collect();
        if waiting.len() < t.attempts as usize {
            return Ok(());
        }
        match choose(&policy, &waiting) {
            Some(cid) => {
                let by = format!("nerudo ({policy})");
                if let Err(e) = self.pick(id, &cid, &by).await? {
                    console_warn!("{id}: the {policy} policy could not pick {cid}: {e}");
                }
            }
            None => {
                self.event("task", format!("{id}: no attempt passed its checks, so the {policy} policy picked none; a person decides"), None, Some(id), None).await?;
            }
        }
        Ok(())
    }

    /// `GET task/{id}/compare` for the app: each attempt side by side.
    pub(super) async fn app_compare(&self, cfg: &Config, id: &str) -> Result<Response> {
        let Some(t) = self.task(id).await? else {
            return Ok(Response::from_json(&json!({ "error": "unknown task" }))?.with_status(404));
        };
        let meta: Meta = self.get("meta").await?.unwrap_or_default();
        let mut store = self.ui_store(cfg, meta.head).await?;
        let mut out = vec![];
        for (r, trial) in self.attempts(&t).await? {
            if !store.contains(&r.req.commit) {
                if let Some(fork) = self.get::<Fork>(&format!("f:{}", r.req.fork)).await? {
                    let tr = FetchTransport::default();
                    let remote = Remote::new(&fork.remote, Some(fork.authorization.clone()));
                    let haves: Vec<Oid> = meta.head.into_iter().chain([r.req.base]).collect();
                    let _ = fetch_with(&tr, &remote, &[r.req.commit], &haves, FetchOpts { any_haves: true, ..FetchOpts::default() }, &mut store).await;
                }
            }
            let (why, files) = match (store.commit(&r.req.commit), store.commit(&r.req.base)) {
                (Ok(c), Ok(base)) => {
                    let (before, after) = super::views::changed_files(&store, Some(&base.tree), &c.tree).map_err(g)?;
                    (c.body().to_string(), diff_json(&store, &before, &after))
                }
                _ => (String::new(), Value::Null),
            };
            let subject = if r.subject.is_empty() { store.commit(&r.req.commit).map(|c| c.message.lines().next().unwrap_or("").to_string()).unwrap_or_default() } else { r.subject.clone() };
            // The plan side by side too: the app reads GET context/{change}?rev=&v= with these.
            let version = self.sent_version(&r.req).await?.map(|(v, _)| v);
            out.push(json!({
                "change_id": r.req.change_id, "rev": r.req.rev, "state": r.state, "agent": super::ui::agent_of(&cfg.trunk, &r.req.fork),
                "subject": subject, "footprint": r.footprint, "submitted_ms": r.submitted_ms, "reason": r.reason, "seq": r.seq,
                "trial": trial, "why": why, "files": files, "context_commit": r.req.context_commit, "context_version": version,
            }));
        }
        self.ui_store_back(store);
        Response::from_json(&json!({ "task": t, "policy": policy(&t), "policies": POLICIES, "attempts": out }))
    }
}

/// The policy's choice among the attempts waiting for a pick, or None: leave it to a person.
///
/// A decision model plugs in here as one more policy. It gets the task and the trials
/// (CI verdict, files and lines changed, the agent's why) and returns a change id with a
/// confidence. Below the confidence the application sets, it returns None, and a person
/// picks in the app as with the default policy.
fn choose(policy: &str, waiting: &[&(ChangeRec, Option<Trial>)]) -> Option<String> {
    let mut ok: Vec<(&ChangeRec, &Trial)> = waiting.iter().filter_map(|(r, t)| t.as_ref().filter(|t| t.passed != Some(false)).map(|t| (r, t))).collect();
    match policy {
        "smallest" => ok.sort_by_key(|(r, t)| (t.adds + t.dels, t.files, r.submitted_ms)),
        "first" => ok.sort_by_key(|(r, _)| r.submitted_ms),
        _ => return None,
    }
    ok.first().map(|(r, _)| r.req.change_id.clone())
}

/// Files that differ between two trees, and the lines added and deleted in text files.
fn line_counts(store: &Store, before: &BTreeMap<String, FileEntry>, after: &BTreeMap<String, FileEntry>) -> (u32, u32, u32) {
    let paths: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    let (mut files, mut adds, mut dels) = (0, 0, 0);
    for p in paths {
        let (a, b) = (before.get(p), after.get(p));
        if a.map(|e| e.oid) == b.map(|e| e.oid) {
            continue;
        }
        files += 1;
        let text = |e: Option<&FileEntry>| -> String {
            e.and_then(|e| store.blob(&e.oid).ok()).filter(|b| b.len() <= super::ui::MAX_FILE_BYTES).map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default()
        };
        let (x, y) = (text(a), text(b));
        let xa: Vec<&str> = x.lines().collect();
        let ya: Vec<&str> = y.lines().collect();
        for (k, _) in super::ui::edit_script(&xa, &ya) {
            match k {
                '+' => adds += 1,
                '-' => dels += 1,
                _ => {}
            }
        }
    }
    (files, adds, dels)
}
