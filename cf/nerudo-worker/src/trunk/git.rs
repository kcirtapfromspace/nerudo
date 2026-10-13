//! Plain git against the sequencer, as against a git host: `git clone` and `git fetch` read
//! main, and `git push origin my-branch` opens or updates the pull request of that branch.
//!
//!   GET  git/info/refs?service=git-upload-pack     main, read with the object's own token
//!   POST git/git-upload-pack
//!   GET  git/info/refs?service=git-receive-pack    the pusher's fork, made on the first push
//!   POST git/git-receive-pack                      to the fork; then each pushed branch
//!                                                  opens or updates PR <branch>
//!
//! Auth is HTTP Basic: an agent's name with its own agent key, or a person in nerudo.toml
//! with their personal key (the `k` of their invite link). The agent token works only for an
//! agent that has no fork yet: its first push makes the fork, and from then on only the
//! agent's key is its password (agents.rs). `nerudo git-credential` gets the key before the
//! first push, so git never sees the agent token. The agent token never takes a person's
//! name or a person's fork name. A public repo reads with no credentials. Nobody pushes
//! main, tags, or refs outside refs/heads/: main moves only through the queue, and releases
//! are tags that the sequencer signs.
//!
//! The change id of a pushed branch is scoped to its fork and exact branch name. Existing
//! branch PRs retain their old ids when their stored fork and branch match. A push to a
//! branch whose PR merged opens a new PR, `<id>..2`, then
//! `..3`: a merged change id takes no new revision (trunk/revisions.rs). A push while a
//! revision of the PR is in the batch being pushed waits for that batch. A PR's base must be
//! on main, because the three-way merge reads it: it is the newest commit behind the push
//! that is on main, or that an earlier PR of the branch merged (its changes are on main
//! already).
//!
//! Storage: `ft:{agent}` when the object last minted the write token of that agent's fork.
//! `fg:{agent}` when a push with the agent token made that agent's fork: a push is two
//! requests (info/refs, then receive-pack), and the second one still takes the agent token.

use super::context::is_context_branch;
use super::ui::same;
use super::*;
use js_sys::{Reflect, Uint8Array};
use nerudo_git::{insert_before_flush, parse_commands, parse_report, refusal, remote_lines};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

/// The largest push the object takes. The pack stays on the JS heap: wasm memory only grows.
const MAX_PUSH_BYTES: u32 = 64 << 20;
/// The command section of a push must end within this many bytes.
const MAX_COMMAND_BYTES: u32 = 1 << 20;
/// The trunk read token (it lives a day) is kept this long.
const READ_TOKEN_KEEP_MS: f64 = 10.0 * 60_000.0;
/// A fork write token (it lives a day) is used for this long after it was minted.
const FORK_TOKEN_KEEP_MS: f64 = 12.0 * 3_600_000.0;
/// Commits of main that can be a base, nearest the tip first.
const MAIN_WALK: usize = 20_000;
/// Commits behind a push that are searched for a base.
const PUSH_WALK: usize = 2_000;
/// On Artifacts: landings of the log that can be a base, newest first, and commits behind the
/// oldest of them, read one at a time.
const MAIN_LOG: usize = 2_000;
const BEFORE_LOG: usize = 64;
/// How long the agent token pushes to the fork that its push made, until that push is sent.
const FIRST_PUSH_MS: f64 = 10.0 * 60_000.0;

/// The sender of HTTP Basic credentials that are right.
struct Pusher {
    /// The agent whose fork it pushes to: an agent, or a person's fork name.
    name: String,
    /// It sent the agent token, for an agent without a fork: its push makes the fork.
    by_token: bool,
}

/// What a link to a PR may name: `{org}/{repo}` for a public repo the Hub named.
struct Links {
    web: Option<String>,
    named: Option<String>,
    trunk: String,
}

impl Links {
    fn pr(&self, number: Option<u64>, cid: &str) -> Option<String> {
        let web = self.web.as_ref()?;
        Some(match (&self.named, number) {
            (Some(name), Some(n)) => format!("{web}/{name}/pulls/{n}"),
            _ => format!("{web}/app/{}/pulls/{cid}", self.trunk),
        })
    }
}

/// The PR of a branch, as branch_pr finds it.
pub(super) struct BranchPr {
    /// Its change id: the branch's, or `<id>..<n>` after n - 1 PRs of the branch merged.
    cid: String,
    latest: Option<ChangeRec>,
    /// The revisions that the earlier PRs of the branch merged, oldest first.
    merged: Vec<ChangeRec>,
}

/// The `remote:` lines for a push that waited for a batch, and the batch did not end. The fork
/// has the branch now, so git sends nothing for the same commit again.
fn still_merging(branch: &str, latest: Option<&ChangeRec>, rev: u32) -> Vec<String> {
    let pr = latest.map(number).unwrap_or_else(|| change_id_of(branch));
    let what = if rev > 0 { format!("revision {rev}") } else { "a revision".into() };
    vec![
        format!("Pull request {pr} is merging {what} now, so this push did not change it. Your fork has {branch}."),
        "When the merge ends, make a new commit (or git commit --amend --no-edit) and push again: git sends nothing for a commit that the fork has.".into(),
    ]
}

