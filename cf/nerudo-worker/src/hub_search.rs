//! Pure global-search planning and aggregation. Only a caller's authorized repository
//! set enters these functions; continuation never contains an inaccessible name/count.

use crate::hub::{Org, Repo, User};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const REPOS_PER_PAGE: usize = 8;
pub(crate) const CONCURRENCY: usize = 4;
pub(crate) const REPO_RESULTS: usize = 12;
pub(crate) const TIMEOUT_MS: u64 = 4000;
const MAX_RESULTS: usize = 100;

pub(crate) struct Options {
    pub query: String,
    pub params: BTreeMap<String, String>,
    pub kind: Option<String>,
    owners: Vec<String>,
    repos: Vec<String>,
    visibility: String,
    limit: usize,
    fuzzy: bool,
    case: bool,
    /// Supplied by the persistent Hub index; excluded from cursor fingerprints.
    file_candidates: Option<BTreeSet<String>>,
}

impl Options {
    pub(crate) fn parse(params: &BTreeMap<String, String>) -> Result<Self, String> {
        let kind = params.get("kind").filter(|v| !v.is_empty()).cloned();
        if kind.as_ref().is_some_and(|kind| !["user", "org", "repo", "file"].contains(&kind.as_str())) {
            return Err("Search kind must be user, org, repo or file.".into());
        }
        let visibility = params.get("visibility").map(String::as_str).unwrap_or("all");
        if !["all", "public", "private"].contains(&visibility) {
            return Err("Visibility must be all, public or private.".into());
        }
        let limit = match params.get("limit") {
            Some(value) => value.parse::<usize>().ok().filter(|v| (1..=MAX_RESULTS).contains(v)).ok_or_else(|| format!("Result limit must be between 1 and {MAX_RESULTS}."))?,
            None => 60,
        };
        let owners = scopes(params, "owner")?;
        let repos = scopes(params, "repo")?;
        let mut forwarded: BTreeMap<_, _> = params
            .iter()
            .filter(|(key, _)| ["q", "mode", "ext", "language", "type", "include", "exclude", "ignored", "case", "word", "regex", "fuzzy"].contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        forwarded.entry("mode".into()).or_insert_with(|| "all".into());
        forwarded.insert("limit".into(), REPO_RESULTS.to_string());
        crate::trunk::validate_search_query(&forwarded)?;
        let fuzzy = !matches!(forwarded.get("fuzzy").map(String::as_str), Some("0" | "false"));
        let case = matches!(forwarded.get("case").map(String::as_str), Some("1" | "true"));
        Ok(Self { query: params.get("q").cloned().unwrap_or_default(), params: forwarded, kind, owners, repos, visibility: visibility.into(), limit, fuzzy, case, file_candidates: None })
    }

    pub(crate) fn accepts_repo(&self, repo: &Repo) -> bool {
        (self.visibility == "all" || (self.visibility == "public") == repo.public)
            && (self.owners.is_empty() || self.owners.iter().any(|owner| owner.eq_ignore_ascii_case(&repo.org)))
            && (self.repos.is_empty() || self.repos.iter().any(|name| name.eq_ignore_ascii_case(&repo.repo) || name.eq_ignore_ascii_case(&repo_name(repo))))
    }

    pub(crate) fn searches_files(&self) -> bool {
        self.kind.as_deref().is_none_or(|kind| kind == "file")
    }

    pub(crate) fn mode(&self) -> &str { self.params.get("mode").map(String::as_str).unwrap_or("all") }
    pub(crate) fn regex(&self) -> bool { matches!(self.params.get("regex").map(String::as_str), Some("1" | "true")) }
    pub(crate) fn matches_path_name(&self, path: &str) -> bool {
        crate::trunk::rank_search_name(path, &self.query, self.fuzzy, self.case).is_some()
    }
    pub(crate) fn set_file_candidates(&mut self, candidates: BTreeSet<String>) { self.file_candidates = Some(candidates); }
    pub(crate) fn file_candidate(&self, repo: &Repo) -> bool {
        self.file_candidates.as_ref().is_none_or(|candidates| candidates.contains(&repo.trunk))
    }

    /// Count owned repositories only for owners whose metadata can appear. A
    /// private repository unrelated to the query needs no membership RPC.
    pub(crate) fn metadata_candidates(&self, orgs: &[Org], users: &[User], repos: &[Repo]) -> BTreeSet<String> {
        let mut owners = BTreeSet::new();
        if self.kind.as_deref().is_none_or(|kind| kind == "org") {
            for org in orgs {
                if metadata_match(&org.org, &org.title, &org.description, self).is_some() { owners.insert(org.org.clone()); }
            }
        }
        if self.kind.as_deref().is_none_or(|kind| kind == "user") {
            for user in users {
                if (self.owners.is_empty() || self.owners.iter().any(|owner| owner == &user.user))
                    && metadata_match(&user.user, &user.title, "", self).is_some() { owners.insert(user.user.clone()); }
            }
        }
        repos.iter().filter(|repo| self.accepts_repo(repo) && (owners.contains(&repo.org)
            || (self.kind.as_deref().is_none_or(|kind| kind == "repo") && metadata_match(&repo_name(repo), "", &repo.description, self).is_some())))
            .map(|repo| repo.trunk.clone()).collect()
    }

    fn fingerprint(&self, viewer: &str) -> String {
        let input =
            json!({ "params": self.params, "kind": self.kind, "owner": self.owners, "repo": self.repos, "visibility": self.visibility, "limit": self.limit, "viewer": viewer });
        hex(&Sha256::digest(input.to_string().as_bytes()))
    }
}

fn scopes(params: &BTreeMap<String, String>, key: &str) -> Result<Vec<String>, String> {
    let value = params.get(key).map(String::as_str).unwrap_or("");
    let names: Vec<_> = value.split(',').map(str::trim).filter(|v| !v.is_empty()).map(str::to_lowercase).collect();
    if value.len() > 2400 || names.len() > 16 || names.iter().any(|v| v.len() > 140 || v.chars().any(char::is_control)) {
        return Err(format!("Use at most 16 valid {key} scopes."));
    }
    Ok(names)
}

pub(crate) fn repo_name(repo: &Repo) -> String {
    format!("{}/{}", repo.org, repo.repo)
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct Cursor {
    version: u8,
    pub after: String,
    pub offset: usize,
    fingerprint: String,
}

pub(crate) fn cursor(token: Option<&str>, options: &Options, viewer: &str) -> Result<Cursor, String> {
    let Some(token) = token.filter(|t| !t.is_empty()) else {
        return Ok(Cursor::default());
    };
    let fail = || "This search cursor is invalid. Start the search again.".to_string();
    if token.len() > 1200 || token.len() % 2 != 0 {
        return Err(fail());
    }
    let bytes = token
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let digit = |c: u8| match c {
                b'0'..=b'9' => Some(c - b'0'),
                b'a'..=b'f' => Some(c - b'a' + 10),
                _ => None,
            };
            Some(digit(pair[0])? * 16 + digit(pair[1])?)
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(fail)?;
    let cursor: Cursor = serde_json::from_slice(&bytes).map_err(|_| fail())?;
    if cursor.version != 1 || cursor.fingerprint != options.fingerprint(viewer) || cursor.after.len() > 140 || cursor.offset > 1_000_000 {
        return Err(fail());
    }
    if !cursor.after.is_empty() && cursor.after.split_once('/').is_none_or(|(owner, name)| !crate::hub::valid_org(owner) || !crate::hub::valid_repo(name)) {
        return Err(fail());
    }
    Ok(cursor)
}

fn encode_cursor(after: String, offset: usize, options: &Options, viewer: &str) -> String {
    hex(&serde_json::to_vec(&Cursor { version: 1, after, offset, fingerprint: options.fingerprint(viewer) }).expect("cursor is serializable"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// No administrator override here: authorization decisions must be supplied by the
/// existing read gate before any repository is scheduled for content search.
pub(crate) fn authorized_repos(repos: &[Repo], allowed: &BTreeSet<String>, options: &Options) -> Vec<Repo> {
    let mut visible: Vec<_> = repos.iter().filter(|r| allowed.contains(&repo_name(r)) && options.accepts_repo(r)).cloned().collect();
    visible.sort_by_key(repo_name);
    visible
}

pub(crate) fn batch(repos: &[Repo], options: &Options, cursor: &Cursor) -> Vec<Repo> {
    if !options.searches_files() {
        return vec![];
    }
    repos.iter().filter(|r| repo_name(r) > cursor.after && options.file_candidate(r)).take(REPOS_PER_PAGE).cloned().collect()
}

pub(crate) enum Outcome {
    Search(Value),
    /// 403/404 after the initial check: drop all metadata and counts for this repo.
    Denied,
    /// A timed out/unavailable authorized repo, without disclosing its error text.
    Failed,
}

pub(crate) fn aggregate(
    orgs: &[Org],
    users: &[User],
    repos: &[Repo],
    page: &[Repo],
    outcomes: &BTreeMap<String, Outcome>,
    options: &Options,
    cursor: &Cursor,
    viewer: &str,
) -> Value {
    let visible: Vec<_> = repos.iter().filter(|r| !matches!(outcomes.get(&repo_name(r)), Some(Outcome::Denied))).collect();
    let visible_names: BTreeSet<_> = visible.iter().map(|r| repo_name(r)).collect();
    let count = |owner: &str| visible.iter().filter(|repo| repo.org == owner).count();
    let mut results = vec![];
    let mut matched_users = 0;
    let mut matched_orgs = 0;
    let mut matched_repos = 0;
    let mut matched_files = 0;
    let mut searched_repos = 0;
    let mut failed_repos = 0;
    let mut truncated = false;
    let mut scan_limited = false;
    if cursor.after.is_empty() {
        if options.kind.as_deref().is_none_or(|kind| kind == "user") {
            for user in users {
                // User profiles are public, including those owning zero repos.
                if !options.owners.is_empty() && !options.owners.iter().any(|owner| owner == &user.user) {
                    continue;
                }
                if !options.repos.is_empty() && count(&user.user) == 0 {
                    continue;
                }
                if let Some((score, positions)) = metadata_match(&user.user, &user.title, "", options) {
                    matched_users += 1;
                    results.push(json!({ "kind": "user", "user": user.user, "title": user.title, "repos": count(&user.user), "score": score, "positions": positions }));
                }
            }
        }
        if options.kind.as_deref().is_none_or(|kind| kind == "org") {
            for org in orgs {
                let repos = count(&org.org);
                if repos == 0 {
                    continue;
                }
                if let Some((score, positions)) = metadata_match(&org.org, &org.title, &org.description, options) {
                    matched_orgs += 1;
                    results
                        .push(json!({ "kind": "org", "org": org.org, "title": org.title, "description": org.description, "repos": repos, "score": score, "positions": positions }));
                }
            }
        }
        if options.kind.as_deref().is_none_or(|kind| kind == "repo") {
            for repo in &visible {
                if let Some((score, positions)) = metadata_match(&repo_name(repo), "", &repo.description, options) {
                    matched_repos += 1;
                    results.push(json!({ "kind": "repo", "org": repo.org, "repo": repo.repo, "trunk": repo.trunk, "public": repo.public, "description": repo.description, "score": score, "positions": positions }));
                }
            }
        }
    }
    for repo in page {
        let name = repo_name(repo);
        if !visible_names.contains(&name) {
            continue;
        }
        match outcomes.get(&name) {
            Some(Outcome::Search(response)) => {
                searched_repos += 1;
                matched_files += response["stats"]["matched_files"].as_u64().unwrap_or(0) as usize;
                truncated |= response["stats"]["truncated"].as_bool().unwrap_or(false);
                scan_limited |= response["stats"]["scan_limited"].as_bool().unwrap_or(false);
                for file in response["files"].as_array().into_iter().flatten().take(REPO_RESULTS) {
                    if !file["path"].is_string() {
                        continue;
                    }
                    let mut result = file.clone();
                    result["kind"] = json!("file");
                    result["org"] = json!(repo.org);
                    result["repo"] = json!(repo.repo);
                    result["trunk"] = json!(repo.trunk);
                    result["public"] = json!(repo.public);
                    result["commit"] = response["commit"].clone();
                    results.push(result);
                }
            }
            Some(Outcome::Failed) | None => failed_repos += 1,
            Some(Outcome::Denied) => {}
        }
    }
    results.sort_by(|a, b| b["score"].as_i64().unwrap_or(0).cmp(&a["score"].as_i64().unwrap_or(0)).then_with(|| result_key(a).cmp(&result_key(b))));
    let total_results = results.len();
    let offset = cursor.offset.min(total_results);
    let returned: Vec<_> = results.into_iter().skip(offset).take(options.limit).collect();
    let end = offset + returned.len();
    let last = page.last().map(repo_name);
    let remaining_repos = last.as_ref().map(|last| visible.iter().filter(|r| repo_name(r) > *last && options.file_candidate(r)).count()).unwrap_or(0);
    let next_cursor = if end < total_results {
        Some(encode_cursor(cursor.after.clone(), end, options, viewer))
    } else if remaining_repos > 0 {
        last.map(|last| encode_cursor(last, 0, options, viewer))
    } else {
        None
    };
    let partial = next_cursor.is_some() || failed_repos > 0 || truncated || scan_limited;
    json!({ "query": options.query, "results": returned, "next_cursor": next_cursor,
        "stats": { "accessible_repos": visible.len(), "searched_repos": searched_repos, "failed_repos": failed_repos, "remaining_repos": remaining_repos,
            "matched_users": matched_users, "matched_orgs": matched_orgs, "matched_repos": matched_repos, "matched_files": matched_files, "returned": returned.len(),
            "partial": partial, "truncated": truncated, "scan_limited": scan_limited, "directory_incomplete": false },
        "limits": { "repos_per_page": REPOS_PER_PAGE, "concurrency": CONCURRENCY, "repo_results": REPO_RESULTS, "timeout_ms": TIMEOUT_MS } })
}

/// Report incomplete authorization coverage without disclosing whether a failed
/// check belonged to an inaccessible repository, its name, count or error.
pub(crate) fn mark_directory_incomplete(result: &mut Value, incomplete: bool) {
    result["stats"]["directory_incomplete"] = json!(incomplete);
    if incomplete {
        result["stats"]["partial"] = json!(true);
    }
}

fn metadata_match(name: &str, title: &str, description: &str, options: &Options) -> Option<(i64, Vec<usize>)> {
    let query = &options.query;
    if let Some((score, positions)) = crate::trunk::rank_search_name(name, query, options.fuzzy, options.case) {
        return Some((20_000 + score.max(1), positions));
    }
    if let Some((score, _)) = crate::trunk::rank_search_name(title, query, options.fuzzy, options.case) {
        return Some((15_000 + score.max(1) / 4, vec![]));
    }
    let description = if options.case { description.to_string() } else { description.to_lowercase() };
    let terms: Vec<_> = query.split_whitespace().map(|term| if options.case { term.to_string() } else { term.to_lowercase() }).collect();
    (!terms.is_empty() && terms.iter().all(|term| description.contains(term))).then_some((10_000, vec![]))
}

fn result_key(value: &Value) -> String {
    match value["kind"].as_str() {
        Some("user") => format!("0:{}", value["user"].as_str().unwrap_or("")),
        Some("org") => format!("1:{}", value["org"].as_str().unwrap_or("")),
        Some("repo") => format!("2:{}/{}", value["org"].as_str().unwrap_or(""), value["repo"].as_str().unwrap_or("")),
        _ => format!("3:{}/{}/{}", value["org"].as_str().unwrap_or(""), value["repo"].as_str().unwrap_or(""), value["path"].as_str().unwrap_or("")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(pairs: &[(&str, &str)]) -> Options {
        Options::parse(&pairs.iter().map(|(key, value)| (key.to_string(), value.to_string())).collect()).unwrap()
    }

    fn repo(owner: &str, name: &str, public: bool) -> Repo {
        Repo { org: owner.into(), repo: name.into(), trunk: format!("{owner}-{name}"), public, ..Default::default() }
    }

    fn response(paths: &[&str]) -> Outcome {
        Outcome::Search(json!({ "commit": "abc123", "files": paths.iter().map(|path| json!({ "path": path, "extension": "ts", "language": "TypeScript",
            "category": "source", "size": 24, "score": 0, "positions": [], "lines": [{ "line": 3, "text": "needle in contents", "ranges": [[0, 6]], "clipped": false }], "matches": 1 })).collect::<Vec<_>>(),
            "stats": { "matched_files": paths.len(), "truncated": false, "scan_limited": false } }))
    }

    fn allowed(repos: &[Repo], options: &Options) -> Vec<Repo> {
        authorized_repos(repos, &repos.iter().map(repo_name).collect(), options)
    }

    #[test]
    fn authorization_precedes_planning_and_private_names_counts_and_keys_stay_hidden() {
        let options = options(&[("q", "")]);
        let candidates = vec![repo("public", "shown", true), repo("secret-org", "hidden-name", false)];
        let visible = authorized_repos(&candidates, &BTreeSet::from(["public/shown".into()]), &options);
        let page = batch(&visible, &options, &Cursor::default());
        assert_eq!(page.len(), 1);
        let orgs = vec![Org { org: "public".into(), ..Default::default() }, Org { org: "secret-org".into(), ..Default::default() }];
        let users = vec![User { user: "person".into(), title: "Public profile".into(), key: Some("PRIVATE_KEY_FIELD".into()), ..Default::default() }];
        let outcomes = BTreeMap::from([("public/shown".into(), response(&[])), ("secret-org/hidden-name".into(), response(&["SECRET_FILE"]))]);
        let result = aggregate(&orgs, &users, &visible, &page, &outcomes, &options, &Cursor::default(), "anon");
        assert_eq!(result["stats"]["accessible_repos"], 1);
        assert_eq!(result["stats"]["failed_repos"], 0);
        assert!(result["results"].as_array().unwrap().iter().any(|v| v["kind"] == "user" && v["user"] == "person" && v["repos"] == 0));
        for secret in ["secret-org", "hidden-name", "SECRET_FILE", "PRIVATE_KEY_FIELD"] {
            assert!(!result.to_string().contains(secret));
        }
    }

    #[test]
    fn cursor_is_bound_to_viewer_query_and_all_filters_with_generic_errors() {
        let params = options(&[("q", "needle"), ("owner", "acme"), ("visibility", "private")]);
        let token = encode_cursor("acme/repo".into(), 5, &params, "person-a");
        let next = cursor(Some(&token), &params, "person-a").unwrap();
        assert_eq!(next.after, "acme/repo");
        assert_eq!(next.offset, 5);
        let wrong_query = options(&[("q", "changed"), ("owner", "acme"), ("visibility", "private")]);
        let wrong_filter = options(&[("q", "needle"), ("owner", "acme"), ("visibility", "public")]);
        let errors = [
            cursor(Some(&token), &params, "person-b").err().unwrap(),
            cursor(Some(&token), &wrong_query, "person-a").err().unwrap(),
            cursor(Some(&token), &wrong_filter, "person-a").err().unwrap(),
            cursor(Some("bad-token"), &params, "person-a").err().unwrap(),
        ];
        assert!(errors.iter().all(|error| error == &errors[0]));
        assert!(!errors[0].contains("acme") && !errors[0].contains("person"));
    }

    #[test]
    fn continuation_reaches_a_late_repo_after_a_full_batch_without_hits() {
        let options = options(&[("q", "needle"), ("kind", "file")]);
        let repos: Vec<_> = (0..REPOS_PER_PAGE + 1).map(|i| repo("acme", &format!("repo-{i:02}"), true)).collect();
        let repos = allowed(&repos, &options);
        let first = batch(&repos, &options, &Cursor::default());
        let outcomes = first.iter().map(|repo| (repo_name(repo), response(&[]))).collect();
        let result = aggregate(&[], &[], &repos, &first, &outcomes, &options, &Cursor::default(), "anon");
        assert!(result["results"].as_array().unwrap().is_empty());
        assert_eq!(result["stats"]["remaining_repos"], 1);
        let next = cursor(result["next_cursor"].as_str(), &options, "anon").unwrap();
        let last = batch(&repos, &options, &next);
        assert_eq!(last.len(), 1);
        let outcomes = BTreeMap::from([(repo_name(&last[0]), response(&["src/needle.ts"]))]);
        let result = aggregate(&[], &[], &repos, &last, &outcomes, &options, &next, "anon");
        assert_eq!(result["results"][0]["path"], "src/needle.ts");
        assert!(result["next_cursor"].is_null());
        assert_eq!(result["stats"]["accessible_repos"], REPOS_PER_PAGE + 1);
    }

    #[test]
    fn read_time_denial_drops_repo_metadata_and_counts_but_failures_are_generic() {
        let options = options(&[("q", "")]);
        let repos = allowed(&[repo("revoked-org", "gone", false), repo("acme", "unavailable", false), repo("acme", "ok", true)], &options);
        let page = batch(&repos, &options, &Cursor::default());
        let outcomes = BTreeMap::from([("revoked-org/gone".into(), Outcome::Denied), ("acme/unavailable".into(), Outcome::Failed), ("acme/ok".into(), response(&["src/ok.ts"]))]);
        let orgs = vec![Org { org: "revoked-org".into(), ..Default::default() }, Org { org: "acme".into(), ..Default::default() }];
        let result = aggregate(&orgs, &[], &repos, &page, &outcomes, &options, &Cursor::default(), "person");
        assert_eq!(result["stats"]["accessible_repos"], 2);
        assert_eq!(result["stats"]["failed_repos"], 1);
        assert_eq!(result["stats"]["searched_repos"], 1);
        assert_eq!(result["stats"]["partial"], true);
        assert!(!result.to_string().contains("revoked-org"));
        assert!(result.get("errors").is_none());
    }

    #[test]
    fn relevance_orders_entity_names_before_code_and_pages_batch_overflow() {
        let options = options(&[("q", "needle"), ("limit", "1")]);
        let repos = allowed(&[repo("acme", "project", true)], &options);
        let page = batch(&repos, &options, &Cursor::default());
        let users = vec![User { user: "needle".into(), ..Default::default() }];
        let outcomes = BTreeMap::from([("acme/project".into(), response(&["src/code.ts"]))]);
        let first = aggregate(&[], &users, &repos, &page, &outcomes, &options, &Cursor::default(), "anon");
        assert_eq!(first["results"][0]["kind"], "user");
        let next = cursor(first["next_cursor"].as_str(), &options, "anon").unwrap();
        assert_eq!(next.after, "");
        assert_eq!(next.offset, 1);
        let second = aggregate(&[], &users, &repos, &page, &outcomes, &options, &next, "anon");
        assert_eq!(second["results"][0]["kind"], "file");
        assert!(second["next_cursor"].is_null());
    }

    #[test]
    fn visibility_owner_repo_and_entity_only_search_bound_the_scope() {
        let candidates = [repo("acme", "main", true), repo("acme", "private", false), repo("other", "private", false)];
        let options = options(&[("visibility", "private"), ("owner", "acme"), ("repo", "acme/private"), ("kind", "repo")]);
        let visible = allowed(&candidates, &options);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].repo, "private");
        assert!(batch(&visible, &options, &Cursor::default()).is_empty());
        assert!(Options::parse(&BTreeMap::from([("q".into(), "(".into()), ("regex".into(), "1".into())])).is_err());
    }

    #[test]
    fn exact_and_case_options_apply_to_metadata_as_well_as_files() {
        let exact = options(&[("q", "needle"), ("fuzzy", "0")]);
        assert!(metadata_match("n-e-e-d-l-e", "", "", &exact).is_none());
        assert!(metadata_match("needle", "", "", &exact).is_some());
        let case = options(&[("q", "NEEDLE"), ("case", "1"), ("fuzzy", "0")]);
        assert!(metadata_match("needle", "", "", &case).is_none());
        assert!(metadata_match("other", "", "NEEDLE in description", &case).is_some());
    }

    #[test]
    fn repeated_overflow_and_batch_cursors_reach_every_accessible_result_once() {
        let options = options(&[("q", "needle"), ("kind", "file"), ("limit", "3")]);
        let repos: Vec<_> = (0..19).map(|i| repo("acme", &format!("repo-{i:02}"), true)).collect();
        let repos = allowed(&repos, &options);
        let mut next = Cursor::default();
        let mut found = BTreeSet::new();
        let mut previous_after = String::new();
        for _ in 0..30 {
            let page = batch(&repos, &options, &next);
            let outcomes = page.iter().map(|repo| (repo_name(repo), response(&["src/needle.ts"]))).collect();
            let result = aggregate(&[], &[], &repos, &page, &outcomes, &options, &next, "anon");
            for hit in result["results"].as_array().unwrap() {
                assert!(found.insert(result_key(hit)));
            }
            let Some(token) = result["next_cursor"].as_str() else {
                break;
            };
            next = cursor(Some(token), &options, "anon").unwrap();
            assert!(next.after >= previous_after);
            if next.after != previous_after {
                assert_eq!(next.offset, 0);
            }
            previous_after = next.after.clone();
        }
        assert_eq!(found.len(), repos.len());
    }

    #[test]
    fn unavailable_permission_checks_report_partial_without_hidden_metadata() {
        let options = options(&[("q", "needle")]);
        let mut result = aggregate(&[], &[], &[], &[], &BTreeMap::new(), &options, &Cursor::default(), "person");
        mark_directory_incomplete(&mut result, true);
        assert!(result["results"].as_array().unwrap().is_empty());
        assert_eq!(result["stats"]["directory_incomplete"], true);
        assert_eq!(result["stats"]["partial"], true);
        assert_eq!(result["stats"]["accessible_repos"], 0);
        assert_eq!(result["stats"]["failed_repos"], 0);
        assert!(result.get("errors").is_none());
    }

    #[test]
    fn persistent_shortlist_skips_unrelated_repos_without_losing_continuation() {
        let mut options = options(&[("q","needle"),("kind","file")]);
        let repos: Vec<_> = (0..20).map(|i| repo("acme",&format!("repo-{i:02}"),true)).collect();
        options.set_file_candidates(BTreeSet::from([repos[18].trunk.clone(),repos[19].trunk.clone()]));
        let visible=allowed(&repos,&options);
        let page=batch(&visible,&options,&Cursor::default());
        assert_eq!(page.iter().map(repo_name).collect::<Vec<_>>(),vec!["acme/repo-18","acme/repo-19"]);
        let outcomes=page.iter().map(|repo| (repo_name(repo),response(&["src/needle.ts"]))).collect();
        let result=aggregate(&[],&[],&visible,&page,&outcomes,&options,&Cursor::default(),"anon");
        assert_eq!(result["results"].as_array().unwrap().len(),2);
        assert!(result["next_cursor"].is_null());
        assert_eq!(result["stats"]["remaining_repos"],0);
    }

    #[test]
    fn metadata_shortlist_checks_only_matching_repos_and_matching_owner_counts() {
        let options=options(&[("q","needle")]);
        let repos=[repo("matched-org","one",false),repo("matched-org","two",true),repo("unrelated","secret",false),repo("elsewhere","needle",false)];
        let orgs=[Org {org:"matched-org".into(),title:"Needle organization".into(),..Default::default()}];
        let shortlist=options.metadata_candidates(&orgs,&[],&repos);
        assert_eq!(shortlist,BTreeSet::from([repos[0].trunk.clone(),repos[1].trunk.clone(),repos[3].trunk.clone()]));
        let files=super::tests::options(&[("q","needle"),("kind","file")]);
        assert!(files.metadata_candidates(&orgs,&[],&repos).is_empty());
        assert!(files.matches_path_name("src/n-e-e-d-l-e.ts"));
    }
}
