//! Metrics of the landing loop in Workers Analytics Engine: one data point for each round
//! (trunk/timings.rs), and one for each alarm that ends in an error. Queries over them give the
//! service levels in cf/scale/slo.md.
//!
//! The binding is optional. Without `METRICS` (local dev, the fault harness, a config that does
//! not name it), nothing is written. A write does not wait for I/O and never fails the round.
//!
//! The columns are fixed by position, because the queries name them so (`double4` is the
//! landed count). Add a column only at the end, and never move one.

use super::timings::Round;
use super::Trunk;
use crate::types::ChangeRec;
use worker::*;

const BINDING: &str = "METRICS";

/// Landing-latency buckets: a landing counts in each bucket whose limit it is within.
pub const WAIT_BUCKETS_MS: [f64; 3] = [2_000.0, 10_000.0, 30_000.0];

/// The time a change joined the queue: when it was sent, or when it came back from waiting for
/// a person (an approval, an allowance, a pick). Queue age and landing latency count from here,
/// so the time a person takes is not the sequencer's.
pub fn queued_since(r: &ChangeRec) -> f64 {
    r.queued_ms.unwrap_or(r.submitted_ms) as f64
}

/// The 20 doubles of a round, in column order (cf/scale/slo.md, "The data points").
pub fn round_doubles(r: &Round) -> [f64; 20] {
    let waits = &r.waits_ms;
    let within = |limit: f64| waits.iter().filter(|w| **w <= limit).count() as f64;
    let kinds = |ks: &[&str]| ks.iter().map(|k| r.ejected_by.get(*k).copied().unwrap_or(0)).sum::<usize>() as f64;
    let conflict = kinds(&["conflict"]);
    let failed = kinds(&["check_failed", "ci_failed", "ci_crashed"]);
    [
        r.queued as f64,
        r.oldest_ms,
        r.batched as f64,
        r.landed as f64,
        waits.iter().sum(),
        waits.iter().copied().fold(0.0, f64::max),
        within(WAIT_BUCKETS_MS[0]),
        within(WAIT_BUCKETS_MS[1]),
        within(WAIT_BUCKETS_MS[2]),
        r.requests as f64,
        r.rx_bytes as f64,
        r.pack_bytes as f64,
        r.fetch_ms,
        r.push_ms,
        r.ci_ms,
        r.round_ms,
        r.fetch_retries as f64,
        conflict,
        failed,
        r.ejected as f64 - conflict - failed,
    ]
}

/// What failed an alarm, from its error: "ci" (the CI Worker did not give a verdict), "git" (a
/// git host: a lost connection, an HTTP error, bad data) or "other".
pub fn error_cause(msg: &str) -> &'static str {
    if msg.contains("CI Worker") {
        "ci"
    } else if ["transport:", "protocol:", "corrupt:", "missing object:"].iter().any(|p| msg.starts_with(p)) {
        "git"
    } else {
        "other"
    }
}

/// At most `n` bytes of `s`, cut at a character boundary.
fn cut(s: &str, n: usize) -> &str {
    let mut end = s.len().min(n);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

impl Trunk {
    /// The trunk name the object was made from, or its id. An index has at most 96 bytes.
    fn metric_index(&self) -> String {
        let id = self.state.id();
        cut(&id.name().unwrap_or_else(|| id.to_string()), 96).to_string()
    }

    fn metric_write(&self, blobs: [&str; 3], doubles: [f64; 20]) {
        let Ok(ds) = self.env.analytics_engine(BINDING) else {
            return;
        };
        let index = self.metric_index();
        let point = AnalyticsEngineDataPointBuilder::new().indexes([index.as_str()]).blobs(blobs).doubles(doubles).build();
        if let Err(e) = ds.write_data_point(&point) {
            console_warn!("metrics: {e}");
        }
    }

    /// One data point for a finished round: blob1 "round", blob2 the type check of the batch.
    pub(super) fn metric_round(&self, r: &Round) {
        self.metric_write(["round", &r.ci, ""], round_doubles(r));
    }

    /// One data point for an alarm that ended in an error: blob1 "alarm_error", blob2 the
    /// cause, blob3 the start of the error. The doubles are 0.
    pub(super) fn metric_alarm_error(&self, e: &Error) {
        let msg = e.to_string();
        self.metric_write(["alarm_error", error_cause(&msg), cut(&msg, 256)], [0.0; 20]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn doubles_of_a_round() {
        let r = Round {
            queued: 40,
            oldest_ms: 12_500.0,
            batched: 16,
            landed: 15,
            ejected: 4,
            ejected_by: BTreeMap::from([("conflict".into(), 1), ("ci_failed".into(), 1), ("check_failed".into(), 1), ("fetch_failed".into(), 1)]),
            waits_ms: vec![1_000.0, 2_000.0, 9_000.0, 31_000.0],
            fetch_retries: 2,
            ..Round::default()
        };
        let d = round_doubles(&r);
        assert_eq!(d[0], 40.0);
        assert_eq!(d[1], 12_500.0);
        assert_eq!((d[2], d[3]), (16.0, 15.0));
        assert_eq!((d[4], d[5]), (43_000.0, 31_000.0));
        // Within 2 s, 10 s, 30 s: the buckets include their limit.
        assert_eq!((d[6], d[7], d[8]), (2.0, 3.0, 3.0));
        assert_eq!(d[16], 2.0);
        assert_eq!((d[17], d[18], d[19]), (1.0, 2.0, 1.0));
    }

    #[test]
    fn causes() {
        assert_eq!(error_cause("CI Worker answered HTTP 503"), "ci");
        // The errors of ci.rs after the deadline work (trunk/stall.rs reads the same text).
        assert_eq!(error_cause("ci_unavailable: no answer from the CI Worker in 25 s"), "ci");
        assert_eq!(error_cause("ci_unavailable: the CI Worker answered HTTP 503: down"), "ci");
        assert_eq!(error_cause("transport: POST git-receive-pack: no answer in 30 s"), "git");
        assert_eq!(error_cause("transport: POST git-receive-pack: HTTP 503: busy"), "git");
        assert_eq!(error_cause("protocol: ERR not our ref"), "git");
        assert_eq!(error_cause("simulated crash after push"), "other");
    }

    #[test]
    fn cut_at_a_char() {
        assert_eq!(cut("abc", 2), "ab");
        assert_eq!(cut("aé", 2), "a");
        assert_eq!(cut("ab", 9), "ab");
    }
}
