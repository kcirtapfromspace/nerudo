//! The revisions of a pull request: the rules of cf/scale/backlog.md (T-8, T-9).
//!
//! A change id merges one time: a merged change id takes no new revision, and a plain git
//! push to a merged branch opens a new PR, `<branch id>..<n>` (trunk/git.rs). Revisions only
//! go forward. A new revision replaces an open older one:
//!
//!   POST land rev N+1   N+1 queued; N still queued or parked
//!   next round          N ejected {"kind": "superseded", "rev": N+1}; N+1 is gated
//!
//! The landing loop is the only writer of N's record, so a record it holds in memory is never
//! out of date. From the time the loop picks a batch until it records it, a new revision of a
//! PR in the batch is refused (409, `merging`). Before it applies the batch, the loop drops a
//! member whose PR got a newer revision after the round started; the next round closes it.
//! So once land accepts N+1, N cannot merge. A crash after the push leaves `attempt`, and the
//! fence holds until the next run reconciles it.

use super::*;
use serde_json::json;

/// How long a plain git push waits for a batch that merges a revision of its PR.
pub(super) const MERGE_WAIT_MS: f64 = 30_000.0;

/// The change ids of the batch that the landing loop applies and pushes now. Freed on drop,
/// on every way out of the round; drop wakes the requests that wait for the batch.
pub(super) struct Merging<'a>(&'a RefCell<Mem>, Vec<String>);

impl Drop for Merging<'_> {
    fn drop(&mut self) {
        let mut m = self.0.borrow_mut();
        for cid in &self.1 {
            m.merging.remove(cid);
        }
        for w in m.waiters.drain(..) {
            let _ = w.send(());
        }
    }
}

/// A batch member: the change, its allowance, and the trailers of its landed commit.
type Member = (ChangeRec, Option<String>, Vec<String>);

/// (change id, revision) of a key `{change_id}:{rev}`. A change id has no `:`.
pub(super) fn split_key(key: &str) -> Option<(&str, u32)> {
    let (cid, rev) = key.rsplit_once(':')?;
    Some((cid, rev.parse().ok()?))
}

/// Each key with a newer revision of its change id among `keys`, and that newest revision.
pub(super) fn stale<'a>(keys: impl IntoIterator<Item = &'a String>) -> BTreeMap<String, u32> {
    let keys: Vec<(&String, &str, u32)> = keys.into_iter().filter_map(|k| split_key(k).map(|(c, r)| (k, c, r))).collect();
    let mut newest: BTreeMap<&str, u32> = BTreeMap::new();
    for (_, cid, rev) in &keys {
        let n = newest.entry(cid).or_insert(*rev);
        *n = (*n).max(*rev);
    }
    keys.iter().filter(|(_, cid, rev)| newest[cid] > *rev).map(|(k, cid, _)| ((*k).clone(), newest[cid])).collect()
}

/// Why `POST land` refuses `rev` as a new revision of a PR: (kind, words). `latest`: its latest
/// revision. `merged`: its merged revision and landing. `merging`: the revision in a batch now.
pub(super) fn refusal(label: &str, rev: u32, latest: u32, merged: Option<(u32, u64)>, merging: Option<u32>) -> Option<(&'static str, String)> {
    if let Some((r, seq)) = merged {
        return Some((
            "merged",
            format!("{label} merged revision {r} as landing {seq}. A merged pull request takes no new revision: send the new work as a new change_id. A git push to its branch opens a new pull request."),
        ));
    }
    if let Some(r) = merging {
        return Some((
            "merging",
            format!("{label} is merging revision {r} now. Wait for its receipt: if it merges, send the new work as a new change_id; if not, send it as revision {}.", latest + 1),
        ));
    }
    if rev < latest {
        return Some(("older", format!("{label} has revision {latest}, and revision {rev} is older. Send the next revision as {} or higher.", latest + 1)));
    }
    None
}

/// The change id of the n-th PR of a branch whose first PR is `first` (n >= 2). Git does not
/// permit `..` in a branch name, so no branch has this id. None: it would be too long.
pub(super) fn successor(first: &str, n: u32) -> Option<String> {
    let cid = format!("{first}..{n}");
    valid_change_id(&cid).then_some(cid)
}

