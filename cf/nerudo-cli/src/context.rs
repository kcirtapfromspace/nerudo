//! Agent context: the plan, the decisions and the rejected alternatives an agent records
//! with a change. It is versioned in Artifacts, in the agent's own fork, on an orphan
//! branch `nerudo/context` that holds `changes/<change>/rev-<n>.md`. `land` commits the
//! file there and pushes it with the change; the sequencer links the landed commit to it
//! with a `Context: <fork>@<commit>` trailer.
//!
//! Secrets never go into a context file: known token shapes, Authorization values,
//! `key=value` secrets, URL passwords, PEM private keys, and the exact tokens this process
//! holds are replaced with `[redacted]` before anything is committed.

use futures::executor::block_on;
use nerudo_git::{fetch, ls_refs, Commit, FileEntry, Oid, Remote, Signature, Store, Transport, MODE_BLOB};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::process::{Command, Stdio};

/// The context branch in every agent's fork.
pub const BRANCH: &str = "refs/heads/nerudo/context";
/// The largest context file the sequencer accepts; longer text is cut here.
pub const MAX_BYTES: usize = 200 * 1024;
const REDACTED: &str = "[redacted]";

/// Where a change's context lives on the context branch.
pub fn path(cid: &str, rev: u32) -> String {
    format!("changes/{cid}/rev-{rev}.md")
}

/// What the file says about the change it explains.
pub struct Meta<'a> {
    pub change_id: &'a str,
    pub rev: u32,
    pub agent: &'a str,
    pub task: Option<&'a str>,
    pub commit: &'a str,
    pub base: &'a str,
    pub at_ms: i64,
}

/// The context file: a front-matter block, then the agent's own words (redacted, and cut
/// at MAX_BYTES). `secrets` are exact strings this process knows to be credentials.
pub fn document(m: &Meta, text: &str, secrets: &[String]) -> String {
    let mut head = format!(
        "---\nchange: {}\nrevision: {}\nagent: {}\n",
        m.change_id, m.rev, m.agent
    );
    if let Some(t) = m.task {
        head.push_str(&format!("task: {t}\n"));
    }
    head.push_str(&format!("commit: {}\nbase: {}\nwritten: {}\n---\n\n", m.commit, m.base, iso(m.at_ms)));
    let mut body = redact(text.trim(), secrets);
    let room = MAX_BYTES.saturating_sub(head.len() + 64);
    if body.len() > room {
        let mut cut = room;
        while !body.is_char_boundary(cut) {
            cut -= 1;
        }
        body.truncate(cut);
        body.push_str("\n\n[cut: the context was longer than the limit]");
    }
    head + &body + "\n"
}

/// Replace secrets in `text` with [redacted].
pub fn redact(text: &str, secrets: &[String]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_pem = false;
    for line in text.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\n', '\r']);
        if bare.contains("-----BEGIN") && bare.contains("PRIVATE KEY-----") {
            in_pem = true;
            out.push_str(REDACTED);
            out.push_str(&line[bare.len()..]);
            continue;
        }
        if in_pem {
            if bare.contains("-----END") {
                in_pem = false;
            }
            continue;
        }
        out.push_str(&redact_line(bare));
        out.push_str(&line[bare.len()..]);
    }
    for s in secrets.iter().filter(|s| s.len() >= 8) {
        out = out.replace(s.as_str(), REDACTED);
    }
    out
}

const PREFIXES: [&str; 14] = [
    "sk-", "sk_live_", "sk_test_", "rk_live_", "ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_", "glpat-", "xoxb-", "xoxp-", "AIza",
];
const SECRET_KEYS: [&str; 8] = ["token", "secret", "password", "passwd", "api_key", "apikey", "api-key", "private_key"];

