//! Where the landing loop spends its time: one record per round of `schedule_with`, the
//! last TIMINGS_KEPT of them served by `GET .../timings`, and one console line per round.
//!
//! The clock of a Worker moves only at I/O: `Date.now()` stands still while the isolate
//! computes. So a stage that only computes (footprint, apply, pack build) reads about 0 ms,
//! and its time shows in the next stage that waits on I/O. `round_ms` is the full round.

use super::{now, Trunk};
use crate::types::ChangeRec;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use worker::*;

/// Rounds kept in storage, newest last.
const TIMINGS_KEPT: usize = 64;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Round {
    /// The start of the alarm this round ran in: rounds with the same value ran in one alarm.
    pub alarm_ms: i64,
    pub at_ms: i64,
    /// The batch number, if the round pushed one.
    pub batch: Option<u64>,
    /// Changes on the queue at the start of the round; all of them go through the gate.
    pub queued: usize,
    /// Commits fetched from forks (or trunk) in the gate, and the time those fetches took.
    pub fetched: usize,
    pub fetch_ms: f64,
    pub fetch_max_ms: f64,
    /// Fetch of the trunk head and reconcile, in the first round of an alarm only.
    pub head_ms: f64,
    /// Reading the queue, the change records, the rules and the allowances.
    pub load_ms: f64,
    /// The gate without its fetches: footprint, seals, allowances, record writes, park.
    pub gate_ms: f64,
    pub divert_ms: f64,
    /// Choosing the batch, and writing the footprints of the changes that wait.
    pub pick_ms: f64,
    pub waiting: usize,
    /// apply_change and trunk_checks for each change of the batch.
    pub apply_ms: f64,
    pub ci_ms: f64,
    /// The intent record and the pack. Pack building only computes, so it shows in push_ms.
    pub pack_ms: f64,
    pub push_ms: f64,
    /// The atomic landing record, then the events (`events_ms` of it).
    pub record_ms: f64,
    pub events_ms: f64,
    pub batched: usize,
    pub landed: usize,
    pub ejected: usize,
    pub parked: usize,
    /// HTTP requests to git hosts in this round, bytes received from them, and the pack sent.
    pub requests: u64,
    pub rx_bytes: u64,
    pub pack_bytes: usize,
    pub round_ms: f64,
    /// The age of the oldest queued change at the start of the round (metrics::queued_since).
    pub oldest_ms: f64,
    /// Fetches that failed for a reason that can pass: those changes wait to fetch again.
    pub fetch_retries: usize,
    /// The type check of the batch: "" (none ran), "passed", "failed" (a change of the batch
    /// failed it) or "trunk_red" (trunk itself fails it, so nothing lands).
    pub ci: String,
    /// The ejected changes by the kind of their reason.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub ejected_by: BTreeMap<String, usize>,
    /// For each change the round landed, the time from the queue to the landing. Kept in
    /// memory only, for the metrics (trunk/metrics.rs).
    #[serde(skip)]
    pub waits_ms: Vec<f64>,
}

impl Round {
    /// Count the ejected changes, in total and by kind.
    pub fn eject(&mut self, ejected: &[ChangeRec]) {
        self.ejected = ejected.len();
        self.ejected_by.clear();
        for r in ejected {
            let kind = r.reason.as_ref().and_then(|x| x["kind"].as_str()).unwrap_or("other");
            *self.ejected_by.entry(kind.to_string()).or_default() += 1;
        }
    }
}

/// The time since the last lap.
pub struct Lap(f64);

impl Lap {
    pub fn start() -> Self {
        Lap(now())
    }

    pub fn take(&mut self) -> f64 {
        let t = now();
        let d = t - self.0;
        self.0 = t;
        d
    }
}

impl Trunk {
    /// Keep a finished round in memory (served at once), log it as one line, and write it to the
    /// metrics (trunk/metrics.rs).
    pub(super) fn round_done(&self, mut r: Round, started: f64) {
        r.round_ms = now() - started;
        console_log!("nerudo.round {}", serde_json::to_string(&r).unwrap_or_default());
        self.metric_round(&r);
        self.mem.borrow_mut().rounds.push(r);
    }

    /// The rounds of this alarm go to storage once, at its end, in the same write as `extra`.
    pub(super) async fn flush_timings(&self, extra: Vec<(String, String)>) -> Result<()> {
        let fresh = std::mem::take(&mut self.mem.borrow_mut().rounds);
        let mut items = extra;
        if !fresh.is_empty() {
            let mut kept: Vec<Round> = self.get("timings").await?.unwrap_or_default();
            kept.extend(fresh);
            let cut = kept.len().saturating_sub(TIMINGS_KEPT);
            items.push(("timings".into(), serde_json::to_string(&kept[cut..])?));
        }
        self.put_all(items).await
    }

    /// `GET .../timings`: the last rounds of the landing loop, oldest first, with the rounds
    /// of an alarm that still runs.
    pub(super) async fn api_timings(&self) -> Result<Response> {
        let mut rounds: Vec<Round> = self.get("timings").await?.unwrap_or_default();
        rounds.extend(self.mem.borrow().rounds.iter().cloned());
        Response::from_json(&serde_json::json!({ "rounds": rounds }))
    }
}
