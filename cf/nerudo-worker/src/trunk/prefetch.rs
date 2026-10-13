//! The gate's fetches, before the gate looks at each change. Each fetch is a round trip to
//! the git host: about 25 ms to a local Gitea, about 300 ms to Artifacts. One change at a
//! time, these round trips set the pace of the landing loop on Artifacts (cf/scale/stages.md).
//! So the gate asks each fork once for all of its queued commits, and asks up to
//! FETCHES_IN_FLIGHT forks at once.
//!
//! The landing store holds trees, not file contents (cf/scale/mono.md). So the prefetch has
//! three steps, each one request to each host, the requests of a step at the same time:
//! 1. trunk: the queued changes' bases that the store lacks, as commits and trees;
//! 2. each fork: its queued commits, with the trees and files they add (as without the trees-
//!    only store, so a change usually needs no more requests). If the fork refuses, or sends a
//!    delta on a file the store lacks, the request goes again as commits and trees only;
//! 3. each fork: the files that a merge, a seal or a rule reads and the store still lacks
//!    (merge::blobs_needed); and trunk: the files that head moved on those paths, nerudo.toml
//!    and derived sources. In the usual case this step sends nothing.
//!
//! A failed request changes nothing for the gate. If it was for one change, the gate takes
//! its error as that change's fetch for this round (retry or eject, as before). If it was for
//! more changes, the gate fetches them again one at a time, so a change that its fork can't
//! give can't stop or eject the others.

use super::objects::Readers;
use super::{fetch_retry, now, Trunk};
use crate::transport::FetchTransport;
use crate::types::{ChangeRec, Fork};
use futures::stream::{self, StreamExt};
use nerudo_core::merge::blobs_needed;
use nerudo_core::{ChangeInput, Rules};
use nerudo_git::{fetch_body, fetch_pack, ingest, Error as GitError, FetchOpts, Oid, Remote, Store};
use std::collections::{BTreeMap, BTreeSet};
use worker::*;

/// Fetches that run at once. A Worker invocation has at most 6 connections open; more wait.
const FETCHES_IN_FLIGHT: usize = 6;
/// After a fork answers a git fetch with HTTP 5xx, the gate reads changes through the binding
/// for this long, and then tries git again. Artifacts answers 500 to every git fetch of a repo
/// with very big trees, and each try costs about 2 s.
const GIT_DOWN_MS: f64 = 600_000.0;

pub(super) struct Prefetched {
    /// Requests made.
    pub requests: usize,
    /// The error of each request that was for one change, by its commit.
    pub errors: BTreeMap<Oid, GitError>,
    /// The longest request, in ms.
    pub max_ms: f64,
}

/// One request: where, the commits of the changes it is for, and what it asks. `fallback`: what
/// it asks again when the host refuses it, or its pack cannot be read.
struct Want {
    remote: Remote,
    commits: Vec<Oid>,
    wants: Vec<Oid>,
    haves: Vec<Oid>,
    opts: FetchOpts,
    fallback: Option<FetchOpts>,
}

