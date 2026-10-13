# nerudo on nerudo: the work of 2026-10-12 as tasks, PRs and a release

Nerudo (formerly nerudo) keeps its own code in the live repo `nerudo/nerudo`
(<https://nerudo-web.patrickdeutsch.workers.dev/nerudo/nerudo>). On 2026-10-12 that repo
stopped at the work of 2026-10-11. This run put the work of 2026-10-11 (evening) and
2026-10-12 on it with nerudo's own workflow:

1. Each workstream is a task on the board. Its agent writes the task and claims it.
2. The agent sends its PRs with plain `git push`. Each PR names its task.
3. The sequencer merges the PRs. Each merge moves its task to Done.
4. An agent proposes the release. A person signs it.

The script is `dogfood.py`. It is on branch `dogfood`, not in this tree. The data of the run
is in [`../evidence/dogfood-20261012.json`](../evidence/dogfood-20261012.json). The states
below (for example "waits for a signature") are the states at the end of the run.

## Summary

- 18 tasks for the work (T-11 to T-28), and 11 tasks for the work that remains (T-29 to
  T-39). Each work task went Backlog, In progress, Merging, Done.
- 21 PRs (#50 to #70) from 16 agents. 20 PRs merged, in landings 50 to 69. One PR (#65)
  waits for a maintainer's approval, because it edits `.github/`.
- `main` is now `ced7f71`. Its tree is the tree of our `7958202` (the freeze build), with two
  differences: `.github/workflows/faults.yml` is in #65, and `nerudo.toml` is only on
  `nerudo/nerudo`.
- The agent `dogfood` proposed the release v0.2.0 (landing 69, 25 PRs). It waits for the
  signature of `kcirtapfromspace`.
- The run used the agent token and agent keys only. It did not use the admin token, and it
  did not seal or sign anything.
- Time: 20:20 to 20:43 MDT for the survey, the tasks and the PRs. The pushes took 2 minutes.

## What was on nerudo/nerudo before

- `main` was `c312484` (landing 49). Its tree was our `fa07043` and one more file,
  `nerudo.toml`. `fa07043` is the last commit that the run of 2026-10-11 pushed.
- `nerudo.toml` has `[seals] governance = [".github/**", "cf/nerudo-ci/**",
  "nerudo.toml"]`: a PR that edits these paths waits for one `maintainer`. It has no `[ci]`.
- The tasks were T-1 to T-10. T-5 to T-9 were in Backlog. There were no open PRs.
- The release v0.1.0 was at landing 44. The draft was v0.1.1 (PRs #45 to #49).

In our history from `fa07043` to `7958202`, only one file is in a governance path:
`.github/workflows/faults.yml` (the ops workstream). Thus that file is a PR of its own, and
the other PRs do not wait for a person.

## Method

`dogfood.py` builds each PR from our committed history, never from a working tree:

1. A step is a range of our first-parent history (`from..to`) and an agent.
2. The PR's commit is `origin/main`, with each path that changed in the range set to its
   content at `to`. Before it changes a path, the script checks that `main` has our content
   at `from` for that path. If not, it stops (drift).
3. A wave is a run of steps that change different files. The script pushes the PRs of a
   wave together. Then it follows each PR until it merges, closes or waits for a person.
   Then it starts the next wave on the new `main`. There are 14 waves.
4. The message of each PR is a summary of its range, with a conventional subject (`feat`,
   `fix`, `perf`, `test`, `docs`, `ci`). Thus the release draft has sections and a version.
   The last paragraph is `Task: T-n`.
5. `dry` builds each step on the step before it, with no push, and compares the end with
   `7958202`. Before the run, the only difference was `nerudo.toml`.

The agents:

- `nerudo mcp` runs as each agent and calls `new_task` and `claim`. Its first call makes
  the agent's fork, and it keeps the agent key in `~/.nerudo/agents/<trunk>/<agent>`.
- `git push origin HEAD:refs/heads/<branch>` sends the PR. The script first clears the
  credential helpers of the global git config. Then `nerudo git-credential` gives git the
  agent key from the key file. The push names the branch explicitly, because the global
  `push.default` is `tracking`.
- The agent names are new: `users`, `approve`, `beaten`, `digest`, `keys`, `faults`,
  `repos`, `hubmemo`, `stages`, `mono`, `resil`, `hubnext`, `tenk`, `ops`, `ciq`, `freeze`,
  and `dogfood` for the backlog and the release. The agents `hub` and `claude` already had
  forks from before agent keys and no key file on this computer. Their keys need the admin
  (`nerudo agent-key`), so the run did not use them.

## Tasks

| Task | Agent | Title | Status |
|---|---|---|---|
| T-11 | users | Users: one name and one sign-in key across orgs, teams and repos | Done (#50) |
| T-12 | approve | Approve a parked PR from the coding agent | Done (#51) |
| T-13 | beaten | A closed PR names the PR that beat it, with its plan | Done (#52) |
| T-14 | digest | Approvals made under older code must verify again | Done (#53) |
| T-15 | keys | A key for each agent; the shared agent token acts for no agent that has a fork | Done (#54) |
| T-16 | faults | Fault tests in Nix; a network error in the gate keeps the change queued; verify reads the whole log | Done (#55) |
| T-17 | stages | Gate prefetch and stage timings | Done (#58) |
| T-18 | hubmemo | Hub name memo | Done (#57) |
| T-19 | repos | Many-repo test | Done (#56) |
| T-20 | mono | Large repos, step 1: a trees-only trunk store | Done (#59, #60) |
| T-21 | resil | Resilience: deadlines, backoff, a visible stall, push again, 503 on overload | Done (#61) |
| T-22 | hubnext | Hub next: a two-layer cache of hub records, cached repo views | Done (#62) |
| T-23 | tenk | Rpc disposal and the 10,000-repo test | Done (#63) |
| T-24 | ops | Ops: metrics, SLOs, the alert Worker, the CI workflow, the Trunk body fix | Done (#64, #68); #65 waits |
| T-25 | ciq | CI bisection: find the culprit of a failed type check by halving the batch | Done (#66) |
| T-26 | freeze | A fetch with no packfile is transient | Done (#67) |
| T-27 | mono | Large repos, steps 2 and 3: lazy trees through the binding, the UI, release notes, git push, import | Done (#70) |
| T-28 | freeze | The freeze build: merge the day's branches, resolve the conflicts, test | Done (#69) |

The backlog, written by `dogfood`. Each task has one paragraph that says what and why.

| Task | Title |
|---|---|
| T-29 | Reads off the Trunk object: the View object (branch reads-split) |
| T-30 | CI queue: apply again after the culprit, and check ahead (ci-queue changes 2 and 3) |
| T-31 | Split the Hub by org; listing pages that scan the Hub; floods of random names |
| T-32 | Bind the metrics dataset and deploy the alert Worker |
| T-33 | Deadlines for host.rs REST calls, decide and the plain-git proxy; jitter for fetch_retry |
| T-34 | A land request can return a bare HTTP 500 |
| T-35 | A red main with a batch [fix, bad] waits forever |
| T-36 | An alarm that fires while the landing loop runs sets no new alarm |
| T-37 | A cold Trunk object fetches all of main (10,000 trunks); check on the large-repo build |
| T-38 | A CI type check over 25 s never lands (tune TIMEOUT_MS) |
| T-39 | A task can close while another of its PRs waits for a person (found by this run) |

T-7, T-8 and T-9 (from 2026-10-11) are In progress with the agents `branches` and
`revisions` of two other sessions. They push their PRs on the new `main`.

## PRs

| PR | Branch | Agent | Task | State | Our range |
|---|---|---|---|---|---|
| #50 | users/one-name-one-key | users | T-11 | merged, landing 50 | fa07043..46e1d5d |
| #51 | approve/from-the-agent | approve | T-12 | merged, landing 51 | 46e1d5d..2414fce |
| #52 | beaten/what-beat-it | beaten | T-13 | merged, landing 52 | 2414fce..68576fd |
| #53 | digest/canonical-rules-digest | digest | T-14 | merged, landing 53 | 68576fd..da22414 |
| #54 | keys/a-key-for-each-agent | keys | T-15 | merged, landing 54 | da22414..9924c36 |
| #55 | faults/nix-fault-tests | faults | T-16 | merged, landing 55 | 9924c36..3b28852 |
| #56 | repos/many-trunks | repos | T-19 | merged, landing 56 | 3b28852..0e4ec24 |
| #57 | hubmemo/name-memo | hubmemo | T-18 | merged, landing 57 | 0e4ec24..f1dde0a |
| #58 | stages/prefetch-and-timings | stages | T-17 | merged, landing 58 | f1dde0a..128b641 |
| #59 | mono/trees-only-store | mono | T-20 | merged, landing 59 | 128b641..d3f6e71 |
| #60 | mono/one-fetch-small-repo | mono | T-20 | merged, landing 60 | d3f6e71..58991e2 |
| #61 | resil/deadlines-and-stalls | resil | T-21 | merged, landing 61 | 58991e2..88f40f8 |
| #62 | hubnext/record-cache | hubnext | T-22 | merged, landing 62 | 88f40f8..dda8646 |
| #63 | tenk/rpc-disposal | tenk | T-23 | merged, landing 63 | dda8646..ae0bc0e |
| #64 | ops/metrics-and-slos | ops | T-24 | merged, landing 64 | ae0bc0e..414113f, not `.github/` |
| #65 | ops/faults-workflow | ops | T-24 | **waits for a maintainer's approval** (governance: `.github/workflows/faults.yml`) | ae0bc0e..414113f, only `.github/` |
| #66 | ciq/halving | ciq | T-25 | merged, landing 65 | 414113f..0ad9050 |
| #67 | freeze/no-packfile-transient | freeze | T-26 | merged, landing 66 | 0ad9050..0730147 |
| #68 | ops/body-redirect-manual | ops | T-24 | merged, landing 67 | 0730147..73ad913 |
| #69 | freeze/build-report | freeze | T-28 | merged, landing 68 | 73ad913..bc7fb75 |
| #70 | mono/lazy-trees | mono | T-27 | merged, landing 69 | bc7fb75..7958202 |

No PR was closed. Each PR merged in a batch of its own. The PRs of one wave edit different
files. But `nerudo.toml` makes one footprint unit of each directory under `cf/`, and each
PR edits `cf/nerudo-worker`, `cf/scale` or `cf/evidence`. Changes to the same unit do not
share a batch.

## The release

The draft at landing 69 is v0.2.0: `feat` subjects give a minor version before 1.0. It has
25 PRs (#45 to #70, without #65), 19 agents, and T-10 to T-28 closed. The draft notes of the
sequencer list each PR under its type.

The notes in [`release-v0.2.0.md`](release-v0.2.0.md) group the PRs by workstream (users and
agents, reliability, scale, large repos, operations, docs). Each line names its PR, its task
and its agent. They also list #45 to #49 and say that #65 is not in the release.

The agent `dogfood` proposed v0.2.0 with these notes (`release_propose` in `nerudo mcp`).
The proposal is in the inbox of `kcirtapfromspace`. The sequencer keeps the notes of a
proposal, but a signature covers the notes that the signer gives. Thus, to sign these notes
for landing 69, a maintainer runs this in `cf/`:

```sh
nerudo release --trunk nerudo/nerudo --at 69 --notes scale/release-v0.2.0.md
```

`nerudo release` with no `--notes` signs the draft notes of the sequencer, and the Releases
tab of the web app shows those notes and that command. Without `--at`, the release also
includes each PR that merges before the signature.

## What needs a person

- **#65** (`.github/workflows/faults.yml`) waits for one maintainer's approval:
  `nerudo seal --trunk nerudo/nerudo --change ops-faults-workflow`, or the inbox in the
  web app. Until then, `main` does not have the workflow file.
- **v0.2.0** waits for the signature of a maintainer (see above).

## Findings

- **A task closed while one of its PRs still waited for a person** (T-39). The agent pushed
  #64 and #65 of T-24 together. #64 merged 1.7 s before #65 parked, and the sequencer moved
  T-24 to Done. A PR for a Done task does not move it back, so T-24 shows Done while #65
  waits.
- **One `git fetch` of `main` got HTTP 500** ("expected flush after ref listing"),
  immediately after landing 61. The same fetch worked 2 s later, three times. The script
  now tries the fetch again. The cause is not known. T-34 is about a similar bare 500 on
  `land`.

## Run it again

Run these commands on branch `dogfood`, which has the script. The script reads the
environment of `cf/nerudo-worker/.prod.vars` and prints no token.
It skips the tasks that exist (by title) and the PRs that merged or wait. `NERUDO_CLI` is
the path of the `nerudo` binary (`cargo build -p nerudo-cli`); the default is
`cf/target/debug/nerudo`.

```sh
set -a; . cf/nerudo-worker/.prod.vars; set +a
export DOGFOOD_DIR=<a scratch directory> NERUDO_CLI=<path of the nerudo binary>
python3 -I cf/scale/dogfood.py survey    # read only
python3 -I cf/scale/dogfood.py plan      # the waves
python3 -I cf/scale/dogfood.py dry       # build each PR locally; compare with 7958202
python3 -I cf/scale/dogfood.py tasks
python3 -I cf/scale/dogfood.py push      # all waves, or: push 3 4
python3 -I cf/scale/dogfood.py verify    # main against 7958202
python3 -I cf/scale/dogfood.py status --json cf/evidence/dogfood-20261012.json
python3 -I cf/scale/dogfood.py notes     # cf/scale/release-<tag>.md from the draft
python3 -I cf/scale/dogfood.py propose   # as agent dogfood; nobody signs
```

## Limits

- The PRs are summaries of our history: one squashed commit for each range. The commit
  ids of the ranges are in each PR's message and in the table above.
- The tasks of the work were written after the work. Their bodies say what and why, and
  link to the documents of each workstream.
- The release proposal does not change the draft that the web app shows. Only
  `nerudo release --notes` signs the notes of the proposal.
