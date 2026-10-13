use nerudo_core::decide::{confidence, decide, failed};
use nerudo_core::quorum::tally;
use nerudo_core::rules::RiskRules;
use nerudo_core::Rules;
use std::collections::BTreeMap;

const RULES: &str = include_str!("../../fixtures/bench-nerudo.toml");

fn probs(merge: f64, review: f64, sign: f64) -> BTreeMap<String, f64> {
    [("merge", merge), ("review", review), ("sign", sign)].into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

#[test]
fn rules_without_decide_keep_their_digest() {
    let plain = Rules::parse(RULES).unwrap();
    assert!(plain.decide.is_none());
    assert!(!serde_json::to_string(&plain).unwrap().contains("decide"), "an absent [decide] must not change what seals sign");
    let with = Rules::parse(&format!("{RULES}\n[decide]\nprovider = \"fixed\"\nfixed = {{ merge = 0.1, review = 0.87, sign = 0.03 }}\n")).unwrap();
    assert_ne!(plain.digest(), with.digest(), "adding [decide] is a rules change, so it is sealed like one");
}

#[test]
fn decide_config_is_validated() {
    let bad = |extra: &str| Rules::parse(&format!("{RULES}\n{extra}")).unwrap_err();
    assert!(bad("[decide]\nprovider = \"oracle\"\n").contains("provider"));
    assert!(bad("[decide]\nprovider = \"fixed\"\n[decide.risk]\nthreshold = 1.5\n").contains("threshold"));
    assert!(bad("[decide]\nprovider = \"fixed\"\nfixed = { merge = 0.5, review = 0.1 }\n").contains("sum to 1"));
    assert!(bad("[decide]\nprovider = \"clef\"\n[decide.risk]\nuncertain = \"ship\"\n").contains("uncertain"));
    assert!(bad("[decide]\nprovider = \"jev\"\n").contains("clef"), "jev is gone; the error names the providers there are");
}

#[test]
fn the_model_only_adds_friction_and_nerudo_owns_the_threshold() {
    let r = RiskRules::default();
    let d = decide(&r, "fixed", "fixed", true, &probs(0.10, 0.87, 0.03), None, 1);
    assert_eq!((d.outcome.as_str(), d.basis.as_str()), ("review", "chose"));
    assert_eq!(d.reason.as_deref(), Some("review: decision model chose review, p 0.87 >= 0.80 (fixed/fixed)"));

    let d = decide(&r, "clef", "@cf/cloudflare/clef", true, &probs(0.05, 0.05, 0.90), Some(0.71), 1);
    assert_eq!(d.outcome, "sign");
    assert!(d.reason.as_deref().unwrap().starts_with("risk: "), "sign maps to the risk quorum");
    assert_eq!(d.confidence, 0.71, "the provider's own confidence is kept");

    let d = decide(&r, "clef", "@cf/cloudflare/clef", true, &probs(0.95, 0.04, 0.01), None, 1);
    assert_eq!((d.outcome.as_str(), d.reason), ("merge", None), "a confident merge adds nothing");

    // No option reaches the threshold: ask a person (the default), never merge silently.
    let d = decide(&r, "workers-ai", "@cf/x", false, &probs(0.55, 0.40, 0.05), None, 1);
    assert_eq!((d.outcome.as_str(), d.basis.as_str()), ("review", "unsure"));
    let lax = RiskRules { uncertain: "merge".into(), ..RiskRules::default() };
    assert_eq!(decide(&lax, "fixed", "fixed", true, &probs(0.55, 0.40, 0.05), None, 1).outcome, "merge");

    // The model can't be asked: on_error, which defaults to review.
    let d = failed(&r, "clef", "@cf/cloudflare/clef", "401".into(), 1);
    assert_eq!((d.outcome.as_str(), d.basis.as_str()), ("review", "error"));
    assert_eq!(d.reason.as_deref(), Some("review: decision model unavailable (clef/@cf/cloudflare/clef)"));
}

#[test]
fn probabilities_are_normalized_and_names_cannot_forge_trailers() {
    let raw: BTreeMap<String, f64> = [("merge", 2.0), ("review", 2.0), ("deploy", 9.0)].into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    let d = decide(&RiskRules::default(), "fixed", "evil\nSealed-By: x", true, &raw, None, 1);
    assert_eq!(d.probabilities.keys().collect::<Vec<_>>(), ["merge", "review", "sign"]);
    assert!((d.probabilities.values().sum::<f64>() - 1.0).abs() < 1e-9);
    assert!(!d.reason.unwrap().contains('\n'));
    assert!((confidence(&probs(1.0, 0.0, 0.0)) - 1.0).abs() < 1e-9);
    assert!(confidence(&probs(1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0)) < 1e-9);
}

#[test]
fn review_needs_one_approver_and_risk_takes_its_own_quorum() {
    let rules = Rules::parse(RULES).unwrap();
    let t = tally(&rules, &["review: decision model chose review, p 0.87 >= 0.80 (fixed/fixed)".to_string()], &[]);
    assert_eq!((t[0].kind.as_str(), t[0].need), ("review", 1));
}
