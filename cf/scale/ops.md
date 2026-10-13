# Operations: metrics, SLOs, alerts and tests in CI

> This document comes from branch `ops`. The build of branch `freeze` has the code of `ops`
> up to `fbe9c58`, with one change: commit `414113f` reverts `dba1e25`. Thus there is no
> `METRICS` binding, and the metric points write nothing. Nobody deployed the alert Worker.
> The workflow file `.github/workflows/faults.yml` is in the tree.

This document tells how to see the landing loop without an experiment, and how each change
gets its tests. It covers:

1. Metrics: each round of the landing loop writes one data point to Workers Analytics Engine.
2. SLOs, the queries and a report: [`slo.md`](slo.md).
3. Alerts: a Worker with a cron trigger that posts to a webhook (`../nerudo-slo`).
4. A test of 1 to 3 on a Worker for the test only, on Cloudflare with Artifacts.
5. A GitHub Actions workflow that runs the tests and the fault harness.
6. The cause and the fix of the flake of the `approvals` scenario.

All work is from 2026-10-12. The data is in
[`../evidence/ops-20261012.json`](../evidence/ops-20261012.json).

## Summary

- The Trunk object writes one data point for each round, and one for each alarm that ends
  in an error. Without the binding `METRICS`, it writes nothing. Thus the deployed config,
  local dev and the fault harness operate as before.
- Four SLOs: queue age, landing latency, errors of the loop, and CI availability
  ([`slo.md`](slo.md)). `slo_report.py` prints them. The alert Worker pages on a fast burn of
  the error budget.
- On a test Worker, bursts of 128 and 256 changes landed with a mean latency of 5.1 s and a
  maximum of 9.1 s. A simulated crash and a CI outage of 3.5 minutes made two SLOs fail. The
  alert Worker, in `wrangler dev`, sent a page for the CI outage and a ticket for the crash.
- The flake of `approvals`: a reply that the Trunk object sent before it read the request
  body broke the connection of that body. The next request on the connection failed
  ("Network connection lost"). A 403 did it in `approvals`; `resume` and `share` do it too.
  The Worker now sends small bodies to the object whole, and the refusals read the body.
  With 4 threads, 18 of 2,000 pairs of requests failed before the fixes, and 0 of 4,000 after.
- The workflow `.github/workflows/faults.yml` runs `cargo test`, the wasm check, the tests of
  the alert rules and `nix run .#faults`. It needs no secret and deploys nothing. It was not
  run on GitHub. The same commands ran on this computer (macOS), and in a Linux container.

## Turn the metrics on

The code is in `nerudo-worker/src/trunk/metrics.rs`. It needs one binding in the config of
the Worker (commit `dba1e25` adds it to `nerudo-worker/wrangler.toml`):

```toml
[[analytics_engine_datasets]]
binding = "METRICS"
dataset = "nerudo_metrics"
```

Analytics Engine makes the dataset at the first write. To turn the metrics off, remove the
binding. The code does not need a change.

A write does not wait for I/O. If a write fails, the object logs `metrics: ...` and the round
continues. One round writes one data point. An alarm runs at most 50 rounds, and Analytics
Engine allows 250 writes in one invocation.

Cost: the Workers Paid plan includes 10 million data points each month, then $0.25 for each
million. On 2026-10-12, Analytics Engine bills nothing. A trunk that lands one batch each
second writes 2.6 million data points in 30 days. The 200-trunk test of
[`repos.md`](repos.md) (200 landings each second, batch size 1) would write 200 data points
each second: 518 million in 30 days, approximately $127 at the published price.

### Changes in the Trunk object

The changes are small, so that the work of other branches on `trunk.rs` can merge:

