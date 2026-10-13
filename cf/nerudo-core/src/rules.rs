//! `nerudo.toml` on trunk: how paths map to footprint units, which files are merged by
//! rule, regenerated, or allocated, which edits are seal boundaries, and who may approve
//! them. The approver list lives here, on trunk, so changing it is itself a governance
//! change that the current approvers must seal.

use crate::glob::matches;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Where the rules live on trunk.
pub const RULES_PATH: &str = "nerudo.toml";
pub const LEGACY_RULES_PATH: &str = "landseq.toml";

pub fn is_rules_path(path: &str) -> bool {
    path == RULES_PATH || path == LEGACY_RULES_PATH
}

/// Read either generation of rules. Two files are ambiguous and must never silently
/// change the authority of a trunk; migrate by replacing the old file in one commit.
pub fn rules_entry<T>(files: &BTreeMap<String, T>) -> Result<Option<(&str, &T)>, String> {
    match (files.get(RULES_PATH), files.get(LEGACY_RULES_PATH)) {
        (Some(_), Some(_)) => Err(format!("keep only one rules file: {RULES_PATH} or {LEGACY_RULES_PATH}")),
        (Some(e), None) => Ok(Some((RULES_PATH, e))),
        (None, Some(e)) => Ok(Some((LEGACY_RULES_PATH, e))),
        (None, None) => Ok(None),
    }
}

pub fn rules_file(store: &nerudo_git::Store, tree: &nerudo_git::Oid) -> nerudo_git::Result<Option<nerudo_git::FileEntry>> {
    let mut files = BTreeMap::new();
    for path in [RULES_PATH, LEGACY_RULES_PATH] {
        if let Some(e) = store.file(tree, path)? { files.insert(path.to_string(), e); }
    }
    rules_entry(&files).map(|e| e.map(|(_, e)| *e)).map_err(nerudo_git::Error::Protocol)
}

/// The kinds of seal reason (the text before the first `:`). `gates::seal_boundaries` emits
/// "governance", "ratchet", "irreversible" and "approval"; a decision model (`[decide]`) adds
/// "review" or "risk", never a path rule.
pub const SEAL_KINDS: [&str; 6] = ["governance", "ratchet", "irreversible", "approval", "review", "risk"];

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Rules {
    #[serde(default)]
    pub footprint: Footprint,
    #[serde(default)]
    pub commutative: Vec<Commutative>,
    #[serde(default)]
    pub derived: Vec<Derived>,
    #[serde(default)]
    pub allocate: Vec<Allocate>,
    #[serde(default)]
    pub seals: SealRules,
    #[serde(default)]
    pub checks: Vec<Check>,
    /// The humans whose keys may sign seals and allowances on this trunk.
    #[serde(default)]
    pub approvers: Vec<Approver>,
    #[serde(default)]
    pub allowances: AllowanceRules,
    /// The real part of each batch's CI run.
    #[serde(default)]
    pub ci: CiRules,
    /// A decision model that judges each change before it lands. Left out of the
    /// serialized form when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decide: Option<DecideRules>,
    /// Who must sign a release. Left out of the serialized form when absent; absent, a
    /// release needs one maintainer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub releases: Option<ReleaseRules>,
    // A new field needs a default. The canonical form leaves out a field at its default,
    // so the digest of a file that does not use the new field stays as it was.
}

/// `[releases]`: the quorum that signs a release tag.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ReleaseRules {
    pub require: Quorum,
}

/// `[decide]`: ask a decision model (typed probabilities over fixed options) how much
/// human attention a change needs before it lands. The model can only add a seal reason
/// ("review" or "risk"); it never removes one the path rules require. The thresholds live
/// here, on trunk, under a governance seal; the model only returns probabilities.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DecideRules {
    /// "clef" (Cloudflare's decision model on the AI binding), "workers-ai" (an LLM on the
    /// AI binding; it reports its own probabilities, which are not calibrated), or "fixed"
    /// (the probabilities below, for tests and demos).
    pub provider: String,
    /// The model to ask. Pin a version: thresholds are tuned against one.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub risk: RiskRules,
    /// For provider "fixed": the probability of each option (merge, review, sign).
    #[serde(default)]
    pub fixed: BTreeMap<String, f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RiskRules {
    /// Act on "review" or "sign" only when the model gives it at least this probability.
    #[serde(default = "default_threshold")]
    pub threshold: f64,
    /// What to do when no option reaches the threshold: "review" (ask a person) or "merge".
    #[serde(default = "default_review")]
    pub uncertain: String,
    /// What to do when the model can't be asked: "review" or "merge".
    #[serde(default = "default_review")]
    pub on_error: String,
    /// Only ask about changes that touch these paths. Empty: every change.
    #[serde(default)]
    pub paths: Vec<String>,
}

