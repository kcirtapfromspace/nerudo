# Recording the demo video

This is the script for the demo video of NERUDO. The target length is
approximately 7 minutes (the limit is 5 to 10 minutes). Use one terminal and one browser
tab. Each number below comes from `README.md` (Measured) or from a live `nerudo demo` run.

The text in quotes is what you say. The other lines tell what to show.

## Before recording

1. Make the terminal approximately 140 columns wide, with a large font and a dark theme.
   When stdout is a terminal, the demo shows batch lines in green, parked changes in yellow
   and ejections in red.
2. Run `cargo build --release -p nerudo-cli`, so that nothing compiles on camera.
3. Load the tokens of the deployment, but do not show them:
   `set -a; . nerudo-worker/.prod.vars; set +a`. Never `cat` that file or run `env` on
   camera. The demo itself never prints a token.
4. Do one full run first: `cargo run --release -p nerudo-cli -- demo --yes`. This run
   warms the Worker, and it makes sure that `~/.nerudo/key` exists.
5. Open the benchmark report (`results/report.html`) in the browser tab, at the Cloudflare
   panel.

## Shot list

**0:00 to 0:45: the problem.** Show the report tab: the columns "Forgejo PR + merge" and
"push to main".
- "Coding agents now open changes faster than one branch can take them. Today they land
  through pull requests or by pushing to main."
- PR and merge API: each PR passes its own CI. But at 16 agents, 41 of 48 trunk commits are
  broken, and at 32 agents, 91 of 96. Each PR passed CI on its own base. Together, they
  broke trunk.
- Push to main is correct but serial. It lands approximately 16 changes each minute for any
  number of agents. At 16 agents it needs 10.7 CI runs for each landed change (22 at 32
  agents), because each lost race runs CI again.

**0:45 to 1:45: the design.** Show the diagram at the top of `README.md`.
- One Durable Object for each trunk is the only writer. Agents never push to trunk. Each
  agent has an Artifacts fork that it can write, and a trunk token that can only read.
- The object lands changes whose footprints do not overlap as one batch, with one CI run.
  Then it moves the trunk ref on Artifacts with a compare-and-swap push. Artifacts refuses
  a stale old value, also when the push is a fast-forward (tested). Thus a lost race cannot
  corrupt trunk.
- The sequencer merges by rule the files that always conflict in git. It appends changelog
  lines, makes the lockfile again, and gives migration numbers at landing.
- Some changes need a person: CI configuration, the rules file, and irreversible SQL. They
  wait for a seal that the person signs with their key, for that exact commit.
  `nerudo.toml` on trunk lists the keys that count, and how many must agree.
- All of it is Rust: the git client, the merge, the Worker and the CLI. There is no
  separate database: the queue, the receipts and the log are in the key-value storage of
  the object.

**1:45 to 2:15: the rules.** Scroll slowly through `fixtures/demo-nerudo.toml`. Each
block has a comment of one line that tells what it does. Read three of them aloud.

**2:15 to 5:00: the live run.** Run `cargo run --release -p nerudo-cli -- demo --crash-once`.
- Section 1: the Durable Object itself created the trunk. Point at the remote URL:
  "nothing outside the object has ever held a token that can write this repo".
  With `--crash-once`, the object stops immediately after its first push.
- Section 2: the forks appear, then the changes. Point at a green batch line: "four
  changes, one CI run". Point at a migration line: the agent wrote `NEXT_<id>.sql`, and
  the sequencer gave it its number. If a red ejection appears, say: "edited a line next to
  one that had just landed; the agent rebuilt on the new trunk and resubmitted, nothing
  else waited for it".
- The yellow line: a migration that drops a column is parked.
- Section 3: the prompt shows the reason and the exact commit. Type `y`. The change lands
  in the next batch.
- Section 4: the demo examines trunk from outside, with a read-only token:
  - Each commit passes the trunk checks.
  - The `Sealed-By` trailer of the drop-column landing verifies against the approvers in
    `nerudo.toml` at its parent.
  - Each change is on trunk exactly one time.
  - The migration numbers are unique.
  - The hash chain of the landing log ends at the tip of trunk.
- Numbers: the CI runs for each change (approximately 0.5 here), and "recovered 1". The
  sequencer reconciled the crash between the push and the record from trunk, and nothing
  landed two times.

**5:00 to 5:45: fencing.** Run `NERUDO_HOST=artifacts python3 scripts/scenarios.py fencing`.
It takes less than one minute; start it before you talk.
- The git tokens of an agent cannot push to trunk or to the fork of another agent. These
  are its fork token and its read-only token for trunk.
- An agent acts only with its own key. The shared agent token can only make a new agent.
  A request for a fork with the token alone, or with the key of another agent, gets 403.
- If someone takes the fork name of an agent first, the object makes the fork with a new
  name. It never gives a token for a repo that it did not make.

**5:45 to 6:45: the numbers at scale.** Show the report tab, at the Cloudflare panel. The
workload is the same as for the baselines, live on Workers and Artifacts.
- 52.5 landings each minute at 8 agents, 47.8 at 16, and 41.4 at 32. Push to main stays
  near 16.
- 0 broken trunk commits and 0 lost changes at each size.
- 0.31 CI runs for each landed change at 16 agents. Push to main needs 10.7.
- 6.4 to 8.0 git requests for each landing. Artifacts bills for each operation, so this
  number is the cost.
- The throughput is highest at 8 agents. A batch stays at approximately 3 changes, because
  popular targets overlap, and each batch takes a little more time. The next step is to run
  the CI of the next batch early.
- Optional: on throwaway Workers, 2,000 repos landed 1,978 changes each second. The changes
  did not overlap, and there was no CI. The Artifacts error rule stopped the run after
  131 s: one account reached its limit at approximately 1,000 to 2,000 landings each second
  (`README.md`, Measured).

**6:45 to 7:15: close.** "A trunk that only one thing can write, on Workers and Artifacts.
Agents keep their own forks; the sequencer decides what lands and proves it with a receipt.
Source, MIT or Apache-2.0, and `nerudo demo` runs all of this on your own account."

## If something goes wrong on camera

| Problem | Cause and action |
|---|---|
| `fork failed` | Artifacts did not make a fork, also after the sequencer tried again. Run the demo again. |
| `HTTP 401` | The tokens are not loaded in this shell. Load them (step 3 of "Before recording"). |
| The run is short | This is on purpose: 18 changes in approximately 30 s. For a longer shot, use `--agents 8 --changes 4`. |
