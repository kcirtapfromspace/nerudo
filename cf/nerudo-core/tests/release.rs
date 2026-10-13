use ed25519_dalek::{Signer, SigningKey};
use nerudo_core::release::{
    bump_of, conventional, next_version, notes_digest, release_trailers, render_notes, split_message, tag_message, valid_tag, verify_release, Bump, FileStats, NoteLine, ReleaseSpec,
};
use nerudo_core::Rules;
use nerudo_git::{Kind, Oid, Signature, Tag};

const RULES: &str = include_str!("../../fixtures/bench-nerudo.toml");

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn key() -> (SigningKey, String) {
    let k = SigningKey::generate(&mut rand_core::OsRng);
    let pk = hex(&k.verifying_key().to_bytes());
    (k, pk)
}

fn approver(name: &str, pk: &str, roles: &[&str]) -> String {
    format!("[[approvers]]\nname = \"{name}\"\nkey = \"{pk}\"\nroles = {roles:?}\n")
}

#[test]
fn versions_follow_conventional_commits() {
    assert_eq!(conventional("feat(api)!: drop v1"), Some(("feat".into(), true)));
    assert_eq!(conventional("fix: off by one"), Some(("fix".into(), false)));
    assert_eq!(conventional("edit t09 line 4, changelog"), None);
    assert_eq!(bump_of("feat: x", ""), Bump::Minor);
    assert_eq!(bump_of("fix: x", "Details.\n\nBREAKING CHANGE: the config moved"), Bump::Major);
    assert_eq!(next_version(None, &[Bump::Major]), "v0.1.0");
    assert_eq!(next_version(Some("v0.1.0"), &[Bump::Patch]), "v0.1.1");
    assert_eq!(next_version(Some("v0.1.4"), &[Bump::Patch, Bump::Minor]), "v0.2.0");
    assert_eq!(next_version(Some("v0.3.1"), &[Bump::Major]), "v0.4.0", "below 1.0 a break bumps the minor version");
    assert_eq!(next_version(Some("v1.3.1"), &[Bump::Major]), "v2.0.0");
    assert_eq!(next_version(Some("v1.3.1"), &[]), "v1.3.2");
    assert_eq!(next_version(Some("v2.0.0-rc.1"), &[Bump::Minor]), "v2.0.0");
    assert!(valid_tag("v1.2.3") && valid_tag("v1.2.3-rc.1") && !valid_tag("1.2.3") && !valid_tag("v1.2") && !valid_tag("v01.2.3") && !valid_tag("v1.2.3-"));
}

#[test]
fn notes_group_merged_prs_and_name_people() {
    let lines = vec![
        NoteLine { subject: "feat(cart): a price per line".into(), pr: "#7".into(), agent: Some("a03".into()), task: Some("T-2".into()), ..Default::default() },
        NoteLine { subject: "fix: round prices".into(), pr: "#8".into(), agent: Some("a01".into()), ..Default::default() },
        NoteLine { subject: "drop the legacy_flag column".into(), pr: "a02c3".into(), agent: Some("a02".into()), approved_by: vec!["you".into()], ..Default::default() },
    ];
    let n = render_notes("v0.2.0", Some("v0.1.0"), &lines, &["you".into()], Some(FileStats { files: 4, adds: 20, dels: 3 }));
    assert!(n.contains("## Features\n\n- feat(cart): a price per line (#7) by a03 · T-2"), "{n}");
    assert!(n.contains("## Fixes\n\n- fix: round prices (#8) by a01"), "{n}");
    assert!(n.contains("## Other\n\n- drop the legacy_flag column (a02c3) by a02"), "{n}");
    assert!(n.contains("## Approved changes"), "{n}");
    assert!(n.contains("- Agents: a01, a02, a03") && n.contains("- Approvers: you"), "{n}");
    assert!(n.ends_with("**Full changelog**: v0.1.0...v0.2.0 · 4 files changed, +20 −3"), "{n}");
    let first = render_notes("v0.1.0", None, &[], &[], None);
    assert!(first.contains("No pull requests merged") && first.contains("v0.1.0 (first release)"));
}

