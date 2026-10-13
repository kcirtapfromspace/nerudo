# Service levels of the landing loop

> This document comes from branch `ops`. The build of branch `freeze` has no `METRICS`
> binding (commit `414113f`), so the Trunk objects write no data points. The queries, the
> report and the alerts need that binding first.

This document gives the service level objectives (SLOs) of the landing loop, the data that
measures them, the queries, a report script, and the alerts. The Trunk object writes the data
to Workers Analytics Engine (`trunk/metrics.rs`). [`ops.md`](ops.md) tells how to start the
metrics, and what the test on Cloudflare showed.

The SLOs and the queries are in [`slo.json`](slo.json). The report script
([`slo_report.py`](slo_report.py)) and the alert Worker ([`../nerudo-slo`](../nerudo-slo))
read that file. Change an SLO there, not in the code.

## The SLOs

Each SLO is a ratio: good events divided by all events, over 28 days. A ratio gives an error
budget, and the alerts use the rate at which the budget is used (the burn rate). Thus the
targets for "p95 queue age" and "p95 landing latency" are written as "99% of events are
within a limit". The report also shows the percentiles (see [The report](#the-report)).

| SLO | Event | Good event | Objective |
|---|---|---|---|
| `queue_age` | A round of the landing loop | The oldest queued change joined the queue 30 s ago or less | 99% |
| `landing_latency` | A landing | The change landed 10 s or less after it joined the queue | 99% |
| `loop_errors` | A round, or an alarm that ended in an error (not of CI) | The round ended with no error | 99.9% |
| `ci_availability` | A round that called the CI Worker, or an alarm error of cause `ci` | The CI Worker gave a verdict: passed, failed, or trunk fails | 99.5% |

### Why these limits

The limits come from the measurements on Cloudflare with Artifacts:

| SLO | Measured | Source |
|---|---|---|
| `queue_age` 30 s | A burst of 512 changes drained in 12 to 30 s (cap 16 and 64, after the gate fix). At 3 changes each second on one trunk, p99 latency was 7.8 to 10.0 s. At 1 change each second or less, p99 was 1.1 to 1.6 s. | [`stages.md`](stages.md), [`repos.md`](repos.md) |
| `landing_latency` 10 s | The same. In the test of this document, the client sent bursts of 128 and 256 changes at once. They landed with a mean of 5.1 s and a maximum of 9.1 s. | [`repos.md`](repos.md), [`ops.md`](ops.md) |
| `loop_errors` 99.9% | 126,990 changes on 200 trunks: no Worker error. The errors in the fault tests came only from the faults (a killed process, a git host that resets connections). | [`repos.md`](repos.md), [`../faults/README.md`](../faults/README.md) |
| `ci_availability` 99.5% | Before the backoff of [`resilience.md`](resilience.md), each trunk failed one round each second when CI was down (`ci_down`: 20 s on 8 trunks gave 160 CI requests). Then an outage of 1 minute on 10 trunks gave 600 bad events. With the backoff, the same 20 s on 8 trunks gave 40 CI requests. | [`../faults/README.md`](../faults/README.md) |

Thus a trunk that is saturated (3 changes each second) is near the 10 s limit. A trunk that
waits longer than 30 s has a problem. Examples are a git host that fails, a CI outage, or a
queue that is too long for one trunk.

## The data points

The Trunk object writes one data point for each round of the landing loop, and one for each
alarm that ends in an error. The dataset is `nerudo_metrics` (binding `METRICS`). The columns
are fixed by position. A new column goes at the end. Do not move a column.