| Place | Change |
|---|---|
| `trunk.rs`, `mod` list | `mod metrics;` |
| `trunk.rs`, `alarm()` | On an error: `self.metric_alarm_error(e)` |
| `trunk.rs`, `land()` | The new field `queued_ms: None` |
| `trunk.rs`, `requeue()` | `rec.queued_ms = Some(now)`: the change returned from a wait for a person |
| `trunk.rs`, `schedule_with()` | `tm.oldest_ms`, `tm.fetch_retries += 1`, `tm.eject(...)` (4 places), `tm.ci`, `tm.waits_ms`; a round in which trunk fails the type check now calls `round_done` |
| `trunk/timings.rs` | New fields of `Round`: `oldest_ms`, `fetch_retries`, `ci`, `ejected_by`, `waits_ms` (memory only). `round_done` calls `metric_round`. |
| `types.rs` | `ChangeRec.queued_ms` (optional; not written when empty) |
| `ci.rs` | Each error of a CI call starts with "CI Worker", so that the metrics give it the cause `ci` |
| `lib.rs` | `to_trunk`: the Worker sends a small request body to the Trunk object whole (the flake fix). When a Trunk object throws, the Worker logs `trunk {name}: {error}`. The reply (HTTP 500) is the same. |

## The test on Cloudflare

### What ran

| Item | Value |
|---|---|
| Worker | `nerudo-scale-ops`, for the test only. Versions: `fe88b32a` (commit `932774f`), `9670df4b` (secrets), `4e50f657` (commit `bbbd702`), `93a620e1` (CI bound to the stub below), `15e3d770` (CI bound to `nerudo-ci-scale` again), `765a50a2` (commit `fbe9c58`, the final code). |
| Bindings | `TRUNK`, `HUB`, `ARTIFACTS` (namespace `nerudo-scale`), `CI` (`nerudo-ci-scale`), `METRICS` (dataset `nerudo_scale_ops`). No `AI`. |
| CI stub | `nerudo-ci-scale-ops` (version `6098da7b`): HTTP 503 for 4 minutes, then each check went to `nerudo-ci-scale`. It was deleted after the test. |
| Client | This computer. `ops_burst.py` (not committed) uses `scripts/scenarios.py` and `stages.py`. It read the front page of the live Worker every 2 minutes; each reply was HTTP 200. |

The steps:

1. Bursts: a new trunk with 4 agents. Each agent pushed its commits first. Then the client
   sent all `POST land` requests at once (16 at the same time), with no pause of the trunk.
   It waited until the queue was empty.
2. CI: a trunk with `[ci] typecheck`. Three changes passed the type check, and one failed it.
3. Crash: a trunk with `debug_crash_after_push = 1`. One alarm ended in an error after the
   push. The next alarm recovered the attempt.
4. CI outage: the CI binding went to the stub. One change on a CI trunk waited during the
   outage, and landed when CI was available again.

### Results

| Trunk | Changes | Landed | Drain (s) | Batches | Git requests |
|---|---|---|---|---|---|
| `ops-b0-155413` | 128 | 128 | 6.7 | 9 | 24 |
| `ops-b1-155443` | 256 | 8 (see below) | | 3 | 11 |
| `ops-b0-155641` | 256 | 256 | 13.7 | 16 | 39 |
| `ops-b0-155826` | 128 | 128 | 6.7 | 9 | 24 |
| `ops-b1-155852` | 256 | 256 | 12.8 | 16 | 40 |
| `ops-b2-155928` | 256 | 256 | 16.8 | 17 | 56 |
| `ops-ci-155530` | 4 | 3, and 1 ejected for the type check | | 4 | |
| `ops-crash-155545` | 1 | 1, recovered after the crash | | 1 | |
| `ops-cidown-160500` | 1 | 1, 217 s after it was sent | | 1 | |
| `ops-b0-165906` (final code) | 128 | 128 | 6.3 | 8 | 22 |
| `ops-b1-165930` (final code) | 256 | 256 | 10.6 | 16 | 40 |
| `ops-ci-170002` (final code) | 4 | 3, and 1 ejected for the type check | | 4 | |

The drain time is from the first `POST land` to an empty queue, on the clock of the client.
No change was ejected except the one with the type error. Each burst trunk landed each of its
changes.

In `ops-b1-155443`, 248 of 256 `POST land` requests got HTTP 500 ("INTERNAL SERVER ERROR")
in 24 s, and 8 landed. The Worker answered a bare 500 because the request to the Trunk object
threw, and it logged nothing. `wrangler tail` did not run at that time. Cloudflare's analytics
counted these invocations as successes.

