//! Allowances and seals: human decisions, signed with Ed25519, bound to exact digests.
//!
//! An allowance lets ordinary changes land without a per-change decision: one signature
//! covers up to `max_landings` changes in the named footprint units before it expires.
//! A seal approves one specific change that crossed a seal boundary. Both bind the rules
//! digest, so editing `nerudo.toml` voids every outstanding approval.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    if s.len() % 2 != 0 {
        return Err("odd-length hex".into());
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| "bad hex".to_string())).collect()
}

pub fn verify(pubkey_hex: &str, msg: &[u8], sig_hex: &str) -> Result<(), String> {
    let pk: [u8; 32] = hex_decode(pubkey_hex)?.try_into().map_err(|_| "public key must be 32 bytes")?;
    let sig: [u8; 64] = hex_decode(sig_hex)?.try_into().map_err(|_| "signature must be 64 bytes")?;
    let vk = VerifyingKey::from_bytes(&pk).map_err(|e| e.to_string())?;
    vk.verify(msg, &Signature::from_bytes(&sig)).map_err(|_| "bad signature".to_string())
}

/// Verify a signature over a spec that names a rules digest when the digest itself is not
/// at hand (trunk's trailers leave it out): try each digest in `digests`, the forms the rules
/// accept, and return the one that was signed.
pub fn verify_any(pubkey_hex: &str, sig_hex: &str, digests: &[String], msg: impl Fn(&str) -> Vec<u8>) -> Result<String, String> {
    let mut err = "the rules have no digest".to_string();
    for d in digests {
        match verify(pubkey_hex, &msg(d), sig_hex) {
            Ok(()) => return Ok(d.clone()),
            Err(e) => err = e,
        }
    }
    Err(err)
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AllowanceSpec {
    pub id: String,
    pub trunk: String,
    /// Footprint-unit globs this allowance covers, e.g. ["targets/*", "changes/**"].
    pub units: Vec<String>,
    pub max_landings: u32,
    pub expires_at_ms: i64,
    pub rules_digest: String,
    /// Hex public key of the approver.
    pub approver: String,
}

impl AllowanceSpec {
    pub fn signing_bytes(&self) -> Vec<u8> {
        // Version-one wire domain: retain it so outstanding and historical seals verify.
        let mut b = b"landseq-allowance-v1\n".to_vec();
        b.extend(serde_json::to_vec(self).expect("serialize"));
        b
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Allowance {
    pub spec: AllowanceSpec,
    pub signature: String,
    /// Charged before landing; released only when the change definitely didn't land.
    pub reserved: u32,
    pub spent: u32,
}

impl Allowance {
    pub fn remaining(&self) -> u32 {
        self.spec.max_landings.saturating_sub(self.reserved + self.spent)
    }

    /// `rules_digests`: [`crate::Rules::digests`] of the rules in force.
    pub fn covers(&self, units: &std::collections::BTreeSet<String>, now_ms: i64, rules_digests: &[String]) -> bool {
        now_ms < self.spec.expires_at_ms
            && rules_digests.contains(&self.spec.rules_digest)
            && self.remaining() > 0
            && units.iter().all(|u| self.spec.units.iter().any(|g| crate::glob::matches(g, u)))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SealSpec {
    pub trunk: String,
    pub change_id: String,
    pub rev: u32,
    /// The agent's commit id: the seal covers exactly this content.
    pub commit: String,
    pub rules_digest: String,
    pub reasons: Vec<String>,
    pub expires_at_ms: i64,
    pub approver: String,
}

impl SealSpec {
    pub fn signing_bytes(&self) -> Vec<u8> {
        // A brand rename cannot change bytes already signed by an approver.
        let mut b = b"landseq-seal-v1\n".to_vec();
        b.extend(serde_json::to_vec(self).expect("serialize"));
        b
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Seal {
    pub spec: SealSpec,
    pub signature: String,
}
