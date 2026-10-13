//! Smart HTTP client: protocol v2 `ls-refs` and `fetch`, v1 `receive-pack`.
//!
//! Stateless: every call is a complete HTTP request through [`Transport`], which makes
//! it usable from a Durable Object where the only I/O primitive is `fetch`.

use crate::error::{Error, Result};
use crate::oid::Oid;
use crate::pack::parse_pack;
use crate::pktline::{pkt_str, text, Pkt, PktReader, DELIM, FLUSH};
use crate::store::Store;

const AGENT: &str = "agent=nerudo/0.1";

/// HTTP as the engine sees it. Implemented over `fetch` in the Worker and over a
/// socket in native tests.
#[allow(async_fn_in_trait)]
pub trait Transport {
    async fn request(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, String)],
        body: Option<Vec<u8>>,
    ) -> Result<(u16, Vec<u8>)>;
}

#[derive(Clone, Debug)]
pub struct Remote {
    /// e.g. https://<account>.artifacts.cloudflare.net/git/ns/repo.git
    pub url: String,
    /// Full Authorization header value ("Basic ..." or "Bearer ...").
    pub authorization: Option<String>,
}

impl Remote {
    pub fn new(url: &str, authorization: Option<String>) -> Self {
        Self { url: url.trim_end_matches('/').to_string(), authorization }
    }

