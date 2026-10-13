# Backlog decisions

Each section records the rule that an agent chose for a task on nerudo/nerudo, before the
code. One section for each agent.

## T-8 and T-9: the revisions of a pull request (agent `revisions`)

### The problem

- T-8. A plain `git push` to the branch of a merged pull request (PR) made revision N+1 of
  the same PR, and that revision could merge. The MCP `land` tool refused a merged change id.
  `nerudo verify` counts the `Change-Id:` trailers on main, and it fails when one change id
  is on main two times.
- T-9. A push of revision N+1 while revision N waited in the queue kept both in the queue.
  The base of N+1 is main, not N, so N+1 also contains the edits of N. When N merged first,
  N+1 applied the same edits again: the result was a conflict, or a second landing of the same
  change id.
- `cf/STRESS_TESTING.md` found a third case: an older revision that arrived after a newer one
  moved `latest:` back, and both revisions landed in one batch.

### The rules

1. **A change id merges one time.** A merged change id is terminal: it takes no new revision.
   `POST land` refuses a new revision of a merged change id (409). The MCP `land` tool refuses
   it before it pushes, as before.
2. **A push to a merged branch opens a new PR.** A plain `git push` to the branch of a merged
   PR opens a new PR with a new number. Its change id is `<branch id>..<n>`, with n = 2, 3 and
   so on. Git does not permit `..` in a branch name, so no other branch can have that change
   id. The `remote:` lines say that the old PR is merged, and they give the number of the new
   PR. The merged commit of the old PR can be the base of the new PR, as it was for a revision.
   The MCP `land` tool pushes `land/<change id>-<rev>` refs, so it cannot send a change id
   with `..`: it tells the agent to push the branch with git.
3. **Revisions only go forward.** `POST land` refuses a new revision that is lower than the
   latest revision (409). A resend of a revision that exists returns its receipt, as before.
   A resend with a different commit gets 409.
4. **A new revision replaces an open older revision.** At the start of its next round, the
   landing loop closes each queued or parked revision that has a newer revision. The closed
   revision has the state `ejected` and the reason `{"kind": "superseded", "rev": <newer>}`.
   It never merges. It is not a closed PR: the PR stays open with the newer revision. An
   approval is for one commit, so the approvals of a parked revision do not move to the new
   revision. The MCP `land` tool sends a new commit for a waiting change as the next
   revision, the same as `git push`.
   The landing loop is the only writer of these records, so a record that the loop holds in
   memory is never out of date. Thus `land` does not write the older record. Until the next
   round (one alarm, usually less than one second), the receipt of the older revision still
   says `queued` or parked. The replacement is atomic in the property that matters: after
   `land` accepts revision N+1, revision N cannot merge (rule 5).
5. **A revision in a batch is fenced.** The fence starts when the landing loop picks a batch,
   and it stops when the loop records the batch. In that time, `POST land` refuses a new
   revision of a PR in the batch with 409 and `{"kind": "merging"}`. Before the loop applies
   the batch, it removes each revision whose PR got a newer revision after the round started.
   The next round closes that revision. If the Worker stops after the push, the intent record
   (`attempt`) stays, and the fence holds until the next run reconciles it. A plain
   `git push` waits for the batch (at most 30 s), and then applies rule 2 or rule 4.
   `GET queue` shows `"merging": true` for a revision in the batch.
6. **Receipts and verify agree.** The receipt of a replaced revision names the revision that
   replaced it, and the MCP `receipt` tool says REPLACED. `nerudo verify` keeps its check
   that each change id is on main one time. Rules 1 to 5 make that true for each new landing.

### Why not the other rule for T-8

The other rule is: a push to a merged branch is the next revision of the same PR. Then one
change id can land more than one time. `nerudo verify`, the receipts (one `seq` for each
change id) and the task links would all need a second meaning. Git hosts use rule 2: a merged
PR takes no new commits, and new work on the branch goes in a new PR.

### The change