fn redact_line(line: &str) -> String {
    // An Authorization header keeps its name, never its value.
    if let Some(i) = line.to_ascii_lowercase().find("authorization:") {
        let end = i + "authorization:".len();
        return format!("{} {REDACTED}", &line[..end]);
    }
    let mut out = String::with_capacity(line.len());
    // Set by "Bearer" and by a secret key with its value in the next word ("password: x").
    let mut next_is_secret = false;
    for word in split_keep(line) {
        if word.trim().is_empty() {
            out.push_str(word);
            continue;
        }
        let w = word.trim_matches(|c: char| "\"'`,;()[]{}<>".contains(c));
        let secret = !w.is_empty()
            && (next_is_secret
                || PREFIXES.iter().any(|p| w.starts_with(p) && w.len() >= p.len() + 8)
                || (w.starts_with("AKIA") && w.len() == 20 && w[4..].chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()))
                || (w.starts_with("eyJ") && w.matches('.').count() == 2 && w.len() > 40));
        next_is_secret = w.eq_ignore_ascii_case("bearer");
        let replaced = if secret {
            word.replace(w, REDACTED)
        } else if let Some((k, v)) = assignment(w) {
            let secret_key = SECRET_KEYS.iter().any(|s| k.to_ascii_lowercase().contains(s));
            if secret_key && v.is_empty() {
                next_is_secret = true;
                word.to_string()
            } else if secret_key {
                word.replace(v, REDACTED)
            } else {
                word.to_string()
            }
        } else if let Some(at) = url_password(w) {
            word.replace(at, REDACTED)
        } else {
            word.to_string()
        };
        out.push_str(&replaced);
    }
    out
}

/// Words and the whitespace between them, in order.
fn split_keep(s: &str) -> Vec<&str> {
    let mut parts = vec![];
    let mut start = 0;
    let mut space = None;
    for (i, c) in s.char_indices() {
        let is = c.is_whitespace();
        if space != Some(is) && i > start {
            parts.push(&s[start..i]);
            start = i;
        }
        space = Some(is);
    }
    if start < s.len() {
        parts.push(&s[start..]);
    }
    parts
}

/// `key=value` or `key: value` in one word.
fn assignment(w: &str) -> Option<(&str, &str)> {
    let i = w.find(['=', ':'])?;
    let (k, v) = (&w[..i], &w[i + 1..]);
    if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c)) || v.starts_with("//") {
        return None;
    }
    Some((k, v))
}

/// The password in `scheme://user:password@host`.
fn url_password(w: &str) -> Option<&str> {
    let rest = &w[w.find("://")? + 3..];
    let userinfo = &rest[..rest.find('@')?];
    let (_, pass) = userinfo.split_once(':')?;
    (!pass.is_empty() && !userinfo.contains('/')).then_some(pass)
}

/// UTC time as 2026-10-11T16:30:00Z.
pub fn iso(ms: i64) -> String {
    let s = ms.div_euclid(1000);
    let (days, secs) = (s.div_euclid(86_400), s.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, secs % 3600 / 60, secs % 60)
}

// ---------------------------------------------------------------------- the commit

/// The context commit, built in memory (the demo's agents use no git CLI): the parent's
/// files plus `path`.
pub fn commit_native(store: &mut Store, parent: Option<Oid>, path: &str, doc: &str, agent: &str, at_ms: i64) -> Oid {
    let mut files: BTreeMap<String, FileEntry> = match parent.and_then(|p| store.commit(&p).ok()) {
        Some(c) => store.flatten(&c.tree).unwrap_or_default(),
        None => BTreeMap::new(),
    };
    files.insert(path.to_string(), FileEntry { mode: MODE_BLOB, oid: store.put_blob(doc.as_bytes()) });
    let tree = store.write_tree(&files);
    let sig = Signature::new(agent, &format!("{agent}@agents.invalid"), at_ms / 1000);
    store.put_commit(&Commit {
        tree,
        parents: parent.into_iter().collect(),
        author: sig.clone(),
        committer: sig,
        extra: vec![],
        message: format!("{}\n", subject_of(path)),
    })
}

/// "context: a01c1 rev 2" for changes/a01c1/rev-2.md.
fn subject_of(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    match parts.as_slice() {
        ["changes", cid, file] => format!("context: {cid} {}", file.trim_end_matches(".md").replace('-', " ")),
        _ => format!("context: {path}"),
    }
}