| Column | Round (`blob1 = 'round'`) | Alarm error (`blob1 = 'alarm_error'`) |
|---|---|---|
| `index1` | The trunk name | The trunk name |
| `blob2` | The type check of the batch: `''` (none ran), `passed`, `failed` (a change failed it), `trunk_red` (trunk itself fails it). `unavailable` is kept for a CI outage that does not end the alarm. | The cause: `ci` (the error starts with "CI Worker"), `git` (`transport:`, `protocol:`, `corrupt:`, `missing object:`) or `other` |
| `blob3` | Empty | The first 256 bytes of the error |
| `double1` | Changes on the queue at the start of the round | 0 |
| `double2` | Age of the oldest queued change, ms | 0 |
| `double3` | Changes in the batch | 0 |
| `double4` | Changes that landed | 0 |
| `double5` | Sum of the latencies of the landed changes, ms | 0 |
| `double6` | The longest latency of a landed change, ms | 0 |
| `double7`, `double8`, `double9` | Landed changes with a latency of 2 s, 10 s, 30 s or less | 0 |
| `double10`, `double11`, `double12` | Git requests, bytes received from git hosts, bytes of the pushed pack | 0 |
| `double13`, `double14`, `double15`, `double16` | Fetch, push, CI and round time, ms | 0 |
| `double17` | Fetches that failed and wait to try again (`fetch_retry`) | 0 |
| `double18`, `double19`, `double20` | Ejected for a conflict; for a failed check or CI (`check_failed`, `ci_failed`, `ci_crashed`); for another reason | 0 |

The `timestamp` column is the time of the write. The field names of a round are in
`trunk/timings.rs` (`Round`); `GET /v1/trunks/{trunk}/timings` and the `nerudo.round` console
line show the same rounds.

### What the latency counts

- Queue age and landing latency count from the time the change joined the queue. That is
  the time of `POST land`. For a change that waited for a person, it is the time that the
  change returned to the queue (after an approval, an allowance or a pick: `queued_ms` of
  the change). Thus the time of a person is
  not in the SLO. The task asked for "submitted to landed". This is different on purpose.
- The time that an admin pauses the trunk counts. A paused trunk with queued changes uses
  the `queue_age` budget when it resumes.
- The time that a change waits to fetch again (`fetch_retry`, up to approximately 7 minutes)
  counts. That is the purpose: a git host that fails must show.
- A landing that the loop recovers after a crash (the push was done, the record was not)
  is not in a round. It is not in `landing_latency`.
- The clock of a Worker moves only at I/O (see [`stages.md`](stages.md)). The latencies are
  times between I/O, so this has almost no effect on them.

### Sampling

Analytics Engine samples. Each stored row has `_sample_interval`: the number of written
data points that it stands for. Each query must weight with it: a count is
`sum(_sample_interval)`, a sum is `sum(_sample_interval * doubleN)`, and a percentile is
`quantileExactWeighted(q)(doubleN, _sample_interval)`. The results are estimates.

In the test on Cloudflare (2026-10-12), the trunks wrote approximately one round each
second. Analytics Engine kept 44 of 75 round data points, with `_sample_interval` up to 3:

| | Written | Estimated from Analytics Engine |
|---|---|---|
| Rounds | 75 | 74 |
| Landings | 1,037 | 1,002 (−3.4%) |
| Landings of one trunk | 128 | 112 to 114 (−11%) |

A maximum (`max(double2)`) can miss the row that had it. The sampling is per `index1`, so a
trunk with few rounds keeps all of them.

## The queries

All queries go to the Analytics Engine SQL API:
`POST https://api.cloudflare.com/client/v4/accounts/{account}/analytics_engine/sql`, with a
token that has the permission Account Analytics Read. Add `FORMAT JSON` to get JSON.

The SLOs, for the last 28 days (the same query that `slo_report.py` and the alert Worker
make):

```sql
SELECT
  sumIf(_sample_interval, blob1 = 'round' AND double2 <= 30000) AS queue_age_good,
  sumIf(_sample_interval, blob1 = 'round') AS queue_age_total,
  sumIf(_sample_interval * double8, blob1 = 'round') AS landing_latency_good,
  sumIf(_sample_interval * double4, blob1 = 'round') AS landing_latency_total,
  sumIf(_sample_interval, blob1 = 'round') AS loop_errors_good,
  sumIf(_sample_interval, blob1 = 'round' OR (blob1 = 'alarm_error' AND blob2 != 'ci')) AS loop_errors_total,
  sumIf(_sample_interval, blob1 = 'round' AND blob2 IN ('passed', 'failed', 'trunk_red')) AS ci_good,
  sumIf(_sample_interval, (blob1 = 'round' AND blob2 IN ('passed', 'failed', 'trunk_red', 'unavailable'))
                          OR (blob1 = 'alarm_error' AND blob2 = 'ci')) AS ci_total
FROM nerudo_metrics
WHERE timestamp > NOW() - INTERVAL '40320' MINUTE
```

