//! The trunk as a named repo on the Hub (crate::hub): who can read it, its summary for org
//! pages, and links to it in the web app.
//!
//! Storage: `vis` is true when the repo is public. A public repo's pages and app API read
//! with no key, as with the share link. Acting still needs a personal link.

use super::ui::{account_key, same, user_key};
use super::*;
use serde_json::json;

/// How long a user's sign-in check holds before the Hub is asked again.
const ACCOUNT_TTL_MS: f64 = 60_000.0;

#[derive(Deserialize)]
pub(super) struct VisibilityReq {
    public: bool,
}

impl Trunk {
    pub(super) async fn is_public(&self) -> Result<bool> {
        Ok(self.get::<bool>("vis").await?.unwrap_or(false))
    }

    /// `POST .../visibility {public}` (admin). The Hub sends it each time this trunk gets
    /// or loses a name, so the trunk also forgets the name it knew.
    pub(super) async fn set_visibility(&self, r: VisibilityReq) -> Result<Response> {
        let Some(cfg) = self.get::<Config>("cfg").await? else {
            return bad(404, "unknown trunk: init it first");
        };
        self.put("vis", &r.public).await?;
        self.mem.borrow_mut().hub_name = None;
        Response::from_json(&json!({ "trunk": cfg.trunk, "public": r.public }))
    }

    /// A person's key: their personal link for this trunk, or their sign-in key (one per
    /// user, for every repo). The sign-in key works only for a user the Hub has, and only if
    /// this repo's approver of that name has the user's key: an approver with another key is
    /// someone else with the same name. A user with no key yet is only an approver listed
    /// with no key (one who has not joined); a name alone takes no approver who has a key.
    pub(super) async fn person_ok(&self, trunk: &str, user: &str, given: &str) -> Result<bool> {
        if same(given.as_bytes(), user_key(&self.env, trunk, user)?.as_bytes()) {
            return Ok(true);
        }
        if !same(given.as_bytes(), account_key(&self.env, user)?.as_bytes()) {
            return Ok(false);
        }
        if let Some((ok, until)) = self.mem.borrow().accounts.get(user).copied() {
            if now() < until {
                return Ok(ok);
            }
        }
        let ok = match crate::hub::user(&self.env, user).await? {
            None => false,
            Some(u) => {
                let rules = self.rules().await?;
                match u.key {
                    None => rules.approvers.iter().any(|a| a.name == user && a.key.is_empty()),
                    Some(k) => rules.approvers.iter().any(|a| a.name == user && a.key == k),
                }
            }
        };
        self.mem.borrow_mut().accounts.insert(user.to_string(), (ok, now() + ACCOUNT_TTL_MS));
        Ok(ok)
    }

    /// `GET .../summary`: what an org page shows of this repo.
    pub(super) async fn summary(&self) -> Result<Response> {
        let Some(cfg) = self.get::<Config>("cfg").await? else {
            return bad(404, "unknown trunk");
        };
        let stats: Stats = self.get("stats").await?.unwrap_or_default();
        let meta: Meta = self.get("meta").await?.unwrap_or_default();
        let mut open: BTreeSet<String> = self.get::<Vec<String>>("q").await?.unwrap_or_default().into_iter().collect();
        open.extend(self.parked_keys().await?);
        let closed = stats.ejected_conflict + stats.ejected_check + stats.ejected_ci + stats.ejected_other + stats.not_picked;
        let latest_release = self.releases().await?.first().map(|r| json!({ "tag": r.tag, "title": r.title, "seq": r.seq, "created_ms": r.created_ms }));
        let dir = self.directory().await?;
        // Approver keys are public in nerudo.toml; a user's profile matches on them. A person
        // with no key has not joined nerudo yet.
        let keys: BTreeMap<String, String> = self.rules().await?.approvers.iter().filter(|a| !a.key.is_empty()).map(|a| (a.name.clone(), a.key.clone())).collect();
        let updated_ms = self.events(1).await?.first().map(|e| e.at_ms);
        Response::from_json(&json!({
            "trunk": cfg.trunk, "head": meta.head, "public": self.is_public().await?,
            "stats": { "merged": stats.landed, "open": open.len(), "closed": closed, "batches": stats.batches, "submitted": stats.submitted },
            "latest_release": latest_release, "people": dir.people, "keys": keys, "agents": dir.agents, "updated_ms": updated_ms,
            // Why the queue waits, when the landing loop failed (trunk/stall.rs).
            "stall": self.stall().await?,
        }))
    }

    /// This trunk's `{org}/{repo}` on the Hub. A name once found stays in memory until
    /// `visibility` says it changed.
    pub(super) async fn hub_name(&self, trunk: &str) -> Option<String> {
        if let Some(known) = self.mem.borrow().hub_name.clone() {
            return Some(known);
        }
        let name = match crate::hub::name_of(&self.env, trunk).await {
            Ok(Some(r)) => format!("{}/{}", r.org, r.repo),
            Ok(None) => return None,
            Err(e) => {
                console_warn!("hub lookup of {trunk}: {e}");
                return None;
            }
        };
        self.mem.borrow_mut().hub_name = Some(name.clone());
        Some(name)
    }

    /// A link into the web app (the `NERUDO_WEB` var), or None without it:
    /// `{web}/{org}/{repo}/{path}?{query}` for a public repo with a name, else
    /// `{web}/app/{trunk}/{path}?{query}` (the app can't read a private name).
    pub(super) async fn web_link(&self, trunk: &str, path: &str, query: &str) -> Option<String> {
        let web = crate::variable(&self.env, "NERUDO_WEB").ok()?.to_string();
        let web = web.trim().trim_end_matches('/');
        if web.is_empty() {
            return None;
        }
        let named = match self.is_public().await {
            Ok(true) => self.hub_name(trunk).await,
            _ => None,
        };
        let base = match named {
            Some(name) => format!("{web}/{name}"),
            None => format!("{web}/app/{trunk}"),
        };
        let path = path.trim_matches('/');
        let path = if path.is_empty() { String::new() } else { format!("/{path}") };
        let query = if query.is_empty() { String::new() } else { format!("?{query}") };
        Some(format!("{base}{path}{query}"))
    }
}
