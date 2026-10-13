//! Deterministic gates. Seal boundaries decide whether a change needs a human seal;
//! trunk checks are invariants the tip must satisfy after every batch. Both always run
//! from the trunk's rules, never from files inside the change.

use crate::glob::matches;
use crate::merge::ChangeInput;
use crate::rules::Rules;
use nerudo_git::{FileEntry, Store};
use std::collections::{BTreeMap, BTreeSet};

type Flat = BTreeMap<String, FileEntry>;

/// The text of a file. A file whose content is not in the store is an error, never empty
/// text: a gate must not pass because it could not read what it checks.
fn text(store: &Store, e: Option<&FileEntry>) -> Result<String, String> {
    match e {
        None => Ok(String::new()),
        Some(e) => store.blob(&e.oid).map(|b| String::from_utf8_lossy(b).into_owned()).map_err(|e| e.to_string()),
    }
}

/// Whether `seal_boundaries` reads the file at the change's base for an edit of `path`: a
/// test that must keep its assertions, or a migration whose new lines it reads.
pub fn reads_base(rules: &Rules, path: &str) -> bool {
    rules.seals.tests.iter().chain(rules.seals.migrations.iter()).any(|g| matches(g, path))
}

/// Reasons this change needs a human seal (empty = an allowance is enough).
pub fn seal_boundaries(store: &Store, rules: &Rules, change: &ChangeInput) -> Result<Vec<String>, String> {
    let tree = |c: &nerudo_git::Oid| store.commit(c).map(|c| c.tree).map_err(|e| e.to_string());
    let edited = store.diff(Some(&tree(&change.base)?), Some(&tree(&change.commit)?)).map_err(|e| e.to_string())?;
    let base: Flat = edited.iter().filter_map(|(p, b, _)| b.map(|e| (p.clone(), e))).collect();
    let ours: Flat = edited.iter().filter_map(|(p, _, c)| c.map(|e| (p.clone(), e))).collect();
    let changed: Vec<&String> = edited.iter().map(|(p, ..)| p).collect();
    let mut reasons = vec![];
    for p in &changed {
        // The rules themselves (under either name) are governance whatever the globs say: a
        // change that rewrote the approvers or the quorums without a seal would gate the next
        // batch under its own rules, and a filename migration cannot step around the gate.
        if crate::rules::is_rules_path(p) || rules.seals.governance.iter().any(|g| matches(g, p)) {
            reasons.push(format!("governance: {p}"));
        }
        if rules.seals.tests.iter().any(|g| matches(g, p)) && base.contains_key(*p) {
            let before = text(store, base.get(*p))?;
            let after = text(store, ours.get(*p))?;
            reasons.extend(ratchet(&rules.seals.test_style, p, &before, &after));
        }
        if rules.seals.migrations.iter().any(|g| matches(g, p)) {
            let before = text(store, base.get(*p))?;
            let after = text(store, ours.get(*p))?;
            let old: BTreeSet<&str> = before.lines().collect();
            for line in after.lines().filter(|l| !old.contains(l)) {
                let code = line.split("--").next().unwrap_or("").to_ascii_lowercase();
                if ["drop table", "drop column", "delete from", "truncate"].iter().any(|k| code.contains(k)) {
                    reasons.push(format!("irreversible: {p}: {}", line.trim()));
                }
            }
        }
    }
    // One reason for the whole change, however many files it touches.
    let gated: Vec<&&String> = changed.iter().filter(|p| rules.seals.approval.iter().any(|g| matches(g, p))).collect();
    if let Some(p) = gated.first() {
        reasons.push(match gated.len() - 1 {
            0 => format!("approval: {p}"),
            more => format!("approval: {p} and {more} more"),
        });
    }
    // Pending migrations are added files, not edits; check them even if no glob names them.
    for p in changed.iter().filter(|p| rules.pending(p).is_some() && !rules.seals.migrations.iter().any(|g| matches(g, p))) {
        for line in text(store, ours.get(*p))?.lines() {
            let code = line.split("--").next().unwrap_or("").to_ascii_lowercase();
            if ["drop table", "drop column", "delete from", "truncate"].iter().any(|k| code.contains(k)) {
                reasons.push(format!("irreversible: {p}: {}", line.trim()));
            }
        }
    }
    Ok(reasons)
}

/// Existing tests must not disappear, be skipped, or have their assertions changed.
fn ratchet(style: &str, path: &str, before: &str, after: &str) -> Vec<String> {
    let (old, new) = (tests(style, before), tests(style, after));
    let mut out = vec![];
    for (name, body) in &old {
        match new.get(name) {
            None => out.push(format!("ratchet: {path}: removed {name}")),
            Some(nb) => {
                let a: Vec<&String> = body.iter().filter(|l| is_assert(style, l)).collect();
                let b: Vec<&String> = nb.iter().filter(|l| is_assert(style, l)).collect();
                if a != b {
                    out.push(format!("ratchet: {path}: changed assertions in {name}"));
                }
            }
        }
    }
    let skips = |s: &str| s.lines().filter(|l| is_skip(style, l.trim())).count();
    if skips(after) > skips(before) {
        out.push(format!("ratchet: {path}: added a skip"));
    }
    out
}

