//! The landing loop: pick a footprint-disjoint batch, apply it onto trunk in a private
//! staging worktree, run CI once for the batch, then append to the log and move trunk.
//! The sequencer is the only writer of trunk, so the final ref update never races.

use crate::config::Config;
use crate::git::{Git, command};
use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub type Key = (String, u32);

#[derive(Clone, Debug, serde::Deserialize, Serialize)]
pub struct LandRequest {
    pub change_id: String,
    pub rev: u32,
    pub commit: String,
    pub base: String,
    #[serde(default)]
    pub subject: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Queued,
    Landed,
    Ejected,
}

#[derive(Clone, Debug, Serialize)]
pub struct Rec {
    pub change_id: String,
    pub rev: u32,
    pub state: State,
    pub seq: Option<u64>,
    pub trunk_commit: Option<String>,
    pub reason: Option<String>,
    #[serde(skip)]
    pub req: LandRequest,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Stats {
    pub submitted: u64,
    pub landed: u64,
    pub batches: u64,
    pub batch_sizes: Vec<usize>,
    pub ci_runs: u64,
    pub bisects: u64,
    pub ejected_conflict: u64,
    pub ejected_ci: u64,
    pub footprint_deferrals: u64,
    pub derived_regenerations: u64,
    pub allocations: u64,
    pub commutative_merges: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct LogEntry {
    pub seq: u64,
    pub change_id: String,
    pub rev: u32,
    pub commit_in: String,
    pub commit_out: String,
    pub batch: u64,
    pub landed_at: f64,
    pub hash: String,
}

#[derive(Default)]
pub struct Inner {
    pub changes: HashMap<Key, Rec>,
    pub latest_rev: HashMap<String, u32>,
    pub queue: VecDeque<Key>,
    pub stats: Stats,
    pub log: Vec<LogEntry>,
    pub head: String,
    pub mirror: MirrorStats,
}

/// Downstream mirror (a forge) that only ever receives trunk from the sequencer.
#[derive(Clone, Debug, Default, Serialize)]
pub struct MirrorStats {
    pub pushes: u64,
    pub failures: u64,
    pub mirrored_head: String,
    pub mirrored_seq: u64,
    /// Seconds from landing to the mirror acknowledging a push that contains it.
    pub lags: Vec<f64>,
}

pub struct Shared {
    pub inner: Mutex<Inner>,
    pub wake: Condvar,
    pub mirror_wake: Condvar,
    pub changed: tokio::sync::Notify,
}

enum Outcome {
    Applied(String),
    Ejected(String),
}

pub struct Sequencer {
    pub shared: Arc<Shared>,
    repo: Git,
    staging: Git,
    staging_dir: PathBuf,
    tmp_index: PathBuf,
    trunk: String,
    cfg: Config,
    ci_seconds: f64,
    batch_cap: usize,
    db: Connection,
    footprints: HashMap<Key, BTreeSet<String>>,
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64()
}

impl Sequencer {
    pub fn open(repo: PathBuf, work: PathBuf, trunk: &str, ci_seconds: f64, batch_cap: usize)
        -> Result<Self> {
        std::fs::create_dir_all(&work)?;
        let repo_git = Git::new(&repo);
        let head = repo_git.rev(&format!("refs/heads/{trunk}"))?;
        let cfg_text = repo_git.run(&["show", &format!("{head}:nerudo.toml")])
            .or_else(|_| repo_git.run(&["show", &format!("{head}:landseq.toml")]))
            .context("trunk has no nerudo.toml")?;
        let cfg: Config = toml::from_str(&cfg_text)?;
        if let Some(c) = cfg.commutative.iter().find(|c| c.rule != "append") {
            anyhow::bail!("{}: commutative rule {:?} not implemented (only \"append\")", c.path, c.rule);
        }
        let staging_dir = work.join("staging");
        if !staging_dir.exists() {
            repo_git.run(&["worktree", "add", "-q", "--detach",
                           staging_dir.to_str().unwrap(), &head])?;
        }
        let db = Connection::open(work.join("log.db"))?;
        db.execute_batch(
            "create table if not exists landings(
                seq integer primary key, change_id text not null, rev integer not null,
                commit_in text not null, commit_out text not null, batch integer not null,
                landed_at real not null, prev_hash text not null, hash text not null,
                unique(change_id, rev));")?;
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner { head, ..Default::default() }),
            wake: Condvar::new(),
            mirror_wake: Condvar::new(),
            changed: tokio::sync::Notify::new(),
        });
        Ok(Self {
            shared,
            repo: repo_git,
            staging: Git::new(&staging_dir),
            tmp_index: work.join("strip.idx"),
            staging_dir,
            trunk: trunk.to_string(),
            cfg,
            ci_seconds,
            batch_cap,
            db,
            footprints: HashMap::new(),
        })
    }

    pub fn run(mut self) {
        loop {
            let snapshot: Vec<Key> = {
                let mut g = self.shared.inner.lock().unwrap();
                while g.queue.is_empty() {
                    g = self.shared.wake.wait(g).unwrap();
                }
                drop(g);
                // Short collection window so near-simultaneous submissions share a batch.
                std::thread::sleep(Duration::from_millis(30));
                self.shared.inner.lock().unwrap().queue.iter().cloned().collect()
            };
            let batch = self.select(&snapshot);
            {
                let mut g = self.shared.inner.lock().unwrap();
                g.queue.retain(|k| !batch.contains(k));
                g.stats.batches += 1;
                g.stats.batch_sizes.push(batch.len());
            }
            if let Err(e) = self.run_batch(batch.clone()) {
                eprintln!("batch error: {e:#}");
                for k in batch {
                    self.finish(&k, State::Ejected, None, None, Some(format!("internal: {e:#}")));
                }
                let head = self.shared.inner.lock().unwrap().head.clone();
                let _ = self.staging.run(&["checkout", "-q", "--detach", "-f", &head]);
            }
            self.shared.changed.notify_waiters();
        }
    }

    /// Greedy maximal footprint-disjoint set, in arrival order. A change that overlaps an
    /// earlier deferred change also waits, so conflicting changes keep their order.
    fn select(&mut self, queue: &[Key]) -> Vec<Key> {
        let mut taken = BTreeSet::new();
        let mut blocked = BTreeSet::new();
        let mut batch = Vec::new();
        let mut deferred = 0;
        for k in queue {
            let fp = match self.footprint(k) {
                Ok(fp) => fp,
                Err(_) => BTreeSet::from(["*".to_string()]),
            };
            let free = fp.is_disjoint(&taken) && fp.is_disjoint(&blocked);
            if batch.len() < self.batch_cap && free {
                taken.extend(fp);
                batch.push(k.clone());
            } else {
                if !free {
                    deferred += 1;
                }
                blocked.extend(fp);
            }
        }
        self.shared.inner.lock().unwrap().stats.footprint_deferrals += deferred;
        batch
    }

    /// W(C): footprint units of every non-special path the change touches.
    fn footprint(&mut self, k: &Key) -> Result<BTreeSet<String>> {
        if let Some(fp) = self.footprints.get(k) {
            return Ok(fp.clone());
        }
        let req = self.shared.inner.lock().unwrap().changes[k].req.clone();
        let names = self.repo.run(&["diff", "--name-only", "--no-renames", "--end-of-options", &req.base, &req.commit])?;
        let fp: BTreeSet<String> = names.lines()
            .filter(|p| !self.cfg.is_special(p))
            .map(|p| self.cfg.unit_of(p))
            .collect();
        self.footprints.insert(k.clone(), fp.clone());
        Ok(fp)
    }

    fn run_batch(&mut self, keys: Vec<Key>) -> Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        let start = self.shared.inner.lock().unwrap().head.clone();
        self.staging.run(&["checkout", "-q", "--detach", "-f", &start])?;
        let mut applied: Vec<(Key, String)> = Vec::new();
        for k in &keys {
            match self.apply(k)? {
                Outcome::Applied(c) => applied.push((k.clone(), c)),
                Outcome::Ejected(reason) => {
                    self.shared.inner.lock().unwrap().stats.ejected_conflict += 1;
                    self.finish(k, State::Ejected, None, None, Some(reason));
                }
            }
        }
        if applied.is_empty() {
            return Ok(());
        }
        let mut envs: Vec<(String, String)> = self.cfg.ci.env.iter()
            .map(|(k, v)| (k.clone(), v.clone())).collect();
        envs.push(("CI_SECONDS".into(), self.ci_seconds.to_string()));
        envs.push(("PYTHONDONTWRITEBYTECODE".into(), "1".into()));
        self.shared.inner.lock().unwrap().stats.ci_runs += 1;
        let (ok, out) = command(&self.staging_dir, &self.cfg.ci.command, &envs)?;
        if ok {
            let tip = applied.last().unwrap().1.clone();
            self.land(&applied, &start, &tip)?;
        } else if applied.len() == 1 {
            self.shared.inner.lock().unwrap().stats.ejected_ci += 1;
            let reason = format!("ci failed: {}", out.trim());
            self.finish(&applied[0].0, State::Ejected, None, None, Some(reason));
        } else {
            // Bisect: one of these broke the build. If every member passes alone, the
            // footprint model missed an interaction.
            self.shared.inner.lock().unwrap().stats.bisects += 1;
            let keys: Vec<Key> = applied.into_iter().map(|(k, _)| k).collect();
            let (a, b) = keys.split_at(keys.len() / 2);
            self.run_batch(a.to_vec())?;
            self.run_batch(b.to_vec())?;
        }
        Ok(())
    }

    /// Apply one change onto the staging tip. Exclusive paths are three-way merged; special
    /// paths are never taken from the change but merged by rule or regenerated.
    fn apply(&mut self, k: &Key) -> Result<Outcome> {
        let req = self.shared.inner.lock().unwrap().changes[k].req.clone();
        let head = self.staging.rev("HEAD")?;
        let changed = self.repo.run(&["diff", "--name-status", "--no-renames", "--end-of-options", &req.base, &req.commit])?;
        let changed: Vec<(String, String)> = changed.lines()
            .filter_map(|l| l.split_once('\t'))
            .map(|(s, p)| (s.to_string(), p.to_string()))
            .collect();

        // 1. The change with its special paths reverted to base.
        // update-index needs a work tree, so use the staging worktree (same object store) with
        // a scratch index; the staging worktree itself is untouched.
        let idx = self.staging.with_env("GIT_INDEX_FILE", self.tmp_index.to_str().unwrap());
        idx.run(&["read-tree", &req.commit])?;
        for (_, path) in changed.iter().filter(|(_, p)| self.cfg.is_special(p)) {
            let entry = self.repo.run(&["ls-tree", &req.base, "--", path])?;
            match entry.split_once('\t') {
                Some((meta, _)) => {
                    let f: Vec<&str> = meta.split_whitespace().collect();
                    idx.run(&["update-index", "--cacheinfo", &format!("{},{},{}", f[0], f[2], path)])?;
                }
                None => {
                    idx.run(&["update-index", "--force-remove", path])?;
                }
            }
        }
        let tree = idx.run(&["write-tree"])?.trim().to_string();
        let stripped = self.repo.run(&["commit-tree", &tree, "-p", &req.base, "-m", "stripped"])?
            .trim().to_string();

        // 2. Three-way merge of the exclusive part onto trunk, without a worktree.
        let out = self.repo.output(&["merge-tree", "--write-tree", "--name-only",
                                     "--merge-base", &req.base, &head, &stripped])?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        match out.status.code() {
            Some(0) => {}
            Some(1) => {
                let files: Vec<&str> = stdout.lines().skip(1).take_while(|l| !l.is_empty()).collect();
                return Ok(Outcome::Ejected(format!("conflict in {}", files.join(", "))));
            }
            _ => anyhow::bail!("merge-tree: {}", String::from_utf8_lossy(&out.stderr)),
        }
        let merged = stdout.lines().next().unwrap_or_default().to_string();
        self.staging.run(&["read-tree", "--reset", "-u", &merged])?;

        let mut stats = Stats::default();
        // 3. Commutative files: append the lines the change appended.
        for c in &self.cfg.commutative {
            if !changed.iter().any(|(_, p)| p == &c.path) {
                continue;
            }
            let diff = self.repo.run(&["diff", "-U0", &req.base, &req.commit, "--", &c.path])?;
            let added: String = diff.lines()
                .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
                .map(|l| format!("{}\n", &l[1..]))
                .collect();
            let path = self.staging_dir.join(&c.path);
            let mut text = std::fs::read_to_string(&path).unwrap_or_default();
            text.push_str(&added);
            std::fs::write(&path, text)?;
            stats.commutative_merges += 1;
        }
        // 4. Allocated names: number pending files now, on top of everything already landed.
        for a in &self.cfg.allocate {
            let prefix = format!("{}/{}", a.dir, a.pending_prefix);
            for (status, path) in &changed {
                let Some(name) = path.strip_prefix(&prefix) else { continue };
                if !status.starts_with('A') {
                    continue;
                }
                let content = self.repo.run(&["show", &format!("{}:{}", req.commit, path)])?;
                let dir = self.staging_dir.join(&a.dir);
                let next = std::fs::read_dir(&dir)?
                    .filter_map(|e| e.ok()?.file_name().into_string().ok())
                    .filter_map(|f| f.split('_').next()?.parse::<u64>().ok())
                    .max()
                    .unwrap_or(0) + 1;
                std::fs::write(dir.join(format!("{next:0w$}_{name}", w = a.width)), content)?;
                stats.allocations += 1;
            }
        }
        // 5. Derived files: regenerate.
        for d in &self.cfg.derived {
            let (ok, out) = command(&self.staging_dir, &d.command, &[])?;
            if !ok {
                anyhow::bail!("derived {}: {}", d.path, out);
            }
            stats.derived_regenerations += 1;
        }
        // 6. One linear commit per change, author preserved, Change-Id recorded.
        self.staging.run(&["add", "-A"])?;
        let author = self.repo.run(&["log", "-1", "--format=%an <%ae>", &req.commit])?;
        let msg = format!("{}\n\nChange-Id: {}\nRevision: {}\n", req.subject, req.change_id, req.rev);
        self.staging.run(&["commit", "-q", "--allow-empty", "--author", author.trim(), "-m", &msg])?;
        {
            let mut g = self.shared.inner.lock().unwrap();
            g.stats.commutative_merges += stats.commutative_merges;
            g.stats.allocations += stats.allocations;
            g.stats.derived_regenerations += stats.derived_regenerations;
        }
        Ok(Outcome::Applied(self.staging.rev("HEAD")?))
    }

    /// Append the batch to the hash-chained log and move trunk, in one transaction.
    fn land(&mut self, applied: &[(Key, String)], start: &str, tip: &str) -> Result<()> {
        let batch = self.shared.inner.lock().unwrap().stats.batches;
        let tx = self.db.transaction()?;
        let (mut seq, mut prev): (u64, String) = tx.query_row(
            "select coalesce(max(seq),0), coalesce((select hash from landings order by seq desc limit 1), '') from landings",
            [], |r| Ok((r.get::<_, i64>(0)? as u64, r.get(1)?)))?;
        if prev.is_empty() {
            prev = "0".repeat(64);
        }
        let mut entries = Vec::new();
        for ((cid, rev), commit_out) in applied {
            seq += 1;
            let commit_in = self.shared.inner.lock().unwrap().changes[&(cid.clone(), *rev)].req.commit.clone();
            let hash = hex::encode(Sha256::digest(
                format!("{prev}\n{seq}\n{cid}\n{rev}\n{commit_in}\n{commit_out}").as_bytes()));
            let t = now();
            tx.execute("insert into landings values (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                       params![seq as i64, cid, rev, commit_in, commit_out, batch as i64, t, prev, hash])?;
            entries.push(LogEntry { seq, change_id: cid.clone(), rev: *rev, commit_in,
                                    commit_out: commit_out.clone(), batch, landed_at: t,
                                    hash: hash.clone() });
            prev = hash;
        }
        // Compare-and-swap even though we are the only writer: a direct push to trunk is an
        // invariant violation and must fail loudly, not be overwritten.
        self.repo.run(&["update-ref", &format!("refs/heads/{}", self.trunk), tip, start])?;
        tx.commit()?;
        let mut g = self.shared.inner.lock().unwrap();
        g.head = tip.to_string();
        g.stats.landed += entries.len() as u64;
        for e in &entries {
            if let Some(r) = g.changes.get_mut(&(e.change_id.clone(), e.rev)) {
                r.state = State::Landed;
                r.seq = Some(e.seq);
                r.trunk_commit = Some(e.commit_out.clone());
            }
        }
        g.log.extend(entries);
        drop(g);
        self.shared.mirror_wake.notify_all();
        Ok(())
    }

    fn finish(&self, k: &Key, state: State, seq: Option<u64>, commit: Option<String>,
              reason: Option<String>) {
        if let Some(reason) = &reason {
            eprintln!("eject {}@{}: {}", k.0, k.1, reason.lines().take(3).collect::<Vec<_>>().join(" | "));
        }
        let mut g = self.shared.inner.lock().unwrap();
        if let Some(r) = g.changes.get_mut(k) {
            r.state = state;
            r.seq = seq;
            r.trunk_commit = commit;
            r.reason = reason;
        }
    }
}