impl Trunk {
    pub(super) async fn prefetch(&self, rd: &mut Readers, t: &FetchTransport, store: &mut Store, rules: &Rules, head: Oid, recs: &[ChangeRec]) -> Result<Prefetched> {
        let trunk = &rd.remote.clone();
        let mut out = Prefetched { requests: 0, errors: BTreeMap::new(), max_ms: 0.0 };
        // Changes the gate fetches for this round, and their forks' remotes.
        let mut due: Vec<&ChangeRec> = vec![];
        let mut remotes: BTreeMap<String, Remote> = BTreeMap::new();
        for rec in recs {
            if fetch_retry(rec).is_some_and(|(_, next_ms)| next_ms > now()) {
                continue;
            }
            if !remotes.contains_key(&rec.req.fork) {
                // An unknown fork is the gate's to eject.
                let Some(fork) = self.get::<Fork>(&format!("f:{}", rec.req.fork)).await? else {
                    continue;
                };
                remotes.insert(rec.req.fork.clone(), Remote::new(&fork.remote, Some(fork.authorization)));
            }
            due.push(rec);
        }

        // 1. The bases, from trunk: one commit each and the trees that differ from head. On
        //    Artifacts the gate reads each base's commit alone, and its trees when needed.
        let bases: BTreeSet<Oid> = due.iter().map(|r| r.req.base).filter(|b| !store.contains(b)).collect();
        if !bases.is_empty() && !rd.lazy {
            let fallback = Some(FetchOpts { no_blobs: false, ..FetchOpts::TREES });
            let want = Want { remote: trunk.clone(), commits: vec![], wants: bases.into_iter().collect(), haves: vec![head], opts: FetchOpts::TREES, fallback };
            self.fetch_all(t, store, vec![want], &mut out).await;
        }

        // 2. The commits, from each fork, with what they add. On a trunk whose forks Artifacts
        //    could not serve by git (HTTP 5xx), the gate reads them through the binding instead,
        //    for GIT_DOWN_MS (trunk/objects.rs).
        let mut forks: BTreeMap<&str, Want> = BTreeMap::new();
        let git_down = rd.lazy && self.mem.borrow().git_down_until > now();
        for rec in due.iter().filter(|r| !git_down && !store.contains(&r.req.commit)) {
            let w = forks.entry(&rec.req.fork).or_insert_with(|| Want {
                remote: remotes[&rec.req.fork].clone(),
                commits: vec![],
                wants: vec![],
                haves: vec![head],
                opts: FetchOpts::default(),
                fallback: Some(FetchOpts { no_blobs: true, ..FetchOpts::default() }),
            });
            if !w.wants.contains(&rec.req.commit) {
                w.commits.push(rec.req.commit);
                w.wants.push(rec.req.commit);
            }
            if !w.haves.contains(&rec.req.base) {
                w.haves.push(rec.req.base);
            }
        }
        let failed = self.fetch_all(t, store, forks.into_values().collect(), &mut out).await;
        if rd.lazy && failed > 0 {
            self.mem.borrow_mut().git_down_until = now() + GIT_DOWN_MS;
        }

        // 3. The files the gates and landings read: the changes' sides from each fork, head's
        //    side from trunk.
        let mut forks: BTreeMap<&str, Want> = BTreeMap::new();
        let mut theirs: BTreeSet<Oid> = BTreeSet::new();
        let have: Vec<&ChangeRec> = due.iter().copied().filter(|r| store.contains(&r.req.commit) && !out.errors.contains_key(&r.req.commit)).collect();
        for rec in have {
            let input = ChangeInput { change_id: rec.req.change_id.clone(), rev: rec.req.rev, commit: rec.req.commit, base: rec.req.base, trailers: vec![] };
            // A change whose trees can't all be read is the gate's to eject or fetch again.
            let Ok((ours, head_side)) = self.fill(rd, Some(&rec.req.fork), t, store, |s| blobs_needed(s, rules, &head, &input)).await? else { continue };
            theirs.extend(head_side);
            if ours.is_empty() {
                continue;
            }
            let w = forks.entry(&rec.req.fork).or_insert_with(|| Want {
                remote: remotes[&rec.req.fork].clone(),
                commits: vec![],
                wants: vec![],
                haves: vec![],
                opts: FetchOpts::default(),
                fallback: None,
            });
            w.commits.push(rec.req.commit);
            for o in ours {
                if !w.wants.contains(&o) {
                    w.wants.push(o);
                }
            }
        }
        let mut wants: Vec<Want> = forks.into_values().collect();
        if !theirs.is_empty() {
            wants.push(Want { remote: trunk.clone(), commits: vec![], wants: theirs.into_iter().collect(), haves: vec![], opts: FetchOpts::default(), fallback: None });
        }
        self.fetch_all(t, store, wants, &mut out).await;
        Ok(out)
    }

    /// Send `wants`, FETCHES_IN_FLIGHT at once, and put each pack into `store`. A request that
    /// fails for a reason other than the network goes again with its fallback.
    /// Returns the number of requests that failed with HTTP 5xx.
    async fn fetch_all(&self, t: &FetchTransport, store: &mut Store, wants: Vec<Want>, out: &mut Prefetched) -> usize {
        let requests: Vec<(Want, Vec<u8>)> = wants.into_iter().filter_map(|w| fetch_body(&w.wants, &w.haves, w.opts, store).map(|body| (w, body))).collect();
        out.requests += requests.len();
        let mut packs = stream::iter(requests.iter().map(|(w, body)| async move {
            let t0 = now();
            let pack = fetch_pack(t, &w.remote, body.clone()).await;
            (w, pack, now() - t0)
        }))
        .buffer_unordered(FETCHES_IN_FLIGHT);
        let mut server_errors = 0;
        while let Some((w, pack, ms)) = packs.next().await {
            out.max_ms = out.max_ms.max(ms);
            if matches!(pack, Err(GitError::Status(s, _)) if s >= 500) {
                server_errors += 1;
            }
            let mut got = pack.and_then(|p| ingest(&p, store));
            if let (Err(e), Some(fallback)) = (&got, w.fallback) {
                if !e.transient() {
                    console_warn!("gate: a prefetch went again with its fallback: {e}");
                    if let Some(body) = fetch_body(&w.wants, &w.haves, fallback, store) {
                        out.requests += 1;
                        got = fetch_pack(t, &w.remote, body).await.and_then(|p| ingest(&p, store));
                    }
                }
            }
            match got {
                Ok(_) => {}
                Err(e) if w.commits.len() == 1 => {
                    out.errors.insert(w.commits[0], e);
                }
                // The gate fetches what each change still lacks, one change at a time.
                Err(e) => console_warn!("gate: a prefetch for {} changes failed: {e}", w.commits.len()),
            }
        }
        server_errors
    }
}