impl Trunk {
    pub(super) async fn git(&self, req: &mut Request, rest: &[&str]) -> Result<Response> {
        let Some(cfg) = self.get::<Config>("cfg").await? else {
            return plain(404, "repository not found");
        };
        let service = req.url()?.query_pairs().find(|(k, _)| k == "service").map(|(_, v)| v.into_owned()).unwrap_or_default();
        // The receive-pack POST is the second half of a push: it ends the agent token's one push.
        let sends = req.method() == Method::Post && matches!(rest, ["git-receive-pack"]);
        let pusher = match self.git_user(req, &cfg, sends).await? {
            Ok(w) => w,
            Err(None) => return need_auth(),
            Err(Some(why)) => return plain(403, &why),
        };
        let by_token = pusher.as_ref().is_some_and(|p| p.by_token);
        let who = pusher.map(|p| p.name);
        match (req.method(), rest) {
            (Method::Get, ["info", "refs"]) if service == "git-upload-pack" => {
                if who.is_none() && !self.is_public().await? {
                    return need_auth();
                }
                let url = format!("{}/info/refs?service=git-upload-pack", cfg.remote.trim_end_matches('/'));
                proxy(req, &url, &self.git_read_token(&cfg).await?, None).await
            }
            (Method::Post, ["git-upload-pack"]) => {
                if who.is_none() && !self.is_public().await? {
                    return need_auth();
                }
                let url = format!("{}/git-upload-pack", cfg.remote.trim_end_matches('/'));
                let body = req.inner().body().map(JsValue::from);
                proxy(req, &url, &self.git_read_token(&cfg).await?, body).await
            }
            (Method::Get, ["info", "refs"]) if service == "git-receive-pack" => {
                let Some(who) = who else { return need_auth() };
                let fork = match self.git_fork(&cfg, &who, by_token).await? {
                    Ok(f) => f,
                    Err(e) => return plain(502, &e),
                };
                proxy(req, &format!("{}/info/refs?service=git-receive-pack", fork.remote.trim_end_matches('/')), &fork.authorization, None).await
            }
            (Method::Post, ["git-receive-pack"]) => {
                let Some(who) = who else { return need_auth() };
                self.receive_pack(req, &cfg, &who, by_token).await
            }
            (_, ["info", "refs"]) => plain(400, "nerudo serves the smart HTTP protocol only (git 1.6.6 or newer)"),
            _ => plain(404, "not found"),
        }
    }

    /// The sender of HTTP Basic credentials, and the agent whose fork it pushes to: an agent,
    /// or a person's name with anything but [A-Za-z0-9] as `_` (as for a fork made from the
    /// web). `sends`: this request sends a push. Ok(None): no credentials were sent. Err(None):
    /// wrong ones. Err(Some): the agent token for an agent that has a fork, and why not.
    async fn git_user(&self, req: &Request, cfg: &Config, sends: bool) -> Result<std::result::Result<Option<Pusher>, Option<String>>> {
        let Some(h) = req.headers().get("Authorization")? else {
            return Ok(Ok(None));
        };
        let creds = h.strip_prefix("Basic ").and_then(|b| unbase64(b.trim())).and_then(|raw| String::from_utf8(raw).ok());
        let Some((user, pass)) = creds.as_deref().and_then(|c| c.split_once(':')) else {
            return Ok(Err(None));
        };
        let pusher = |name: String, by_token: bool| Ok(Ok(Some(Pusher { name, by_token })));
        // A person's name takes their own key (sign-in or personal link), never the agent token.
        if self.directory().await?.people.contains_key(user) {
            if !self.person_ok(&cfg.trunk, user, pass).await? {
                return Ok(Err(None));
            }
            return pusher(user.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect(), false);
        }
        // An agent's own key is its password.
        if pass.starts_with("lsa1.") {
            let ok = self.key_agent(pass).await?.is_ok_and(|a| a == user);
            return if ok { pusher(user.to_string(), false) } else { Ok(Err(None)) };
        }
        // The agent token never takes the name of a person's fork (`lee_ann` for `lee-ann`).
        if self.is_person(user).await? {
            return Ok(Err(None));
        }
        let token = crate::secret(&self.env, "NERUDO_AGENT_TOKEN").map(|s| s.to_string()).unwrap_or_default();
        if !host::valid_agent(user) || token.is_empty() || !same(pass.as_bytes(), token.as_bytes()) {
            return Ok(Err(None));
        }
        // The agent token makes the fork of a new agent, with that agent's first push. Then
        // it no longer acts for the agent: only the agent's key does.
        if self.get::<Fork>(&format!("fa:{user}")).await?.is_some() {
            let key = format!("fg:{user}");
            let made = self.get::<f64>(&key).await?.filter(|t| now() - t < FIRST_PUSH_MS);
            if made.is_none() {
                return Ok(Err(Some(format!(
                    "agent {user} has a fork on trunk {}. The agent token is not its password any more: its agent key is. \
                     nerudo git-credential gives git the key. No key? {}",
                    cfg.trunk,
                    super::agents::remedy(&cfg.trunk, user)
                ))));
            }
            if sends {
                self.state.storage().delete(&key).await?;
            }
        }
        pusher(user.to_string(), true)
    }

