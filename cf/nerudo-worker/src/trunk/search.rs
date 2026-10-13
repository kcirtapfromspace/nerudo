//! Repository discovery over one immutable Git tree. These functions are independent
//! of Workers so ranking, filters, bounded scans and composition are tested natively.

use nerudo_git::{FileEntry, Store};
use regex::{Regex, RegexBuilder};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub(super) const MAX_FILE_BYTES: usize = 256 * 1024;
const MAX_SCAN_BYTES: usize = 16 * 1024 * 1024;
const MAX_CONTENT_FILES: usize = 20_000;
const MAX_RESULTS: usize = 100;
const MAX_LINES_PER_FILE: usize = 20;
const MAX_SNIPPET_CHARS: usize = 280;
const MAX_QUERY_CHARS: usize = 512;
const MAX_README_BYTES: usize = 32 * 1024;
const CATEGORIES: [&str; 6] = ["source", "tests", "docs", "config", "assets", "other"];

type Flat = BTreeMap<String, FileEntry>;

pub(super) struct SearchOptions {
    query: String,
    mode: String,
    extensions: Vec<String>,
    categories: Vec<String>,
    languages: Vec<String>,
    includes: Vec<Regex>,
    excludes: Vec<Regex>,
    ignored: bool,
    fuzzy: bool,
    case: bool,
    matcher: Option<Regex>,
    regex: bool,
    limit: usize,
}

impl SearchOptions {
    pub(super) fn parse(q: &BTreeMap<String, String>) -> Result<Self, String> {
        let mode = q.get("mode").map(String::as_str).unwrap_or("files");
        if !["all", "files", "content"].contains(&mode) {
            return Err("Search mode must be all, files or content.".into());
        }
        let query = q.get("q").map(String::as_str).unwrap_or("");
        let query = if mode == "files" { query.trim() } else { query }.to_string();
        if query.chars().count() > MAX_QUERY_CHARS {
            return Err(format!("Search queries are limited to {MAX_QUERY_CHARS} characters."));
        }
        let extensions = values(q, "ext")?.into_iter().map(|e| if e.eq_ignore_ascii_case("none") { String::new() } else { e.trim_start_matches('.').to_lowercase() }).collect();
        let categories = values(q, "type")?;
        if categories.iter().any(|t| !CATEGORIES.contains(&t.as_str())) {
            return Err("File type must be source, tests, docs, config, assets or other.".into());
        }
        let languages = values(q, "language")?;
        let includes = values(q, "include")?.iter().map(|p| glob(p)).collect::<Result<Vec<_>, _>>()?;
        let excludes = values(q, "exclude")?.iter().map(|p| glob(p)).collect::<Result<Vec<_>, _>>()?;
        let case = flag(q, "case", false)?;
        let word = flag(q, "word", false)?;
        let is_regex = flag(q, "regex", false)?;
        let matcher = if mode != "files" && !query.is_empty() {
            let pattern = if is_regex { query.clone() } else { regex::escape(&query) };
            let pattern = if word { format!(r"\b(?:{pattern})\b") } else { pattern };
            Some(
                RegexBuilder::new(&pattern)
                    .case_insensitive(!case)
                    .size_limit(256 * 1024)
                    .dfa_size_limit(512 * 1024)
                    .build()
                    .map_err(|e| format!("Invalid search pattern: {e}"))?,
            )
        } else {
            None
        };
        let limit = match q.get("limit") {
            Some(n) => n.parse::<usize>().ok().filter(|n| (1..=MAX_RESULTS).contains(n)).ok_or_else(|| format!("Result limit must be between 1 and {MAX_RESULTS}."))?,
            None => MAX_RESULTS,
        };
        Ok(Self {
            query,
            mode: mode.into(),
            extensions,
            categories,
            languages,
            includes,
            excludes,
            ignored: flag(q, "ignored", false)?,
            fuzzy: flag(q, "fuzzy", true)?,
            case,
            matcher,
            regex: is_regex,
            limit,
        })
    }

    fn accepts(&self, path: &str, info: &FileInfo) -> bool {
        (self.ignored || !is_ignored(path))
            && (self.extensions.is_empty() || self.extensions.iter().any(|e| e.eq_ignore_ascii_case(&info.extension)))
            && (self.categories.is_empty() || self.categories.iter().any(|c| c == info.category))
            && (self.languages.is_empty() || info.language.is_some_and(|l| self.languages.iter().any(|v| v.eq_ignore_ascii_case(l))))
            && (self.includes.is_empty() || self.includes.iter().any(|p| p.is_match(path)))
            && !self.excludes.iter().any(|p| p.is_match(path))
    }

    pub(super) fn content_phrase(&self) -> Option<String> {
        // FTS narrows candidates; the existing matcher still verifies every hit.
        (!self.regex && self.matcher.is_some() && self.query.len() >= 3 && self.query.bytes().all(|c| (32..=126).contains(&c)))
            .then(|| format!("\"{}\"", self.query.replace('"', "\"\"")))
    }

    pub(super) fn wants_content(&self) -> bool { self.matcher.is_some() }

    pub(super) fn accepts_path(&self, path: &str) -> bool { self.accepts(path, &file_info(path)) }
}

pub(super) fn path_metadata(path: &str) -> Value {
    let info = file_info(path);
    json!({ "path": path, "extension": info.extension, "language": info.language, "category": info.category, "ignored": is_ignored(path) })
}

fn flag(q: &BTreeMap<String, String>, key: &str, default: bool) -> Result<bool, String> {
    match q.get(key).map(String::as_str) {
        None => Ok(default),
        Some("1" | "true") => Ok(true),
        Some("0" | "false") => Ok(false),
        _ => Err(format!("{key} must be 0 or 1.")),
    }
}

fn values(q: &BTreeMap<String, String>, key: &str) -> Result<Vec<String>, String> {
    let Some(value) = q.get(key) else {
        return Ok(vec![]);
    };
    if value.len() > 2048 {
        return Err(format!("The {key} filter is too long."));
    }
    let values: Vec<_> = value.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect();
    if values.len() > 16 || values.iter().any(|v| v.chars().count() > 256) {
        return Err(format!("Use at most 16 {key} filters of 256 characters each."));
    }
    Ok(values)
}

