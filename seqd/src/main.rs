//! seqd: landing sequencer MVP.
//!
//! Agents push their commit to `refs/changes/<id>/<rev>` and POST /land. seqd is the only
//! writer of trunk: it batches footprint-disjoint changes, runs CI once per batch, bisects
//! on failure, and appends each landing to a hash-chained log.
//!
//! Not implemented (MVP): speculative execution of batch k+1, Raft replication, read-set
//! footprints, footprint learning.

mod config;
mod git;
mod sched;

use axum::extract::{Path, Query, State as AxState};
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use sched::{LandRequest, Rec, Shared, State};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser)]
struct Args {
    /// Bare repository that holds trunk and refs/changes/*.
    #[arg(long)]
    repo: PathBuf,
    /// Private working directory (staging worktree, log database).
    #[arg(long)]
    work: PathBuf,
    #[arg(long, default_value = "127.0.0.1:7878")]
    listen: String,
    #[arg(long, default_value = "main")]
    trunk: String,
    /// Passed to CI as CI_SECONDS (simulated build+test time).
    #[arg(long, default_value_t = 0.0)]
    ci_seconds: f64,
    #[arg(long, default_value_t = 16)]
    batch_cap: usize,
    /// Optional downstream mirror (e.g. a Forgejo or GitHub remote URL).
    #[arg(long)]
    mirror_url: Option<String>,
    /// Ref to update on the mirror; defaults to refs/heads/<trunk>.
    #[arg(long)]
    mirror_ref: Option<String>,
}

#[derive(Deserialize)]
struct WaitQuery {
    rev: Option<u32>,
    wait: Option<u64>,
}

/// A change id is 1 to 64 of [A-Za-z0-9._-]; a commit or base is 40 lowercase hex. These
/// go on git's command line and into the landed commit's message, so nothing else is taken
/// (an option string in place of a revision would be read by git as an option).
fn request_problem(req: &LandRequest) -> Option<&'static str> {
    let id_ok = !req.change_id.is_empty() && req.change_id.len() <= 64 && req.change_id.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    let oid_ok = |s: &str| s.len() == 40 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !id_ok {
        Some("change_id must be 1 to 64 of [A-Za-z0-9._-]")
    } else if !oid_ok(&req.commit) || !oid_ok(&req.base) {
        Some("commit and base must be 40 lowercase hex characters")
    } else if req.subject.chars().any(|c| c.is_control()) {
        Some("subject must be one line without control characters")
    } else {
        None
    }
}

async fn land(AxState(s): AxState<Arc<Shared>>, Json(req): Json<LandRequest>) -> Result<Json<Rec>, (axum::http::StatusCode, String)> {
    if let Some(why) = request_problem(&req) {
        return Err((axum::http::StatusCode::BAD_REQUEST, why.to_string()));
    }
    Ok(Json(sched::submit(&s, req)))
}

async fn change(AxState(s): AxState<Arc<Shared>>, Path(cid): Path<String>,
                Query(q): Query<WaitQuery>) -> Json<serde_json::Value> {
    // At most ten minutes: a larger value overflowed the Instant and dropped the connection.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(q.wait.unwrap_or(0).min(600));
    loop {
        let notified = s.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let rec = {
            let g = s.inner.lock().unwrap();
            let rev = q.rev.or_else(|| g.latest_rev.get(&cid).copied());
            rev.and_then(|r| g.changes.get(&(cid.clone(), r)).cloned())
        };
        match rec {
            None => return Json(serde_json::json!({"state": "unknown"})),
            Some(r) if r.state != State::Queued => return Json(serde_json::to_value(r).unwrap()),
            Some(r) => {
                if tokio::time::timeout_at(deadline, notified).await.is_err() {
                    return Json(serde_json::to_value(r).unwrap());
                }
            }
        }
    }
}

async fn stats(AxState(s): AxState<Arc<Shared>>) -> Json<serde_json::Value> {
    let g = s.inner.lock().unwrap();
    let mut v = serde_json::to_value(&g.stats).unwrap();
    v["head"] = g.head.clone().into();
    v["mirror"] = serde_json::to_value(&g.mirror).unwrap();
    v["queued"] = g.queue.len().into();
    Json(v)
}

#[derive(Deserialize)]
struct LogQuery {
    since: Option<u64>,
}

async fn log(AxState(s): AxState<Arc<Shared>>, Query(q): Query<LogQuery>) -> Json<serde_json::Value> {
    let g = s.inner.lock().unwrap();
    let since = q.since.unwrap_or(0);
    let entries: Vec<_> = g.log.iter().filter(|e| e.seq > since).collect();
    Json(serde_json::to_value(entries).unwrap())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let seq = sched::Sequencer::open(args.repo.clone(), args.work, &args.trunk, args.ci_seconds,
                                     args.batch_cap)?;
    let shared = seq.shared.clone();
    std::thread::spawn(move || seq.run());
    if let Some(url) = args.mirror_url {
        let s = shared.clone();
        let mirror_ref = args.mirror_ref.unwrap_or(format!("refs/heads/{}", args.trunk));
        std::thread::spawn(move || sched::run_mirror(s, args.repo, url, mirror_ref));
    }
    let app = Router::new()
        .route("/land", post(land))
        .route("/changes/{cid}", get(change))
        .route("/stats", get(stats))
        .route("/log", get(log))
        .with_state(shared);
    let listener = tokio::net::TcpListener::bind(&args.listen).await?;
    eprintln!("seqd listening on {}", args.listen);
    axum::serve(listener, app).await?;
    Ok(())
}