fn signed_tag(rules: &Rules, keys: &[(&str, &SigningKey, &str)], notes: &str, tamper: impl Fn(&mut ReleaseSpec)) -> Tag {
    let commit = Oid::from_hex(&"ab".repeat(20)).unwrap();
    let base = ReleaseSpec {
        trunk: "t".into(),
        tag: "v0.2.0".into(),
        commit: commit.to_hex(),
        seq: 9,
        title: "v0.2.0".into(),
        notes_digest: notes_digest(notes),
        previous: Some("v0.1.0".into()),
        prerelease: false,
        rules_digest: rules.digest(),
        expires_at_ms: 2_000_000_000_000,
        approver: String::new(),
    };
    let signed: Vec<(String, ReleaseSpec, String)> = keys
        .iter()
        .map(|(name, k, pk)| {
            let mut s = ReleaseSpec { approver: pk.to_string(), ..base.clone() };
            tamper(&mut s);
            let sig = hex(&k.sign(&s.signing_bytes()).to_bytes());
            (name.to_string(), s, sig)
        })
        .collect();
    Tag {
        object: commit,
        kind: Kind::Commit,
        name: base.tag.clone(),
        tagger: Some(Signature::new("nerudo", "nerudo@t.invalid", 1_800_000_000)),
        message: tag_message(&base.title, notes, &release_trailers(&base, &signed)),
    }
}

#[test]
fn a_release_tag_verifies_from_outside() {
    let (alice, apk) = key();
    let (bob, bpk) = key();
    let (mallory, mpk) = key();
    let rules = Rules::parse(&format!("{RULES}\n{}{}", approver("alice", &apk, &["maintainer"]), approver("bob", &bpk, &["maintainer"]))).unwrap();
    let notes = "## Fixes\n\n- fix: x (#1) by a01";

    let t = signed_tag(&rules, &[("alice", &alice, &apk)], notes, |_| {});
    assert_eq!(split_message(&t.message), ("v0.2.0".to_string(), notes.to_string()));
    let ok = verify_release(&t, "t", &rules).unwrap();
    assert_eq!((ok.seq, ok.signers.clone()), (9, vec!["alice".to_string()]));

    // Edited notes no longer match the digest that was signed.
    let mut edited = t.clone();
    edited.message = edited.message.replace("fix: x", "fix: y");
    assert!(verify_release(&edited, "t", &rules).unwrap_err().contains("digest"));

    // A key outside nerudo.toml, or a signature over a different commit, is refused.
    let t = signed_tag(&rules, &[("mallory", &mallory, &mpk)], notes, |_| {});
    assert!(verify_release(&t, "t", &rules).unwrap_err().contains("not an approver"));
    let t = signed_tag(&rules, &[("alice", &alice, &apk)], notes, |s| s.commit = "cd".repeat(20));
    assert!(verify_release(&t, "t", &rules).unwrap_err().contains("bad signature"));

    // [releases] can ask for two maintainers.
    let two = Rules::parse(&format!(
        "{RULES}\n{}{}\n[releases]\nrequire = {{ roles = [\"maintainer\"], count = 2 }}\n",
        approver("alice", &apk, &["maintainer"]),
        approver("bob", &bpk, &["maintainer"])
    ))
    .unwrap();
    let t = signed_tag(&two, &[("alice", &alice, &apk)], notes, |_| {});
    assert!(verify_release(&t, "t", &two).unwrap_err().contains("needs 2 maintainer"));
    let t = signed_tag(&two, &[("alice", &alice, &apk), ("bob", &bob, &bpk)], notes, |_| {});
    assert_eq!(verify_release(&t, "t", &two).unwrap().signers, vec!["alice".to_string(), "bob".to_string()]);
}

#[test]
fn a_release_signed_under_an_earlier_form_of_the_digest_still_verifies() {
    let (alice, apk) = key();
    let rules = Rules::parse(&format!("{RULES}\n{}", approver("alice", &apk, &["maintainer"]))).unwrap();
    let notes = "## Fixes\n\n- fix: x (#1) by a01";
    for d in rules.digests() {
        let t = signed_tag(&rules, &[("alice", &alice, &apk)], notes, |s| s.rules_digest = d.clone());
        assert_eq!(verify_release(&t, "t", &rules).unwrap().signers, vec!["alice".to_string()]);
    }
    let edited = Rules::parse(&format!("{RULES}\n{}\n[allowances]\nrequired = true\n", approver("alice", &apk, &["maintainer"]))).unwrap();
    let t = signed_tag(&rules, &[("alice", &alice, &apk)], notes, |s| s.rules_digest = edited.digest());
    assert!(verify_release(&t, "t", &rules).unwrap_err().contains("bad signature"));
}

#[test]
fn rules_without_releases_keep_their_digest() {
    let (_, apk) = key();
    let plain = Rules::parse(&format!("{RULES}\n{}", approver("alice", &apk, &["maintainer"]))).unwrap();
    assert!(!serde_json::to_string(&plain).unwrap().contains("releases"));
    assert_eq!(plain.release_quorum().roles, vec!["maintainer".to_string()]);
    let bad = Rules::parse(&format!("{RULES}\n{}\n[releases]\nrequire = {{ roles = [\"maintainer\"], count = 3 }}\n", approver("alice", &apk, &["maintainer"]))).unwrap_err();
    assert!(bad.contains("releases.require"), "{bad}");
}
