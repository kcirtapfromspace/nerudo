# NERUDO

A Git platform for people coordinating concurrent coding agents, built on Cloudflare
Workers, Durable Objects and Artifacts. Each attempt brings code, a task, preserved
context and a receipt; repository rules govern integration, approvals and releases.

This repository is the frozen Cloudflare competition snapshot. Use the
[`competition-submission-2026-10-13` release](https://github.com/kcirtapfromspace/nerudo/releases/tag/competition-submission-2026-10-13).
[FREEZE.md](FREEZE.md) records its source provenance. The application implementation is
in [`cf/`](cf/README.md#try-it).

[Live app](https://nerudo-web.patrickdeutsch.workers.dev/) ·
[Public demo](https://nerudo-web.patrickdeutsch.workers.dev/nerudo/demo) ·
[Submission fields](SUBMISSION_FIELDS.txt)

Clone the frozen version and run its offline checks:

```sh
git clone --branch competition-submission-2026-10-13 https://github.com/kcirtapfromspace/nerudo.git
cd nerudo
cargo test --manifest-path cf/Cargo.toml --workspace --locked
python3 -B -m unittest discover -s cf/scripts -p 'test_llm_*.py'
```

For deployment, follow [`cf/README.md` → Try it](cf/README.md#try-it) with your own
Workers Paid account, Artifacts namespace and secret bindings.

**[Measurements and benchmarks](BENCHMARKS.md)**: every result of this repo, with charts
and diagrams, in one page. [Competition submission packet](SUBMISSION.md) gives the project
vision, reproducibility steps, evidence bounds, and narrated demo storyboard.

This repo has a landing sequencer (a minimum viable product) and a benchmark. The
benchmark compares the sequencer with the ways that agents land changes today: plain git,
plain jj, Forgejo and GitHub.

The sequencer (`seqd/`) is the only writer of trunk. Agents push their commit to
`refs/changes/<id>/<rev>` and ask seqd to land it. seqd does these steps:

- It lands batches of changes whose footprints do not overlap.
- It runs CI one time for each batch. If CI fails, it bisects the batch.
- It merges commutative files by rule, and it makes derived files again.
- It gives allocated names, for example migration numbers, when a change lands.
- It adds each landing to a hash-chained log.

A forge can be a downstream mirror of trunk.

## The Cloudflare entry (`cf/`)

`cf/` is NERUDO, a merge queue for coding agents on Cloudflare
Workers, Durable Objects and Artifacts. For each repo, one Durable Object is the only
writer of `main`. Agents open pull requests from their own Artifacts forks, through MCP
tools in Claude Code, Codex, Antigravity or Cursor. The sequencer merges them in batches
with one CI run for each batch. The CI run includes a type check with ts-rust in a
Worker.

- People write tasks. Agents claim them, and a merge closes them.
- Several agents can attempt one task. A person compares the attempts and picks one.
- Agents record their context. The sequencer keeps an immutable copy in its own repo.
- A decision model (Cloudflare's Clef, an LLM on Workers AI, or fixed) can ask for a
  review or a signature.
- People sign approvals and releases with their own keys. `nerudo verify` checks the
  signatures from outside.
- `nerudo replay` rebuilds the recent history of a real repo with many agents.
- The web app (the Worker `nerudo-web`) shows `main` as a graph, with pull requests,
  tasks, releases and branches. It is at https://nerudo-web.patrickdeutsch.workers.dev.

`cf/README.md` tells how to run it, and what we verified and measured.
`cf/scale/README.md` gives the scale tests of 2026-10-12: up to 10,000 repos on one
account, big repos, CI and faults. The drivers use synthetic commits on real Artifacts;
[real multi-provider coding workflows](cf/LLM_WORKFLOWS.md) are recorded separately.
[BENCHMARKS.md](BENCHMARKS.md) keeps peak rates, short admission runs, CI conditions,
and historical build versions explicit.

## Layout

| Path | What |
|---|---|
| `seqd/` | Rust sequencer (axum and SQLite). It runs the git CLI. It is the local baseline for the bench. It has no authentication, it listens on 127.0.0.1 only, and it runs the CI and derived commands of `nerudo.toml` from the checkout of the change. Do not expose it. The product is `cf/`. |
| `cf/` | The Cloudflare entry: a merge queue for coding agents. See `cf/README.md`. |
| `bench/workload.py` | A synthetic repo, the specifications of the seeded changes, and the analysis of trunk correctness |
| `bench/agents.py` | The agent strategies (strict, lax, pr, jj, seq), and the Forgejo and GitHub API clients |
| `bench/run.py` | Runs one configuration or a sweep. It adds JSON lines to `results/runs.jsonl`. |
| `bench/sweep.sh`, `bench/sweep2.sh` | The measurement sweeps of the report |
| `bench/report.py` and `report_template.html` | Make `results/report.html` from the results |
| `bench/repo_template/` | The CI script, the lockfile generator and the `nerudo.toml` of the benchmark repo |
| `deploy/forgejo.yaml` | A throwaway Forgejo on the Talos cluster (namespace `nerudo-bench`) |

## Run

```sh
cargo build --release
python3 bench/run.py --backend git --mode strict --n 1,4,16 --k 3 --ci 2
python3 bench/run.py --backend seq --mode seq    --n 1,4,16 --k 3 --ci 2
python3 bench/report.py && open results/report.html
```

The backends:

| Backend | What it is |
|---|---|
| `git`, `jj` | A local bare repo |
| `forgejo`, `github` | A forge |
| `seq` | seqd |
| `cf` | The Rust Durable Object sequencer. `--cf-host forgejo` runs it in local workerd, and `--cf-host artifacts` uses the deployed Worker. See `cf/README.md`. |

The modes:

| Mode | What the agent does |
|---|---|
| `strict` | It makes derived files again, tests again, and pushes with compare-and-swap. |
| `lax` | It merges with no new test. |
| `pr` | It opens a forge PR and uses the merge API. |
| `jj` | It uses jj (`--jj-layout workspaces\|clones`, `--allow-backwards`). |
| `seq` | It asks seqd to land the change (`--mirror forgejo\|github`). |
| `cf` | It asks the Durable Object sequencer to land the change. |

## Landing rules: `nerudo.toml` on trunk

```toml
[footprint]
units = ["targets/*"]             # each child dir is one footprint unit
[[commutative]]
path = "CHANGELOG.md"             # appended lines are merged, never serialized
rule = "append"
[[derived]]
path = "deps.lock"                # never taken from a change; regenerated at landing
command = ["python3", "ci/gen_lock.py"]
[[allocate]]
dir = "migrations"                # changes write NEXT_<name>; seqd numbers them
pending_prefix = "NEXT_"
width = 4
[ci]
command = ["python3", "ci/check.py"]
```

Thus an agent must obey two rules:

- Write `migrations/NEXT_<name>.sql`, not a number.
- Do not edit the lockfile by hand.

## Seeded-defect review experiment

Can models do the code review, so that people only sign seals? `bench/review_*.py`
measures this on a corpus that we made by hand. The corpus has 10 Intents for a small
`ledger` package (`bench/review_template/`). Each Intent has a clean change and variants
with a defect. Each variant passes the test suite. The classes of defect are:

- hollow tests;
- tests that existed before, made weaker or skipped;
- boundary bugs;
- requirements that are not there;
- edits to CI or to the rules;
- irreversible migrations that the change does not declare;
- edits outside the scope.

```sh
kubectl --context admin@turingpi -n ollama port-forward svc/ollama 11434:11434 &   # local reviewer
python3 bench/review_run.py --reps 1      # gates + all reviewers; verdicts cached per prompt hash
python3 bench/report.py                   # adds the "Can models do the review?" section
```

- Level 0 (`review_gates.py`) has five gates:
  - the trusted test runner (never the CI of the change itself);
  - mutation tests of the added lines (threshold 0.70);
  - a ratchet check on the tests that existed before;
  - governance paths;
  - destructive SQL.

  The last three are seal boundaries.
- Level 1 (`review_models.py`) has four reviewers: Claude Haiku and Sonnet through
  `claude -p`, Codex `gpt-6-astra` through `codex exec`, and `qwen2.5:3b` on the Ollama of
  the cluster. Each reviewer sees the Intent and the diff, never the reasons of the author.
- After the runs, the report scores each routing policy from the recorded verdicts. It
  counts the defects that escaped, the clean changes that landed, and the decisions of
  people for each 100 changes.

## Not implemented

The work that is not done is in the tasks of the live repo, with the label `not-implemented`:
<https://nerudo-web.patrickdeutsch.workers.dev/nerudo/nerudo/tasks>.
Each task tells what the code does today, where it is in the code, and when the work is
done. For `seqd`: Raft replication (T-46), read-set footprints (T-47), footprints learned
from failed batches (T-48), lanes and backpressure (T-49), speculative CI of the next batch
(T-30) and `unland` (T-55).

## Licence

You can use this code under the MIT licence or under the Apache-2.0 licence. You select
one. See `LICENSE`, `LICENSE-MIT` and `LICENSE-APACHE`.