/// Git-style path globs: *, **, ?, character classes, and **/ matching zero or
/// more directories. Patterns are anchored to the repository root.
fn glob(pattern: &str) -> Result<Regex, String> {
    let chars: Vec<char> = pattern.trim_start_matches('/').chars().collect();
    let mut out = String::from("^");
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' if chars.get(i + 1) == Some(&'*') => {
                i += 1;
                if chars.get(i + 1) == Some(&'/') {
                    out.push_str("(?:.*/)?");
                    i += 1;
                } else {
                    out.push_str(".*");
                }
            }
            '*' => out.push_str("[^/]*"),
            '?' => out.push_str("[^/]"),
            '[' => {
                let start = i;
                i += 1;
                out.push('[');
                if chars.get(i) == Some(&'!') {
                    out.push('^');
                    i += 1;
                }
                while i < chars.len() && chars[i] != ']' {
                    if chars[i] == '\\' {
                        out.push_str("\\\\");
                    } else {
                        out.push(chars[i]);
                    }
                    i += 1;
                }
                if i == chars.len() || i == start + 1 {
                    return Err(format!("Invalid path glob: {pattern}"));
                }
                out.push(']');
            }
            '\\' => {
                i += 1;
                let Some(c) = chars.get(i) else {
                    return Err(format!("Invalid path glob: {pattern}"));
                };
                out.push_str(&regex::escape(&c.to_string()));
            }
            c => out.push_str(&regex::escape(&c.to_string())),
        }
        i += 1;
    }
    if pattern.ends_with('/') {
        out.push_str(".*");
    }
    out.push('$');
    RegexBuilder::new(&out).dot_matches_new_line(true).size_limit(64 * 1024).build().map_err(|_| format!("Invalid path glob: {pattern}"))
}

struct FileInfo {
    extension: String,
    language: Option<&'static str>,
    category: &'static str,
}

fn file_info(path: &str) -> FileInfo {
    let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    let extension = name.rsplit_once('.').map(|(_, e)| e.to_string()).unwrap_or_default();
    let language = match extension.as_str() {
        "ts" | "tsx" | "mts" | "cts" => Some("TypeScript"),
        "js" | "jsx" | "mjs" | "cjs" => Some("JavaScript"),
        "rs" => Some("Rust"),
        "py" | "pyi" | "pyw" => Some("Python"),
        "go" => Some("Go"),
        "java" => Some("Java"),
        "kt" | "kts" => Some("Kotlin"),
        "swift" => Some("Swift"),
        "c" | "h" => Some("C"),
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => Some("C++"),
        "cs" => Some("C#"),
        "fs" | "fsx" => Some("F#"),
        "rb" | "rake" => Some("Ruby"),
        "php" => Some("PHP"),
        "vue" => Some("Vue"),
        "svelte" => Some("Svelte"),
        "css" => Some("CSS"),
        "scss" | "sass" => Some("SCSS"),
        "less" => Some("Less"),
        "html" | "htm" => Some("HTML"),
        "sql" => Some("SQL"),
        "sh" | "bash" | "zsh" => Some("Shell"),
        "ps1" | "psm1" => Some("PowerShell"),
        "lua" => Some("Lua"),
        "pl" | "pm" => Some("Perl"),
        "r" => Some("R"),
        "dart" => Some("Dart"),
        "ex" | "exs" => Some("Elixir"),
        "erl" | "hrl" => Some("Erlang"),
        "hs" | "lhs" => Some("Haskell"),
        "clj" | "cljs" | "cljc" => Some("Clojure"),
        "scala" | "sc" => Some("Scala"),
        "jl" => Some("Julia"),
        "zig" => Some("Zig"),
        "nim" => Some("Nim"),
        "elm" => Some("Elm"),
        "ml" | "mli" => Some("OCaml"),
        "sol" => Some("Solidity"),
        "proto" => Some("Protocol Buffers"),
        "graphql" | "gql" => Some("GraphQL"),
        "tf" | "tfvars" => Some("HCL"),
        "nix" => Some("Nix"),
        "wat" => Some("WebAssembly"),
        _ => match name.as_str() {
            "dockerfile" | "containerfile" => Some("Dockerfile"),
            "makefile" | "gnumakefile" => Some("Makefile"),
            _ => None,
        },
    };
    let test = path.split('/').any(|p| matches!(p.to_ascii_lowercase().as_str(), "test" | "tests" | "__tests__" | "spec" | "specs"))
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.contains("_test.")
        || name.contains("_spec.")
        || name.starts_with("test_");
    let category = if test {
        "tests"
    } else if path.split('/').any(|p| matches!(p.to_ascii_lowercase().as_str(), "doc" | "docs" | "documentation"))
        || matches!(extension.as_str(), "md" | "mdx" | "rst" | "txt" | "adoc" | "pdf" | "tex")
        || name.starts_with("readme")
        || name.starts_with("license")
        || name.starts_with("copying")
    {
        "docs"
    } else if language.is_some() {
        "source"
    } else if matches!(extension.as_str(), "json" | "jsonc" | "yaml" | "yml" | "toml" | "ini" | "cfg" | "conf" | "xml" | "properties" | "env")
        || name.starts_with('.')
        || matches!(name.as_str(), "cargo.lock" | "gemfile" | "requirements.txt" | "procfile" | "go.sum")
    {
        "config"
    } else if matches!(
        extension.as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "svg"
            | "webp"
            | "avif"
            | "ico"
            | "bmp"
            | "tif"
            | "tiff"
            | "woff"
            | "woff2"
            | "ttf"
            | "otf"
            | "mp3"
            | "wav"
            | "ogg"
            | "mp4"
            | "webm"
            | "mov"
            | "csv"
            | "tsv"
            | "parquet"
            | "zip"
            | "gz"
            | "tar"
            | "bin"
            | "wasm"
    ) {
        "assets"
    } else {
        "other"
    };
    FileInfo { extension, language, category }
}

/// This is an explicit common-path heuristic, not .gitignore or Linguist parsing.
fn is_ignored(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    ignored_directory(path)
        || name.ends_with(".min.js")
        || name.ends_with(".min.css")
        || name.ends_with(".map")
        || name.ends_with(".generated.ts")
        || name.ends_with(".generated.js")
        || name.ends_with(".generated.rs")
        || name.ends_with(".g.cs")
        || name.ends_with(".pb.go")
        || name.ends_with("_pb2.py")
        || matches!(
            name,
            "package-lock.json"
                | "npm-shrinkwrap.json"
                | "yarn.lock"
                | "pnpm-lock.yaml"
                | "bun.lock"
                | "bun.lockb"
                | "cargo.lock"
                | "poetry.lock"
                | "uv.lock"
                | "composer.lock"
                | "gemfile.lock"
                | "go.sum"
        )
}

