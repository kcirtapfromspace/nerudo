//! Quorum: which seals a change needs, whether it has them, and the record of them that
//! lands on trunk as commit trailers.
//!
//! The trailers carry everything a seal's signature covers except the rules digest, which
//! is a digest of `nerudo.toml` at the landed commit's parent, in one of the forms
//! `Rules::digests` accepts. So anyone who can read trunk can re-check every landing
//! against the approvers and quorums in force when it landed, without asking the sequencer.

use crate::auth::{verify_any, Seal, SealSpec};
use crate::gates::seal_boundaries;
use crate::merge::ChangeInput;
use crate::rules::{Rules, RULES_PATH};
use nerudo_git::{Commit, Oid, Store};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const CHANGE_COMMIT: &str = "Change-Commit";
pub const SEAL_REASON: &str = "Seal-Reason";
pub const SEALED_BY: &str = "Sealed-By";
pub const BREAK_GLASS: &str = "Break-Glass";

/// Progress toward the quorum for one kind of seal reason.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Tally {
    pub kind: String,
    pub need: u32,
    /// Roles that count (empty: any approver).
    pub roles: Vec<String>,
    /// Approvers whose seals count, by name.
    pub have: Vec<String>,
}

impl Tally {
    pub fn met(&self) -> bool {
        self.have.len() as u32 >= self.need
    }
}

/// "governance: ci/check.py" -> "governance".
pub fn kind(reason: &str) -> &str {
    reason.split(':').next().unwrap_or(reason).trim()
}

/// One tally per kind of reason. `seals` must already be valid for this change; each
/// approver counts once per kind, however many seals they sent.
pub fn tally(rules: &Rules, reasons: &[String], seals: &[SealSpec]) -> Vec<Tally> {
    let kinds: BTreeSet<&str> = reasons.iter().map(|r| kind(r)).collect();
    kinds
        .into_iter()
        .map(|k| {
            let q = rules.quorum(k);
            let mut have: Vec<String> = vec![];
            for a in seals.iter().filter_map(|s| rules.approver(&s.approver)) {
                if a.eligible(&q.roles) && !have.contains(&a.name) {
                    have.push(a.name.clone());
                }
            }
            have.sort();
            Tally { kind: k.to_string(), need: q.count, roles: q.roles, have }
        })
        .collect()
}

pub fn met(tallies: &[Tally]) -> bool {
    tallies.iter().all(Tally::met)
}

/// Why `s` doesn't count toward this change's quorum now, or None if it does. The
/// signature itself is checked when the seal is submitted.
pub fn seal_problem(rules: &Rules, s: &SealSpec, change: &ChangeInput, reasons: &[String], now_ms: i64) -> Option<&'static str> {
    if s.change_id != change.change_id || s.rev != change.rev || s.commit != change.commit.to_hex() {
        Some("seal is for a different change or commit")
    } else if !rules.accepts(&s.rules_digest) {
        Some("seal was signed against different landing rules")
    } else if rules.approver(&s.approver).is_none() {
        Some("signer is not an approver in nerudo.toml")
    } else if now_ms >= s.expires_at_ms {
        Some("seal expired")
    } else if s.reasons != reasons {
        Some("seal names different reasons than the change has")
    } else {
        None
    }
}

/// Trailer lines for a change that lands under `seals`: the agent's commit, the reasons,
/// and one `Sealed-By: <name> <key> <expires_at_ms> <signature>` per approver whose seal
/// counts toward a quorum the change needs (a valid seal from someone without an eligible
/// role is left out, so trunk never overstates who approved).
pub fn seal_trailers(rules: &Rules, commit_in: &Oid, reasons: &[String], seals: &[Seal]) -> Vec<String> {
    let mut out = vec![format!("{CHANGE_COMMIT}: {}", commit_in.to_hex())];
    out.extend(reasons.iter().map(|r| format!("{SEAL_REASON}: {r}")));
    let specs: Vec<SealSpec> = seals.iter().map(|s| s.spec.clone()).collect();
    let counted: BTreeSet<String> = tally(rules, reasons, &specs).into_iter().flat_map(|t| t.have).collect();
    let mut signed: Vec<(&str, &Seal)> = seals
        .iter()
        .filter_map(|s| rules.approver(&s.spec.approver).map(|a| (a.name.as_str(), s)))
        .filter(|(name, _)| counted.contains(*name))
        .collect();
    signed.sort_by_key(|(name, _)| *name);
    for (name, s) in signed {
        out.push(format!("{SEALED_BY}: {name} {} {} {}", s.spec.approver, s.spec.expires_at_ms, s.signature));
    }
    out
}