/// The context commit, built with git in the agent's checkout without touching its index or
/// working tree: a temporary index, a blob, a tree and a commit on top of the fork's
/// context branch. Returns the commit; push it to BRANCH with the change.
pub fn commit_with_git(repo: &str, remote: &str, auth: &str, agent: &str, path: &str, doc: &str, at_ms: i64) -> Result<String, String> {
    let mut tip = git(repo, &["ls-remote", remote, BRANCH], Some(auth), &[], None)?;
    if tip.trim().is_empty() {
        tip = git(repo, &["ls-remote", remote, "refs/heads/landseq/context"], Some(auth), &[], None)?;
    }
    let parent = tip.split_whitespace().next().filter(|o| o.len() == 40).map(str::to_string);
    if let Some(p) = &parent {
        git(repo, &["fetch", "--quiet", "--no-tags", remote, p], Some(auth), &[], None)?;
    }
    let blob = git(repo, &["hash-object", "-w", "--stdin"], None, &[], Some(doc))?;
    // In the repo's own git directory (not a shared /tmp): nobody else can plant or swap it.
    let git_dir = git(repo, &["rev-parse", "--absolute-git-dir"], None, &[], None)?;
    let index = std::path::PathBuf::from(git_dir.trim()).join(format!("nerudo-context-{}-{at_ms}.index", std::process::id()));
    let index_s = index.to_string_lossy().to_string();
    let env = [("GIT_INDEX_FILE", index_s.as_str())];
    let made = (|| {
        match &parent {
            Some(p) => git(repo, &["read-tree", p], None, &env, None)?,
            None => git(repo, &["read-tree", "--empty"], None, &env, None)?,
        };
        git(repo, &["update-index", "--add", "--cacheinfo", &format!("100644,{blob},{path}")], None, &env, None)?;
        let tree = git(repo, &["write-tree"], None, &env, None)?;
        let when = format!("{} +0000", at_ms / 1000);
        let email = format!("{agent}@agents.invalid");
        let who = [
            ("GIT_AUTHOR_NAME", agent),
            ("GIT_AUTHOR_EMAIL", email.as_str()),
            ("GIT_AUTHOR_DATE", when.as_str()),
            ("GIT_COMMITTER_NAME", agent),
            ("GIT_COMMITTER_EMAIL", email.as_str()),
            ("GIT_COMMITTER_DATE", when.as_str()),
        ];
        let subject = subject_of(path);
        let mut args = vec!["commit-tree", tree.as_str(), "-m", subject.as_str()];
        if let Some(p) = &parent {
            args.extend(["-p", p.as_str()]);
        }
        git(repo, &args, None, &who, None)
    })();
    let _ = std::fs::remove_file(&index);
    made
}

