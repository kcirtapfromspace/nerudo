//! nerudo on Cloudflare Workers. The Worker only routes: every trunk is one Durable
//! Object, and that object is the only thing that writes the trunk. The Hub object gives
//! trunks names, `{org}/{repo}` (hub.rs).

mod brand;
mod ci;
mod host;
mod hub;
mod hub_index;
mod hub_search;
mod names;
mod retry;
mod transport;
mod trunk;
mod types;

use worker::*;

pub use hub::Hub;
pub use trunk::Trunk;

/// Existing deployments and clients may still use the previous environment/header names.
/// New names always take precedence, including an explicitly empty value.
pub(crate) fn secret(env: &Env, name: &str) -> Result<String> {
    env.secret(name).or_else(|e| match name.strip_prefix("NERUDO_") {
        Some(suffix) => env.secret(&format!("LANDSEQ_{suffix}")),
        None => Err(e),
    }).map(|v| v.to_string())
}

pub(crate) fn variable(env: &Env, name: &str) -> Result<String> {
    env.var(name).or_else(|e| match name.strip_prefix("NERUDO_") {
        Some(suffix) => env.var(&format!("LANDSEQ_{suffix}")),
        None => Err(e),
    }).map(|v| v.to_string())
}

pub(crate) fn request_header(req: &Request, name: &str) -> Result<Option<String>> {
    match req.headers().get(name)? {
        Some(v) => Ok(Some(v)),
        None => match name.to_ascii_lowercase().strip_prefix("x-nerudo-") {
            Some(suffix) => req.headers().get(&format!("x-landseq-{suffix}")),
            None => Ok(None),
        },
    }
}