impl Default for RiskRules {
    fn default() -> Self {
        RiskRules { threshold: default_threshold(), uncertain: default_review(), on_error: default_review(), paths: vec![] }
    }
}

fn default_threshold() -> f64 {
    0.8
}

fn default_review() -> String {
    "review".into()
}

/// The options a risk decision chooses from, least friction first.
pub const RISK_OPTIONS: [&str; 3] = ["merge", "review", "sign"];
pub const DECIDE_PROVIDERS: [&str; 3] = ["clef", "workers-ai", "fixed"];

impl DecideRules {
    fn check(&self) -> Result<(), String> {
        if !DECIDE_PROVIDERS.contains(&self.provider.as_str()) {
            return Err(format!("provider {:?} must be one of {}", self.provider, DECIDE_PROVIDERS.join(", ")));
        }
        let r = &self.risk;
        if !(r.threshold > 0.0 && r.threshold <= 1.0) {
            return Err(format!("risk.threshold {} must be in (0, 1]", r.threshold));
        }
        for (k, v) in [("risk.uncertain", &r.uncertain), ("risk.on_error", &r.on_error)] {
            if !RISK_OPTIONS.contains(&v.as_str()) {
                return Err(format!("{k} {v:?} must be one of {}", RISK_OPTIONS.join(", ")));
            }
        }
        if self.provider == "fixed" {
            if let Some(k) = self.fixed.keys().find(|k| !RISK_OPTIONS.contains(&k.as_str())) {
                return Err(format!("fixed.{k}: options are {}", RISK_OPTIONS.join(", ")));
            }
            let sum: f64 = self.fixed.values().sum();
            if self.fixed.values().any(|p| !(0.0..=1.0).contains(p)) || (sum - 1.0).abs() > 0.01 {
                return Err(format!("fixed: probabilities must be in [0, 1] and sum to 1 (they sum to {sum:.3})"));
            }
        }
        Ok(())
    }