/// Run git in `repo`. `auth` is an Authorization header for the remote; credential helpers
/// are cleared, and the header goes through GIT_CONFIG_* variables, never the command line.
fn git(repo: &str, args: &[&str], auth: Option<&str>, env: &[(&str, &str)], stdin: Option<&str>) -> Result<String, String> {
    let mut c = Command::new("git");
    c.current_dir(repo).args(args).env("GIT_TERMINAL_PROMPT", "0");
    let mut config = vec![("credential.helper".to_string(), String::new())];
    if let Some(a) = auth.filter(|a| !a.is_empty()) {
        config.push(("http.extraHeader".into(), format!("Authorization: {a}")));
    }
    c.env("GIT_CONFIG_COUNT", config.len().to_string());
    for (i, (k, v)) in config.iter().enumerate() {
        c.env(format!("GIT_CONFIG_KEY_{i}"), k).env(format!("GIT_CONFIG_VALUE_{i}"), v);
    }
    for (k, v) in env {
        c.env(k, v);
    }
    c.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("cannot run git: {e}"))?;
    if let Some(text) = stdin {
        use std::io::Write;
        child.stdin.take().ok_or("git: no stdin")?.write_all(text.as_bytes()).map_err(|e| format!("git: {e}"))?;
    }
    let out = child.wait_with_output().map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!("git {}: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

// ---------------------------------------------------------------------- reading

/// GET context/{change} in words an agent can use.
pub fn describe(c: &Value) -> String {
    let cid = c["change_id"].as_str().unwrap_or("?");
    let rev = &c["rev"];
    let agent = c["agent"].as_str().unwrap_or("?");
    let others: Vec<String> = c["revisions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["rev"] != *rev)
        .map(|r| format!("rev {} ({})", r["rev"], r["state"].as_str().unwrap_or("?")))
        .collect();
    let more = if others.is_empty() { String::new() } else { format!("\nOther revisions with context: {}.", others.join(", ")) };
    let versions = c["versions"].as_array().map(Vec::len).unwrap_or(0);
    let pinned = c["versions"].as_array().into_iter().flatten().find(|v| v["pinned"] == true).map(|v| format!("v{} is pinned to the merged commit", v["v"]));
    let copy = match (c["immutable"].as_bool(), c["v"].as_u64()) {
        (Some(true), Some(v)) => format!(
            "version v{v} of {versions}, the sequencer's immutable copy in {}{}",
            c["repo"].as_str().unwrap_or("the context repo"),
            pinned.map(|p| format!("; {p}")).unwrap_or_default()
        ),
        _ => format!("from fork {}, branch nerudo/context", c["fork"].as_str().unwrap_or("?")),
    };
    match c["text"].as_str() {
        Some(text) => format!("Context of {cid} rev {rev}, written by {agent} ({copy}):\n\n{text}{more}"),
        None => match c["error"].as_str() {
            Some(e) => format!("{cid} rev {rev} by {agent} has context, but it cannot be read: {e}.{more}"),
            None => format!("{cid} rev {rev} by {agent} has no context.{more}"),
        },
    }
}

/// The first lines of a context file's plan, for a receipt or a list: the lines under a
/// "Plan" heading, then under "Decisions", then other text. Front matter, headings and the
/// rejected alternatives are left out. At most `n` lines, each cut at 200 characters.
pub fn plan_lines(text: &str, n: usize) -> Vec<String> {
    let body = text.strip_prefix("---\n").and_then(|r| r.find("\n---\n").map(|i| &r[i + 5..])).unwrap_or(text);
    let (mut plan, mut decisions, mut other) = (vec![], vec![], vec![]);
    let mut section = String::new();
    for l in body.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if l.starts_with('#') {
            section = l.trim_start_matches('#').trim().to_lowercase();
            continue;
        }
        let line: String = l.chars().take(200).collect();
        match section.as_str() {
            s if s.starts_with("plan") => plan.push(line),
            s if s.starts_with("decision") => decisions.push(line),
            s if s.starts_with("rejected") || s.starts_with("alternative") => {}
            _ => other.push(line),
        }
    }
    plan.into_iter().chain(decisions).chain(other).take(n).collect()
}

// ---------------------------------------------------------------------- verify

/// Check every `Context:` trailer on trunk from the outside. Each must name the context
/// repo (`GET context-repo`, read with a read-only token); its commit must be on that
/// repo's main, so nothing rewrote it; the commit must add exactly one file, the change's
/// `changes/<change>/rev-<n>/v<k>.md`, and change nothing else; and that file's `source:`
/// must be the fork@commit in the change's receipt. Returns how many trailers check out,
/// and what is wrong with the others.
pub fn verify_trailers<T: Transport>(t: &T, api: &crate::Api, store: &Store, chain: &[Oid]) -> (usize, Vec<String>) {
    let mut pins = vec![];
    for c in chain {
        let Ok(commit) = store.commit(c) else { continue };
        if let Some(link) = commit.trailer("Context") {
            let cid = commit.trailer("Change-Id").unwrap_or_default();
            let rev = commit.trailer("Revision").unwrap_or_default();
            pins.push((c.to_hex()[..7].to_string(), cid, rev, link));
        }
    }
    if pins.is_empty() {
        return (0, vec![]);
    }
    let info = match api.try_call("GET", "context-repo", None) {
        Ok(v) => v,
        Err(e) => return (0, vec![format!("the context repo: {e}")]),
    };
    let name = info["name"].as_str().unwrap_or_default().to_string();
    let remote = Remote::new(info["remote"].as_str().unwrap_or_default(), info["authorization"].as_str().map(str::to_string));
    let mut repo = Store::new();
    let main = match block_on(ls_refs(t, &remote, "refs/heads/main")) {
        Ok(refs) => refs.into_iter().find(|(n, _)| n == "refs/heads/main").map(|(_, o)| o),
        Err(e) => return (0, vec![format!("{name}: {e}")]),
    };
    let Some(main) = main else {
        return (0, vec![format!("{name} has no main")]);
    };
    if let Err(e) = block_on(fetch(t, &remote, &[main], &[], &mut repo)) {
        return (0, vec![format!("{name}: {e}")]);
    }
    let mut on_main = BTreeSet::new();
    let mut cur = Some(main);
    while let Some(o) = cur {
        on_main.insert(o);
        cur = repo.commit(&o).ok().and_then(|c| c.parents.first().copied());
    }
    let (mut good, mut bad) = (0, vec![]);
    for (at, cid, rev, link) in pins {
        match check_pin(api, &repo, &on_main, &name, &cid, &rev, &link) {
            Ok(()) => good += 1,
            Err(e) => bad.push(format!("{at} ({cid} rev {rev}): {e}")),
        }
    }
    (good, bad)
}

fn check_pin(api: &crate::Api, repo: &Store, on_main: &BTreeSet<Oid>, name: &str, cid: &str, rev: &str, link: &str) -> Result<(), String> {
    let (repo_name, hex) = link.rsplit_once('@').ok_or("the trailer is not repo@commit")?;
    if repo_name != name {
        return Err(format!("it points at {repo_name}, not the context repo {name}"));
    }
    let oid = Oid::from_hex(hex).map_err(|e| e.to_string())?;
    if !on_main.contains(&oid) {
        return Err(format!("{} is not on {name}'s main", &hex[..8.min(hex.len())]));
    }
    let c = repo.commit(&oid).map_err(|e| e.to_string())?;
    let now = repo.flatten(&c.tree).map_err(|e| e.to_string())?;
    let before = match c.parents.first() {
        Some(p) => repo.flatten(&repo.commit(p).map_err(|e| e.to_string())?.tree).map_err(|e| e.to_string())?,
        None => BTreeMap::new(),
    };
    let changed: Vec<&String> = now.keys().chain(before.keys()).collect::<BTreeSet<_>>().into_iter().filter(|p| now.get(*p) != before.get(*p)).collect();
    let prefix = format!("changes/{cid}/rev-{rev}/v");
    let [path] = changed.as_slice() else {
        return Err(format!("the context commit changes {} files, not one", changed.len()));
    };
    if !path.starts_with(&prefix) || before.contains_key(*path) {
        return Err(format!("the context commit touches {path}, not a new {prefix}<k>.md"));
    }
    let text = String::from_utf8_lossy(repo.blob(&now[*path].oid).map_err(|e| e.to_string())?).into_owned();
    let source = front_matter(&text, "source").ok_or("the copy has no source")?;
    let receipt = api.try_call("GET", &format!("changes/{cid}?rev={rev}"), None)?;
    let want = format!("{}@{}", receipt["req"]["fork"].as_str().unwrap_or("?"), receipt["req"]["context_commit"].as_str().unwrap_or("?"));
    if source != want {
        return Err(format!("the copy's source {source} is not the receipt's {want}"));
    }
    Ok(())
}

/// A key's value in a file's front matter (the last one, if it repeats).
fn front_matter(text: &str, key: &str) -> Option<String> {
    let body = text.strip_prefix("---\n")?;
    let end = body.find("\n---\n")?;
    body[..end].lines().filter_map(|l| l.strip_prefix(&format!("{key}: ")).map(str::to_string)).last()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_tokens_and_keeps_commit_ids() {
        let secrets = vec!["s3cr3t-nerudo-token-value".to_string()];
        let text = "Authorization: Bearer abc.def\n\
                    curl -H 'x' https://u:hunter2pass@example.com/x\n\
                    use ghp_0123456789abcdefABCDEF and sk-live1234567890abcdef\n\
                    API_KEY=zzz123 password: hunter2 token=s3cr3t-nerudo-token-value\n\
                    built on 4158bd90b24b1c8f5969c16a331375418e2d0978\n\
                    -----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----\nafter\n";
        let r = redact(text, &secrets);
        for leaked in ["abc.def", "hunter2pass", "ghp_0123", "sk-live", "zzz123", "s3cr3t", "AAAA"] {
            assert!(!r.contains(leaked), "{leaked} leaked in:\n{r}");
        }
        assert!(r.contains("4158bd90b24b1c8f5969c16a331375418e2d0978"), "{r}");
        assert!(r.contains("Authorization: [redacted]"), "{r}");
        assert!(r.contains("after"), "{r}");
        assert!(r.contains("password: [redacted]"), "{r}");
        assert!(r.contains("API_KEY=[redacted]"), "{r}");
    }

    #[test]
    fn bearer_value_is_redacted() {
        let r = redact("send Bearer abcdefghijklmnop to it", &[]);
        assert_eq!(r, "send Bearer [redacted] to it");
    }

    #[test]
    fn document_has_front_matter_and_a_limit() {
        let m = Meta { change_id: "a01c1", rev: 2, agent: "a01", task: Some("T-1"), commit: "c", base: "b", at_ms: 1_791_734_418_000 };
        let d = document(&m, &"x".repeat(MAX_BYTES * 2), &[]);
        assert!(d.starts_with("---\nchange: a01c1\nrevision: 2\nagent: a01\ntask: T-1\n"));
        assert!(d.len() <= MAX_BYTES);
        assert!(d.contains("[cut:"));
        assert_eq!(iso(1_791_734_418_000), "2026-10-11T16:00:18Z");
        assert_eq!(path("a01c1", 2), "changes/a01c1/rev-2.md");
    }

    #[test]
    fn plan_lines_skip_front_matter_headings_and_rejected() {
        let t = "---\nchange: a01ts\nsource: f@1\n---\n\n# a01ts: price() takes a currency\n\n## Plan\nprice() takes the currency.\n\n\
                 ## Decisions\n- Callers pass 'EUR'.\n\n## Rejected\n- A default currency.\n\n## Revision 2\nBuilt again.\n";
        assert_eq!(plan_lines(t, 2), vec!["price() takes the currency.", "- Callers pass 'EUR'."]);
        assert_eq!(plan_lines(t, 9), vec!["price() takes the currency.", "- Callers pass 'EUR'.", "Built again."]);
        assert_eq!(plan_lines("just one line", 3), vec!["just one line"]);
    }

    #[test]
    fn front_matter_reads_the_last_value() {
        let t = "---\nchange: x1\nsource: f@1\nsource: f@2\n---\n\nbody\nsource: no\n";
        assert_eq!(front_matter(t, "source").as_deref(), Some("f@2"));
        assert_eq!(front_matter("no front matter", "source"), None);
    }

    #[test]
    fn native_commit_keeps_earlier_files() {
        let mut s = Store::new();
        let a = commit_native(&mut s, None, &path("a", 1), "one", "a01", 0);
        let b = commit_native(&mut s, Some(a), &path("b", 1), "two", "a01", 0);
        let files = s.flatten(&s.commit(&b).unwrap().tree).unwrap();
        assert!(files.contains_key("changes/a/rev-1.md") && files.contains_key("changes/b/rev-1.md"));
        assert_eq!(s.commit(&b).unwrap().message, "context: b rev 1\n");
    }
}
