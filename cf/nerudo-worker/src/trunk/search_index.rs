//! Durable, commit-scoped search snapshots. Git reads happen only in bounded alarm
//! jobs; an authorized query reads SQLite and runs the existing exact matcher.

use super::*;
use serde_json::{json, Value};
use worker::SqlStorageValue as Bind;

const TREE_CHUNK: usize = 8;
const BLOB_CHUNK: usize = 16;
const READ_TIMEOUT_MS: u64 = 1500;
const QUERY_BYTES: usize = 16 << 20;
const MAX_PATH_HINTS: usize = 20_000;
const MAX_GRAM_HINTS: usize = 100_000;

const GC_GENERATIONS_SQL: &str = "DELETE FROM search_generations WHERE published=1 AND head<>? AND head IN (SELECT head FROM search_generations WHERE published=1 AND head<>COALESCE((SELECT active_head FROM search_state WHERE slot=1),'') ORDER BY seq DESC LIMIT -1 OFFSET 2)";
const ACTIVATE_SQL: &str = "UPDATE search_state SET active_head=? WHERE slot=1 AND EXISTS(SELECT 1 FROM search_generations WHERE head=? AND complete=1)";
const GC_BLOBS_SQL: &str = "DELETE FROM search_blobs WHERE NOT EXISTS(SELECT 1 FROM search_files f WHERE f.oid=search_blobs.oid)";
const GRAMS_SCHEMA: &str = "CREATE VIRTUAL TABLE IF NOT EXISTS search_fts_grams USING fts5vocab(search_fts,'row')";
const PUBLISH_GRAMS_SQL: &str = "SELECT term gram FROM search_fts_grams LIMIT ?";

