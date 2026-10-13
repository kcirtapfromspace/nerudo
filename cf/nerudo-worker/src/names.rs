//! A memo of the Hub's repo records, so that most requests by name do not ask the one Hub
//! object (cf/scale/hub.md, cf/scale/hub-next.md). It has two layers: a map in this isolate,
//! and the Cache API of this colo (`caches.default`), which all isolates of this Worker there
//! share. The keys are those of the Hub:
//!
//!   repo:{org}/{repo}   the repo with that name, or none
//!   trunk:{trunk}       the repo that names that trunk, or none
//!
//! It keeps only records. The trunk still decides who may read, and `hub::cached_view` shows
//! a repo's page only when the trunk says at that time that the repo is public.
//!
//! - Only reads use it. A write by name (land, push, a new fork) asks the Hub every time, so
//!   it can't go to a trunk that lost the name. The answer refreshes the memo.
//! - A public record lives TTL_MS from the Hub's answer, in both layers together. A private
//!   record and "none" live SHORT_TTL_MS, so that a repo made public or made new shows soon
//!   everywhere. A name change (hub.rs `put_repo`, `delete_repo`) removes the keys in this
//!   isolate and in this colo. Other isolates and colos keep the old record until it expires.
//! - An answer of the Hub goes into the colo after the response (lib.rs, `wait_until`), so
//!   a lookup does not wait for the Cache API.
//! - One request at a time asks for a key in each isolate. A read that finds that request in
//!   progress waits for its answer, at most WAIT_MS. Without this, a new isolate (after each
//!   deploy) asks once for each request, not once for each name, and above the rate of the
//!   Hub that load keeps the memo empty (cf/scale/hub.md, "Cold start").

use crate::hub::Repo;
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Duration;
use worker::{Cache, Date, Delay, Env, Headers, Method, Response, Result, Url};

/// How long a public record stays in the memo.
pub(crate) const TTL_MS: f64 = 30_000.0;
/// How long a private record and "no such name" stay: short, so that a repo made public or
/// made new shows soon everywhere (`hub::cached_view` gives 404 for them without a sign-in),
/// and long enough that a flood of one name does not reach the Hub.
pub(crate) const SHORT_TTL_MS: f64 = 3_000.0;
/// Records in the memo of an isolate, at most: about 3 MB.
const MAX_NAMES: usize = 10_000;
/// How long a read waits for another request that asks the Hub for the same key.
const WAIT_MS: u64 = 2_000;

thread_local! {
    static NAMES: RefCell<Memo<Option<Repo>>> = RefCell::new(Memo::new(MAX_NAMES));
    /// Answers of the Hub that go into the colo after the response (`flush`).
    static TO_PUT: RefCell<Vec<ToPut>> = const { RefCell::new(Vec::new()) };
}

struct ToPut {
    key: String,
    record: Option<Repo>,
    until: f64,
    /// The memo's generation when the lookup started.
    gen: u64,
}

/// A request by name that only reads, so it can use the memo: GET or HEAD, and a clone or
/// fetch (`git-upload-pack`). A push is a write, its ref list (`info/refs?service=
/// git-receive-pack`) too. `rest` is the path after the name, as `hub::forward` gets it.
pub(crate) fn is_read(method: &Method, rest: &str, url: &Url) -> bool {
    let push = rest == "git/git-receive-pack" || url.query_pairs().any(|(k, v)| k == "service" && v == "git-receive-pack");
    !push && (matches!(method, Method::Get | Method::Head) || rest == "git/git-upload-pack")
}

/// The trunk that `{org}/{repo}` names. A read (`read` true) can use the memo; a write asks
/// the Hub.
pub(crate) async fn trunk_of(env: &Env, org: &str, repo: &str, read: bool) -> Result<Option<String>> {
    if read {
        return Ok(by_name(env, org, repo).await?.map(|r| r.trunk));
    }
    let key = format!("repo:{org}/{repo}");
    let asking = Asking::begin(&key, now());
    let r = crate::hub::lookup(env, org, repo).await?;
    let now = now();
    NAMES.with(|m| m.borrow_mut().end(key, asking.gen, r.clone(), now, now + ttl(&r)));
    Ok(r.map(|r| r.trunk))
}

