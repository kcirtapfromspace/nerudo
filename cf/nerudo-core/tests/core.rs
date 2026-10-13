use ed25519_dalek::{Signer, SigningKey};
use nerudo_core::merge::{digest_index, footprint};
use nerudo_core::quorum::{met, seal_problem, seal_trailers, tally, BREAK_GLASS};
use nerudo_core::{apply_change, auth, seal_boundaries, trunk_checks, verify_landing, Allowance, AllowanceSpec, ChangeInput, Ejection, Rules, Seal, SealSpec};
use nerudo_git::{Commit, FileEntry, Oid, Signature, Store, MODE_BLOB};
use std::collections::BTreeMap;

const RULES: &str = include_str!("../../fixtures/bench-nerudo.toml");

fn sig(n: &str) -> Signature {
    Signature::new(n, &format!("{n}@bench.invalid"), 1_791_400_000)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

struct Repo {
    store: Store,
    rules: Rules,
}

impl Repo {
    fn new() -> (Repo, Oid) {
        Repo::seed(RULES, false)
    }

    /// A repo whose seed carries its own nerudo.toml, as a real trunk does.
    fn governed(extra: &str) -> (Repo, Oid) {
        Repo::seed(&format!("{RULES}\n{extra}"), true)
    }

    fn seed(rules: &str, on_trunk: bool) -> (Repo, Oid) {
        let mut repo = Repo { store: Store::new(), rules: Rules::parse(rules).unwrap() };
        let mut files: BTreeMap<String, &str> = BTreeMap::new();
        if on_trunk {
            files.insert("nerudo.toml".into(), rules);
        }
        files.insert("targets/t00/src.txt".into(), "a\nb\nc\n");
        files.insert("targets/t01/src.txt".into(), "x\ny\nz\n");
        files.insert("CHANGELOG.md".into(), "# Changelog\n");
        files.insert("deps/base.txt".into(), "base==1.0\n");
        files.insert("migrations/0001_init.sql".into(), "create table users(id int);\n");
        files.insert(
            "tests/test_money.py".into(),
            "import unittest\n\nclass T(unittest.TestCase):\n    def test_one(self):\n        self.assertEqual(1, 1)\n\n    def test_two(self):\n        self.assertEqual(2, 2)\n",
        );
        files.insert("ci/check.py".into(), "print('ok')\n");
        let mut flat = repo.flat_from(&files);
        let lock = digest_index(&repo.store, &flat, "deps").unwrap();
        let lock_oid = repo.store.put_blob(lock.as_bytes());
        flat.insert("deps.lock".into(), FileEntry { mode: MODE_BLOB, oid: lock_oid });
        let tree = repo.store.write_tree(&flat);
        let c = Commit { tree, parents: vec![], author: sig("seed"), committer: sig("seed"), extra: vec![], message: "seed\n".into() };
        let root = repo.store.put_commit(&c);
        (repo, root)
    }

    fn flat_from(&mut self, files: &BTreeMap<String, &str>) -> BTreeMap<String, FileEntry> {
        files.iter().map(|(p, t)| (p.clone(), FileEntry { mode: MODE_BLOB, oid: self.store.put_blob(t.as_bytes()) })).collect()
    }

    /// An agent's change on `base`: path -> Some(new content) | None (delete).
    fn change(&mut self, base: Oid, id: &str, edits: &[(&str, Option<String>)]) -> ChangeInput {
        let tree = self.store.commit(&base).unwrap().tree;
        let mut flat = self.store.flatten(&tree).unwrap();
        for (p, c) in edits {
            match c {
                Some(t) => {
                    let oid = self.store.put_blob(t.as_bytes());
                    flat.insert(p.to_string(), FileEntry { mode: MODE_BLOB, oid });
                }
                None => {
                    flat.remove(*p);
                }
            }
        }
        let t = self.store.write_tree(&flat);
        let c = Commit { tree: t, parents: vec![base], author: sig(id), committer: sig(id), extra: vec![], message: format!("{id}: change\n") };
        ChangeInput { change_id: id.into(), rev: 1, commit: self.store.put_commit(&c), base, trailers: vec![] }
    }

    fn file(&self, commit: Oid, path: &str) -> Option<String> {
        let flat = self.store.flatten(&self.store.commit(&commit).unwrap().tree).unwrap();
        flat.get(path).map(|e| String::from_utf8_lossy(self.store.blob(&e.oid).unwrap()).into_owned())
    }
}

#[test]
fn rules_merge_two_concurrent_changes_onto_one_trunk() {
    let (mut r, root) = Repo::new();
    // Both agents start from the same base, like concurrent agents.
    let a = r.change(root, "a01", &[
        ("targets/t00/src.txt", Some("a\nB-by-a01\nc\n".into())),
        ("CHANGELOG.md", Some("# Changelog\n- a01\n".into())),
        ("deps/a01.txt", Some("dep-a01==1.0\n".into())),
        ("migrations/NEXT_a01.sql", Some("alter table users add column a int;\n".into())),
        ("deps.lock", Some("garbage the agent wrote\n".into())),
    ]);
    let b = r.change(root, "b01", &[
        ("targets/t01/src.txt", Some("x\nY-by-b01\nz\n".into())),
        ("CHANGELOG.md", Some("# Changelog\n- b01\n".into())),
        ("deps/b01.txt", Some("dep-b01==1.0\n".into())),
        ("migrations/NEXT_b01.sql", Some("alter table users add column b int;\n".into())),
    ]);
    assert_eq!(footprint(&r.store, &r.rules, &a).unwrap().into_iter().collect::<Vec<_>>(), vec!["deps/a01.txt", "targets/t00"]);
    let committer = sig("nerudo");
    let la = apply_change(&mut r.store, &r.rules, root, &a, &committer).unwrap();
    let lb = apply_change(&mut r.store, &r.rules, la.commit, &b, &committer).unwrap();

    assert_eq!(r.file(lb.commit, "CHANGELOG.md").unwrap(), "# Changelog\n- a01\n- b01\n");
    assert_eq!(la.allocated, vec!["migrations/0002_a01.sql"]);
    assert_eq!(lb.allocated, vec!["migrations/0003_b01.sql"]);
    assert!(r.file(lb.commit, "migrations/NEXT_b01.sql").is_none());
    assert_eq!(r.file(lb.commit, "targets/t00/src.txt").unwrap(), "a\nB-by-a01\nc\n");
    assert_eq!(r.file(lb.commit, "targets/t01/src.txt").unwrap(), "x\nY-by-b01\nz\n");
    let tip = r.store.flatten(&r.store.commit(&lb.commit).unwrap().tree).unwrap();
    trunk_checks(&r.store, &r.rules, &tip).expect("trunk invariants hold");
    let landed = r.store.commit(&lb.commit).unwrap();
    assert_eq!(landed.trailer("Change-Id").as_deref(), Some("b01"));
    assert_eq!(landed.author.name, "b01");
    assert_eq!(landed.parents, vec![la.commit]);
}

#[test]
fn a_change_that_adds_a_path_git_refuses_to_check_out_is_ejected() {
    use nerudo_core::merge::{edits, path_problem};
    let (mut r, root) = Repo::new();
    let committer = sig("landseq");
    // Each of these would leave trunk impossible to clone (git refuses the path at checkout),
    // or reads as another path somewhere; the hook one is a shell script on every clone.
    for bad in [
        ".git/hooks/post-checkout",
        ".GIT/config",
        ".git./config",
        "targets/t00/.g\u{200c}it/config",
        "GIT~1/config",
        "targets/t00/../../x",
        "./x",
        "targets//x",
        "targets/t00/a\\b",
        "targets/t00/a\nb",
        &format!("targets/t00/{}", "n".repeat(256)),
        &format!("{}f", "d/".repeat(2100)),
    ] {
        let c = r.change(root, "evil", &[(bad, Some("echo pwned\n".into()))]);
        assert!(matches!(footprint(&r.store, &r.rules, &c), Err(Ejection::BadPath { .. })), "{bad:?}");
        assert!(matches!(edits(&r.store, &c), Err(Ejection::BadPath { .. })), "{bad:?}");
        assert!(matches!(apply_change(&mut r.store, &r.rules, root, &c, &committer), Err(Ejection::BadPath { .. })), "{bad:?}");
    }
    // An entry whose mode is not a file's.
    let tree = r.store.commit(&root).unwrap().tree;
    let mut flat = r.store.flatten(&tree).unwrap();
    let oid = r.store.put_blob(b"x\n");
    flat.insert("targets/t00/odd".into(), FileEntry { mode: 0o100600, oid });
    let t = r.store.write_tree(&flat);
    let c = Commit { tree: t, parents: vec![root], author: sig("evil"), committer: sig("evil"), extra: vec![], message: "odd mode\n".into() };
    let odd = ChangeInput { change_id: "odd".into(), rev: 1, commit: r.store.put_commit(&c), base: root, trailers: vec![] };
    assert!(matches!(footprint(&r.store, &r.rules, &odd), Err(Ejection::BadPath { .. })));
    // The receipt names the kind.
    let v = serde_json::to_value(Ejection::BadPath { detail: "x".into() }).unwrap();
    assert_eq!(v["kind"], "bad_path");
    // Dotfiles that are not .git are ordinary paths, and deleting anything is fine.
    for ok in [".gitignore", ".github/workflows/ci.yml", ".gitattributes", "targets/t00/.git-blame-ignore-revs", "a.git/b", "git~10/x"] {
        assert_eq!(path_problem(ok), None, "{ok}");
    }
    let fine = r.change(root, "fine", &[(".github/workflows/ci.yml", Some("on: push\n".into())), ("targets/t00/src.txt", None)]);
    assert!(footprint(&r.store, &r.rules, &fine).is_ok());
    apply_change(&mut r.store, &r.rules, root, &fine, &committer).expect("ordinary paths land");
}

#[test]
fn the_bytes_a_change_adds_are_counted_from_its_files() {
    use nerudo_core::merge::{change_bytes, MAX_CHANGE_BYTES};
    let (mut r, root) = Repo::new();
    let small = r.change(root, "s", &[("targets/t00/src.txt", Some("a\nb\nc\nd\n".into())), ("targets/t00/new.txt", Some("hello\n".into()))]);
    assert_eq!(change_bytes(&r.store, &small).unwrap(), 8 + 6);
    // A deletion adds nothing.
    let del = r.change(root, "d", &[("targets/t00/src.txt", None)]);
    assert_eq!(change_bytes(&r.store, &del).unwrap(), 0);
    // One file over the limit: the gate ejects it as too_big (trunk.rs).
    let big = "x".repeat(MAX_CHANGE_BYTES + 1);
    let huge = r.change(root, "h", &[("targets/t00/blob.bin", Some(big))]);
    assert!(change_bytes(&r.store, &huge).unwrap() > MAX_CHANGE_BYTES);
}

#[test]
fn the_rules_file_is_a_seal_boundary_whatever_the_globs_say() {
    // Rules whose governance globs leave landseq.toml out, as a careless file might.
    let rules = RULES.replace("\"landseq.toml\", ", "").replace("\"landseq.toml\"", "");
    assert!(!rules.contains("\"landseq.toml\""), "{rules}");
    let (mut r, root) = Repo::seed(&rules, true);
    let c = r.change(root, "coup", &[("landseq.toml", Some(format!("{rules}\n[[approvers]]\nname = \"mallory\"\nroles = [\"maintainer\"]\n")))]);
    let reasons = seal_boundaries(&r.store, &r.rules, &c).unwrap();
    assert_eq!(reasons, vec!["governance: landseq.toml"]);
    // Listed in the globs too: one reason, not two.
    let (mut r, root) = Repo::new();
    let c = r.change(root, "edit", &[("landseq.toml", Some(RULES.to_string()))]);
    assert_eq!(seal_boundaries(&r.store, &r.rules, &c).unwrap(), vec!["governance: landseq.toml"]);
}

#[test]
fn same_file_edited_differently_is_a_conflict() {
    let (mut r, root) = Repo::new();
    let a = r.change(root, "a01", &[("targets/t00/src.txt", Some("a\nB-by-a\nc\n".into()))]);
    let b = r.change(root, "b01", &[("targets/t00/src.txt", Some("a\nB-by-b\nc\n".into()))]);
    let la = apply_change(&mut r.store, &r.rules, root, &a, &sig("nerudo")).unwrap();
    let e = apply_change(&mut r.store, &r.rules, la.commit, &b, &sig("nerudo")).unwrap_err();
    let Ejection::Conflict { paths, hunks, beaten_by } = e else { panic!("not a conflict: {e:?}") };
    assert_eq!(paths, vec!["targets/t00/src.txt".to_string()]);
    assert!(beaten_by.is_empty());
    let h = &hunks["targets/t00/src.txt"];
    assert!(h.contains("<<<<<<< main\nB-by-a\n") && h.contains("B-by-b\n>>>>>>> this change"), "{h}");
}

#[test]
fn a_conflict_receipt_from_before_hunks_still_reads() {
    let old: Ejection = serde_json::from_str(r#"{"kind":"conflict","paths":["targets/t00/src.txt"]}"#).unwrap();
    assert_eq!(old, Ejection::Conflict { paths: vec!["targets/t00/src.txt".into()], hunks: Default::default(), beaten_by: vec![] });
    assert_eq!(serde_json::to_string(&old).unwrap(), r#"{"kind":"conflict","paths":["targets/t00/src.txt"]}"#);
}

#[test]
fn conflict_hunks_keep_only_the_conflicts_and_are_cut() {
    let (mut r, root) = Repo::new();
    let long: String = (0..2000).map(|i| format!("line {i}\n")).collect();
    let a = r.change(root, "a01", &[("targets/t00/src.txt", Some(long.replace("line 5\n", "A\n").replace("line 1500\n", "A2\n")))]);
    let la = apply_change(&mut r.store, &r.rules, root, &a, &sig("nerudo")).unwrap();
    let base = r.change(root, "b00", &[("targets/t00/src.txt", Some(long.clone()))]);
    let lb = apply_change(&mut r.store, &r.rules, root, &base, &sig("nerudo")).unwrap();
    let b = r.change(lb.commit, "b01", &[("targets/t00/src.txt", Some(long.replace("line 5\n", "B\n").replace("line 1500\n", "B2\n")))]);
    let Ejection::Conflict { hunks, .. } = apply_change(&mut r.store, &r.rules, la.commit, &b, &sig("nerudo")).unwrap_err() else { panic!() };
    let h = &hunks["targets/t00/src.txt"];
    assert!(h.len() < 400 && h.contains("line 3\n") && !h.contains("line 100\n") && h.contains("…\n") && h.contains("B2\n"), "{h}");
    let many: String = (0..800).map(|i| format!("<<<<<<< ours\nx{i}\n=======\ny{i}\n>>>>>>> theirs\n")).collect();
    let cut = nerudo_core::merge::hunks_of(&many);
    assert!(cut.len() <= nerudo_core::merge::HUNK_BYTES + 8 && cut.ends_with("[cut]\n"), "{}", cut.len());
}

#[test]
fn different_lines_of_the_same_file_merge() {
    let (mut r, root) = Repo::new();
    let a = r.change(root, "a01", &[("targets/t00/src.txt", Some("A\nb\nc\n".into()))]);
    let b = r.change(root, "b01", &[("targets/t00/src.txt", Some("a\nb\nC\n".into()))]);
    let la = apply_change(&mut r.store, &r.rules, root, &a, &sig("nerudo")).unwrap();
    let lb = apply_change(&mut r.store, &r.rules, la.commit, &b, &sig("nerudo")).unwrap();
    assert_eq!(r.file(lb.commit, "targets/t00/src.txt").unwrap(), "A\nb\nC\n");
}

#[test]
fn dup_numbering_from_an_agent_that_bypassed_allocation_fails_the_trunk_check() {
    let (mut r, root) = Repo::new();
    let a = r.change(root, "a01", &[("migrations/0002_x.sql", Some("--\n".into()))]);
    let b = r.change(root, "b01", &[("migrations/0002_y.sql", Some("--\n".into()))]);
    let la = apply_change(&mut r.store, &r.rules, root, &a, &sig("l")).unwrap();
    let lb = apply_change(&mut r.store, &r.rules, la.commit, &b, &sig("l")).unwrap();
    let tip = r.store.flatten(&r.store.commit(&lb.commit).unwrap().tree).unwrap();
    assert!(trunk_checks(&r.store, &r.rules, &tip).unwrap_err().contains("duplicate number 0002"));
}

#[test]
fn an_approver_with_no_key_is_listed_but_signs_nothing() {
    let text = format!("{RULES}\n[[approvers]]\nname = \"yusukebe\"\nroles = [\"maintainer\"]\n\n[seals.require]\ngovernance = {{ roles = [\"maintainer\"], count = 1 }}\n");
    let rules = Rules::parse(&text).unwrap();
    assert_eq!(rules.approvers[0].key, "");
    assert!(rules.approver("").is_none());
    let two = format!("{text}\n[[approvers]]\nname = \"cjol\"\nroles = [\"maintainer\"]\n");
    assert_eq!(Rules::parse(&two).unwrap().approvers.len(), 2, "two people with no key are two people");
    assert!(Rules::parse(&format!("{RULES}\n[[approvers]]\nname = \"x\"\nkey = \"abc\"\n")).is_err());
}

#[test]
fn an_approval_rule_holds_any_change_on_its_paths() {
    let (mut r, root) = Repo::new();
    r.rules = Rules::parse(&RULES.replace("[seals]\n", "[seals]\napproval = [\"targets/**\"]\n")).unwrap();
    let one = r.change(root, "a", &[("targets/t00/src.txt", Some("q\n".into()))]);
    assert_eq!(seal_boundaries(&r.store, &r.rules, &one).unwrap(), vec!["approval: targets/t00/src.txt"]);
    let two = r.change(root, "b", &[("targets/t00/src.txt", Some("q\n".into())), ("targets/t01/src.txt", Some("r\n".into()))]);
    assert_eq!(seal_boundaries(&r.store, &r.rules, &two).unwrap(), vec!["approval: targets/t00/src.txt and 1 more"]);
    let other = r.change(root, "c", &[("CHANGELOG.md", Some("# Changes\n".into()))]);
    assert!(seal_boundaries(&r.store, &r.rules, &other).unwrap().is_empty());
}

#[test]
fn seal_boundaries_flag_governance_ratchet_and_destructive_sql() {
    let (mut r, root) = Repo::new();
    let clean = r.change(root, "c", &[("targets/t00/src.txt", Some("q\n".into()))]);
    assert!(seal_boundaries(&r.store, &r.rules, &clean).unwrap().is_empty());
    let gov = r.change(root, "g", &[("ci/check.py", Some("import sys; sys.exit(0)\n".into()))]);
    assert_eq!(seal_boundaries(&r.store, &r.rules, &gov).unwrap(), vec!["governance: ci/check.py"]);
    let weak = r.change(root, "w", &[(
        "tests/test_money.py",
        Some("import unittest\n\nclass T(unittest.TestCase):\n    def test_one(self):\n        self.assertTrue(True)\n\n    def test_two(self):\n        self.assertEqual(2, 2)\n".into()),
    )]);
    assert_eq!(seal_boundaries(&r.store, &r.rules, &weak).unwrap(), vec!["ratchet: tests/test_money.py: changed assertions in test_one"]);
    let gone = r.change(root, "d", &[(
        "tests/test_money.py",
        Some("import unittest\n\nclass T(unittest.TestCase):\n    def test_two(self):\n        self.assertEqual(2, 2)\n".into()),
    )]);
    assert_eq!(seal_boundaries(&r.store, &r.rules, &gone).unwrap(), vec!["ratchet: tests/test_money.py: removed test_one"]);
    let skip = r.change(root, "s", &[(
        "tests/test_money.py",
        Some("import unittest\n\nclass T(unittest.TestCase):\n    @unittest.skip(\"flaky\")\n    def test_one(self):\n        self.assertEqual(1, 1)\n\n    def test_two(self):\n        self.assertEqual(2, 2)\n".into()),
    )]);
    assert_eq!(seal_boundaries(&r.store, &r.rules, &skip).unwrap(), vec!["ratchet: tests/test_money.py: added a skip"]);
    let drop = r.change(root, "x", &[("migrations/NEXT_cleanup.sql", Some("-- tidy\nalter table users drop column nickname;\n".into()))]);
    assert_eq!(
        seal_boundaries(&r.store, &r.rules, &drop).unwrap(),
        vec!["irreversible: migrations/NEXT_cleanup.sql: alter table users drop column nickname;"]
    );
}

#[test]
fn allowances_and_seals_verify_and_bind_to_rules() {
    let key = SigningKey::generate(&mut rand_core::OsRng);
    let pk = hex(&key.verifying_key().to_bytes());
    let rules = Rules::parse(RULES).unwrap();
    let spec = AllowanceSpec {
        id: "al-1".into(),
        trunk: "bench".into(),
        units: vec!["targets/*".into(), "deps/*".into(), "changes/*".into()],
        max_landings: 2,
        expires_at_ms: 2_000_000_000_000,
        rules_digest: rules.digest(),
        approver: pk.clone(),
    };
    let sig_hex = hex(&key.sign(&spec.signing_bytes()).to_bytes());
    auth::verify(&pk, &spec.signing_bytes(), &sig_hex).expect("valid allowance");
    let mut tampered = spec.clone();
    tampered.max_landings = 1000;
    assert!(auth::verify(&pk, &tampered.signing_bytes(), &sig_hex).is_err(), "budget can't be edited after signing");

    let mut a = Allowance { spec, signature: sig_hex, reserved: 0, spent: 0 };
    let units = ["targets/t00".to_string(), "deps/a.txt".to_string()].into_iter().collect();
    assert!(a.covers(&units, 1_000, &rules.digests()));
    assert!(!a.covers(&units, 1_000, &["different-rules".to_string()]), "editing rules voids allowances");
    let other = ["ci".to_string()].into_iter().collect();
    assert!(!a.covers(&other, 1_000, &rules.digests()), "outside the approved units");
    a.reserved = 1;
    a.spent = 1;
    assert!(!a.covers(&units, 1_000, &rules.digests()), "budget exhausted");

    let seal = SealSpec {
        trunk: "bench".into(),
        change_id: "g".into(),
        rev: 1,
        commit: Oid::ZERO.to_hex(),
        rules_digest: rules.digest(),
        reasons: vec!["governance: ci/check.py".into()],
        expires_at_ms: 2_000_000_000_000,
        approver: pk.clone(),
    };
    let s = hex(&key.sign(&seal.signing_bytes()).to_bytes());
    auth::verify(&pk, &seal.signing_bytes(), &s).unwrap();
}

#[test]
fn digest_index_is_byte_identical_to_the_bench_generator() {
    let d = std::env::temp_dir().join(format!("nerudo-lock-{}", std::process::id()));
    std::fs::create_dir_all(d.join("deps")).unwrap();
    let entries = [("base.txt", "base==1.0\n"), ("a07c02.txt", "dep-a07c02==1.0\n"), ("a00c01.txt", " dep-a00c01==1.0 \n")];
    let mut store = Store::new();
    let mut flat = BTreeMap::new();
    for (n, t) in entries {
        std::fs::write(d.join("deps").join(n), t).unwrap();
        flat.insert(format!("deps/{n}"), FileEntry { mode: MODE_BLOB, oid: store.put_blob(t.as_bytes()) });
    }
    let gen = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/repo_template/ci/gen_lock.py");
    let out = std::process::Command::new("python3").arg(&gen).arg(&d).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let py = std::fs::read_to_string(d.join("deps.lock")).unwrap();
    assert_eq!(digest_index(&store, &flat, "deps").unwrap(), py);
}

struct Human {
    name: &'static str,
    key: SigningKey,
}

impl Human {
    fn new(name: &'static str) -> Human {
        Human { name, key: SigningKey::generate(&mut rand_core::OsRng) }
    }
    fn pk(&self) -> String {
        hex(&self.key.verifying_key().to_bytes())
    }
    fn entry(&self, roles: &str) -> String {
        format!("[[approvers]]\nname = \"{}\"\nkey = \"{}\"\nroles = [{roles}]\n", self.name, self.pk())
    }
    fn seal(&self, rules: &Rules, c: &ChangeInput, reasons: &[String], expires_at_ms: i64) -> Seal {
        let spec = SealSpec {
            trunk: "t".into(),
            change_id: c.change_id.clone(),
            rev: c.rev,
            commit: c.commit.to_hex(),
            rules_digest: rules.digest(),
            reasons: reasons.to_vec(),
            expires_at_ms,
            approver: self.pk(),
        };
        let signature = hex(&self.key.sign(&spec.signing_bytes()).to_bytes());
        Seal { spec, signature }
    }
}

const FAR: i64 = 2_000_000_000_000;

/// alice and bob maintain; carol is the DBA. CI edits need both maintainers.
fn team() -> (Human, Human, Human, String) {
    let (a, b, c) = (Human::new("alice"), Human::new("bob"), Human::new("carol"));
    let extra = format!(
        "{}{}{}\n[seals.require]\ngovernance = {{ roles = [\"maintainer\"], count = 2 }}\nirreversible = {{ roles = [\"dba\"], count = 1 }}\n",
        a.entry("\"maintainer\""),
        b.entry("\"maintainer\""),
        c.entry("\"dba\"")
    );
    (a, b, c, extra)
}

#[test]
fn the_demo_rules_parse_once_the_demo_fills_in_a_name_and_a_key() {
    let text = include_str!("../../fixtures/demo-nerudo.toml");
    assert!(Rules::parse(text).is_err(), "the placeholder isn't a key");
    let rules = Rules::parse(&text.replace("<your nerudo pubkey>", &Human::new("you").pk()).replace("<your name>", "you")).unwrap();
    assert_eq!(rules.approvers.len(), 2, "you, and dana so that @dba reaches someone");
    assert!(rules.approvers.iter().any(|a| a.name == "dana" && a.roles == ["dba"]));
}

#[test]
fn approver_lists_and_quorums_are_validated() {
    let (a, b, _, extra) = team();
    assert!(Rules::parse(&format!("{RULES}\n{extra}")).is_ok());
    let bad = |e: &str| Rules::parse(&format!("{RULES}\n{e}")).unwrap_err();
    assert!(bad(&format!("{}{}", a.entry(""), a.entry(""))).contains("listed twice"));
    assert!(bad("[[approvers]]\nname = \"x\"\nkey = \"abc\"\n").contains("64 lowercase hex"));
    assert!(bad(&format!("{}\n[seals.require]\nvibes = {{ count = 1 }}\n", a.entry(""))).contains("kind must be one of"));
    // Two maintainers required, one exists: the trunk could never land a CI edit.
    let short = format!("{}{}\n[seals.require]\ngovernance = {{ roles = [\"maintainer\"], count = 2 }}\n", a.entry("\"maintainer\""), b.entry("\"dba\""));
    assert!(bad(&short).contains("count 2 but 1 approvers"));
    assert!(bad(&format!("{}\n[allowances]\nroles = [\"release\"]\n", a.entry("\"maintainer\""))).contains("no approver holds"));
}

#[test]
fn quorum_counts_distinct_approvers_with_an_eligible_role() {
    let (a, b, c, extra) = team();
    let (mut r, root) = Repo::governed(&extra);
    let rules = r.rules.clone();
    let g = r.change(root, "g", &[("ci/check.py", Some("print('still ok')\n".into()))]);
    let reasons = seal_boundaries(&r.store, &rules, &g).unwrap();
    let specs = |hs: &[&Human]| hs.iter().map(|h| h.seal(&rules, &g, &reasons, FAR).spec).collect::<Vec<_>>();
    let t = tally(&rules, &reasons, &specs(&[&a, &a]));
    assert_eq!((t[0].have.clone(), t[0].need, met(&t)), (vec!["alice".to_string()], 2, false), "one key twice counts once");
    assert!(!met(&tally(&rules, &reasons, &specs(&[&a, &c]))), "the DBA is not a maintainer");
    assert!(met(&tally(&rules, &reasons, &specs(&[&b, &a]))));

    let both = vec!["governance: ci/check.py".to_string(), "irreversible: migrations/x.sql: drop table t;".into()];
    assert!(!met(&tally(&rules, &both, &specs(&[&a, &b]))), "irreversible SQL also needs the DBA");
    assert!(met(&tally(&rules, &both, &specs(&[&a, &b, &c]))));
    let t = tally(&rules, &["ratchet: tests/x.py: removed test_a".to_string()], &specs(&[&c]));
    assert!(met(&t) && t[0].need == 1 && t[0].roles.is_empty(), "a kind without a quorum needs any one approver");

    let s = a.seal(&rules, &g, &reasons, FAR).spec;
    assert_eq!(seal_problem(&rules, &s, &g, &reasons, 1_000), None);
    assert!(seal_problem(&rules, &s, &g, &reasons, FAR).is_some(), "expired");
    assert!(seal_problem(&rules, &s, &g, &["governance: other".into()], 1_000).is_some(), "signer can't pick the reasons");
    let outsider = Human::new("mallory").seal(&rules, &g, &reasons, FAR).spec;
    assert!(seal_problem(&rules, &outsider, &g, &reasons, 1_000).is_some(), "not enrolled");
}

#[test]
fn sealed_landings_verify_from_trunk_alone() {
    let (a, b, c, extra) = team();
    let (mut r, root) = Repo::governed(&extra);
    let rules = r.rules.clone();
    let land = |r: &mut Repo, mut input: ChangeInput, seals: &[Seal]| {
        let reasons = seal_boundaries(&r.store, &rules, &input).unwrap();
        if !seals.is_empty() {
            input.trailers = seal_trailers(&rules, &input.commit, &reasons, seals);
        }
        apply_change(&mut r.store, &rules, root, &input, &sig("nerudo")).unwrap().commit
    };

    let g = r.change(root, "g", &[("ci/check.py", Some("print('still ok')\n".into()))]);
    let reasons = seal_boundaries(&r.store, &rules, &g).unwrap();
    let out = land(&mut r, g.clone(), &[a.seal(&rules, &g, &reasons, FAR), b.seal(&rules, &g, &reasons, FAR)]);
    let ok = verify_landing(&r.store, "t", &root, &out).unwrap();
    assert_eq!((ok.kinds, ok.signers), (vec!["governance".to_string()], vec!["alice".to_string(), "bob".into()]));
    let msg = r.store.commit(&out).unwrap().message;
    assert!(msg.contains("Sealed-By: alice ") && msg.ends_with("Landed-By: nerudo\n"), "{msg}");
    let with_dba = seal_trailers(&rules, &g.commit, &reasons, &[a.seal(&rules, &g, &reasons, FAR), c.seal(&rules, &g, &reasons, FAR), b.seal(&rules, &g, &reasons, FAR)]);
    assert!(!with_dba.iter().any(|t| t.starts_with("Sealed-By: carol")), "a seal that doesn't count isn't recorded as approval");

    let one = land(&mut r, g.clone(), &[a.seal(&rules, &g, &reasons, FAR)]);
    assert!(verify_landing(&r.store, "t", &root, &one).unwrap_err().contains("needs 2 maintainer for governance, has alice"));
    let none = land(&mut r, g.clone(), &[]);
    assert!(verify_landing(&r.store, "t", &root, &none).unwrap_err().contains("has none"), "an unsealed CI edit is caught");
    let wrong_trunk = verify_landing(&r.store, "other", &root, &out).unwrap_err();
    assert!(wrong_trunk.contains("bad signature"), "{wrong_trunk}");

    // The migration is renamed on trunk, so the verifier matches reasons by kind.
    let d = r.change(root, "d", &[("migrations/NEXT_drop.sql", Some("alter table users drop column x;\n".into()))]);
    let dr = seal_boundaries(&r.store, &rules, &d).unwrap();
    let out = land(&mut r, d.clone(), &[c.seal(&rules, &d, &dr, FAR)]);
    assert_eq!(verify_landing(&r.store, "t", &root, &out).unwrap().signers, vec!["carol".to_string()]);

    let clean = r.change(root, "c", &[("targets/t00/src.txt", Some("q\n".into()))]);
    let out = land(&mut r, clone_input(&clean), &[]);
    assert!(verify_landing(&r.store, "t", &root, &out).unwrap().kinds.is_empty());

    // Break-glass may rewrite nerudo.toml and nothing else.
    let toml = format!("{RULES}\n{extra}\n# recovered\n");
    let mut rules_only = r.change(root, "bg", &[("nerudo.toml", Some(toml))]);
    rules_only.trailers = vec![format!("{BREAK_GLASS}: quorum lost")];
    let out = apply_change(&mut r.store, &rules, root, &rules_only, &sig("nerudo")).unwrap().commit;
    assert_eq!(verify_landing(&r.store, "t", &root, &out).unwrap().break_glass.as_deref(), Some("quorum lost"));
    let mut ci = clone_input(&g);
    ci.trailers = vec![format!("{BREAK_GLASS}: quorum lost")];
    let out = apply_change(&mut r.store, &rules, root, &ci, &sig("nerudo")).unwrap().commit;
    assert!(verify_landing(&r.store, "t", &root, &out).unwrap_err().contains("changes more than nerudo.toml"));
}

#[test]
fn approvals_signed_under_an_earlier_form_of_the_digest_still_count() {
    let (a, b, _, extra) = team();
    let (mut r, root) = Repo::governed(&extra);
    let rules = r.rules.clone();
    let g = r.change(root, "g", &[("ci/check.py", Some("print('still ok')\n".into()))]);
    let reasons = seal_boundaries(&r.store, &rules, &g).unwrap();
    // Alice signed before an upgrade, against the digest 46e1d5d advertised; Bob signs after.
    let resign = |h: &Human, digest: &str| {
        let spec = SealSpec { rules_digest: digest.to_string(), ..h.seal(&rules, &g, &reasons, FAR).spec };
        Seal { signature: hex(&h.key.sign(&spec.signing_bytes()).to_bytes()), spec }
    };
    let forms = rules.digests();
    assert!(forms.len() > 1, "the bench rules parse back from an earlier form");
    let old = resign(&a, &forms[1]);
    assert_eq!(seal_problem(&rules, &old.spec, &g, &reasons, 1_000), None);
    let input = ChangeInput { trailers: seal_trailers(&rules, &g.commit, &reasons, &[old, b.seal(&rules, &g, &reasons, FAR)]), ..g.clone() };
    let out = apply_change(&mut r.store, &rules, root, &input, &sig("nerudo")).unwrap().commit;
    assert_eq!(verify_landing(&r.store, "t", &root, &out).unwrap().signers, vec!["alice".to_string(), "bob".into()]);

    // A digest of other rules is refused, at the gate and on trunk.
    let edited = Rules::parse(&format!("{}\n{extra}", RULES.replace("[seals]\n", "[seals]\napproval = [\"**\"]\n"))).unwrap();
    let stale = resign(&a, &edited.digest());
    assert!(seal_problem(&rules, &stale.spec, &g, &reasons, 1_000).is_some());
    let input = ChangeInput { trailers: seal_trailers(&rules, &g.commit, &reasons, &[stale, b.seal(&rules, &g, &reasons, FAR)]), ..g.clone() };
    let out = apply_change(&mut r.store, &rules, root, &input, &sig("nerudo")).unwrap().commit;
    let err = verify_landing(&r.store, "t", &root, &out).unwrap_err();
    assert!(err.contains("alice's seal: bad signature"), "{err}");
}

#[test]
fn approvals_over_each_form_of_the_digest_pass_the_same_checks_as_the_gate() {
    // The checks the sequencer makes: check_signed when a seal or an allowance arrives
    // (approver, accepts, signature), tally_for at the gate (seal_problem, then tally), and
    // the allowance budget (covers). Then verify_landing on trunk.
    let (a, b, _, extra) = team();
    let (mut r, root) = Repo::governed(&extra);
    let rules = r.rules.clone();
    let forms = rules.digests();
    assert_eq!(forms.len(), 4, "the canonical form, and the forms of 46e1d5d (07ae487 too), e468cda and e2523c3");
    let g = r.change(root, "g", &[("ci/check.py", Some("print('still ok')\n".into()))]);
    let reasons = seal_boundaries(&r.store, &rules, &g).unwrap();
    for (i, d) in forms.iter().enumerate() {
        let seals: Vec<Seal> = [&a, &b]
            .iter()
            .map(|h| {
                let spec = SealSpec { rules_digest: d.clone(), ..h.seal(&rules, &g, &reasons, FAR).spec };
                Seal { signature: hex(&h.key.sign(&spec.signing_bytes()).to_bytes()), spec }
            })
            .collect();
        for s in &seals {
            assert!(rules.approver(&s.spec.approver).is_some() && rules.accepts(&s.spec.rules_digest), "form {i}");
            auth::verify(&s.spec.approver, &s.spec.signing_bytes(), &s.signature).unwrap();
            assert_eq!(seal_problem(&rules, &s.spec, &g, &reasons, 1_000), None, "form {i}");
        }
        let specs: Vec<SealSpec> = seals.iter().map(|s| s.spec.clone()).collect();
        assert!(met(&tally(&rules, &reasons, &specs)), "form {i}: two maintainers sealed");
        let input = ChangeInput { trailers: seal_trailers(&rules, &g.commit, &reasons, &seals), ..g.clone() };
        let out = apply_change(&mut r.store, &rules, root, &input, &sig("nerudo")).unwrap().commit;
        assert_eq!(verify_landing(&r.store, "t", &root, &out).unwrap().signers, vec!["alice".to_string(), "bob".into()], "form {i}");

        let spec = AllowanceSpec {
            id: format!("al-{i}"),
            trunk: "t".into(),
            units: vec!["targets/*".into()],
            max_landings: 1,
            expires_at_ms: FAR,
            rules_digest: d.clone(),
            approver: a.pk(),
        };
        let signature = hex(&a.key.sign(&spec.signing_bytes()).to_bytes());
        auth::verify(&spec.approver, &spec.signing_bytes(), &signature).unwrap();
        let al = Allowance { spec, signature, reserved: 0, spent: 0 };
        assert!(al.covers(&["targets/t00".to_string()].into_iter().collect(), 1_000, &forms), "form {i}");
    }
}

fn clone_input(c: &ChangeInput) -> ChangeInput {
    ChangeInput { trailers: vec![], ..c.clone() }
}

#[test]
fn a_rules_file_that_does_not_parse_fails_the_trunk_check() {
    let (a, _, _, extra) = team();
    let (mut r, root) = Repo::governed(&extra);
    let broken = format!("{RULES}\n{}\n[seals.require]\ngovernance = {{ count = 3 }}\n", a.entry(""));
    let ch = r.change(root, "x", &[("nerudo.toml", Some(broken))]);
    let rules = r.rules.clone();
    let out = apply_change(&mut r.store, &rules, root, &ch, &sig("nerudo")).unwrap();
    let files = r.store.flatten(&out.tree).unwrap();
    assert!(trunk_checks(&r.store, &rules, &files).unwrap_err().contains("count 3 but 1 approvers"));
}

#[test]
fn ci_typecheck_takes_a_tsconfig_inside_the_repo() {
    let r = Rules::parse(&format!("{RULES}\n[ci]\ntypecheck = {{ tsconfig = \"web/tsconfig.json\" }}\n")).unwrap();
    assert_eq!(r.ci.typecheck.unwrap().tsconfig, "web/tsconfig.json");
    assert!(Rules::parse(RULES).unwrap().ci.typecheck.is_none(), "no [ci]: only the simulated CI time");
    for bad in ["", "/etc/tsconfig.json", "../tsconfig.json", "a//tsconfig.json", "./tsconfig.json"] {
        let text = format!("{RULES}\n[ci]\ntypecheck = {{ tsconfig = {bad:?} }}\n");
        assert!(Rules::parse(&text).unwrap_err().contains("ci.typecheck.tsconfig"), "{bad:?} was accepted");
    }
}

#[test]
fn the_agents_why_lands_and_cannot_forge_trailers() {
    let (mut r, root) = Repo::new();
    let mut ch = r.change(root, "w1", &[("targets/t00/src.txt", Some("a\nB\nc\n".into()))]);
    let mut c = r.store.commit(&ch.commit).unwrap();
    c.message = "w1: capital B\n\nThe parser needs a capital B.\n\nChange-Id: forged\nSealed-By: mallory 00 0 00\nBreak-Glass: trust me\n".into();
    ch.commit = r.store.put_commit(&c);
    let out = apply_change(&mut r.store, &r.rules, root, &ch, &sig("nerudo")).unwrap();
    let landed = r.store.commit(&out.commit).unwrap();
    assert!(landed.message.starts_with("w1: capital B\n\nThe parser needs a capital B.\n"), "{}", landed.message);
    assert_eq!(landed.trailers("Change-Id"), vec!["w1".to_string()]);
    assert_eq!(landed.trailer("Revision").as_deref(), Some("1"));
    assert!(landed.trailers("Sealed-By").is_empty() && landed.trailer("Break-Glass").is_none());
    assert!(landed.body().starts_with("The parser needs a capital B."), "{}", landed.body());
}

#[test]
fn a_decision_models_review_reason_must_be_sealed_to_verify() {
    let (a, _, _, extra) = team();
    let (mut r, root) = Repo::governed(&extra);
    let rules = r.rules.clone();
    // No path rule asks for a seal; the decision model asked for a review.
    let ch = r.change(root, "dm", &[("targets/t00/src.txt", Some("q\n".into()))]);
    let reasons = vec!["review: decision model chose review, p 0.87 >= 0.80 (fixed/fixed)".to_string()];
    let mut sealed = clone_input(&ch);
    sealed.trailers = seal_trailers(&rules, &ch.commit, &reasons, &[a.seal(&rules, &ch, &reasons, FAR)]);
    let out = apply_change(&mut r.store, &rules, root, &sealed, &sig("nerudo")).unwrap().commit;
    let ok = verify_landing(&r.store, "t", &root, &out).unwrap();
    assert_eq!((ok.kinds, ok.signers), (vec!["review".to_string()], vec!["alice".to_string()]));

    let mut unsealed = clone_input(&ch);
    unsealed.trailers = seal_trailers(&rules, &ch.commit, &reasons, &[]);
    let out = apply_change(&mut r.store, &rules, root, &unsealed, &sig("nerudo")).unwrap().commit;
    assert!(verify_landing(&r.store, "t", &root, &out).unwrap_err().contains("for review, has none"));
}

#[test]
fn git_branch_multicommit_delta_keeps_tip_metadata_and_exact_seal() {
    let (approver, second_approver, _, extra) = team();
    let (mut r, root) = Repo::governed(&extra);
    let first = r.change(root, "first", &[("ci/check.py", Some("print('checked')\n".into()))]);
    let mut tip = r.change(first.commit, "tip", &[("targets/t00/src.txt", Some("a\nB\nc\n".into()))]);
    tip.base = root;
    tip.rev = 2;
    let mut original = r.store.commit(&tip.commit).unwrap();
    original.message = "finish the branch\n\nKeep both commits in this task.\n".into();
    tip.commit = r.store.put_commit(&original);
    let reasons = seal_boundaries(&r.store, &r.rules, &tip).unwrap();
    assert!(reasons.iter().any(|r| r == "governance: ci/check.py"), "earlier commits must be gated: {reasons:?}");
    tip.trailers = seal_trailers(&r.rules, &tip.commit, &reasons, &[approver.seal(&r.rules, &tip, &reasons, FAR), second_approver.seal(&r.rules, &tip, &reasons, FAR)]);
    let landed = apply_change(&mut r.store, &r.rules, root, &tip, &sig("nerudo")).unwrap();
    assert_eq!(r.file(landed.commit, "ci/check.py").as_deref(), Some("print('checked')\n"));
    assert_eq!(r.file(landed.commit, "targets/t00/src.txt").as_deref(), Some("a\nB\nc\n"));
    let commit = r.store.commit(&landed.commit).unwrap();
    assert_eq!(commit.author, original.author);
    assert!(commit.message.starts_with(&original.message));
    assert_eq!(commit.trailer("Revision").as_deref(), Some("2"));
    verify_landing(&r.store, "t", &root, &landed.commit).expect("the seal covers the original branch tip");
}

#[test]
fn git_branch_multicommit_updates_and_successors_keep_the_complete_delta() {
    let (mut r, root) = Repo::new();
    let first = r.change(root, "branch", &[("CHANGELOG.md", Some("# Changelog\n- first\n".into())), ("targets/t00/first.txt", Some("first\n".into()))]);
    let second = r.change(first.commit, "second", &[("targets/t01/second.txt", Some("second\n".into()))]);
    let mut third = r.change(second.commit, "branch", &[("CHANGELOG.md", Some("# Changelog\n- first\n- third\n".into()))]);
    // Updating an open PR keeps its original base, even after several local commits.
    third.base = root;
    third.rev = 2;
    let all = apply_change(&mut r.store, &r.rules, root, &third, &sig("nerudo")).unwrap();
    let retry = apply_change(&mut r.store, &r.rules, root, &third, &sig("nerudo")).unwrap();
    assert_eq!(all.commit, retry.commit, "retrying the same input is deterministic");
    assert_eq!(r.file(all.commit, "targets/t00/first.txt").as_deref(), Some("first\n"));
    assert_eq!(r.file(all.commit, "targets/t01/second.txt").as_deref(), Some("second\n"));
    // If the first PR landed, its original tip becomes the successor's base. Earlier
    // special-path edits must not be replayed while later local commits still land.
    let first_landed = apply_change(&mut r.store, &r.rules, root, &first, &sig("nerudo")).unwrap();
    third.base = first.commit;
    third.change_id = "branch..2".into();
    third.rev = 1;
    let successor = apply_change(&mut r.store, &r.rules, first_landed.commit, &third, &sig("nerudo")).unwrap();
    assert_eq!(r.file(successor.commit, "CHANGELOG.md").as_deref(), Some("# Changelog\n- first\n- third\n"));
    assert_eq!(r.file(successor.commit, "targets/t01/second.txt").as_deref(), Some("second\n"));
}

#[test]
fn git_branch_unrelated_base_still_fails_closed() {
    let (mut r, root) = Repo::new();
    let first = r.change(root, "first", &[]);
    let mut tip = r.change(first.commit, "tip", &[]);
    let unrelated = r.change(root, "unrelated", &[]);
    tip.base = unrelated.commit;
    assert!(matches!(footprint(&r.store, &r.rules, &tip), Err(Ejection::BadBase { .. })));
}

#[test]
fn git_branch_lazy_history_is_missing_until_its_parent_arrives() {
    let (mut r, root) = Repo::new();
    let first = r.change(root, "first", &[("targets/t00/first.txt", Some("first\n".into()))]);
    let mut tip = r.change(first.commit, "tip", &[("targets/t01/second.txt", Some("second\n".into()))]);
    tip.base = root;
    let keep = r.store.new_objects(&tip.commit, &Oid::ZERO).unwrap().into_iter().filter(|o| *o != first.commit).collect();
    let mut lazy = r.store.clone();
    lazy.retain(&keep);
    let err = footprint(&lazy, &r.rules, &tip).unwrap_err();
    assert!(matches!(&err, Ejection::Missing { detail } if detail.contains(&first.commit.to_hex())), "{err:?}");
    assert!(lazy.take_misses().iter().any(|(oid, kind)| *oid == first.commit && *kind == "commit"));
    lazy.insert(r.store.get(&first.commit).unwrap().clone());
    assert_eq!(footprint(&lazy, &r.rules, &tip).unwrap().len(), 2);
}

#[test]
fn git_branch_missing_merge_parent_does_not_hide_a_known_base() {
    let (mut r, root) = Repo::new();
    let first = r.change(root, "first", &[]);
    let mut tip = r.change(first.commit, "tip", &[]);
    let mut commit = r.store.commit(&tip.commit).unwrap();
    commit.parents.insert(0, Oid::from_hex(&"ab".repeat(20)).unwrap());
    tip.commit = r.store.put_commit(&commit);
    tip.base = root;
    assert!(footprint(&r.store, &r.rules, &tip).is_ok());
}

#[test]
fn git_branch_ancestry_walk_is_bounded() {
    use nerudo_core::merge::MAX_BASE_COMMITS;
    let (mut r, root) = Repo::new();
    let mut commit = r.store.commit(&root).unwrap();
    let mut parent = root;
    for n in 0..=MAX_BASE_COMMITS {
        commit.parents = vec![parent];
        commit.message = format!("branch commit {n}\n");
        parent = r.store.put_commit(&commit);
    }
    let tip = ChangeInput { change_id: "long".into(), rev: 1, commit: parent, base: root, trailers: vec![] };
    assert!(matches!(footprint(&r.store, &r.rules, &tip), Err(Ejection::BadBase { detail }) if detail.contains("exceeds")));
}
