//! A push with no answer, pushed again (cf/scale/resilience.md).
//!
//! A push that reached its deadline (transport.rs) or lost its connection may still reach
//! the git host later: a server can apply it after the Worker stopped waiting. The attempt
//! record stays, and the next run reads trunk (`reconcile`). If trunk is still at the start
//! of the attempt, the push may not have happened yet, or it never will. If the sequencer then
//! made a new batch from the same start, that would be a different commit with the same `old`.
//! When the late push then landed first, the new push would fail as an external move, and the
//! changes of the late push would be on main with no landing record.
//!
//! So the sequencer sends the same push again: the same `old`, the same `new`. The two pushes
//! are one compare-and-swap. At most one of them changes the ref, and the result is the same.
//! This needs the commits of the attempt in memory. After a restart they are gone, and the
//! sequencer makes a new batch as before (a crash stops the push of the old process too).

use super::{g, Trunk};
use crate::transport::FetchTransport;
use crate::types::{Attempt, Config, Stats};
use nerudo_git::{ls_refs, push, write_pack, Oid, RefUpdate, Remote, Store};
use worker::*;

impl Trunk {
    /// `actual` is where trunk is now. Returns where trunk is after the push again: the tip
    /// of the attempt if it landed, else `actual` or a newer read. An error keeps the attempt
    /// record, and the next run (after the interval of trunk/stall.rs) asks again.
    pub(super) async fn push_again(&self, cfg: &Config, t: &FetchTransport, trunk: &Remote, store: &Store, att: &Attempt, actual: Oid) -> Result<Oid> {
        if actual != att.start || att.tip == att.start || !store.contains(&att.tip) {
            return Ok(actual);
        }
        // Only what the batch changed, as the first push (never a walk of the whole tree). If
        // the store can't give the pack, make a new batch as after a restart.
        let Ok(pack) = store.new_objects(&att.tip, &att.start).and_then(|objs| store.object_list(&objs)).map(|l| write_pack(&l)) else {
            return Ok(actual);
        };
        let update = [RefUpdate { name: cfg.branch.clone(), old: att.start, new: att.tip }];
        let report = push(t, trunk, &update, pack).await.map_err(g)?;
        let mut stats: Stats = self.get("stats").await?.unwrap_or_default();
        stats.pushes_again += 1;
        self.put("stats", &stats).await?;
        if report.all_ok() {
            console_warn!("reconcile: batch {} pushed again, and it landed", att.batch);
            return Ok(att.tip);
        }
        // Refused: the first push landed, someone moved trunk, or the host refused for a
        // reason of its own (a lock that the first push still holds).
        let refs = ls_refs(t, trunk, &cfg.branch).await.map_err(g)?;
        let now_at = refs.iter().find(|(n, _)| n == &cfg.branch).map(|(_, o)| *o).unwrap_or(att.start);
        if now_at == att.start {
            // Trunk did not move, so the first push can still land. Keep the attempt record.
            return Err(Error::RustError(format!("the git host refused batch {} again with main at its start: {:?}", att.batch, report.refs)));
        }
        Ok(now_at)
    }
}