pub(super) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS search_generations(head TEXT PRIMARY KEY, seq INTEGER NOT NULL, base_head TEXT, phase INTEGER NOT NULL DEFAULT 0, complete INTEGER NOT NULL DEFAULT 0, published INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS search_state(slot INTEGER PRIMARY KEY CHECK(slot=1), active_head TEXT);
INSERT OR IGNORE INTO search_state(slot,active_head) VALUES(1,NULL);
CREATE TABLE IF NOT EXISTS search_files(head TEXT NOT NULL,path TEXT NOT NULL,oid TEXT NOT NULL,mode INTEGER NOT NULL,PRIMARY KEY(head,path));
CREATE INDEX IF NOT EXISTS search_files_oid ON search_files(oid);
CREATE TABLE IF NOT EXISTS search_trees(head TEXT NOT NULL,path TEXT NOT NULL,oid TEXT NOT NULL,PRIMARY KEY(head,path));
CREATE TABLE IF NOT EXISTS search_frontier(head TEXT NOT NULL,path TEXT NOT NULL,oid TEXT NOT NULL,PRIMARY KEY(head,path));
CREATE TABLE IF NOT EXISTS search_blobs(oid TEXT PRIMARY KEY,size INTEGER NOT NULL,state TEXT NOT NULL,body TEXT,unicode_text INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS search_settings(name TEXT PRIMARY KEY,value INTEGER NOT NULL);
"#;

pub(super) const FTS_SCHEMA: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS search_fts USING fts5(oid UNINDEXED,body,tokenize='trigram');
CREATE VIRTUAL TABLE IF NOT EXISTS search_fts_vocab USING fts5vocab(search_fts,'instance');
CREATE VIRTUAL TABLE IF NOT EXISTS search_fts_grams USING fts5vocab(search_fts,'row');
CREATE TRIGGER IF NOT EXISTS search_blob_insert AFTER INSERT ON search_blobs WHEN NEW.state='text' BEGIN INSERT INTO search_fts(oid,body) VALUES(NEW.oid,NEW.body); END;
CREATE TRIGGER IF NOT EXISTS search_blob_delete AFTER DELETE ON search_blobs BEGIN DELETE FROM search_fts WHERE oid=OLD.oid; END;
"#;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Dirty { pub head: Oid, pub seq: u64 }

#[derive(Deserialize)]
struct Active { active_head: Option<String> }

#[derive(Deserialize)]
struct Generation { head: String, seq: u64, base_head: Option<String>, phase: u64 }

#[derive(Deserialize)]
struct TreeRow { path: String, oid: String }

#[derive(Deserialize)]
struct FileRow { path: String, oid: String, mode: u32, size: usize, state: String, unicode_text: u64 }

#[derive(Deserialize)]
struct BodyRow { oid: String, body: String }

#[derive(Deserialize)]
struct GramRow { gram: String }

impl Trunk {
    fn index_rows<T: DeserializeOwned>(&self, query: &str, bindings: Vec<Bind>) -> Result<Vec<T>> {
        self.state.storage().sql().exec(query, Some(bindings))?.to_array::<T>()
    }

    fn index_exec(&self, query: &str, bindings: Vec<Bind>) -> Result<()> {
        self.state.storage().sql().exec(query, Some(bindings))?.to_array::<Value>()?;
        Ok(())
    }

    fn index_schema(&self) -> Result<()> {
        self.index_exec(SCHEMA, vec![])?;
        let settings = self.index_rows::<Value>("SELECT value FROM search_settings WHERE name='fts'", vec![])?;
        if settings.is_empty() {
            let enabled = self.index_exec(FTS_SCHEMA, vec![]).is_ok();
            self.index_exec("INSERT OR REPLACE INTO search_settings(name,value) VALUES('fts',?)", vec![i64::from(enabled).into()])?;
        }
        // Upgrade an already populated index without rebuilding its text.
        if self.index_fts()? { self.index_exec(GRAMS_SCHEMA, vec![])?; }
        Ok(())
    }

    fn index_fts(&self) -> Result<bool> {
        Ok(self.index_rows::<Value>("SELECT value FROM search_settings WHERE name='fts'", vec![])?.first().is_some_and(|v| v["value"] == 1))
    }

    fn index_active(&self) -> Result<Option<String>> {
        Ok(self.index_rows::<Active>("SELECT active_head FROM search_state WHERE slot=1", vec![])?.into_iter().next().and_then(|v| v.active_head))
    }

    pub(super) async fn queue_search_index(&self, head: Oid, seq: u64) -> Result<()> {
        self.index_schema()?;
        let active = self.index_active()?;
        self.index_exec("INSERT OR IGNORE INTO search_generations(head,seq,base_head) VALUES(?,?,?)", vec![head.to_hex().into(), (seq as i64).into(), active.into()])?;
        self.ensure_alarm().await
    }

    /// Admin-only scheduling hook. It never reads Git or returns indexed contents.
    pub(super) async fn api_search_index(&self) -> Result<Response> {
        let meta: Meta = self.get("meta").await?.unwrap_or_default();
        let Some(head) = meta.head else { return bad(404, "trunk has no commit"); };
        self.put("search_dirty", &Dirty { head, seq: meta.seq }).await?;
        self.queue_search_index(head, meta.seq).await?;
        // Registration may occur after an unnamed repository finished its local
        // snapshot. An explicit backfill also republishes such a ready snapshot.
        self.index_exec("UPDATE search_generations SET published=0 WHERE head=?", vec![head.to_hex().into()])?;
        self.notify_search_dirty(head, meta.seq);
        Response::from_json(&json!({ "head": head, "seq": meta.seq, "state": "scheduled" }))
    }

    pub(super) async fn api_search_index_status(&self) -> Result<Response> {
        self.index_schema()?;
        let meta: Meta = self.get("meta").await?.unwrap_or_default();
        let active = self.index_active()?;
        let jobs = self.index_rows::<Value>("SELECT head,seq,phase,complete,published,(SELECT COUNT(*) FROM search_frontier f WHERE f.head=g.head) pending_trees,(SELECT COUNT(*) FROM search_files f WHERE f.head=g.head) files,(SELECT COUNT(*) FROM search_files f WHERE f.head=g.head AND mode<>57344 AND NOT EXISTS(SELECT 1 FROM search_blobs b WHERE b.oid=f.oid)) pending_files FROM search_generations g WHERE complete=0 OR published=0 ORDER BY seq DESC LIMIT 8", vec![])?;
        Response::from_json(&json!({ "head": meta.head, "seq": meta.seq, "active_head": active,
            "ready": meta.head.is_some_and(|h| active.as_deref()==Some(&h.to_hex())), "jobs": jobs,
            "fts": self.index_fts()?, "tree_chunk": TREE_CHUNK, "blob_chunk": BLOB_CHUNK, "read_timeout_ms": READ_TIMEOUT_MS }))
    }

    pub(super) fn notify_search_dirty(&self, head: Oid, seq: u64) {
        let env = self.env.clone();
        let trunk = self.state.id().to_string();
        // Config's trunk is the namespace name, while state.id is opaque. Resolve
        // the configuration inside the background task, without delaying landing.
        let storage = self.state.storage();
        self.state.wait_until(async move {
            let result = async {
                let raw: Option<String> = storage.get("cfg").await?;
                let Some(cfg) = raw.and_then(|s| serde_json::from_str::<Config>(&s).ok()) else { return Ok::<(), Error>(()); };
                hub_index_call(&env, &cfg.trunk, "dirty", &json!({ "head": head, "seq": seq })).await
            }.await;
            if result.is_err() { console_warn!("search index dirty notification will retry for {trunk}"); }
        });
    }

    /// Read a completed generation, or an explicit incomplete status while a job
    /// prepares it. Queries never call ui_store, fetch or read_ui_misses.
    pub(super) async fn indexed_search(&self, options: &super::search::SearchOptions, requested: Oid, seq: u64, current: Oid, historical: bool) -> Result<Value> {
        self.index_schema()?;
        let wanted = requested.to_hex();
        let complete = self.index_rows::<Value>("SELECT complete FROM search_generations WHERE head=?", vec![wanted.clone().into()])?.first().is_some_and(|r| r["complete"] == 1);
        let head = if complete { Some(wanted.clone()) } else if historical { None } else { self.index_active()? };
        if !complete { self.queue_search_index(requested, seq).await?; }
        let mut rows: Vec<FileRow> = vec![];
        let mut bodies: BTreeMap<String, String> = BTreeMap::new();
        let mut limited = false;
        if let Some(head) = &head {
            let query = "SELECT f.path,f.oid,f.mode,COALESCE(b.size,0) size,COALESCE(b.state,'unavailable') state,COALESCE(b.unicode_text,0) unicode_text FROM search_files f LEFT JOIN search_blobs b ON b.oid=f.oid WHERE f.head=? ORDER BY f.path";
            let cursor = self.state.storage().sql().exec(query, Some(vec![head.clone().into()]))?;
            for row in cursor.next::<FileRow>() { rows.push(row?); }
            if options.wants_content() {
                let candidates = if let Some(phrase) = options.content_phrase().filter(|_| self.index_fts().unwrap_or(false)) {
                    let mut candidates = BTreeSet::new();
                    // Parameterized, quoted literal FTS query; never interpret a
                    // user's source text as the FTS query language.
                    let result = (|| -> Result<()> {
                        let cursor = self.state.storage().sql().exec("SELECT oid FROM search_fts WHERE search_fts MATCH ? AND oid IN (SELECT oid FROM search_files WHERE head=?)", Some(vec![phrase.into(),head.clone().into()]))?;
                        for row in cursor.next::<Value>() { if let Some(oid) = row?["oid"].as_str() { candidates.insert(oid.to_string()); } }
                        Ok(())
                    })();
                    // A tokenizer/query failure can reduce performance, never
                    // search coverage: verify all text in that case.
                    result.is_ok().then_some(candidates)
                } else { None };
                let wanted: BTreeSet<_> = rows.iter().filter(|r| r.state == "text" && options.accepts_path(&r.path)
                    && (r.unicode_text != 0 || candidates.as_ref().is_none_or(|c| c.contains(&r.oid)))).map(|r| r.oid.clone()).collect();
                let wanted: Vec<_> = wanted.into_iter().collect();
                let mut bytes = 0;
                for chunk in wanted.chunks(64) {
                    let query = format!("SELECT oid,body FROM search_blobs WHERE oid IN ({}) AND state='text'", vec!["?"; chunk.len()].join(","));
                    let cursor = self.state.storage().sql().exec(&query, Some(chunk.iter().cloned().map(Into::into).collect::<Vec<Bind>>()))?;
                    for row in cursor.next::<BodyRow>() {
                        let row = row?;
                        if bytes + row.body.len() > QUERY_BYTES { limited = true; break; }
                        bytes += row.body.len();
                        bodies.insert(row.oid, row.body);
                    }
                    if limited { break; }
                }
            }
        }
        let documents: Vec<_> = rows.iter().map(|r| super::search::SearchDocument { path: &r.path, mode: r.mode, size: r.size,
            text: bodies.get(&r.oid).map(String::as_str), state: &r.state }).collect();
        let mut out = super::search::search_documents(&documents, options);
        let state = if complete { "ready" } else if head.is_some() { "stale" } else if historical { "historical_unindexed" } else { "building" };
        out["commit"] = json!(head.as_deref().unwrap_or(&wanted));
        out["stats"]["indexed"] = json!(head.is_some());
        out["stats"]["index_state"] = json!(state);
        out["stats"]["index_head"] = json!(head);
        out["stats"]["current_head"] = json!(current);
        out["stats"]["index_pending"] = json!(!complete);
        out["stats"]["index_stale"] = json!(!complete && head.is_some());
        out["stats"]["partial"] = json!(!complete || limited || out["stats"]["truncated"] == true);
        if limited { out["stats"]["scan_limited"] = json!(true); out["stats"]["truncated"] = json!(true); }
        Ok(out)
    }

    /// Landing has priority. The search job processes one small chunk per alarm,
    /// with a bounded network deadline and a durable frontier/cursor.
    pub(super) async fn search_index_step(&self) -> Result<()> {
        let Some(cfg) = self.get::<Config>("cfg").await? else { return Ok(()); };
        let meta: Meta = self.get("meta").await?.unwrap_or_default();
        let Some(current) = meta.head else { return Ok(()); };
        if let Some(dirty) = self.get::<Dirty>("search_dirty").await? {
            self.queue_search_index(dirty.head, dirty.seq).await?;
            if dirty.head == current {
                let _ = within_index(hub_index_call(&self.env, &cfg.trunk, "dirty", &json!({ "head": dirty.head, "seq": dirty.seq }))).await;
            }
        }
        self.index_schema()?;
        let mut jobs = self.index_rows::<Generation>("SELECT head,seq,base_head,phase,complete,published FROM search_generations WHERE complete=0 OR published=0 ORDER BY (head=?) DESC,seq DESC LIMIT 1", vec![current.to_hex().into()])?;
        let Some(job) = jobs.pop() else { return Ok(()); };
        if job.phase == 0 {
            let oid = Oid::from_hex(&job.head).map_err(g)?;
            self.index_fill(&cfg, &[(oid, "commit")]).await?;
            let store = self.index_store().await?;
            let commit = store.commit(&oid).map_err(g)?;
            self.index_exec("INSERT OR REPLACE INTO search_frontier(head,path,oid) VALUES(?,'',?)", vec![job.head.clone().into(), commit.tree.to_hex().into()])?;
            self.index_exec("UPDATE search_generations SET phase=1 WHERE head=?", vec![job.head.clone().into()])?;
        } else if job.phase == 1 {
            let trees = self.index_rows::<TreeRow>("SELECT path,oid FROM search_frontier WHERE head=? ORDER BY path LIMIT ?", vec![job.head.clone().into(), (TREE_CHUNK as i64).into()])?;
            if trees.is_empty() { self.index_exec("UPDATE search_generations SET phase=2 WHERE head=?", vec![job.head.clone().into()])?; }
            else {
                let misses = trees.iter().map(|row| Oid::from_hex(&row.oid).map(|oid| (oid, "tree"))).collect::<nerudo_git::Result<Vec<_>>>().map_err(g)?;
                self.index_fill(&cfg, &misses).await?;
                let store = self.index_store().await?;
                for row in trees {
                    let old = match &job.base_head {
                        Some(base) => self.index_rows::<Value>("SELECT oid FROM search_trees WHERE head=? AND path=?", vec![base.clone().into(), row.path.clone().into()])?.first().and_then(|v| v["oid"].as_str()).map(str::to_string),
                        None => None,
                    };
                    if old.as_deref() == Some(&row.oid) {
                        self.index_copy_subtree(&job.head, job.base_head.as_deref().unwrap(), &row.path)?;
                    } else {
                        let entries = store.tree(&Oid::from_hex(&row.oid).map_err(g)?).map_err(g)?;
                        self.index_exec("INSERT OR REPLACE INTO search_trees(head,path,oid) VALUES(?,?,?)", vec![job.head.clone().into(), row.path.clone().into(), row.oid.clone().into()])?;
                        for entry in entries {
                            let path = if row.path.is_empty() { entry.name } else { format!("{}/{}", row.path, entry.name) };
                            if entry.mode == nerudo_git::MODE_TREE {
                                self.index_exec("INSERT OR IGNORE INTO search_frontier(head,path,oid) VALUES(?,?,?)", vec![job.head.clone().into(), path.into(), entry.oid.to_hex().into()])?;
                            } else {
                                self.index_exec("INSERT OR REPLACE INTO search_files(head,path,oid,mode) VALUES(?,?,?,?)", vec![job.head.clone().into(), path.into(), entry.oid.to_hex().into(), (entry.mode as i64).into()])?;
                            }
                        }
                    }
                    self.index_exec("DELETE FROM search_frontier WHERE head=? AND path=?", vec![job.head.clone().into(), row.path.into()])?;
                }
            }
        } else if job.phase == 2 {
            let files = self.index_rows::<FileRow>("SELECT f.path,f.oid,f.mode,0 size,'pending' state,0 unicode_text FROM search_files f WHERE head=? AND mode<>57344 AND NOT EXISTS(SELECT 1 FROM search_blobs b WHERE b.oid=f.oid) GROUP BY f.oid ORDER BY f.path LIMIT ?", vec![job.head.clone().into(), (BLOB_CHUNK as i64).into()])?;
            if files.is_empty() {
                self.index_exec("UPDATE search_generations SET complete=1,phase=3 WHERE head=?", vec![job.head.clone().into()])?;
                if job.head == current.to_hex() { self.index_exec(ACTIVATE_SQL, vec![job.head.clone().into(),job.head.clone().into()])?; }
            } else {
                let misses = files.iter().map(|file| Oid::from_hex(&file.oid).map(|oid| (oid, "blob"))).collect::<nerudo_git::Result<Vec<_>>>().map_err(g)?;
                self.index_fill(&cfg, &misses).await?;
                let store = self.index_store().await?;
                for file in files {
                    let bytes = store.blob(&Oid::from_hex(&file.oid).map_err(g)?).map_err(g)?;
                    let (state, body) = blob_text(bytes);
                    self.index_exec("INSERT OR IGNORE INTO search_blobs(oid,size,state,body,unicode_text) VALUES(?,?,?,?,?)", vec![file.oid.clone().into(), (bytes.len() as i64).into(), state.into(), body.map(str::to_string).into(), i64::from(body.is_some_and(|text| !text.is_ascii())).into()])?;
                }
            }
        } else {
            if job.head == current.to_hex() && within_index(async { Ok(self.hub_name(&cfg.trunk).await) }).await?.is_some() { self.publish_search_index(&cfg, &job).await?; }
            self.index_exec("UPDATE search_generations SET published=1 WHERE head=?", vec![job.head.clone().into()])?;
            if self.get::<Dirty>("search_dirty").await?.is_some_and(|dirty| dirty.head.to_hex() == job.head) { self.state.storage().delete("search_dirty").await?; }
            // Keep the active head and two recent immutable historical snapshots.
            self.index_exec(GC_GENERATIONS_SQL, vec![job.head.clone().into()])?;
            self.index_exec("DELETE FROM search_files WHERE head NOT IN(SELECT head FROM search_generations)", vec![])?;
            self.index_exec("DELETE FROM search_trees WHERE head NOT IN(SELECT head FROM search_generations)", vec![])?;
            // Immutable OID rows are shared by retained generations. Remove only
            // content that no live or in-progress snapshot still references.
            self.index_exec(GC_BLOBS_SQL, vec![])?;
        }
        let pending = !self.index_rows::<Value>("SELECT head FROM search_generations WHERE complete=0 OR published=0 LIMIT 1", vec![])?.is_empty();
        if pending { self.ensure_alarm().await?; }
        Ok(())
    }

    async fn index_fill(&self, cfg: &Config, misses: &[(Oid, &'static str)]) -> Result<()> {
        let fill = async {
            let mut store = self.index_store().await?;
            // Trim only before fetching, never between fill and its consumer.
            // A large object may take the current chunk over the cache budget.
            if store.bytes() > 64 << 20 { *store = Store::new(); }
            let mut readers = self.readers(cfg).await?;
            let transport = FetchTransport::default();
            self.resolve_many(&mut readers, &transport, &mut store, misses).await
        };
        within_index(fill).await
    }

    fn index_copy_subtree(&self, head: &str, base: &str, path: &str) -> Result<()> {
        let (prefix, chars) = subtree_prefix(path);
        for table in ["search_files", "search_trees"] {
            self.index_exec(&copy_subtree_sql(table), vec![head.into(), base.into(), path.into(), (chars as i64).into(), prefix.clone().into()])?;
        }
        Ok(())
    }

    async fn publish_search_index(&self, cfg: &Config, job: &Generation) -> Result<()> {
        let rows = self.index_rows::<FileRow>("SELECT f.path,f.oid,f.mode,COALESCE(b.size,0) size,COALESCE(b.state,'unavailable') state,COALESCE(b.unicode_text,0) unicode_text FROM search_files f LEFT JOIN search_blobs b ON b.oid=f.oid WHERE f.head=? ORDER BY path LIMIT ?", vec![job.head.clone().into(), (MAX_PATH_HINTS as i64 + 1).into()])?;
        let mut partial = rows.len() > MAX_PATH_HINTS;
        let mut paths: Vec<_> = rows.iter().take(MAX_PATH_HINTS).map(|row| {
            let mut value = super::search::path_metadata(&row.path);
            value["mode"] = json!(row.mode); value["size"] = json!(row.size); value
        }).collect();
        let unicode = self.index_rows::<Value>("SELECT 1 present FROM search_files f JOIN search_blobs b ON b.oid=f.oid WHERE f.head=? AND b.unicode_text=1 LIMIT 1", vec![job.head.clone().into()])?.len() > 0;
        let mut grams: Vec<String> = if self.index_fts()? {
            // A row vocabulary visits each distinct gram once, rather than
            // joining every token occurrence across every file. Retained older
            // blobs form a conservative superset; exact current-head matching
            // still happens inside the authorized repository.
            let rows = self.index_rows::<GramRow>(PUBLISH_GRAMS_SQL, vec![(MAX_GRAM_HINTS as i64 + 1).into()])?;
            partial |= rows.len() > MAX_GRAM_HINTS;
            rows.into_iter().filter(|r| r.gram.len() == 3 && r.gram.bytes().all(|c| (32..=126).contains(&c))).take(MAX_GRAM_HINTS).map(|r| r.gram.to_ascii_lowercase()).collect()
        } else { partial = true; vec![] };
        let mut descriptor = json!({ "version": 1, "trunk": cfg.trunk, "head": job.head, "seq": job.seq, "indexed_at": now() as i64,
            "complete": true, "paths": paths, "grams": grams, "unicode_content": unicode, "candidate_partial": partial });
        // Large repositories still use the local full index; Hub hints degrade
        // conservatively to avoid ever dropping a possible filename/content hit.
        while descriptor.to_string().len() > 4_000_000 {
            partial = true;
            if paths.len() > grams.len() / 8 { paths.truncate(paths.len() / 2); } else { grams.truncate(grams.len() / 2); }
            descriptor["paths"] = json!(paths); descriptor["grams"] = json!(grams); descriptor["candidate_partial"] = json!(partial);
        }
        within_index(hub_index_call(&self.env, &cfg.trunk, "", &descriptor)).await
    }
}

fn subtree_prefix(path: &str) -> (String, usize) {
    let prefix = if path.is_empty() { String::new() } else { format!("{path}/") };
    let chars = prefix.chars().count();
    (prefix, chars)
}

fn copy_subtree_sql(table: &str) -> String {
    let columns = if table == "search_files" { "path,oid,mode" } else { "path,oid" };
    format!("INSERT OR REPLACE INTO {table}(head,{columns}) SELECT ?,{columns} FROM {table} WHERE head=? AND (path=? OR substr(path,1,?)=?)")
}

fn blob_text(bytes: &[u8]) -> (&'static str, Option<&str>) {
    if bytes.len() > super::search::MAX_FILE_BYTES { return ("large", None); }
    if bytes.contains(&0) { return ("binary", None); }
    match std::str::from_utf8(bytes) { Ok(text) => ("text", Some(text)), Err(_) => ("binary", None) }
}

async fn within_index<F: std::future::Future<Output = Result<T>>, T>(future: F) -> Result<T> {
    let future = std::pin::pin!(future);
    let timer = std::pin::pin!(Delay::from(Duration::from_millis(READ_TIMEOUT_MS)));
    match select(future, timer).await { Either::Left((result, _)) => result, Either::Right(_) => Err(Error::RustError("search indexing chunk timed out; will resume".into())) }
}

async fn hub_index_call(env: &Env, trunk: &str, action: &str, body: &Value) -> Result<()> {
    let stub = env.durable_object("HUB")?.id_from_name("hub")?.get_stub()?;
    let mut init = RequestInit::new();
    init.with_method(if action.is_empty() { Method::Put } else { Method::Post }).with_body(Some(body.to_string().into()));
    let suffix = if action.is_empty() { String::new() } else { format!("/{action}") };
    let response = stub.fetch_with_request(Request::new_with_init(&format!("https://hub.internal/search-index/{trunk}{suffix}"), &init)?).await?;
    // An unnamed trunk has no Hub directory record yet. Registration explicitly
    // queues publication later, while the full local index remains available.
    if response.status_code() == 404 && action == "dirty" { return Ok(()); }
    if response.status_code() != 200 { return Err(Error::RustError("search index publication will retry".into())); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::{Command, Stdio};

    // worker's SQL binding is Wasm-only. Exercise the same schema/statements in
    // native SQLite, including the actual FTS5 trigram tokenizer and triggers.
    fn sqlite(script: &str, extra: Value) {
        let bootstrap = "import json,sys,sqlite3\np=json.load(sys.stdin)\nc=sqlite3.connect(':memory:')\nc.executescript(p['schema'])\nc.executescript(p['fts'])\n";
        let mut child = Command::new("python3").args(["-c", &format!("{bootstrap}{script}")]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("native SQLite tests need Python 3's sqlite3 module");
        let input = json!({"schema": SCHEMA, "fts": FTS_SCHEMA, "activate": ACTIVATE_SQL, "gc": GC_BLOBS_SQL, "gc_generations": GC_GENERATIONS_SQL, "publish_grams": PUBLISH_GRAMS_SQL,
            "copy_files": copy_subtree_sql("search_files"), "copy_trees": copy_subtree_sql("search_trees"), "extra": extra});
        child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "SQLite regression: {}", String::from_utf8_lossy(&output.stderr));
    }

    #[test]
    fn resumable_generation_cannot_replace_active_before_completion() {
        sqlite(r#"
c.execute("INSERT INTO search_generations(head,seq,phase,complete,published) VALUES('old',1,3,1,1)")
c.execute(p['activate'],('old','old'))
c.execute("INSERT INTO search_generations(head,seq,base_head,phase) VALUES('new',2,'old',1)")
c.execute("INSERT INTO search_frontier VALUES('new','src','tree')")
c.execute("INSERT INTO search_files VALUES('new','README.md','blob',33188)")
c.commit()
# A restart observes the persisted partial tree, but readers retain old head.
c.execute(p['activate'],('new','new'))
assert c.execute('SELECT active_head FROM search_state').fetchone()[0]=='old'
assert c.execute("SELECT path FROM search_frontier WHERE head='new'").fetchall()==[('src',)]
c.execute("DELETE FROM search_frontier WHERE head='new'")
c.execute("UPDATE search_generations SET complete=1,phase=3 WHERE head='new'")
c.execute(p['activate'],('new','new'))
assert c.execute('SELECT active_head FROM search_state').fetchone()[0]=='new'
"#, Value::Null);
    }

    #[test]
    fn incremental_subtree_copy_preserves_unicode_and_applies_renames_and_deletes() {
        let (prefix, chars) = subtree_prefix("é");
        sqlite(r#"
c.executemany('INSERT INTO search_files VALUES(?,?,?,?)',[
 ('old','é/a.rs','same',33188),('old','é/deep/b.rs','same',33188),
 ('old','edited/deleted.rs','deleted',33188),('old','edited/old.rs','rename',33188),
 ('old','éx/not-a-child.rs','other',33188)])
c.execute("INSERT INTO search_trees VALUES('old','é','unchanged')")
prefix,chars=p['extra']
for key in ['copy_files','copy_trees']:
 c.execute(p[key],('new','old','é',chars,prefix))
c.execute("INSERT INTO search_files VALUES('new','edited/new.rs','rename',33188)")
assert c.execute("SELECT path FROM search_files WHERE head='new' ORDER BY path").fetchall()==[('edited/new.rs',),('é/a.rs',),('é/deep/b.rs',)]
assert c.execute("SELECT oid FROM search_trees WHERE head='new' AND path='é'").fetchone()[0]=='unchanged'
# Root copying uses the empty prefix, including both direct and nested files.
c.execute(p['copy_files'],('root','old','',0,''))
assert c.execute("SELECT COUNT(*) FROM search_files WHERE head='root'").fetchone()[0]==5
"#, json!([prefix, chars]));
    }

    #[test]
    fn snapshot_gc_preserves_empty_active_and_recent_historical_commits() {
        sqlite(r#"
c.executemany('INSERT INTO search_generations(head,seq,phase,complete,published) VALUES(?,?,3,1,1)', [('active',5),('recent-empty',4),('recent',3),('historical',2),('old',1)])
c.execute(p['activate'],('active','active'))
c.execute("INSERT INTO search_trees VALUES('active','','empty-root')")
c.execute(p['gc_generations'],('historical',))
c.execute('DELETE FROM search_trees WHERE head NOT IN(SELECT head FROM search_generations)')
assert c.execute("SELECT complete FROM search_generations WHERE head='active'").fetchone()==(1,)
assert c.execute("SELECT oid FROM search_trees WHERE head='active'").fetchone()==('empty-root',)
assert c.execute("SELECT complete FROM search_generations WHERE head='recent-empty'").fetchone()==(1,)
assert c.execute("SELECT complete FROM search_generations WHERE head='recent'").fetchone()==(1,)
assert c.execute("SELECT complete FROM search_generations WHERE head='historical'").fetchone()==(1,)
assert c.execute("SELECT head FROM search_generations WHERE head='old'").fetchone() is None
"#, Value::Null);
    }

    #[test]
    fn publication_visits_distinct_grams_and_keeps_current_content_coverage() {
        sqlite(r#"
c.execute("INSERT INTO search_blobs VALUES('old',9,'text','olderword',0)")
c.execute("INSERT INTO search_blobs VALUES('new',180000,'text',?,0)",('newneedle '*20000,))
c.execute("INSERT INTO search_files VALUES('current','src/new.rs','new',33188)")
grams={row[0] for row in c.execute(p['publish_grams'],(100001,))}
assert {'new','nee','eed','edl','dle'} <= grams
assert 'old' in grams # Older blobs may cause extra candidates, never lost hits.
assert len(grams)<30 # Publication work does not grow with token occurrences.
"#, Value::Null);
    }

    #[test]
    fn immutable_blob_reuse_atomic_fts_insert_and_orphan_gc() {
        sqlite(r#"
c.execute("INSERT INTO search_blobs VALUES('shared',14,'text','hello world!\n',0)")
c.execute("INSERT OR IGNORE INTO search_blobs VALUES('shared',1,'text','different',0)")
assert c.execute("SELECT COUNT(*) FROM search_fts WHERE oid='shared'").fetchone()[0]==1
c.executemany('INSERT INTO search_files VALUES(?,?,?,?)',[('old','a.rs','shared',33188),('new','b.rs','shared',33188)])
c.execute("DELETE FROM search_files WHERE head='old'")
c.execute(p['gc'])
assert c.execute("SELECT body FROM search_blobs WHERE oid='shared'").fetchone()[0]=='hello world!\n'
c.execute("DELETE FROM search_files WHERE head='new'")
c.execute(p['gc'])
assert c.execute('SELECT COUNT(*) FROM search_blobs').fetchone()[0]==0
assert c.execute('SELECT COUNT(*) FROM search_fts').fetchone()[0]==0
# A statement failure rolls back its FTS trigger too; a retry cannot mistake an
# OID row for text that never became searchable.
c.executescript("CREATE TRIGGER fail_index AFTER INSERT ON search_blobs WHEN NEW.oid='fail' BEGIN SELECT RAISE(ABORT,'failure'); END;")
try:
 c.execute("INSERT INTO search_blobs VALUES('fail',7,'text','failure',0)")
 assert False
except sqlite3.IntegrityError: pass
assert c.execute("SELECT COUNT(*) FROM search_blobs WHERE oid='fail'").fetchone()[0]==0
assert c.execute("SELECT COUNT(*) FROM search_fts WHERE oid='fail'").fetchone()[0]==0
"#, Value::Null);
    }

    #[test]
    fn fts_literal_candidates_cover_quotes_punctuation_case_and_unicode_fallback() {
        let cases = ["call(\"Hello\")", "abc\"def", "abc.def", "a b", "line\nbreak", "éclair", "Hi"];
        let extra: Vec<_> = cases.into_iter().map(|q| {
            let opts = super::super::search::SearchOptions::parse(&BTreeMap::from([("mode".into(),"content".into()),("q".into(),q.into())])).unwrap();
            json!({"q":q,"phrase":opts.content_phrase()})
        }).collect();
        sqlite(r#"
for i,case in enumerate(p['extra']):
 body='prefix '+case['q'].upper()+' suffix'
 c.execute('INSERT INTO search_blobs VALUES(?,?,?,?,?)',(str(i),len(body),'text',body,int(not body.isascii())))
 if case['phrase'] is not None:
  hits=c.execute('SELECT oid FROM search_fts WHERE search_fts MATCH ?', (case['phrase'],)).fetchall()
  assert (str(i),) in hits,(case,hits)
 else:
  assert '\n' in case['q'] or len(case['q'])<3 or not case['q'].isascii()
"#, json!(extra));
        let regex = super::super::search::SearchOptions::parse(&BTreeMap::from([("mode".into(),"all".into()),("q".into(),"abc.*def".into()),("regex".into(),"1".into())])).unwrap();
        assert!(regex.content_phrase().is_none());
    }

    #[test]
    fn blob_classification_skips_binary_and_oversize_text() {
        assert_eq!(blob_text(b"Hello\n"),("text",Some("Hello\n")));
        assert_eq!(blob_text(b"binary\0content"),("binary",None));
        assert_eq!(blob_text(&[0xff]),("binary",None));
        assert_eq!(blob_text(&vec![b'a'; super::super::search::MAX_FILE_BYTES+1]),("large",None));
    }
}