    /// The Authorization that reads trunk, kept for a few minutes.
    async fn git_read_token(&self, cfg: &Config) -> Result<String> {
        if let Some((a, until)) = self.mem.borrow().git_read.clone() {
            if now() < until {
                return Ok(a);
            }
        }
        // A host that mints no read token (Forgejo) reads with the object's own credential.
        let a = host::trunk_read_authorization(&self.env, &cfg.host).await?.unwrap_or_else(|| cfg.authorization.clone());
        self.mem.borrow_mut().git_read = Some((a.clone(), now() + READ_TOKEN_KEEP_MS));
        Ok(a)
    }

    /// The pusher's fork, made on their first push, with a write token well inside its life.
    /// `mark`: a push with the agent token makes the fork here, and its receive-pack follows.
    async fn git_fork(&self, cfg: &Config, agent: &str, mark: bool) -> Result<std::result::Result<Fork, String>> {
        let known = self.get::<Fork>(&format!("fa:{agent}")).await?;
        let minted = self.get::<f64>(&format!("ft:{agent}")).await?.unwrap_or(0.0);
        if let Some(f) = known.clone().filter(|_| now() - minted < FORK_TOKEN_KEEP_MS) {
            return Ok(Ok(f));
        }
        if known.is_none() && self.list_prefix("fa:").await?.len() >= super::MAX_AGENTS {
            return Ok(Err(format!("this trunk has {} agents already; no new agent can fork it", super::MAX_AGENTS)));
        }
        // Two first pushes for one name: only one makes the fork (as for POST /forks).
        let _first = match known.is_some() {
            true => None,
            false => match self.mem.borrow_mut().first_forks.insert(agent.to_string()) {
                true => Some(FirstFork(&self.mem, agent.to_string())),
                false => return Ok(Err(format!("the fork of {agent} is being made now; push again in a few seconds"))),
            },
        };
        let slot = self.fork_slot().await;
        let res = host::fork(&self.env, &cfg.host, agent, known.clone()).await;
        drop(slot);
        let f = match res {
            Ok(f) => f,
            Err(e) => return Ok(Err(format!("fork failed: {e}"))),
        };
        let rec = serde_json::to_string(&f)?;
        let mut writes = vec![(format!("f:{}", f.name), rec.clone()), (format!("fa:{agent}"), rec), (format!("ft:{agent}"), now().to_string())];
        if mark && known.is_none() {
            writes.push((format!("fg:{agent}"), now().to_string()));
        }
        self.put_all(writes).await?;
        Ok(Ok(f))
    }