/// The front page for browsers. Trunks are private: each one opens with its share link.
const FRONT: &str = r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>nerudo</title>
<style>:root{--bg:#f7f6f2;--fg:#1c1b19;--mute:#6d6b64;--accent:#c2410c;--code:#efede6}@media (prefers-color-scheme: dark){:root{--bg:#121211;--fg:#ebeae5;--mute:#9a988f;--accent:#fb923c;--code:#23221f}}
body{margin:0;background:var(--bg);color:var(--fg);font:16px/1.6 ui-sans-serif,system-ui,-apple-system,sans-serif}main{max-width:680px;margin:0 auto;padding:48px 16px}
h1{font-size:34px;margin:0 0 8px}h1 i{color:var(--accent);font-style:normal}p{color:var(--mute)}code{background:var(--code);padding:1px 6px;border-radius:4px;font-size:14px}li{margin:6px 0}</style></head>
<body><main><h1>NERUDO</h1><p>A landing sequencer for coding agents, on Cloudflare Workers, Durable Objects and Artifacts.</p>
<ul><li>No pull requests. Agents commit and call <code>land</code> from Claude Code, Codex, Antigravity or Cursor (<code>nerudo mcp</code>).</li>
<li>One sequencer per trunk lands changes in batches, with one real CI run for each batch, and ejects what breaks trunk.</li>
<li>Humans seal what agents can't land alone: CI config, the rules, irreversible SQL.</li></ul>
<p>Each trunk opens with its share link: <code>nerudo share --trunk NAME</code>.</p></main></body></html>"#;

#[event(fetch)]
async fn main(req: Request, env: Env, ctx: Context) -> Result<Response> {
    let resp = match route(req, env.clone()).await {
        Err(e) if overloaded(&e) => busy(&e),
        other => other,
    };
    // Names that this request got from the Hub go into the colo's cache after the response.
    if names::to_put() {
        ctx.wait_until(names::flush(env));
    }
    resp
}

/// The phrases of the errors that a Durable Object stub gives when the object refuses a
/// request because too many wait for it (cf/scale/hub.md). The object did not run it.
const OVERLOAD: [&str; 3] = ["Durable Object is overloaded", "Too many requests queued", "Requests queued for too long"];
/// Seconds a client waits before it sends the request again.
const RETRY_AFTER_S: u32 = 1;

/// A Hub or Trunk object refused the request: overloaded. Any other error stays an error
/// (HTTP 500), so a real fault is not hidden.
fn overloaded(e: &Error) -> bool {
    if let Error::UnknownJsError { original, .. } = e {
        if js_sys::Reflect::get(original, &"overloaded".into()).ok().and_then(|v| v.as_bool()) == Some(true) {
            return true;
        }
    }
    let msg = e.to_string();
    OVERLOAD.iter().any(|p| msg.contains(p))
}

/// HTTP 503 with Retry-After: the client can send the same request again.
fn busy(e: &Error) -> Result<Response> {
    let headers = Headers::new();
    headers.set("retry-after", &RETRY_AFTER_S.to_string())?;
    let body = serde_json::json!({ "error": format!("busy, try again in {RETRY_AFTER_S} s: {e}"), "retry_after_s": RETRY_AFTER_S });
    Ok(Response::from_json(&body)?.with_status(503).with_headers(headers))
}

async fn route(req: Request, env: Env) -> Result<Response> {
    let path = req.path();
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    // A trunk id is 1 to 100 of [A-Za-z0-9._-] (hub::valid_trunk). Any other name gets no
    // object: each name makes one, and a request with no token could make them without end.
    if parts.len() >= 4 && parts[0] == "v1" && parts[1] == "trunks" && !parts[2].is_empty() {
        if !hub::valid_trunk(parts[2]) {
            return Ok(Response::from_json(&serde_json::json!({ "error": "no such trunk" }))?.with_status(404));
        }
        let stub = env.durable_object("TRUNK")?.id_from_name(parts[2])?.get_stub()?;
        // X-Nerudo-Repo comes only from the git routes by name, below; X-Nerudo-Trunk is the
        // name the object was reached by, which init holds the trunk id to.
        let req = hub::resend(&req, req.url()?.as_str())?;
        req.headers().set("x-nerudo-trunk", parts[2])?;
        return to_trunk(stub, req, parts[2], parts[3] == "git").await;
    }
    if parts.len() >= 3 && parts[0] == "v1" && parts[1] == "hub" {
        // A public repo's page, from the memo of names when it can.
        if let Some(resp) = hub::cached_view(&req, &env, &parts[2..]).await? {
            return Ok(resp);
        }
        return hub::api(req, &env, &parts[2..]).await;
    }
    // /v1/repos/{org}/{repo}/... is /v1/trunks/{trunk}/... by name.
    if parts.len() >= 5 && parts[0] == "v1" && parts[1] == "repos" {
        let rest = parts[4..].join("/");
        return hub::forward(req, &env, parts[2], parts[3], &rest, false).await;
    }
    // Web pages of one trunk: /t/{trunk}/...
    if parts.len() >= 2 && parts[0] == "t" && !parts[1].is_empty() {
        if !hub::valid_trunk(parts[1]) {
            return Ok(Response::error("no such trunk", 404)?);
        }
        let stub = env.durable_object("TRUNK")?.id_from_name(parts[1])?.get_stub()?;
        return to_trunk(stub, req, parts[1], false).await;
    }
    // git by name: /{org}/{repo}[.git]/info/refs | git-upload-pack | git-receive-pack. The
    // reserved org names keep these apart from the routes above.
    let git = match parts.as_slice() {
        [_, _, "info", "refs"] => Some("git/info/refs"),
        [_, _, "git-upload-pack"] => Some("git/git-upload-pack"),
        [_, _, "git-receive-pack"] => Some("git/git-receive-pack"),
        _ => None,
    };
    if let Some(rest) = git {
        let repo = parts[1].strip_suffix(".git").unwrap_or(parts[1]);
        return hub::forward(req, &env, parts[0], repo, rest, true).await;
    }
    if req.headers().get("accept")?.is_some_and(|a| a.contains("text/html")) {
        let headers = Headers::new();
        headers.set("content-type", "text/html; charset=utf-8")?;
        headers.set("content-security-policy", "default-src 'none'; style-src 'unsafe-inline'")?;
        return Ok(Response::ok(FRONT)?.with_headers(headers));
    }
    Response::ok(
        "nerudo: POST /v1/trunks/{trunk}/init | forks | land | allowances | seals | pause | resume | break-glass | visibility; \
         GET /v1/trunks/{trunk}/changes/{id}?wait=s | stats | summary | log | trunk; \
         GET /v1/hub/orgs | orgs/{owner} | users/{user} | me | repos/{owner}/{repo} | trunks/{trunk}; \
         PUT /v1/hub/orgs/{org} | users/{user} | repos/{owner}/{repo}; POST /v1/hub/users/{user}/invite; \
         ANY /v1/repos/{org}/{repo}/... (a trunk by name)",
    )
}

/// Request bodies up to this size go to a Trunk object whole (see `to_trunk`).
const WHOLE_BODY: u64 = 8 << 20;

/// A request to a Trunk object. The Worker reads a small body first and sends it whole. When the
/// object answered before it read a body that came as a stream, the Worker could not read the
/// rest of the request ("Can't read from request stream after response has been sent"), and the
/// connection of the request broke: in `wrangler dev`, the next request on it failed ("Network
/// connection lost"). A git body, or a body with no length, still goes as a stream.
///
/// If the object throws, the client gets a bare HTTP 500 and the invocation counts as a
/// success, so log the error with the trunk.
async fn to_trunk(stub: Stub, mut req: Request, trunk: &str, git: bool) -> Result<Response> {
    let len = req.headers().get("content-length")?.and_then(|n| n.parse::<u64>().ok()).unwrap_or(0);
    if !git && len > 0 && len <= WHOLE_BODY && !matches!(req.method(), Method::Get | Method::Head) {
        let body = req.bytes().await?;
        let mut init = RequestInit::new();
        // Redirect manual, as the request that came in: the client follows a redirect itself.
        // With the default (follow), the stub followed the 303 of a web form post, and the
        // client got the page at its target (200), or 403 there with no key.
        init.with_method(req.method())
            .with_headers(req.headers().clone())
            .with_redirect(RequestRedirect::Manual)
            .with_body(Some(js_sys::Uint8Array::from(&body[..]).into()));
        req = Request::new_with_init(req.url()?.as_str(), &init)?;
    }
    let res = stub.fetch_with_request(req).await;
    if let Err(e) = &res {
        console_error!("trunk {trunk}: {e}");
    }
    res
}

/// The request carries `Authorization: Bearer <the secret>`. The compare takes the same time
/// wherever the token differs.
pub(crate) fn bearer_ok(env: &Env, req: &Request, secret: &str) -> bool {
    let want = match crate::secret(env, secret) {
        Ok(s) => s.to_string(),
        Err(_) => return false,
    };
    let got = req.headers().get("Authorization").ok().flatten();
    let got = got.as_deref().and_then(|v| v.strip_prefix("Bearer ")).unwrap_or_default();
    !want.is_empty() && trunk::same_key(got.as_bytes(), want.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_overload_is_busy() {
        for m in [
            "Error: Durable Object is overloaded. Too many requests queued.",
            "Error: Durable Object is overloaded. Requests queued for too long.",
            "hub repo/a/b: Durable Object is overloaded. Too many requests queued.",
        ] {
            assert!(overloaded(&Error::RustError(m.into())), "{m}");
        }
        for m in ["Error: Network connection lost.", "internal error", "hub repo/a/b: HTTP 500: INTERNAL SERVER ERROR", "trunk not initialized: POST /init"] {
            assert!(!overloaded(&Error::RustError(m.into())), "{m}");
        }
    }
}
