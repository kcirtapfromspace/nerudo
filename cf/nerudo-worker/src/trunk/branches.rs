//! Working branches: every agent's fork, the refs in it, and what became of the change each
//! ref carries. Nothing merges from a branch: the sequencer lands changes from them. A ref
//! carries a revision of a PR: `land/<change>-<rev>` names it, and a branch pushed with plain
//! git carries the revision of its fork-scoped PR whose commit it points at. A ref
//! whose revision landed, whose PR closed, or whose revision was ejected and then sent again
//! as a later revision, is stale, and cleanup deletes it.

use super::context::CONTEXT_BRANCH;
use super::git::{branch_owned_by, change_id_of};
use super::*;
use crate::types::HostCfg;

/// Revisions of a PR that are searched, newest first, for the commit of a plain branch.
const BRANCH_REVS: u32 = 32;
/// PRs of one branch that are searched: the first, and the ones a push opened after a merge.
const BRANCH_PRS: u32 = 16;

pub(super) struct Branch {
    pub agent: String,
    pub fork: Fork,
    pub refs: std::result::Result<Vec<RefRow>, String>,
}

pub(super) struct RefRow {
    pub name: String,
    pub oid: Oid,
    /// The PR revision the ref carries: the change, its revision and its state.
    pub change: Option<(String, u32, Option<Phase>)>,
    pub stale: bool,
}

impl Trunk {
    /// Every agent fork of this trunk, with its refs.
    pub(super) async fn branches(&self, cfg: &Config) -> Result<Vec<Branch>> {
        let mut out = vec![];
        for (k, v) in self.list_prefix("fa:").await? {
            let agent = k["fa:".len()..].to_string();
            let Ok(fork) = serde_json::from_str::<Fork>(&v) else { continue };
            let (fork, refs) = self.fork_refs(cfg, &agent, fork).await?;
            let refs = match refs {
                Ok(r) => {
                    let mut rows = vec![];
                    for (name, oid) in r {
                        rows.push(self.ref_row(&cfg.branch, &fork.name, name, oid).await?);
                    }
                    Ok(rows)
                }
                Err(e) => Err(e),
            };
            out.push(Branch { agent, fork, refs });
        }
        Ok(out)
    }

    /// The refs of a fork. A fork token lasts a day; when it has run out, the host gives a
    /// new one, and the object keeps it.
    pub(super) async fn fork_refs(&self, cfg: &Config, agent: &str, fork: Fork) -> Result<(Fork, std::result::Result<Vec<(String, Oid)>, String>)> {
        let t = FetchTransport::default();
        let first = ls_refs(&t, &Remote::new(&fork.remote, Some(fork.authorization.clone())), "refs/").await;
        let err = match first {
            Ok(r) => return Ok((fork, Ok(r))),
            Err(e) => e.to_string(),
        };
        if !matches!(cfg.host, HostCfg::Artifacts { .. }) {
            return Ok((fork, Err(err)));
        }
        match host::fork(&self.env, &cfg.host, agent, Some(fork.clone())).await {
            Ok(fresh) => {
                let rec = serde_json::to_string(&fresh)?;
                self.put_all(vec![(format!("f:{}", fresh.name), rec.clone()), (format!("fa:{agent}"), rec)]).await?;
                let again = ls_refs(&t, &Remote::new(&fresh.remote, Some(fresh.authorization.clone())), "refs/").await;
                Ok((fresh, again.map_err(|e| e.to_string())))
            }
            Err(e) => Ok((fork, Err(format!("{err}; a new token was refused: {e}")))),
        }
    }

