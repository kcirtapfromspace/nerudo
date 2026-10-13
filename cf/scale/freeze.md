# The freeze build

This document tells what the branch `freeze` contains, the conflicts of its merges, and the
tests of the build. The base is the live code: `58991e2` (scale-safe, mono-repos and step 1
of mono-next). The live Worker also runs an uncommitted patch (`hub.rs`, `trunk/app.rs`).
That patch applies to `freeze` with no change.

The build has two levels. `bc7fb75` is the build without steps 2 and 3 of mono-next. Keep it
as the fallback. `7958202` merges mono-next (steps 2 and 3) on top of it. See
[Steps 2 and 3 of mono-next](#steps-2-and-3-of-mono-next).

All runs are from 2026-10-12. The data is in
[`../evidence/freeze-20261012.json`](../evidence/freeze-20261012.json).

## Summary

- `7958202` adds steps 2 and 3 of mono-next. Its tests: 116 unit tests, the harness 100 of
  100, the 17 scenarios 17 of 17, and the big-repo checks of mono-repos pass. On one trunk,
  a small-repo landing takes the same time as on the live build (0.582 s against 0.578 s).
  The points below are for `bc7fb75`.
- 6 parts and one fix found by the tests. Each is a signed merge or commit.
- `cargo test` (the workspace, 111 tests), the wasm check and clippy pass. Clippy gives 9
  warnings, and all of them are in code from before this build.
- The fault harness (`nix run .#faults`): 100 passes, 0 known gaps, 0 failures, in two runs.
- The 17 scenarios on Artifacts (namespace `nerudo-freeze`): all passed on the final
  candidate. On the first candidate, `social` and `branches` failed. The cause was in the
  body fix of ops, and commit `73ad913` corrects it.
- A small repo is not slower. One change on the tiny repo: median 0.65 s on the candidate
  and 0.76 s on the live build. Each build landed 12 changes, on the same Worker in the same
  7 minutes.
  A burst of 128 changes: median drain 3.80 s on the candidate and 4.67 s on the live build,
  3 bursts each. The spread between runs is larger than the difference.

## Contents

| Step | Commit | What |
|---|---|---|
| 1 | `7f7b860` | Merge `resilience` (598c2cd): deadlines for CI and git, the stall and its interval, the push again, 503 on overload (`resilience.md`) |
| 1 | `88f40f8` | The push again sends `store.new_objects(tip, start)`, as the first push does |
| 2 | `dda8646` | Merge `hub-next` (882b278): the memo of hub records in the isolate and the colo cache |
| 3 | `ae0bc0e` | Merge `scale-10k` (b8a4f2d): `host.rs` disposes the results of Artifacts binding calls (34d2439); the 10,000-trunk tools and docs |
| 4 | `de2241c` | Merge `ops` (fbe9c58): metric points, the alert Worker (not deployed), the CI workflow, the body fix of `to_trunk` |
| 4 | `414113f` | Revert `dba1e25`: no `METRICS` binding, so the metrics write nothing |
| 5 | `0ad9050` | ci-queue change 1 only (10c3a6e): find the culprit of a failed type check by halving the batch (`ci.md`) |
| 6 | `0730147` | A fetch with no packfile is transient: the gate keeps the change queued |
| Fix | `73ad913` | `to_trunk` keeps redirect manual (see below) |
| 7 | `7958202` | Merge `mono-next` (4d70529): steps 2 and 3. A big trunk reads its trees one at a time through the Artifacts binding and keeps them in storage. The web UI, release notes, plain git push and import work on the partial store (`mono.md`) |

Not in this build: the docs commits of ops after fbe9c58 (`c24112b`, `bce5b5f`), changes 2
and 3 of ci-queue, and the `cf/README.md` edits of ci-queue.

## Conflicts

| Merge | File | Resolution |
|---|---|---|
| hub-next | `lib.rs` | The file that hub-next made (`lib.rs.merged-with-resilience`). The `lib.rs` of `58991e2` is the same as that of scale-safe, so the file applies as it is. It has the overload mapping of resilience around `route`, and the colo cache put of hub-next after the response. |
| ops | `ci.rs` | The deadline and the `ci_unavailable` errors of resilience. Each error now also names the CI Worker ("no answer from the CI Worker in 25 s", "CI Worker unreachable: ..."). It does this because `trunk/metrics.rs` gives the cause `ci` to an error that contains "CI Worker". New tests in `metrics.rs` check these texts. |
| ops | `trunk.rs`, `alarm()` | The stall of resilience (record, interval, return Ok), and the metric of ops for each failed run |
| ops | `trunk.rs`, the type check | `typecheck_files` of mono-repos, then `tm.ci` of ops |
| ci-queue | `trunk.rs` | The patch applied with a 3-way merge and no conflict |

## The fix found by the scenarios

The body fix of ops (`da946c6`) sends a small request body to the Trunk object whole: it
makes a new `Request`. A new `Request` follows redirects by default. The stub thus followed
the 303 that a web form post gets on `/t/{trunk}/...` (`act/comment`, `act/cleanup`). The
client got the page at the target (HTTP 200), or HTTP 403 there with no key. The scenarios
`social` and `branches` failed on the first candidate this way. The fault harness did not
find it: its parity scenarios do not post web forms.

Commit `73ad913` sets redirect manual on the new `Request`, as on the request that the Worker received. On
the second candidate, `social` and `branches` passed, and then all 17 scenarios passed.

## Steps 2 and 3 of mono-next

Commit `7958202` merges `mono-next` at `4d70529` on top of `bc7fb75`, with no rewrite.

### Conflicts of this merge

| File | Resolution |
|---|---|
| `trunk.rs`, the module list | `metrics` (ops) and `objects` (mono-next): both |
| `trunk.rs`, the type check | `self.typecheck_files(&mut rd, ...)` of mono-next before each `ci::typecheck`, also in the search of ci-queue that halves the batch, and `tm.ci` of ops. Without it, a lazy trunk does not have the TypeScript files, and the check fails with "missing object". |
| `types.rs`, `Stats` | `pushes_again` (resilience), `object_reads` and `objects_from_storage` (mono-next): all |
| `host.rs` (no text conflict, but it did not compile) | Since scale-10k, `call()` returns `Rpc`, which disposes the result. `RepoReader` keeps its repo stub as an `Rpc`. `tree()` clones the value before the cast to an array, as the notes of the mono-repos agent say. |

### Tests of this merge

| Test | Result |
|---|---|
| `cargo test --workspace` | 116 tests pass (also the tests of `objects`, `views`, `nerudo-git` trees and `nerudo-core` partial) |
| The wasm check and clippy | No error; the same 9 warnings |
| `nix run .#faults` at `7958202` | 100 passes, 0 known gaps, 0 failures ([`../evidence/faults-20261012-freeze-mono.json`](../evidence/faults-20261012-freeze-mono.json)). A first run was stopped by hand. |
| The 17 scenarios on Artifacts, candidate 3 (`7958202` + the live patch, version `edbab265`) | 17 of 17 passed. A first run was stopped by hand after 6 passes. |
| The live patch | `git apply --check` passes on `7958202` |
| Big repos (the mono-repos agent, `nerudo-scale-mono`, version `253cbbf7`; evidence on mono-next `5e26896`) | m700 (728 MB) lands with 13.6 MB of memory: one change 5.8 s, a batch of 3 in 11.6 s. Read-back and `git fsck` pass, and the app reads give 200. A plain git push opened PR #5, and it landed. d120k (lazy) lands with 1.9 MB: one change 10.6 s, a batch of 2 in 15.9 s. A plain git push opened PR #4, and it landed. The import of cloudflare/agents took 13.0 s. Small repos: tiny 0.61 s and m25 1.15 s (live 0.70 s and 1.49 s). |

### Speed of this merge against the live build

On separate trunks (a new tiny repo for each run of 6 changes), the live build was faster in
this period. The median was 0.48 s against 0.65 s, with 25 changes for each build. The first change of each trunk
is not counted.

But the two builds sent the same requests with the same bytes. The time
outside the git requests was also the same (40 ms and 38 ms). Only the times of the fetch and the
push to Artifacts were different. Thus the test ran again on one trunk.

One trunk, blocks of 6 single landings, and the deployed build changed after each block (live,
candidate, live, candidate, live, candidate). The table does not count the first landing of
each block (a new isolate):

| Block | Build | Median | Fetch (median) | Push (median) |
|---|---|---|---|---|
| 1 | Live | 0.553 s | 259 ms | 268 ms |
| 2 | Candidate 3 | 0.559 s | 277 ms | 261 ms |
| 3 | Live | 0.586 s | 273 ms | 278 ms |
| 4 | Candidate 3 | 0.582 s | 277 ms | 274 ms |
| 5 | Live | 0.636 s | 280 ms | 276 ms |
| 6 | Candidate 3 | 0.617 s | 295 ms | 285 ms |
| All | Live | 0.578 s (15 changes) | | |
| All | Candidate 3 | 0.582 s (15 changes) | | |

On the same repo, the two builds land a change in the same time. The difference on separate
trunks came from the repos at Artifacts, not from the code. The mean of candidate 3 is higher
(0.645 s against 0.592 s) because of one landing with a push of 1 s. Bursts of 128 changes:
2.70 s and 3.43 s on candidate 3, 3.70 s and 2.97 s on the live build.

### The parity default of the harness

`social` and `branches` are now in the default scenarios of `parity` (`faults/run.py`).

`social` posts web forms, which get a 303, as the forms that `da946c6` broke. It passed on
this computer. On `7958202` with `73ad913` reverted (a scratch copy), the local `social`
failed with the same error as on Artifacts: `AssertionError: (403, None)`. Thus the harness
now finds this fault.

`branches` skips itself on this computer (Gitea refuses ref deletions)
and runs on Artifacts.

## Tests of bc7fb75

### Local

| Test | Result |
|---|---|
| `cargo test --workspace` | 111 tests pass |
| `cargo check -p nerudo-worker --target wasm32-unknown-unknown` | Pass |
| `cargo clippy --workspace`, and for wasm32 | No error. 9 warnings, all in code from before this build |
| `nix run .#faults` at `0730147` | 100 passes, 0 known gaps, 0 failures |
| `nix run .#faults` at `73ad913` | 100 passes, 0 known gaps, 0 failures ([`../evidence/faults-20261012-freeze.json`](../evidence/faults-20261012-freeze.json)) |

In both harness runs:

- `ci_down` sent 5 CI requests for each trunk (budget 8).
- `ci_hang` asked again after 26 s.
- `git_hang` landed again 2.8 to 3.1 s (replies dropped) and 10.0 to 10.2 s (requests dropped)
  after the git host answered.

### On Cloudflare

The candidate is `freeze` with the live patch, applied in a scratch copy (never committed).
It ran on the throwaway Worker `nerudo-scale-resil`: Artifacts namespace `nerudo-freeze`, `CI` bound
to `nerudo-ci-scale`, no `AI` binding.

| Candidate | Version | Scenarios |
|---|---|---|
| 1: `0730147` + patch | `6a9fef3a` | 15 of 17 passed; `social` and `branches` failed (see above) |
| 2: `73ad913` + patch | `4385d273` | 17 of 17 passed: approvals, quorum, recovery, ci, ci_red, social, branches, context, compare, release, hub, git, users, aliases, ide_approval, beaten, identity |

`fencing` and `decide` did not run. Candidate 2 is deployed now as version `3c71811d` (the same
build).

### Speed against the live build

The live build (`58991e2` with the patch) and candidate 2 ran on the same Worker, in turns,
from 19:47 to 19:54 MDT.

| Test | Candidate 2 | Live build |
|---|---|---|
| One change at a time on the tiny repo (46 files, 23 KB), 2 runs of 6: median of each run | 0.547 s, 0.735 s | 0.770 s, 0.749 s |
| The same, median of the 12 changes | 0.65 s | 0.76 s |
| Burst of 128 changes, 4 forks, cap 16 (`stages.py`): drain | 5.58 s, 2.74 s, 3.80 s | 4.14 s, 4.67 s, 5.42 s |
| The same: first landing | 1.28 s, 0.71 s, 0.94 s | 0.99 s, 1.10 s, 1.39 s |

The push to Artifacts is most of each burst (2.4 to 5.1 s of the drain), and it changes from
run to run. Step 1 of mono-next gave 0.70 s for the tiny repo on another Worker.

## Limits

- The overload of a Trunk object, and the alert Worker of ops, were not tested here.
- The metric points of ops were not tested with a binding. With no binding, they write
  nothing, and the harness runs with no binding.
- The speed comparison is 2 runs of 6 changes and 3 bursts for each build: it can find a
  large slowdown, not a small one.

## Artifacts repos

All repos are in the namespace `nerudo-freeze`:

- The trunks `scn-resil-*` of the scenarios, and their forks `<trunk>--<agent>`.
- `stages-resilf0-*` and `stages-resill0-*` of the bursts, with 4 forks each.
- `mono-resil-tiny-f1`, `-f2`, `-l1` and `-l2`.

The evidence file lists the bursts and tiny repos.