    /// `by_token`: the agent token sent this push, and it made the fork of `who`.
    async fn receive_pack(&self, req: &mut Request, cfg: &Config, who: &str, by_token: bool) -> Result<Response> {
        let Some(body) = read_body(req, MAX_PUSH_BYTES).await? else {
            return plain(413, &format!("a push can be at most {} MiB", MAX_PUSH_BYTES >> 20));
        };
        let head = body.subarray(0, body.length().min(MAX_COMMAND_BYTES)).to_vec();
        let cmds = match parse_commands(&head) {
            Ok(Some(c)) => c,
            Ok(None) => return plain(400, "the push commands do not end in the first MiB"),
            Err(e) => return plain(400, &e.to_string()),
        };
        drop(head);
        let sideband = cmds.has("side-band-64k");

        // Refuse what can never be pushed before anything reaches the fork.
        let mut reasons: Vec<Option<String>> = vec![];
        let mut lines: Vec<String> = vec![];
        let mut teach = false;
        for c in &cmds.list {
            let why = if c.name == cfg.branch {
                teach = true;
                Some("main moves only through the merge queue".to_string())
            } else if c.name.starts_with("refs/tags/") {
                teach = true;
                Some("releases are tags that the sequencer signs".to_string())
            } else if let Some(branch) = c.name.strip_prefix("refs/heads/") {
                let cid = change_id_of(branch);
                if is_context_branch(&c.name) || c.is_delete() || valid_change_id(&cid) {
                    None
                } else {
                    lines.push(format!("{branch} cannot be a pull request: a pull request branch is 1 to 64 of A-Z a-z 0-9 . _ - and /."));
                    Some("not a pull request branch name".to_string())
                }
            } else {
                teach = true;
                Some("push a branch (refs/heads/...)".to_string())
            };
            reasons.push(why);
        }
        if teach {
            lines.push("main moves only through the merge queue. Push a branch, and it opens a pull request:".into());
            lines.push("  git push origin HEAD:my-change".into());
            lines.push("The queue merges the pull request when its checks pass.".into());
            lines.push("Releases are tags that the sequencer signs (nerudo release). You cannot push tags.".into());
        }
        let fork = match self.git_fork(cfg, who, false).await? {
            Ok(f) => f,
            Err(e) => return plain(502, &e),
        };
        // The agent token's one push: it made the fork, and the next push needs the key.
        let note = match by_token {
            true => vec![
                format!("This push made the fork of agent {who}. Its next push needs its agent key as the password, not the agent token."),
                format!("{}. nerudo git-credential gets the key before the first push.", super::agents::remedy(&cfg.trunk, who)),
            ],
            false => vec![],
        };
        // A scoped identifier already claimed by another PR stays theirs. Ordinary equal
        // branch names in different forks have different identifiers.
        for (c, why) in cmds.list.iter().zip(reasons.iter_mut()) {
            let Some(branch) = c.name.strip_prefix("refs/heads/") else { continue };
            if why.is_some() || c.is_delete() || is_context_branch(&c.name) {
                continue;
            }
            if let Err(e) = self.branch_pr(&fork.name, branch).await? {
                lines.push(format!("{branch}: {e}"));
                *why = Some("another pull request's identifier".into());
            }
        }
        if reasons.iter().any(Option::is_some) {
            let refused: Vec<(String, String)> = cmds
                .list
                .iter()
                .zip(&reasons)
                .map(|(c, why)| (c.name.clone(), why.clone().unwrap_or_else(|| "not pushed: another ref in this push was refused".into())))
                .collect();
            lines.extend(note);
            return pack_result(refusal(&refused, &lines, sideband));
        }

        // Pass the push on to the fork as it came.
        let headers = Headers::new();
        for k in ["Content-Type", "Accept"] {
            if let Some(v) = req.headers().get(k)? {
                headers.set(k, &v)?;
            }
        }
        headers.set("Authorization", &fork.authorization)?;
        let mut init = RequestInit::new();
        init.with_method(Method::Post).with_headers(headers).with_body(Some(body.into()));
        let mut up = Fetch::Request(Request::new_with_init(&format!("{}/git-receive-pack", fork.remote.trim_end_matches('/')), &init)?).send().await?;
        if up.status_code() != 200 {
            return pass_through(up);
        }
        let resp = up.bytes().await?;
        let Ok(report) = parse_report(&resp, sideband) else {
            return pack_result(resp);
        };
        let accepted: HashSet<&str> = report.refs.iter().filter(|(_, ng)| ng.is_none()).map(|(n, _)| n.as_str()).collect();
        let links = Links {
            web: crate::variable(&self.env, "NERUDO_WEB").ok().map(|v| v.to_string().trim().trim_end_matches('/').to_string()).filter(|w| !w.is_empty()),
            // The router's X-Nerudo-Repo, or the Hub's name when the push used the trunk id.
            named: match self.is_public().await? {
                true => match crate::request_header(&req, "X-Nerudo-Repo")?.filter(|r| valid_repo_name(r)) {
                    Some(r) => Some(r),
                    None => self.hub_name(&cfg.trunk).await,
                },
                false => None,
            },
            trunk: cfg.trunk.clone(),
        };
        let mut lines = vec![];
        for c in cmds.list.iter().filter(|c| accepted.contains(c.name.as_str()) && !is_context_branch(&c.name)) {
            let Some(branch) = c.name.strip_prefix("refs/heads/") else { continue };
            if c.is_delete() {
                if let Ok(pr) = self.branch_pr(&fork.name, branch).await? {
                    if let Some(r) = pr.latest.as_ref().or(pr.merged.last()) {
                        lines.push(format!("Deleted {branch} on your fork. Pull request {} is unchanged.", number(r)));
                    }
                }
                continue;
            }
            lines.extend(self.push_pr(cfg, &fork, branch, c.new, &links).await?);
        }
        lines.extend(note);
        if !sideband || lines.is_empty() {
            return pack_result(resp);
        }
        pack_result(insert_before_flush(&resp, &remote_lines(&lines)))
    }

    /// Stable first PR id, including compatible histories created before fork scoping.
    pub(super) async fn branch_first_id(&self, fork: &str, branch: &str) -> Result<String> {
        let scoped = branch_change_id(fork, branch);
        let canonical = self.latest_rec(&scoped).await?;
        let legacy = match canonical {
            None => self.latest_rec(&change_id_of(branch)).await?,
            Some(_) => None,
        };
        Ok(branch_id_for(fork, branch, canonical.as_ref().map(|r| &r.req), legacy.as_ref().map(|r| &r.req)))
    }

    /// The PR that a push to `branch` opens or updates: the first PR of the branch, or once that
    /// merged, the next one (`<id>..2`, `<id>..3`), with the revisions that merged before it.
    pub(super) async fn branch_pr(&self, fork: &str, branch: &str) -> Result<std::result::Result<BranchPr, String>> {
        let first = self.branch_first_id(fork, branch).await?;
        let mut cid = first.clone();
        let mut merged = vec![];
        let mut n = 1;
        loop {
            let latest = self.latest_rec(&cid).await?;
            if let Some(r) = latest.as_ref().filter(|r| !branch_owned_by(&r.req, fork, branch)) {
                return Ok(Err(format!("the branch identifier is already pull request {} from another branch or fork; push your work as another branch", number(r))));
            }
            let landed = match &latest {
                Some(r) => self.merged_revs(&cid, r.req.rev).await?,
                None => vec![],
            };
            if landed.is_empty() {
                return Ok(Ok(BranchPr { cid, latest, merged }));
            }
            merged.extend(landed);
            n += 1;
            match super::revisions::successor(&first, n) {
                Some(next) => cid = next,
                None => {
                    let last = merged.last().map(number).unwrap_or_default();
                    return Ok(Err(format!("pull request {last} merged this branch, and its name is too long for a new pull request. Push your work as a branch with a shorter name.")));
                }
            }
        }
    }

