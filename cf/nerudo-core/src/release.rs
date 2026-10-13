//! Releases: a named, signed point on trunk.
//!
//! A release is an annotated tag `refs/tags/vX.Y.Z` on a trunk commit. The sequencer, the
//! only writer of trunk, pushes it, and only once approvers have signed a release statement
//! for that exact commit, tag and notes. The statement's fields that are not in the tag
//! object itself travel as trailers in the tag message, so anyone who can read trunk can
//! re-check a release without asking the sequencer: the signatures verify against the
//! approvers in nerudo.toml at the released commit, and together meet its release quorum.

use crate::auth::verify_any;
use crate::rules::{Quorum, Rules};
use nerudo_git::{Kind, Tag};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const RELEASED_BY: &str = "Released-By";
pub const RELEASE_SEQ: &str = "Release-Seq";
pub const PREVIOUS_RELEASE: &str = "Previous-Release";
pub const NOTES_DIGEST: &str = "Notes-Digest";
pub const PRERELEASE: &str = "Prerelease";

/// What an approver signs to release: this tag, on this commit, with these notes.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ReleaseSpec {
    pub trunk: String,
    /// Like "v1.2.0".
    pub tag: String,
    /// The trunk commit the tag points at.
    pub commit: String,
    /// The landing at that commit.
    pub seq: u64,
    pub title: String,
    /// SHA-256 of the release notes (see [`notes_digest`]).
    pub notes_digest: String,
    /// The release before this one, if any.
    #[serde(default)]
    pub previous: Option<String>,
    #[serde(default)]
    pub prerelease: bool,
    /// Digest of nerudo.toml at `commit`: its approvers and release quorum decide.
    pub rules_digest: String,
    pub expires_at_ms: i64,
    /// Hex public key of the approver.
    pub approver: String,
}