/// Idempotent admission: resubmitting (change_id, rev) returns the existing record.
pub fn submit(shared: &Shared, req: LandRequest) -> Rec {
    let key = (req.change_id.clone(), req.rev);
    let mut g = shared.inner.lock().unwrap();
    if let Some(r) = g.changes.get(&key) {
        return r.clone();
    }
    let rec = Rec { change_id: req.change_id.clone(), rev: req.rev, state: State::Queued,
                    seq: None, trunk_commit: None, reason: None, req: req.clone() };
    g.changes.insert(key.clone(), rec.clone());
    let latest = g.latest_rev.entry(req.change_id.clone()).or_insert(0);
    *latest = (*latest).max(req.rev);
    g.queue.push_back(key);
    g.stats.submitted += 1;
    drop(g);
    shared.wake.notify_one();
    rec
}

/// Push trunk to the mirror whenever it moves, coalescing bursts into one push of the
/// latest head. A plain (fast-forward) push: the mirror has no other writers, so a
/// rejection means someone bypassed the sequencer, and it is counted, not forced.
pub fn run_mirror(shared: Arc<Shared>, repo: PathBuf, url: String, mirror_ref: String) {
    let git = Git::new(repo);
    // A forge outage must not become a retry storm: back off exponentially, capped at 30 s.
    let mut backoff = Duration::from_millis(500);
    loop {
        let (head, seq) = {
            let mut g = shared.inner.lock().unwrap();
            loop {
                let seq = g.log.last().map(|e| e.seq).unwrap_or(0);
                if seq > g.mirror.mirrored_seq {
                    break (g.head.clone(), seq);
                }
                g = shared.mirror_wake.wait(g).unwrap();
            }
        };
        let ok = git.run(&["push", "-q", &url, &format!("{head}:{mirror_ref}")]).is_ok();
        let t = now();
        let mut g = shared.inner.lock().unwrap();
        g.mirror.pushes += 1;
        if ok {
            let prev = g.mirror.mirrored_seq;
            let lags: Vec<f64> = g.log.iter().filter(|e| e.seq > prev && e.seq <= seq)
                .map(|e| t - e.landed_at).collect();
            g.mirror.lags.extend(lags);
            g.mirror.mirrored_seq = seq;
            g.mirror.mirrored_head = head;
            backoff = Duration::from_millis(500);
        } else {
            g.mirror.failures += 1;
            drop(g);
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    }
}
