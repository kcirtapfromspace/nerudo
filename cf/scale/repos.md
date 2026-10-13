# Many trunks at once

This test answers one question: are trunks independent? Each trunk is one Durable Object,
and each trunk pushes to its own Artifacts repo. If the trunks are independent, then K
trunks on one Worker land K times the changes of one trunk. Also, the latency of a change
does not increase when K increases.

The test ran on 2026-10-12 against a Worker for the test only, on Cloudflare with
Artifacts. It did not send load to the live Workers.

## Result

The trunks were independent at every step of the test. The highest load was 200 trunks at
1 change each second. They landed 199.99 changes each second, and the p50 latency was
641 ms. With 1 trunk, the p50 latency was 712 ms.

| Offered to each trunk | K | Expected landings/s (K × the rate of 1 trunk) | Measured landings/s | Measured ÷ expected | Slowest trunk (landings/s) |
|---|---|---|---|---|---|
| 0.5/s | 1 | 0.500 | 0.500 | 1.00 | 0.500 |
| 0.5/s | 10 | 5.00 | 5.003 | 1.00 | 0.500 |
| 0.5/s | 50 | 25.0 | 25.005 | 1.00 | 0.500 |
| 0.5/s | 200 | 100.0 | 100.074 | 1.00 | 0.484 |
| 1/s | 1 | 1.000 | 1.000 | 1.00 | 1.000 |
| 1/s | 200 | 200.0 | 199.988 | 1.00 | 0.996 |
| 3/s (saturated) | 1 | 2.963 | 2.963 | 1.00 | 2.963 |
| 3/s (saturated) | 50 | 148.2 | 150.049 | 1.01 | 2.911 |

At 3 changes each second, one trunk is near its limit (see "The limit of one trunk"). Fifty
saturated trunks together landed 50 times the rate of one saturated trunk.

Of 126,990 changes sent, 126,988 landed. Each landed exactly once (`nerudo verify` on
all 200 trunks). Two pushes to a fork failed (see "Errors"). No change was ejected.

## What the test used

| Item | Value |
|---|---|
| Worker | `nerudo-scale-repos`, version `c2dcbfee-7997-40ac-a20a-b3a389ec2d6d` (the secrets were set after the first version, `98273cdb-529a-4917-8ad4-f8113c7ee4f4`) |
| Code | Commit `3b28852`, built with `worker-build --release`. The test did not change product code. |
| Bindings | `TRUNK`, `HUB`, `ARTIFACTS` (namespace `nerudo-scale`). No `CI` binding and no `AI` binding. |
| Artifacts repos | 200 trunks, `repos-000` to `repos-199`. 400 forks, `repos-NNN--a0` and `repos-NNN--a1`. Total 600 repos. The test did not delete them. |
| Client | One Mac. The load generator `repos-load` (Rust, `cf/scale/repos-load`). |

The Worker does not need the `CI` and `AI` bindings for these trunks. The Trunk object
calls `CI` only in `ci::typecheck`, and only when `nerudo.toml` has `[ci] typecheck`. It
calls `AI` only when `nerudo.toml` has `[decide]`. The seed `nerudo.toml` has neither.

The test sent requests only to `/v1/trunks/{trunk}/...`. It did not use the Hub object.

### Trunks and forks

`repos-load setup` makes each trunk with `POST init` and a seed. The Trunk object creates
the Artifacts repo and pushes the seed. Then the generator asks for the forks of 2 agents,
`a0` and `a1`, one after the other. It makes 1 to 6 trunks at the same time, so each trunk
makes at most 1 fork at a time.

The seed has `nerudo.toml`, `README.md`, `changes/.keep` and 12 files under `targets/`.
Its rules are:

```toml
[footprint]
units = ["targets/*"]

[seals]
governance = ["nerudo.toml"]
```

| Step | Time for each trunk (p50, max) | Failures |
|---|---|---|
| `init` (create the repo, push the seed) | 3.4 s, 5.4 s | 0 of 200 |
| Fork | 2.8 s, 4.8 s | 0 of 400 |

### Changes

Each change adds one new file, `changes/NN/<change id>.txt`. Thus the footprint of each
change is one path that no other change touches. No two changes overlap.

The generator makes each change in memory with `nerudo-git`. It does not use a git
process. The parent of each change is the commit that the fork has: the seed. The
generator does not fetch `main` before a change. For each change it does 3 requests:

1. Push the change to the fork of the agent (2 requests to Artifacts).
2. `POST /v1/trunks/{trunk}/land` with the key of the agent (1 request to the Worker).