/// The repo named `{org}/{repo}`, public or not, for a read.
pub(crate) async fn by_name(env: &Env, org: &str, repo: &str) -> Result<Option<Repo>> {
    if !crate::hub::valid_org(org) || !crate::hub::valid_repo(repo) {
        return Ok(None);
    }
    record(env, &format!("repo:{org}/{repo}")).await
}

/// The repo that names `trunk`, public or not, for a read.
pub(crate) async fn by_trunk(env: &Env, trunk: &str) -> Result<Option<Repo>> {
    if !crate::hub::valid_trunk(trunk) {
        return Ok(None);
    }
    record(env, &format!("trunk:{trunk}")).await
}

/// The record at `key`: from this isolate, else from this colo, else from the Hub.
async fn record(env: &Env, key: &str) -> Result<Option<Repo>> {
    let (mut waited, mut step) = (0, 5);
    loop {
        match NAMES.with(|m| m.borrow_mut().look(key, now())) {
            Look::Hit(r) => return Ok(r),
            Look::Asking if waited < WAIT_MS => {
                Delay::from(Duration::from_millis(step)).await;
                waited += step;
                step = (step * 2).min(200);
            }
            _ => break,
        }
    }
    let asking = Asking::begin(key, now());
    if let Some((r, until)) = colo_get(env, key).await {
        NAMES.with(|m| m.borrow_mut().end(key.to_string(), asking.gen, r.clone(), now(), until));
        return Ok(r);
    }
    // The Hub keeps repo:{name} and trunk:{trunk}; it answers them at repo/{name} and trunk/{trunk}.
    let r: Option<Repo> = crate::hub::hub(env, Method::Get, &key.replacen(':', "/", 1), None).await?;
    let now = now();
    let until = now + ttl(&r);
    // A name change in this isolate during the lookup: the answer is too old for both layers.
    if NAMES.with(|m| m.borrow_mut().end(key.to_string(), asking.gen, r.clone(), now, until)) {
        TO_PUT.with(|p| p.borrow_mut().push(ToPut { key: key.to_string(), record: r.clone(), until, gen: asking.gen }));
    }
    Ok(r)
}

/// How long an answer of the Hub stays in the memo.
fn ttl(r: &Option<Repo>) -> f64 {
    match r {
        Some(r) if r.public => TTL_MS,
        _ => SHORT_TTL_MS,
    }
}

/// Answers of the Hub wait to go into the colo.
pub(crate) fn to_put() -> bool {
    TO_PUT.with(|p| !p.borrow().is_empty())
}

/// Put the waiting answers of the Hub into this colo. lib.rs runs this after the response
/// (`wait_until`), so a lookup does not wait for the Cache API. An answer older than a name
/// change in this isolate is dropped.
pub(crate) async fn flush(env: Env) {
    for p in TO_PUT.with(|p| std::mem::take(&mut *p.borrow_mut())) {
        if p.still_new(NAMES.with(|m| m.borrow().gen), now()) {
            colo_put(&env, &p.key, &p.record, p.until).await;
        }
    }
}

impl ToPut {
    /// No name changed in this isolate since the lookup started, and the answer has not
    /// expired.
    fn still_new(&self, gen: u64, now: f64) -> bool {
        gen == self.gen && now < self.until
    }
}

/// The keys changed (a repo made, moved, renamed, made public or private, or deleted): forget
/// them in this isolate and in this colo.
pub(crate) async fn forget(env: &Env, keys: &[String]) {
    NAMES.with(|m| {
        let mut m = m.borrow_mut();
        for k in keys {
            m.forget(k);
        }
    });
    for k in keys {
        if let Ok(url) = colo_url(env, k) {
            let _ = Cache::default().delete(url, false).await;
        }
    }
}

