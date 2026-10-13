//! What nerudo does with a decision model's answer. The model returns probabilities over
//! fixed options (merge, review, sign); this module, not the model, turns them into an
//! outcome with the thresholds in `[decide.risk]`. The outcome can only add a seal reason:
//! "review" (one approver signs) or "risk" (the quorum in `[seals.require].risk`).

use crate::rules::{RiskRules, RISK_OPTIONS};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One decision, as the receipt records it.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Decision {
    /// Which decision point asked: "risk".
    pub point: String,
    /// "clef", "workers-ai" or "fixed".
    pub provider: String,
    /// The model that answered, as it named itself.
    pub model: String,
    /// Whether the probabilities are calibrated (Clef) or self-reported (an LLM).
    pub calibrated: bool,
    pub probabilities: BTreeMap<String, f64>,
    /// The model's top option and its probability.
    pub choice: String,
    pub p: f64,
    /// 1 when all probability is on one option, 0 when it is spread evenly.
    pub confidence: f64,
    pub threshold: f64,
    /// What nerudo did: "merge", "review" or "sign".
    pub outcome: String,
    /// Why: "chose", "unsure" (no option reached the threshold) or "error".
    pub basis: String,
    /// The seal reason this decision added, if any.
    pub reason: Option<String>,
    /// The provider's error, when basis is "error".
    pub error: Option<String>,
    pub at_ms: i64,
}

/// 1 minus the normalized entropy of `p` over the risk options.
pub fn confidence(p: &BTreeMap<String, f64>) -> f64 {
    let n = RISK_OPTIONS.len() as f64;
    let h: f64 = RISK_OPTIONS.iter().map(|o| p.get(*o).copied().unwrap_or(0.0)).filter(|x| *x > 0.0).map(|x| -x * x.ln()).sum();
    (1.0 - h / n.ln()).clamp(0.0, 1.0)
}

/// Keeps only the risk options, clamps each to [0, 1], and rescales them to sum to 1.
pub fn normalize(raw: &BTreeMap<String, f64>) -> BTreeMap<String, f64> {
    let mut p: BTreeMap<String, f64> = RISK_OPTIONS.iter().map(|o| (o.to_string(), raw.get(*o).copied().unwrap_or(0.0).clamp(0.0, 1.0))).collect();
    let sum: f64 = p.values().sum();
    if sum > 0.0 {
        p.values_mut().for_each(|v| *v /= sum);
    }
    p
}

/// A model name safe to put in a commit trailer.
fn clean(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric() || "._/-@".contains(*c)).take(80).collect()
}

/// The decision for these probabilities. `confidence` is the provider's own value when it
/// reports one (Clef does for Choice answers), else computed here.
pub fn decide(rules: &RiskRules, provider: &str, model: &str, calibrated: bool, probabilities: &BTreeMap<String, f64>, reported_confidence: Option<f64>, at_ms: i64) -> Decision {
    let p = normalize(probabilities);
    let (choice, top) = RISK_OPTIONS.iter().map(|o| (o.to_string(), p[*o])).fold((String::from("merge"), -1.0), |a, b| if b.1 > a.1 { b } else { a });
    let (outcome, basis) = if top >= rules.threshold { (choice.clone(), "chose") } else { (rules.uncertain.clone(), "unsure") };
    let who = format!("{}/{}", clean(provider), clean(model));
    let reason = match (outcome.as_str(), basis) {
        ("merge", _) => None,
        (o, "chose") => Some(format!("{}: decision model chose {o}, p {top:.2} >= {:.2} ({who})", kind(o), rules.threshold)),
        (o, _) => Some(format!("{}: decision model unsure, top {choice} p {top:.2} < {:.2} ({who})", kind(o), rules.threshold)),
    };
    Decision {
        point: "risk".into(),
        provider: provider.into(),
        model: model.into(),
        calibrated,
        confidence: reported_confidence.unwrap_or_else(|| confidence(&p)),
        probabilities: p,
        choice,
        p: top,
        threshold: rules.threshold,
        outcome,
        basis: basis.into(),
        reason,
        error: None,
        at_ms,
    }
}

/// The decision when the model could not be asked: `[decide.risk].on_error` applies.
pub fn failed(rules: &RiskRules, provider: &str, model: &str, error: String, at_ms: i64) -> Decision {
    let outcome = rules.on_error.clone();
    let reason = (outcome != "merge").then(|| format!("{}: decision model unavailable ({}/{})", kind(&outcome), clean(provider), clean(model)));
    Decision { point: "risk".into(), provider: provider.into(), model: model.into(), threshold: rules.threshold, outcome, basis: "error".into(), reason, error: Some(error), at_ms, ..Default::default() }
}

/// The seal kind an outcome adds: review needs one approver, sign the "risk" quorum.
fn kind(outcome: &str) -> &'static str {
    if outcome == "sign" {
        "risk"
    } else {
        "review"
    }
}