/// What a landed commit needed and carried.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Landed {
    /// Kinds of seal reason the commit's own diff trips under its parent's rules.
    pub kinds: Vec<String>,
    /// Approvers whose seals on it verify.
    pub signers: Vec<String>,
    pub break_glass: Option<String>,
}

fn rules_at(store: &Store, commit: &Commit) -> Result<Rules, String> {
    let files = store.flatten(&commit.tree).map_err(|e| e.to_string())?;
    let (_, entry) = crate::rules::rules_entry(&files)?.ok_or_else(|| format!("no {RULES_PATH}"))?;
    let text = String::from_utf8_lossy(store.blob(&entry.oid).map_err(|e| e.to_string())?).into_owned();
    Rules::parse(&text)
}

/// Check one trunk commit from the outside: every seal it carries verifies against the
/// approvers in its parent's `nerudo.toml`, and together they meet the quorum those rules
/// set for what the commit changed. The reasons are recomputed from the commit's own diff
/// and compared by kind, since allocated files are renamed at landing.
pub fn verify_landing(store: &Store, trunk: &str, parent: &Oid, commit: &Oid) -> Result<Landed, String> {
    let c = store.commit(commit).map_err(|e| e.to_string())?;
    let p = store.commit(parent).map_err(|e| e.to_string())?;
    let rules = rules_at(store, &p).map_err(|e| format!("parent {}: {e}", parent.to_hex()))?;
    let change_id = c.trailer("Change-Id").unwrap_or_default();
    let rev: u32 = c.trailer("Revision").and_then(|r| r.parse().ok()).unwrap_or(0);
    let diff = ChangeInput { change_id: change_id.clone(), rev, commit: *commit, base: *parent, trailers: vec![] };
    let mut needed = seal_boundaries(store, &rules, &diff)?;
    // A decision model's reasons ("review", "risk") can't be recomputed from trunk, but a
    // landing that carries one must still have met that reason's quorum.
    needed.extend(c.trailers(SEAL_REASON).into_iter().filter(|r| matches!(kind(r), "review" | "risk")));
    let mut out = Landed { kinds: needed.iter().map(|r| kind(r).to_string()).collect::<BTreeSet<_>>().into_iter().collect(), ..Landed::default() };

    if let Some(reason) = c.trailer(BREAK_GLASS) {
        let pf = store.flatten(&p.tree).map_err(|e| e.to_string())?;
        let cf = store.flatten(&c.tree).map_err(|e| e.to_string())?;
        let keys: BTreeSet<&String> = pf.keys().chain(cf.keys()).collect();
        let changed: Vec<&&String> = keys.iter().filter(|k| pf.get(**k) != cf.get(**k)).collect();
        if changed.iter().any(|k| !crate::rules::is_rules_path(k)) {
            return Err(format!("{BREAK_GLASS} commit changes more than {RULES_PATH}"));
        }
        out.break_glass = Some(reason);
        return Ok(out);
    }

    let reasons = c.trailers(SEAL_REASON);
    let commit_in = c.trailer(CHANGE_COMMIT).unwrap_or_default();
    let digests = rules.digests();
    let mut specs = vec![];
    for line in c.trailers(SEALED_BY) {
        let f: Vec<&str> = line.split_whitespace().collect();
        let [name, key, expires, sig] = f[..] else {
            return Err(format!("malformed {SEALED_BY}: {line}"));
        };
        let a = rules.approver(key).filter(|a| a.name == name).ok_or_else(|| format!("{name} is not an approver at the parent"))?;
        let mut spec = SealSpec {
            trunk: trunk.to_string(),
            change_id: change_id.clone(),
            rev,
            commit: commit_in.clone(),
            rules_digest: String::new(),
            reasons: reasons.clone(),
            expires_at_ms: expires.parse().map_err(|_| format!("{name}: bad expiry"))?,
            approver: key.to_string(),
        };
        let signed = verify_any(key, sig, &digests, |d| SealSpec { rules_digest: d.to_string(), ..spec.clone() }.signing_bytes());
        spec.rules_digest = signed.map_err(|e| format!("{name}'s seal: {e}"))?;
        if c.committer.when * 1000 >= spec.expires_at_ms {
            return Err(format!("{name}'s seal had expired when it landed"));
        }
        out.signers.push(a.name.clone());
        specs.push(spec);
    }
    let signed: BTreeSet<&str> = reasons.iter().map(|r| kind(r)).collect();
    for t in tally(&rules, &needed, &specs) {
        if !t.met() || !signed.contains(t.kind.as_str()) {
            let who = if t.roles.is_empty() { "approvers".to_string() } else { t.roles.join("/") };
            return Err(format!("needs {} {who} for {}, has {}", t.need, t.kind, if t.have.is_empty() { "none".into() } else { t.have.join(", ") }));
        }
    }
    Ok(out)
}
