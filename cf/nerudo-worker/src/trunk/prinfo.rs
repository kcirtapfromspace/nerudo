//! What a pull request shows, the way people expect from a git host: a number (#12), its
//! size (+12 −3 in 4 files), a timeline of what happened to it, its checks, and its
//! commits. The sequencer already keeps the facts; this module only counts and arranges
//! them.
//!
//! Storage: `pr_seq` the last PR number, `num:{change}` a change's number, `pr:{n}` the
//! change with number n, `ce:{change}:{ms}{rand}` a line on one PR's timeline.

use super::tasks::Event;
use super::ui::{agent_of, edit_script, MAX_FILE_BYTES};
use super::*;
use crate::types::Size;
use nerudo_git::MODE_GITLINK;
use serde_json::{json, Value};

/// "#12 a01c3", or the change id alone for a change from before numbers.
pub(super) fn pr_label(r: &ChangeRec) -> String {
    match r.number {
        Some(n) => format!("#{n} {}", r.req.change_id),
        None => r.req.change_id.clone(),
    }
}

/// Files changed, and lines added and deleted, between the change's base and its commit.
/// A binary or very large file counts as changed, with no lines. None: the trees, or a file
/// that `size_files` names, are not in the store yet.
pub(super) fn change_size(store: &Store, base: &Oid, commit: &Oid) -> Option<Size> {
    let changed = store.diff(Some(&store.commit(base).ok()?.tree), Some(&store.commit(commit).ok()?.tree)).ok()?;
    if changed.iter().flat_map(|(_, a, b)| [a, b]).flatten().any(|e| e.mode != MODE_GITLINK && !store.contains(&e.oid)) {
        return None;
    }
    let mut size = Size::default();
    let text = |e: Option<&FileEntry>| -> Option<String> {
        match e {
            None => Some(String::new()),
            Some(e) => store.blob(&e.oid).ok().filter(|b| b.len() <= MAX_FILE_BYTES).and_then(|b| String::from_utf8(b.to_vec()).ok()),
        }
    };
    for (_, a, b) in &changed {
        let (a, b) = (a.as_ref(), b.as_ref());
        if a.map(|e| e.oid) == b.map(|e| e.oid) {
            continue;
        }
        size.files += 1;
        if let (Some(x), Some(y)) = (text(a), text(b)) {
            let xa: Vec<&str> = x.lines().collect();
            let ya: Vec<&str> = y.lines().collect();
            for (k, _) in edit_script(&xa, &ya) {
                match k {
                    '+' => size.adds += 1,
                    '-' => size.dels += 1,
                    _ => {}
                }
            }
        }
    }
    Some(size)
}

/// The files that `change_size` reads and the store lacks: each edited file at the base and in
/// the change. The gate does not wait for them (a landing does not read most base files); the
/// landing loop fetches them while it pushes.
pub(super) fn size_files(store: &Store, base: &Oid, commit: &Oid) -> Vec<Oid> {
    let (Ok(b), Ok(c)) = (store.commit(base), store.commit(commit)) else { return vec![] };
    let Ok(changed) = store.diff(Some(&b.tree), Some(&c.tree)) else { return vec![] };
    let ids: BTreeSet<Oid> = changed.iter().flat_map(|(_, a, b)| [a, b]).flatten().filter(|e| e.mode != MODE_GITLINK && !store.contains(&e.oid)).map(|e| e.oid).collect();
    ids.into_iter().collect()
}

/// Count the size of each change in `recs` that has none: fetch from trunk the files that
/// `change_size` reads and the store lacks, in one request. A failed fetch leaves the sizes out.
pub(super) async fn fill_sizes(t: &FetchTransport, trunk: &Remote, store: &mut Store, recs: &mut [&mut ChangeRec]) {
    let want: BTreeSet<Oid> = recs.iter().filter(|r| r.size.is_none()).flat_map(|r| size_files(store, &r.req.base, &r.req.commit)).collect();
    if !want.is_empty() {
        if let Err(e) = fetch_with(t, trunk, &want.into_iter().collect::<Vec<_>>(), &[], FetchOpts::default(), store).await {
            console_warn!("PR sizes: the files could not be read: {e}");
        }
    }
    for r in recs.iter_mut().filter(|r| r.size.is_none()) {
        r.size = change_size(store, &r.req.base, &r.req.commit);
    }
}

