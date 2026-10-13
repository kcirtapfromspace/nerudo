//! The web UI: HTML that the Trunk Durable Object renders, with no JavaScript. There is no
//! pull request anywhere. The board shows changes that move from the queue through
//! batches onto trunk. A landing page shows one change on trunk: why, the diff, the
//! receipt, the seals and its conversation. The inbox shows what waits for a human.
//!
//! Every page needs a link. A share link (`?k=`, an HMAC of the trunk name keyed with the
//! admin token; `nerudo share`) lets anybody read. A personal link (`?u=name&k=`, an HMAC
//! of the trunk and the name; `nerudo invite`) is for a person in nerudo.toml: with it,
//! that person can comment, react and request reviews. Actions are plain HTML forms. A
//! public repo (repo.rs) reads with no link.

use super::social::{normalize_emoji, with_emoji, Directory, Who};
use super::*;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

/// Landings the board shows.
const LOG_ON_BOARD: u64 = 40;
/// A diff larger than this (old lines times new lines) shows as removed and added.
/// Most line edits a diff looks for (Myers); past that, the changed part shows as replaced.
const MAX_DIFF_EDITS: usize = 1_500;
pub(super) const MAX_FILE_BYTES: usize = 256 * 1024;
/// The reactions offered as buttons; any other emoji or :shortcode: can be typed.
const QUICK: [&str; 8] = ["👍", "🎉", "🚀", "👀", "❤️", "😄", "😕", "👎"];

type Flat = BTreeMap<String, FileEntry>;

struct Ctx {
    trunk: String,
    k: String,
    /// The person, with a personal link. None: a read-only share link.
    user: Option<String>,
    dir: Directory,
    inbox: usize,
}

impl Ctx {
    fn href(&self, path: &str) -> String {
        let p = if path.is_empty() { String::new() } else { format!("/{path}") };
        let u = self.user.as_ref().map(|u| format!("&u={u}")).unwrap_or_default();
        format!("/t/{}{p}?k={}{u}", self.trunk, self.k)
    }
}

/// The UI's copy of trunk, borrowed from the object for one request (`ui_store`). It goes back
/// when dropped, also when the request stops early, so the objects a page read stay.
pub(super) struct UiStore<'a> {
    store: Option<Store>,
    mem: &'a RefCell<Mem>,
}

impl std::ops::Deref for UiStore<'_> {
    type Target = Store;
    fn deref(&self) -> &Store {
        self.store.as_ref().expect("present until drop")
    }
}

impl std::ops::DerefMut for UiStore<'_> {
    fn deref_mut(&mut self) -> &mut Store {
        self.store.as_mut().expect("present until drop")
    }
}

impl<'a> UiStore<'a> {
    pub(super) fn taken(mem: &'a RefCell<Mem>, store: Store) -> Self {
        Self { store: Some(store), mem }
    }
}

impl Drop for UiStore<'_> {
    fn drop(&mut self) {
        if let Some(s) = self.store.take() {
            let mut mem = self.mem.borrow_mut();
            mem.ui = s;
            mem.ui_out = false;
            for waiter in mem.ui_waiters.drain(..) {
                let _ = waiter.send(());
            }
        }
    }
}

const UI_WAIT_MS: f64 = 10_000.0;

/// On a lazy trunk, the UI's copy starts again empty above this size.
const UI_LAZY_MAX_BYTES: usize = 64 << 20;

