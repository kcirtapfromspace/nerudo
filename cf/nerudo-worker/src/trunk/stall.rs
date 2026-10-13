//! Why the landing loop waits, and when it tries again (cf/scale/resilience.md).
//!
//! A run of the landing loop fails when CI gives no verdict (ci.rs), when the git host does
//! not answer (transport.rs), or for any other error. The changes stay queued. The next run
//! starts after 1 s, 2 s, 4 s ... at most 30 s, with jitter (crate::retry): one try for each
//! interval, not one each second. The record of the stall is in storage at `stall`, so a
//! restart keeps the count. GET .../queue, .../summary and .../changes/{id} (for a queued
//! change) show it, and the PR page shows it as a pending check. The next run that succeeds
//! deletes it.

use super::{now, Trunk};
use crate::retry::backoff_ms;
use crate::types::{ChangeRec, State as Phase};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use worker::*;

const KEY: &str = "stall";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Stall {
    /// `ci_unavailable`, `git_unavailable` or `error`.
    pub kind: String,
    /// The last error, at most 300 characters.
    pub detail: String,
    /// The first run that failed, and the runs that failed since then.
    pub since_ms: i64,
    pub attempts: u32,
    /// When the next run starts.
    pub next_ms: i64,
}

/// The kind of a failed run, from its error. CI errors start with ci::UNAVAILABLE; git
/// errors with "transport:" (nerudo_git::Error, a lost connection or an HTTP error), or are
/// a fetch with no packfile (transient too).
pub(crate) fn kind_of(error: &str) -> &'static str {
    if error.starts_with(crate::ci::UNAVAILABLE) {
        "ci_unavailable"
    } else if error.starts_with("transport:") || error == "protocol: fetch returned no packfile" {
        "git_unavailable"
    } else {
        "error"
    }
}

impl Trunk {
    /// After a run that failed: count it, and set the alarm for the next run.
    pub(super) async fn stalled(&self, error: &Error) -> Result<Stall> {
        let old: Option<Stall> = self.get(KEY).await?;
        let attempts = old.as_ref().map_or(0, |s| s.attempts).saturating_add(1);
        let delay = backoff_ms(attempts, js_sys::Math::random());
        let detail = error.to_string();
        let s = Stall {
            kind: kind_of(&detail).into(),
            detail: detail.chars().take(300).collect(),
            since_ms: old.map_or(now() as i64, |s| s.since_ms),
            attempts,
            next_ms: (now() + delay) as i64,
        };
        self.put(KEY, &s).await?;
        self.state.storage().set_alarm(Duration::from_millis(delay as u64)).await?;
        Ok(s)
    }

    /// After a run that succeeded.
    pub(super) async fn unstalled(&self) -> Result<()> {
        if self.stall().await?.is_some() {
            self.put_all(vec![(KEY.into(), "null".into())]).await?;
        }
        Ok(())
    }

    pub(super) async fn stall(&self) -> Result<Option<Stall>> {
        self.get(KEY).await
    }

    /// A change's receipt. A queued change also shows the stall, if the loop has one.
    pub(super) async fn receipt(&self, rec: &ChangeRec) -> Result<Response> {
        let mut v = serde_json::to_value(rec)?;
        if rec.state == Phase::Queued {
            if let Some(s) = self.stall().await? {
                v["stall"] = serde_json::to_value(s)?;
            }
        }
        Response::from_json(&v)
    }
}

/// The stall as one line for a person: "CI is unavailable for 41 s (5 tries failed, next in
/// 8 s): ...".
pub(super) fn words(s: &Stall, now_ms: i64) -> String {
    let what = match s.kind.as_str() {
        "ci_unavailable" => "CI is unavailable",
        "git_unavailable" => "the git host does not answer",
        _ => "the sequencer fails",
    };
    let secs = |ms: i64| (ms.max(0) + 999) / 1000;
    let detail = s.detail.split_once(": ").filter(|_| s.kind == "ci_unavailable").map_or(s.detail.as_str(), |(_, d)| d);
    let tries = if s.attempts == 1 { "1 try".to_string() } else { format!("{} tries", s.attempts) };
    format!("{what} for {} s ({tries} failed, next in {} s): {detail}", secs(now_ms - s.since_ms), secs(s.next_ms - now_ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert_eq!(kind_of("ci_unavailable: no answer from the CI Worker in 25 s"), "ci_unavailable");
        assert_eq!(kind_of(&nerudo_git::Error::Transport("POST git-receive-pack: no answer in 30 s".into()).to_string()), "git_unavailable");
        assert_eq!(kind_of(&nerudo_git::Error::Status(503, "POST git-upload-pack: HTTP 503".into()).to_string()), "git_unavailable");
        assert_eq!(kind_of(&nerudo_git::Error::Protocol("fetch returned no packfile".into()).to_string()), "git_unavailable");
        assert!(nerudo_git::Error::Protocol("fetch returned no packfile".into()).transient());
        assert_eq!(kind_of("protocol: ERR not our ref"), "error");
        assert_eq!(kind_of("trunk not initialized: POST /init"), "error");
        assert_eq!(kind_of(&nerudo_git::Error::Missing("abc".into()).to_string()), "error");
    }

    #[test]
    fn a_timeout_counts_as_a_failing_check_only_after_runs_in_a_row() {
        use crate::ci::{TIMED_OUT, UNAVAILABLE};
        let timeout = format!("{UNAVAILABLE}: {TIMED_OUT} in 25 s");
        let down = format!("{UNAVAILABLE}: the CI Worker answered HTTP 503: down");
        let stall = |kind: &str, attempts: u32, detail: &str| Stall { kind: kind.into(), detail: detail.into(), since_ms: 0, attempts, next_ms: 0 };
        // The first timeouts fail the run, as a CI outage does.
        assert!(!super::super::timed_out(&timeout, None));
        assert!(!super::super::timed_out(&timeout, Some(&stall("ci_unavailable", 2, &timeout))));
        // Three runs in a row of no answer: the tree is what takes too long.
        assert!(super::super::timed_out(&timeout, Some(&stall("ci_unavailable", 3, &timeout))));
        // A CI Worker that is down, or a git outage, is never the tree's fault.
        assert!(!super::super::timed_out(&down, Some(&stall("ci_unavailable", 3, &down))));
        assert!(!super::super::timed_out(&timeout, Some(&stall("ci_unavailable", 3, &down))));
        assert!(!super::super::timed_out(&timeout, Some(&stall("git_unavailable", 5, "transport: lost"))));
    }

    #[test]
    fn words_for_people() {
        let s = Stall { kind: "ci_unavailable".into(), detail: "ci_unavailable: the CI Worker answered HTTP 503: down".into(), since_ms: 1_000, attempts: 5, next_ms: 50_000 };
        assert_eq!(words(&s, 42_000), "CI is unavailable for 41 s (5 tries failed, next in 8 s): the CI Worker answered HTTP 503: down");
        let s = Stall { kind: "git_unavailable".into(), detail: "transport: POST git-receive-pack: no answer in 30 s".into(), since_ms: 0, attempts: 1, next_ms: 0 };
        assert_eq!(words(&s, 1_500), "the git host does not answer for 2 s (1 try failed, next in 0 s): transport: POST git-receive-pack: no answer in 30 s");
    }
}