impl Trunk {
    /// Write the sizes that the push's read made countable, for changes that did not land in
    /// `attempt` (they wait for a later batch).
    pub(super) async fn store_sizes(&self, store: &Store, no_size: &[(String, Oid, Oid)], attempt: &Attempt) -> Result<()> {
        for (key, base, commit) in no_size.iter().filter(|(k, ..)| !attempt.landed.iter().any(|l| &l.key == k)) {
            let Some(size) = change_size(store, base, commit) else { continue };
            if let Some(mut rec) = self.get::<ChangeRec>(&format!("c:{key}")).await? {
                if rec.size.is_none() && rec.state == Phase::Queued {
                    rec.size = Some(size);
                    self.put(&format!("c:{key}"), &rec).await?;
                }
            }
        }
        Ok(())
    }

    /// The number for a change id: the one it has, or the next one, with the writes that
    /// record it (to go in the same atomic write as the change).
    pub(super) async fn pr_number(&self, cid: &str) -> Result<(u64, Vec<(String, String)>)> {
        if let Some(n) = self.get::<u64>(&format!("num:{cid}")).await? {
            return Ok((n, vec![]));
        }
        let n = self.get::<u64>("pr_seq").await?.unwrap_or(0) + 1;
        Ok((n, vec![(format!("num:{cid}"), n.to_string()), (format!("pr:{n}"), serde_json::to_string(cid)?), ("pr_seq".into(), n.to_string())]))
    }

    /// The first line of a PR's timeline, or a new revision of it.
    pub(super) async fn opened(&self, req: &LandReq, number: u64) -> Result<()> {
        let cfg = self.cfg().await?;
        let agent = agent_of(&cfg.trunk, &req.fork).to_string();
        let text = if req.rev == 1 { format!("{agent} opened #{number}") } else { format!("{agent} pushed revision {} of #{number}", req.rev) };
        self.change_event("opened", text, &req.change_id, Some(&agent)).await
    }

    /// A change id from what a person typed: the id itself, "#12", or "12".
    pub(super) async fn resolve_pr(&self, id: &str) -> Result<String> {
        if self.get::<u32>(&format!("latest:{id}")).await?.is_some() {
            return Ok(id.to_string());
        }
        let digits = id.trim_start_matches("%23").trim_start_matches('#');
        if let Ok(n) = digits.parse::<u64>() {
            if let Some(cid) = self.get::<String>(&format!("pr:{n}")).await? {
                return Ok(cid);
            }
        }
        Ok(id.to_string())
    }

