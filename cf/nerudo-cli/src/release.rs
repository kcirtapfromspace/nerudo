//! `nerudo release`: sign a release of trunk with your key, and check release tags from
//! outside. The sequencer drafts the release (the next version and the notes), you read it
//! and sign the statement for that exact tag, commit and notes, and the sequencer pushes
//! the annotated tag once the release quorum in nerudo.toml has signed. The key never
//! leaves this machine.

use crate::{die, hex, load_key, now_ms, ttl_ms, Api};
use ed25519_dalek::Signer;
use futures::executor::block_on;
use nerudo_core::release::{notes_digest, verify_release, ReleaseSpec, Released};
use nerudo_core::{Rules, RULES_PATH};
use nerudo_git::{fetch, ls_refs, Oid, Remote, Store, Transport};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::io::{BufRead, Write};

pub(crate) struct Options {
    pub version: Option<String>,
    /// A landing number, or a trunk commit (at least 7 hex characters).
    pub at: Option<String>,
    pub notes_file: Option<String>,
    pub title: Option<String>,
    pub prerelease: bool,
    pub yes: bool,
    pub ttl: String,
}

/// The landing number for `at`: a number, or the landing whose trunk commit starts with it.
fn landing_of(api: &Api, at: &str) -> u64 {
    if let Ok(n) = at.parse::<u64>() {
        return n;
    }
    if at.len() < 7 || !at.chars().all(|c| c.is_ascii_hexdigit()) {
        die("--at takes a landing number or a trunk commit (7+ hex characters)");
    }
    api.landing_log()
        .into_iter()
        .find(|e| e.commit_out.starts_with(at))
        .map(|e| e.seq)
        .unwrap_or_else(|| die(&format!("no landing on trunk is commit {at}")))
}