The same burst ran 3 more times (896 changes, with `wrangler tail --status error`), and 2
more times on the final code (384 changes), with no error. The cause is not known. Commit
`dfef0ed` now logs the error of a Trunk object with the trunk name, so the next time shows
the cause.

### The SLO report and the alerts

`slo_report.py --dataset nerudo_scale_ops --window 3h --trunk ops-` gave the report in
[`slo.md`](slo.md#the-report): `queue_age` and `landing_latency` met (100%), `loop_errors` at
98.67% (the crash), `ci_availability` at 3.05% (127 rounds failed during the CI outage).

The alert Worker ran in `wrangler dev --test-scheduled` on this computer, against the real
SQL API and the test dataset, with a webhook on this computer:

| Run | Time | Posted |
|---|---|---|
| 1 | 10 minutes after the crash, before the CI outage | `ticket: loop_errors is at 98.67% (objective 99.9%). Burn rate 13.3 over 360 min and 13.3 over 30 min.` No page: the burn rate over 60 min was 13.3, below 14.4. |
| 2 | Just after the CI outage | `page: ci_availability is at 3.81% (objective 99.5%). Burn rate 192.4 over 60 min and 200.0 over 5 min.`, and tickets for `loop_errors` and `ci_availability` |

### Metrics against the true counts

`GET .../timings` and `GET .../stats` of each trunk give the true counts. Analytics Engine
kept 44 of the 75 round data points (`_sample_interval` up to 3). The weighted estimate was 74
rounds and 1,002 landings, against 75 and 1,037. See [Sampling](slo.md#sampling).

Two landings are not in the metrics. The landing that the loop recovered after the crash is
not in a round. The sampling did not keep the data point of the one round of
`ops-cidown-160500`.

### Artifacts repos and cost

The test made 12 trunk repos and 36 forks in the namespace `nerudo-scale`, and did not delete
them:

- Trunks: `ops-b0-155413`, `ops-b1-155443`, `ops-b0-155641`, `ops-b0-155826`,
  `ops-b1-155852`, `ops-b2-155928`, `ops-ci-155530`, `ops-crash-155545`, `ops-cidown-160500`,
  `ops-b0-165906`, `ops-b1-165930`, `ops-ci-170002`.
- Forks (`{trunk}--{agent}`, `host.rs`): `--a00` to `--a03` for each of the 8 burst trunks,
  and `--a00` for the other 4.

The Worker got 1,390 requests from 21:40 to 22:40 UTC (the analytics of Cloudflare), and
approximately 500 more on the final code. The Durable Objects got these requests and the
alarms: fewer than 4,000 in all. At $0.30 and $0.15 for
each million, the cost is less than $0.01. The SQL queries and the data points are in the
included amounts.

## Tests in GitHub Actions

`.github/workflows/faults.yml` runs on each push to `main` and each pull request that changes
`cf/`, and on a manual start. One job on `ubuntu-24.04`:

1. `cargo test --workspace --locked` (in `cf/`)
2. `cargo check -p nerudo-worker --target wasm32-unknown-unknown --locked`
3. `node --test nerudo-slo/test/slo.test.mjs` (the alert rules)
4. Nix (`DeterminateSystems/nix-installer-action`, with `magic-nix-cache-action` as the cache
   of the store), then `libssl-dev` (apt) and `cargo install worker-build --version 0.8.7 --locked`
5. `nix run .#faults` (all cases). The JSON report, and the logs of a failed run, are kept
   as an artifact for 14 days.

It has the permission `contents: read` only. It uses no secret, it makes no request to
Cloudflare, and it deploys nothing. The flake did not change: it already supports
`x86_64-linux` and `aarch64-linux` (for an ARM runner, use `runs-on: ubuntu-24.04-arm`).

The workflow did not run on GitHub, because this work does not push. The same commands ran:

| Where | Steps | Result |
|---|---|---|
| This computer (macOS, aarch64) | 1, 2, 3 | All tests passed |
| This computer, `nix run .#faults` at `bbbd702`, `dba1e25` and `fbe9c58` | 5 | 82 passed, 5 known gaps, 0 failed (each run) |
| Linux container (`ubuntu:24.04`, aarch64, Docker), as a user that is not root, at `86fa17d` | 1 to 5 | Steps 1 to 4 passed. Harness: 49 passed, 3 known gaps, 1 failed (`ci_down` stopped on "Network connection lost"; see [the flake](#the-flake-of-the-approvals-scenario)) |
| The same container at `fbe9c58` | 1 to 5 | All passed. Harness: 82 passed, 5 known gaps, 0 failed |

The container used the single-user Nix installer from nixos.org instead of the Determinate
installer, and Node 22 from nixpkgs instead of `actions/setup-node`. Gitea does not run as
root, and a GitHub runner is not root either.

The first runs in the container found two errors in the workflow (commit `86fa17d`):

1. Node 22 does not find test files in a directory argument (`node --test nerudo-slo/`).
   Node 26 on this computer does. The step now names the test file.
2. `cargo install worker-build` needs the OpenSSL headers. The step now installs
   `libssl-dev`. The image of the GitHub runner probably has it, but the step does not
   depend on that.

The third run found a limit of the flake on `aarch64-linux`: at the locked nixpkgs,
toxiproxy is not in the binary cache (`nix path-info --store https://cache.nixos.org`). Nix
builds it and runs its tests, and the timing test `TestFullstreamLatencyBiasUp` failed in the
container. For `x86_64-linux` (the GitHub runner) and `aarch64-darwin`, all six tools of the
flake are in the cache. The container built toxiproxy again (up to 5 tries) before it ran the
harness. The flake did not change.

## The flake of the approvals scenario

### The symptom

In some runs of the fault harness, the `approvals` scenario failed. The admin request
`POST .../changes/c1/close` came immediately after a request that got 403. `wrangler dev` logged:
"Error inside ProxyWorker ... POST .../changes/c1/close (failed after 1 attempt): Network
connection lost."

### The cause

The Trunk object answered some requests before it read the request body. Then:

1. The Worker had sent the request, with its body as a stream, to the Trunk object. The
   object answered before it read the body.
2. The Worker then could not read the rest of the request: "Uncaught TypeError: Can't read
   from request stream after response has been sent." `wrangler dev` logged this error (or
   "NOSENTRY reporting trace with exceptions") for each such request.
3. The connection from the proxy of `wrangler dev` to the Worker was then not usable. If
   the proxy sent the next request on that connection, that request failed.

In `approvals`, the request was `POST .../changes/{cid}/close` with the agent token and no
agent key: the Trunk object sent 403 before it read the body. The 401 and 404 replies, and
the 403 for a wrong agent key, did the same. So do three routes that never read the body:
`POST resume`, `POST share` and `POST agents/{name}/key`. The harness sends `resume` with a
body (`{}`).

In the Linux container, the case `ci_down` failed with the same error after it
resumed its trunks. The logs of that run were lost with the container. Thus the request that
broke the connection is not known.

### The test

A script sent pairs of requests to a local `wrangler dev` (the version of the flake,
4.148.0). The first request of a pair was a POST that the Trunk object answered before it
read the body. The second was an admin POST that read its body (404). Four threads sent
pairs at the same time.

| First request | Build | Pairs | Failed pairs | "Can't read from request stream" errors |
|---|---|---|---|---|
| 403 of `close` | Before the fixes, 1 thread | 400 | 0 | 400 of 400 |
| 403 of `close` | Before the fixes, 4 threads | 1,200 (3 runs) | 12 (5, 0, 7) | 1,195 of 1,200 |
| Control: 404 of `close` (reads its body) | Before the fixes, 4 threads | 400 | 0 | 0 |
| 403 of `close` | `75dd7b5`, 4 threads | 2,000 (3 runs) | 0 | 0 |
| 404 of `share` (does not read its body) | Before the fixes, and `75dd7b5` (the same for `share`) | 800 (2 runs) | 6 (2, 4) | 800 of 800 |
| 404 of `share` | `da946c6`, 4 threads | 1,600 (2 runs) | 0 | 0 |
| 403 of `close` | `da946c6`, 4 threads | 400 | 0 | 0 |

Each failed pair had HTTP 500 "Network connection lost" from the proxy, on the first or the
second request.

### The fix

Two commits:

1. `75dd7b5`: a 401, 403 or 404 can come before the handler reads the body. Before such a
   reply, the Trunk object reads the body (at most 1 MiB) and drops it (`refuse` in
   `trunk.rs`). This covers the requests that come to the object by another path
   (`/v1/repos/...` through the Hub), and the git routes.
2. `da946c6`: the Worker reads a request body of at most 8 MiB, with a known length, before
   it sends the request to the Trunk object. Then it sends the body whole (`to_trunk` in
   `lib.rs`). Thus no route of `/v1/trunks/...` or `/t/...` can break the connection. A git
   body, or a body with no length, still goes as a stream.

A request by name (`/v1/repos/{org}/{repo}/...`) goes to the Trunk object through the Hub
(`hub::forward`), not through `to_trunk`. On that path, `resume`, `share` and `POST
agents/{name}/key` still answer before they read the body; only the refusals read it. This
work did not change `hub.rs`, because the live Worker runs changes to it that are not
committed.

This also removes the uncaught error from these requests on Cloudflare. The harness and the
scenarios did not change: they do not try a request again.

## Is it safe before the freeze?

| Commit | Change | Risk |
|---|---|---|
| `75dd7b5` | Read the body before an early refusal | Low. It touches only replies that are refusals. The body read stops at 1 MiB. |
| `932774f` | Metrics | Low without the binding: the code returns at once when `METRICS` is absent. The round records have new fields; old stored rounds read with defaults (`#[serde(default)]`). `ChangeRec.queued_ms` is optional and is not written when empty. |
| `bbbd702` | CI errors start with "CI Worker" | Very low: only the text of an error. |
| `dfef0ed` | Log the error of a Trunk object | Very low: one log line, the same reply. |
| `da946c6` | The Worker reads a small body before it sends the request to the Trunk object | Low. Each API request with a body of at most 8 MiB is read in the Worker first (JSON bodies are small). A larger body, for example a big seed of `init`, and git bodies go as a stream, as before. |
| `53ca479`, `fbd9c8b`, `132d89a`, `2d7157a`, `86fa17d`, `fbe9c58`, `c24112b` | Report, alert Worker, workflow, documents and evidence | None for the live Worker: nothing deploys. |
| `dba1e25` | The `METRICS` binding in `wrangler.toml` | Low. With it, the next live deploy writes data points. Leave this commit out to ship the code with the metrics off. |

The test Worker ran all of these commits except `dba1e25` (it had its own `METRICS`
binding). On the final code (`765a50a2`), 20 rounds of four requests each gave the
expected replies: a 403 of `close`, `resume` with a body, a 404, and a `GET`. The fault harness ran all of them (see
[Tests in GitHub Actions](#tests-in-github-actions)).

## Limits

- One run of each burst size, on one test Worker. The bursts were short (at most 256
  changes), so the percentiles come from few rounds.
- The alert Worker was not deployed. It ran only in `wrangler dev`, with the cron started by
  hand.
- The workflow did not run on GitHub. The Linux test used a container on an ARM computer,
  not the x86-64 runner of GitHub.
- The cause of the 248 errors in `ops-b1-155443` is not known.
- See also the limits in [`slo.md`](slo.md#limits).

## Files

| File | Contents |
|---|---|
| `../nerudo-worker/src/trunk/metrics.rs` | The data points |
| `slo.json` | The SLOs, the queries and the alert rules |
| `slo.md` | The SLOs, the columns, the queries, the report and the alerts |
| `slo_report.py` | The report |
| `../nerudo-slo/` | The alert Worker, and `node --test` for its rules |
| `../../.github/workflows/faults.yml` | The tests in GitHub Actions |
| `../evidence/ops-20261012.json` | The bursts, the true counts and the estimates, the SLO reports, the alerts, the flake test, and the fault harness runs |