fn now() -> f64 {
    Date::now().as_millis() as f64
}

// ------------------------------------------------------------------- the colo's layer

/// The Cache API key of a record. The id of this Worker's Hub object keeps apart the records
/// of other Workers that can share the cache.
fn colo_url(env: &Env, key: &str) -> Result<String> {
    let hub = env.durable_object("HUB")?.id_from_name("hub")?.to_string();
    Ok(cache_url(&hub, key))
}

fn cache_url(hub: &str, key: &str) -> String {
    format!("https://hub-cache.nerudo.internal/{hub}/{}", key.replacen(':', "/", 1))
}

/// The record that this colo keeps for `key`, and when it expires. The expiry is in the entry
/// (`x-nerudo-until`), so a record lives TTL_MS from the Hub's answer in all layers. Any
/// error is a miss.
async fn colo_get(env: &Env, key: &str) -> Option<(Option<Repo>, f64)> {
    let mut resp = Cache::default().get(colo_url(env, key).ok()?, false).await.ok()??;
    let until: f64 = resp.headers().get("x-nerudo-until").ok()??.parse().ok()?;
    if now() >= until {
        return None;
    }
    Some((serde_json::from_str(&resp.text().await.ok()?).ok()?, until))
}

async fn colo_put(env: &Env, key: &str, r: &Option<Repo>, until: f64) {
    let put = async {
        let headers = Headers::new();
        headers.set("content-type", "application/json")?;
        headers.set("cache-control", &format!("max-age={}", max_age(until - now())))?;
        headers.set("x-nerudo-until", &format!("{until:.0}"))?;
        let resp = Response::ok(serde_json::to_string(r)?)?.with_headers(headers);
        Cache::default().put(colo_url(env, key)?, resp).await
    };
    let _ = put.await;
}

/// Whole seconds for Cache-Control, at least 1. The entry's own expiry decides.
fn max_age(ms: f64) -> u64 {
    (ms / 1000.0).ceil().max(1.0) as u64
}

// ------------------------------------------------------------------- the isolate's layer

/// This request asks the Hub for a key. Dropped when the answer comes, when the lookup fails,
/// or when the request goes away.
struct Asking {
    key: String,
    /// The memo's generation at the start: a name change during the lookup makes the answer
    /// too old to keep.
    gen: u64,
}

impl Asking {
    fn begin(key: &str, now: f64) -> Self {
        let gen = NAMES.with(|m| m.borrow_mut().begin(key, now));
        Self { key: key.to_string(), gen }
    }
}

impl Drop for Asking {
    fn drop(&mut self) {
        NAMES.with(|m| m.borrow_mut().done(&self.key));
    }
}

/// What the memo knows about a key now.
#[derive(Debug, PartialEq)]
enum Look<V> {
    Hit(V),
    /// No live entry, and a request in this isolate began to ask the Hub for it less than
    /// WAIT_MS ago. (A request that the runtime drops without its destructor must not hold
    /// the key for longer.)
    Asking,
    Miss,
}

/// Keys with their value and the time each one expires (ms), and the keys that requests ask
/// the Hub for. The clock is an argument, so the tests can move it.
struct Memo<V> {
    map: HashMap<String, (V, f64)>,
    /// Requests that ask the Hub, for each key, and when the last one began.
    asking: HashMap<String, (u32, f64)>,
    /// One more at each `forget`.
    gen: u64,
    max: usize,
}

impl<V: Clone> Memo<V> {
    fn new(max: usize) -> Self {
        Self { map: HashMap::new(), asking: HashMap::new(), gen: 0, max }
    }