/// Show the draft, ask, sign, and send. None: the person said no.
pub(crate) fn release(api: &Api, trunk: &str, o: &Options) -> Option<Value> {
    let key = load_key();
    let mut q = vec![];
    if let Some(at) = &o.at {
        q.push(format!("at={}", landing_of(api, at)));
    }
    if let Some(v) = &o.version {
        q.push(format!("version={v}"));
    }
    let path = if q.is_empty() { "releases/draft".to_string() } else { format!("releases/draft?{}", q.join("&")) };
    let d = api.call("GET", &path, None);
    let tag = d["tag"].as_str().unwrap_or_else(|| die(&format!("no draft: {d}"))).to_string();
    let notes = match &o.notes_file {
        Some(f) => std::fs::read_to_string(f).unwrap_or_else(|e| die(&format!("{f}: {e}"))),
        None => d["notes"].as_str().unwrap_or_default().to_string(),
    };
    let title = o.title.clone().unwrap_or_else(|| d["title"].as_str().unwrap_or(&tag).to_string());
    let commit = d["commit"].as_str().unwrap_or_default().to_string();
    let seq = d["seq"].as_u64().unwrap_or(0);
    let previous = d["previous"].as_str().map(str::to_string);
    let s = &d["stats"];
    println!(
        "release {tag} at landing {seq}, commit {}{}",
        &commit[..commit.len().min(12)],
        previous.as_deref().map(|p| format!(", after {p}")).unwrap_or_else(|| ", the first release".into())
    );
    println!(
        "  {} PRs from {} agents in {} batches, {} CI runs; {} files changed, +{} −{}",
        s["prs"], s["agents"], s["batches"], s["ci_runs"], d["files"]["files"], d["files"]["adds"], d["files"]["dels"]
    );
    let who: Vec<&str> = d["quorum"]["eligible"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    println!("  needs {} signature{} from: {}", d["quorum"]["count"], if d["quorum"]["count"] == 1 { "" } else { "s" }, who.join(", "));
    println!("\n{}\n", notes.lines().map(|l| format!("  | {l}")).collect::<Vec<_>>().join("\n"));
    if !o.yes {
        print!("Sign {tag} for this exact commit and these notes? [y/N] ");
        std::io::stdout().flush().unwrap();
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line).unwrap();
        if !matches!(line.trim(), "y" | "Y" | "yes") {
            return None;
        }
    }
    let spec = ReleaseSpec {
        trunk: trunk.to_string(),
        tag: tag.clone(),
        commit,
        seq,
        title,
        notes_digest: notes_digest(&notes),
        previous,
        prerelease: o.prerelease,
        rules_digest: d["rules_digest"].as_str().unwrap_or_default().to_string(),
        expires_at_ms: now_ms() + ttl_ms(&o.ttl),
        approver: hex(&key.verifying_key().to_bytes()),
    };
    let sig = hex(&key.sign(&spec.signing_bytes()).to_bytes());
    let out = api.call("POST", "releases", Some(json!({ "spec": spec, "signature": sig, "notes": notes })));
    let have: Vec<&str> = out["quorum"]["have"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    if out["complete"] == true {
        println!("released {tag}: the sequencer pushed refs/tags/{tag} (tag object {}), signed by {}", out["release"]["tag_object"].as_str().map(|t| &t[..12]).unwrap_or("?"), have.join(", "));
    } else {
        println!("signed {tag}: {} of {} signatures ({}); it is cut when the rest sign", have.len(), out["quorum"]["need"], have.join(", "));
    }
    Some(out)
}

/// One tag on trunk, checked.
pub(crate) enum Checked {
    /// A nerudo release whose signatures verify and meet the quorum.
    Release(Released),
    /// A release tag that does not verify.
    Bad(String, String),
    /// A tag that is not a nerudo release (a plain tag, or one from before nerudo).
    Other,
}

/// Every tag on trunk, checked against `store` (trunk's history, fetched already):
/// a release tag must point at a commit on trunk (`chain`) and verify against the
/// nerudo.toml at that commit.
pub(crate) fn check_tags<T: Transport>(t: &T, remote: &Remote, trunk: &str, store: &Store, chain: &[Oid]) -> Vec<Checked> {
    let refs = match block_on(ls_refs(t, remote, "refs/tags/")) {
        Ok(r) => r,
        Err(e) => return vec![Checked::Bad("refs/tags/".into(), format!("can't list tags: {e}"))],
    };
    let refs: Vec<(String, Oid)> = refs.into_iter().filter(|(n, _)| !n.ends_with("^{}")).collect();
    if refs.is_empty() {
        return vec![];
    }
    // Only the tag objects come over: we already have trunk.
    let mut tags = Store::new();
    let wants: Vec<Oid> = refs.iter().map(|(_, o)| *o).filter(|o| !store.contains(o)).collect();
    let haves: Vec<Oid> = chain.last().copied().into_iter().collect();
    if !wants.is_empty() {
        if let Err(e) = block_on(fetch(t, remote, &wants, &haves, &mut tags)) {
            return vec![Checked::Bad("refs/tags/".into(), format!("can't fetch tags: {e}"))];
        }
    }
    let on_trunk: BTreeSet<&Oid> = chain.iter().collect();
    let mut out = vec![];
    for (name, oid) in refs {
        let short = name.trim_start_matches("refs/tags/").to_string();
        let Ok(tag) = tags.tag(&oid) else {
            out.push(Checked::Other);
            continue;
        };
        if tag.trailer(nerudo_core::release::RELEASE_SEQ).is_none() {
            out.push(Checked::Other);
            continue;
        }
        if !on_trunk.contains(&tag.object) {
            out.push(Checked::Bad(short, format!("points at {}, which is not on trunk", tag.object)));
            continue;
        }
        let rules = store
            .commit(&tag.object)
            .and_then(|c| store.flatten(&c.tree))
            .ok()
            .and_then(|f| nerudo_core::rules::rules_entry(&f).ok().flatten().map(|(_, e)| *e))
            .and_then(|e| store.blob(&e.oid).ok().map(|b| String::from_utf8_lossy(b).into_owned()))
            .ok_or_else(|| format!("no {RULES_PATH} at {}", tag.object))
            .and_then(|text| Rules::parse(&text));
        match rules.and_then(|r| verify_release(&tag, trunk, &r)) {
            Ok(r) => out.push(Checked::Release(r)),
            Err(e) => out.push(Checked::Bad(short, e)),
        }
    }
    out
}