impl ReleaseSpec {
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut b = b"landseq-release-v1\n".to_vec();
        b.extend(serde_json::to_vec(self).expect("serialize"));
        b
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SignedRelease {
    pub spec: ReleaseSpec,
    pub signature: String,
}

/// Notes as they are stored and signed: trimmed, with \n line ends.
pub fn canonical_notes(notes: &str) -> String {
    notes.replace("\r\n", "\n").trim().to_string()
}

pub fn notes_digest(notes: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(canonical_notes(notes).as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// A release tag: "v" and a semantic version, with an optional pre-release suffix.
pub fn valid_tag(tag: &str) -> bool {
    tag.len() <= 64 && parse_version(tag).is_some()
}

/// "v1.2.3" or "v1.2.3-rc.1" -> (1, 2, 3, "rc.1").
pub fn parse_version(tag: &str) -> Option<(u64, u64, u64, Option<String>)> {
    let v = tag.strip_prefix('v')?;
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') => (c, Some(p.to_string())),
        Some(_) => return None,
        None => (v, None),
    };
    let n: Vec<u64> = core.split('.').map(|p| if p.is_empty() || (p.len() > 1 && p.starts_with('0')) { None } else { p.parse().ok() }).collect::<Option<_>>()?;
    let [a, b, c] = n[..] else { return None };
    Some((a, b, c, pre))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bump {
    Patch,
    Minor,
    Major,
}

/// A conventional commit's type and whether it breaks: "feat(api)!: x" -> ("feat", true).
pub fn conventional(subject: &str) -> Option<(String, bool)> {
    let (head, _) = subject.split_once(": ")?;
    let breaking = head.ends_with('!');
    let head = head.trim_end_matches('!');
    let kind = head.split('(').next().unwrap_or(head);
    if kind.is_empty() || !kind.chars().all(|c| c.is_ascii_alphabetic()) || (head.contains('(') && !head.ends_with(')')) {
        return None;
    }
    Some((kind.to_ascii_lowercase(), breaking))
}

/// How one merged change moves the version.
pub fn bump_of(subject: &str, body: &str) -> Bump {
    let breaking_note = body.lines().any(|l| l.starts_with("BREAKING CHANGE:") || l.starts_with("BREAKING-CHANGE:"));
    match conventional(subject) {
        Some((_, true)) => Bump::Major,
        _ if breaking_note => Bump::Major,
        Some((k, _)) if k == "feat" => Bump::Minor,
        _ => Bump::Patch,
    }
}

/// The next version after `previous`, given how the merged changes move it. The first
/// release is v0.1.0. Below 1.0 a breaking change bumps the minor version.
pub fn next_version(previous: Option<&str>, bumps: &[Bump]) -> String {
    let Some((a, b, c, pre)) = previous.and_then(parse_version) else {
        return "v0.1.0".into();
    };
    let bump = bumps.iter().copied().max().unwrap_or(Bump::Patch);
    // A pre-release becomes its release first.
    if pre.is_some() {
        return format!("v{a}.{b}.{c}");
    }
    match (bump, a) {
        (Bump::Major, 0) | (Bump::Minor, _) => format!("v{a}.{}.0", b + 1),
        (Bump::Major, _) => format!("v{}.0.0", a + 1),
        (Bump::Patch, _) => format!("v{a}.{b}.{}", c + 1),
    }
}

/// One merged change, as the notes list it.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct NoteLine {
    pub subject: String,
    /// How the notes name the PR: "#12", or the change id.
    pub pr: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub task: Option<String>,
    /// Approvers whose signature it needed before it merged.
    #[serde(default)]
    pub approved_by: Vec<String>,
    #[serde(default)]
    pub body: String,
}

/// The section a subject goes under.
pub fn section_of(subject: &str) -> &'static str {
    match conventional(subject).map(|(k, _)| k) {
        Some(k) if k == "feat" => "Features",
        Some(k) if k == "fix" => "Fixes",
        Some(k) if k == "docs" => "Docs",
        Some(k) if k == "refactor" || k == "perf" => "Refactors",
        _ => "Other",
    }
}

pub const SECTIONS: [&str; 5] = ["Features", "Fixes", "Docs", "Refactors", "Other"];

/// Files changed between two releases: (files, lines added, lines removed).
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub struct FileStats {
    pub files: usize,
    pub adds: u64,
    pub dels: u64,
}

/// Release notes in markdown, generated from the merged changes.
pub fn render_notes(tag: &str, previous: Option<&str>, lines: &[NoteLine], approvers: &[String], files: Option<FileStats>) -> String {
    let mut out = String::new();
    let item = |l: &NoteLine| {
        let mut s = format!("- {} ({})", l.subject, l.pr);
        if let Some(a) = &l.agent {
            s.push_str(&format!(" by {a}"));
        }
        if let Some(t) = &l.task {
            s.push_str(&format!(" · {t}"));
        }
        s
    };
    let breaking: Vec<&NoteLine> = lines.iter().filter(|l| bump_of(&l.subject, &l.body) == Bump::Major).collect();
    if !breaking.is_empty() {
        out.push_str("## Breaking changes\n\n");
        for l in &breaking {
            out.push_str(&item(l));
            out.push('\n');
        }
        out.push('\n');
    }
    for section in SECTIONS {
        let these: Vec<&NoteLine> = lines.iter().filter(|l| section_of(&l.subject) == section).collect();
        if these.is_empty() {
            continue;
        }
        out.push_str(&format!("## {section}\n\n"));
        for l in these {
            out.push_str(&item(l));
            out.push('\n');
        }
        out.push('\n');
    }
    if lines.is_empty() {
        out.push_str("No pull requests merged since the last release.\n\n");
    }
    let approved: Vec<&NoteLine> = lines.iter().filter(|l| !l.approved_by.is_empty()).collect();
    if !approved.is_empty() {
        out.push_str("## Approved changes\n\nThese needed a signature from a person before they merged.\n\n");
        for l in approved {
            out.push_str(&format!("- {} ({}) approved by {}\n", l.subject, l.pr, l.approved_by.join(", ")));
        }
        out.push('\n');
    }
    let agents: BTreeSet<&str> = lines.iter().filter_map(|l| l.agent.as_deref()).collect();
    out.push_str("## Contributors\n\n");
    if !agents.is_empty() {
        out.push_str(&format!("- Agents: {}\n", agents.into_iter().collect::<Vec<_>>().join(", ")));
    }
    let mut people: Vec<&str> = approvers.iter().map(String::as_str).collect();
    for l in lines {
        people.extend(l.approved_by.iter().map(String::as_str));
    }
    let people: BTreeSet<&str> = people.into_iter().collect();
    if !people.is_empty() {
        out.push_str(&format!("- Approvers: {}\n", people.into_iter().collect::<Vec<_>>().join(", ")));
    }
    out.push('\n');
    let range = match previous {
        Some(p) => format!("{p}...{tag}"),
        None => format!("{tag} (first release)"),
    };
    match files {
        Some(f) => out.push_str(&format!("**Full changelog**: {range} · {} files changed, +{} −{}\n", f.files, f.adds, f.dels)),
        None => out.push_str(&format!("**Full changelog**: {range}\n")),
    }
    canonical_notes(&out)
}

/// The tag message: the title, the notes, and the trailers that carry the rest of what
/// was signed.
pub fn tag_message(spec_title: &str, notes: &str, trailers: &[String]) -> String {
    let notes = canonical_notes(notes);
    let title = spec_title.lines().next().unwrap_or("").trim();
    let mut m = format!("{title}\n\n");
    if !notes.is_empty() {
        m.push_str(&notes);
        m.push_str("\n\n");
    }
    m.push_str(&trailers.join("\n"));
    m.push('\n');
    m
}

/// Trailers for a release signed by `signed` (name, spec, signature), sorted by name.
pub fn release_trailers(spec: &ReleaseSpec, signed: &[(String, ReleaseSpec, String)]) -> Vec<String> {
    let mut out = vec![format!("{RELEASE_SEQ}: {}", spec.seq), format!("{NOTES_DIGEST}: {}", spec.notes_digest)];
    if let Some(p) = &spec.previous {
        out.push(format!("{PREVIOUS_RELEASE}: {p}"));
    }
    if spec.prerelease {
        out.push(format!("{PRERELEASE}: yes"));
    }
    let mut s: Vec<&(String, ReleaseSpec, String)> = signed.iter().collect();
    s.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, sp, sig) in s {
        out.push(format!("{RELEASED_BY}: {name} {} {} {sig}", sp.approver, sp.expires_at_ms));
    }
    out
}

