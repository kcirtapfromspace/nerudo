//! Start a trunk from an existing repo instead of a seed. The trunk's first commit is a root
//! commit with the tree of one upstream commit plus a nerudo.toml: everything after it is
//! landed by the sequencer, and the upstream history before it stays where it came from.
//!
//! Two ways in. A public HTTPS repo (GitHub, say) is imported into Artifacts with the
//! binding's `import()`, as a read-only `<trunk>-upstream` repo, and the object reads the
//! start commit from it. A repo Artifacts can't reach (a private one) comes as a git pack
//! that holds the start commit and its tree. Either way only the object ever writes trunk.

use super::*;
use crate::types::HostCfg;
use std::collections::HashSet;

/// How long the object waits for an Artifacts import to become readable.
const IMPORT_WAIT_TRIES: u32 = 90;

#[derive(Deserialize)]
pub(super) struct ImportSpec {
    /// A public HTTPS git URL for Artifacts to import. Or send `pack`.
    #[serde(default)]
    pub url: Option<String>,
    /// The upstream branch to import.
    #[serde(default = "default_branch")]
    pub branch: String,
    /// Commits of `branch` to import; `commit` must be among them.
    #[serde(default = "default_depth")]
    pub depth: u32,
    /// A git pack, base64, that holds `commit` and its whole tree.
    #[serde(default)]
    pub pack: Option<String>,
    /// Read the start commit's whole tree into this object, and push it to a new trunk repo,
    /// instead of making the trunk repo as a fork of the import (the default).
    #[serde(default)]
    pub whole: bool,
    /// The upstream commit the trunk starts from.
    pub commit: Oid,
    /// nerudo.toml for the new trunk.
    pub rules: String,
    /// What to call the upstream in the first commit's message, like "cloudflare/agents".
    #[serde(default)]
    pub source: String,
}

fn default_branch() -> String {
    "main".into()
}

fn default_depth() -> u32 {
    100
}

