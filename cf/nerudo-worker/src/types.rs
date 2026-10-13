//! Durable state of a trunk. Every value is stored as a JSON string in the Durable
//! Object's key-value storage (no SQL).

use nerudo_git::Oid;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Config {
    pub trunk: String,
    /// Smart-HTTP URL of the trunk repo, and the Authorization header that can push to it.
    /// Filled in by the Durable Object when it creates the trunk from a seed.
    #[serde(default)]
    pub remote: String,
    /// Empty for Artifacts: the Durable Object mints its own trunk write token at init.
    #[serde(default)]
    pub authorization: String,
    #[serde(default = "default_branch")]
    pub branch: String,
    pub host: HostCfg,
    /// Simulated CI time per batch, so measurements are comparable with the bench.
    #[serde(default)]
    pub ci_ms: u64,
    #[serde(default = "default_cap")]
    pub batch_cap: usize,
    /// Test hook: simulate this many crashes between a successful push and its landing record.
    #[serde(default)]
    pub debug_crash_after_push: u32,
}

fn default_branch() -> String {
    "refs/heads/main".into()
}

fn default_cap() -> usize {
    16
}

/// First commit of a trunk the Durable Object creates itself (Artifacts only): path -> text.
/// The object pushes it with the creation token and then revokes that token, so nothing
/// outside the object ever holds a credential that can write trunk.
#[derive(Clone, Debug, Deserialize)]
pub struct Seed {
    pub files: BTreeMap<String, String>,
    #[serde(default = "seed_message")]
    pub message: String,
}

fn seed_message() -> String {
    "seed\n".into()
}

/// Where agent forks come from. Artifacts in production; Forgejo stands in locally.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostCfg {
    Forgejo { api: String, owner: String, repo: String, token: String },
    Artifacts { binding: String, repo: String },
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Meta {
    pub head: Option<Oid>,
    pub seq: u64,
    pub prev_hash: String,
    pub batches: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LandReq {
    pub change_id: String,
    pub rev: u32,
    /// Fork name returned by POST /forks; its credentials stay in the Durable Object.
    pub fork: String,
    pub commit: Oid,
    pub base: Oid,
    /// The task (work item) this change is for, like T-12.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// The agent's context for this change: a commit on the fork's `nerudo/context` branch
    /// that holds `changes/<change>/rev-<n>.md`. The landed commit links to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_commit: Option<Oid>,
    /// The branch it was pushed as with plain git, like `feature/x` (the change id is the
    /// branch with each `/` as `-`). None: sent through the API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

impl LandReq {
    pub fn key(&self) -> String {
        format!("{}:{}", self.change_id, self.rev)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Queued,
    AwaitingSeal,
    AwaitingAllowance,
    /// One of a task's competing attempts: it ran CI as a dry run and waits until a person
    /// (or the task's pick policy) chooses which attempt ships.
    AwaitingPick,
    Landed,
    Ejected,
}

/// The receipt an agent waits for. `state == landed` with a `seq` is the only proof.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChangeRec {
    pub req: LandReq,
    pub state: State,
    pub seq: Option<u64>,
    pub trunk_commit: Option<Oid>,
    /// Structured reason for anything that isn't landed (machine-readable, never only a log line).
    pub reason: Option<serde_json::Value>,
    #[serde(default)]
    pub seal_reasons: Vec<String>,
    pub allowance: Option<String>,
    /// Approvers whose seals it landed under, by name.
    #[serde(default)]
    pub sealed_by: Vec<String>,
    pub submitted_ms: i64,
    /// When the change last came back to the queue after it waited for a person (an approval,
    /// an allowance or a pick). Queue age and landing latency count from here (trunk/metrics.rs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_ms: Option<i64>,
    pub landed_ms: Option<i64>,
    pub batch: Option<u64>,
    #[serde(default)]
    pub allocated: Vec<String>,
    /// Footprint units of the change, for backpressure.
    #[serde(default)]
    pub footprint: Vec<String>,
    /// CI runs in the batch it landed in: 1, or more when a change in that batch failed CI.
    #[serde(default)]
    pub ci_runs: u64,
    /// The first line of the agent's commit message, recorded when the change is gated.
    #[serde(default)]
    pub subject: String,
    /// What a decision model (`[decide]` in nerudo.toml) answered about this revision, and
    /// what the sequencer did with it. Asked once; kept for audit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<nerudo_core::decide::Decision>,
    /// The PR number: one counter per trunk, given when a change id is first sent and kept
    /// for every later revision. Records from before numbers have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<u64>,
    /// Files, added and deleted lines between the change's base and its commit, counted once
    /// when the change is gated, so lists can show the size without the diff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<Size>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub struct Size {
    pub files: u32,
    pub adds: u32,
    pub dels: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Fork {
    pub name: String,
    pub remote: String,
    pub authorization: String,
}

/// Written before a push, cleared atomically with the landing. If it survives a crash,
/// the next run compares trunk with `tip` to learn whether the push happened.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Attempt {
    pub batch: u64,
    pub start: Oid,
    pub tip: Oid,
    pub landed: Vec<Landing>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Landing {
    pub key: String,
    pub commit_in: Oid,
    pub commit_out: Oid,
    pub allocated: Vec<String>,
    pub allowance: Option<String>,
    #[serde(default)]
    pub sealed_by: Vec<String>,
}

/// An admin action that leaves a record: pausing the trunk, or a break-glass rules change.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AdminNote {
    pub reason: String,
    pub at_ms: i64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Stats {
    pub submitted: u64,
    pub landed: u64,
    pub batches: u64,
    pub batch_sizes: Vec<u32>,
    pub ci_runs: u64,
    pub ejected_conflict: u64,
    pub ejected_check: u64,
    pub ejected_other: u64,
    /// Changes ejected because the batch's CI run failed on them (type errors, or a crash).
    #[serde(default)]
    pub ejected_ci: u64,
    /// Competing attempts closed because another attempt for the same task was picked.
    #[serde(default)]
    pub not_picked: u64,
    /// Revisions closed because a newer revision of the same PR came (trunk/revisions.rs).
    /// Not closed PRs: each PR stays open with its newer revision.
    #[serde(default)]
    pub superseded: u64,
    pub awaiting_seal: u64,
    pub awaiting_allowance: u64,
    pub external_moves: u64,
    pub recovered_attempts: u64,
    /// HTTP requests the sequencer made to git hosts (a proxy for billed Artifacts operations).
    pub git_requests: u64,
    pub fetches: u64,
    /// Gate fetches from a fork that failed for a reason that can pass; the change stayed queued.
    #[serde(default)]
    pub fetch_retries: u64,
    pub pushes: u64,
    /// Pushes sent again with the same commit, after a push with no answer (trunk/repush.rs).
    #[serde(default)]
    pub pushes_again: u64,
    /// Trees and commits read one at a time through the Artifacts binding, and from this
    /// object's storage (trunk/objects.rs).
    #[serde(default)]
    pub object_reads: u64,
    #[serde(default)]
    pub objects_from_storage: u64,
}