/// (title, notes) of a tag message written by [`tag_message`].
pub fn split_message(message: &str) -> (String, String) {
    let m = message.trim_end();
    let (title, rest) = m.split_once("\n\n").unwrap_or((m, ""));
    let notes = match rest.rsplit_once("\n\n") {
        Some((notes, _trailers)) => notes,
        None => "",
    };
    (title.trim().to_string(), canonical_notes(notes))
}

/// What a release tag carries, checked.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Released {
    pub tag: String,
    pub commit: String,
    pub seq: u64,
    pub signers: Vec<String>,
}

/// Approvers whose release signatures count, and whether they meet the quorum.
pub fn release_tally(rules: &Rules, specs: &[ReleaseSpec]) -> (Quorum, Vec<String>) {
    let q = rules.release_quorum();
    let mut have: Vec<String> = vec![];
    for a in specs.iter().filter_map(|s| rules.approver(&s.approver)) {
        if a.eligible(&q.roles) && !have.contains(&a.name) {
            have.push(a.name.clone());
        }
    }
    have.sort();
    (q, have)
}

/// Check a release tag from the outside: it points at a commit, its notes match their
/// digest, and every Released-By signature verifies against the approvers in `rules`
/// (nerudo.toml at the released commit) and together they meet the release quorum. No
/// signature may have expired by the tagger time.
pub fn verify_release(tag: &Tag, trunk: &str, rules: &Rules) -> Result<Released, String> {
    if tag.kind != Kind::Commit {
        return Err(format!("{} points at a {}, not a commit", tag.name, tag.kind.name()));
    }
    if !valid_tag(&tag.name) {
        return Err(format!("{} is not a release tag (vX.Y.Z)", tag.name));
    }
    let seq: u64 = tag.trailer(RELEASE_SEQ).and_then(|s| s.parse().ok()).ok_or_else(|| format!("{}: no {RELEASE_SEQ}", tag.name))?;
    let digest = tag.trailer(NOTES_DIGEST).ok_or_else(|| format!("{}: no {NOTES_DIGEST}", tag.name))?;
    let (title, notes) = split_message(&tag.message);
    if notes_digest(&notes) != digest {
        return Err(format!("{}: the notes don't match their digest", tag.name));
    }
    let when_ms = tag.tagger.as_ref().map(|t| t.when * 1000).unwrap_or(0);
    let digests = rules.digests();
    let mut specs = vec![];
    let mut signers = vec![];
    for line in tag.trailers(RELEASED_BY) {
        let f: Vec<&str> = line.split_whitespace().collect();
        let [name, key, expires, sig] = f[..] else {
            return Err(format!("malformed {RELEASED_BY}: {line}"));
        };
        let a = rules.approver(key).filter(|a| a.name == name).ok_or_else(|| format!("{name} is not an approver at the released commit"))?;
        let mut spec = ReleaseSpec {
            trunk: trunk.to_string(),
            tag: tag.name.clone(),
            commit: tag.object.to_hex(),
            seq,
            title: title.clone(),
            notes_digest: digest.clone(),
            previous: tag.trailer(PREVIOUS_RELEASE),
            prerelease: tag.trailer(PRERELEASE).is_some_and(|v| v == "yes"),
            rules_digest: String::new(),
            expires_at_ms: expires.parse().map_err(|_| format!("{name}: bad expiry"))?,
            approver: key.to_string(),
        };
        let signed = verify_any(key, sig, &digests, |d| ReleaseSpec { rules_digest: d.to_string(), ..spec.clone() }.signing_bytes());
        spec.rules_digest = signed.map_err(|e| format!("{name}'s release signature: {e}"))?;
        if when_ms >= spec.expires_at_ms {
            return Err(format!("{name}'s release signature had expired when the tag was made"));
        }
        signers.push(a.name.clone());
        specs.push(spec);
    }
    let (q, have) = release_tally(rules, &specs);
    if (have.len() as u32) < q.count {
        let who = if q.roles.is_empty() { "approvers".to_string() } else { q.roles.join("/") };
        return Err(format!("{} needs {} {who}, has {}", tag.name, q.count, if have.is_empty() { "none".into() } else { have.join(", ") }));
    }
    signers.sort();
    Ok(Released { tag: tag.name.clone(), commit: tag.object.to_hex(), seq, signers })
}

/// Group counts for a release's stats: how many distinct values `f` takes.
pub fn distinct<T, K: Ord>(items: &[T], f: impl Fn(&T) -> Option<K>) -> usize {
    items.iter().filter_map(f).collect::<BTreeSet<K>>().len()
}

/// Lines added and removed between two texts.
pub fn line_stats(old: &str, new: &str) -> (u64, u64) {
    let patch = diffy::create_patch(old, new);
    let (mut adds, mut dels) = (0u64, 0u64);
    for h in patch.hunks() {
        for l in h.lines() {
            match l {
                diffy::Line::Insert(_) => adds += 1,
                diffy::Line::Delete(_) => dels += 1,
                diffy::Line::Context(_) => {}
            }
        }
    }
    (adds, dels)
}
