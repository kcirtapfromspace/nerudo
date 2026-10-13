//! nerudo-git's Transport over the Workers `fetch` API, counting requests.
//!
//! Each request has a deadline (crate::retry). With no answer in ANSWER_MS (plus time to send
//! a large body), or a pause of IDLE_MS in the body of the answer, the request stops and fails
//! as a lost connection: `Error::Transport`, which is transient. The gate then keeps the change
//! queued (fetch_retry), and a push that fails goes through the attempt record: the next run
//! reads trunk to know whether the push landed (trunk.rs `reconcile`).

use crate::retry::within;
use futures::StreamExt;
use nerudo_git::{Error as GitError, Result as GitResult, Transport};
use std::cell::Cell;
use std::pin::pin;
use worker::{AbortController, Fetch, Headers, Method, Request, RequestInit};

/// The time to the status and headers of the answer. Longer than each request measured on
/// Artifacts (8.3 s for the first read of a 728 MB repo, cf/scale/mono.md); a stall of 24 s
/// was seen once under load (cf/scale/repos.md).
pub const ANSWER_MS: u64 = 30_000;
/// Added to ANSWER_MS for each MB that the request sends: a large push gets time to upload.
const UPLOAD_MS_PER_MB: u64 = 1_000;
/// The longest pause between two parts of the body of the answer. A large fetch can take
/// longer than ANSWER_MS in all, but it does not stop for this long.
pub const IDLE_MS: u64 = 30_000;
/// The longest answer a request takes. A pack is read whole into memory (nerudo_git
/// MAX_PACK_BYTES is the limit for one); nothing else the sequencer asks for comes near this.
/// The request stops at the limit, with an error that the same request will give again.
pub const MAX_BODY_BYTES: usize = 72 << 20;

#[derive(Default)]
pub struct FetchTransport {
    pub requests: Cell<u64>,
    /// Bytes of the response bodies.
    pub rx_bytes: Cell<u64>,
}

impl Transport for FetchTransport {
    async fn request(&self, method: &str, url: &str, headers: &[(&str, String)], body: Option<Vec<u8>>) -> GitResult<(u16, Vec<u8>)> {
        self.requests.set(self.requests.get() + 1);
        let err = |e: worker::Error| GitError::Transport(e.to_string());
        // The service, not the URL: "POST git-upload-pack".
        let what = format!("{method} {}", url.rsplit('/').next().unwrap_or_default());
        let h = Headers::new();
        for (k, v) in headers {
            h.set(k, v).map_err(err)?;
        }
        let mut init = RequestInit::new();
        init.with_method(Method::from(method.to_string())).with_headers(h);
        let deadline = ANSWER_MS + body.as_ref().map_or(0, |b| b.len() as u64 * UPLOAD_MS_PER_MB / 1_000_000);
        if let Some(b) = body {
            init.with_body(Some(js_sys::Uint8Array::from(b.as_slice()).into()));
        }
        let req = Request::new_with_init(url, &init).map_err(err)?;
        // At a deadline, abort: a request that is only dropped keeps its connection.
        let abort = AbortController::default();
        let send = Fetch::Request(req);
        let mut resp = match within(deadline, send.send_with_signal(&abort.signal())).await {
            Some(r) => r.map_err(err)?,
            None => {
                abort.abort();
                return Err(GitError::Transport(format!("{what}: no answer in {} s", deadline / 1000)));
            }
        };
        let status = resp.status_code();
        let mut body = Vec::new();
        // A response with no body (HEAD, 204) has no stream.
        if let Ok(stream) = resp.stream() {
            let mut stream = pin!(stream);
            loop {
                match within(IDLE_MS, stream.next()).await {
                    Some(Some(chunk)) => {
                        let chunk = chunk.map_err(err)?;
                        if body.len() + chunk.len() > MAX_BODY_BYTES {
                            abort.abort();
                            return Err(GitError::Protocol(format!("{what}: the answer is longer than {MAX_BODY_BYTES} bytes")));
                        }
                        body.extend_from_slice(&chunk);
                    }
                    Some(None) => break,
                    None => {
                        abort.abort();
                        return Err(GitError::Transport(format!("{what}: the answer stopped for {} s after {} bytes", IDLE_MS / 1000, body.len())));
                    }
                }
            }
        }
        self.rx_bytes.set(self.rx_bytes.get() + body.len() as u64);
        Ok((status, body))
    }
}

pub fn basic(user: &str, password: &str) -> String {
    format!("Basic {}", base64(format!("{user}:{password}").as_bytes()))
}

pub fn base64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut o = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        o.push(T[(n >> 18) as usize & 63] as char);
        o.push(T[(n >> 12) as usize & 63] as char);
        o.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        o.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    o
}