fn is_assert(style: &str, l: &str) -> bool {
    match style {
        "rust" => l.contains("assert"),
        _ => l.contains("assert") || l.contains("self.fail("),
    }
}

fn is_skip(style: &str, l: &str) -> bool {
    match style {
        "rust" => l.starts_with("#[ignore"),
        _ => l.starts_with("@unittest.skip") || l.starts_with("@unittest.expectedFailure") || l.starts_with("@pytest.mark.skip"),
    }
}

/// Test name -> body lines, for the supported styles.
fn tests(style: &str, src: &str) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    let mut cur: Option<(String, Vec<String>)> = None;
    let mut pending_test_attr = false;
    for line in src.lines() {
        let t = line.trim();
        let start = match style {
            "rust" => {
                if t.starts_with("#[test]") {
                    pending_test_attr = true;
                    None
                } else if pending_test_attr && (t.starts_with("fn ") || t.starts_with("async fn ")) {
                    pending_test_attr = false;
                    t.trim_start_matches("async ").strip_prefix("fn ").and_then(|r| r.split('(').next()).map(str::to_string)
                } else {
                    None
                }
            }
            _ => t.strip_prefix("def ").filter(|r| r.starts_with("test")).and_then(|r| r.split('(').next()).map(str::to_string),
        };
        let ends = style != "rust" && (line.starts_with("class ") || line.starts_with("if __name__") || (t.starts_with("def ") && start.is_none()));
        if start.is_some() || ends {
            if let Some((n, b)) = cur.take() {
                out.insert(n, b);
            }
        }
        if let Some(n) = start {
            cur = Some((n, vec![]));
        } else if let Some((_, b)) = cur.as_mut() {
            if !t.is_empty() {
                b.push(t.to_string());
            }
        }
    }
    if let Some((n, b)) = cur {
        out.insert(n, b);
    }
    out
}

/// The files of `tree` that `trunk_checks` reads, by full path: nerudo.toml, the directory of
/// each numbering and pending check, and each derived file and its source directory. Checking
/// these gives the same result as checking the whole flattened tree, without flattening it.
/// Err(Missing) names a file whose content the store lacks.
pub fn check_files(store: &Store, rules: &Rules, tree: &nerudo_git::Oid) -> Result<Flat, nerudo_git::Error> {
    let mut files = Flat::new();
    let mut read = vec![];
    for path in [crate::rules::RULES_PATH, crate::rules::LEGACY_RULES_PATH] {
        if let Some(e) = store.file(tree, path)? {
            files.insert(path.into(), e);
            read.push(e);
        }
    }
    for c in &rules.checks {
        if matches!(c.kind.as_str(), "unique-numbering" | "no-pending") {
            files.extend(store.flatten_dir(tree, &c.dir)?);
        }
    }
    if rules.checks.iter().any(|c| c.kind == "derived-consistent") {
        for d in &rules.derived {
            let source = store.flatten_dir(tree, &d.source)?;
            read.extend(source.values().copied());
            files.extend(source);
            if let Some(e) = store.file(tree, &d.path)? {
                files.insert(d.path.clone(), e);
                read.push(e);
            }
        }
    }
    if let Some(e) = read.iter().find(|e| !store.contains(&e.oid)) {
        return Err(nerudo_git::Error::Missing(e.oid.to_hex()));
    }
    Ok(files)
}

/// Invariants the trunk tip must satisfy. Returns the first violation.
pub fn trunk_checks(store: &Store, rules: &Rules, files: &Flat) -> Result<(), String> {
    // The sequencer reads its next rules from here, so they must always parse.
    if let Some((_, e)) = crate::rules::rules_entry(files)? {
        Rules::parse(&text(store, Some(e))?)?;
    }
    for c in &rules.checks {
        match c.kind.as_str() {
            "unique-numbering" => {
                let prefix = format!("{}/", c.dir);
                let mut seen: BTreeMap<String, &String> = BTreeMap::new();
                for k in files.keys().filter(|k| k.starts_with(&prefix)) {
                    let name = &k[prefix.len()..];
                    if let Some(num) = name.split('_').next().filter(|n| n.chars().all(|ch| ch.is_ascii_digit()) && !n.is_empty()) {
                        if let Some(prev) = seen.insert(num.to_string(), k) {
                            return Err(format!("duplicate number {num}: {prev} and {k}"));
                        }
                    }
                }
            }
            "no-pending" => {
                let prefix = format!("{}/{}", c.dir, c.prefix);
                if let Some(k) = files.keys().find(|k| k.starts_with(&prefix)) {
                    return Err(format!("pending file on trunk: {k}"));
                }
            }
            "derived-consistent" => {
                for d in &rules.derived {
                    let want = crate::merge::digest_index(store, files, &d.source).map_err(|e| format!("{e:?}"))?;
                    let have = text(store, files.get(&d.path))?;
                    if want != have {
                        return Err(format!("{} is stale", d.path));
                    }
                }
            }
            other => return Err(format!("unknown check {other:?}")),
        }
    }
    Ok(())
}