impl Trunk {
    /// Make the trunk repo from `imp`, push its first commit, and keep that commit's objects
    /// in the sequencer's store. Ok(Err(..)) is the caller's mistake, as (status, message).
    /// Ok(Ok(token)) is the trunk's creation token; the caller revokes it.
    pub(super) async fn import_trunk(&self, cfg: &mut Config, imp: &ImportSpec) -> Result<std::result::Result<String, (u16, String)>> {
        if let Err(e) = Rules::parse(&imp.rules) {
            return Ok(Err((400, format!("{RULES_PATH}: {e}"))));
        }
        let HostCfg::Artifacts { repo, .. } = &cfg.host else {
            return Ok(Err((400, "an import needs the Artifacts host".into())));
        };
        let t = FetchTransport::default();
        let mut store = Store::new();
        let source = if imp.source.is_empty() { imp.url.clone().unwrap_or_else(|| "a git pack".into()) } else { imp.source.clone() };
        let mut upstream_note = String::new();
        if let Some(b64) = &imp.pack {
            let Some(bytes) = base64_decode(b64) else {
                return Ok(Err((400, "pack: not base64".into())));
            };
            match nerudo_git::parse_pack(&bytes, None) {
                Ok(objects) => {
                    for o in objects {
                        store.insert(o);
                    }
                }
                Err(e) => return Ok(Err((400, format!("pack: {e}")))),
            }
        } else if let Some(url) = &imp.url {
            if !url.starts_with("https://") {
                return Ok(Err((400, "url: Artifacts imports public https:// remotes".into())));
            }
            let name = format!("{repo}-upstream");
            let (remote, token) = match host::import_repo(&self.env, &cfg.host, &name, url, &imp.branch, imp.depth).await {
                Ok(x) => x,
                Err(e) => return Ok(Err((502, format!("import {url}: {e}")))),
            };
            // The import may still be running: read the branch until it is there.
            let up = Remote::new(&remote, Some(token));
            let want = format!("refs/heads/{}", imp.branch);
            let mut got = None;
            for _ in 0..IMPORT_WAIT_TRIES {
                if let Ok(refs) = ls_refs(&t, &up, &want).await {
                    if refs.iter().any(|(n, _)| *n == want) && !imp.whole {
                        // The trunk repo as a fork of the import, and the first commit made
                        // from its trees through the binding: no file passes through this object.
                        return self.import_by_fork(cfg, imp, &name, &source).await;
                    }
                    if let Some((_, head)) = refs.into_iter().find(|(n, _)| *n == want) {
                        // The start commit alone (deepen 1): its tree, not the history behind
                        // it, which a big repo can't afford in an object's memory. A server that
                        // won't send a commit that isn't a branch tip sends the whole import.
                        let shallow = nerudo_git::fetch_depth(&t, &up, &[imp.commit], &[], Some(1), &mut store).await;
                        let read = match shallow {
                            Ok(_) if store.contains(&imp.commit) => Ok(0),
                            other => {
                                console_warn!("shallow read of {} failed ({:?}); reading the whole import", imp.commit, other.err());
                                fetch(&t, &up, &[head], &[], &mut store).await
                            }
                        };
                        match read {
                            Ok(_) => {
                                got = Some(head);
                                break;
                            }
                            Err(e) => console_warn!("reading the import: {e}"),
                        }
                    }
                }
                Delay::from(Duration::from_secs(2)).await;
            }
            if got.is_none() {
                return Ok(Err((504, format!("the import of {url} was not readable after {} s", IMPORT_WAIT_TRIES * 2))));
            }
            upstream_note = format!(" It is also in the read-only Artifacts repo {name}.");
        } else {
            return Ok(Err((400, "an import needs a url or a pack".into())));
        }

        let x = match store.commit(&imp.commit) {
            Ok(c) => c,
            Err(_) => return Ok(Err((400, format!("{} is not in the import (raise depth, or send it in the pack)", imp.commit)))),
        };
        let mut files = match store.flatten(&x.tree) {
            Ok(f) => f,
            Err(e) => return Ok(Err((400, format!("the tree of {} is incomplete: {e}", imp.commit)))),
        };
        files.remove(nerudo_core::rules::LEGACY_RULES_PATH);
        files.insert(RULES_PATH.into(), FileEntry { mode: MODE_BLOB, oid: store.put_blob(imp.rules.as_bytes()) });
        let tree = store.write_tree(&files);
        let subject = x.message.lines().next().unwrap_or("").to_string();
        let short = &imp.commit.to_hex()[..7];
        let message = format!(
            "Import {source} at {short}: {subject}\n\nThis trunk starts from {source} at {}, with {RULES_PATH} added. Every later commit was landed by nerudo. History before this commit stays upstream.{upstream_note}\n",
            imp.commit
        );
        let sig = Signature::new("nerudo", "nerudo@trunk.invalid", (now() / 1000.0) as i64);
        let first = store.put_commit(&Commit { tree, parents: vec![], author: sig.clone(), committer: sig, extra: vec![], message });

        // Keep only what the first commit reaches: upstream history and other branches go.
        let keep: Vec<Oid> = store.missing_from(&first, &[]).map_err(g)?;
        let keep_set: HashSet<Oid> = keep.iter().copied().collect();
        store.retain(&keep_set);

        let branch = cfg.branch.strip_prefix("refs/heads/").unwrap_or(&cfg.branch).to_string();
        let (remote, token) = match host::create_trunk(&self.env, &cfg.host, &branch).await {
            Ok(x) => x,
            Err(e) => return Ok(Err((409, format!("create trunk repo: {e}")))),
        };
        cfg.remote = remote;
        let pack = write_pack(&store.object_list(&keep).map_err(g)?);
        let update = [RefUpdate { name: cfg.branch.clone(), old: Oid::ZERO, new: first }];
        let report = push(&t, &Remote::new(&cfg.remote, Some(token.clone())), &update, pack).await.map_err(g)?;
        if !report.all_ok() {
            return Ok(Err((502, format!("first push refused: {:?}", report.refs))));
        }
        self.put_store(store);
        Ok(Ok(token))
    }
}

