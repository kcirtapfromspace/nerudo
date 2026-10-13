//! One rule for each call that leaves the Worker (cf/scale/resilience.md): the call has a
//! deadline, and after a failure the caller tries again after an interval that doubles,
//! with jitter, up to a maximum.
//!
//! - CI (ci.rs): a deadline for the answer of the CI Worker.
//! - git (transport.rs): a deadline for the answer of the git host, and for each pause in
//!   the body of that answer.
//! - The landing loop (trunk/stall.rs): after a run that failed, the next run starts after
//!   `backoff_ms`, and the queue shows why it waits.

use futures::future::{select, Either};
use std::future::Future;
use std::pin::pin;
use std::time::Duration;
use worker::Delay;

/// The interval after the first failure, and the longest interval.
pub const FIRST_MS: f64 = 1_000.0;
pub const MAX_MS: f64 = 30_000.0;
/// Each interval is longer by a random 0 to 25 %, so trunks that failed at the same time
/// (one CI service for all of them) do not all try again at the same time. The jitter only
/// adds time: an outage of T seconds never gets more tries than the intervals with no jitter.
pub const JITTER: f64 = 0.25;

/// The output of `fut`, or None if it takes more than `ms`. On None, `fut` is dropped: the
/// caller must also abort the request that it started (an AbortController), else the request
/// keeps one of the 6 connections of the invocation.
pub async fn within<F: Future>(ms: u64, fut: F) -> Option<F::Output> {
    match select(pin!(fut), Delay::from(Duration::from_millis(ms))).await {
        Either::Left((out, _)) => Some(out),
        Either::Right(_) => None,
    }
}

/// The interval after try number `attempts` failed (1 is the first failure): 1 s, 2 s, 4 s ...
/// at most 30 s, longer by `rand` × 25 %. `rand` is from 0 to 1 (Math.random in the Worker).
pub fn backoff_ms(attempts: u32, rand: f64) -> f64 {
    let base = (FIRST_MS * 2f64.powi(attempts.saturating_sub(1).min(16) as i32)).min(MAX_MS);
    base * (1.0 + JITTER * rand.clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The budget of the fault harness (cf/faults/run.py `backoff_budget`): the tries of one
    /// trunk during an outage of `t` seconds.
    fn budget(t: f64) -> f64 {
        2.0 + t.max(2.0).log2().ceil() + t / 30.0
    }

    /// The tries in an outage of `t` seconds: one at 0, then one after each interval. A
    /// failed try takes no time here; a slow one (a deadline) only makes fewer tries.
    fn tries(t: f64, rand: f64) -> u32 {
        let (mut at, mut n) = (0.0, 0);
        while at <= t * 1000.0 {
            n += 1;
            at += backoff_ms(n, rand);
        }
        n
    }

    #[test]
    fn doubles_and_stops_at_the_maximum() {
        let ms: Vec<f64> = (1..=8).map(|a| backoff_ms(a, 0.0)).collect();
        assert_eq!(ms, [1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0, 30_000.0, 30_000.0, 30_000.0]);
        assert_eq!(backoff_ms(0, 0.0), 1_000.0);
        assert_eq!(backoff_ms(u32::MAX, 0.0), 30_000.0);
    }

    #[test]
    fn jitter_only_adds_up_to_a_quarter() {
        for a in 1..=10 {
            let base = backoff_ms(a, 0.0);
            assert_eq!(backoff_ms(a, 1.0), base * 1.25);
            assert!(backoff_ms(a, 0.5) > base && backoff_ms(a, 0.5) < base * 1.25);
            assert_eq!(backoff_ms(a, 7.0), base * 1.25, "rand is clamped");
            assert_eq!(backoff_ms(a, -1.0), base, "rand is clamped");
        }
    }

    #[test]
    fn an_outage_stays_within_the_harness_budget() {
        for t in [1.0, 2.0, 5.0, 10.0, 20.0, 30.0, 45.0, 60.0, 120.0, 300.0, 600.0, 3_600.0, 86_400.0] {
            for rand in [0.0, 0.3, 1.0] {
                let n = tries(t, rand);
                assert!(f64::from(n) <= budget(t), "{t} s, rand {rand}: {n} tries, budget {}", budget(t));
            }
        }
        // Today's rule, one try each second, does not: 21 tries in 20 s.
        assert!(21.0 > budget(20.0));
    }
}