The SLI is good ÷ total. The burn rate is (1 − SLI) ÷ (1 − objective). A burn rate of 1 uses
the full budget in the window.

Percentiles of the queue and of the age of the oldest change:

```sql
SELECT
  quantileExactWeighted(0.95)(double1, _sample_interval) AS queued_p95,
  quantileExactWeighted(0.50)(double2, _sample_interval) AS age_p50_ms,
  quantileExactWeighted(0.95)(double2, _sample_interval) AS age_p95_ms,
  quantileExactWeighted(0.99)(double2, _sample_interval) AS age_p99_ms
FROM nerudo_metrics
WHERE blob1 = 'round' AND timestamp > NOW() - INTERVAL '1' DAY
```

The latency of each landing is not in the data. A round has the sum, the maximum and the
counts within 2, 10 and 30 s. An upper bound of the p95 landing latency weights the maximum of
each round with its landings (the weight must be an integer):

```sql
SELECT quantileExactWeighted(0.95)(double6, _sample_interval * toUInt32(double4)) AS wait_p95_upper_ms
FROM nerudo_metrics
WHERE blob1 = 'round' AND double4 > 0 AND timestamp > NOW() - INTERVAL '1' DAY
```

The mean landing latency is `sum(_sample_interval * double5) / sum(_sample_interval * double4)`.

The trunks with the oldest queue, and their errors:

```sql
SELECT index1 AS trunk,
  sumIf(_sample_interval, blob1 = 'round') AS rounds,
  sumIf(_sample_interval * double4, blob1 = 'round') AS landed,
  max(double2) AS age_max_ms,
  sumIf(_sample_interval, blob1 = 'alarm_error') AS errors
FROM nerudo_metrics
WHERE timestamp > NOW() - INTERVAL '1' DAY
GROUP BY trunk ORDER BY age_max_ms DESC LIMIT 10
```

The alarm errors by cause, with the last error:

```sql
SELECT blob2 AS cause, sum(_sample_interval) AS n, argMax(blob3, timestamp) AS last
FROM nerudo_metrics
WHERE blob1 = 'alarm_error' AND timestamp > NOW() - INTERVAL '1' DAY
GROUP BY cause ORDER BY n DESC
```

To see only some trunks, add `AND startsWith(index1, 'prefix')`.

## The report

```sh
CLOUDFLARE_ACCOUNT_ID=<account> CLOUDFLARE_API_TOKEN=<token> \
  python3 -I cf/scale/slo_report.py [--dataset nerudo_metrics] [--window 28d] [--trunk PREFIX] [--json FILE]
```

The script reads the account id and the token only from the environment. Use a token with
only Account Analytics Read. It prints each SLO, the percentiles, and the git and stage
totals. It also prints the ejections, the alarm errors by cause, and the 10 trunks with the
oldest queue. This is
the report of the test on Cloudflare ([`ops.md`](ops.md)), on the dataset of the test Worker:

```text
Dataset nerudo_scale_ops, last 180 minutes, trunks ops-*

SLO               objective       SLI      good     total   burn  status
queue_age           99.000%  100.000%        74        74   0.00  met
landing_latency     99.000%  100.000%      1002      1002   0.00  met
loop_errors         99.900%   98.667%        74        75  13.33  MISSED
ci_availability     99.500%    3.053%         4       131 193.89  MISSED

Rounds 74, landings 1002, mean batch 13.5
Landing latency: mean 5.10 s, p95 at most 8.52 s; within 2 s 12.575%, 10 s 100.000%, 30 s 100.000%
Queue: length p95 160, max 192; age of the oldest change p50 4.23 s, p95 7.88 s, p99 8.34 s, max 8.34 s
Git: 186 requests (0.19 for each landing), 0.27 MB received, 3.64 MB pushed, 0 fetch retries
Time in rounds: fetch 9.9 s, push 42.7 s, CI 0.1 s, all 52.7 s
Ejected: 0 conflict, 1 failed a check or CI, 0 other

Alarm errors: ci 127 (last: CI Worker answered HTTP 503); other 1 (last: simulated crash after push)
```

