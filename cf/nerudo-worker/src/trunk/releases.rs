//! Releases: a named, signed point on trunk, cut by the sequencer.
//!
//! The sequencer is trunk's only writer, so it also writes the tags. A release starts as a
//! draft (the next version from the merged PRs' conventional-commit subjects, and notes
//! generated from them). An approver signs a release statement for that exact tag, commit
//! and notes with `nerudo release` (the key never leaves their terminal). Once the
//! signatures meet the release quorum in nerudo.toml at that commit, the sequencer pushes
//! an annotated tag `refs/tags/<tag>` (create-only) whose trailers carry the signatures, so
//! `nerudo verify` can re-check it from outside. Agents can propose a release; the
//! proposal lands in the approvers' inbox.
//!
//! Storage:
//!   rel:{tag}             a release
//!   rel_sig:{tag}:{key}   one approver's signature, until the quorum is met
//!   relp:{ms}{rand}       a release an agent or person proposed
//!
//! To mirror an upstream release at a given trunk commit (replay), find the landing seq
//! whose `commit_out` is that commit (`GET log`), then `GET releases/draft?at=<seq>&version=<tag>`
//! and sign it with `nerudo release --trunk T --at <seq> --version <tag> --yes`.

use super::social::Note;
use super::ui::{agent_of, MAX_FILE_BYTES};
use super::*;
use nerudo_core::release::{
    bump_of, canonical_notes, line_stats, next_version, notes_digest, release_tally, release_trailers, render_notes, tag_message, valid_tag, FileStats, NoteLine, ReleaseSpec,
    SignedRelease,
};
use nerudo_git::{Kind, Object, Tag};
use serde_json::{json, Value};