    /// What the ref `name` of a fork carries. `main`: the trunk's branch, as a full ref name.
    pub(super) async fn ref_row(&self, main: &str, fork: &str, name: String, oid: Oid) -> Result<RefRow> {
        if let Some((cid, rev)) = land_ref(&name) {
            let rec = self.rec(&cid, rev).await?.filter(|r| r.req.fork == fork && r.req.commit == oid);
            let latest = self.get::<u32>(&format!("latest:{cid}")).await?.unwrap_or(rev);
            let stale = rec.as_ref().is_some_and(|r| stale(r, latest));
            return Ok(RefRow { name, oid, change: Some((cid, rev, rec.map(|r| r.state))), stale });
        }
        // A branch pushed with plain git carries the revision of its PR whose commit it points
        // at: a revision of its first PR, or after a merge, of a later PR (`feature-x..2`,
        // trunk/revisions.rs). A branch at any other commit has work that no revision has, so it
        // is never stale.
        if let Some(branch) = plain_branch(main, &name) {
            let first = self.branch_first_id(fork, &branch).await?;
            let mut cid = first.clone();
            for n in 2..=BRANCH_PRS + 1 {
                let Some(latest) = self.get::<u32>(&format!("latest:{cid}")).await? else { break };
                for rev in (latest.saturating_sub(BRANCH_REVS) + 1..=latest).rev() {
                    if let Some(r) = self.rec(&cid, rev).await?.filter(|r| r.req.commit == oid && branch_owned_by(&r.req, fork, &branch)) {
                        let stale = stale(&r, latest);
                        return Ok(RefRow { name, oid, change: Some((cid, rev, Some(r.state))), stale });
                    }
                }
                match super::revisions::successor(&first, n) {
                    Some(next) => cid = next,
                    None => break,
                }
            }
        }
        Ok(RefRow { name, oid, change: None, stale: false })
    }

    /// Delete the stale refs of `agent`'s fork. Returns how many it deleted.
    pub(super) async fn cleanup(&self, cfg: &Config, agent: &str) -> Result<std::result::Result<usize, String>> {
        let Some(fork) = self.get::<Fork>(&format!("fa:{agent}")).await? else {
            return Ok(Err(format!("{agent} has no fork on this trunk")));
        };
        let (fork, refs) = self.fork_refs(cfg, agent, fork).await?;
        let refs = match refs {
            Ok(r) => r,
            Err(e) => return Ok(Err(format!("cannot read the fork: {e}"))),
        };
        let mut updates = vec![];
        for (name, oid) in refs {
            if self.ref_row(&cfg.branch, &fork.name, name.clone(), oid).await?.stale {
                updates.push(RefUpdate { name, old: oid, new: Oid::ZERO });
            }
        }
        if updates.is_empty() {
            return Ok(Ok(0));
        }
        let t = FetchTransport::default();
        let remote = Remote::new(&fork.remote, Some(fork.authorization.clone()));
        match push(&t, &remote, &updates, Vec::new()).await {
            Ok(report) if report.all_ok() => Ok(Ok(updates.len())),
            Ok(report) => Ok(Err(format!("the host refused some deletions: {:?}", report.refs))),
            Err(e) => Ok(Err(e.to_string())),
        }
    }

    /// `POST .../cleanup {as}` (agent token): delete the stale refs of the agent's own fork.
    /// `as` is an agent, never a person.
    pub(super) async fn api_cleanup(&self, caller: &super::agents::Caller, body: serde_json::Value) -> Result<Response> {
        let agent = match self.acting_agent(caller, "as", body["as"].as_str().unwrap_or_default()).await? {
            Ok(a) => a,
            Err((status, e)) => return bad(status, e),
        };
        let cfg = self.cfg().await?;
        match self.cleanup(&cfg, &agent).await? {
            Ok(n) => Response::from_json(&serde_json::json!({ "deleted": n })),
            Err(e) => bad(502, e),
        }
    }
}

/// `refs/heads/land/<change>-<rev>`: the change and its revision.
fn land_ref(name: &str) -> Option<(String, u32)> {
    name.strip_prefix("refs/heads/land/")
        .and_then(|rest| rest.rsplit_once('-'))
        .filter(|(cid, _)| super::valid_change_id(cid))
        .and_then(|(cid, rev)| rev.parse::<u32>().ok().map(|r| (cid.to_string(), r)))
}

