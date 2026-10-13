//! Persistent repository-level search postings. This is a shortlist, never a read
//! grant: the front Worker and each selected trunk still enforce current access.
//! Only direct Durable Object requests can publish or inspect these tables.

use crate::hub_search::Options;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use worker::{Result, SqlStorage, SqlStorageValue};

const VERSION: u32 = 1;
const INSERT_CHUNK: usize = 30;
const QUERY_CHUNK: usize = 64;
const MAX_PATHS: usize = 20_000;
const MAX_GRAMS: usize = 100_000;

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct IndexedPath {
    pub path: String,
    pub extension: String,
    pub language: Option<String>,
    pub category: String,
    pub ignored: bool,
    pub mode: u32,
    pub size: usize,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct Publication {
    pub version: u32,
    pub trunk: String,
    pub head: String,
    #[serde(default)]
    pub seq: u64,
    pub indexed_at: i64,
    pub complete: bool,
    pub paths: Vec<IndexedPath>,
    pub grams: Vec<String>,
    pub unicode_content: bool,
    /// A bounded hint set cannot exclude a repo if any paths/grams were omitted.
    #[serde(default)]
    pub candidate_partial: bool,
}

impl Publication {
    pub(crate) fn validate(&self, trunk: &str) -> std::result::Result<(), String> {
        if self.version != VERSION
            || self.trunk != trunk
            || !crate::hub::valid_trunk(trunk)
            || !valid_head(&self.head)
            || self.seq > 9_007_199_254_740_991
            || !(0..=9_007_199_254_740_991).contains(&self.indexed_at)
            || !self.complete
            || self.paths.len() > MAX_PATHS
            || self.grams.len() > MAX_GRAMS
        {
            return Err("Invalid search index publication.".into());
        }
        if self.paths.iter().any(|file| {
            file.path.is_empty()
                || file.path.len() > 4096
                || file.path.contains('\0')
                || file.extension.len() > 256
                || file
                    .language
                    .as_ref()
                    .is_some_and(|language| language.len() > 256)
                || !["source", "tests", "docs", "config", "assets", "other"]
                    .contains(&file.category.as_str())
        }) || self.grams.iter().any(|gram| {
            gram.len() != 3
                || !gram
                    .bytes()
                    .all(|byte| byte.is_ascii() && !byte.is_ascii_control())
        }) {
            return Err("Invalid search index publication.".into());
        }
        Ok(())
    }
}

pub(crate) fn valid_head(head: &str) -> bool {
    head.len() == 40
        && head
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn schema(sql: &SqlStorage) -> Result<()> {
    sql.exec("CREATE TABLE IF NOT EXISTS hub_search_repos (trunk TEXT PRIMARY KEY, target_head TEXT NOT NULL, target_seq INTEGER NOT NULL DEFAULT 0, published_head TEXT NOT NULL DEFAULT '', complete INTEGER NOT NULL DEFAULT 0, indexed_at INTEGER NOT NULL DEFAULT 0, unicode_content INTEGER NOT NULL DEFAULT 1, candidate_partial INTEGER NOT NULL DEFAULT 0)", None)?;
    sql.exec("CREATE TABLE IF NOT EXISTS hub_search_paths (trunk TEXT NOT NULL, path TEXT NOT NULL, data TEXT NOT NULL, PRIMARY KEY (trunk,path))", None)?;
    sql.exec("CREATE TABLE IF NOT EXISTS hub_search_grams (gram TEXT NOT NULL, trunk TEXT NOT NULL, PRIMARY KEY (gram,trunk))", None)?;
    sql.exec(
        "CREATE INDEX IF NOT EXISTS hub_search_grams_trunk ON hub_search_grams(trunk)",
        None,
    )?;
    Ok(())
}

pub(crate) fn dirty(sql: &SqlStorage, trunk: &str, head: &str, seq: u64) -> Result<()> {
    schema(sql)?;
    sql.exec("INSERT INTO hub_search_repos(trunk,target_head,target_seq) VALUES (?,?,?) ON CONFLICT(trunk) DO UPDATE SET target_head=excluded.target_head,target_seq=excluded.target_seq,complete=CASE WHEN published_head=excluded.target_head THEN complete ELSE 0 END WHERE excluded.target_seq>=target_seq", vec![trunk.into(), head.into(), (seq as i64).into()])?;
    Ok(())
}

#[derive(Deserialize)]
struct Generation {
    target_head: String,
    target_seq: u64,
    published_head: String,
    complete: i64,
}

fn accepts_generation(generation: &Generation, publication: &Publication) -> bool {
    publication.seq > generation.target_seq
        || (publication.seq == generation.target_seq && publication.head == generation.target_head)
}

/// All generation writes are synchronous, so no other DO request can observe a
/// half-replaced posting set. An older background job cannot replace a newer head.
pub(crate) fn publish(sql: &SqlStorage, publication: &Publication) -> Result<bool> {
    schema(sql)?;
    let generations: Vec<Generation> = sql
        .exec(
            "SELECT target_head,target_seq,published_head,complete FROM hub_search_repos WHERE trunk=?",
            vec![publication.trunk.clone().into()],
        )?
        .to_array()?;
    if generations
        .first()
        .is_some_and(|generation| !accepts_generation(generation, publication))
    {
        return Ok(false);
    }
    if generations.first().is_some_and(|generation| {
        generation.complete == 1 && generation.published_head == publication.head
    }) {
        // A timed-out caller may retry after the first write committed. Updating
        // the sequence is cheap; immutable same-head paths/grams already exist.
        sql.exec(
            "UPDATE hub_search_repos SET target_head=?,target_seq=? WHERE trunk=?",
            vec![
                publication.head.clone().into(),
                (publication.seq as i64).into(),
                publication.trunk.clone().into(),
            ],
        )?;
        return Ok(true);
    }
    sql.exec("INSERT INTO hub_search_repos(trunk,target_head,target_seq) VALUES (?,?,?) ON CONFLICT(trunk) DO UPDATE SET target_head=excluded.target_head,target_seq=excluded.target_seq,complete=0", vec![publication.trunk.clone().into(), publication.head.clone().into(), (publication.seq as i64).into()])?;
    sql.exec(
        "DELETE FROM hub_search_paths WHERE trunk=?",
        vec![publication.trunk.clone().into()],
    )?;
    sql.exec(
        "DELETE FROM hub_search_grams WHERE trunk=?",
        vec![publication.trunk.clone().into()],
    )?;
    for paths in publication.paths.chunks(INSERT_CHUNK) {
        let query = format!(
            "INSERT OR REPLACE INTO hub_search_paths(trunk,path,data) VALUES {}",
            vec!["(?,?,?)"; paths.len()].join(",")
        );
        let mut bindings = vec![];
        for file in paths {
            bindings.extend([
                publication.trunk.clone().into(),
                file.path.clone().into(),
                serde_json::to_string(file)?.into(),
            ]);
        }
        sql.exec(&query, bindings)?;
    }
    for grams in publication.grams.chunks(INSERT_CHUNK) {
        let query = format!(
            "INSERT OR IGNORE INTO hub_search_grams(gram,trunk) VALUES {}",
            vec!["(?,?)"; grams.len()].join(",")
        );
        let bindings: Vec<SqlStorageValue> = grams
            .iter()
            .flat_map(|gram| [gram.clone().into(), publication.trunk.clone().into()])
            .collect();
        sql.exec(&query, bindings)?;
    }
    sql.exec("UPDATE hub_search_repos SET published_head=?,complete=1,indexed_at=?,unicode_content=?,candidate_partial=? WHERE trunk=?", vec![publication.head.clone().into(), publication.indexed_at.into(), (publication.unicode_content as i32).into(), (publication.candidate_partial as i32).into(), publication.trunk.clone().into()])?;
    Ok(true)
}

#[derive(Deserialize)]
struct IndexedRepo {
    trunk: String,
    complete: i64,
    unicode_content: i64,
    candidate_partial: i64,
}
#[derive(Deserialize)]
struct PathRow {
    trunk: String,
    path: String,
}
#[derive(Deserialize)]
struct TrunkRow {
    trunk: String,
}

#[derive(Default, Serialize, Deserialize)]
pub(crate) struct Candidates {
    pub files: BTreeSet<String>,
    pub pending: BTreeSet<String>,
}

/// Literal ASCII grams can rule out a repository. Regexes, short queries and
/// Unicode casefolding conservatively retain repositories rather than lose hits.
pub(crate) fn query_grams(options: &Options) -> Option<BTreeSet<String>> {
    if options.regex() || !options.query.is_ascii() {
        return None;
    }
    let query = options.query.to_ascii_lowercase();
    let grams: BTreeSet<_> = query
        .as_bytes()
        .windows(3)
        .filter(|gram| gram.iter().all(|byte| !byte.is_ascii_control()))
        .map(|gram| String::from_utf8(gram.to_vec()).expect("ASCII query"))
        .collect();
    (!grams.is_empty()).then_some(grams)
}

pub(crate) fn candidates(
    sql: &SqlStorage,
    trunks: &BTreeSet<String>,
    options: &Options,
) -> Result<Candidates> {
    if !options.searches_files() || trunks.is_empty() {
        return Ok(Candidates::default());
    }
    schema(sql)?;
    let mut result = Candidates::default();
    let scoped: Vec<_> = trunks.iter().cloned().collect();
    let mut indexed: Vec<IndexedRepo> = vec![];
    for chunk in scoped.chunks(QUERY_CHUNK) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let query = format!("SELECT trunk,complete,unicode_content,candidate_partial FROM hub_search_repos WHERE trunk IN ({placeholders})");
        let bindings: Vec<SqlStorageValue> =
            chunk.iter().map(|trunk| trunk.clone().into()).collect();
        indexed.extend(sql.exec(&query, bindings)?.to_array::<IndexedRepo>()?);
    }
    let known: BTreeSet<_> = indexed
        .iter()
        .filter(|repo| repo.complete == 1)
        .map(|repo| repo.trunk.clone())
        .collect();
    result.pending = trunks.difference(&known).cloned().collect();
    result.files.extend(result.pending.clone());
    result.files.extend(
        indexed
            .iter()
            .filter(|repo| repo.candidate_partial != 0 && trunks.contains(&repo.trunk))
            .map(|repo| repo.trunk.clone()),
    );
    let mode = options.mode();
    if mode != "content" {
        // Filename subsequences and typo matches cannot be safely pruned by
        // contiguous trigrams. Match the persistent path directory itself.
        let needed: Vec<_> = trunks.difference(&result.files).cloned().collect();
        for chunk in needed.chunks(QUERY_CHUNK) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let query =
                format!("SELECT trunk,path FROM hub_search_paths WHERE trunk IN ({placeholders})");
            let bindings: Vec<SqlStorageValue> =
                chunk.iter().map(|trunk| trunk.clone().into()).collect();
            for row in sql.exec(&query, bindings)?.next::<PathRow>() {
                let row = row?;
                if !result.files.contains(&row.trunk) && options.matches_path_name(&row.path) {
                    result.files.insert(row.trunk);
                }
            }
        }
    }
    if mode != "files" && !options.query.is_empty() {
        if let Some(grams) = query_grams(options) {
            // A handful of grams is a conservative superset and bounds SQL's
            // bind count even for a 512-character query. Exact matching follows.
            let grams: Vec<_> = grams.into_iter().take(16).collect();
            let placeholders = vec!["?"; grams.len()].join(",");
            for chunk in scoped.chunks(QUERY_CHUNK) {
                let scope_placeholders = vec!["?"; chunk.len()].join(",");
                let query = format!("SELECT trunk FROM hub_search_grams WHERE gram IN ({placeholders}) AND trunk IN ({scope_placeholders}) GROUP BY trunk HAVING COUNT(*)=?");
                let mut bindings: Vec<SqlStorageValue> =
                    grams.iter().map(|gram| gram.clone().into()).collect();
                bindings.extend(chunk.iter().map(|trunk| trunk.clone().into()));
                bindings.push((grams.len() as i64).into());
                let matching: Vec<TrunkRow> = sql.exec(&query, bindings)?.to_array()?;
                result
                    .files
                    .extend(matching.into_iter().map(|row| row.trunk));
            }
            result.files.extend(
                indexed
                    .iter()
                    .filter(|repo| repo.unicode_content != 0 && trunks.contains(&repo.trunk))
                    .map(|repo| repo.trunk.clone()),
            );
        } else {
            result.files.extend(trunks.clone());
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    fn options(pairs: &[(&str, &str)]) -> Options {
        Options::parse(
            &pairs
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect::<BTreeMap<_, _>>(),
        )
        .unwrap()
    }
    #[test]
    fn grams_are_conservative_for_regex_unicode_and_short_queries() {
        assert!(query_grams(&options(&[("q", "ab")])).is_none());
        assert!(query_grams(&options(&[("q", "café")])).is_none());
        assert!(query_grams(&options(&[("q", "needle.*"), ("regex", "1")])).is_none());
        assert_eq!(
            query_grams(&options(&[("q", "NEEDLE")])),
            Some(BTreeSet::from([
                "nee".into(),
                "eed".into(),
                "edl".into(),
                "dle".into()
            ]))
        );
    }
    #[test]
    fn internal_publication_requires_complete_bound_generation() {
        let mut publication = Publication {
            version: 1,
            trunk: "trunk".into(),
            head: "a".repeat(40),
            seq: 1,
            indexed_at: 0,
            complete: true,
            paths: vec![],
            grams: vec!["abc".into()],
            unicode_content: false,
            candidate_partial: false,
        };
        assert!(publication.validate("trunk").is_ok());
        assert!(publication.validate("other").is_err());
        publication.complete = false;
        assert!(publication.validate("trunk").is_err());
        assert!(!valid_head("abc"));
    }

    #[test]
    fn stale_publication_cannot_revert_a_newer_dirty_generation() {
        let generation = Generation {
            target_head: "b".repeat(40),
            target_seq: 2,
            published_head: "a".repeat(40),
            complete: 0,
        };
        let mut publication = Publication {
            version: 1,
            trunk: "trunk".into(),
            head: "a".repeat(40),
            seq: 1,
            indexed_at: 0,
            complete: true,
            paths: vec![],
            grams: vec![],
            unicode_content: false,
            candidate_partial: false,
        };
        assert!(!accepts_generation(&generation, &publication));
        publication.seq = 2;
        assert!(!accepts_generation(&generation, &publication));
        publication.head = generation.target_head.clone();
        assert!(accepts_generation(&generation, &publication));
        publication.seq = 3;
        publication.head = "c".repeat(40);
        assert!(accepts_generation(&generation, &publication));
    }
}