    /// The timeline, checks and commits of a PR, for the app's PR page. `landing` and
    /// `quorum` are what app_change already read; comments come with the thread.
    pub(super) async fn pr_info(&self, cfg: &Config, r: &ChangeRec, landing: &Value, store: &Store) -> Result<Value> {
        let cid = &r.req.change_id;
        let latest = self.get::<u32>(&format!("latest:{cid}")).await?.unwrap_or(r.req.rev);
        let mut revs: Vec<ChangeRec> = vec![];
        for n in 1..=latest {
            if let Some(x) = self.rec(cid, n).await? {
                revs.push(x);
            }
        }
        let agent = agent_of(&cfg.trunk, &r.req.fork).to_string();

        // Timeline: the PR's own lines, then what older records can tell. Comments are drawn
        // by the app from the thread, so they are left out here.
        let mut lines: Vec<Value> = vec![];
        let mut have: BTreeSet<String> = BTreeSet::new();
        for (_, v) in self.list_prefix(&format!("ce:{cid}:")).await? {
            if let Ok(e) = serde_json::from_str::<Event>(&v) {
                if e.kind == "comment" || e.kind == "review" {
                    continue;
                }
                have.insert(e.kind.clone());
                lines.push(json!({ "at_ms": e.at_ms, "kind": e.kind, "text": e.text, "who": e.who }));
            }
        }
        if !have.contains("opened") {
            for x in &revs {
                let text = if x.req.rev == 1 { format!("{agent} opened this pull request") } else { format!("{agent} pushed revision {}", x.req.rev) };
                lines.push(json!({ "at_ms": x.submitted_ms, "kind": "opened", "text": text, "who": agent }));
            }
        }
        if !have.contains("merged") {
            if let (Phase::Landed, Some(at)) = (&r.state, r.landed_ms) {
                let sha = r.trunk_commit.map(|o| o.to_hex()[..7].to_string()).unwrap_or_default();
                lines.push(json!({ "at_ms": at, "kind": "merged", "text": format!("merged as {sha} in batch {}", r.batch.unwrap_or(0)), "who": null }));
            }
        }
        for rq in self.reviews(cid).await? {
            lines.push(json!({ "at_ms": rq.at_ms, "kind": "review", "text": format!("{} asked {} for a review", rq.by, rq.reviewer), "who": rq.by }));
        }
        for x in &revs {
            for d in &x.decisions {
                let p = d.probabilities.get(&d.choice).copied().unwrap_or(0.0);
                lines.push(json!({
                    "at_ms": d.at_ms, "kind": "decision",
                    "text": format!("the decision model ({}) answered {} {p:.2}: {}", d.provider, d.choice, outcome_words(&d.outcome)),
                    "who": null,
                }));
            }
        }
        lines.sort_by_key(|v| v["at_ms"].as_i64().unwrap_or(0));

        let commits: Vec<Value> = revs
            .iter()
            .map(|x| {
                let c = store.commit(&x.req.commit).ok();
                json!({
                    "rev": x.req.rev, "sha": x.req.commit, "base": x.req.base, "state": x.state,
                    "subject": c.as_ref().map(|c| c.message.lines().next().unwrap_or("").to_string()).unwrap_or_else(|| x.subject.clone()),
                    "author": c.as_ref().map(|c| c.author.name.clone()).unwrap_or_else(|| agent.clone()),
                    "at_ms": c.as_ref().map(|c| c.author.when * 1000).unwrap_or(x.submitted_ms),
                })
            })
            .collect();

        let rules = self.rules().await?;
        let mut checks = checks(r, landing, rules.ci.typecheck.is_some(), self.get::<Value>(&format!("tr:{}:{cid}", r.req.task.clone().unwrap_or_default())).await?);
        // A queued PR waits for the landing loop: show why, if a run failed (trunk/stall.rs).
        if r.state == Phase::Queued {
            if let Some(s) = self.stall().await? {
                stall_check(&mut checks, &s, rules.ci.typecheck.is_some(), now() as i64);
            }
        }
        let size = r.size.or_else(|| change_size(store, &r.req.base, &r.req.commit));
        Ok(json!({
            "timeline": lines, "commits": commits, "checks": checks, "size": size,
            // A branch pushed with plain git keeps its name; one sent with `land` is its land/ ref.
            "head_ref": r.req.branch.clone().unwrap_or_else(|| format!("{}:land/{cid}-{}", r.req.fork, r.req.rev)),
            "base_ref": cfg.branch.trim_start_matches("refs/heads/"),
        }))
    }
}

fn outcome_words(outcome: &str) -> &'static str {
    match outcome {
        "merge" => "no extra step",
        "review" => "a person reviews it",
        "sign" => "a maintainer signs it",
        _ => "recorded",
    }
}