impl Trunk {
    /// The trunk repo as an Artifacts fork of the import `upstream`, with a new first commit:
    /// the tree of `imp.commit` plus nerudo.toml. The binding reads the start commit and its
    /// root tree; the push sends three objects (nerudo.toml, the root tree, the commit). So
    /// the import's size does not reach this object's memory.
    async fn import_by_fork(&self, cfg: &mut Config, imp: &ImportSpec, upstream: &str, source: &str) -> Result<std::result::Result<String, (u16, String)>> {
        let HostCfg::Artifacts { repo, .. } = &cfg.host else {
            return Ok(Err((400, "an import needs the Artifacts host".into())));
        };
        let repo = repo.clone();
        let Some(up) = host::reader(&self.env, &cfg.host, upstream).await? else {
            return Ok(Err((400, "an import needs the Artifacts host".into())));
        };
        let Some(info) = up.commit(&imp.commit.to_hex()).await? else {
            return Ok(Err((400, format!("{} is not in the import (raise depth)", imp.commit))));
        };
        let Some(entries) = up.tree(&info.tree).await? else {
            return Ok(Err((502, format!("the import has no tree {}", info.tree))));
        };
        let mut store = Store::new();
        let rules = store.put_blob(imp.rules.as_bytes());
        let mut list: Vec<nerudo_git::TreeEntry> = vec![];
        for (mode, name, h) in entries.into_iter().filter(|(_, n, _)| !nerudo_core::rules::is_rules_path(n)) {
            list.push(nerudo_git::TreeEntry { mode, name, oid: Oid::from_hex(&h).map_err(g)? });
        }
        list.push(nerudo_git::TreeEntry { mode: MODE_BLOB, name: RULES_PATH.into(), oid: rules });
        let tree = store.insert(nerudo_git::Object::new(nerudo_git::Kind::Tree, nerudo_git::serialize_tree(&mut list)));
        let subject = info.message.lines().next().unwrap_or("").to_string();
        let short = &imp.commit.to_hex()[..7];
        let message = format!(
            "Import {source} at {short}: {subject}\n\nThis trunk starts from {source} at {}, with {RULES_PATH} added. Every later commit was landed by nerudo. History before this commit stays upstream. It is also in the read-only Artifacts repo {upstream}.\n",
            imp.commit
        );
        let sig = Signature::new("nerudo", "nerudo@trunk.invalid", (now() / 1000.0) as i64);
        let first = store.put_commit(&Commit { tree, parents: vec![], author: sig.clone(), committer: sig, extra: vec![], message });

        let (remote, token) = match host::fork_repo(&self.env, &cfg.host, upstream, &repo).await {
            Ok(x) => x,
            Err(e) => return Ok(Err((409, format!("create trunk repo: {e}")))),
        };
        cfg.remote = remote;
        let t = FetchTransport::default();
        let trunk = Remote::new(&cfg.remote, Some(token.clone()));
        // The fork has the import's branch; the first commit replaces it (or starts the branch).
        let old = ls_refs(&t, &trunk, &cfg.branch).await.map_err(g)?.into_iter().find(|(n, _)| *n == cfg.branch).map(|(_, o)| o).unwrap_or(Oid::ZERO);
        let objects = [store.get(&rules).map_err(g)?, store.get(&tree).map_err(g)?, store.get(&first).map_err(g)?];
        let update = [RefUpdate { name: cfg.branch.clone(), old, new: first }];
        let report = push(&t, &trunk, &update, write_pack(&objects)).await.map_err(g)?;
        if !report.all_ok() {
            return Ok(Err((502, format!("first push refused: {:?}", report.refs))));
        }
        self.put_store(store);
        Ok(Ok(token))
    }
}

/// Standard base64 (with or without padding); None on any other byte.
pub(super) fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let bytes: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=').collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= val(c)? << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
        if chunk.len() == 1 {
            return None;
        }
    }
    Some(out)
}