    /// Open or update the PR of a branch the fork accepted. Returns the `remote:` lines.
    async fn push_pr(&self, cfg: &Config, fork: &Fork, branch: &str, commit: Oid, links: &Links) -> Result<Vec<String>> {
        // A revision of the branch's PR in the batch being pushed: wait for the batch, then
        // update the PR, or open the next one if it merged. The batch can pick the PR while
        // its base is read, so try again then.
        let mut waited = None;
        for _ in 0..3 {
            let mut pr = match self.branch_pr(&fork.name, branch).await? {
                Ok(pr) => pr,
                Err(e) => return Ok(vec![format!("{branch}: {e}")]),
            };
            if let Some(rev) = self.merging(&pr.cid).await? {
                if let Some(rev) = self.wait_merged(&pr.cid).await? {
                    return Ok(still_merging(branch, pr.latest.as_ref(), rev));
                }
                let label = pr.latest.as_ref().map(number).unwrap_or_else(|| pr.cid.clone());
                waited = Some(format!("This push waited for the batch with revision {rev} of pull request {label}."));
                pr = match self.branch_pr(&fork.name, branch).await? {
                    Ok(pr) => pr,
                    Err(e) => return Ok(vec![format!("{branch}: {e}")]),
                };
            }
            if let Some(mut lines) = self.push_pr_once(cfg, fork, branch, commit, links, pr).await? {
                if let Some(w) = waited {
                    lines.insert(lines.len().min(1), w);
                }
                return Ok(lines);
            }
        }
        let pr = self.branch_pr(&fork.name, branch).await?.ok();
        Ok(still_merging(branch, pr.as_ref().and_then(|p| p.latest.as_ref()), 0))
    }

    /// One try of push_pr. None: the PR's revision went into a batch meanwhile.
    async fn push_pr_once(&self, cfg: &Config, fork: &Fork, branch: &str, commit: Oid, links: &Links, pr: BranchPr) -> Result<Option<Vec<String>>> {
        let BranchPr { cid, latest, merged } = pr;
        if let Some(r) = latest.as_ref().filter(|r| r.req.commit == commit) {
            let mut out = vec![format!("Pull request {} for {branch} already has this commit.", number(r))];
            out.extend(links.pr(r.number, &cid).map(|l| format!("  {l}")));
            return Ok(Some(out));
        }
        let before = merged.last().filter(|_| latest.is_none());
        if let Some(m) = before.filter(|m| m.req.commit == commit) {
            return Ok(Some(vec![format!("Pull request {} for {branch} merged this commit already. Commit your next change, then push again.", number(m))]));
        }
        let (base, trailer) = match self.push_base(cfg, fork, commit, &merged).await? {
            Ok(found) => found,
            Err(e) => return Ok(Some(vec![format!("{branch}: {e}")])),
        };
        if base == commit {
            return Ok(Some(vec![format!("{branch} has no commit that is not on main. Commit your change, then push again.")]));
        }
        let mut out = vec![];
        let mut task = None;
        if let Some(id) = trailer.clone().or_else(|| task_of_branch(branch)) {
            if self.task(&id).await?.is_some() {
                task = Some(id);
            } else if trailer.is_some() {
                out.push(format!("There is no task {id}, so the pull request has no task."));
            }
        }
        let task = task.or_else(|| latest.as_ref().and_then(|r| r.req.task.clone()));
        let rev = latest.as_ref().map(|r| r.req.rev + 1).unwrap_or(1);
        let req = LandReq { change_id: cid.clone(), rev, fork: fork.name.clone(), commit, base, task: task.clone(), context_commit: None, branch: Some(branch.to_string()) };
        let mut resp = self.land(&super::agents::Caller::trusted(), req).await?;
        let ok = resp.status_code() == 200;
        let v: Value = resp.json().await.unwrap_or(Value::Null);
        if !ok {
            if v["kind"] == "merging" {
                return Ok(None);
            }
            out.push(format!("Could not open a pull request for {branch}: {}", v["error"].as_str().unwrap_or("unknown error")));
            return Ok(Some(out));
        }
        let n = v["number"].as_u64();
        let label = n.map(|n| format!("#{n}")).unwrap_or_else(|| cid.clone());
        out.insert(0, if rev == 1 { format!("Opened pull request {label} for {branch}") } else { format!("Updated pull request {label} (revision {rev})") });
        if let Some(m) = before {
            out.insert(1, format!("Pull request {} for {branch} is merged, and a merged pull request takes no new commits. So this push opened a new one.", number(m)));
        }
        // The revision it replaces does not merge (trunk/revisions.rs).
        if let Some(prev) = latest.as_ref().filter(|r| !matches!(r.state, Phase::Landed | Phase::Ejected)) {
            out.push(format!("Revision {rev} replaces revision {}, which does not merge.", prev.req.rev));
            if prev.state == Phase::AwaitingSeal {
                out.push(format!("It needs its approvals again: they were for the commit of revision {}.", prev.req.rev));
            }
        }
        out.extend(links.pr(n, &cid).map(|l| format!("  {l}")));
        if let Some(t) = &task {
            out.push(format!("It is linked to task {t}."));
        }
        out.push("The queue merges it when checks pass and it has the approvals it needs.".into());
        Ok(Some(out))
    }

