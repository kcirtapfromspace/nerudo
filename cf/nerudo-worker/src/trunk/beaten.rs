//! What beat a PR. When the sequencer closes a PR for a conflict, the receipt names the
//! landings since the PR's base that changed the same units first (`beaten_by`). When the
//! type check fails on main, the receipt names the landings that moved main under it
//! (`moved`). Each entry carries the landing's pinned `Context:` trailer, so the agent that
//! rebuilds can read the plan it collided with (GET context/{change}).
//!
//! Main is linear, one commit per landing, so a walk of first parents finds them. Each
//! commit names its change in its `Change-Id` and `Revision` trailers.

use super::context::CONTEXT_TRAILER;
use super::ui::agent_of;
use super::*;
use nerudo_core::merge::{Ejection, Prior};
use serde_json::{json, Value};

/// Commits walked back from the tip, at most.
const WALK: usize = 200;
/// Landings listed for a CI failure.
const MOVED: usize = 3;

/// The directory of a path: "src" for src/cart.ts, "" for a file at the root.
fn dir_of(p: &str) -> &str {
    p.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

impl Trunk {
    /// The landings from `from` back to `base` (not included), newest first, each with the
    /// footprint of its record. The walk stops after WALK commits, at a commit that is not in
    /// `store`, or at the root.
    async fn landings_since(&self, store: &Store, from: Oid, base: Oid) -> Result<Vec<(Prior, Vec<String>)>> {
        let cfg = self.cfg().await?;
        let mut out = vec![];
        let mut cur = Some(from);
        for _ in 0..WALK {
            let Some(oid) = cur.filter(|o| *o != base) else { break };
            let Ok(c) = store.commit(&oid) else { break };
            cur = c.parents.first().copied();
            let (Some(cid), Some(rev)) = (c.trailer("Change-Id"), c.trailer("Revision").and_then(|r| r.parse::<u32>().ok())) else {
                continue;
            };
            let Some(rec) = self.get::<ChangeRec>(&format!("c:{cid}:{rev}")).await? else { continue };
            let prior = Prior {
                change_id: cid,
                rev,
                number: rec.number,
                agent: agent_of(&cfg.trunk, &rec.req.fork).to_string(),
                subject: c.message.lines().next().unwrap_or("").to_string(),
                commit: oid.to_hex(),
                context: c.trailer(CONTEXT_TRAILER),
                paths: vec![],
            };
            out.push((prior, rec.footprint));
        }
        Ok(out)
    }

    /// Read the trunk commits from `from` back to `base` that the store lacks (it holds one
    /// commit of trunk and the ones it landed), at most WALK, without file contents, so that
    /// `landings_since` can walk them. A failure only makes the receipt name fewer landings.
    pub(super) async fn load_history(&self, t: &FetchTransport, trunk: &Remote, store: &mut Store, from: Oid, base: Oid) {
        let mut cur = from;
        for _ in 0..WALK {
            if cur == base {
                return;
            }
            match store.commit(&cur) {
                Ok(c) => match c.parents.first() {
                    Some(p) => cur = *p,
                    None => return,
                },
                Err(_) => {
                    let opts = FetchOpts { depth: Some(WALK as u32), no_blobs: true, any_haves: false };
                    if let Err(e) = fetch_with(t, trunk, &[cur], &[base], opts, store).await {
                        console_warn!("reading trunk history for a receipt: {e}");
                    }
                    return;
                }
            }
        }
    }

    /// The reason for an ejection at apply. For a conflict, it adds `beaten_by`: the landings
    /// since the PR's base whose footprint holds the unit of a conflicting path. `from` is
    /// the commit the PR was applied on.
    pub(super) async fn apply_ejection(&self, rules: &Rules, store: &Store, from: Oid, rec: &ChangeRec, e: Ejection) -> Result<Value> {
        let Ejection::Conflict { paths, hunks, .. } = e else {
            return Ok(serde_json::to_value(&e)?);
        };
        let mut beaten_by = vec![];
        for (mut p, units) in self.landings_since(store, from, rec.req.base).await? {
            p.paths = paths.iter().filter(|x| units.contains(&rules.unit_of(x))).cloned().collect();
            if !p.paths.is_empty() {
                beaten_by.push(p);
            }
        }
        // Oldest first: the first landing to take the lines is the one that beat it.
        beaten_by.reverse();
        Ok(serde_json::to_value(&Ejection::Conflict { paths, hunks, beaten_by })?)
    }

    /// A ci_failed reason gains `moved`: at most MOVED landings since the PR's base, from
    /// `under` (the commit the PR was applied on) back. First the landings that share a unit
    /// with the PR or hold a file that the type check names; then the landings in the
    /// directory of such a file; then the newest.
    pub(super) async fn main_moved(&self, store: &Store, under: Oid, rec: &ChangeRec, mut reason: Value) -> Result<Value> {
        if reason["kind"] != "ci_failed" {
            return Ok(reason);
        }
        let named: BTreeSet<String> = reason["diagnostics"].as_array().into_iter().flatten().filter_map(|d| d["file"].as_str().map(str::to_string)).collect();
        let dirs: BTreeSet<&str> = named.iter().map(|f| dir_of(f)).collect();
        let mut all: Vec<(u8, Prior)> = vec![];
        for (mut p, units) in self.landings_since(store, under, rec.req.base).await? {
            let close: Vec<String> = units.iter().filter(|u| rec.footprint.contains(u) || named.contains(*u)).cloned().collect();
            let near: Vec<String> = units.iter().filter(|u| dirs.contains(dir_of(u))).cloned().collect();
            let score = if !close.is_empty() { 2 } else if !near.is_empty() { 1 } else { 0 };
            p.paths = if close.is_empty() { near } else { close };
            all.push((score, p));
        }
        all.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
        let moved: Vec<Prior> = all.into_iter().take(MOVED).map(|(_, p)| p).collect();
        reason["moved"] = json!(moved);
        Ok(reason)
    }
}

#[cfg(test)]
mod tests {
    use super::dir_of;

    #[test]
    fn dirs() {
        assert_eq!(dir_of("src/cart.ts"), "src");
        assert_eq!(dir_of("a/b/c.ts"), "a/b");
        assert_eq!(dir_of("README.md"), "");
    }
}