impl Trunk {
    /// Background index reads never fetch the entire repository before borrowing.
    pub(super) async fn index_store(&self) -> Result<UiStore<'_>> {
        Ok(UiStore::taken(&self.mem, self.take_ui().await?))
    }

    pub(super) async fn ui(&self, req: &mut Request, rest: &[&str]) -> Result<Response> {
        let url = req.url()?;
        let q: BTreeMap<String, String> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        let Some(cfg) = self.get::<Config>("cfg").await? else {
            return html(404, &bare("Unknown trunk", "<p>There is no trunk with this name.</p>"));
        };
        let dir = self.directory().await?;
        // A key that is not one of ours opens nothing, and goes into no link on the page.
        let given = q.get("k").map(String::as_str).filter(|k| valid_key(k)).unwrap_or_default();
        let mut user = q.get("u").cloned();
        let person = match &user {
            Some(u) if dir.people.contains_key(u) => self.person_ok(&cfg.trunk, u, given).await?,
            _ => false,
        };
        let ok = match &user {
            _ if person => true,
            // A signed-in user who is not a person here reads a public repo as anyone does.
            Some(u) if same(given.as_bytes(), account_key(&self.env, u)?.as_bytes()) && self.is_public().await? => {
                user = None;
                true
            }
            Some(_) => false,
            None => same(given.as_bytes(), share_key(&self.env, &cfg.trunk)?.as_bytes()) || self.is_public().await?,
        };
        if !ok {
            return html(
                403,
                &bare(
                    "Link needed",
                    "<p>Open this trunk with its share link (<code>nerudo share --trunk NAME</code>) or with your personal link (<code>nerudo invite --trunk NAME --user YOU</code>).</p>",
                ),
            );
        }
        let ctx = Ctx { trunk: cfg.trunk.clone(), k: given.to_string(), user, dir, inbox: self.parked_keys().await?.len() };
        if req.method() == Method::Post {
            return match rest {
                ["act", what] => self.act(req, &ctx, what).await,
                _ => not_found(&ctx),
            };
        }
        match rest {
            [] | [""] => self.board(&ctx, &cfg).await,
            ["inbox"] => self.inbox(&ctx, &cfg).await,
            ["landing", seq] => match seq.parse() {
                Ok(n) => self.landing_page(&ctx, &cfg, n).await,
                Err(_) => not_found(&ctx),
            },
            ["change", cid] => self.change_page(&ctx, &cfg, cid, q.get("rev").and_then(|r| r.parse().ok())).await,
            ["tree", path @ ..] => self.tree(&ctx, &cfg, &decode(&path.join("/")), q.get("at").and_then(|a| a.parse().ok())).await,
            ["branches"] => self.branches_page(&ctx, &cfg).await,
            ["history"] => self.history(&ctx, &cfg, q.get("before").and_then(|b| b.parse().ok()), q.get("path").map(String::as_str)).await,
            _ => not_found(&ctx),
        }
    }

    /// `POST .../share` (admin): the read-only link to this trunk's pages (`url`) and, when
    /// the Worker knows the web app's address (`NERUDO_WEB`), to the app (`app`; by name
    /// for a public repo on the Hub, see repo.rs).
    pub(super) async fn share(&self, req: &Request) -> Result<Response> {
        let Some(cfg) = self.get::<Config>("cfg").await? else {
            return bad(404, "unknown trunk: init it first");
        };
        let origin = req.url()?.origin().ascii_serialization();
        let key = share_key(&self.env, &cfg.trunk)?;
        let app = self.web_link(&cfg.trunk, "", &format!("k={key}")).await;
        Response::from_json(&serde_json::json!({ "url": format!("{origin}/t/{}?k={key}", cfg.trunk), "app": app }))
    }

    /// `POST .../invite {user}` (admin): a person's own link. The person must be an
    /// approver in nerudo.toml on trunk.
    pub(super) async fn invite(&self, url: Url, body: Value) -> Result<Response> {
        let Some(cfg) = self.get::<Config>("cfg").await? else {
            return bad(404, "unknown trunk: init it first");
        };
        let user = body["user"].as_str().unwrap_or_default();
        if !self.directory().await?.people.contains_key(user) {
            return bad(400, format!("{user:?} is not an approver in nerudo.toml on trunk"));
        }
        let key = user_key(&self.env, &cfg.trunk, user)?;
        let origin = url.origin().ascii_serialization();
        let app = self.web_link(&cfg.trunk, "", &format!("k={key}&u={user}")).await;
        Response::from_json(&serde_json::json!({ "url": format!("{origin}/t/{}?k={key}&u={user}", cfg.trunk), "app": app }))
    }

    /// A form post from a person's page: comment, review or react. It answers 303 back to
    /// the page, so a reload does not post again.
    async fn act(&self, req: &mut Request, ctx: &Ctx, what: &str) -> Result<Response> {
        let Some(user) = ctx.user.clone() else {
            return html(403, &bare("Personal link needed", "<p>Only a personal link can comment, react or request reviews.</p>"));
        };
        let form = req.form_data().await?;
        let field = |k: &str| match form.get(k) {
            Some(FormEntry::Field(s)) => s,
            _ => String::new(),
        };
        if what == "fork" {
            return self.fork_for_person(ctx, &user).await;
        }
        if what == "cleanup" {
            let cfg = self.cfg().await?;
            let agent = field("agent");
            let back = ctx.href("branches");
            return match self.cleanup(&cfg, &agent).await? {
                Ok(_) => {
                    let headers = Headers::new();
                    headers.set("location", &back)?;
                    Ok(Response::empty()?.with_status(303).with_headers(headers))
                }
                Err(e) => page(ctx, "Not done", "branches", &format!(r#"<h1>Not done</h1><p>{}</p><p><a href="{}">Back</a></p>"#, esc(&e), esc(&back)), None),
            };
        }
        let cid = field("change");
        let result: std::result::Result<(), String> = match what {
            "comment" => self.add_comment(&cid, &user, false, &field("body")).await?.map(|_| ()),
            "review" => self.add_review_request(&cid, &field("reviewer"), &user).await?.map(|_| ()),
            "react" => {
                let typed = field("emoji");
                let typed = if typed.is_empty() { field("other") } else { typed };
                match normalize_emoji(&typed) {
                    Some(e) if self.get::<u32>(&format!("latest:{cid}")).await?.is_some() => match self.reaction_target(&cid, Some(&field("comment"))).await? {
                        Ok(target) => {
                            self.toggle_reaction(&target, &e, &user).await?;
                            Ok(())
                        }
                        Err(e) => Err(e),
                    },
                    Some(_) => Err(format!("unknown change {cid}")),
                    None => Err("give one emoji, or a :shortcode: like :rocket:".into()),
                }
            }
            _ => Err(format!("unknown action {what}")),
        };
        let back = field("back");
        let back = if back.starts_with(&format!("/t/{}/", ctx.trunk)) || back.starts_with(&format!("/t/{}?", ctx.trunk)) {
            back
        } else {
            ctx.href(&format!("change/{cid}"))
        };
        match result {
            Ok(()) => {
                let headers = Headers::new();
                headers.set("location", &back)?;
                headers.set("cache-control", "no-store")?;
                Ok(Response::empty()?.with_status(303).with_headers(headers))
            }
            Err(e) => page(ctx, "Not done", "", &format!(r#"<h1>Not done</h1><p>{}</p><p><a href="{}">Back</a></p>"#, esc(&e), esc(&back)), None),
        }
    }

    /// The UI's copy of trunk, up to `head`. It fetches only what is new. On a trunk in lazy
    /// mode (trunk/objects.rs) it fetches nothing: a page reads the objects it needs one at a
    /// time (see `fetch` in trunk.rs). The copy goes back to the object when the guard drops.
    pub(super) async fn ui_store(&self, cfg: &Config, head: Option<Oid>) -> Result<UiStore<'_>> {
        let mut store = self.index_store().await?;
        let lazy = self.get::<bool>("lazy_trees").await?.unwrap_or(false);
        if lazy && store.bytes() > UI_LAZY_MAX_BYTES {
            *store = Store::new();
        }
        if let Some(h) = head.filter(|h| !lazy && !store.contains(h)) {
            let haves: Vec<Oid> = self.mem.borrow().ui_head.into_iter().filter(|o| store.contains(o)).collect();
            let t = FetchTransport::default();
            let remote = Remote::new(&cfg.remote, Some(cfg.authorization.clone()));
            fetch(&t, &remote, &[h], &haves, &mut store).await.map_err(g)?;
            self.mem.borrow_mut().ui_head = Some(h);
        }
        Ok(store)
    }

    /// A single cache serves both pages and background indexing. A second reader waits
    /// for the owner rather than fetching another complete copy of a large repository.
    pub(super) async fn take_ui(&self) -> Result<Store> {
        let deadline = now() + UI_WAIT_MS;
        while self.mem.borrow().ui_out {
            let left = deadline - now();
            if left <= 0.0 {
                return Err(Error::RustError("busy: another request has the repository cache; try again in a few seconds".into()));
            }
            let (tx, rx) = oneshot::channel();
            self.mem.borrow_mut().ui_waiters.push(tx);
            let _ = select(rx, Delay::from(Duration::from_millis(left as u64))).await;
        }
        let mut mem = self.mem.borrow_mut();
        mem.ui_out = true;
        Ok(std::mem::take(&mut mem.ui))
    }

    pub(super) fn ui_store_back(&self, store: UiStore<'_>) {
        drop(store);
    }

    pub(super) async fn rec(&self, cid: &str, rev: u32) -> Result<Option<ChangeRec>> {
        self.get(&format!("c:{cid}:{rev}")).await
    }

    pub(super) async fn latest_rec(&self, cid: &str) -> Result<Option<ChangeRec>> {
        match self.get::<u32>(&format!("latest:{cid}")).await? {
            Some(rev) => self.rec(cid, rev).await,
            None => Ok(None),
        }
    }

    /// Changes that have not landed yet: queued, then parked for a human.
    pub(super) async fn pending(&self) -> Result<Vec<ChangeRec>> {
        let mut keys: Vec<String> = self.get("q").await?.unwrap_or_default();
        for k in self.parked_keys().await? {
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
        let mut out = vec![];
        for k in keys {
            if let Some(r) = self.get::<ChangeRec>(&format!("c:{k}")).await? {
                out.push(r);
            }
        }
        Ok(out)
    }

    /// Comment counts per change, from one listing.
    pub(super) async fn comment_counts(&self) -> Result<BTreeMap<String, usize>> {
        let mut out: BTreeMap<String, usize> = BTreeMap::new();
        for (k, _) in self.list_prefix("cm:").await? {
            if let Some(cid) = k["cm:".len()..].split(':').next() {
                *out.entry(cid.to_string()).or_default() += 1;
            }
        }
        Ok(out)
    }

    /// The change as the agent made it, against its base: fetched from its fork if needed.
    async fn change_diff(&self, store: &mut Store, rec: &ChangeRec, head: Option<Oid>) -> Result<String> {
        if !store.contains(&rec.req.commit) {
            if let Some(fork) = self.get::<Fork>(&format!("f:{}", rec.req.fork)).await? {
                let t = FetchTransport::default();
                let remote = Remote::new(&fork.remote, Some(fork.authorization.clone()));
                let haves: Vec<Oid> = head.into_iter().chain([rec.req.base]).collect();
                let _ = fetch_with(&t, &remote, &[rec.req.commit], &haves, FetchOpts { any_haves: true, ..FetchOpts::default() }, store).await;
            }
        }
        Ok(match (store.commit(&rec.req.commit), store.commit(&rec.req.base)) {
            (Ok(c), Ok(base)) => {
                let mut s = String::new();
                let why = c.body();
                if !why.is_empty() {
                    let _ = write!(s, r#"<div class="why">{}</div>"#, esc(&with_emoji(why)));
                }
                let (before, after) = super::views::changed_files(store, Some(&base.tree), &c.tree).map_err(g)?;
                s.push_str(&diff_trees(store, &before, &after));
                s
            }
            _ => r#"<p class="mute">The diff is not available (the fork's token may have run out).</p>"#.into(),
        })
    }

    // ------------------------------------------------------------------ pages

    async fn board(&self, ctx: &Ctx, cfg: &Config) -> Result<Response> {
        let meta: Meta = self.get("meta").await?.unwrap_or_default();
        let stats: Stats = self.get("stats").await?.unwrap_or_default();
        let store = self.ui_store(cfg, meta.head).await?;
        let pending = self.pending().await?;
        let talk = self.comment_counts().await?;
        let talk_badge = |cid: &str| talk.get(cid).map(|n| format!(r#" <span class="mute">💬 {n}</span>"#)).unwrap_or_default();
        let mut b = String::new();

        let per = if stats.landed > 0 { stats.ci_runs as f64 / stats.landed as f64 } else { 0.0 };
        let _ = write!(
            b,
            r#"<section class="numbers">{}{}{}{}{}</section>"#,
            num(&stats.landed.to_string(), "changes landed"),
            num(&stats.batches.to_string(), "batches"),
            num(&format!("{per:.2}"), "CI runs per change"),
            num(&(stats.ejected_conflict + stats.ejected_ci).to_string(), &format!("ejected ({} by CI)", stats.ejected_ci)),
            num(&ctx.inbox.to_string(), "waiting for a human"),
        );

        b.push_str("<h2>In flight</h2>");
        if pending.is_empty() {
            b.push_str(r#"<p class="mute">Nothing is waiting. Agents land through <code>nerudo mcp</code> or the API.</p>"#);
        } else {
            b.push_str(r#"<div class="scroll"><table><tr><th>Change</th><th>Agent</th><th>State</th><th>Touches</th><th>Why it waits</th></tr>"#);
            for r in &pending {
                let _ = write!(
                    b,
                    r#"<tr><td><a href="{}">{} <span class="mute">rev {}</span></a>{}</td><td>{}</td><td>{}</td><td class="mono">{}</td><td>{}</td></tr>"#,
                    ctx.href(&format!("change/{}", r.req.change_id)),
                    esc(&r.req.change_id),
                    r.req.rev,
                    talk_badge(&r.req.change_id),
                    esc(agent_of(&ctx.trunk, &r.req.fork)),
                    state_pill(&r.state),
                    esc(&r.footprint.join(", ")),
                    waits_because(r),
                );
            }
            b.push_str("</table></div>");
        }

        b.push_str("<h2>Trunk</h2>");
        let from = meta.seq.saturating_sub(LOG_ON_BOARD) + 1;
        let mut batches: BTreeMap<u64, Vec<(LogEntry, Option<ChangeRec>)>> = BTreeMap::new();
        for seq in from..=meta.seq {
            if let Some(e) = self.get::<LogEntry>(&format!("log:{seq:012}")).await? {
                let rec = self.rec(&e.change_id, e.rev).await?;
                batches.entry(e.batch).or_default().push((e, rec));
            }
        }
        if batches.is_empty() {
            let _ = write!(b, r#"<p class="mute">Nothing has landed yet. Trunk is at <code>{}</code>.</p>"#, short(meta.head));
        }
        b.push_str(r#"<div class="trunk">"#);
        for (batch, entries) in batches.iter().rev() {
            let runs = entries.first().and_then(|(_, r)| r.as_ref()).map(|r| r.ci_runs).unwrap_or(1).max(1);
            let when = entries.first().and_then(|(_, r)| r.as_ref()).and_then(|r| r.landed_ms).map(clock).unwrap_or_default();
            let _ = write!(
                b,
                r#"<div class="batch"><div class="meta">batch {batch} · {} · {} · {when}</div>"#,
                plural(entries.len() as u64, "change"),
                plural(runs, "CI run")
            );
            for (e, rec) in entries.iter().rev() {
                let subject = Oid::from_hex(&e.commit_out).ok().and_then(|o| store.commit(&o).ok()).map(|c| c.message.lines().next().unwrap_or("").to_string()).unwrap_or_default();
                // Many agents start the subject with the change id; it is already shown.
                let subject = subject.strip_prefix(&format!("{}: ", e.change_id)).unwrap_or(&subject).to_string();
                let agent = rec.as_ref().map(|r| agent_of(&ctx.trunk, &r.req.fork).to_string()).unwrap_or_default();
                let sealed = rec.as_ref().filter(|r| !r.sealed_by.is_empty()).map(|r| format!(r#" <span class="pill seal">sealed by {}</span>"#, esc(&r.sealed_by.join(", ")))).unwrap_or_default();
                let numbered = rec.as_ref().filter(|r| !r.allocated.is_empty()).map(|r| format!(r#" <span class="pill">numbered {}</span>"#, esc(&r.allocated.join(", ")))).unwrap_or_default();
                let _ = write!(
                    b,
                    r#"<div class="land"><a href="{}"><span class="mono mute">{}</span> <b>{}</b> {}</a> <span class="mute">by {}</span>{}{sealed}{numbered}</div>"#,
                    ctx.href(&format!("landing/{}", e.seq)),
                    &e.commit_out[..7.min(e.commit_out.len())],
                    esc(&e.change_id),
                    esc(&with_emoji(&subject)),
                    esc(&agent),
                    talk_badge(&e.change_id),
                );
            }
            b.push_str("</div>");
        }
        b.push_str("</div>");

        let mut ejected: Vec<ChangeRec> = self
            .list_prefix("c:")
            .await?
            .into_iter()
            .filter_map(|(_, v)| serde_json::from_str::<ChangeRec>(&v).ok())
            .filter(|r| r.state == Phase::Ejected)
            .collect();
        if !ejected.is_empty() {
            ejected.sort_by_key(|r| std::cmp::Reverse(r.submitted_ms));
            b.push_str(r#"<h2>Ejected</h2><div class="scroll"><table><tr><th>Change</th><th>Agent</th><th>Reason</th></tr>"#);
            for r in ejected.iter().take(8) {
                let _ = write!(
                    b,
                    r#"<tr><td><a href="{}&rev={}">{} <span class="mute">rev {}</span></a></td><td>{}</td><td>{}</td></tr>"#,
                    ctx.href(&format!("change/{}", r.req.change_id)),
                    r.req.rev,
                    esc(&r.req.change_id),
                    r.req.rev,
                    esc(agent_of(&ctx.trunk, &r.req.fork)),
                    reason_short(r.reason.as_ref()),
                );
            }
            b.push_str("</table></div>");
        }
        self.ui_store_back(store);
        // Reload often while changes land; slowly when only humans can move things on.
        let refresh = if pending.iter().any(|r| r.state == Phase::Queued) {
            Some(2)
        } else if pending.is_empty() {
            None
        } else {
            Some(5)
        };
        page(ctx, "Board", "board", &b, refresh)
    }

    async fn landing_page(&self, ctx: &Ctx, cfg: &Config, seq: u64) -> Result<Response> {
        let Some(e) = self.get::<LogEntry>(&format!("log:{seq:012}")).await? else {
            return not_found(ctx);
        };
        let meta: Meta = self.get("meta").await?.unwrap_or_default();
        let rec = self.rec(&e.change_id, e.rev).await?;
        let store = self.ui_store(cfg, meta.head).await?;
        let out = Oid::from_hex(&e.commit_out).map_err(g)?;
        let Ok(commit) = store.commit(&out) else {
            self.ui_store_back(store);
            return not_found(ctx);
        };
        let mut b = String::new();
        let subject = commit.message.lines().next().unwrap_or("");
        let _ = write!(b, r#"<p class="mute">landing {seq} · batch {} · {}</p><h1>{}</h1>"#, e.batch, clock(e.landed_at_ms), esc(&with_emoji(subject)));
        let why = commit.body();
        if why.is_empty() {
            b.push_str(r#"<p class="mute">The agent wrote no why for this change.</p>"#);
        } else {
            let _ = write!(b, r#"<h2>Why</h2><div class="why">{}</div>"#, rich(why, &ctx.dir));
        }

        b.push_str(r#"<h2>Receipt</h2><div class="card"><table class="kv">"#);
        let row = |b: &mut String, k: &str, v: String| {
            let _ = write!(b, "<tr><th>{k}</th><td>{v}</td></tr>");
        };
        row(&mut b, "Change", format!(r#"<a href="{}"><code>{}</code></a> rev {}"#, ctx.href(&format!("change/{}", e.change_id)), esc(&e.change_id), e.rev));
        row(&mut b, "Agent", format!("{} &lt;{}&gt;", esc(&commit.author.name), esc(&commit.author.email)));
        row(&mut b, "On trunk", format!("<code>{}</code> (seq {seq})", esc(&e.commit_out)));
        row(&mut b, "Agent's commit", format!("<code>{}</code>", esc(&e.commit_in)));
        if let Some(r) = &rec {
            row(&mut b, "CI", format!("{} in batch {}", plural(r.ci_runs.max(1), "CI run"), e.batch));
            if !r.allocated.is_empty() {
                row(&mut b, "Numbered at landing", esc(&r.allocated.join(", ")));
            }
            if !r.sealed_by.is_empty() {
                row(&mut b, "Sealed by", esc(&r.sealed_by.join(", ")));
            }
        }
        for (k, label) in [("Seal-Reason", "Seal reason"), ("Sealed-By", "Seal"), ("Change-Commit", "Sealed commit"), ("Break-Glass", "Break-glass"), ("Context", "Agent context")] {
            for v in commit.trailers(k) {
                row(&mut b, label, format!(r#"<span class="mono small">{}</span>"#, esc(&v)));
            }
        }
        b.push_str("</table></div>");

        b.push_str(&self.conversation(ctx, &e.change_id, &ctx.href(&format!("landing/{seq}"))).await?);

        let parent = commit.parents.first().map(|p| store.commit(p).map(|p| p.tree)).transpose().map_err(g)?;
        let (before, after) = super::views::changed_files(&store, parent.as_ref(), &commit.tree).map_err(g)?;
        b.push_str("<h2>Diff on trunk</h2>");
        b.push_str(&diff_trees(&store, &before, &after));
        let prev = if seq > 1 { format!(r#"<a href="{}">← landing {}</a>"#, ctx.href(&format!("landing/{}", seq - 1)), seq - 1) } else { String::new() };
        let next = if seq < meta.seq { format!(r#"<a href="{}">landing {} →</a>"#, ctx.href(&format!("landing/{}", seq + 1)), seq + 1) } else { String::new() };
        let _ = write!(b, r#"<p class="pager">{prev} {next}</p>"#);
        self.ui_store_back(store);
        page(ctx, &format!("Landing {seq}"), "board", &b, None)
    }

    /// Reactions, review requests and comments on a change, with the forms to add them.
    async fn conversation(&self, ctx: &Ctx, cid: &str, back: &str) -> Result<String> {
        let mut b = String::from("<h2>Conversation</h2>");
        b.push_str(&self.reaction_bar(ctx, cid, None, back).await?);

        let latest = self.latest_rec(cid).await?;
        let open = latest.as_ref().is_some_and(|r| matches!(r.state, Phase::Queued | Phase::AwaitingSeal | Phase::AwaitingAllowance));
        let sealed_by: Vec<String> = latest
            .as_ref()
            .and_then(|r| r.reason.as_ref())
            .and_then(|r| r["quorum"].as_array().cloned())
            .into_iter()
            .flatten()
            .flat_map(|t| t["have"].as_array().cloned().unwrap_or_default())
            .filter_map(|v| v.as_str().map(str::to_string))
            .chain(latest.as_ref().map(|r| r.sealed_by.clone()).unwrap_or_default())
            .collect();
        let reviews = self.reviews(cid).await?;
        if !reviews.is_empty() {
            b.push_str(r#"<div class="reviews">"#);
            for r in &reviews {
                let status = if sealed_by.contains(&r.reviewer) {
                    r#"<span class="pill landed">sealed</span>"#
                } else if open {
                    r#"<span class="pill parked">review requested</span>"#
                } else {
                    r#"<span class="pill queued">closed</span>"#
                };
                let _ = write!(b, r#"<div>{status} {} <span class="mute">asked by {} · {}</span></div>"#, mention_html(&r.reviewer, &ctx.dir), esc(&r.by), clock(r.at_ms));
            }
            b.push_str("</div>");
        }

        for c in self.comments(cid).await? {
            let badge = if c.agent { r#" <span class="pill">agent</span>"# } else { "" };
            let _ = write!(
                b,
                r#"<div class="comment"><div class="meta"><b>{}</b>{badge} · {}</div><div class="body">{}</div>{}</div>"#,
                esc(&c.author),
                clock(c.at_ms),
                rich(&c.body, &ctx.dir),
                self.reaction_bar(ctx, cid, Some(&c.id), back).await?
            );
        }

        match &ctx.user {
            Some(user) => {
                let _ = write!(
                    b,
                    r#"<form class="say" method="post" action="{}"><input type="hidden" name="change" value="{}"><input type="hidden" name="back" value="{}">
<textarea name="body" rows="3" maxlength="4000" placeholder="Comment as {}. Mention @person, @team or an agent; :shortcodes: work."></textarea><button>Comment</button></form>"#,
                    ctx.href("act/comment"),
                    esc(cid),
                    esc(back),
                    esc(user)
                );
                let mut options = String::new();
                for p in ctx.dir.people.keys() {
                    let _ = write!(options, r#"<option value="{0}">{0}</option>"#, esc(p));
                }
                for t in ctx.dir.teams() {
                    let _ = write!(options, r#"<option value="@{0}">@{0} (team)</option>"#, esc(&t));
                }
                for a in &ctx.dir.agents {
                    let _ = write!(options, r#"<option value="{0}">{0} (agent)</option>"#, esc(a));
                }
                let _ = write!(
                    b,
                    r#"<form class="ask" method="post" action="{}"><input type="hidden" name="change" value="{}"><input type="hidden" name="back" value="{}">Request a review from <select name="reviewer">{options}</select> <button>Request review</button></form>"#,
                    ctx.href("act/review"),
                    esc(cid),
                    esc(back)
                );
            }
            None => b.push_str(r#"<p class="mute small">Open your personal link to comment, react or request a review.</p>"#),
        }
        Ok(b)
    }

    /// The reactions on a change (comment: None) or on one comment, with the buttons to
    /// toggle them for a person.
    async fn reaction_bar(&self, ctx: &Ctx, cid: &str, comment: Option<&str>, back: &str) -> Result<String> {
        let target = match comment {
            Some(id) => format!("m.{id}"),
            None => format!("c.{cid}"),
        };
        let have = self.reactions(&target).await?;
        let me = ctx.user.as_deref().unwrap_or("");
        let mut b = String::from(r#"<div class="rx">"#);
        match &ctx.user {
            Some(_) => {
                let _ = write!(
                    b,
                    r#"<form method="post" action="{}"><input type="hidden" name="change" value="{}"><input type="hidden" name="comment" value="{}"><input type="hidden" name="back" value="{}">"#,
                    ctx.href("act/react"),
                    esc(cid),
                    esc(comment.unwrap_or("")),
                    esc(back)
                );
                let mut shown: Vec<String> = QUICK.iter().map(|s| s.to_string()).collect();
                for e in have.keys() {
                    if !shown.contains(e) {
                        shown.push(e.clone());
                    }
                }
                for e in &shown {
                    let who = have.get(e).cloned().unwrap_or_default();
                    let on = if who.iter().any(|w| w == me) { " on" } else { "" };
                    let count = if who.is_empty() { String::new() } else { format!(" {}", who.len()) };
                    let _ = write!(b, r#"<button class="emo{on}" name="emoji" value="{0}" title="{1}">{0}{count}</button>"#, esc(e), esc(&who.join(", ")));
                }
                b.push_str(r#"<input name="other" size="9" placeholder=":rocket:" aria-label="any emoji"><button class="emo" name="emoji" value="">+</button></form>"#);
            }
            None => {
                for (e, who) in &have {
                    let _ = write!(b, r#"<span class="emo" title="{}">{} {}</span>"#, esc(&who.join(", ")), esc(e), who.len());
                }
            }
        }
        b.push_str("</div>");
        Ok(b)
    }

    async fn inbox(&self, ctx: &Ctx, cfg: &Config) -> Result<Response> {
        let meta: Meta = self.get("meta").await?.unwrap_or_default();
        let mut store = self.ui_store(cfg, meta.head).await?;
        let parked: Vec<ChangeRec> = self.pending().await?.into_iter().filter(|r| matches!(r.state, Phase::AwaitingSeal | Phase::AwaitingAllowance)).collect();
        let mut b = String::new();

        if let Some(user) = &ctx.user {
            let roles = ctx.dir.people.get(user).cloned().unwrap_or_default();
            let mine: Vec<_> = self
                .all_reviews()
                .await?
                .into_iter()
                .filter(|r| r.reviewer == *user || r.reviewer.strip_prefix('@').is_some_and(|t| roles.iter().any(|x| x == t)))
                .collect();
            let _ = write!(b, r#"<h1>Inbox for {}</h1>"#, esc(user));
            b.push_str("<h2>Review requests for you and your teams</h2>");
            let mut any = false;
            for r in &mine {
                let Some(rec) = self.latest_rec(&r.change_id).await? else { continue };
                if !matches!(rec.state, Phase::Queued | Phase::AwaitingSeal | Phase::AwaitingAllowance) {
                    continue;
                }
                any = true;
                let _ = write!(
                    b,
                    r#"<div class="row"><a href="{}"><b>{}</b></a> {} <span class="mute">for {} · asked by {} · {}</span></div>"#,
                    ctx.href(&format!("change/{}", r.change_id)),
                    esc(&r.change_id),
                    state_pill(&rec.state),
                    mention_html(&r.reviewer, &ctx.dir),
                    esc(&r.by),
                    clock(r.at_ms)
                );
            }
            if !any {
                b.push_str(r#"<p class="mute">No open review requests.</p>"#);
            }
            b.push_str("<h2>Mentions</h2>");
            let notes = self.notes(user, 20).await?;
            if notes.iter().all(|n| n.kind == "review_request") {
                b.push_str(r#"<p class="mute">Nobody has mentioned you yet.</p>"#);
            }
            for n in notes.iter().filter(|n| n.kind != "review_request") {
                let via = n.team.as_ref().map(|t| format!(" (to @{})", esc(t))).unwrap_or_default();
                let _ = write!(
                    b,
                    r#"<div class="row"><b>{}</b>{via} on <a href="{}">{}</a> <span class="mute">· {}</span><div class="body">{}</div></div>"#,
                    esc(&n.from),
                    ctx.href(&format!("change/{}", n.change_id)),
                    esc(&n.change_id),
                    clock(n.at_ms),
                    rich(&n.text, &ctx.dir)
                );
            }
            b.push_str("<h2>Everything that needs a seal</h2>");
        } else {
            b.push_str(r#"<h1>Needs a human</h1>"#);
        }
        b.push_str(r#"<p class="mute">Agents can't land these on their own. Read the diff, then sign with your key in a terminal.</p>"#);
        if parked.is_empty() {
            b.push_str(r#"<p>Nothing waits for a human.</p>"#);
        }
        for r in &parked {
            let _ = write!(
                b,
                r#"<div class="card"><h3><a href="{}">{}</a> <span class="mute">rev {} by {}</span> {}</h3>"#,
                ctx.href(&format!("change/{}", r.req.change_id)),
                esc(&r.req.change_id),
                r.req.rev,
                esc(agent_of(&ctx.trunk, &r.req.fork)),
                state_pill(&r.state)
            );
            b.push_str(&parked_detail(ctx, r));
            b.push_str(&self.change_diff(&mut store, r, meta.head).await?);
            b.push_str("</div>");
        }
        self.ui_store_back(store);
        page(ctx, "Inbox", "inbox", &b, (!parked.is_empty()).then_some(5))
    }

    async fn change_page(&self, ctx: &Ctx, cfg: &Config, cid: &str, rev: Option<u32>) -> Result<Response> {
        let rev = match rev {
            Some(r) => Some(r),
            None => self.get::<u32>(&format!("latest:{cid}")).await?,
        };
        let Some(r) = (match rev {
            Some(rev) => self.rec(cid, rev).await?,
            None => None,
        }) else {
            return not_found(ctx);
        };
        let mut b = String::new();
        let _ = write!(
            b,
            r#"<p class="mute">change</p><h1>{} <span class="mute">rev {}</span> {}</h1><p>by {} · submitted {}</p>"#,
            esc(&r.req.change_id),
            r.req.rev,
            state_pill(&r.state),
            esc(agent_of(&ctx.trunk, &r.req.fork)),
            clock(r.submitted_ms)
        );
        match r.state {
            Phase::Landed => {
                let seq = r.seq.unwrap_or(0);
                let _ = write!(b, r#"<p>Landed as seq {seq}. <a href="{}">Open the landing</a>.</p>"#, ctx.href(&format!("landing/{seq}")));
            }
            Phase::Queued => b.push_str("<p>Waiting for a batch.</p>"),
            Phase::AwaitingPick => b.push_str("<p>A competing attempt for its task: it waits until one attempt is picked in the app.</p>"),
            Phase::AwaitingSeal | Phase::AwaitingAllowance => {
                let _ = write!(b, r#"<div class="card">{}</div>"#, parked_detail(ctx, &r));
            }
            Phase::Ejected => {
                let _ = write!(b, r#"<div class="card">{}</div>"#, reason_long(&r));
            }
        }
        if r.req.rev > 1 {
            let _ = write!(b, r#"<p><a href="{}&rev={}">Earlier revision</a></p>"#, ctx.href(&format!("change/{}", r.req.change_id)), r.req.rev - 1);
        }
        b.push_str(&self.conversation(ctx, cid, &ctx.href(&format!("change/{cid}"))).await?);
        if r.state != Phase::Landed {
            let meta: Meta = self.get("meta").await?.unwrap_or_default();
            let mut store = self.ui_store(cfg, meta.head).await?;
            b.push_str("<h2>The change, as the agent made it</h2>");
            b.push_str(&self.change_diff(&mut store, &r, meta.head).await?);
            self.ui_store_back(store);
        }
        let refresh = (r.state == Phase::Queued).then_some(2);
        page(ctx, &r.req.change_id, "board", &b, refresh)
    }

    async fn tree(&self, ctx: &Ctx, cfg: &Config, path: &str, landing: Option<u64>) -> Result<Response> {
        let path = path.trim_matches('/');
        let meta: Meta = self.get("meta").await?.unwrap_or_default();
        let Some(head) = meta.head else {
            return not_found(ctx);
        };
        // The tree at one landing, or at the trunk tip.
        let commit = match landing {
            Some(seq) => match self.get::<LogEntry>(&format!("log:{seq:012}")).await? {
                Some(e) => Oid::from_hex(&e.commit_out).map_err(g)?,
                None => return not_found(ctx),
            },
            None => head,
        };
        let at_q = landing.map(|s| format!("&at={s}")).unwrap_or_default();
        let link = |p: &str| format!("{}{at_q}", ctx.href(p));
        let store = self.ui_store(cfg, Some(head)).await?;
        let view = super::views::path_view(&store, &store.commit(&commit).map_err(g)?.tree, path).map_err(g)?;
        let mut b = String::new();
        let mut crumbs = format!(r#"<a href="{}">{}</a>"#, link("tree"), esc(&ctx.trunk));
        let mut at = String::new();
        for part in path.split('/').filter(|p| !p.is_empty()) {
            at = if at.is_empty() { part.to_string() } else { format!("{at}/{part}") };
            let _ = write!(crumbs, r#" / <a href="{}">{}</a>"#, link(&format!("tree/{}", encode(&at))), esc(part));
        }
        let where_ = match landing {
            Some(seq) => format!(
                r#"at <a href="{}">landing {seq}</a> ({}) · <a href="{}">trunk tip</a>"#,
                ctx.href(&format!("landing/{seq}")),
                short(Some(commit)),
                ctx.href(&format!("tree/{}", encode(path)))
            ),
            None => format!("at the trunk tip ({})", short(Some(head))),
        };
        let _ = write!(b, r#"<p class="crumbs mono">{crumbs} <span class="mute">{where_}</span></p>"#);
        if let Some(super::views::PathView::File(_)) = &view {
            let _ = write!(b, r#"<p><a href="{}&path={}">History of this file</a></p>"#, ctx.href("history"), encode(path));
        }
        if let Some(super::views::PathView::File(e)) = &view {
            match store.blob(&e.oid) {
                Ok(bytes) if bytes.len() <= MAX_FILE_BYTES && std::str::from_utf8(bytes).is_ok() => {
                    let text = std::str::from_utf8(bytes).unwrap_or_default();
                    b.push_str(r#"<div class="file"><table class="diff">"#);
                    for (i, line) in text.lines().enumerate() {
                        let _ = write!(b, r#"<tr><td class="n">{}</td><td>{}</td></tr>"#, i + 1, esc(line));
                    }
                    b.push_str("</table></div>");
                }
                Ok(bytes) => {
                    let _ = write!(b, r#"<p class="mute">Binary or large file, {} bytes.</p>"#, bytes.len());
                }
                Err(_) => b.push_str(r#"<p class="mute">The file is not available.</p>"#),
            }
        } else {
            let prefix = if path.is_empty() { String::new() } else { format!("{path}/") };
            let Some(super::views::PathView::Dir(dirs, files)) = view else {
                self.ui_store_back(store);
                return not_found(ctx);
            };
            b.push_str(r#"<div class="card"><table>"#);
            for d in &dirs {
                let _ = write!(b, r#"<tr><td class="mono"><a href="{}">{}/</a></td></tr>"#, link(&format!("tree/{}", encode(&format!("{prefix}{d}")))), esc(d));
            }
            for f in &files {
                let _ = write!(b, r#"<tr><td class="mono"><a href="{}">{}</a></td></tr>"#, link(&format!("tree/{}", encode(&format!("{prefix}{f}")))), esc(f));
            }
            b.push_str("</table></div>");
        }
        self.ui_store_back(store);
        page(ctx, if path.is_empty() { "Files" } else { path }, "files", &b, None)
    }
}

impl Trunk {
    async fn branches_page(&self, ctx: &Ctx, cfg: &Config) -> Result<Response> {
        let meta: Meta = self.get("meta").await?.unwrap_or_default();
        let mut b = String::from("<h1>Branches</h1>");
        let _ = write!(
            b,
            r#"<p class="mute">Trunk <code>{}</code> is at <code>{}</code> after {}. Agents work in their own forks. Nothing merges from a branch: the sequencer lands each change from it.</p>"#,
            esc(&cfg.branch),
            short(meta.head),
            plural(meta.seq, "landing")
        );
        let branches = self.branches(cfg).await?;
        if branches.is_empty() {
            b.push_str(r#"<p>No agent has a fork yet.</p>"#);
        }
        for br in &branches {
            let _ = write!(b, r#"<div class="card"><h3>{} <span class="mute mono small">{}</span></h3>"#, esc(&br.agent), esc(&br.fork.name));
            match &br.refs {
                Err(e) => {
                    let _ = write!(b, r#"<p class="mute">Cannot read this fork: {}</p>"#, esc(e));
                }
                Ok(rows) => {
                    b.push_str(r#"<div class="scroll"><table><tr><th>Ref</th><th>Commit</th><th>Change</th><th>State</th></tr>"#);
                    for r in rows {
                        let (change, state) = match &r.change {
                            Some((cid, rev, st)) => (
                                format!(r#"<a href="{}">{} <span class="mute">rev {rev}</span></a>"#, esc(&format!("{}&rev={rev}", ctx.href(&format!("change/{cid}")))), esc(cid)),
                                st.as_ref().map(state_pill).unwrap_or(r#"<span class="pill queued">unknown</span>"#).to_string(),
                            ),
                            None if r.name == "refs/heads/main" || r.name == "HEAD" => (r#"<span class="mute">where the fork started</span>"#.into(), String::new()),
                            None => (String::new(), String::new()),
                        };
                        let stale = if r.stale { r#" <span class="pill queued">stale</span>"# } else { "" };
                        let _ = write!(
                            b,
                            r#"<tr><td class="mono">{}</td><td class="mono">{}</td><td>{change}</td><td>{state}{stale}</td></tr>"#,
                            esc(r.name.strip_prefix("refs/heads/").unwrap_or(&r.name)),
                            &r.oid.to_hex()[..7]
                        );
                    }
                    b.push_str("</table></div>");
                    let stale = rows.iter().filter(|r| r.stale).count();
                    if stale > 0 && ctx.user.is_some() {
                        let _ = write!(
                            b,
                            r#"<form class="ask" method="post" action="{}"><input type="hidden" name="agent" value="{}"><button>Delete {} stale {}</button> <span class="mute small">Their PRs merged or closed, or a later revision replaced them.</span></form>"#,
                            ctx.href("act/cleanup"),
                            esc(&br.agent),
                            stale,
                            if stale == 1 { "ref" } else { "refs" }
                        );
                    } else if stale > 0 {
                        let _ = write!(b, r#"<p class="mute small">{} stale. A personal link can delete them.</p>"#, plural(stale as u64, "ref"));
                    }
                }
            }
            b.push_str("</div>");
        }
        if let Some(user) = &ctx.user {
            let _ = write!(
                b,
                r#"<form class="ask" method="post" action="{}"><button>Fork this trunk as {}</button> <span class="mute small">Your own fork, to land changes from as an agent does.</span></form>"#,
                ctx.href("act/fork"),
                esc(user)
            );
        }
        page(ctx, "Branches", "branches", &b, None)
    }

    /// A person's own fork. The page shows how to land from it; it never shows a token.
    async fn fork_for_person(&self, ctx: &Ctx, user: &str) -> Result<Response> {
        let agent: String = user.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
        let mut resp = self.fork(ForkReq { agent: agent.clone() }).await?;
        let v: Value = resp.json().await.unwrap_or(Value::Null);
        let Some(remote) = v["remote"].as_str() else {
            return page(ctx, "Not done", "branches", &format!("<h1>Not done</h1><pre>{}</pre>", esc(&v.to_string())), None);
        };
        let b = format!(
            r#"<h1>Your fork</h1><p><code>{}</code> is yours to push to. Push a branch with plain git, and the push opens a PR from this fork. Your user name is <code>{}</code>. Your password is the key of your personal link.</p>
<pre class="cmd">git clone &lt;this site&gt;/v1/trunks/{}/git
git switch -c my-change &amp;&amp; git commit -am "Say why"
git push origin my-change</pre>
<p class="mute">The agent token does not act for a person, so <code>nerudo mcp</code> with the agent token cannot use this fork. Fork: <code>{}</code></p><p><a href="{}">Back to branches</a></p>"#,
            esc(v["name"].as_str().unwrap_or("?")),
            esc(user),
            esc(&ctx.trunk),
            esc(remote),
            ctx.href("branches")
        );
        page(ctx, "Your fork", "branches", &b, None)
    }

    /// Landings newest first, 50 to a page; with `path`, only the landings that changed it.
    async fn history(&self, ctx: &Ctx, cfg: &Config, before: Option<u64>, path: Option<&str>) -> Result<Response> {
        let meta: Meta = self.get("meta").await?.unwrap_or_default();
        let store = self.ui_store(cfg, meta.head).await?;
        let top = before.unwrap_or(meta.seq + 1).saturating_sub(1).min(meta.seq);
        let mut b = String::new();
        match path {
            Some(p) => {
                let _ = write!(b, r#"<h1>History of <code>{}</code></h1>"#, esc(p));
            }
            None => b.push_str("<h1>History</h1>"),
        }
        b.push_str(r#"<div class="scroll"><table><tr><th>Landing</th><th>Change</th><th>What</th><th>Agent</th><th>Batch</th><th>When</th></tr>"#);
        // A file's history reads further back; trunk's pages hold 50 landings.
        let (limit, floor) = if path.is_some() { (200, top.saturating_sub(400)) } else { (50, 0) };
        let mut shown = 0;
        let mut seq = top;
        while seq > floor && shown < limit {
            if let Some(e) = self.get::<LogEntry>(&format!("log:{seq:012}")).await? {
                let commit = Oid::from_hex(&e.commit_out).ok().and_then(|o| store.commit(&o).ok());
                let touched = match (path, &commit) {
                    (None, _) => true,
                    (Some(p), Some(c)) => {
                        let (now, was) = super::views::file_at(&store, c, p).unwrap_or_default();
                        now != was
                    }
                    (Some(_), None) => false,
                };
                if touched {
                    shown += 1;
                    let subject = commit.as_ref().map(|c| c.message.lines().next().unwrap_or("").to_string()).unwrap_or_default();
                    let subject = subject.strip_prefix(&format!("{}: ", e.change_id)).unwrap_or(&subject).to_string();
                    let agent = commit.as_ref().map(|c| c.author.name.clone()).unwrap_or_default();
                    let _ = write!(
                        b,
                        r#"<tr><td><a href="{}">{}</a> <a class="mute small" href="{}&at={}">tree</a></td><td><a href="{}">{}</a></td><td>{}</td><td>{}</td><td>{}</td><td class="mute">{}</td></tr>"#,
                        ctx.href(&format!("landing/{}", e.seq)),
                        e.seq,
                        ctx.href("tree"),
                        e.seq,
                        ctx.href(&format!("change/{}", e.change_id)),
                        esc(&e.change_id),
                        esc(&with_emoji(&subject)),
                        esc(&agent),
                        e.batch,
                        clock(e.landed_at_ms)
                    );
                }
            }
            seq -= 1;
        }
        b.push_str("</table></div>");
        if shown == 0 {
            b.push_str(r#"<p class="mute">No landings here.</p>"#);
        }
        if path.is_none() && seq > 0 {
            let _ = write!(b, r#"<p class="pager"><span></span><a href="{}&before={}">Older →</a></p>"#, ctx.href("history"), seq + 1);
        }
        self.ui_store_back(store);
        page(ctx, "History", "history", &b, None)
    }
}

/// Why a parked change waits, the quorum so far, and the command a human runs.
fn parked_detail(ctx: &Ctx, r: &ChangeRec) -> String {
    let mut b = String::new();
    if r.state == Phase::AwaitingSeal {
        b.push_str("<ul>");
        for reason in &r.seal_reasons {
            let _ = write!(b, "<li>{}</li>", esc(reason));
        }
        b.push_str("</ul>");
        for t in r.reason.as_ref().and_then(|v| v["quorum"].as_array()).into_iter().flatten() {
            let have: Vec<&str> = t["have"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            let roles: Vec<&str> = t["roles"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            let _ = write!(
                b,
                r#"<p>Quorum for {}: {} of {} {} {}</p>"#,
                esc(t["kind"].as_str().unwrap_or("?")),
                have.len(),
                t["need"],
                if roles.is_empty() { "approvers".to_string() } else { roles.iter().map(|r| mention_html(&format!("@{r}"), &ctx.dir)).collect::<Vec<_>>().join(" or ") },
                if have.is_empty() { String::new() } else { format!("({})", esc(&have.join(", "))) }
            );
        }
        let _ = write!(b, r#"<pre class="cmd">nerudo seal --trunk {} --change {} --rev {}</pre>"#, esc(&ctx.trunk), esc(&r.req.change_id), r.req.rev);
    } else {
        let units = r.reason.as_ref().map(|v| v["units"].to_string()).unwrap_or_default();
        let _ = write!(b, r#"<p>Needs an allowance for {}.</p><pre class="cmd">nerudo allowance --trunk {} --units '…' --max N</pre>"#, esc(&units), esc(&ctx.trunk));
    }
    b
}

// ---------------------------------------------------------------------- text

/// Comment or commit text as HTML: escaped, :shortcodes: as emoji, and @mentions of known
/// people, teams and agents highlighted.
fn rich(text: &str, dir: &Directory) -> String {
    let s = esc(&with_emoji(text));
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '@' && (i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_')) {
            let name: String = chars[i + 1..].iter().take_while(|c| c.is_ascii_alphanumeric() || "._-".contains(**c)).collect();
            let name = name.trim_end_matches(['.', '-']).to_string();
            if !name.is_empty() && dir.resolve(&name).is_some() {
                out.push_str(&mention_html(&format!("@{name}"), dir));
                i += 1 + name.chars().count();
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// A person, team or agent as a highlighted name. `who` may start with @.
fn mention_html(who: &str, dir: &Directory) -> String {
    let (class, title) = match dir.resolve(who) {
        Some(Who::Team(t)) => ("mention team", format!("team: {}", dir.team(&t).join(", "))),
        Some(Who::Agent(_)) => ("mention agent", "agent".to_string()),
        Some(Who::Person(p)) => ("mention", dir.people.get(&p).map(|r| r.join(", ")).unwrap_or_default()),
        None => ("mention", String::new()),
    };
    let label = if who.starts_with('@') { who.to_string() } else { format!("@{who}") };
    format!(r#"<span class="{class}" title="{}">{}</span>"#, esc(&title), esc(&label))
}

// ---------------------------------------------------------------------- diff

/// Changed files between two trees, each with a line diff and three lines of context.
fn diff_trees(store: &Store, before: &Flat, after: &Flat) -> String {
    let paths: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    let mut out = String::new();
    let mut n = 0;
    for p in paths {
        let (a, b) = (before.get(p), after.get(p));
        if a.map(|e| e.oid) == b.map(|e| e.oid) {
            continue;
        }
        n += 1;
        let what = match (a, b) {
            (None, _) => "added",
            (_, None) => "deleted",
            _ => "changed",
        };
        let text = |e: Option<&FileEntry>| -> Option<String> {
            match e {
                None => Some(String::new()),
                Some(e) => store.blob(&e.oid).ok().filter(|b| b.len() <= MAX_FILE_BYTES).and_then(|b| String::from_utf8(b.to_vec()).ok()),
            }
        };
        let _ = write!(out, r#"<div class="file"><h3>{} <span class="mute">{what}</span></h3>"#, esc(p));
        match (text(a), text(b)) {
            (Some(x), Some(y)) => out.push_str(&line_diff(&x, &y)),
            _ => out.push_str(r#"<p class="mute pad">Binary or large file.</p>"#),
        }
        out.push_str("</div>");
    }
    if n == 0 {
        out.push_str(r#"<p class="mute">No file changes.</p>"#);
    }
    out
}

/// One file's diff as table rows: old line, new line, text.
fn line_diff(old: &str, new: &str) -> String {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let ops = edit_script(&a, &b);
    // Show changed lines with three lines of context around them.
    let near: Vec<bool> = (0..ops.len()).map(|i| ops[i.saturating_sub(3)..(i + 4).min(ops.len())].iter().any(|(k, _)| *k != ' ')).collect();
    let mut out = String::from(r#"<table class="diff">"#);
    let (mut i, mut j) = (0, 0);
    let mut gap = false;
    for (idx, (k, line)) in ops.iter().enumerate() {
        let (old_n, new_n) = match k {
            ' ' => {
                i += 1;
                j += 1;
                (i.to_string(), j.to_string())
            }
            '-' => {
                i += 1;
                (i.to_string(), String::new())
            }
            _ => {
                j += 1;
                (String::new(), j.to_string())
            }
        };
        if !near[idx] {
            gap = true;
            continue;
        }
        if gap {
            out.push_str(r#"<tr class="gap"><td class="n">⋯</td><td class="n"></td><td></td></tr>"#);
            gap = false;
        }
        let class = match k {
            '+' => "add",
            '-' => "del",
            _ => "",
        };
        let _ = write!(out, r#"<tr class="{class}"><td class="n">{old_n}</td><td class="n">{new_n}</td><td>{k} {}</td></tr>"#, esc(line));
    }
    out.push_str("</table>");
    out
}

/// A shortest edit script: common prefix and suffix, then Myers' diff of the middle when it
/// takes at most MAX_DIFF_EDITS edits.
pub(super) fn edit_script<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<(char, &'a str)> {
    let (n, m) = (a.len(), b.len());
    let mut pre = 0;
    while pre < n && pre < m && a[pre] == b[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < n - pre && suf < m - pre && a[n - 1 - suf] == b[m - 1 - suf] {
        suf += 1;
    }
    let (x, y) = (&a[pre..n - suf], &b[pre..m - suf]);
    let mut out: Vec<(char, &str)> = a[..pre].iter().map(|l| (' ', *l)).collect();
    match myers(x, y, MAX_DIFF_EDITS) {
        Some(ops) => out.extend(ops),
        None => {
            out.extend(x.iter().map(|l| ('-', *l)));
            out.extend(y.iter().map(|l| ('+', *l)));
        }
    }
    out.extend(a[n - suf..].iter().map(|l| (' ', *l)));
    out
}

/// Myers' diff: the shortest edit script from `x` to `y`, or None past `max_d` edits. It
/// costs O((N+M)D) time and O(D²) memory, so a big file with a few edits stays exact.
fn myers<'a>(x: &[&'a str], y: &[&'a str], max_d: usize) -> Option<Vec<(char, &'a str)>> {
    let (n, m) = (x.len() as isize, y.len() as isize);
    let dmax = (n + m).min(max_d as isize);
    let off = dmax + 2;
    let mut v = vec![0isize; (2 * off + 1) as usize];
    // trace[d]: V before step d, for k in -(d+1)..=d+1.
    let mut trace: Vec<Vec<isize>> = vec![];
    let mut end = None;
    'search: for d in 0..=dmax {
        trace.push(v[(off - d - 1) as usize..=(off + d + 1) as usize].to_vec());
        for k in (-d..=d).step_by(2) {
            let i = (off + k) as usize;
            let mut px = if k == -d || (k != d && v[i - 1] < v[i + 1]) { v[i + 1] } else { v[i - 1] + 1 };
            let mut py = px - k;
            while px < n && py < m && x[px as usize] == y[py as usize] {
                px += 1;
                py += 1;
            }
            v[i] = px;
            if px >= n && py >= m {
                end = Some(d);
                break 'search;
            }
        }
    }
    end?;
    let mut ops = vec![];
    let (mut px, mut py) = (n, m);
    for (d, snap) in trace.iter().enumerate().rev() {
        let d = d as isize;
        let at = |k: isize| snap[(k + d + 1) as usize];
        let k = px - py;
        let pk = if k == -d || (k != d && at(k - 1) < at(k + 1)) { k + 1 } else { k - 1 };
        let (qx, qy) = (at(pk), at(pk) - pk);
        while px > qx && py > qy {
            ops.push((' ', x[px as usize - 1]));
            px -= 1;
            py -= 1;
        }
        if d > 0 {
            if px == qx {
                ops.push(('+', y[py as usize - 1]));
            } else {
                ops.push(('-', x[px as usize - 1]));
            }
        }
        (px, py) = (qx, qy);
    }
    ops.reverse();
    Some(ops)
}

// ---------------------------------------------------------------------- helpers

fn mac_hex(env: &Env, msg: &str) -> Result<String> {
    let secret = crate::secret(env, "NERUDO_ADMIN_TOKEN")?.to_string();
    let mac = hmac_sha256(secret.as_bytes(), msg.as_bytes());
    Ok(mac[..16].iter().map(|b| format!("{b:02x}")).collect())
}

pub(super) fn share_key(env: &Env, trunk: &str) -> Result<String> {
    mac_hex(env, &format!("landseq share v1\n{trunk}"))
}

pub(super) fn user_key(env: &Env, trunk: &str, user: &str) -> Result<String> {
    mac_hex(env, &format!("landseq user v1\n{trunk}\n{user}"))
}

/// A user's sign-in key: one for every repo that lists the user as a person.
pub(crate) fn account_key(env: &Env, user: &str) -> Result<String> {
    mac_hex(env, &format!("landseq account v1\n{user}"))
}

/// An agent's own key on one trunk: `lsa1.<agent>.<gen>.<mac>`. A new `gen` revokes the
/// keys of the older ones (agents.rs).
pub(super) fn agent_key(env: &Env, trunk: &str, agent: &str, gen: u64) -> Result<String> {
    Ok(format!("lsa1.{agent}.{gen}.{}", mac_hex(env, &format!("landseq agent v1\n{trunk}\n{agent}\n{gen}"))?))
}



fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    inner.update(msg);
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// The shape of every key `mac_hex` makes: 32 lowercase hex characters. A request's key is
/// taken only in this shape, so no other text reaches a page or a storage key as a key.
pub(super) fn valid_key(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Equal, in time that does not depend on where the inputs differ.
pub(crate) fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c => o.push(c),
        }
    }
    o
}

/// Percent-encode a repo path for a URL, keeping `/`.
fn encode(path: &str) -> String {
    path.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn decode(s: &str) -> String {
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn short(o: Option<Oid>) -> String {
    o.map(|o| o.to_hex()[..7].to_string()).unwrap_or_else(|| "none".into())
}

fn plural(n: u64, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// UTC time of day for a millisecond timestamp.
fn clock(ms: i64) -> String {
    let s = (ms / 1000).rem_euclid(86_400);
    format!("{:02}:{:02}:{:02} UTC", s / 3600, s / 60 % 60, s % 60)
}

pub(super) fn agent_of<'a>(trunk: &str, fork: &'a str) -> &'a str {
    fork.strip_prefix(trunk).and_then(|r| r.strip_prefix("--")).unwrap_or(fork)
}

fn num(value: &str, label: &str) -> String {
    format!(r#"<div class="num"><b>{}</b><span>{}</span></div>"#, esc(value), esc(label))
}

fn state_pill(s: &Phase) -> &'static str {
    match s {
        Phase::Queued => r#"<span class="pill queued">queued</span>"#,
        Phase::AwaitingSeal => r#"<span class="pill parked">needs a seal</span>"#,
        Phase::AwaitingAllowance => r#"<span class="pill parked">needs an allowance</span>"#,
        Phase::AwaitingPick => r#"<span class="pill parked">waiting for a pick</span>"#,
        Phase::Landed => r#"<span class="pill landed">landed</span>"#,
        Phase::Ejected => r#"<span class="pill ejected">ejected</span>"#,
    }
}

fn waits_because(r: &ChangeRec) -> String {
    match r.state {
        Phase::AwaitingSeal => esc(&r.seal_reasons.join("; ")),
        Phase::AwaitingAllowance => "no allowance covers it".into(),
        Phase::AwaitingPick => "a competing attempt: it waits until one attempt is picked in the app".into(),
        _ if r.footprint.is_empty() => "not gated yet".into(),
        _ => "its units overlap an earlier change, or the batch is full".into(),
    }
}

fn diagnostics(reason: &Value) -> Vec<String> {
    reason["diagnostics"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|d| {
            let s = |k: &str| d[k].as_str().map(str::to_string).unwrap_or_else(|| d[k].to_string());
            format!("{} {}:{} {}", s("code"), s("file"), s("line"), s("text"))
        })
        .collect()
}

fn reason_short(reason: Option<&Value>) -> String {
    let Some(r) = reason else { return String::new() };
    match r["kind"].as_str().unwrap_or("?") {
        "ci_failed" => format!(r#"<span class="pill ejected">CI</span> <span class="mono small">{}</span>"#, esc(diagnostics(r).first().map(String::as_str).unwrap_or(""))),
        "ci_crashed" => r#"<span class="pill ejected">CI</span> the checker crashed"#.into(),
        "conflict" => format!(r#"<span class="pill ejected">conflict</span> <span class="mono small">{}</span>"#, esc(&r["paths"].to_string())),
        k => esc(k),
    }
}

fn reason_long(r: &ChangeRec) -> String {
    let Some(reason) = r.reason.as_ref() else { return String::new() };
    match reason["kind"].as_str().unwrap_or("?") {
        "ci_failed" => {
            let lines: String = diagnostics(reason).iter().map(|d| format!("{}\n", esc(d))).collect();
            format!("<p>The type check failed on trunk with this change. It may pass on its own base: a change that landed first can break it.</p><pre>{lines}</pre>")
        }
        "ci_crashed" => format!("<p>The type checker crashed on trunk with this change.</p><pre>{}</pre>", esc(&reason["detail"].to_string())),
        "conflict" => format!("<p>It edits lines that changed on trunk. The agent builds it again on the new trunk.</p><pre>{}</pre>", esc(&reason["paths"].to_string())),
        _ => format!("<pre>{}</pre>", esc(&reason.to_string())),
    }
}

// ---------------------------------------------------------------------- layout

const CSS: &str = r#"
:root{--bg:#f7f6f2;--fg:#1c1b19;--mute:#6d6b64;--line:#e2dfd6;--card:#ffffff;--accent:#c2410c;--ok:#15803d;--warn:#b45309;--bad:#b91c1c;--add:#e6f4ea;--del:#fbe9e9;--code:#efede6;--hi:#fdf0e6}
@media (prefers-color-scheme: dark){:root{--bg:#121211;--fg:#ebeae5;--mute:#9a988f;--line:#2b2a27;--card:#1a1a18;--accent:#fb923c;--ok:#4ade80;--warn:#fbbf24;--bad:#f87171;--add:#10301b;--del:#3b1717;--code:#23221f;--hi:#33210f}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.5 ui-sans-serif,system-ui,-apple-system,"Segoe UI",sans-serif}
a{color:inherit}
header{border-bottom:1px solid var(--line);padding:12px 20px;display:flex;gap:22px;align-items:baseline;flex-wrap:wrap}
.brand{font-weight:700;text-decoration:none}.brand i{color:var(--accent);font-style:normal}
nav a{color:var(--mute);text-decoration:none;margin-right:16px;padding-bottom:3px}
nav a.on{color:var(--fg);border-bottom:2px solid var(--accent)}
.who{margin-left:auto;color:var(--mute);font-size:13px}
main{max-width:1080px;margin:0 auto;padding:20px 16px 40px}
h1{font-size:24px;margin:4px 0 12px;line-height:1.25}
h2{font-size:12px;text-transform:uppercase;letter-spacing:.09em;color:var(--mute);margin:30px 0 10px;font-weight:600}
h3{font-size:15px;margin:0 0 8px}
.mute{color:var(--mute)}.small{font-size:12px;overflow-wrap:anywhere;word-break:break-all}
.numbers{display:flex;gap:30px;flex-wrap:wrap;margin:6px 0 4px}
.num b{display:block;font-size:28px;line-height:1.1}.num span{color:var(--mute);font-size:13px}
.scroll{overflow-x:auto}
table{border-collapse:collapse;width:100%}
td,th{text-align:left;padding:7px 8px;border-bottom:1px solid var(--line);vertical-align:top}
th{font-size:12px;color:var(--mute);font-weight:500}
.kv th{width:190px}
code,pre,.mono{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:13px}
code{background:var(--code);padding:1px 5px;border-radius:4px;word-break:break-all}
pre{background:var(--code);padding:10px 12px;border-radius:6px;overflow-x:auto;white-space:pre-wrap}
pre.cmd{border-left:3px solid var(--accent)}
.pill{font-size:12px;padding:1px 8px;border-radius:999px;border:1px solid currentColor;white-space:nowrap}
.landed{color:var(--ok)}.parked{color:var(--warn)}.ejected{color:var(--bad)}.queued{color:var(--mute)}.seal{color:var(--warn)}
.trunk{border-left:3px solid var(--accent);margin:6px 0 0 6px;padding-left:20px}
.batch{position:relative;margin:0 0 18px}
.batch:before{content:"";position:absolute;left:-28px;top:5px;width:13px;height:13px;border-radius:50%;background:var(--bg);border:3px solid var(--accent)}
.batch .meta{color:var(--mute);font-size:13px;margin-bottom:2px}
.land{padding:2px 0}.land a{text-decoration:none}.land a:hover b{text-decoration:underline}
.why{white-space:pre-wrap;background:var(--card);border:1px solid var(--line);border-left:3px solid var(--accent);border-radius:6px;padding:12px 14px;margin:8px 0}
.card{background:var(--card);border:1px solid var(--line);border-radius:8px;padding:14px 16px;margin:12px 0;overflow-x:auto}
.file{border:1px solid var(--line);border-radius:8px;margin:12px 0;overflow-x:auto;background:var(--card)}
.file h3{padding:8px 12px;margin:0;border-bottom:1px solid var(--line);font-family:ui-monospace,Menlo,monospace;font-size:13px}
.pad{padding:8px 12px}
.diff td{border:0;padding:0 10px;font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:12.5px;white-space:pre}
.diff .n{color:var(--mute);text-align:right;width:1%;user-select:none}
.diff .add{background:var(--add)}.diff .del{background:var(--del)}.diff .gap td{color:var(--mute)}
.crumbs a{text-decoration:none}
.pager{display:flex;justify-content:space-between;margin-top:20px}
.mention{background:var(--hi);color:var(--accent);border-radius:4px;padding:0 3px;font-weight:600}
.mention.team:after{content:" team";font-weight:400;font-size:11px;color:var(--mute)}
.mention.agent:after{content:" agent";font-weight:400;font-size:11px;color:var(--mute)}
.comment{border-left:2px solid var(--line);padding:4px 0 4px 14px;margin:12px 0}
.comment .meta,.row .mute{font-size:13px}.comment .body,.row .body{white-space:pre-wrap;margin:2px 0 4px}
.row{padding:8px 0;border-bottom:1px solid var(--line)}
.reviews div{margin:4px 0}
.rx{display:flex;flex-wrap:wrap;gap:4px;align-items:center;margin:4px 0}
.rx form{display:flex;flex-wrap:wrap;gap:4px;align-items:center;margin:0}
.emo{font:inherit;font-size:14px;background:var(--card);border:1px solid var(--line);border-radius:999px;padding:1px 9px;color:var(--fg);cursor:pointer}
.emo.on{border-color:var(--accent);background:var(--hi)}
span.emo{cursor:default}
.rx input{font:inherit;font-size:13px;width:7em;padding:2px 8px;border:1px solid var(--line);border-radius:999px;background:var(--card);color:var(--fg)}
form.say{margin:16px 0 8px;display:flex;flex-direction:column;gap:8px;max-width:720px}
form.say textarea{font:inherit;padding:8px 10px;border:1px solid var(--line);border-radius:6px;background:var(--card);color:var(--fg)}
form button,form.ask select{font:inherit;padding:5px 12px;border-radius:6px;border:1px solid var(--line);background:var(--card);color:var(--fg)}
form.say button,form.ask button{align-self:flex-start;background:var(--accent);border-color:var(--accent);color:#fff;cursor:pointer}
form.ask{margin:8px 0;display:flex;gap:8px;align-items:center;flex-wrap:wrap}
footer{color:var(--mute);font-size:13px;padding:24px 16px 40px;text-align:center;border-top:1px solid var(--line)}
@media (max-width:640px){header{padding:10px 16px;gap:12px}h1{font-size:20px}.kv th{width:auto}.who{margin-left:0}}
"#;

fn page(ctx: &Ctx, title: &str, active: &str, body: &str, refresh: Option<u32>) -> Result<Response> {
    let on = |k: &str| if k == active { r#" class="on""# } else { "" };
    let inbox = if ctx.inbox > 0 { format!("Inbox ({})", ctx.inbox) } else { "Inbox".into() };
    let refresh = refresh.map(|s| format!(r#"<meta http-equiv="refresh" content="{s}">"#)).unwrap_or_default();
    let who = match &ctx.user {
        Some(u) => format!(r#"<span class="who">you are <b>{}</b></span>"#, esc(u)),
        None => r#"<span class="who">read-only link</span>"#.into(),
    };
    let doc = format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">{refresh}<title>{} · {} · nerudo</title><style>{CSS}</style></head><body>
<header><a class="brand" href="{}">NERUDO</a><span class="mono">{}</span><nav><a{} href="{}">Board</a><a{} href="{}">{inbox}</a><a{} href="{}">Branches</a><a{} href="{}">History</a><a{} href="{}">Files</a></nav>{who}</header>
<main>{body}</main>
<footer>No pull requests: agents land, humans seal, and trunk is the record. This page works without JavaScript.</footer>
</body></html>"#,
        esc(title),
        esc(&ctx.trunk),
        ctx.href(""),
        esc(&ctx.trunk),
        on("board"),
        ctx.href(""),
        on("inbox"),
        ctx.href("inbox"),
        on("branches"),
        ctx.href("branches"),
        on("history"),
        ctx.href("history"),
        on("files"),
        ctx.href("tree"),
    );
    html(200, &doc)
}

fn bare(title: &str, body: &str) -> String {
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{} · nerudo</title><style>{CSS}</style></head><body><header><span class="brand">NERUDO</span></header><main><h1>{}</h1>{body}</main></body></html>"#,
        esc(title),
        esc(title)
    )
}

fn not_found(ctx: &Ctx) -> Result<Response> {
    let mut r = page(ctx, "Not found", "", "<h1>Not found</h1>", None)?;
    r = r.with_status(404);
    Ok(r)
}

fn html(status: u16, doc: &str) -> Result<Response> {
    let headers = Headers::new();
    headers.set("content-type", "text/html; charset=utf-8")?;
    headers.set("cache-control", "no-store")?;
    // The key is in the URL: never send it on to another site.
    headers.set("referrer-policy", "no-referrer")?;
    headers.set("x-content-type-options", "nosniff")?;
    headers.set("content-security-policy", "default-src 'none'; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'self'; frame-ancestors 'none'")?;
    Ok(Response::ok(doc)?.with_status(status).with_headers(headers))
}

#[cfg(test)]
mod tests {
    use super::edit_script;

    #[test]
    fn canceled_cache_owner_returns_objects_and_wakes_waiting_readers() {
        use super::{Mem, UiStore};
        use futures::channel::oneshot;
        use nerudo_git::Store;
        use std::cell::RefCell;

        let mem = RefCell::new(Mem::default());
        let mut cache = Store::new();
        let oid = cache.put_blob(b"keep this cached object");
        let (tx, mut rx) = oneshot::channel();
        mem.borrow_mut().ui_out = true;
        mem.borrow_mut().ui_waiters.push(tx);
        {
            let owner = UiStore::taken(&mem, cache);
            assert!(mem.borrow().ui_out);
            assert!(!mem.borrow().ui.contains(&oid));
            assert!(rx.try_recv().unwrap().is_none());
            // Ending a request before its fetch completes drops the same guard.
            drop(owner);
        }
        assert!(!mem.borrow().ui_out);
        assert!(mem.borrow().ui.contains(&oid));
        assert!(mem.borrow().ui_waiters.is_empty());
        assert_eq!(rx.try_recv().unwrap(), Some(()));
    }

    fn counts(ops: &[(char, &str)]) -> (usize, usize) {
        (ops.iter().filter(|o| o.0 == '+').count(), ops.iter().filter(|o| o.0 == '-').count())
    }

    /// The script rebuilds both sides: ' ' and '-' give `a`, ' ' and '+' give `b`.
    fn rebuilds(a: &[&str], b: &[&str], ops: &[(char, &str)]) -> bool {
        let left: Vec<&str> = ops.iter().filter(|o| o.0 != '+').map(|o| o.1).collect();
        let right: Vec<&str> = ops.iter().filter(|o| o.0 != '-').map(|o| o.1).collect();
        left == a && right == b
    }

    #[test]
    fn a_big_file_with_a_few_edits_counts_exactly() {
        let a: Vec<String> = (0..3000).map(|i| format!("line {i}")).collect();
        let mut b = a.clone();
        b[0] = "first, changed".into();
        b[2999] = "last, changed".into();
        for j in 0..5 {
            b.insert(1500, format!("new {j}"));
        }
        let (a, b): (Vec<&str>, Vec<&str>) = (a.iter().map(String::as_str).collect(), b.iter().map(String::as_str).collect());
        let ops = edit_script(&a, &b);
        assert_eq!(counts(&ops), (7, 2));
        assert!(rebuilds(&a, &b, &ops));
    }

    #[test]
    fn as_short_as_a_longest_common_subsequence() {
        let mut seed = 7u64;
        let mut next = move || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as usize
        };
        let words = ["a", "b", "c", "d"];
        for _ in 0..300 {
            let a: Vec<&str> = (0..next() % 12).map(|_| words[next() % 4]).collect();
            let b: Vec<&str> = (0..next() % 12).map(|_| words[next() % 4]).collect();
            let mut t = vec![vec![0usize; b.len() + 1]; a.len() + 1];
            for i in (0..a.len()).rev() {
                for j in (0..b.len()).rev() {
                    t[i][j] = if a[i] == b[j] { t[i + 1][j + 1] + 1 } else { t[i + 1][j].max(t[i][j + 1]) };
                }
            }
            let ops = edit_script(&a, &b);
            let (adds, dels) = counts(&ops);
            assert_eq!((adds, dels), (b.len() - t[0][0], a.len() - t[0][0]), "{a:?} -> {b:?}");
            assert!(rebuilds(&a, &b, &ops), "{a:?} -> {b:?}: {ops:?}");
        }
    }
}