| Where | What |
|---|---|
| `nerudo-worker/src/trunk/revisions.rs` (new) | The rules: `refusal` (merged, merging, older), `supersede` (the start of each round), `fence` (between the pick of a batch and its apply), `wait_merged` (for git), `successor` (`<id>..<n>`). Unit tests for the pure parts. |
| `nerudo-worker/src/trunk.rs` | `land` refuses with 409 and a `kind`, before it keeps the context and again just before the write. The same revision with another commit gets `taken`. `schedule_with` calls `supersede` and `fence`. `GET queue` shows `merging`. `Mem.merging` holds the fence. |
| `nerudo-worker/src/trunk/git.rs` | `branch_pr` finds the PR of a branch (the first PR, or `<id>..<n>` after merges). `push_pr` waits for a batch that has the PR, opens a new PR after a merge, and says which revision a new revision replaces. The base can be a commit that an earlier PR of the branch merged. |
| `nerudo-worker/src/trunk/branches.rs` | The branches view (T-7) finds the revision of a plain branch in the later PRs of the branch too, so a branch whose `..2` PR merged is stale. |
| `nerudo-worker/src/types.rs` | `Stats.superseded`. It is not in the count of closed PRs. |
| `nerudo-worker/src/trunk/prinfo.rs` | The check row of a replaced revision. |
| `nerudo-cli/src/mcp.rs` | `land`: a new commit for a waiting change is the next revision; a change id with `..` is refused; a 409 shows its words. `receipt`: REPLACED. |
| `scripts/scenarios.py` | New scenario `revisions`. The `git` scenario: a push to the merged branch opens PR `<id>..2`. |

In the recorded build, a change was one commit on `main` (`bad_base` if the
parent of the commit is not its base). A second commit on top of a waiting revision gives a
revision that is ejected as `bad_base`. Push the commit made again (`git commit --amend`).

The submission candidate removes that restriction: it proves that the claimed base is
an ancestor of the original tip, then lands the complete tip-versus-base delta. The walk
is bounded to 2,000 distinct commits; approvals still cover the original tip. This is a
current source change in `nerudo-core/src/merge.rs`, separate from the historical tests
below. Its deployment and fresh plain-Git scenarios need their own verification.

### Tests (2026-10-12)

| Test | Result |
|---|---|
| `cargo test` for nerudo-core, nerudo-git, nerudo-cli, nerudo-worker | Pass. 4 new unit tests in `trunk/revisions.rs`. |
| `cargo check -p nerudo-worker --target wasm32-unknown-unknown` | Pass, no warnings. |
| `nix run .#faults` (all cases) | 100 passed, 0 known gaps, 0 failed: on 7958202 with the change, and again on the PR commit on main 493ca6a (`cf/evidence/faults-t8-t9-20261012.json`; it names 254bc62, the same code as the PR before this file and this table changed). |
| Scenarios on a throwaway Worker with Artifacts | On 7958202 with the change: `revisions`, `identity`, `git`, `beaten`, `compare`, `approvals`, `context` and `quorum` pass. On the PR commit (main 493ca6a, with T-7): `revisions`, `git`, `branches`, `identity`, `beaten`, `compare` and `approvals` pass. |

The `revisions` scenario shows each rule on Artifacts:

- On a paused trunk, revision 3 replaced revision 1 (`superseded`, rev 3). Revision 2 after
  revision 3 got 409 `older`. Revision 3 with another commit got 409 `taken`. A second
  `git push` (the commit made again) replaced revision 1 of branch `b1`.
- A new revision replaced a revision that waited for an approval. The MCP receipt of the old
  revision said REPLACED.
- A revision after the merge got 409 `merged`.
- While revision 1 of `f1` was in the batch (`GET queue`: `merging`), revision 2 got 409
  `merging`. After the merge, it got 409 `merged`.
- A `git push` while the PR of `b2` was in the batch waited for the batch, then opened PR
  `b2..2`. Its base was the merged commit, so it changed only the 2 new files.
- `nerudo verify`: each change id is on main one time. `stats.superseded` was 3, and the
  count of closed PRs was 0.