/// Exact branch name pushed with plain Git. Main and context branches are never PRs.
fn plain_branch(main: &str, name: &str) -> Option<String> {
    let branch = name.strip_prefix("refs/heads/")?;
    if name == main || name == "refs/heads/main" || name == CONTEXT_BRANCH {
        return None;
    }
    let cid = change_id_of(branch);
    super::valid_change_id(&cid).then_some(branch.to_string())
}

/// A ref that carries revision `r` is stale when it landed; when its PR closed (its author or
/// an admin closed it, or it was a competing attempt that wasn't picked); or when it was
/// ejected and a later revision replaced it. `latest`: the PR's newest revision.
fn stale(r: &ChangeRec, latest: u32) -> bool {
    let closed = matches!(r.reason.as_ref().and_then(|v| v["kind"].as_str()), Some("closed" | "not_picked"));
    match r.state {
        Phase::Landed => true,
        Phase::Ejected => closed || latest > r.req.rev,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(state: &str, rev: u32, kind: Option<&str>) -> ChangeRec {
        let zero = "0".repeat(40);
        serde_json::from_value(serde_json::json!({
            "req": { "change_id": "feature-x", "rev": rev, "fork": "t--a00", "commit": zero, "base": zero },
            "state": state,
            "reason": kind.map(|k| serde_json::json!({ "kind": k })),
            "submitted_ms": 0,
        }))
        .unwrap()
    }

    #[test]
    fn land_refs() {
        assert_eq!(land_ref("refs/heads/land/e2-3"), Some(("e2".into(), 3)));
        assert_eq!(land_ref("refs/heads/land/fix-the-cart-12"), Some(("fix-the-cart".into(), 12)));
        assert_eq!(land_ref("refs/heads/land/e2"), None);
        assert_eq!(land_ref("refs/heads/land/e2-x"), None);
        assert_eq!(land_ref("refs/heads/e2-3"), None);
    }

    #[test]
    fn plain_branches() {
        let main = "refs/heads/main";
        assert_eq!(plain_branch(main, "refs/heads/feature/x").as_deref(), Some("feature/x"));
        assert_eq!(plain_branch(main, "refs/heads/t-7/hello").as_deref(), Some("t-7/hello"));
        // A land ref that does not parse is a branch like any other.
        assert_eq!(plain_branch(main, "refs/heads/land/e2").as_deref(), Some("land/e2"));
        assert_eq!(plain_branch(main, "refs/heads/main"), None);
        assert_eq!(plain_branch("refs/heads/trunk", "refs/heads/trunk"), None);
        assert_eq!(plain_branch("refs/heads/trunk", "refs/heads/main"), None);
        assert_eq!(plain_branch(main, CONTEXT_BRANCH), None);
        assert_eq!(plain_branch(main, "HEAD"), None);
        assert_eq!(plain_branch(main, "refs/tags/v1.0.0"), None);
        assert_eq!(plain_branch(main, "refs/heads/has space"), None);
        assert_eq!(plain_branch(main, &format!("refs/heads/{}", "a".repeat(65))), None);
    }

    #[test]
    fn stale_rule() {
        assert!(stale(&rec("landed", 1, None), 1));
        // A landed revision is stale even when a later revision is open.
        assert!(stale(&rec("landed", 1, None), 2));
        // Ejected (a conflict, CI): its author fixes it and sends the next revision.
        assert!(!stale(&rec("ejected", 1, Some("check_failed")), 1));
        assert!(stale(&rec("ejected", 1, Some("check_failed")), 2));
        assert!(stale(&rec("ejected", 2, Some("closed")), 2));
        assert!(stale(&rec("ejected", 1, Some("not_picked")), 1));
        for open in ["queued", "awaiting_seal", "awaiting_allowance", "awaiting_pick"] {
            assert!(!stale(&rec(open, 1, None), 1), "{open}");
            assert!(!stale(&rec(open, 1, None), 2), "{open}");
        }
    }
}