    fn look(&mut self, key: &str, now: f64) -> Look<V> {
        match self.get(key, now) {
            Some(v) => Look::Hit(v),
            None if self.asking.get(key).is_some_and(|(_, since)| now - since < WAIT_MS as f64) => Look::Asking,
            None => Look::Miss,
        }
    }

    fn begin(&mut self, key: &str, now: f64) -> u64 {
        let a = self.asking.entry(key.to_string()).or_default();
        *a = (a.0 + 1, now);
        self.gen
    }

    fn done(&mut self, key: &str) {
        if let Some((n, _)) = self.asking.get_mut(key) {
            *n -= 1;
            if *n == 0 {
                self.asking.remove(key);
            }
        }
    }

    /// The answer to a lookup that started at generation `gen`, kept until `until`. False,
    /// and not kept, when a key changed after the lookup started.
    fn end(&mut self, key: String, gen: u64, v: V, now: f64, until: f64) -> bool {
        if gen != self.gen {
            return false;
        }
        self.put(key, v, now, until);
        true
    }

    fn get(&mut self, key: &str, now: f64) -> Option<V> {
        match self.map.get(key) {
            Some((v, until)) if now < *until => Some(v.clone()),
            Some(_) => {
                self.map.remove(key);
                None
            }
            None => None,
        }
    }

    /// When the memo is full, expired keys go first; when none has expired, all go.
    fn put(&mut self, key: String, v: V, now: f64, until: f64) {
        if self.map.len() >= self.max && !self.map.contains_key(&key) {
            self.map.retain(|_, (_, u)| now < *u);
            if self.map.len() >= self.max {
                self.map.clear();
            }
        }
        self.map.insert(key, (v, until));
    }