### Load

The load is open loop. Each trunk gets a change at fixed times, every 1/R seconds, and the
time does not depend on earlier changes. The generator spreads the start times of the trunks
evenly over one period. Agents `a0` and `a1` send the changes in turn. A pool of 16 to 384
threads sends the changes.

Each run has these phases:

1. Fetch the seed of each fork, and read `stats` of each trunk.
2. Send changes for 300 s (180 s at 3/s).
3. Stop admission. Read `stats` of each trunk every 10 s until each queue is empty.
4. Read the landing log of each trunk, and the receipt of each change that is not in the
   log.

During the run, the generator reads `stats` of each trunk every 10 s. It does not use the
long poll `changes/{id}?wait=`. Every 60 s it reads the front page of the live Worker
(`nerudo.patrickdeutsch.workers.dev`). A second loop read the same page every 2 minutes.

The generator stops admission if one of these occurs:

- The live Worker does not answer 200.
- A request gets HTTP 429.
- More than 5% of the changes in the last 60 s failed.

None of these occurred.

### Measurements

- Latency is `landed_at_ms` (landing log) minus `submitted_ms` (the receipt from `land`).
  The Trunk object writes both times.
- A landing rate is the number of landings in the steady window, divided by the length of
  the window. The steady window starts 30 s after the first submission, and it stops at
  the last submission. Both times come from the server.
- The send lag is the time from the planned send time to the real send time, on the Mac.
  If the Mac cannot send the load, the send lag increases.
- `git_requests` and `batch_sizes` come from `stats`.

## Run the test

```sh
cd cf/scale/repos-load && cargo build --release
B=target/release/repos-load
ulimit -n 8192    # macOS allows 256 open files by default
$B setup --vars scale.vars --state state.json --count 200 --conc 6
$B run --vars scale.vars --state state.json --k 200 --rate 0.5 --secs 300 \
  --tag k200r05 --out raw-k200r05.json --threads 256
python3 -I ../repos_report.py summary.json raw-*.json
```

`scale.vars` has `NERUDO_WORKER`, `NERUDO_ADMIN_TOKEN` and `NERUDO_AGENT_TOKEN` of the
test Worker. `state.json` has fork tokens and agent keys. Do not add these two files to the
repo.

## Results

The runs went in this order. A run started only if the run before it had no HTTP 429,
an error rate below 5%, and a live Worker that answered 200. The summary of each run, with a row for each trunk, is in
`cf/evidence/scale-repos-20261012.json`.

| Run | K | Offered/s (each trunk) | Landed / sent | Errors | Landings/s | Latency p50 / p95 / p99 (ms) | Batch mean / max | git requests / landing | Max send lag (ms) |
|---|---|---|---|---|---|---|---|---|---|
| `k1r05` | 1 | 0.5 (0.5) | 150 / 150 | 0 | 0.50 | 667 / 824 / 1,253 | 1.0 / 1 | 3.0 | 10 |
| `k1r1` | 1 | 1 (1) | 300 / 300 | 0 | 1.00 | 712 / 1,045 / 1,321 | 1.0 / 2 | 3.0 | 5 |
| `k1r3` | 1 | 3 (3) | 540 / 540 | 0 | 2.96 | 5,749 / 8,732 / 9,974 | 11.5 / 16 | 1.18 | 9 |
| `k10r05` | 10 | 5 (0.5) | 1,500 / 1,500 | 0 | 5.00 | 667 / 928 / 1,165 | 1.0 / 3 | 3.0 | 10 |
| `k50r05` | 50 | 25 (0.5) | 7,500 / 7,500 | 0 | 25.0 | 645 / 836 / 1,096 | 1.0 / 4 | 3.0 | 18 |
| `k200r05` | 200 | 100 (0.5) | 29,998 / 30,000 | 2 | 100.1 | 639 / 831 / 1,163 | 1.0 / 12 | 3.0 | 71 |
| `k50r3` | 50 | 150 (3) | 27,000 / 27,000 | 0 | 150.0 | 2,775 / 5,506 / 7,755 | 5.4 / 16 | 1.37 | 8 |
| `k200r1` | 200 | 200 (1) | 60,000 / 60,000 | 0 | 200.0 | 641 / 877 / 1,647 | 1.01 / 9 | 2.99 | 12 |

In each run, all queues were empty within 12 s after the generator sent the last change. The
generator read `stats` every 10 s, so the test cannot measure a shorter drain.