    /// The base of a pushed commit, and the `Task:` trailer of its message. The base is the
    /// commit behind the push that is nearest the tip of main: a commit on main, or one that an
    /// earlier PR of the branch merged (`merged`: its changes are on main as the commit it
    /// landed as).
    async fn push_base(&self, cfg: &Config, fork: &Fork, commit: Oid, merged: &[ChangeRec]) -> Result<std::result::Result<(Oid, Option<String>), String>> {
        let meta: Meta = self.get("meta").await?.unwrap_or_default();
        let Some(head) = meta.head else {
            return Ok(Err("main has no commit yet".into()));
        };
        // Main, nearest the tip first. On Artifacts the landing log names main's commits and
        // the store reads only the commits behind the oldest landing, one at a time; elsewhere
        // the UI's copy of trunk has main's history.
        let mut rd = self.readers(cfg).await?;
        let t = FetchTransport::default();
        let mut store = Store::new();
        let mut main: HashMap<Oid, usize> = HashMap::new();
        let mut order: Vec<Oid> = vec![];
        let mut queue = VecDeque::from([head]);
        if rd.lazy {
            for seq in (1..=meta.seq).rev().take(MAIN_LOG) {
                if let Some(e) = self.get::<LogEntry>(&format!("log:{seq:012}")).await? {
                    if let Ok(o) = Oid::from_hex(&e.commit_out) {
                        queue.push_back(o);
                    }
                }
            }
            for c in std::mem::take(&mut queue) {
                if main.insert(c, order.len()).is_none() {
                    order.push(c);
                }
            }
            // The commits behind the oldest landing (a trunk started on an existing repo).
            let mut cur = order.last().copied();
            for _ in 0..BEFORE_LOG {
                let Some(c) = cur else { break };
                if !self.resolve(&mut rd, None, &t, &mut store, c, "commit").await? {
                    break;
                }
                cur = store.commit(&c).ok().and_then(|x| x.parents.first().copied());
                if let Some(p) = cur.filter(|p| !main.contains_key(p)) {
                    main.insert(p, order.len());
                    order.push(p);
                }
            }
        } else {
            // A copy of the UI's view of trunk (pointers, not objects), so that the UI keeps its
            // own while the fork is read.
            let ui = self.ui_store(cfg, Some(head)).await?;
            store.absorb(&ui);
            self.ui_store_back(ui);
            while let Some(c) = queue.pop_front() {
                if order.len() >= MAIN_WALK {
                    break;
                }
                if main.contains_key(&c) {
                    continue;
                }
                // An imported trunk can be shallow: its history ends where the store does.
                let Ok(x) = store.commit(&c) else { continue };
                main.insert(c, order.len());
                order.push(c);
                queue.extend(x.parents);
            }
        }

        // The fork knows main as of when it was made or last pushed: offer the newest commits
        // of main, then ever older ones, so it sends only what main does not have. On
        // Artifacts, commits and trees only, and commits one at a time if git fails.
        let haves: Vec<Oid> = order.iter().enumerate().filter(|(i, _)| *i < 32 || (i - 31).is_power_of_two()).map(|(_, o)| *o).collect();
        let remote = Remote::new(&fork.remote, Some(fork.authorization.clone()));
        let opts = if rd.lazy { FetchOpts { no_blobs: true, any_haves: true, ..FetchOpts::default() } } else { FetchOpts::default() };
        if let Err(e) = fetch_with(&t, &remote, &[commit], &haves, opts, &mut store).await {
            if !rd.lazy || !self.resolve(&mut rd, Some(&fork.name), &t, &mut store, commit, "commit").await? {
                return Ok(Err(format!("cannot read {} from your fork: {e}", &commit.to_hex()[..7])));
            }
        }
        let mut landed: HashMap<Oid, usize> = HashMap::new();
        for r in merged {
            if let Some(i) = r.trunk_commit.and_then(|tc| main.get(&tc)) {
                landed.insert(r.req.commit, *i);
            }
        }

        // Walk back from the push over every parent; stop at the first base on each path.
        let mut best: Option<(usize, bool, Oid)> = None;
        let mut seen: HashSet<Oid> = HashSet::new();
        let mut queue = VecDeque::from([commit]);
        while let Some(c) = queue.pop_front() {
            if seen.len() >= PUSH_WALK {
                break;
            }
            if !seen.insert(c) {
                continue;
            }
            // On a tie, the commit on main wins over the landed revision.
            let hit = main.get(&c).map(|i| (*i, false)).or_else(|| landed.get(&c).map(|i| (*i, true)));
            if let Some((i, revision)) = hit {
                if best.is_none_or(|(bi, br, _)| (i, revision) < (bi, br)) {
                    best = Some((i, revision, c));
                }
                continue;
            }
            if rd.lazy && !store.contains(&c) && !main.contains_key(&c) {
                self.resolve(&mut rd, Some(&fork.name), &t, &mut store, c, "commit").await?;
            }
            if let Ok(x) = store.commit(&c) {
                queue.extend(x.parents);
            }
        }
        let trailer = store.commit(&commit).ok().and_then(|c| c.trailer("Task"));
        Ok(match best {
            Some((_, _, base)) => Ok((base, trailer)),
            None => Err("the branch has no commit from main; rebase on origin/main and push again".into()),
        })
    }
}