The two missed SLOs are the two faults of the test: one simulated crash, and a CI outage of
approximately 3.5 minutes on one trunk.

## Alerts

### The rules

Two rules for each SLO, as in the Google SRE workbook (multiwindow, multi-burn-rate). A rule
fires when the burn rate is above its limit in the long window and in the short window. The
short window stops the alert soon after the problem stops.

| Rule | Long window | Short window | Burn rate | Meaning |
|---|---|---|---|---|
| `page` | 60 min | 5 min | 14.4 | At this rate, 1 hour uses approximately 2% of the 28-day budget |
| `ticket` | 6 h | 30 min | 6 | At this rate, 6 hours use approximately 5% of the 28-day budget |

A window with fewer than 20 events (`min_events`) does not fire. Thus a quiet trunk with one
error does not page.

### The alert Worker

[`../nerudo-slo`](../nerudo-slo) is a Worker with a cron trigger (every 5 minutes). It runs
the SLO query for each window of the rules (4 queries), and it posts one message to a webhook
when a rule fires: `{"text": ..., "alerts": [...]}`. The text suits a Slack or Discord
incoming webhook. While a rule fires, each run posts again. The Worker keeps no state.

| Setting | Kind | Value |
|---|---|---|
| `ACCOUNT_ID` | secret or var | The account of the dataset |
| `SLO_API_TOKEN` | secret | An API token with Account Analytics Read only |
| `WEBHOOK_URL` | secret | Where the alerts go |
| `DATASET` | var | `nerudo_metrics` |

It has no route (`workers_dev = false`). To deploy it (this work did not deploy it):

```sh
cd cf/nerudo-slo
wrangler secret put ACCOUNT_ID
wrangler secret put SLO_API_TOKEN
wrangler secret put WEBHOOK_URL
wrangler deploy
```

Cost: 4 queries each 5 minutes are approximately 35,000 queries each month. The Workers Paid
plan includes 1 million read queries each month. Billing of Analytics Engine is not active on
2026-10-12.

To test it without a deploy: `node --test cf/nerudo-slo/test/slo.test.mjs` tests the rules. `wrangler dev
--test-scheduled` runs it on this computer; then
`curl "http://127.0.0.1:<port>/cdn-cgi/handler/scheduled?cron=*/5+*+*+*+*"` runs the cron once.
[`ops.md`](ops.md) gives the result of that test against the test dataset.

### Cloudflare Notifications

Cloudflare Notifications send to email, PagerDuty or a webhook. We did not find an alert type
that runs a query on an Analytics Engine dataset. Thus the SLO alerts need the alert Worker.
A Notification for the errors of the Worker itself, to the same webhook, is a useful second
alert. It also fires if the Trunk objects stop the writes of metrics. This work created no
notification policy.

## Limits

- An alarm that does not run writes nothing. If a trunk has queued changes but no alarm, the
  SLOs do not show it. `GET .../stats` shows `queued`, but no metric shows a trunk that is
  silent.
- A request error (for example HTTP 500 on `POST land`) is not in these SLOs. The Worker logs
  it (`trunk {name}: ...`), and the Workers analytics of Cloudflare count requests.
- The rules count events, not time. One trunk that fails each second for 1 minute and 60
  trunks that fail once give the same count.
- When CI is down, the landing loop now waits longer between tries
  ([`resilience.md`](resilience.md)). Thus an outage gives fewer bad events, and the
  `ci_availability` SLI of an outage increases. The `unavailable` value of `blob2` is ready
  for a round that waits for CI and does not end the alarm.