    /// Whether a change that edits `paths` is asked about.
    pub fn asks(&self, paths: &[String]) -> bool {
        self.risk.paths.is_empty() || paths.iter().any(|p| self.risk.paths.iter().any(|g| matches(g, p)))
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CiRules {
    /// `typecheck = { tsconfig = "tsconfig.json" }`: type-check the batch's tree with tsc
    /// (ts-rust in the nerudo-ci Worker). A change that fails it is ejected.
    #[serde(default)]
    pub typecheck: Option<Typecheck>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Typecheck {
    /// The tsconfig file, relative to the repo root.
    pub tsconfig: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Approver {
    pub name: String,
    /// Hex Ed25519 public key (`nerudo pubkey`). Empty: the person has not joined nerudo
    /// yet. They are a person of the repo and on its teams, but they can't sign.
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub roles: Vec<String>,
}

impl Approver {
    /// Whether this approver counts toward a quorum open to `roles` (empty: anyone).
    pub fn eligible(&self, roles: &[String]) -> bool {
        roles.is_empty() || self.roles.iter().any(|r| roles.contains(r))
    }
}

/// How many distinct approvers, holding which roles, must seal a change with one kind of
/// seal reason. A kind with no entry needs one seal from any approver.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Quorum {
    #[serde(default)]
    pub roles: Vec<String>,
    pub count: u32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct AllowanceRules {
    /// When true, ordinary changes land only under an allowance.
    #[serde(default)]
    pub required: bool,
    /// Roles that may sign allowances (empty: any approver).
    #[serde(default)]
    pub roles: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Footprint {
    /// Patterns like "targets/*": each match is one unit. Other paths are their own unit.
    #[serde(default)]
    pub units: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Commutative {
    pub path: String,
    /// Only "append" is implemented: lines the change appended are appended at landing.
    pub rule: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Derived {
    pub path: String,
    /// Built-in generator. The Worker can't run arbitrary commands, so generators are code.
    /// "digest-index": sorted contents of `source/*.txt` under a SHA-256 digest header.
    pub generator: String,
    #[serde(default)]
    pub source: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Allocate {
    pub dir: String,
    pub pending_prefix: String,
    pub width: usize,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SealRules {
    /// Paths whose edit needs a human seal (CI config, these rules, workflows).
    #[serde(default)]
    pub governance: Vec<String>,
    /// Test files whose existing tests must not be removed, skipped, or have assertions changed.
    #[serde(default)]
    pub tests: Vec<String>,
    /// "python-unittest" or "rust".
    #[serde(default)]
    pub test_style: String,
    /// SQL files where destructive statements need a seal.
    #[serde(default)]
    pub migrations: Vec<String>,
    /// Paths where any change waits for a person's approval, as a branch protection rule
    /// does: `approval = ["**"]` for every change.
    #[serde(default)]
    pub approval: Vec<String>,
    /// Seal reason kind -> quorum, e.g. `governance = { roles = ["maintainer"], count = 2 }`.
    #[serde(default)]
    pub require: BTreeMap<String, Quorum>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Check {
    /// "unique-numbering" (dir) | "no-pending" (dir, prefix) | "derived-consistent".
    pub kind: String,
    #[serde(default)]
    pub dir: String,
    #[serde(default)]
    pub prefix: String,
}

impl Rules {
    pub fn parse(text: &str) -> Result<Rules, String> {
        let r: Rules = toml::from_str(text).map_err(|e| format!("nerudo.toml: {e}"))?;
        if let Some(c) = r.commutative.iter().find(|c| c.rule != "append") {
            return Err(format!("{}: commutative rule {:?} is not implemented", c.path, c.rule));
        }
        if let Some(d) = r.derived.iter().find(|d| d.generator != "digest-index") {
            return Err(format!("{}: derived generator {:?} is not built in", d.path, d.generator));
        }
        r.check_approvers().map_err(|e| format!("nerudo.toml: {e}"))?;
        if let Some(d) = &r.decide {
            d.check().map_err(|e| format!("nerudo.toml: decide: {e}"))?;
        }
        if let Some(t) = &r.ci.typecheck {
            if t.tsconfig.is_empty() || t.tsconfig.starts_with('/') || t.tsconfig.split('/').any(|s| s.is_empty() || s == "." || s == "..") {
                return Err(format!("nerudo.toml: ci.typecheck.tsconfig {:?} must be a path inside the repo, like \"tsconfig.json\"", t.tsconfig));
            }
        }
        Ok(r)
    }

    fn check_approvers(&self) -> Result<(), String> {
        let (mut names, mut keys) = (BTreeSet::new(), BTreeSet::new());
        for a in &self.approvers {
            if a.name.is_empty() || !a.name.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) {
                return Err(format!("approver name {:?} must be [A-Za-z0-9._-]+", a.name));
            }
            if !a.key.is_empty() && (a.key.len() != 64 || !a.key.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))) {
                return Err(format!("approver {}: key must be 64 lowercase hex characters (nerudo pubkey), or left out", a.name));
            }
            if !names.insert(&a.name) || (!a.key.is_empty() && !keys.insert(&a.key)) {
                return Err(format!("approver {} is listed twice (same name or key)", a.name));
            }
        }
        for (kind, q) in &self.seals.require {
            if !SEAL_KINDS.contains(&kind.as_str()) {
                return Err(format!("seals.require.{kind}: kind must be one of {}", SEAL_KINDS.join(", ")));
            }
            let eligible = self.approvers.iter().filter(|a| a.eligible(&q.roles)).count();
            if q.count == 0 || q.count as usize > eligible {
                return Err(format!("seals.require.{kind}: count {} but {eligible} approvers can sign it", q.count));
            }
        }
        if let Some(r) = &self.releases {
            let eligible = self.approvers.iter().filter(|a| a.eligible(&r.require.roles)).count();
            if r.require.count == 0 || r.require.count as usize > eligible {
                return Err(format!("releases.require: count {} but {eligible} approvers can sign it", r.require.count));
            }
        }
        if let Some(r) = self.allowances.roles.iter().find(|r| !self.approvers.iter().any(|a| a.roles.contains(r))) {
            return Err(format!("allowances.roles: no approver holds {r:?}"));
        }
        Ok(())
    }

    pub fn approver(&self, key: &str) -> Option<&Approver> {
        self.approvers.iter().find(|a| !a.key.is_empty() && a.key == key)
    }

    /// The quorum for one kind of seal reason.
    pub fn quorum(&self, kind: &str) -> Quorum {
        self.seals.require.get(kind).cloned().unwrap_or(Quorum { roles: vec![], count: 1 })
    }

    /// Who must sign a release: `[releases] require`, or else one maintainer (one
    /// approver of any role when nobody holds "maintainer").
    pub fn release_quorum(&self) -> Quorum {
        if let Some(r) = &self.releases {
            return r.require.clone();
        }
        let maintainer = "maintainer".to_string();
        let roles = if self.approvers.iter().any(|a| a.roles.contains(&maintainer)) { vec![maintainer] } else { vec![] };
        Quorum { roles, count: 1 }
    }

    /// Paths a change never writes directly: merged by rule, regenerated, or allocated.
    pub fn is_special(&self, path: &str) -> bool {
        self.commutative.iter().any(|c| c.path == path)
            || self.derived.iter().any(|d| d.path == path)
            || self.pending(path).is_some()
    }

    /// If `path` is a pending allocation, the (rule, name without prefix).
    pub fn pending<'a>(&'a self, path: &'a str) -> Option<(&'a Allocate, &'a str)> {
        self.allocate.iter().find_map(|a| {
            let rest = path.strip_prefix(&a.dir)?.strip_prefix('/')?;
            rest.strip_prefix(&a.pending_prefix).map(|name| (a, name))
        })
    }

    pub fn unit_of(&self, path: &str) -> String {
        for pat in &self.footprint.units {
            if let Some(prefix) = pat.strip_suffix("/*") {
                if let Some(rest) = path.strip_prefix(prefix).and_then(|r| r.strip_prefix('/')) {
                    let child = rest.split('/').next().unwrap_or(rest);
                    return format!("{prefix}/{child}");
                }
            } else if matches(pat, path) {
                return pat.clone();
            }
        }
        path.to_string()
    }

    /// Stable digest of the rules: seals, allowances and release signatures bind to it, so
    /// editing the rules voids every outstanding approval. SHA-256 of [`Rules::canonical`].
    pub fn digest(&self) -> String {
        sha256_hex(&self.canonical())
    }

    /// The rules as canonical JSON: keys in byte order, no spaces, and no field at its
    /// default. A field is at its default when the JSON without it parses back to the same
    /// rules. So a field added later, with a default, leaves the digest of every file that
    /// does not use it as it was.
    pub fn canonical(&self) -> Vec<u8> {
        canonical_json(self)
    }

    /// Every digest an approval of these rules may carry: [`Rules::digest`] first, then the
    /// forms earlier nerudo versions signed ([`Legacy`]), so their approvals still verify.
    ///
    /// Accepting several forms is safe only because every form is computed from these same
    /// parsed rules, and a form counts only when its JSON parses back to exactly these
    /// rules. So each digest names these rules and no others: a signature over any of them
    /// approves the rules in force now, not an earlier or a later edit of them.
    pub fn digests(&self) -> Vec<String> {
        let full = serde_json::to_value(self).expect("rules serialize");
        let mut out = vec![self.digest()];
        for since in [3, 2, 1] {
            let json = serde_json::to_vec(&Legacy::of(self, since)).expect("rules serialize");
            let back = serde_json::from_slice::<Rules>(&json).ok().and_then(|r| serde_json::to_value(r).ok());
            let d = sha256_hex(&json);
            if back.as_ref() == Some(&full) && !out.contains(&d) {
                out.push(d);
            }
        }
        out
    }

    /// Whether an approval signed against `digest` approves these rules.
    pub fn accepts(&self, digest: &str) -> bool {
        self.digests().iter().any(|d| d == digest)
    }
}

/// `Rules` as `serde_json::to_vec` wrote it when the digest was the hash of that: every
/// field, in declaration order. A struct, not a `serde_json::Value`, because a `Value`
/// sorts the keys. `since` picks the shape:
/// - 1, e2523c3: approvers, allowances and `seals.require`.
/// - 2, e468cda: `ci`, written even when empty. 1ef0dd5 and 89781d4 added `decide` and
///   `releases`, written only when present.
/// - 3, 962ae9a: `seals.approval`. This is the shape of 46e1d5d too.
///
/// The nested structs serialize as they do now. Don't change that: the pinned digests in
/// tests/digest.rs fail if it changes.
#[derive(Serialize)]
struct Legacy<'a> {
    footprint: &'a Footprint,
    commutative: &'a [Commutative],
    derived: &'a [Derived],
    allocate: &'a [Allocate],
    seals: LegacySeals<'a>,
    checks: &'a [Check],
    approvers: &'a [Approver],
    allowances: &'a AllowanceRules,
    #[serde(skip_serializing_if = "Option::is_none")]
    ci: Option<&'a CiRules>,
    #[serde(skip_serializing_if = "Option::is_none")]
    decide: Option<&'a DecideRules>,
    #[serde(skip_serializing_if = "Option::is_none")]
    releases: Option<&'a ReleaseRules>,
}

#[derive(Serialize)]
struct LegacySeals<'a> {
    governance: &'a [String],
    tests: &'a [String],
    test_style: &'a str,
    migrations: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    approval: Option<&'a [String]>,
    require: &'a BTreeMap<String, Quorum>,
}

impl<'a> Legacy<'a> {
    fn of(r: &'a Rules, since: u8) -> Legacy<'a> {
        let s = &r.seals;
        Legacy {
            footprint: &r.footprint,
            commutative: &r.commutative,
            derived: &r.derived,
            allocate: &r.allocate,
            seals: LegacySeals {
                governance: &s.governance,
                tests: &s.tests,
                test_style: &s.test_style,
                migrations: &s.migrations,
                approval: (since >= 3).then_some(&s.approval[..]),
                require: &s.require,
            },
            checks: &r.checks,
            approvers: &r.approvers,
            allowances: &r.allowances,
            ci: (since >= 2).then_some(&r.ci),
            decide: r.decide.as_ref().filter(|_| since >= 2),
            releases: r.releases.as_ref().filter(|_| since >= 2),
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// `value` as canonical JSON (see [`Rules::canonical`]). Generic, so that a test can show
/// that a field added to `Rules` leaves the digest as it was.
pub fn canonical_json<T: Serialize + DeserializeOwned>(value: &T) -> Vec<u8> {
    let full = serde_json::to_value(value).expect("serialize");
    let same = |v: &Value| serde_json::from_value::<T>(v.clone()).ok().and_then(|t| serde_json::to_value(t).ok()).as_ref() == Some(&full);
    let mut v = full.clone();
    drop_defaults(&mut v, "", &same);
    let mut out = vec![];
    write_sorted(&v, &mut out);
    out
}

/// Remove each object key under the JSON pointer `at` whose removal keeps `same(root)`
/// true, and look inside each value that stays. List items stay: their position is data.
fn drop_defaults(root: &mut Value, at: &str, same: &dyn Fn(&Value) -> bool) {
    let (keys, object): (Vec<String>, bool) = match root.pointer(at) {
        Some(Value::Object(m)) => (m.keys().cloned().collect(), true),
        Some(Value::Array(a)) => ((0..a.len()).map(|i| i.to_string()).collect(), false),
        _ => return,
    };
    for k in keys {
        if object {
            let taken = root.pointer_mut(at).and_then(Value::as_object_mut).and_then(|m| m.remove(&k));
            if let Some(v) = taken {
                if same(root) {
                    continue;
                }
                root.pointer_mut(at).and_then(Value::as_object_mut).expect("object").insert(k.clone(), v);
            }
        }
        drop_defaults(root, &format!("{at}/{}", k.replace('~', "~0").replace('/', "~1")), same);
    }
}

/// JSON with each object's keys in byte order, whatever order serde_json's map keeps.
fn write_sorted(v: &Value, out: &mut Vec<u8>) {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push(b'{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                serde_json::to_writer(&mut *out, k).expect("write");
                out.push(b':');
                write_sorted(&m[k], out);
            }
            out.push(b'}');
        }
        Value::Array(a) => {
            out.push(b'[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_sorted(x, out);
            }
            out.push(b']');
        }
        _ => serde_json::to_writer(&mut *out, v).expect("write"),
    }
}