/// The legacy branch id, used only to find PRs that predate fork-scoped identifiers.
pub(super) fn change_id_of(branch: &str) -> String {
    branch.replace('/', "-")
}

/// Fork identity and exact branch name are separated before hashing. The `..` namespace
/// cannot be a Git branch name, and 53 characters leave room for successor PR suffixes.
pub(super) fn branch_change_id(fork: &str, branch: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(fork.as_bytes());
    hash.update([0]);
    hash.update(branch.as_bytes());
    let digest = hash.finalize();
    let hex: String = digest[..24].iter().map(|b| format!("{b:02x}")).collect();
    format!("git..{hex}")
}

pub(super) fn branch_owned_by(req: &LandReq, fork: &str, branch: &str) -> bool {
    req.fork == fork && req.branch.as_deref() == Some(branch)
}

/// Keep an existing legacy PR only with proof of both fork and exact branch ownership.
/// A canonical PR always wins, so a retry cannot jump between independent PR histories.
fn branch_id_for(fork: &str, branch: &str, canonical: Option<&LandReq>, legacy: Option<&LandReq>) -> String {
    if canonical.is_none() && legacy.is_some_and(|r| branch_owned_by(r, fork, branch)) {
        change_id_of(branch)
    } else {
        branch_change_id(fork, branch)
    }
}

/// The task a branch is named for: `T-3`, `T-3-...`, `t-3/...`.
fn task_of_branch(branch: &str) -> Option<String> {
    let rest = branch.strip_prefix("T-").or_else(|| branch.strip_prefix("t-"))?;
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    let after = &rest[digits..];
    (digits > 0 && (after.is_empty() || after.starts_with('-') || after.starts_with('/'))).then(|| format!("T-{}", &rest[..digits]))
}

