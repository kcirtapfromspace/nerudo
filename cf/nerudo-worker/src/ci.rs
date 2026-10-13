//! The real part of a batch's CI run: a TypeScript type check in the nerudo-ci Worker
//! (tsc from ts-rust, as WebAssembly), through the `CI` service binding. `[ci] typecheck`
//! in nerudo.toml turns it on.

use crate::retry::within;
use nerudo_core::rules::Typecheck;
use nerudo_git::{Oid, Store};
use serde_json::{json, Map, Value};
use worker::*;

/// A check with no answer in this time fails as `ci_unavailable`, and the landing loop tries
/// again later (trunk/stall.rs). The CI Worker has 30 s of CPU, so a check that needs more
/// fails there anyway. With the first interval of 1 s, the next try comes within 30 s of a
/// hung one (cf/faults `ci_hang`). A tree whose check needs more than 25 s never lands:
/// raise this value for it.
pub const TIMEOUT_MS: u64 = 25_000;
/// The start of each error for a check that gave no verdict: CI is down, hangs, or answers
/// with no outcome. The Trunk shows it as the reason the queue waits (trunk/stall.rs).
pub const UNAVAILABLE: &str = "ci_unavailable";
/// The detail of a check that gave no answer in TIMEOUT_MS, as against one the CI Worker
/// refused: the landing loop tells the two apart (trunk.rs `timed_out`).
pub const TIMED_OUT: &str = "no answer from the CI Worker";

fn unavailable(detail: impl std::fmt::Display) -> Error {
    Error::RustError(format!("{UNAVAILABLE}: {detail}"))
}

/// The files the type check reads: TypeScript sources and JSON (tsconfig, package.json).
pub(crate) const EXTENSIONS: [&str; 5] = [".ts", ".tsx", ".mts", ".cts", ".json"];

pub enum Verdict {
    Passed,
    /// The tree fails the check. The value is the reason an ejected change carries.
    Failed(Value),
}

impl Verdict {
    pub fn passed(&self) -> bool {
        matches!(self, Verdict::Passed)
    }
}

fn g(e: nerudo_git::Error) -> Error {
    Error::RustError(e.to_string())
}

/// Type-check the tree of `commit`. An error means the check did not run (no CI Worker,
/// or it failed): the caller lands nothing, and every change stays queued for the next try.
/// The error starts with UNAVAILABLE when CI gave no verdict in TIMEOUT_MS.
pub async fn typecheck(env: &Env, store: &Store, commit: &Oid, tc: &Typecheck) -> Result<Verdict> {
    let tree = store.commit(commit).map_err(g)?.tree;
    let mut files = Map::new();
    for (path, entry) in store.flatten(&tree).map_err(g)? {
        if EXTENSIONS.iter().any(|x| path.ends_with(x)) {
            let text = String::from_utf8_lossy(store.blob(&entry.oid).map_err(g)?).into_owned();
            files.insert(path, Value::String(text));
        }
    }
    let headers = Headers::new();
    headers.set("content-type", "application/json")?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post).with_headers(headers).with_body(Some(json!({ "project": tc.tsconfig, "files": files }).to_string().into()));
    let req = Request::new_with_init("https://nerudo-ci/check", &init)?;
    // The same request with an abort signal: at the deadline, the call to the CI Worker stops.
    let abort = AbortController::default();
    let signal = web_sys::RequestInit::new();
    signal.set_signal(Some(&abort.signal()));
    let req: Request = web_sys::Request::new_with_request_and_init(req.inner(), &signal)?.into();
    // Each error names the CI Worker: the metrics count it against CI (trunk/metrics.rs).
    let ci = env.service("CI").map_err(|e| unavailable(format!("CI Worker unreachable: {e}")))?;
    let answer = within(TIMEOUT_MS, async {
        let mut resp = ci.fetch_request(req).await?;
        Ok::<_, Error>((resp.status_code(), resp.text().await?))
    })
    .await;
    let (status, text) = match answer {
        None => {
            abort.abort();
            return Err(unavailable(format!("{TIMED_OUT} in {} s", TIMEOUT_MS / 1000)));
        }
        Some(Err(e)) => return Err(unavailable(format!("CI Worker unreachable: {e}"))),
        Some(Ok(a)) => a,
    };
    if status != 200 {
        return Err(unavailable(format!("the CI Worker answered HTTP {status}: {}", text.chars().take(200).collect::<String>())));
    }
    let out: Value = serde_json::from_str(&text).map_err(|e| unavailable(format!("the CI Worker answered no JSON: {e}")))?;
    Ok(match out["outcome"].as_str() {
        Some("passed") => Verdict::Passed,
        Some("failed") => Verdict::Failed(json!({
            "kind": "ci_failed", "check": "typecheck", "errors": out["errors"], "diagnostics": out["diagnostics"],
        })),
        // The checker itself failed on this tree (a trap, its stack, its memory).
        Some("crashed") => Verdict::Failed(json!({ "kind": "ci_crashed", "check": "typecheck", "detail": out["error"] })),
        _ => return Err(unavailable(format!("the CI Worker answered no outcome: {}", out.to_string().chars().take(200).collect::<String>()))),
    })
}