The send lag stayed below 71 ms, and the local queue of the generator stayed at 0. Thus the
Mac sent the full offered load in each run. The time to push a change to a fork (p50) was
233 to 336 ms. The time of the `land` request (p50) was 73 to 124 ms.

### Latency did not increase during a run

p50 and p95 latency (ms) for each minute of submission:

| Run | Minute 0 | 1 | 2 | 3 | 4 |
|---|---|---|---|---|---|
| `k1r05` | 685 / 844 | 657 / 781 | 668 / 749 | 649 / 1,016 | 681 / 896 |
| `k200r05` | 650 / 932 | 641 / 841 | 638 / 801 | 634 / 813 | 635 / 798 |
| `k200r1` | 643 / 999 | 635 / 829 | 637 / 826 | 644 / 914 | 647 / 893 |
| `k50r3` | 2,854 / 5,616 | 2,625 / 4,980 | 2,876 / 5,851 | | |
| `k1r3` | 4,971 / 7,257 | 5,670 / 7,562 | 6,887 / 9,614 | | |

At 3/s, the latency of one trunk (`k1r3`) increased by approximately 1 s each minute. Thus
3/s is at or slightly above the limit of that trunk. In `k50r3`, the latency did not increase.

### The limit of one trunk

At 1 change each second or less, each change lands alone (batch size 1). Each landing
needs 3 git requests: 1 fetch of the change from the fork, and 2 for the push to `main`.
A landing takes approximately 650 ms.

At 3/s the queue holds changes, and the batches get bigger (mean 5.4 in `k50r3`, 11.5 in
`k1r3`, maximum 16, the default `batch_cap`). One push then lands many changes, so the git
requests for each landing decrease to 1.2 to 1.4.

The gate fetches each queued change from its fork, one fetch after the other
(`trunk.rs`, `schedule_with`). Thus the limit of one trunk is approximately 1 divided by the
time of one fetch from Artifacts. In this test that is approximately 3 changes each second.

The service time is different for each trunk. In `k50r3`, the p50 latency of the trunks
was 1.9 s to 5.4 s, with the same load and the same history. The slowest trunk was
`repos-000`, which had the longest history (1,455 landings before the run, 2,295 at the
end). `k1r3` also used `repos-000`. Thus the K=1 latency at 3/s is one sample, from the
slowest trunk. It is not a typical value.

### Errors

| Kind | Count | Where |
|---|---|---|
| Push to a fork: `artifacts_git_receive_pack_busy` | 2 of 126,990 | `k200r05`, both on fork `repos-042--a1` |
| Fetch from a fork tried again by the gate (`fetch_retries`) | 23 | 2 in `k50r05`, 7 in `k200r05`, 5 in `k50r3`, 9 in `k200r1`. Each change landed. |
| Changes ejected | 0 | |
| `main` moved by something other than the sequencer (`external_moves`) | 0 | |
| HTTP 429 | 0 | |
| Worker errors (`wrangler tail --status error`) | 0 events | |
| Live Worker health | 200 at each check | 29 checks by the 2-minute loop, 45 by the generator |

The two push failures came from one fork. At 286 s in `k200r05`, one push to
`repos-042--a1` took 14.2 s. The next pushes to the same fork took 10.3 s, 6.2 s and 2.2 s,
and two of them failed with `artifacts_git_receive_pack_busy`. Artifacts probably accepts
only one push into a repo at a time. At the same time, the other 399 forks took their usual
time.

The generator does not try a failed push again. It records the push as an error.

In the same run, trunk `repos-081` stopped for approximately 24 s at 245 s. One landing took
24.6 s. Then the next 12 changes landed in one batch. No error was recorded. The latency of
the other trunks did not change.

Both events stayed in one repo. They do not show a rate limit on the account: there was no
429, no quota error, and no other repo slowed.

This event shows a gap: the Trunk object sets no timeout on its git requests
(`transport.rs`). If one request to Artifacts stops, the landing loop of that trunk waits
for the full time. This is the same kind of gap as `ci_hang.timeout` in
`cf/faults/README.md`. The test did not change the code.

### Integrity

After the last run, `nerudo verify` passed on each of the 200 trunks. It clones `main`
with a read-only token. It checks that each landed change is on `main` once, and that the
hash chain of the landing log ends at the tip of `main`. The 200 trunks have 127,003
landings. Of these, 126,988 are from the runs in the table, and 15 are from a short check run
on `repos-000` before them.

### Cost