/// `{org}/{repo}` as the front router sends it in X-Nerudo-Repo.
fn valid_repo_name(s: &str) -> bool {
    let Some((org, repo)) = s.split_once('/') else { return false };
    !org.is_empty() && !repo.is_empty() && org.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') && repo.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

fn number(r: &ChangeRec) -> String {
    r.number.map(|n| format!("#{n}")).unwrap_or_else(|| r.req.change_id.clone())
}

fn plain(status: u16, text: &str) -> Result<Response> {
    let headers = Headers::new();
    headers.set("Content-Type", "text/plain; charset=utf-8")?;
    Ok(Response::ok(format!("{text}\n"))?.with_status(status).with_headers(headers))
}

/// 401 with the header that makes git ask its credential helper and try again.
fn need_auth() -> Result<Response> {
    let headers = Headers::new();
    headers.set("Content-Type", "text/plain; charset=utf-8")?;
    headers.set("WWW-Authenticate", r#"Basic realm="nerudo""#)?;
    Ok(Response::ok("authentication required: an agent name and its agent key, or your name and your personal key\n")?.with_status(401).with_headers(headers))
}

fn pack_result(body: Vec<u8>) -> Result<Response> {
    let headers = Headers::new();
    headers.set("Content-Type", "application/x-git-receive-pack-result")?;
    headers.set("Cache-Control", "no-cache")?;
    Ok(Response::from_bytes(body)?.with_headers(headers))
}

/// Send the request on to the git host and stream its answer back.
async fn proxy(req: &Request, url: &str, authorization: &str, body: Option<JsValue>) -> Result<Response> {
    let headers = Headers::new();
    for k in ["Content-Type", "Content-Encoding", "Accept", "Git-Protocol"] {
        if let Some(v) = req.headers().get(k)? {
            headers.set(k, &v)?;
        }
    }
    if !authorization.is_empty() {
        headers.set("Authorization", authorization)?;
    }
    let mut init = RequestInit::new();
    init.with_method(req.method()).with_headers(headers).with_body(body);
    let up = Fetch::Request(Request::new_with_init(url, &init)?).send().await?;
    pass_through(up)
}

/// The host's answer, its body streamed, never read into wasm memory.
fn pass_through(up: Response) -> Result<Response> {
    let status = up.status_code();
    if status == 401 || status == 403 {
        // The host refused the object's own token: not something git's credentials can fix.
        console_warn!("git host answered {status}");
        return plain(502, "the git host refused the sequencer's token");
    }
    let headers = Headers::new();
    for k in ["Content-Type", "Cache-Control", "Expires", "Pragma"] {
        if let Some(v) = up.headers().get(k)? {
            headers.set(k, &v)?;
        }
    }
    Ok(Response::builder().with_status(status).with_headers(headers).body(up.body().clone()))
}

/// The request body, read on the JS heap. None: it is longer than `cap`.
async fn read_body(req: &Request, cap: u32) -> Result<Option<Uint8Array>> {
    let Some(stream) = req.inner().body() else {
        return Ok(Some(Uint8Array::new_with_length(0)));
    };
    let reader = call(&stream, "getReader")?;
    let chunks = js_sys::Array::new();
    let mut total: u32 = 0;
    loop {
        let step = JsFuture::from(call(&reader, "read")?.unchecked_into::<js_sys::Promise>()).await?;
        if Reflect::get(&step, &"done".into())?.as_bool() == Some(true) {
            break;
        }
        let chunk: Uint8Array = Reflect::get(&step, &"value".into())?.unchecked_into();
        total = total.saturating_add(chunk.length());
        if total > cap {
            let _ = call(&reader, "cancel");
            return Ok(None);
        }
        chunks.push(&chunk);
    }
    let all = Uint8Array::new_with_length(total);
    let mut at = 0;
    for c in chunks.iter() {
        let c: Uint8Array = c.unchecked_into();
        all.set(&c, at);
        at += c.length();
    }
    Ok(Some(all))
}

fn call(obj: &JsValue, method: &str) -> Result<JsValue> {
    let f: js_sys::Function = Reflect::get(obj, &method.into())?.unchecked_into();
    Ok(f.call0(obj)?)
}

fn unbase64(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let s = s.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for chunk in s.chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= val(c)? << (18 - 6 * i);
        }
        match chunk.len() {
            4 => out.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8, n as u8]),
            3 => out.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8]),
            2 => out.push((n >> 16) as u8),
            _ => return None,
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(fork: &str, branch: &str) -> LandReq {
        LandReq {
            change_id: change_id_of(branch),
            rev: 1,
            fork: fork.into(),
            commit: Oid::ZERO,
            base: Oid::ZERO,
            task: None,
            context_commit: None,
            branch: Some(branch.into()),
        }
    }

    #[test]
    fn branch_ids_scope_equal_names_to_each_agents_fork() {
        let a = branch_change_id("repo--alice", "feature/x");
        let b = branch_change_id("repo--bob", "feature/x");
        assert_ne!(a, b);
        for cid in [&a, &b] {
            assert!(valid_change_id(cid));
            assert_eq!(cid.len(), 53);
            assert!(super::super::revisions::successor(cid, 2).is_some());
        }
        // The separator prevents ambiguous concatenations of fork and branch names.
        assert_ne!(branch_change_id("ab", "c"), branch_change_id("a", "bc"));
    }

    #[test]
    fn branch_ids_distinguish_slashes_from_hyphens() {
        assert_eq!(change_id_of("feature/x"), change_id_of("feature-x"), "the old encoding collided");
        assert_ne!(branch_change_id("repo--alice", "feature/x"), branch_change_id("repo--alice", "feature-x"));
        assert_ne!(branch_change_id("repo--alice", "a/b-c"), branch_change_id("repo--alice", "a-b/c"));
    }

    #[test]
    fn legacy_branch_prs_keep_their_ids_for_updates_and_retries() {
        let mut existing = req("repo--alice", "feature/x");
        assert_eq!(branch_id_for("repo--alice", "feature/x", None, Some(&existing)), "feature-x");
        existing.rev = 2;
        existing.commit = Oid::from_hex(&"ab".repeat(20)).unwrap();
        assert_eq!(branch_id_for("repo--alice", "feature/x", None, Some(&existing)), "feature-x");
        assert_eq!(super::super::revisions::successor("feature-x", 2).as_deref(), Some("feature-x..2"));
        // Ownership is needed for both axes of the old collision, including deletion lookup.
        assert_eq!(branch_id_for("repo--bob", "feature/x", None, Some(&existing)), branch_change_id("repo--bob", "feature/x"));
        assert_eq!(branch_id_for("repo--alice", "feature-x", None, Some(&existing)), branch_change_id("repo--alice", "feature-x"));
        existing.branch = None;
        assert!(!branch_owned_by(&existing, "repo--alice", "feature/x"));
    }

    #[test]
    fn canonical_prs_keep_their_ids_across_revisions_and_successors() {
        let mut existing = req("repo--alice", "feature/x");
        existing.change_id = branch_change_id(&existing.fork, existing.branch.as_deref().unwrap());
        let canonical = existing.change_id.clone();
        let legacy = req("repo--alice", "feature/x");
        assert_eq!(branch_id_for("repo--alice", "feature/x", Some(&existing), Some(&legacy)), canonical);
        existing.rev = 3;
        existing.commit = Oid::from_hex(&"cd".repeat(20)).unwrap();
        assert_eq!(branch_id_for("repo--alice", "feature/x", Some(&existing), None), canonical);
        let successor = super::super::revisions::successor(&canonical, 2).unwrap();
        assert_ne!(successor, canonical);
        assert!(valid_change_id(&successor));
        assert!(branch_owned_by(&existing, "repo--alice", "feature/x"));
        assert!(!branch_owned_by(&existing, "repo--bob", "feature/x"));
    }
}