fn ignored_directory(path: &str) -> bool {
    path.split('/').any(|p| {
        matches!(
            p.to_ascii_lowercase().as_str(),
            ".git"
                | "node_modules"
                | "vendor"
                | "third_party"
                | "third-party"
                | "dist"
                | "build"
                | "target"
                | "coverage"
                | ".next"
                | ".nuxt"
                | ".cache"
                | "__pycache__"
                | ".venv"
                | "venv"
                | "generated"
                | "__generated__"
        )
    })
}

#[derive(Default, Serialize)]
struct SearchStats {
    total_files: usize,
    filtered_files: usize,
    scanned_files: usize,
    scanned_bytes: usize,
    matched_files: usize,
    matched_lines: usize,
    returned_files: usize,
    returned_lines: usize,
    skipped_filtered: usize,
    skipped_binary: usize,
    skipped_large: usize,
    skipped_gitlinks: usize,
    skipped_unavailable: usize,
    truncated: bool,
    scan_limited: bool,
}

#[derive(Serialize)]
struct SearchFile {
    path: String,
    extension: String,
    language: Option<&'static str>,
    category: &'static str,
    size: usize,
    score: i64,
    positions: Vec<usize>,
    lines: Vec<SearchLine>,
    matches: usize,
}

#[derive(Serialize)]
struct SearchLine {
    line: usize,
    text: String,
    ranges: Vec<(usize, usize)>,
    clipped: bool,
}

pub(super) struct SearchDocument<'a> {
    pub path: &'a str,
    pub mode: u32,
    pub size: usize,
    pub text: Option<&'a str>,
    pub state: &'a str,
}

pub(super) fn search(store: &Store, flat: &Flat, opts: &SearchOptions) -> Value {
    let documents: Vec<_> = flat.iter().map(|(path, entry)| {
        let bytes = if entry.mode == 0o160000 { None } else { store.blob(&entry.oid).ok() };
        let state = match bytes {
            None => "unavailable",
            Some(bytes) if bytes.len() > MAX_FILE_BYTES => "large",
            Some(bytes) if bytes.contains(&0) || std::str::from_utf8(bytes).is_err() => "binary",
            _ => "text",
        };
        SearchDocument { path, mode: entry.mode, size: bytes.map_or(0, |b| b.len()),
            text: bytes.filter(|_| state == "text").and_then(|b| std::str::from_utf8(b).ok()), state }
    }).collect();
    search_documents(&documents, opts)
}