The prices: Workers $0.30 for each million requests, Durable Objects $0.15 for each million
requests and $12.50 for each million GB-s. Artifacts costs nothing until 2026-10-15. The
duration is an upper limit: each trunk active for the full run, at 128 MB.

| Run | Worker requests | DO requests (with alarms) | DO GB-s (at most) | Artifacts requests (client + Trunk) | Cost (at most) |
|---|---|---|---|---|---|
| `k1r05` | 185 | 335 | 39 | 300 + 451 | $0.001 |
| `k1r1` | 335 | 634 | 39 | 600 + 899 | $0.001 |
| `k1r3` | 563 | 610 | 24 | 1,080 + 635 | $0.001 |
| `k10r05` | 1,850 | 3,346 | 394 | 3,000 + 4,493 | $0.006 |
| `k50r05` | 9,250 | 16,745 | 1,969 | 15,000 + 22,493 | $0.03 |
| `k50r3` | 28,150 | 33,149 | 1,219 | 54,000 + 37,053 | $0.03 |
| `k200r05` | 36,998 | 66,976 | 7,875 | 60,000 + 89,971 | $0.12 |
| `k200r1` | 67,000 | 126,611 | 7,875 | 120,000 + 179,323 | $0.14 |

The setup added 200 `init` requests, 400 fork requests, and approximately 1,400 reads of
fork refs and seeds. `nerudo verify` added 200 clones of `main`.

Storage writes are not in the table. They are approximately 10 to 15 rows for each change
(an estimate, not a measurement). That is 1.3 to 1.9 million rows, or $1.30 to $1.90 at
$1.00 for each million rows written. Thus the full test cost $2.30 or less. The
monthly amounts included in Workers Paid probably cover all of it.

## What the results tell

**50 merges each second.** This is the published average rate of merged pull requests on
GitHub. GitHub published approximately 130 million merged pull requests each month in
["The August 17 outage, and the work ahead"](https://github.blog/news-insights/company-news/the-august-17-outage-and-the-work-ahead/) (The GitHub Blog, 2026-08-20). That is approximately 50 each second. One Worker on Artifacts landed 100 changes each second (200 trunks at 0.5/s), 150
(50 saturated trunks) and 200 (200 trunks at 1/s). Thus 50 each second is possible when
the load is spread over approximately 17 or more trunks at their limit. It is also possible
with 50 or more trunks at 1/s. One trunk cannot do it: one trunk lands approximately 3
changes each second with this workload.

**23,000 writes each second.** This is "2 billion each day" from the brief of the test.
That figure has no published source. The test did not reach this rate. It is 115 times the
highest rate of the test. At 1 change each second for each trunk, it needs approximately
23,000 active trunks, or approximately 7,700 trunks at their limit.

The results show that the rate increased linearly from 1 to 200 trunks. They do not show
that the increase continues to thousands of trunks. They do not show the limits of Artifacts
or of Durable Objects for one account at that count.

**What the results do not tell.** They do not tell how fast one busy repo can go when its
changes conflict, when CI takes time, or when the repo is large. In each of these cases
the limit of one trunk is lower than 3 changes each second.

## Limits of the test

- The generator builds the changes on the seed, not on the tip of `main`. A real agent
  fetches `main`, so it makes more requests to Artifacts than this test.
- No two changes overlap. Thus there are no conflicts, no ejections and no new revisions.
- There is no CI: `ci_ms` is 0 and there is no type check. Real CI adds time to each batch
  and lowers the limit of one trunk.
- Each trunk has 2 agents.
- The changes come at fixed intervals, not at random times. Real load has bigger bursts.
- Each run lasted 5 minutes (3 minutes at 3/s), not 30 to 60 minutes. Each condition ran
  once.
- The steps used the same trunks again. Thus the trunks had different history: `repos-000`
  had 2,295 landings at the end, `repos-050` to `repos-199` had 450. The wasm memory of one
  trunk was at most 18 MB (`repos-000`).
- A drain time shorter than approximately 10 s does not show, because the generator read
  `stats` every 10 s.
- `wrangler tail --status error` recorded no event. The test did not prove that the tail
  was connected, because there was no error to compare.
- The rules of the seed are not `bench-nerudo.toml`: no derived files, no changelog
  merge, no checks. Do not compare these numbers with the 52.5 landings each minute of 8
  real agents.
- One client computer in one place sent all the load. Cloudflare probably put all the
  trunk objects in one region. The test does not show other regions.
- The test did not use the Hub object.
- The check of the live Worker reads only the status code. It does not measure the latency
  of the live Worker.