    fn forget(&mut self, key: &str) {
        self.map.remove(key);
        self.gen += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::{cache_url, is_read, max_age, ttl, Look, Memo, Repo, ToPut, SHORT_TTL_MS, TTL_MS};
    use worker::{Method, Url};

    fn read(method: Method, rest: &str, path_and_query: &str) -> bool {
        is_read(&method, rest, &Url::parse(&format!("https://w.example{path_and_query}")).unwrap())
    }

    fn memo() -> Memo<Option<String>> {
        Memo::new(10)
    }

    fn hit(t: &str) -> Look<Option<String>> {
        Look::Hit(Some(t.to_string()))
    }

    #[test]
    fn reads_use_the_memo_and_writes_do_not() {
        assert!(read(Method::Get, "stats", "/v1/repos/o/r/stats"));
        assert!(read(Method::Head, "log", "/v1/repos/o/r/log"));
        assert!(read(Method::Get, "git/info/refs", "/o/r.git/info/refs?service=git-upload-pack"));
        assert!(read(Method::Post, "git/git-upload-pack", "/o/r.git/git-upload-pack"));
        assert!(!read(Method::Post, "land", "/v1/repos/o/r/land"));
        assert!(!read(Method::Post, "forks", "/v1/repos/o/r/forks"));
        assert!(!read(Method::Get, "git/info/refs", "/o/r.git/info/refs?service=git-receive-pack"));
        assert!(!read(Method::Post, "git/git-receive-pack", "/o/r.git/git-receive-pack"));
        // The same through the /v1/repos alias.
        assert!(!read(Method::Get, "git/info/refs", "/v1/repos/o/r/git/info/refs?service=git-receive-pack"));
        assert!(!read(Method::Post, "git/git-receive-pack", "/v1/repos/o/r/git/git-receive-pack"));
    }

    #[test]
    fn a_record_lives_until_its_expiry() {
        let mut m = memo();
        assert_eq!(m.look("repo:o/r", 0.0), Look::Miss);
        m.put("repo:o/r".into(), Some("t1".into()), 1_000.0, 1_000.0 + TTL_MS);
        assert_eq!(m.look("repo:o/r", 1_000.0), hit("t1"));
        assert_eq!(m.look("repo:o/r", 30_999.0), hit("t1"));
        assert_eq!(m.look("repo:o/r", 31_000.0), Look::Miss);
        // The expired entry is gone, not only hidden.
        assert!(m.map.is_empty());
    }

    #[test]
    fn no_such_name_is_kept_for_a_short_time() {
        let mut m = memo();
        let gen = m.begin("repo:o/new", 0.0);
        assert!(m.end("repo:o/new".into(), gen, None, 0.0, SHORT_TTL_MS));
        m.done("repo:o/new");
        assert_eq!(m.look("repo:o/new", 2_999.0), Look::Hit(None));
        assert_eq!(m.look("repo:o/new", 3_000.0), Look::Miss);
        // A repo made in this isolate is found at once: put_repo forgets the name.
        m.put("repo:o/new".into(), None, 0.0, SHORT_TTL_MS);
        m.forget("repo:o/new");
        assert_eq!(m.look("repo:o/new", 1.0), Look::Miss);
    }

    #[test]
    fn put_again_moves_the_name_and_restarts_the_ttl() {
        let mut m = memo();
        m.put("repo:o/r".into(), Some("t1".into()), 0.0, TTL_MS);
        m.put("repo:o/r".into(), Some("t2".into()), 20_000.0, 20_000.0 + TTL_MS);
        assert_eq!(m.look("repo:o/r", 40_000.0), hit("t2"));
    }

    #[test]
    fn forget_removes_the_key_at_once() {
        let mut m = memo();
        m.put("repo:o/r".into(), Some("t1".into()), 0.0, TTL_MS);
        m.put("repo:o/s".into(), Some("t2".into()), 0.0, TTL_MS);
        m.forget("repo:o/r");
        assert_eq!(m.look("repo:o/r", 1.0), Look::Miss);
        assert_eq!(m.look("repo:o/s", 1.0), hit("t2"));
        m.forget("repo:o/unknown");
    }

    #[test]
    fn a_read_waits_while_another_request_asks_the_hub() {
        let mut m = memo();
        assert_eq!(m.look("repo:o/r", 0.0), Look::Miss);
        let gen = m.begin("repo:o/r", 0.0);
        assert_eq!(m.look("repo:o/r", 1.0), Look::Asking);
        // A second asker (a write, or a read that waited too long) and the first one end.
        let gen2 = m.begin("repo:o/r", 0.0);
        assert!(m.end("repo:o/r".into(), gen, Some("t1".into()), 2.0, 2.0 + TTL_MS));
        m.done("repo:o/r");
        assert_eq!(m.look("repo:o/r", 3.0), hit("t1"));
        assert!(m.end("repo:o/r".into(), gen2, Some("t1".into()), 4.0, 4.0 + TTL_MS));
        m.done("repo:o/r");
        assert!(m.asking.is_empty());
        // An asker that never ends holds the key for WAIT_MS only.
        m.begin("repo:o/x", 10.0);
        assert_eq!(m.look("repo:o/x", 2_009.0), Look::Asking);
        assert_eq!(m.look("repo:o/x", 2_010.0), Look::Miss);
        // A lookup that failed: nobody asks any more, so the next read asks.
        m.begin("repo:o/s", 0.0);
        m.done("repo:o/s");
        assert_eq!(m.look("repo:o/s", 5.0), Look::Miss);
    }

    #[test]
    fn an_answer_older_than_a_name_change_is_not_kept() {
        let mut m = memo();
        let gen = m.begin("repo:o/r", 0.0);
        // put_repo moves the name while the lookup is in flight. The answer is not kept, here
        // or in the colo (record() puts it there only when `end` keeps it).
        m.forget("repo:o/r");
        assert!(!m.end("repo:o/r".into(), gen, Some("old-trunk".into()), 1.0, 1.0 + TTL_MS));
        m.done("repo:o/r");
        assert_eq!(m.look("repo:o/r", 2.0), Look::Miss);
        // The same for "no such name": a repo made during the lookup is not hidden.
        let gen = m.begin("repo:o/new", 0.0);
        m.forget("repo:o/new");
        assert!(!m.end("repo:o/new".into(), gen, None, 1.0, 1.0 + SHORT_TTL_MS));
        m.done("repo:o/new");
        assert_eq!(m.look("repo:o/new", 2.0), Look::Miss);
        // Answers that started after the change are kept.
        let gen = m.begin("repo:o/t", 3.0);
        assert!(m.end("repo:o/t".into(), gen, Some("t2".into()), 3.0, 3.0 + TTL_MS));
        m.done("repo:o/t");
        assert_eq!(m.look("repo:o/t", 4.0), hit("t2"));
    }

    #[test]
    fn a_full_memo_drops_expired_keys_first_then_all() {
        let mut m: Memo<Option<String>> = Memo::new(2);
        m.put("repo:o/a".into(), Some("ta".into()), 0.0, 10.0);
        m.put("repo:o/b".into(), Some("tb".into()), 5_000.0, TTL_MS + 5_000.0);
        // At 12 s, o/a has expired and o/b has not. o/a goes, o/b stays.
        m.put("repo:o/c".into(), Some("tc".into()), 12_000.0, 12_000.0 + TTL_MS);
        assert_eq!(m.map.len(), 2);
        assert_eq!(m.look("repo:o/b", 12_000.0), hit("tb"));
        assert_eq!(m.look("repo:o/c", 12_000.0), hit("tc"));
        // Nothing expired: all go, and the new key stays.
        m.put("repo:o/d".into(), Some("td".into()), 13_000.0, 13_000.0 + TTL_MS);
        assert_eq!(m.map.len(), 1);
        assert_eq!(m.look("repo:o/d", 13_000.0), hit("td"));
        // A key already in a full memo is replaced in place.
        m.put("repo:o/e".into(), Some("te".into()), 13_000.0, 13_000.0 + TTL_MS);
        m.put("repo:o/e".into(), Some("te2".into()), 14_000.0, 14_000.0 + TTL_MS);
        assert_eq!(m.map.len(), 2);
    }

    #[test]
    fn an_answer_goes_into_the_colo_only_if_no_name_changed_since_its_lookup() {
        let p = ToPut { key: "repo:o/r".into(), record: None, until: SHORT_TTL_MS, gen: 4 };
        assert!(p.still_new(4, 1.0));
        // A name change in this isolate after the lookup started (forget made gen 5).
        assert!(!p.still_new(5, 1.0));
        // The answer expired before the flush.
        assert!(!p.still_new(4, SHORT_TTL_MS));
    }

    #[test]
    fn a_public_record_lives_long_and_a_private_one_or_none_short() {
        let public = Repo { org: "o".into(), repo: "r".into(), trunk: "t".into(), public: true, ..Default::default() };
        assert_eq!(ttl(&Some(public.clone())), TTL_MS);
        // A repo made public must show soon in other isolates: the page gives 404 from a
        // private record.
        assert_eq!(ttl(&Some(Repo { public: false, ..public })), SHORT_TTL_MS);
        assert_eq!(ttl(&None), SHORT_TTL_MS);
    }

    #[test]
    fn colo_keys_are_apart_for_each_hub_and_for_each_kind() {
        assert_eq!(cache_url("h1", "repo:o/r"), "https://hub-cache.nerudo.internal/h1/repo/o/r");
        assert_eq!(cache_url("h1", "trunk:t-1"), "https://hub-cache.nerudo.internal/h1/trunk/t-1");
        assert_ne!(cache_url("h1", "repo:o/r"), cache_url("h2", "repo:o/r"));
        // The Cache API keeps whole seconds; the entry's own expiry decides.
        assert_eq!(max_age(SHORT_TTL_MS), 3);
        assert_eq!(max_age(29_001.0), 30);
        assert_eq!(max_age(-5.0), 1);
    }
}