/// One row per check, as a git host shows them: pass, fail, pending, or skip.
fn checks(r: &ChangeRec, landing: &Value, typecheck: bool, trial: Option<Value>) -> Vec<Value> {
    let kind = r.reason.as_ref().and_then(|v| v["kind"].as_str()).unwrap_or("");
    let landed = r.state == Phase::Landed;
    let gated = !r.footprint.is_empty() || r.state != Phase::Queued;
    let row = |name: &str, status: &str, detail: String| json!({ "name": name, "status": status, "detail": detail });
    let mut out = vec![];

    // The gate only reads the paths. A conflict shows when the sequencer applies the PR to
    // main: in its batch, or in a trial for a competing attempt.
    let paths = format!("{} path{}", r.footprint.len(), if r.footprint.len() == 1 { "" } else { "s" });
    out.push(match kind {
        "conflict" => {
            let by: Vec<String> = r.reason.as_ref().and_then(|v| v["beaten_by"].as_array().cloned()).unwrap_or_default().iter().map(|b| b["number"].as_u64().map(|n| format!("#{n}")).unwrap_or_else(|| b["change_id"].as_str().unwrap_or("?").to_string())).collect();
            let with = if by.is_empty() { String::new() } else { format!(" with {}", by.join(", ")) };
            row("Paths and conflicts", "fail", format!("conflict in {}{with}: rebase on main", list(&r.reason, "paths")))
        }
        "fetch_retry" => row("Paths and conflicts", "pending", format!("could not fetch the commit from the fork ({}); the sequencer tries again", r.reason.as_ref().and_then(|v| v["detail"].as_str()).unwrap_or("no detail"))),
        "bad_base" | "missing" | "fetch_failed" | "bad_path" | "unknown_fork" => row("Paths and conflicts", "fail", r.reason.as_ref().and_then(|v| v["detail"].as_str()).unwrap_or(kind).replace('_', " ")),
        _ if landed || matches!(kind, "check_failed" | "ci_failed" | "ci_crashed") => row("Paths and conflicts", "pass", format!("{paths}; applied to main with no conflict")),
        _ if r.state == Phase::AwaitingPick => row("Paths and conflicts", "pass", format!("{paths}; applied to main in its trial with no conflict")),
        "not_picked" => row("Paths and conflicts", "skip", "not merged: another attempt was picked".into()),
        "superseded" => row("Paths and conflicts", "skip", format!("not merged: revision {} of this pull request replaced it", r.reason.as_ref().map(|v| v["rev"].to_string()).unwrap_or_default())),
        _ if gated && r.state != Phase::Ejected => row("Paths and conflicts", "pending", format!("{paths} known; not checked against the newest main yet: the sequencer applies it in its batch")),
        _ if gated => row("Paths and conflicts", "skip", format!("{paths} known; not applied to main")),
        _ => row("Paths and conflicts", "pending", "checked when the sequencer gates it".into()),
    });

    out.push(match (landed, kind) {
        (true, _) => row("Trunk checks", "pass", "every commit on main passes nerudo.toml's checks".into()),
        (_, "check_failed") => row("Trunk checks", "fail", r.reason.as_ref().map(|v| v["detail"].to_string()).unwrap_or_default()),
        (_, "not_picked") => row("Trunk checks", "skip", "not run: another attempt was picked".into()),
        _ if r.state == Phase::Ejected => row("Trunk checks", "skip", "not run".into()),
        _ => row("Trunk checks", "pending", "run on the batch it merges in".into()),
    });

    if typecheck {
        out.push(match (landed, kind) {
            (true, _) => row("Type check (ts-rust)", "pass", format!("passed on main; {} CI run{} in its batch", r.ci_runs, if r.ci_runs == 1 { "" } else { "s" })),
            (_, "ci_failed") => {
                let d = r.reason.as_ref().map(|v| v["diagnostics"].clone()).unwrap_or(Value::Null);
                let first = d.get(0).map(|x| format!("{} {}:{} {}", x["code"].as_str().unwrap_or("?"), x["file"].as_str().unwrap_or("?"), x["line"], x["text"].as_str().unwrap_or(""))).unwrap_or_default();
                json!({ "name": "Type check (ts-rust)", "status": "fail", "detail": first, "diagnostics": d })
            }
            (_, "ci_crashed") => row("Type check (ts-rust)", "fail", "the type checker crashed".into()),
            _ if r.state == Phase::Ejected => row("Type check (ts-rust)", "skip", "not run".into()),
            _ => row("Type check (ts-rust)", "pending", "runs once for the whole batch".into()),
        });
    }

    let needs = !r.seal_reasons.is_empty() || !landing["seal_reasons"].as_array().map(Vec::is_empty).unwrap_or(true);
    if needs || r.state == Phase::AwaitingSeal {
        let quorum = r.reason.as_ref().map(|v| v["quorum"].clone()).unwrap_or(Value::Null);
        let missing: Vec<String> = quorum
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| t["have"].as_array().map(Vec::len).unwrap_or(0) < t["need"].as_u64().unwrap_or(0) as usize)
            .map(|t| {
                let roles: Vec<&str> = t["roles"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                let who = if roles.is_empty() { "approver".to_string() } else { roles.join(" or ") };
                let (have, need) = (t["have"].as_array().map(Vec::len).unwrap_or(0) as u64, t["need"].as_u64().unwrap_or(0));
                let more = need.saturating_sub(have);
                format!("needs {more} {who} signature{} for {} ({have} of {need})", if more == 1 { "" } else { "s" }, t["kind"].as_str().unwrap_or("?"))
            })
            .collect();
        out.push(if landed || (!r.sealed_by.is_empty() && missing.is_empty()) {
            row("Approval", "pass", format!("signed by {}", if r.sealed_by.is_empty() { landing["sealed_by"].to_string() } else { r.sealed_by.join(", ") }))
        } else if r.state == Phase::AwaitingSeal {
            row("Approval", "pending", if missing.is_empty() { r.seal_reasons.join("; ") } else { missing.join("; ") })
        } else {
            row("Approval", "skip", r.seal_reasons.join("; "))
        });
    } else {
        out.push(row("Approval", "pass", "not required: no path in this PR needs a signature".into()));
    }

    if let Some(d) = r.decisions.last() {
        let p = d.probabilities.get(&d.choice).copied().unwrap_or(0.0);
        let status = if d.outcome == "merge" || landed { "pass" } else if r.state == Phase::AwaitingSeal { "pending" } else { "skip" };
        out.push(row("Decision model", status, format!("{} {p:.2} (threshold {:.2}) by {}: {}", d.choice, d.threshold, d.model, outcome_words(&d.outcome))));
    }

    if let Some(t) = trial {
        let status = match t["passed"].as_bool() {
            Some(true) => "pass",
            Some(false) => "fail",
            None => "pending",
        };
        out.push(row("Trial (competing attempt)", status, format!("main plus this PR alone: {} files +{} −{}", t["files"], t["adds"], t["dels"])));
    }
    out
}

/// A stall of the landing loop on a queued PR: the type check row says that CI is
/// unavailable, or a row "Sequencer" says what fails. The row carries the stall itself.
fn stall_check(checks: &mut Vec<Value>, s: &super::stall::Stall, typecheck: bool, now_ms: i64) {
    let row = |name: &str| json!({ "name": name, "status": "pending", "detail": super::stall::words(s, now_ms), "stall": s });
    match checks.iter_mut().find(|c| c["name"] == "Type check (ts-rust)") {
        Some(c) if typecheck && s.kind == "ci_unavailable" => *c = row("Type check (ts-rust)"),
        _ => checks.push(row("Sequencer")),
    }
}

fn list(reason: &Option<Value>, key: &str) -> String {
    reason.as_ref().and_then(|v| v[key].as_array().cloned()).unwrap_or_default().iter().filter_map(|x| x.as_str().map(str::to_string)).collect::<Vec<_>>().join(", ")
}
