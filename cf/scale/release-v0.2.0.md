nerudo v0.2.0: the work of 2026-10-11 and 2026-10-12, landed on nerudo/nerudo through its own merge queue.
25 PRs from 19 agents in 25 batches. Each PR from #50 on came from a plain git push by the agent of its workstream. Each of these PRs names its task, and its merge closed the task.

## Users and agents

- A person has one name and one sign-in key across orgs, teams and repos. A repo can list approvers who did not join yet. A PR that waits for a person can be closed. Old names show as the new names (#50 · T-11 · users)
- A person can approve a parked PR from the coding agent (MCP request_approval), with the same seal as nerudo seal (#51 · T-12 · approve)
- A PR closed for a conflict names the landing that beat it, with its plan and the lines in conflict. Compare shows each plan (#52 · T-13 · beaten)
- Approvals made under older code verify again. The rules digest is canonical, and the older forms of the digest still count (#53 · T-14 · digest)
- Each agent gets a key with its first fork. The shared agent token acts for no agent that has a fork, and never for a person (#54 · T-15 · keys)

## Reliability

- Fault tests in Nix: CI down, CI hang, flaky git, kill -9 and growth. A network error in the gate keeps the change queued (fetch_retry). verify reads the whole landing log (#55 · T-16 · faults)
- Calls to CI and git have deadlines. The interval before the next try doubles, with jitter. People can read the stall. After a lost reply, the sequencer sends the same push again. An overload gives HTTP 503 with Retry-After (#61 · T-21 · resil)
- A fetch that returns no packfile is transient. The change stays queued (#67 · T-26 · freeze)

## Scale

- Many-repo test: 200 trunks land 200 changes/s. The rate is linear in the number of trunks, and the p50 latency is that of one trunk (#56 · T-19 · repos)
- Reads by name use a memo of repo names in each isolate. Thus the Hub limit of approximately 600 req/s no longer limits traffic by name (#57 · T-18 · hubmemo)
- The gate fetches the queued commits of each fork in one request, 6 forks at the same time. A burst of 512 changes drains in 12 to 24 s, not 167 s. Each round records its stage timings (GET .../timings) (#58 · T-17 · stages)
- A memo in each isolate and the colo cache keep the Hub records. Thus the repo page and reads by name almost never reach the Hub (#62 · T-22 · hubnext)
- The Trunk object disposes the results of Artifacts binding calls. Before, a Trunk object stayed billed for 11 minutes and more. The test of 10,000 trunks landed 500 changes/s (#63 · T-23 · tenk)
- The sequencer finds the culprit of a failed type check with a search that halves the batch. With 5% bad changes, the CI runs went from 290 to 175 (cap 16) and from 530 to 209 (cap 64) (#66 · T-25 · ciq)

## Large repos

- A trees-only trunk store: the sequencer reads the trees of trunk, not its history or file contents. A tip of 728 MB with 70,026 files lands with 14 MB (#59 · T-20 · mono)
- A landing on a small repo needs one fetch again (tiny 0.70 s, as before the trees-only store) (#60 · T-20 · mono)
- The sequencer reads a big trunk one object at a time, with lazy trees through the Artifacts binding. The web UI, release notes, plain git push and import work on it (#70 · T-27 · mono)

## Operations

- Metrics points for each round (they write nothing with no binding), SLOs and an alert Worker. A small request body goes to the Trunk object whole (#64 · T-24 · ops)
- A request body that goes to the Trunk object whole keeps redirect manual. Before this fix, web form posts got the target page (#68 · T-24 · ops)

## Docs

- The freeze build: what it contains, how the merges resolved the conflicts, and its fault report (100 passed) (#69 · T-28 · freeze)

## Also in this release

- cf/README: a branch that tracks origin/main can push to main; nerudo refuses it (#45 · claude)
- cf: the Graph draws each PR's branch from the main commit it started at (#46 · T-10 · webnav)
- cf/nerudo-core: an approval rule: any change to these paths waits for a person's approval (#47 · T-10 · hub)
- cf/nerudo-cli: replay starts PRs where upstream branched, and replays open upstream PRs (#48 · T-10 · claude)
- nerudo.toml: the approver you is the user kcirtapfromspace (#49 · claude)

## Approved changes

These needed a signature from a person before they merged.

- nerudo.toml: the approver you is the user kcirtapfromspace (#49) approved by you

## Not in this release

- The fault harness runs in a GitHub workflow (#65): it waits for a maintainer's approval.

## Contributors

- Agents: approve, beaten, ciq, claude, digest, faults, freeze, hub, hubmemo, hubnext, keys, mono, ops, repos, resil, stages, tenk, users, webnav
- Tasks closed: T-10, T-11, T-12, T-13, T-14, T-15, T-16, T-17, T-18, T-19, T-20, T-21, T-22, T-23, T-24, T-25, T-26, T-27, T-28

**Full changelog**: v0.1.0...v0.2.0 · 137 files changed, +86376 −894

Tasks: https://nerudo-web.patrickdeutsch.workers.dev/nerudo/nerudo/tasks · Pull requests: https://nerudo-web.patrickdeutsch.workers.dev/nerudo/nerudo/pulls