    fn headers(&self, extra: &[(&'static str, &str)]) -> Vec<(&'static str, String)> {
        let mut h: Vec<(&'static str, String)> = extra.iter().map(|(k, v)| (*k, v.to_string())).collect();
        if let Some(a) = &self.authorization {
            h.push(("Authorization", a.clone()));
        }
        h
    }
}

async fn post<T: Transport>(t: &T, remote: &Remote, service: &str, body: Vec<u8>, v2: bool) -> Result<Vec<u8>> {
    let ctype = format!("application/x-{service}-request");
    let accept = format!("application/x-{service}-result");
    let mut hdrs = vec![("Content-Type", ctype.as_str()), ("Accept", accept.as_str())];
    if v2 {
        hdrs.push(("Git-Protocol", "version=2"));
    }
    let url = format!("{}/{service}", remote.url);
    let (status, resp) = t.request("POST", &url, &remote.headers(&hdrs), Some(body)).await?;
    if status != 200 {
        return Err(Error::Status(status, format!("POST {service}: HTTP {status}: {}", String::from_utf8_lossy(&resp[..resp.len().min(300)]))));
    }
    Ok(resp)
}

fn v2_command(cmd: &str, args: &[String]) -> Vec<u8> {
    let mut b = pkt_str(&format!("command={cmd}\n"));
    b.extend(pkt_str(&format!("{AGENT}\n")));
    b.extend_from_slice(DELIM);
    for a in args {
        b.extend(pkt_str(&format!("{a}\n")));
    }
    b.extend_from_slice(FLUSH);
    b
}

/// List refs under `prefix` (e.g. "refs/heads/") with protocol v2 `ls-refs`.
pub async fn ls_refs<T: Transport>(t: &T, remote: &Remote, prefix: &str) -> Result<Vec<(String, Oid)>> {
    let body = v2_command("ls-refs", &[format!("ref-prefix {prefix}")]);
    let resp = post(t, remote, "git-upload-pack", body, true).await?;
    let mut r = PktReader::new(&resp);
    let mut out = vec![];
    while let Some(p) = r.next_pkt()? {
        match p {
            Pkt::Data(d) => {
                let line = text(d);
                let mut parts = line.split(' ');
                let oid = parts.next().unwrap_or_default();
                let name = parts.next().unwrap_or_default();
                if oid == "unborn" {
                    continue;
                }
                out.push((name.to_string(), Oid::from_hex(oid)?));
            }
            Pkt::Flush => break,
            _ => {}
        }
    }
    Ok(out)
}

/// Fetch `wants` (and everything they need that isn't reachable from `haves`) into `store`.
/// Returns the number of objects received.
pub async fn fetch<T: Transport>(t: &T, remote: &Remote, wants: &[Oid], haves: &[Oid], store: &mut Store) -> Result<usize> {
    fetch_with(t, remote, wants, haves, FetchOpts::default(), store).await
}

/// `fetch` that asks for only the last `depth` commits of history (`deepen`), when given:
/// with depth 1, a commit and its tree, not the history behind it.
pub async fn fetch_depth<T: Transport>(t: &T, remote: &Remote, wants: &[Oid], haves: &[Oid], depth: Option<u32>, store: &mut Store) -> Result<usize> {
    fetch_with(t, remote, wants, haves, FetchOpts { depth, ..FetchOpts::default() }, store).await
}

/// What a fetch asks the server to leave out.
#[derive(Clone, Copy, Debug, Default)]
pub struct FetchOpts {
    /// Commits of history to send (`deepen`); None: all.
    pub depth: Option<u32>,
    /// Send commits and trees only (`filter blob:none`). A blob named in `wants` still comes.
    pub no_blobs: bool,
    /// Send every `haves` commit, also one the store does not hold: a caller that knows the
    /// remote has it and does not need its objects (a commit of trunk, say).
    pub any_haves: bool,
}

impl FetchOpts {
    /// One commit and its trees, no history and no file contents: what the sequencer reads
    /// of trunk. Files come later by id, only the ones a landing reads.
    pub const TREES: FetchOpts = FetchOpts { depth: Some(1), no_blobs: true, any_haves: false };
}

/// `fetch` with options. A server that refuses the filter (git without
/// uploadpack.allowFilter, say) gets the same request again without it.
pub async fn fetch_with<T: Transport>(t: &T, remote: &Remote, wants: &[Oid], haves: &[Oid], opts: FetchOpts, store: &mut Store) -> Result<usize> {
    match fetch_once(t, remote, wants, haves, opts, store).await {
        Err(e) if opts.no_blobs && !e.transient() => fetch_once(t, remote, wants, haves, FetchOpts { no_blobs: false, ..opts }, store).await.map_err(|e2| match e2 {
            Error::Protocol(m) => Error::Protocol(format!("{m} (and with filter blob:none: {e})")),
            other => other,
        }),
        other => other,
    }
}

async fn fetch_once<T: Transport>(t: &T, remote: &Remote, wants: &[Oid], haves: &[Oid], opts: FetchOpts, store: &mut Store) -> Result<usize> {
    let Some(body) = fetch_body(wants, haves, opts, store) else {
        return Ok(0);
    };
    let pack = fetch_pack(t, remote, body).await?;
    ingest(&pack, store)
}

/// The request that `fetch_with` sends, or None if `store` has every want. To run several
/// fetches at once, make each request first, send them with [`fetch_pack`] (it does not use
/// the store), then [`ingest`] each pack.
pub fn fetch_body(wants: &[Oid], haves: &[Oid], opts: FetchOpts, store: &Store) -> Option<Vec<u8>> {
    let wants: Vec<&Oid> = wants.iter().filter(|w| !store.contains(w)).collect();
    if wants.is_empty() {
        return None;
    }
    let mut args: Vec<String> = vec!["ofs-delta".into(), "no-progress".into()];
    args.extend(wants.iter().map(|w| format!("want {w}")));
    args.extend(haves.iter().filter(|h| opts.any_haves || store.contains(h)).map(|h| format!("have {h}")));
    if let Some(d) = opts.depth {
        args.push(format!("deepen {d}"));
    }
    if opts.no_blobs {
        args.push("filter blob:none".into());
    }
    args.push("done".into());
    Some(v2_command("fetch", &args))
}

/// The largest pack a fetch takes, as sent. A pack this size inflates to several times as
/// much; the caller's memory (a Durable Object has 128 MB) must hold both while it is read.
pub const MAX_PACK_BYTES: usize = 64 << 20;

/// Send a request from [`fetch_body`]. Returns the pack.
pub async fn fetch_pack<T: Transport>(t: &T, remote: &Remote, body: Vec<u8>) -> Result<Vec<u8>> {
    let resp = post(t, remote, "git-upload-pack", body, true).await?;
    if resp.len() > MAX_PACK_BYTES + (1 << 20) {
        return Err(Error::Protocol(format!("the fetch answered with {} bytes, over the limit of {MAX_PACK_BYTES}", resp.len())));
    }
    let mut r = PktReader::new(&resp);
    let mut pack = Vec::new();
    let mut in_pack = false;
    while let Some(p) = r.next_pkt()? {
        match p {
            Pkt::Data(d) if in_pack => match d.first() {
                Some(1) => pack.extend_from_slice(&d[1..]),
                Some(2) => {}
                Some(3) => return Err(Error::Protocol(format!("remote error: {}", text(&d[1..])))),
                _ => return Err(Error::Protocol("bad sideband byte".into())),
            },
            Pkt::Data(d) if text(d) == "packfile" => in_pack = true,
            Pkt::Data(d) if text(d).starts_with("ERR ") => return Err(Error::Protocol(text(d))),
            Pkt::Flush if in_pack => break,
            _ => {}
        }
    }
    // The response holds a second copy of the pack: it is freed on return, before the
    // caller inflates the objects.
    if pack.is_empty() {
        return Err(Error::Protocol(crate::error::NO_PACKFILE.into()));
    }
    Ok(pack)
}

/// Put the objects of a fetched pack into `store`. Returns the number of objects.
pub fn ingest(pack: &[u8], store: &mut Store) -> Result<usize> {
    let objects = parse_pack(pack, Some(store))?;
    let n = objects.len();
    for o in objects {
        store.insert(o);
    }
    Ok(n)
}

#[derive(Clone, Debug)]
pub struct RefUpdate {
    pub name: String,
    /// The value the caller believes the ref has now. The server rejects the update if it
    /// differs: this is the compare-and-swap the sequencer relies on.
    pub old: Oid,
    pub new: Oid,
}

#[derive(Clone, Debug, Default)]
pub struct PushReport {
    pub unpack_ok: bool,
    /// (ref, None = ok | Some(reason) = rejected)
    pub refs: Vec<(String, Option<String>)>,
}

impl PushReport {
    pub fn all_ok(&self) -> bool {
        self.unpack_ok && !self.refs.is_empty() && self.refs.iter().all(|(_, r)| r.is_none())
    }
}

/// Push with v1 receive-pack. `pack` must contain every object the server lacks.
pub async fn push<T: Transport>(t: &T, remote: &Remote, updates: &[RefUpdate], pack: Vec<u8>) -> Result<PushReport> {
    // Capability advertisement (also tells us whether the server can report per ref).
    let url = format!("{}/info/refs?service=git-receive-pack", remote.url);
    let (status, adv) = t.request("GET", &url, &remote.headers(&[]), None).await?;
    if status != 200 {
        return Err(Error::Status(status, format!("receive-pack advertisement: HTTP {status}")));
    }
    let caps = advertised_caps(&adv)?;
    let mut want_caps = vec!["report-status"];
    let sideband = caps.iter().any(|c| c == "side-band-64k");
    if sideband {
        want_caps.push("side-band-64k");
    }
    if updates.len() > 1 && caps.iter().any(|c| c == "atomic") {
        want_caps.push("atomic");
    }
    // A deletion (new = zero) needs the server's delete-refs; a push that only deletes
    // sends no pack.
    let deletes = updates.iter().filter(|u| u.new == Oid::ZERO).count();
    if deletes > 0 {
        if !caps.iter().any(|c| c == "delete-refs") {
            return Err(Error::Protocol("the server does not accept ref deletions (no delete-refs)".into()));
        }
        want_caps.push("delete-refs");
    }
    let pack = if deletes == updates.len() { Vec::new() } else { pack };
    let mut body = Vec::new();
    for (i, u) in updates.iter().enumerate() {
        let line = if i == 0 {
            format!("{} {} {}\0{} {AGENT}\n", u.old, u.new, u.name, want_caps.join(" "))
        } else {
            format!("{} {} {}\n", u.old, u.new, u.name)
        };
        body.extend(pkt_str(&line));
    }
    body.extend_from_slice(FLUSH);
    body.extend(pack);
    let resp = post(t, remote, "git-receive-pack", body, false).await?;

    // With side-band the report-status pkt-lines are themselves wrapped in band 1.
    crate::receive::parse_report(&resp, sideband)
}

fn advertised_caps(adv: &[u8]) -> Result<Vec<String>> {
    let mut r = PktReader::new(adv);
    while let Some(p) = r.next_pkt()? {
        if let Pkt::Data(d) = p {
            if let Some(nul) = d.iter().position(|&b| b == 0) {
                return Ok(text(&d[nul + 1..]).split(' ').map(str::to_string).collect());
            }
        }
    }
    Ok(vec![])
}