/// A 409 that names its kind, so a caller can tell `merging` (wait) from the others.
pub(super) fn refused(kind: &str, words: String) -> Result<Response> {
    Ok(Response::from_json(&json!({ "error": words, "kind": kind }))?.with_status(409))
}

impl Trunk {
    /// The merged revisions of a change id, oldest first: at most one under these rules, more
    /// for a PR from before them.
    pub(super) async fn merged_revs(&self, cid: &str, latest: u32) -> Result<Vec<ChangeRec>> {
        let mut out = vec![];
        for rev in 1..=latest {
            if let Some(r) = self.rec(cid, rev).await?.filter(|r| r.state == Phase::Landed) {
                out.push(r);
            }
        }
        Ok(out)
    }

    /// The revision of `cid` that the landing loop merges now: in the batch it picked, or in
    /// an attempt that a crash left for the next run to reconcile.
    pub(super) async fn merging(&self, cid: &str) -> Result<Option<u32>> {
        let att: Option<Attempt> = self.get("attempt").await?;
        if let Some(rev) = att.and_then(|a| a.landed.iter().filter_map(|l| split_key(&l.key)).find(|(c, _)| *c == cid).map(|(_, r)| r)) {
            return Ok(Some(rev));
        }
        Ok(self.mem.borrow().merging.get(cid).copied())
    }

    /// Why `POST land` refuses `req` as a new revision, if it does. Storage reads only, so that
    /// with the write after it, no other request runs in between (the input gate).
    pub(super) async fn revision_refusal(&self, req: &LandReq) -> Result<Option<(&'static str, String)>> {
        let Some(latest) = self.get::<u32>(&format!("latest:{}", req.change_id)).await? else {
            return Ok(None);
        };
        let label = match self.rec(&req.change_id, latest).await? {
            Some(r) => super::prinfo::pr_label(&r),
            None => req.change_id.clone(),
        };
        let merged = self.merged_revs(&req.change_id, latest).await?.pop().map(|r| (r.req.rev, r.seq.unwrap_or(0)));
        let merging = self.merging(&req.change_id).await?;
        Ok(refusal(&label, req.rev, latest, merged, merging))
    }

    /// Wait while the landing loop merges a revision of `cid`, at most `MERGE_WAIT_MS`.
    /// Returns the revision that still merges, if the wait ran out.
    pub(super) async fn wait_merged(&self, cid: &str) -> Result<Option<u32>> {
        let deadline = now() + MERGE_WAIT_MS;
        loop {
            let rev = self.merging(cid).await?;
            let left = deadline - now();
            if rev.is_none() || left <= 0.0 {
                return Ok(rev);
            }
            // The batch's end wakes us; a crash leaves `attempt`, so look again every 5 s.
            let (tx, rx) = oneshot::channel();
            self.mem.borrow_mut().waiters.push(tx);
            let _ = select(rx, Delay::from(Duration::from_millis(left.min(5_000.0) as u64))).await;
        }
    }

    /// At the start of each round: close each queued or parked revision that has a newer
    /// revision queued or parked. Returns the queue without them.
    pub(super) async fn supersede(&self, queue: Vec<String>) -> Result<Vec<String>> {
        let phases = [Phase::AwaitingSeal, Phase::AwaitingAllowance, Phase::AwaitingPick];
        let mut lists: Vec<Vec<String>> = vec![];
        for p in &phases {
            lists.push(self.get(&parked_key(p)).await?.unwrap_or_default());
        }
        let old = stale(queue.iter().chain(lists.iter().flatten()));
        if old.is_empty() {
            return Ok(queue);
        }
        let mut items = vec![];
        let mut closed = vec![];
        for (k, by) in &old {
            let Some(rec) = self.get::<ChangeRec>(&format!("c:{k}")).await? else { continue };
            if matches!(rec.state, Phase::Landed | Phase::Ejected) {
                continue;
            }
            let rec = eject(rec, json!({ "kind": "superseded", "rev": by }));
            items.push((format!("c:{k}"), serde_json::to_string(&rec)?));
            closed.push((rec, *by));
        }
        let queue: Vec<String> = queue.into_iter().filter(|k| !old.contains_key(k)).collect();
        items.push(("q".into(), serde_json::to_string(&queue)?));
        for (p, list) in phases.iter().zip(lists) {
            let kept: Vec<String> = list.into_iter().filter(|k| !old.contains_key(k)).collect();
            items.push((parked_key(p), serde_json::to_string(&kept)?));
        }
        // Not a closed PR: the PR stays open with its newer revision.
        let mut stats: Stats = self.get("stats").await?.unwrap_or_default();
        stats.superseded += closed.len() as u64;
        items.push(("stats".into(), serde_json::to_string(&stats)?));
        self.put_all(items).await?;
        for (rec, by) in &closed {
            self.change_event("superseded", format!("revision {by} replaced revision {}", rec.req.rev), &rec.req.change_id, None).await?;
        }
        self.notify();
        Ok(queue)
    }

