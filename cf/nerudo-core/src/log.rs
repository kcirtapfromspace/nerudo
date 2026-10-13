//! The landing log: append-only, hash-chained. Each entry commits to the previous one, so
//! anyone holding a later hash can detect a rewritten history.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct LogEntry {
    pub seq: u64,
    pub change_id: String,
    pub rev: u32,
    pub commit_in: String,
    pub commit_out: String,
    pub batch: u64,
    pub landed_at_ms: i64,
    pub prev_hash: String,
    pub hash: String,
}

impl LogEntry {
    #[allow(clippy::too_many_arguments)]
    pub fn new(seq: u64, change_id: &str, rev: u32, commit_in: &str, commit_out: &str, batch: u64, landed_at_ms: i64, prev_hash: &str) -> Self {
        let mut e = LogEntry {
            seq,
            change_id: change_id.into(),
            rev,
            commit_in: commit_in.into(),
            commit_out: commit_out.into(),
            batch,
            landed_at_ms,
            prev_hash: prev_hash.into(),
            hash: String::new(),
        };
        e.hash = e.compute_hash();
        e
    }

    pub fn compute_hash(&self) -> String {
        // Immutable version-one domain. Changing it would invalidate the existing chain.
        let s = format!(
            "landseq-log-v1\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
            self.prev_hash, self.seq, self.change_id, self.rev, self.commit_in, self.commit_out, self.batch, self.landed_at_ms
        );
        Sha256::digest(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Verify a contiguous run of entries starting after `prev_hash`.
pub fn verify_chain(prev_hash: &str, entries: &[LogEntry]) -> Result<(), String> {
    let mut prev = prev_hash.to_string();
    for e in entries {
        if e.prev_hash != prev {
            return Err(format!("entry {} does not link to {}", e.seq, &prev[..12]));
        }
        if e.compute_hash() != e.hash {
            return Err(format!("entry {} hash mismatch", e.seq));
        }
        prev = e.hash.clone();
    }
    Ok(())
}