/// Files whose line counts a release or compare reads, at most.
const MAX_STAT_FILES: usize = 400;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Signer {
    pub name: String,
    pub key: String,
    pub expires_at_ms: i64,
    pub signature: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(super) struct ReleaseStats {
    pub prs: usize,
    pub agents: usize,
    pub batches: usize,
    pub ci_runs: u64,
    /// Tasks done with a PR in this release.
    pub tasks_closed: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct ReleaseChange {
    pub seq: u64,
    pub change_id: String,
    /// How the notes name it: "#12", or the change id.
    #[serde(default)]
    pub pr: String,
    pub rev: u32,
    pub subject: String,
    pub agent: Option<String>,
    pub task: Option<String>,
    pub approved_by: Vec<String>,
    pub commit: String,
    pub section: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Release {
    pub tag: String,
    pub commit: String,
    pub seq: u64,
    pub title: String,
    pub notes: String,
    pub previous: Option<String>,
    pub prerelease: bool,
    pub created_ms: i64,
    pub released_by: Vec<Signer>,
    /// The annotated tag object on Artifacts.
    pub tag_object: String,
    pub stats: ReleaseStats,
    pub changes: Vec<ReleaseChange>,
    #[serde(default)]
    pub files: Option<FileStats>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Proposal {
    pub at_ms: i64,
    pub by: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

/// What `nerudo release` signs, and what the app shows before anyone signs.
#[derive(Clone, Debug, Serialize)]
pub(super) struct Draft {
    pub tag: String,
    pub previous: Option<String>,
    pub commit: String,
    pub seq: u64,
    pub title: String,
    pub notes: String,
    pub notes_digest: String,
    pub rules_digest: String,
    pub quorum: Value,
    pub stats: ReleaseStats,
    pub changes: Vec<ReleaseChange>,
    pub files: FileStats,
    pub command: String,
}

#[derive(Deserialize)]
pub(super) struct ReleaseReq {
    pub spec: ReleaseSpec,
    pub signature: String,
    pub notes: String,
}

#[derive(Deserialize)]
pub(super) struct ProposeReq {
    pub by: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

/// A storage key suffix: the time, then a random tail, so keys sort by time.
fn stamp_key(ms: i64) -> String {
    format!("{ms:013}{:04x}", (js_sys::Math::random() * 65536.0) as u32)
}

/// How the notes name a PR: its number, or its change id for PRs from before numbers.
fn pr_label(rec: &ChangeRec) -> String {
    match rec.number {
        Some(n) => format!("#{n}"),
        None => rec.req.change_id.clone(),
    }
}

/// A commit's subject without the "a01c3: " the change id already shows.
fn bare_subject(cid: &str, s: &str) -> String {
    s.strip_prefix(&format!("{cid}: ")).unwrap_or(s).to_string()
}

impl Trunk {
    /// Every release, newest first.
    pub(super) async fn releases(&self) -> Result<Vec<Release>> {
        let mut v: Vec<Release> = self.list_prefix("rel:").await?.into_iter().filter_map(|(_, v)| serde_json::from_str(&v).ok()).collect();
        v.sort_by_key(|r| std::cmp::Reverse((r.seq, r.created_ms)));
        Ok(v)
    }

    /// Releases as the app's snapshot carries them: enough for the graph and the repo tab.
    pub(super) async fn release_marks(&self) -> Result<Value> {
        let v: Vec<Value> = self
            .releases()
            .await?
            .iter()
            .map(|r| {
                let by: Vec<&str> = r.released_by.iter().map(|s| s.name.as_str()).collect();
                json!({ "tag": r.tag, "seq": r.seq, "commit": r.commit, "title": r.title, "created_ms": r.created_ms, "prerelease": r.prerelease, "prs": r.stats.prs, "by": by })
            })
            .collect();
        Ok(Value::Array(v))
    }

    /// The draft of the next release at landing `at` (the trunk tip when None).
    pub(super) async fn release_draft(&self, cfg: &Config, at: Option<u64>, version: Option<&str>) -> Result<std::result::Result<Draft, String>> {
        let meta: Meta = self.get("meta").await?.unwrap_or_default();
        let seq = at.unwrap_or(meta.seq);
        if seq == 0 || seq > meta.seq {
            return Ok(Err(if meta.seq == 0 { "nothing has merged yet".into() } else { format!("there is no landing {seq}; the last is {}", meta.seq) }));
        }
        let previous = self.releases().await?.into_iter().next();
        if let Some(p) = &previous {
            if at.is_none() && seq == p.seq {
                return Ok(Err(format!("Nothing has merged since {}. The next release can be drafted once a PR merges.", p.tag)));
            }
            if seq <= p.seq {
                return Ok(Err(format!("{} is at landing {}; a new release must be at a later landing than {seq}", p.tag, p.seq)));
            }
        }
        let Some(at_entry) = self.get::<LogEntry>(&format!("log:{seq:012}")).await? else {
            return Ok(Err(format!("no landing log entry {seq}")));
        };
        let commit = Oid::from_hex(&at_entry.commit_out).map_err(g)?;
        let from = previous.as_ref().map(|p| p.seq + 1).unwrap_or(1);

        let store = self.ui_store(cfg, meta.head).await?;
        let out = self.draft_with(cfg, &store, seq, from, commit, previous.as_ref(), version).await;
        self.ui_store_back(store);
        out
    }

    async fn draft_with(&self, cfg: &Config, store: &Store, seq: u64, from: u64, commit: Oid, previous: Option<&Release>, version: Option<&str>) -> Result<std::result::Result<Draft, String>> {
        let rules = match read_rules(store, &commit) {
            Ok(r) => r,
            Err(e) => return Ok(Err(format!("nerudo.toml at {}: {e}", commit.to_hex()))),
        };
        let mut changes = vec![];
        let mut lines = vec![];
        let mut bumps = vec![];
        let mut batch_ci: BTreeMap<u64, u64> = BTreeMap::new();
        for s in from..=seq {
            let Some(e) = self.get::<LogEntry>(&format!("log:{s:012}")).await? else { continue };
            let rec = self.rec(&e.change_id, e.rev).await?;
            let c = Oid::from_hex(&e.commit_out).ok().and_then(|o| store.commit(&o).ok());
            let subject = rec.as_ref().map(|r| r.subject.clone()).filter(|s| !s.is_empty()).or_else(|| c.as_ref().map(|c| c.message.lines().next().unwrap_or("").to_string())).unwrap_or_default();
            let subject = bare_subject(&e.change_id, &subject);
            let body = c.as_ref().map(|c| c.body().to_string()).unwrap_or_default();
            let agent = rec.as_ref().map(|r| agent_of(&cfg.trunk, &r.req.fork).to_string());
            let approved_by: Vec<String> = c.as_ref().map(|c| c.trailers(nerudo_core::quorum::SEALED_BY)).unwrap_or_default().iter().filter_map(|l| l.split_whitespace().next().map(str::to_string)).collect();
            let task = rec.as_ref().and_then(|r| r.req.task.clone());
            bumps.push(bump_of(&subject, &body));
            batch_ci.insert(e.batch, rec.as_ref().map(|r| r.ci_runs).unwrap_or(1).max(*batch_ci.get(&e.batch).unwrap_or(&0)));
            let pr = rec.as_ref().map(pr_label).unwrap_or_else(|| e.change_id.clone());
            lines.push(NoteLine { subject: subject.clone(), pr: pr.clone(), agent: agent.clone(), task: task.clone(), approved_by: approved_by.clone(), body });
            changes.push(ReleaseChange {
                seq: s,
                change_id: e.change_id.clone(),
                pr,
                rev: e.rev,
                section: nerudo_core::release::section_of(&subject).to_string(),
                subject,
                agent,
                task,
                approved_by,
                commit: e.commit_out.clone(),
            });
        }
        let tag = match version {
            Some(v) if !valid_tag(v) => return Ok(Err(format!("{v:?} is not a release tag; use vMAJOR.MINOR.PATCH, like v0.2.0"))),
            Some(v) => v.to_string(),
            None => next_version(previous.map(|p| p.tag.as_str()), &bumps),
        };
        if self.get::<Release>(&format!("rel:{tag}")).await?.is_some() {
            return Ok(Err(format!("{tag} is already released")));
        }
        let files = match previous.and_then(|p| Oid::from_hex(&p.commit).ok()) {
            Some(old) => file_stats(store, &old, &commit).unwrap_or_default(),
            None => file_stats_root(store, &commit).unwrap_or_default(),
        };
        let notes = render_notes(&tag, previous.map(|p| p.tag.as_str()), &lines, &[], Some(files));
        let tasks_closed: Vec<String> = {
            let ids: BTreeSet<&str> = changes.iter().map(|c| c.change_id.as_str()).collect();
            self.tasks().await?.into_iter().filter(|t| t.status == "done" && t.changes.iter().any(|c| ids.contains(c.as_str()))).map(|t| t.id).collect()
        };
        let stats = ReleaseStats {
            prs: changes.len(),
            agents: changes.iter().filter_map(|c| c.agent.as_deref()).collect::<BTreeSet<_>>().len(),
            batches: batch_ci.len(),
            ci_runs: batch_ci.values().sum(),
            tasks_closed,
        };
        let q = rules.release_quorum();
        let eligible: Vec<String> = rules.approvers.iter().filter(|a| a.eligible(&q.roles)).map(|a| a.name.clone()).collect();
        Ok(Ok(Draft {
            command: format!("nerudo release --trunk {} --at {seq} --version {tag}", cfg.trunk),
            title: tag.clone(),
            notes_digest: notes_digest(&notes),
            tag,
            previous: previous.map(|p| p.tag.clone()),
            commit: commit.to_hex(),
            seq,
            notes,
            rules_digest: rules.digest(),
            quorum: json!({ "roles": q.roles, "count": q.count, "eligible": eligible }),
            stats,
            changes,
            files,
        }))
    }

    /// POST releases: one approver's signed release statement. Once the signatures meet
    /// the release quorum, the sequencer pushes the tag.
    pub(super) async fn add_release(&self, r: ReleaseReq) -> Result<Response> {
        let cfg = self.cfg().await?;
        let s = &r.spec;
        if s.trunk != cfg.trunk {
            return bad(403, "signed for a different trunk");
        }
        if !valid_tag(&s.tag) {
            return bad(400, "the tag must be vMAJOR.MINOR.PATCH, like v0.2.0");
        }
        if self.get::<Release>(&format!("rel:{}", s.tag)).await?.is_some() {
            return bad(409, format!("{} is already released", s.tag));
        }
        let title = s.title.trim();
        if title.is_empty() || title.contains('\n') || title.chars().count() > 200 {
            return bad(400, "the title is one line of at most 200 characters");
        }
        let notes = canonical_notes(&r.notes);
        if notes.len() > 64 * 1024 {
            return bad(400, "the notes are longer than 64 KiB");
        }
        if notes_digest(&notes) != s.notes_digest {
            return bad(400, "the notes don't match the digest that was signed");
        }
        let meta: Meta = self.get("meta").await?.unwrap_or_default();
        let Some(entry) = self.get::<LogEntry>(&format!("log:{:012}", s.seq)).await? else {
            return bad(404, format!("there is no landing {}", s.seq));
        };
        if entry.commit_out != s.commit || s.seq > meta.seq {
            return bad(409, format!("landing {} is commit {}, not {}", s.seq, entry.commit_out, s.commit));
        }
        let latest = self.releases().await?.into_iter().next();
        if latest.as_ref().map(|l| &l.tag) != s.previous.as_ref() {
            return bad(409, format!("the previous release is {}, not {}", latest.as_ref().map(|l| l.tag.as_str()).unwrap_or("none"), s.previous.as_deref().unwrap_or("none")));
        }
        if latest.as_ref().is_some_and(|l| s.seq <= l.seq) {
            return bad(409, "a release must be at a later landing than the last one");
        }
        let commit = Oid::from_hex(&s.commit).map_err(g)?;
        self.warm_ui(|| async {
            if let Ok(st) = self.ui_store(&cfg, meta.head).await {
                let _ = read_rules(&st, &commit);
            }
        })
        .await?;
        let store = self.ui_store(&cfg, meta.head).await?;
        let rules = read_rules(&store, &commit);
        let rules = match rules {
            Ok(r) => r,
            Err(e) => {
                self.ui_store_back(store);
                return bad(409, format!("nerudo.toml at the release commit: {e}"));
            }
        };
        let who = match rules.approver(&s.approver) {
            Some(a) => a.name.clone(),
            None => {
                self.ui_store_back(store);
                return bad(403, "signer is not an approver in nerudo.toml at the release commit");
            }
        };
        let accepted = rules.digests();
        let checked = if !accepted.contains(&s.rules_digest) {
            Err("signed against different rules than nerudo.toml at the release commit".to_string())
        } else if now() as i64 >= s.expires_at_ms {
            Err("the release signature has expired".to_string())
        } else {
            auth::verify(&s.approver, &s.signing_bytes(), &r.signature)
        };
        if let Err(e) = checked {
            self.ui_store_back(store);
            return bad(403, e);
        }
        self.put(&format!("rel_sig:{}:{}", s.tag, s.approver), &SignedRelease { spec: s.clone(), signature: r.signature.clone() }).await?;

        // The signatures that count: same statement (bar signer, expiry and the form of the
        // rules digest), still valid.
        let mut signed: Vec<(String, ReleaseSpec, String)> = vec![];
        for (_, v) in self.list_prefix(&format!("rel_sig:{}:", s.tag)).await? {
            let Ok(o) = serde_json::from_str::<SignedRelease>(&v) else { continue };
            let same = ReleaseSpec { approver: s.approver.clone(), expires_at_ms: s.expires_at_ms, rules_digest: s.rules_digest.clone(), ..o.spec.clone() } == *s;
            if same && accepted.contains(&o.spec.rules_digest) && (now() as i64) < o.spec.expires_at_ms {
                if let Some(a) = rules.approver(&o.spec.approver) {
                    signed.push((a.name.clone(), o.spec, o.signature));
                }
            }
        }
        let specs: Vec<ReleaseSpec> = signed.iter().map(|(_, s, _)| s.clone()).collect();
        let (q, have) = release_tally(&rules, &specs);
        let quorum = json!({ "roles": q.roles, "need": q.count, "have": have });
        if (have.len() as u32) < q.count {
            self.ui_store_back(store);
            return Response::from_json(&json!({ "tag": s.tag, "by": who, "quorum": quorum, "complete": false }));
        }
        signed.retain(|(name, _, _)| have.contains(name));

        // Cut it: the annotated tag, pushed create-only.
        let previous = latest.as_ref();
        let draft = self.draft_with(&cfg, &store, s.seq, previous.map(|p| p.seq + 1).unwrap_or(1), commit, previous, Some(&s.tag)).await?;
        self.ui_store_back(store);
        let draft = match draft {
            Ok(d) => d,
            Err(e) => return bad(409, e),
        };
        let tag = Tag {
            object: commit,
            kind: Kind::Commit,
            name: s.tag.clone(),
            tagger: Some(Signature::new("nerudo", &format!("sequencer@{}.nerudo.invalid", cfg.trunk), now() as i64 / 1000)),
            message: tag_message(title, &notes, &release_trailers(s, &signed)),
        };
        let obj = Object::new(Kind::Tag, tag.serialize());
        let oid = obj.oid();
        let t = FetchTransport::default();
        let remote = Remote::new(&cfg.remote, Some(cfg.authorization.clone()));
        let refname = format!("refs/tags/{}", s.tag);
        let report = push(&t, &remote, &[RefUpdate { name: refname.clone(), old: Oid::ZERO, new: oid }], write_pack(&[&obj])).await;
        match report {
            Ok(rep) if rep.all_ok() => {}
            Ok(rep) => return bad(409, format!("the host refused {refname}: {:?}", rep.refs)),
            Err(e) => return bad(502, format!("pushing {refname}: {e}")),
        }
        let rel = Release {
            tag: s.tag.clone(),
            commit: s.commit.clone(),
            seq: s.seq,
            title: title.to_string(),
            notes,
            previous: s.previous.clone(),
            prerelease: s.prerelease,
            created_ms: now() as i64,
            released_by: signed.iter().map(|(name, sp, sig)| Signer { name: name.clone(), key: sp.approver.clone(), expires_at_ms: sp.expires_at_ms, signature: sig.clone() }).collect(),
            tag_object: oid.to_hex(),
            stats: draft.stats,
            changes: draft.changes,
            files: Some(draft.files),
        };
        self.put(&format!("rel:{}", rel.tag), &rel).await?;
        for (k, _) in self.list_prefix(&format!("rel_sig:{}:", s.tag)).await? {
            self.state.storage().delete(&k).await?;
        }
        for (k, _) in self.list_prefix("relp:").await? {
            self.state.storage().delete(&k).await?;
        }
        let names: Vec<&str> = rel.released_by.iter().map(|x| x.name.as_str()).collect();
        self.event("release", format!("{} released at landing {} ({} PRs), signed by {}", rel.tag, rel.seq, rel.stats.prs, names.join(", ")), None, None, Some(&who)).await?;
        Response::from_json(&json!({ "tag": rel.tag, "by": who, "quorum": quorum, "complete": true, "release": rel }))
    }

    /// POST releases/propose: an agent asks the approvers for a release. `by` is that agent,
    /// with its key (agents.rs); the admin token names anyone.
    pub(super) async fn propose_release(&self, caller: &super::agents::Caller, r: ProposeReq) -> Result<Response> {
        let by: String = r.by.chars().filter(|c| c.is_ascii_alphanumeric() || "._-@".contains(*c)).take(64).collect();
        if by.is_empty() {
            return bad(400, "by: who proposes it");
        }
        if !caller.admin || caller.agent.is_some() {
            if let Err((status, e)) = self.acting_agent(caller, "by", &by).await? {
                return bad(status, e);
            }
        }
        if let Some(v) = r.version.as_deref().filter(|v| !valid_tag(v)) {
            return bad(400, format!("{v:?} is not a release tag; use vMAJOR.MINOR.PATCH"));
        }
        let cfg = self.cfg().await?;
        // On a lazy trunk, read what the draft needs first (trunk/objects.rs).
        self.warm_ui(|| self.release_draft(&cfg, None, r.version.as_deref())).await?;
        let draft = match self.release_draft(&cfg, None, r.version.as_deref()).await? {
            Ok(d) => d,
            Err(e) => return bad(409, e),
        };
        let p = Proposal {
            at_ms: now() as i64,
            by: by.clone(),
            version: Some(draft.tag.clone()),
            title: r.title.map(|t| t.lines().next().unwrap_or("").chars().take(200).collect()),
            notes: r.notes.map(|n| canonical_notes(&n).chars().take(16 * 1024).collect()),
        };
        self.put(&format!("relp:{}", stamp_key(p.at_ms)), &p).await?;
        let rules = self.rules().await?;
        let q = rules.release_quorum();
        let text = format!("proposes {} at landing {} ({} PRs){}", draft.tag, draft.seq, draft.stats.prs, p.title.as_deref().map(|t| format!(": {t}")).unwrap_or_default());
        let mut told = vec![];
        for a in rules.approvers.iter().filter(|a| a.eligible(&q.roles)) {
            let note = Note { kind: "release_proposal".into(), change_id: format!("release:{}", draft.tag), from: by.clone(), team: None, text: text.clone(), at_ms: p.at_ms };
            self.put(&format!("nt:{}:{}", a.name, stamp_key(p.at_ms)), &note).await?;
            told.push(a.name.clone());
        }
        self.event("release", format!("{by} {text}"), None, None, Some(&by)).await?;
        Response::from_json(&json!({ "proposed": draft.tag, "seq": draft.seq, "prs": draft.stats.prs, "notified": told }))
    }

    // ------------------------------------------------------------ the agent API

    /// GET releases
    pub(super) async fn api_release_list(&self) -> Result<Response> {
        let rels = self.releases().await?;
        let v: Vec<Value> = rels.iter().enumerate().map(|(i, r)| release_json(r, i == 0)).collect();
        Response::from_json(&json!({ "releases": v, "proposals": self.release_proposals().await? }))
    }

    /// GET releases/draft[?at=<seq>&version=<tag>]
    pub(super) async fn api_release_draft(&self, req: &Request) -> Result<Response> {
        let url = req.url()?;
        let q: BTreeMap<String, String> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        let at = match q.get("at") {
            Some(a) => match a.parse::<u64>() {
                Ok(n) => Some(n),
                Err(_) => return bad(400, "at: a landing number"),
            },
            None => None,
        };
        let cfg = self.cfg().await?;
        match self.release_draft(&cfg, at, q.get("version").map(String::as_str).filter(|v| !v.is_empty())).await? {
            Ok(d) => Response::from_json(&d),
            Err(e) => bad(409, e),
        }
    }

    /// GET releases/{tag}
    pub(super) async fn api_release(&self, tag: &str) -> Result<Response> {
        let rels = self.releases().await?;
        match rels.iter().position(|r| r.tag == tag) {
            Some(i) => Response::from_json(&release_json(&rels[i], i == 0)),
            None => bad(404, format!("no release {tag}")),
        }
    }

    /// Proposals not yet released, newest first.
    pub(super) async fn release_proposals(&self) -> Result<Vec<Proposal>> {
        let mut v: Vec<Proposal> = self.list_prefix("relp:").await?.into_iter().filter_map(|(_, v)| serde_json::from_str(&v).ok()).collect();
        v.sort_by_key(|p| std::cmp::Reverse(p.at_ms));
        Ok(v)
    }

    /// Files changed between two releases (or a release and the tip): path, status, lines.
    pub(super) async fn release_compare(&self, cfg: &Config, range: &str) -> Result<Response> {
        let Some((a, b)) = range.split_once("...") else {
            return bad(400, "compare takes A...B, like v0.1.0...v0.2.0");
        };
        let meta: Meta = self.get("meta").await?.unwrap_or_default();
        let resolve = |name: &str, rels: &[Release]| -> Option<Oid> {
            if name == "main" || name == "HEAD" {
                return meta.head;
            }
            rels.iter().find(|r| r.tag == name).and_then(|r| Oid::from_hex(&r.commit).ok())
        };
        let rels = self.releases().await?;
        let (Some(old), Some(new)) = (resolve(a, &rels), resolve(b, &rels)) else {
            return bad(404, "compare names a release tag or main on each side");
        };
        let store = self.ui_store(cfg, meta.head).await?;
        let out = compare_json(&store, &old, &new);
        self.ui_store_back(store);
        match out {
            Ok(mut v) => {
                v["base"] = json!(a);
                v["head"] = json!(b);
                Response::from_json(&v)
            }
            Err(e) => bad(500, e),
        }
    }
}

fn text_of(store: &Store, e: Option<&FileEntry>) -> Option<String> {
    let e = e?;
    let b = store.blob(&e.oid).ok()?;
    if b.len() > MAX_FILE_BYTES || b.contains(&0) {
        return None;
    }
    Some(String::from_utf8_lossy(b).into_owned())
}

/// The files that differ between two commits, with line counts for the first few hundred.
fn compare_files(store: &Store, old: &Oid, new: &Oid) -> std::result::Result<Vec<(String, &'static str, Option<(u64, u64)>)>, String> {
    let tree = |c: &Oid| -> std::result::Result<Oid, String> { store.commit(c).map(|c| c.tree).map_err(|e| e.to_string()) };
    let (a, b) = super::views::changed_files(store, Some(&tree(old)?), &tree(new)?).map_err(|e| e.to_string())?;
    let paths: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    let mut out = vec![];
    for p in paths {
        let (x, y) = (a.get(p), b.get(p));
        if x == y {
            continue;
        }
        let status = match (x, y) {
            (None, Some(_)) => "added",
            (Some(_), None) => "deleted",
            _ => "changed",
        };
        let lines = if out.len() < MAX_STAT_FILES {
            match (text_of(store, x), text_of(store, y)) {
                (Some(o), Some(n)) => Some(line_stats(&o, &n)),
                (None, Some(n)) if x.is_none() => Some(line_stats("", &n)),
                (Some(o), None) if y.is_none() => Some(line_stats(&o, "")),
                _ => None,
            }
        } else {
            None
        };
        out.push((p.clone(), status, lines));
    }
    Ok(out)
}

fn file_stats(store: &Store, old: &Oid, new: &Oid) -> std::result::Result<FileStats, String> {
    let files = compare_files(store, old, new)?;
    let (adds, dels) = files.iter().filter_map(|(_, _, l)| *l).fold((0, 0), |(a, d), (x, y)| (a + x, d + y));
    Ok(FileStats { files: files.len(), adds, dels })
}

/// The first release: what the commits since the first one on trunk changed.
fn file_stats_root(store: &Store, new: &Oid) -> std::result::Result<FileStats, String> {
    let mut root = *new;
    while let Some(p) = store.commit(&root).map_err(|e| e.to_string())?.parents.first().copied() {
        if !store.contains(&p) {
            break; // a shallow history ends here
        }
        root = p;
    }
    file_stats(store, &root, new)
}

fn compare_json(store: &Store, old: &Oid, new: &Oid) -> std::result::Result<Value, String> {
    let files = compare_files(store, old, new)?;
    let (adds, dels) = files.iter().filter_map(|(_, _, l)| *l).fold((0u64, 0u64), |(a, d), (x, y)| (a + x, d + y));
    let list: Vec<Value> = files.iter().map(|(p, s, l)| json!({ "path": p, "status": s, "adds": l.map(|x| x.0), "dels": l.map(|x| x.1) })).collect();
    Ok(json!({ "files": list, "total": { "files": files.len(), "adds": adds, "dels": dels }, "base_commit": old.to_hex(), "head_commit": new.to_hex() }))
}

/// A release as the API returns it.
pub(super) fn release_json(r: &Release, latest: bool) -> Value {
    let mut v = serde_json::to_value(r).unwrap_or(Value::Null);
    v["latest"] = json!(latest);
    v
}
