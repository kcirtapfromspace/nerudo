use nerudo_core::{seal_boundaries, trunk_checks, ChangeInput, Rules};
use nerudo_core::rules::{rules_entry, rules_file, LEGACY_RULES_PATH, RULES_PATH};
use nerudo_git::{Commit, FileEntry, Oid, Signature, Store, MODE_BLOB};
use std::collections::BTreeMap;

fn commit(store: &mut Store, files: &BTreeMap<String, FileEntry>, parents: Vec<Oid>) -> Oid {
    let tree = store.write_tree(files);
    let sig = Signature::new("migration-test", "test@example.invalid", 1);
    store.put_commit(&Commit { tree, parents, author: sig.clone(), committer: sig, extra: vec![], message: "migrate rules".into() })
}

#[test]
fn rules_migrate_without_escaping_the_existing_governance_boundary() {
    let mut store = Store::new();
    let old_text = "[seals]\ngovernance = [\"landseq.toml\"]\n";
    let old = Rules::parse(old_text).unwrap();
    let entry = FileEntry { mode: MODE_BLOB, oid: store.put_blob(old_text.as_bytes()) };
    let legacy = BTreeMap::from([(LEGACY_RULES_PATH.into(), entry)]);
    let base = commit(&mut store, &legacy, vec![]);
    let current = BTreeMap::from([(RULES_PATH.into(), entry)]);
    let changed = commit(&mut store, &current, vec![base]);
    let input = ChangeInput { change_id: "rename".into(), rev: 1, base, commit: changed, trailers: vec![] };
    let reasons = seal_boundaries(&store, &old, &input).unwrap();
    assert!(reasons.contains(&format!("governance: {LEGACY_RULES_PATH}")));
    assert!(reasons.contains(&format!("governance: {RULES_PATH}")));
    for tip in [base, changed] {
        assert_eq!(rules_file(&store, &store.commit(&tip).unwrap().tree).unwrap(), Some(entry));
    }
    trunk_checks(&store, &old, &current).unwrap();
    let mut ambiguous = current;
    ambiguous.insert(LEGACY_RULES_PATH.into(), entry);
    assert!(rules_entry(&ambiguous).is_err());
    assert!(trunk_checks(&store, &old, &ambiguous).is_err());
    let tree = store.write_tree(&ambiguous);
    assert!(rules_file(&store, &tree).is_err());
}

#[test]
fn new_templates_protect_the_new_rules_name() {
    for text in [include_str!("../../fixtures/bench-nerudo.toml"), include_str!("../../fixtures/demo-nerudo.toml"), include_str!("../../fixtures/replay/nerudo.toml")] {
        let text = text.replace("<your nerudo pubkey>", &"01".repeat(32)).replace("<your name>", "tester");
        let rules = Rules::parse(&text).unwrap();
        assert!(rules.seals.governance.iter().any(|g| nerudo_core::glob::matches(g, RULES_PATH)));
    }
}

#[test]
fn the_published_version_one_log_still_verifies() {
    let entry = nerudo_core::LogEntry::new(1, "rename", 1, "in", "out", 1, 123, nerudo_core::log::GENESIS);
    // Golden hash from the pre-rename protocol; changing its domain rewrites history.
    assert_eq!(entry.hash, "b91776e78159761a9cf7f154f8c2229423713c3e183eb5aad895d0554431d5d8");
    nerudo_core::log::verify_chain(nerudo_core::log::GENESIS, &[entry]).unwrap();
}