/// Both Git snapshots and durable SQL snapshots use identical matching/ranking.
/// Text absent from an FTS candidate row is a proved miss, not an unavailable file.
pub(super) fn search_documents(documents: &[SearchDocument<'_>], opts: &SearchOptions) -> Value {
    let mut stats = SearchStats { total_files: documents.len(), ..Default::default() };
    let mut files: Vec<SearchFile> = vec![];
    let mut examined = 0;
    for document in documents {
        let path = document.path;
        if document.mode == 0o160000 { stats.skipped_gitlinks += 1; continue; }
        let info = file_info(path);
        if !opts.accepts(path, &info) { stats.skipped_filtered += 1; continue; }
        stats.filtered_files += 1;
        if stats.scan_limited && opts.mode == "content" { continue; }
        if document.state == "unavailable" { stats.skipped_unavailable += 1; continue; }
        let path_match = if opts.mode != "content" { file_match(path, &opts.query, opts.fuzzy, opts.case) } else { None };
        if opts.mode == "files" { stats.scanned_files += 1; }
        let (lines, matches) = if let Some(matcher) = &opts.matcher {
            match document.state {
                "large" => { stats.skipped_large += 1; (vec![], 0) },
                "binary" => { stats.skipped_binary += 1; (vec![], 0) },
                _ => document.text.map(|text| scan_content(text.as_bytes(), matcher, &mut stats, &mut examined)).unwrap_or((vec![], 0)),
            }
        } else { (vec![], 0) };
        if path_match.is_none() && matches == 0 { continue; }
        stats.matched_files += 1;
        let (score, positions) = match path_match {
            Some((score, positions)) => (if opts.mode == "all" { score.max(1) } else { score }, positions),
            None => (0, vec![]),
        };
        let file = SearchFile { path: path.to_string(), extension: info.extension, language: info.language, category: info.category,
            size: document.size, score, positions, lines, matches };
        let at = files.partition_point(|f| f.score > score || (f.score == score && f.path.as_str() < path));
        if at < opts.limit { files.insert(at, file); files.truncate(opts.limit); }
    }
    stats.returned_files = files.len();
    stats.returned_lines = files.iter().map(|f| f.lines.len()).sum();
    stats.truncated |= stats.scan_limited || stats.matched_files > files.len();
    json!({ "mode": opts.mode, "query": opts.query, "files": files, "stats": stats,
        "limits": { "max_file_bytes": MAX_FILE_BYTES, "max_scan_bytes": MAX_SCAN_BYTES, "max_files": MAX_CONTENT_FILES,
            "max_results": MAX_RESULTS, "max_lines_per_file": MAX_LINES_PER_FILE } })
}

fn scan_content(bytes: &[u8], matcher: &Regex, stats: &mut SearchStats, examined: &mut usize) -> (Vec<SearchLine>, usize) {
    if stats.scan_limited {
        return (vec![], 0);
    }
    *examined += 1;
    if *examined > MAX_CONTENT_FILES {
        stats.scan_limited = true;
        return (vec![], 0);
    }
    if bytes.len() > MAX_FILE_BYTES {
        stats.skipped_large += 1;
        return (vec![], 0);
    }
    if stats.scanned_bytes + bytes.len() > MAX_SCAN_BYTES {
        stats.scan_limited = true;
        return (vec![], 0);
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        stats.skipped_binary += 1;
        return (vec![], 0);
    };
    if bytes.contains(&0) {
        stats.skipped_binary += 1;
        return (vec![], 0);
    }
    stats.scanned_files += 1;
    stats.scanned_bytes += bytes.len();
    let mut lines = vec![];
    let mut matches = 0;
    for (i, line) in text.lines().enumerate() {
        let mut ranges = vec![];
        let mut line_matches = 0;
        for m in matcher.find_iter(line).filter(|m| !m.is_empty()) {
            line_matches += 1;
            if ranges.len() < 16 {
                ranges.push((m.start(), m.end()));
            }
        }
        if line_matches == 0 {
            continue;
        }
        matches += line_matches;
        stats.matched_lines += 1;
        if lines.len() < MAX_LINES_PER_FILE {
            lines.push(snippet(i + 1, line, &ranges));
        } else {
            stats.truncated = true;
        }
    }
    (lines, matches)
}

/// Offsets in JSON use JavaScript UTF-16 units, never Rust byte positions.
fn utf16_offset(s: &str, byte: usize) -> usize {
    s[..byte].encode_utf16().count()
}

fn snippet(line_number: usize, line: &str, ranges: &[(usize, usize)]) -> SearchLine {
    let first = ranges[0].0;
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let focus = chars.partition_point(|(byte, _)| *byte < first);
    let start_char = focus.saturating_sub(60);
    let end_char = (start_char + MAX_SNIPPET_CHARS).min(chars.len());
    let start = chars.get(start_char).map(|x| x.0).unwrap_or(line.len());
    let end = chars.get(end_char).map(|x| x.0).unwrap_or(line.len());
    let clipped = start > 0 || end < line.len();
    let before = if start > 0 { "…" } else { "" };
    let after = if end < line.len() { "…" } else { "" };
    let text = format!("{before}{}{after}", &line[start..end]);
    let positions = ranges
        .iter()
        .filter(|(a, b)| *a < end && *b > start)
        .map(|(a, b)| {
            (
                before.encode_utf16().count() + utf16_offset(&line[start..end], a.saturating_sub(start)),
                before.encode_utf16().count() + utf16_offset(&line[start..end], b.min(&end) - start),
            )
        })
        .collect();
    SearchLine { line: line_number, text, ranges: positions, clipped }
}

/// Lowercase preserves a mapping back to original UTF-16 units (including the
/// rare Unicode lowercase expansion), so highlighting works with emoji paths.
fn folded(s: &str, case: bool) -> Vec<(char, usize)> {
    let mut out = vec![];
    let mut offset = 0;
    for c in s.chars() {
        if case {
            out.push((c, offset));
        } else {
            out.extend(c.to_lowercase().map(|c| (c, offset)));
        }
        offset += c.len_utf16();
    }
    out
}

fn file_match(path: &str, query: &str, fuzzy: bool, case: bool) -> Option<(i64, Vec<usize>)> {
    if query.is_empty() {
        return Some((0, vec![]));
    }
    let text = folded(path, case);
    let name_start = text.iter().rposition(|(c, _)| *c == '/').map(|i| i + 1).unwrap_or(0);
    let mut score = 0;
    let mut positions = vec![];
    for word in query.split_whitespace() {
        let needle: Vec<char> = folded(word, case).iter().map(|x| x.0).collect();
        if needle.is_empty() {
            continue;
        }
        let full: Vec<char> = text.iter().map(|x| x.0).collect();
        let occurrence_score = |at: usize| {
            let exact = (at == name_start || at == 0) && at + needle.len() == full.len();
            let prefix = at == name_start;
            (if exact {
                10_000
            } else if prefix {
                9_000
            } else if at >= name_start {
                8_000
            } else {
                7_000
            }) - at as i64
        };
        if let Some(at) = full.windows(needle.len()).enumerate().filter(|(_, w)| *w == needle.as_slice()).map(|(i, _)| i).max_by_key(|at| occurrence_score(*at)) {
            score += occurrence_score(at);
            positions.extend(text[at..at + needle.len()].iter().map(|x| x.1));
            continue;
        }
        if !fuzzy {
            return None;
        }
        let subsequence = |start: usize| {
            let mut hits = vec![];
            let mut n = 0;
            for (i, (c, _)) in text.iter().enumerate().skip(start) {
                if needle.get(n) == Some(c) {
                    hits.push(i);
                    n += 1;
                }
            }
            (n == needle.len()).then_some(hits)
        };
        if let Some(hits) = subsequence(name_start).or_else(|| subsequence(0)) {
            let spread = hits.last().unwrap() - hits[0] + 1;
            let boundaries = hits.iter().filter(|i| **i == 0 || matches!(full[**i - 1], '/' | '_' | '-' | '.')).count();
            score += 5_000 + (boundaries * 100) as i64 - (spread - hits.len()) as i64 * 10 + if hits[0] >= name_start { 200 } else { 0 };
            positions.extend(hits.iter().map(|i| text[*i].1));
            continue;
        }
        // Typo fallback is deliberately bounded to short basenames. A narrow edit
        // band keeps a miss inexpensive even in a repository with many paths.
        let end = full[name_start..].iter().rposition(|c| *c == '.').map(|i| name_start + i).unwrap_or(full.len());
        let candidate = if needle.contains(&'.') { &full[name_start..] } else { &full[name_start..end] };
        let distance = if needle.len() >= 5 { 2 } else { 1 };
        if !(3..=48).contains(&needle.len()) || candidate.len() > 96 {
            return None;
        }
        let (edits, aligned) = edit_match(&needle, candidate, distance)?;
        score += 3_000 - edits as i64 * 100;
        positions.extend(aligned.into_iter().map(|i| text[name_start + i].1));
    }
    positions.sort_unstable();
    positions.dedup();
    // The UI slices strings by code units. Include both units of a matched emoji.
    let mut offset = 0;
    let mut extra = vec![];
    for c in path.chars() {
        if c.len_utf16() == 2 && positions.binary_search(&offset).is_ok() {
            extra.push(offset + 1);
        }
        offset += c.len_utf16();
    }
    positions.extend(extra);
    positions.sort_unstable();
    Some((score - path.chars().count() as i64, positions))
}

pub(super) fn rank_name(name: &str, query: &str, fuzzy: bool, case: bool) -> Option<(i64, Vec<usize>)> {
    file_match(name, query, fuzzy, case)
}

fn edit_match(a: &[char], b: &[char], max: usize) -> Option<(usize, Vec<usize>)> {
    if a.len().abs_diff(b.len()) > max {
        return None;
    }
    let mut rows = vec![vec![max + 1; b.len() + 1]; a.len() + 1];
    for j in 0..=b.len().min(max) {
        rows[0][j] = j;
    }
    for i in 1..=a.len() {
        if i <= max {
            rows[i][0] = i;
        }
        let start = i.saturating_sub(max).max(1);
        let end = (i + max).min(b.len());
        for j in start..=end {
            rows[i][j] = (rows[i - 1][j] + 1).min(rows[i][j - 1] + 1).min(rows[i - 1][j - 1] + usize::from(a[i - 1] != b[j - 1]));
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                rows[i][j] = rows[i][j].min(rows[i - 2][j - 2] + 1);
            }
        }
        if rows[i].iter().all(|d| *d > max) {
            return None;
        }
    }
    let edits = rows[a.len()][b.len()];
    if edits > max {
        return None;
    }
    let (mut i, mut j) = (a.len(), b.len());
    let mut aligned = vec![];
    while i > 0 && j > 0 {
        if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] && rows[i][j] == rows[i - 2][j - 2] + 1 {
            aligned.extend([j - 1, j - 2]);
            i -= 2;
            j -= 2;
        } else if a[i - 1] == b[j - 1] && rows[i][j] == rows[i - 1][j - 1] {
            aligned.push(j - 1);
            i -= 1;
            j -= 1;
        } else if rows[i][j] == rows[i - 1][j - 1] + 1 {
            i -= 1;
            j -= 1;
        } else if rows[i][j] == rows[i - 1][j] + 1 {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    aligned.reverse();
    Some((edits, aligned))
}

#[derive(Default, Serialize)]
struct Tally {
    files: usize,
    bytes: usize,
}

pub(super) fn digest(store: &Store, flat: &Flat) -> Value {
    let mut total_bytes = 0;
    let mut excluded_files = 0;
    let mut skipped_files = 0;
    let mut source_files = 0;
    let mut source_bytes = 0;
    let mut categories: BTreeMap<&str, Tally> = CATEGORIES.iter().map(|c| (*c, Tally::default())).collect();
    let mut languages: BTreeMap<&str, (Tally, BTreeSet<String>)> = BTreeMap::new();
    let mut types: BTreeMap<String, Tally> = BTreeMap::new();
    let mut directories: BTreeMap<String, Tally> = BTreeMap::new();
    let mut important = vec![];
    let mut stack: BTreeMap<(String, &'static str), BTreeSet<String>> = BTreeMap::new();
    let mut readme_path: Option<String> = None;
    for (path, entry) in flat {
        if entry.mode == 0o160000 {
            skipped_files += 1;
            continue;
        }
        let Ok(bytes) = store.blob(&entry.oid) else {
            skipped_files += 1;
            continue;
        };
        total_bytes += bytes.len();
        // Lockfiles count as generated, but remain useful package-manager evidence.
        if !ignored_directory(path) {
            record_tools(path, &mut stack);
        }
        if is_ignored(path) {
            excluded_files += 1;
            continue;
        }
        let info = file_info(path);
        let category = categories.entry(info.category).or_default();
        category.files += 1;
        category.bytes += bytes.len();
        let file_type = types.entry(info.extension.clone()).or_default();
        file_type.files += 1;
        file_type.bytes += bytes.len();
        let root = path.split_once('/').map(|(p, _)| p).unwrap_or(".");
        let directory = directories.entry(root.to_string()).or_default();
        directory.files += 1;
        directory.bytes += bytes.len();
        if let Some(language) = info.language.filter(|_| ["source", "tests"].contains(&info.category) && !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok()) {
            source_files += 1;
            source_bytes += bytes.len();
            let (tally, extensions) = languages.entry(language).or_default();
            tally.files += 1;
            tally.bytes += bytes.len();
            if !info.extension.is_empty() {
                extensions.insert(info.extension.clone());
            }
        }
        let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
        let root_file = !path.contains('/');
        let is_readme = matches!(name.as_str(), "readme.md" | "readme.markdown" | "readme.mdx" | "readme.rst" | "readme.txt" | "readme");
        if is_readme && readme_path.as_ref().is_none_or(|previous| readme_priority(path) < readme_priority(previous)) {
            readme_path = Some(path.clone());
        }
        let special = if is_readme {
            Some(("README", "readme"))
        } else if name.starts_with("license") || name.starts_with("copying") {
            Some(("License", "license"))
        } else if matches!(
            name.as_str(),
            "package.json"
                | "cargo.toml"
                | "pyproject.toml"
                | "requirements.txt"
                | "go.mod"
                | "gemfile"
                | "composer.json"
                | "pom.xml"
                | "build.gradle"
                | "build.gradle.kts"
                | "deno.json"
                | "deno.jsonc"
                | "mix.exs"
                | "pubspec.yaml"
        ) {
            Some(("Manifest", "manifest"))
        } else if matches!(
            name.as_str(),
            "dockerfile"
                | "containerfile"
                | "makefile"
                | "nerudo.toml"
                | "tsconfig.json"
                | "wrangler.toml"
                | "wrangler.json"
                | "wrangler.jsonc"
                | "next.config.js"
                | "next.config.ts"
                | "vite.config.ts"
                | "vite.config.js"
                | "compose.yaml"
                | "docker-compose.yml"
                | "security.md"
                | "contributing.md"
                | "agents.md"
        ) {
            Some(("Configuration", "configuration"))
        } else if (root_file || path.matches('/').count() <= 2)
            && matches!(name.as_str(), "main.rs" | "lib.rs" | "main.py" | "app.py" | "main.go" | "index.ts" | "index.js" | "app.tsx" | "page.tsx")
        {
            Some(("Entry point", "entrypoint"))
        } else {
            None
        };
        if let Some((label, kind)) = special {
            important.push(json!({ "path": path, "label": label, "kind": kind }));
        }
        if name == "package.json" && bytes.len() <= MAX_FILE_BYTES {
            if let Ok(package) = serde_json::from_slice::<Value>(bytes) {
                for section in ["dependencies", "devDependencies", "peerDependencies"] {
                    if let Some(dependencies) = package.get(section).and_then(Value::as_object) {
                        for dependency in dependencies.keys() {
                            let detected = match dependency.as_str() {
                                "react" => Some(("React", "framework")),
                                "next" => Some(("Next.js", "framework")),
                                "vinext" => Some(("Vinext", "framework")),
                                "vue" => Some(("Vue", "framework")),
                                "nuxt" => Some(("Nuxt", "framework")),
                                "svelte" => Some(("Svelte", "framework")),
                                "@sveltejs/kit" => Some(("SvelteKit", "framework")),
                                "astro" => Some(("Astro", "framework")),
                                "@angular/core" => Some(("Angular", "framework")),
                                "express" => Some(("Express", "framework")),
                                "hono" => Some(("Hono", "framework")),
                                "fastify" => Some(("Fastify", "framework")),
                                "typescript" => Some(("TypeScript", "language")),
                                "vite" => Some(("Vite", "tool")),
                                "vitest" => Some(("Vitest", "tool")),
                                "jest" => Some(("Jest", "tool")),
                                "@playwright/test" | "playwright" => Some(("Playwright", "tool")),
                                "wrangler" => Some(("Cloudflare Workers", "tool")),
                                "tailwindcss" => Some(("Tailwind CSS", "tool")),
                                _ => None,
                            };
                            if let Some((name, kind)) = detected {
                                add_stack(&mut stack, name, kind, path);
                            }
                        }
                    }
                }
                if let Some(manager) = package.get("packageManager").and_then(Value::as_str).and_then(|v| v.split('@').next()) {
                    if matches!(manager, "npm" | "pnpm" | "yarn" | "bun") {
                        add_stack(&mut stack, manager, "package-manager", path);
                    }
                }
            }
        }
    }
    let readme = readme_path.and_then(|path| {
        let bytes = store.blob(&flat.get(&path)?.oid).ok()?;
        let text = std::str::from_utf8(bytes).ok().filter(|_| !bytes.contains(&0));
        let truncated = bytes.len() > MAX_README_BYTES;
        let text = text.map(|text| {
            let mut end = text.len().min(MAX_README_BYTES);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text[..end].to_string()
        });
        Some(json!({ "path": path, "text": text, "truncated": truncated }))
    });
    let mut languages: Vec<_> =
        languages.into_iter().map(|(name, (tally, extensions))| json!({ "name": name, "files": tally.files, "bytes": tally.bytes, "extensions": extensions })).collect();
    languages.sort_by(|a, b| b["bytes"].as_u64().cmp(&a["bytes"].as_u64()).then_with(|| a["name"].as_str().cmp(&b["name"].as_str())));
    let mut file_types: Vec<_> = types.into_iter().map(|(extension, tally)| json!({ "extension": extension, "files": tally.files, "bytes": tally.bytes })).collect();
    file_types.sort_by(|a, b| b["files"].as_u64().cmp(&a["files"].as_u64()).then_with(|| a["extension"].as_str().cmp(&b["extension"].as_str())));
    let mut directories: Vec<_> = directories.into_iter().map(|(path, tally)| json!({ "path": path, "files": tally.files, "bytes": tally.bytes })).collect();
    directories.sort_by(|a, b| b["files"].as_u64().cmp(&a["files"].as_u64()).then_with(|| a["path"].as_str().cmp(&b["path"].as_str())));
    important.sort_by(|a, b| {
        let priority = |v: &Value| match v["kind"].as_str().unwrap_or("") {
            "readme" => 0,
            "manifest" => 1,
            "license" => 2,
            "configuration" => 3,
            _ => 4,
        };
        priority(a)
            .cmp(&priority(b))
            .then_with(|| a["path"].as_str().unwrap_or("").matches('/').count().cmp(&b["path"].as_str().unwrap_or("").matches('/').count()))
            .then_with(|| a["path"].as_str().cmp(&b["path"].as_str()))
    });
    // Return compact composition metadata even for a monorepo with many packages.
    let truncated = languages.len() > 40 || file_types.len() > 40 || directories.len() > 30 || important.len() > 40 || stack.len() > 40;
    languages.truncate(40);
    file_types.truncate(40);
    directories.truncate(30);
    important.truncate(40);
    let stack: Vec<_> =
        stack.into_iter().take(40).map(|((name, kind), paths)| json!({ "name": name, "kind": kind, "paths": paths.into_iter().take(12).collect::<Vec<_>>() })).collect();
    json!({ "method": "extension-based", "total_files": flat.len(), "total_bytes": total_bytes,
        "source_files": source_files, "source_bytes": source_bytes, "excluded_files": excluded_files, "skipped_files": skipped_files, "truncated": truncated,
        "categories": CATEGORIES.iter().map(|name| { let tally = &categories[name]; json!({ "name": name, "files": tally.files, "bytes": tally.bytes }) }).collect::<Vec<_>>(),
        "languages": languages, "file_types": file_types, "directories": directories, "stack": stack, "important_files": important, "readme": readme })
}

fn readme_priority(path: &str) -> (usize, usize, String) {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    (path.matches('/').count(), usize::from(name != "readme.md"), path.to_string())
}

fn add_stack(stack: &mut BTreeMap<(String, &'static str), BTreeSet<String>>, name: &str, kind: &'static str, path: &str) {
    stack.entry((name.into(), kind)).or_default().insert(path.into());
}

fn record_tools(path: &str, stack: &mut BTreeMap<(String, &'static str), BTreeSet<String>>) {
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    let detected = match name {
        "cargo.toml" => Some(("Cargo", "package-manager")),
        "pyproject.toml" | "requirements.txt" | "setup.py" => Some(("Python packaging", "package-manager")),
        "go.mod" => Some(("Go modules", "package-manager")),
        "gemfile" => Some(("Bundler", "package-manager")),
        "composer.json" => Some(("Composer", "package-manager")),
        "pom.xml" => Some(("Maven", "tool")),
        "build.gradle" | "build.gradle.kts" => Some(("Gradle", "tool")),
        "mix.exs" => Some(("Mix", "package-manager")),
        "pubspec.yaml" => Some(("Dart packages", "package-manager")),
        "deno.json" | "deno.jsonc" => Some(("Deno", "tool")),
        "dockerfile" | "containerfile" | "docker-compose.yml" | "compose.yaml" => Some(("Docker", "tool")),
        "wrangler.toml" | "wrangler.json" | "wrangler.jsonc" => Some(("Cloudflare Workers", "tool")),
        "package-lock.json" | "npm-shrinkwrap.json" => Some(("npm", "package-manager")),
        "pnpm-lock.yaml" | "pnpm-workspace.yaml" => Some(("pnpm", "package-manager")),
        "yarn.lock" => Some(("yarn", "package-manager")),
        "bun.lock" | "bun.lockb" => Some(("bun", "package-manager")),
        "poetry.lock" => Some(("Poetry", "package-manager")),
        "uv.lock" => Some(("uv", "package-manager")),
        _ if lower.starts_with(".github/workflows/") => Some(("GitHub Actions", "tool")),
        _ => None,
    };
    if let Some((name, kind)) = detected {
        add_stack(stack, name, kind, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nerudo_git::Oid;

    fn fixture(files: &[(&str, &[u8])]) -> (Store, Flat) {
        let mut store = Store::new();
        let flat = files.iter().map(|(path, bytes)| (path.to_string(), FileEntry { mode: 0o100644, oid: store.put_blob(bytes) })).collect();
        (store, flat)
    }

    fn options(params: &[(&str, &str)]) -> SearchOptions {
        SearchOptions::parse(&params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()).unwrap()
    }

    #[test]
    fn fuzzy_ranks_exact_then_prefix_then_subsequence_and_handles_typos() {
        let (store, flat) = fixture(&[("src/cart.ts", b""), ("src/cartographer.ts", b""), ("src/copy_and_render_text.ts", b""), ("other/catr.ts", b"")]);
        let result = search(&store, &flat, &options(&[("q", "cart")]));
        let paths: Vec<_> = result["files"].as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap()).collect();
        assert_eq!(paths[0], "src/cart.ts");
        assert_eq!(paths[1], "src/cartographer.ts");
        assert!(paths.contains(&"other/catr.ts"));
        assert!(paths.contains(&"src/copy_and_render_text.ts"));
        let result = search(&store, &flat, &options(&[("q", "carrt"), ("fuzzy", "0")]));
        assert!(result["files"].as_array().unwrap().is_empty());
    }

    #[test]
    fn filename_overlap_prefers_basename_and_typo_alignment_is_bounded() {
        let (_, positions) = file_match("app/app.ts", "app", true, false).unwrap();
        assert_eq!(positions, vec![4, 5, 6]);
        let (_, positions) = file_match("emoji/😀.ts", "😀", true, false).unwrap();
        assert_eq!(positions, vec![6, 7]);
        for (needle, candidate) in [("cart", "catr"), ("cart", "car"), ("cart", "carte"), ("cart", "care")] {
            let a: Vec<_> = needle.chars().collect();
            let b: Vec<_> = candidate.chars().collect();
            assert_eq!(edit_match(&a, &b, 1).unwrap().0, 1, "{needle} -> {candidate}");
        }
        assert!(edit_match(&"cart".chars().collect::<Vec<_>>(), &"careful".chars().collect::<Vec<_>>(), 1).is_none());
    }

    #[test]
    fn filters_are_recursive_and_ignored_paths_can_be_included() {
        let (store, flat) =
            fixture(&[("src/root.ts", b"find"), ("src/nested/test.ts", b"find"), ("tests/unit.test.ts", b"find"), ("node_modules/pkg/index.ts", b"find"), ("readme.md", b"find")]);
        let result = search(&store, &flat, &options(&[("ext", ".ts"), ("language", "typescript"), ("include", "src/**"), ("exclude", "**/test.*")]));
        assert_eq!(result["files"].as_array().unwrap().len(), 1);
        assert_eq!(result["files"][0]["path"], "src/root.ts");
        let result = search(&store, &flat, &options(&[("type", "tests")]));
        assert_eq!(result["files"][0]["path"], "tests/unit.test.ts");
        let result = search(&store, &flat, &options(&[("ignored", "1"), ("include", "node_modules/**")]));
        assert_eq!(result["files"][0]["path"], "node_modules/pkg/index.ts");
        let (store, flat) = fixture(&[("Dockerfile", b"FROM scratch"), ("src/a.ts", b"")]);
        let result = search(&store, &flat, &options(&[("ext", "none")]));
        assert_eq!(result["files"][0]["path"], "Dockerfile");
    }

    #[test]
    fn content_case_words_regex_and_unicode_ranges() {
        let (store, flat) = fixture(&[("src/emoji.ts", "😀 café CAT cat caterpillar\ncat(1)\n".as_bytes())]);
        let result = search(&store, &flat, &options(&[("q", "cat"), ("mode", "content"), ("word", "1"), ("case", "1")]));
        assert_eq!(result["files"][0]["matches"], 2);
        assert_eq!(result["files"][0]["lines"][0]["ranges"], json!([[12, 15]]));
        let result = search(&store, &flat, &options(&[("q", r"cat\(\d\)"), ("mode", "content"), ("regex", "1")]));
        assert_eq!(result["files"][0]["lines"][0]["line"], 2);
        let result = search(&store, &flat, &options(&[("q", "café"), ("mode", "content")]));
        assert_eq!(result["files"][0]["lines"][0]["ranges"], json!([[3, 7]]));
    }

    #[test]
    fn invalid_patterns_and_limits_are_errors_and_empty_patterns_do_not_scan() {
        let parse = |pairs: &[(&str, &str)]| SearchOptions::parse(&pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect());
        assert!(parse(&[("q", "("), ("mode", "content"), ("regex", "1")]).is_err());
        assert!(parse(&[("include", "src/[abc")]).is_err());
        assert!(parse(&[("limit", "101")]).is_err());
        assert!(parse(&[("q", &"a".repeat(513))]).is_err());
        let (store, flat) = fixture(&[("src/a.rs", b"cat")]);
        let result = search(&store, &flat, &options(&[("mode", "content")]));
        assert_eq!(result["stats"]["scanned_files"], 0);
        let result = search(&store, &flat, &options(&[("q", "^"), ("mode", "content"), ("regex", "1")]));
        assert!(result["files"].as_array().unwrap().is_empty());
    }

    #[test]
    fn binary_large_gitlinks_and_result_limits_are_visible() {
        let large = vec![b'a'; MAX_FILE_BYTES + 1];
        let (store, mut flat) = fixture(&[("a.ts", b"cat"), ("b.ts", b"cat"), ("binary.dat", b"cat\0bad"), ("large.txt", &large)]);
        flat.insert("submodule".into(), FileEntry { mode: 0o160000, oid: Oid::from_hex(&"0".repeat(40)).unwrap() });
        let result = search(&store, &flat, &options(&[("q", "cat"), ("mode", "content"), ("limit", "1")]));
        assert_eq!(result["stats"]["skipped_binary"], 1);
        assert_eq!(result["stats"]["skipped_large"], 1);
        assert_eq!(result["stats"]["skipped_gitlinks"], 1);
        assert_eq!(result["stats"]["matched_files"], 2);
        assert_eq!(result["stats"]["truncated"], true);
    }

    #[test]
    fn snippet_clips_long_lines_without_splitting_unicode() {
        let line = format!("{}needle{}", "😀".repeat(500), "é".repeat(500));
        let start = line.find("needle").unwrap();
        let snippet = snippet(7, &line, &[(start, start + 6)]);
        assert!(snippet.clipped);
        assert_eq!(snippet.line, 7);
        let units: Vec<_> = snippet.text.encode_utf16().collect();
        assert_eq!(String::from_utf16(&units[snippet.ranges[0].0..snippet.ranges[0].1]).unwrap(), "needle");
    }

    #[test]
    fn content_scan_and_line_caps_are_reported_even_when_more_matches_exist() {
        let body = vec![b'x'; MAX_FILE_BYTES];
        let source: Vec<_> = (0..70).map(|i| (format!("{i:02}.txt"), body.as_slice())).collect();
        let refs: Vec<_> = source.iter().map(|(p, b)| (p.as_str(), *b)).collect();
        let (store, flat) = fixture(&refs);
        let result = search(&store, &flat, &options(&[("q", "no-match"), ("mode", "content")]));
        assert_eq!(result["stats"]["scan_limited"], true);
        assert_eq!(result["stats"]["scanned_bytes"], MAX_SCAN_BYTES);
        assert_eq!(result["stats"]["filtered_files"], 70);
        let many_lines = "cat\n".repeat(25);
        let (store, flat) = fixture(&[("many.txt", many_lines.as_bytes())]);
        let result = search(&store, &flat, &options(&[("q", "cat"), ("mode", "content")]));
        assert_eq!(result["files"][0]["matches"], 25);
        assert_eq!(result["stats"]["matched_lines"], 25);
        assert_eq!(result["stats"]["returned_lines"], MAX_LINES_PER_FILE);
        assert_eq!(result["stats"]["truncated"], true);
    }

    #[test]
    fn digest_reports_code_bytes_and_evidence_without_vendor_skew() {
        let (store, flat) = fixture(&[
            ("src/main.rs", b"fn main() {}"),
            ("tests/unit.rs", b"#[test]"),
            ("README.md", b"# Project"),
            ("package.json", br#"{"dependencies":{"react":"1"},"packageManager":"pnpm@9"}"#),
            ("node_modules/pkg/index.ts", b"very big dependency"),
            ("Cargo.lock", b"lock"),
        ]);
        let result = digest(&store, &flat);
        assert_eq!(result["total_files"], 6);
        assert_eq!(result["excluded_files"], 2);
        assert_eq!(result["source_files"], 2);
        assert_eq!(result["source_bytes"], 19);
        assert_eq!(result["languages"][0]["name"], "Rust");
        assert_eq!(result["readme"]["path"], "README.md");
        assert!(result["stack"].as_array().unwrap().iter().any(|s| s["name"] == "React" && s["paths"] == json!(["package.json"])));
        assert!(result["stack"].as_array().unwrap().iter().any(|s| s["name"] == "pnpm"));
    }

    #[test]
    fn digest_omits_binary_code_and_selects_root_readme_with_utf8_preview() {
        let readme = format!("{}😀", "a".repeat(MAX_README_BYTES - 1));
        let (store, flat) = fixture(&[
            ("README.md", readme.as_bytes()),
            ("docs/README.md", b"nested"),
            ("bad.ts", b"text\0data"),
            ("bad.py", &[0xff, 0x80]),
            ("asset.wasm", b"\0asm"),
            ("vendor/pkg/Cargo.toml", b"[package]"),
        ]);
        let result = digest(&store, &flat);
        assert_eq!(result["source_files"], 0);
        assert_eq!(result["readme"]["path"], "README.md");
        assert_eq!(result["readme"]["truncated"], true);
        assert_eq!(result["readme"]["text"].as_str().unwrap().len(), MAX_README_BYTES - 1);
        assert!(result["stack"].as_array().unwrap().is_empty());
    }

    #[test]
    fn files_rank_all_paths_before_applying_the_limit() {
        let mut source = vec![];
        for i in 0..2000 {
            source.push((format!("aaa/{i}/needle_extra.ts"), b"".to_vec()));
        }
        source.push(("zzz/needle".into(), b"".to_vec()));
        let refs: Vec<_> = source.iter().map(|(p, b)| (p.as_str(), b.as_slice())).collect();
        let (store, flat) = fixture(&refs);
        let result = search(&store, &flat, &options(&[("q", "needle"), ("limit", "1")]));
        assert_eq!(result["files"][0]["path"], "zzz/needle");
        assert_eq!(result["stats"]["matched_files"], 2001);
    }

    #[test]
    fn unified_search_ranks_paths_first_and_merges_content_without_duplicates() {
        let (store, flat) = fixture(&[
            ("a-content.ts", b"needle in contents"),
            ("needle.ts", b"needle in both places"),
            ("z-needle-prefix.ts", b"no content hit"),
            ("needle.bin", b"binary\0data"),
        ]);
        let result = search(&store, &flat, &options(&[("q", "needle"), ("mode", "all")]));
        let files = result["files"].as_array().unwrap();
        assert_eq!(files.len(), 4);
        assert_eq!(files.last().unwrap()["path"], "a-content.ts");
        assert_eq!(files.iter().filter(|f| f["path"] == "needle.ts").count(), 1);
        let both = files.iter().find(|f| f["path"] == "needle.ts").unwrap();
        assert!(!both["positions"].as_array().unwrap().is_empty());
        assert_eq!(both["lines"][0]["line"], 1);
        assert_eq!(both["matches"], 1);
        assert_eq!(result["stats"]["matched_files"], 4);
        assert_eq!(result["stats"]["matched_lines"], 2);
        assert_eq!(result["stats"]["scanned_files"], 3);
        assert_eq!(result["stats"]["skipped_binary"], 1);
    }

    #[test]
    fn unified_search_ranks_late_filename_hits_after_content_budget_expires() {
        let body = vec![b'x'; MAX_FILE_BYTES];
        let mut source: Vec<_> = (0..70).map(|i| (format!("{i:02}.txt"), body.as_slice())).collect();
        source.push(("zzz-needle.txt".into(), b"needle"));
        let refs: Vec<_> = source.iter().map(|(p, b)| (p.as_str(), *b)).collect();
        let (store, flat) = fixture(&refs);
        let result = search(&store, &flat, &options(&[("q", "needle"), ("mode", "all"), ("limit", "1")]));
        assert_eq!(result["files"][0]["path"], "zzz-needle.txt");
        assert_eq!(result["stats"]["filtered_files"], 71);
        assert_eq!(result["stats"]["scan_limited"], true);
        assert_eq!(result["stats"]["scanned_bytes"], MAX_SCAN_BYTES);
    }
}