    /// Between picking a batch and applying it: from here until the batch is recorded, a new
    /// revision of a PR in the batch is refused. A member whose PR got a newer revision after
    /// the round started is dropped, with its allowance reservation; the next round closes it.
    pub(super) async fn fence(&self, batch: Vec<Member>, budget: &mut BTreeMap<String, Allowance>) -> Result<(Vec<Member>, Merging<'_>)> {
        // Mark first, then read: a land after the mark sees it, and one before it wrote latest:.
        let cids: Vec<String> = batch.iter().map(|(r, ..)| r.req.change_id.clone()).collect();
        {
            let mut m = self.mem.borrow_mut();
            for (r, ..) in &batch {
                m.merging.insert(r.req.change_id.clone(), r.req.rev);
            }
        }
        let guard = Merging(&self.mem, cids);
        let mut keep = vec![];
        for (rec, allowance, trailers) in batch {
            let latest = self.get::<u32>(&format!("latest:{}", rec.req.change_id)).await?.unwrap_or(rec.req.rev);
            if latest > rec.req.rev {
                self.mem.borrow_mut().merging.remove(&rec.req.change_id);
                if let Some(al) = allowance.as_ref().and_then(|a| budget.get_mut(a)) {
                    al.reserved -= 1;
                }
                continue;
            }
            keep.push((rec, allowance, trailers));
        }
        Ok((keep, guard))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(k: &[&str]) -> Vec<String> {
        k.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn split_key_takes_the_last_colon() {
        assert_eq!(split_key("fix-1:3"), Some(("fix-1", 3)));
        assert_eq!(split_key("a..2:1"), Some(("a..2", 1)));
        assert_eq!(split_key("nocolon"), None);
        assert_eq!(split_key("x:y"), None);
    }

    #[test]
    fn stale_names_each_older_revision_and_its_replacement() {
        let k = keys(&["a:1", "b:1", "a:3", "c:2", "a:2", "b:1"]);
        let s = stale(&k);
        assert_eq!(s.into_iter().collect::<Vec<_>>(), vec![("a:1".to_string(), 3), ("a:2".to_string(), 3)]);
        assert!(stale(&keys(&["a:1", "b:2"])).is_empty());
        assert!(stale(&keys(&[])).is_empty());
    }

    #[test]
    fn refusal_order_merged_then_merging_then_older() {
        assert!(refusal("#1 a", 2, 1, None, None).is_none());
        assert!(refusal("#1 a", 5, 1, None, None).is_none(), "a gap is allowed");
        let (kind, words) = refusal("#1 a", 2, 1, Some((1, 7)), Some(1)).unwrap();
        assert_eq!(kind, "merged");
        assert!(words.contains("merged revision 1 as landing 7") && words.contains("new change_id"), "{words}");
        let (kind, words) = refusal("#1 a", 3, 2, None, Some(2)).unwrap();
        assert_eq!(kind, "merging");
        assert!(words.contains("revision 3"), "{words}");
        let (kind, words) = refusal("#1 a", 1, 3, None, None).unwrap();
        assert_eq!(kind, "older");
        assert!(words.contains("as 4 or higher"), "{words}");
    }

    #[test]
    fn successor_ids_are_not_branch_names_and_fit() {
        assert_eq!(successor("t-8-fix", 2).as_deref(), Some("t-8-fix..2"));
        assert_eq!(successor("t-8-fix", 12).as_deref(), Some("t-8-fix..12"));
        assert_eq!(successor(&"x".repeat(62), 2), None);
        assert_eq!(successor(&"x".repeat(61), 2).map(|c| c.len()), Some(64));
    }
}
