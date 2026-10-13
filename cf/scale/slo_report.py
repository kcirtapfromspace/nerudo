#!/usr/bin/env python3
"""The service levels of the landing loop, from the Analytics Engine dataset that the Trunk
objects write (cf/scale/slo.md, slo.json).

  CLOUDFLARE_ACCOUNT_ID=... CLOUDFLARE_API_TOKEN=... python3 -I cf/scale/slo_report.py
      [--dataset nerudo_metrics] [--window 28d] [--trunk PREFIX] [--json FILE]

The token needs the permission Account > Account Analytics > Read. The script reads the
account id and the token only from the environment. It does not write them anywhere.

--window is a number and a unit: m (minutes), h (hours) or d (days). --trunk keeps only the
trunks whose name starts with PREFIX.
"""
import argparse
import json
import os
import re
import sys
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
SLO = json.load(open(os.path.join(HERE, "slo.json")))
API = "https://api.cloudflare.com/client/v4/accounts/{account}/analytics_engine/sql"

# The doubles of a round (trunk/metrics.rs, round_doubles). The columns are fixed by position.
ROUND = "blob1 = 'round'"
STATS = {
    "rounds": "sum(_sample_interval)",
    "landed": "sum(_sample_interval * double4)",
    "batched": "sum(_sample_interval * double3)",
    "wait_sum_ms": "sum(_sample_interval * double5)",
    "within_2s": "sum(_sample_interval * double7)",
    "within_10s": "sum(_sample_interval * double8)",
    "within_30s": "sum(_sample_interval * double9)",
    "queued_p95": "quantileExactWeighted(0.95)(double1, _sample_interval)",
    "queued_max": "max(double1)",
    "age_p50_ms": "quantileExactWeighted(0.50)(double2, _sample_interval)",
    "age_p95_ms": "quantileExactWeighted(0.95)(double2, _sample_interval)",
    "age_p99_ms": "quantileExactWeighted(0.99)(double2, _sample_interval)",
    "age_max_ms": "max(double2)",
    "git_requests": "sum(_sample_interval * double10)",
    "rx_bytes": "sum(_sample_interval * double11)",
    "pack_bytes": "sum(_sample_interval * double12)",
    "fetch_ms": "sum(_sample_interval * double13)",
    "push_ms": "sum(_sample_interval * double14)",
    "ci_ms": "sum(_sample_interval * double15)",
    "round_ms": "sum(_sample_interval * double16)",
    "fetch_retries": "sum(_sample_interval * double17)",
    "ejected_conflict": "sum(_sample_interval * double18)",
    "ejected_failed": "sum(_sample_interval * double19)",
    "ejected_other": "sum(_sample_interval * double20)",
}
# The latency of one landing is not in the data: a round has the sum, the max and the counts
# within 2, 10 and 30 s. The max of the round, weighted by its landings, is an upper bound.
WAIT_P95 = ("quantileExactWeighted(0.95)(double6, _sample_interval * toUInt32(double4))", f"{ROUND} AND double4 > 0")


def window_minutes(text):
    m = re.fullmatch(r"(\d+)([mhd])", text)
    if not m:
        sys.exit(f"slo_report: --window {text!r}: use a number and m, h or d (for example 28d)")
    return int(m.group(1)) * {"m": 1, "h": 60, "d": 1440}[m.group(2)]


def where(minutes, prefix, extra=""):
    w = [f"timestamp > NOW() - INTERVAL '{minutes}' MINUTE"]
    if prefix:
        if not re.fullmatch(r"[A-Za-z0-9._/-]+", prefix):
            sys.exit("slo_report: --trunk: letters, digits and . _ / - only")
        w.append(f"startsWith(index1, '{prefix}')")
    if extra:
        w.append(extra)
    return " AND ".join(w)


def slo_sql(dataset, minutes, prefix):
    """One query for all SLOs: good_i and total_i for SLO i. The alert Worker builds the same."""
    cols = ", ".join(f"{s['good']} AS good_{i}, {s['total']} AS total_{i}" for i, s in enumerate(SLO["slos"]))
    return f"SELECT {cols} FROM {dataset} WHERE {where(minutes, prefix)}"


def query(sql):
    account, token = os.environ.get("CLOUDFLARE_ACCOUNT_ID"), os.environ.get("CLOUDFLARE_API_TOKEN")
    if not account or not token:
        sys.exit("slo_report: set CLOUDFLARE_ACCOUNT_ID and CLOUDFLARE_API_TOKEN (Account Analytics Read)")
    req = urllib.request.Request(API.format(account=account), data=(sql + " FORMAT JSON").encode(), method="POST",
                                 headers={"Authorization": f"Bearer {token}", "Content-Type": "text/plain"})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return json.loads(r.read())["data"]
    except urllib.error.HTTPError as e:
        sys.exit(f"slo_report: HTTP {e.code}: {e.read().decode()[:300]}\n{sql}")


def num(v):
    return float(v) if v not in (None, "", "nan") else 0.0


def report(dataset, minutes, prefix):
    out = {"dataset": dataset, "window_minutes": minutes, "trunk_prefix": prefix, "slos": []}
    row = (query(slo_sql(dataset, minutes, prefix)) or [{}])[0]
    for i, s in enumerate(SLO["slos"]):
        good, total = num(row.get(f"good_{i}")), num(row.get(f"total_{i}"))
        sli = good / total if total else None
        # The burn rate: the error rate over the error budget. 1 uses the budget in the window.
        burn = (1 - sli) / (1 - s["objective"]) if sli is not None else None
        out["slos"].append({"name": s["name"], "objective": s["objective"], "good": good, "total": total,
                            "sli": sli, "burn": burn, "met": sli is None or sli >= s["objective"]})
    cols = ", ".join(f"{v} AS {k}" for k, v in STATS.items())
    out["rounds"] = {k: num(v) for k, v in ((query(f"SELECT {cols} FROM {dataset} WHERE {where(minutes, prefix, ROUND)}")
                                            or [{}])[0]).items()}
    p95 = query(f"SELECT {WAIT_P95[0]} AS p95 FROM {dataset} WHERE {where(minutes, prefix, WAIT_P95[1])}")
    out["rounds"]["wait_p95_upper_ms"] = num((p95 or [{}])[0].get("p95"))
    errors = where(minutes, prefix, "blob1 = 'alarm_error'")
    out["errors"] = [{"cause": r["cause"], "count": num(r["n"]), "last": r["last"]} for r in query(
        f"SELECT blob2 AS cause, sum(_sample_interval) AS n, argMax(blob3, timestamp) AS last FROM {dataset} "
        f"WHERE {errors} GROUP BY cause ORDER BY n DESC")]
    out["trunks"] = [{k: (v if k == "trunk" else num(v)) for k, v in r.items()} for r in query(
        f"SELECT index1 AS trunk, sumIf(_sample_interval, {ROUND}) AS rounds, sumIf(_sample_interval * double4, {ROUND}) AS landed, "
        f"max(double2) AS age_max_ms, max(double6) AS wait_max_ms, sumIf(_sample_interval, blob1 = 'alarm_error') AS errors "
        f"FROM {dataset} WHERE {where(minutes, prefix)} GROUP BY trunk ORDER BY age_max_ms DESC LIMIT 10")]
    return out


def pct(x):
    return "no data" if x is None else f"{100 * x:.3f}%"


def show(r):
    print(f"Dataset {r['dataset']}, last {r['window_minutes']} minutes" + (f", trunks {r['trunk_prefix']}*" if r["trunk_prefix"] else ""))
    print()
    print(f"{'SLO':<17} {'objective':>9} {'SLI':>9} {'good':>9} {'total':>9} {'burn':>6}  status")
    for s in r["slos"]:
        burn = "" if s["burn"] is None else f"{s['burn']:.2f}"
        status = "no data" if s["sli"] is None else ("met" if s["met"] else "MISSED")
        print(f"{s['name']:<17} {pct(s['objective']):>9} {pct(s['sli']):>9} {s['good']:>9.0f} {s['total']:>9.0f} {burn:>6}  {status}")
    x = r["rounds"]
    landed, rounds = x.get("landed", 0), x.get("rounds", 0)
    print()
    print(f"Rounds {rounds:.0f}, landings {landed:.0f}, mean batch {landed / rounds if rounds else 0:.1f}")
    if landed:
        print(f"Landing latency: mean {x['wait_sum_ms'] / landed / 1000:.2f} s, p95 at most {x['wait_p95_upper_ms'] / 1000:.2f} s; "
              f"within 2 s {pct(x['within_2s'] / landed)}, 10 s {pct(x['within_10s'] / landed)}, 30 s {pct(x['within_30s'] / landed)}")
    if rounds:
        print(f"Queue: length p95 {x['queued_p95']:.0f}, max {x['queued_max']:.0f}; age of the oldest change p50 "
              f"{x['age_p50_ms'] / 1000:.2f} s, p95 {x['age_p95_ms'] / 1000:.2f} s, p99 {x['age_p99_ms'] / 1000:.2f} s, max {x['age_max_ms'] / 1000:.2f} s")
        print(f"Git: {x['git_requests']:.0f} requests ({x['git_requests'] / landed if landed else 0:.2f} for each landing), "
              f"{x['rx_bytes'] / 1e6:.2f} MB received, {x['pack_bytes'] / 1e6:.2f} MB pushed, {x['fetch_retries']:.0f} fetch retries")
        print(f"Time in rounds: fetch {x['fetch_ms'] / 1000:.1f} s, push {x['push_ms'] / 1000:.1f} s, CI {x['ci_ms'] / 1000:.1f} s, "
              f"all {x['round_ms'] / 1000:.1f} s")
        print(f"Ejected: {x['ejected_conflict']:.0f} conflict, {x['ejected_failed']:.0f} failed a check or CI, {x['ejected_other']:.0f} other")
    print()
    if r["errors"]:
        print("Alarm errors: " + "; ".join(f"{e['cause']} {e['count']:.0f} (last: {e['last'][:120]})" for e in r["errors"]))
    else:
        print("Alarm errors: none")
    if r["trunks"]:
        print()
        print(f"{'trunk (oldest queue first)':<40} {'rounds':>7} {'landed':>7} {'age max s':>9} {'wait max s':>10} {'errors':>6}")
        for t in r["trunks"]:
            print(f"{t['trunk'][:40]:<40} {t['rounds']:>7.0f} {t['landed']:>7.0f} {t['age_max_ms'] / 1000:>9.2f} "
                  f"{t['wait_max_ms'] / 1000:>10.2f} {t['errors']:>6.0f}")
    print()
    print("Counts are estimates: Analytics Engine samples, and each row counts _sample_interval times.")


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--dataset", default=SLO["dataset"])
    p.add_argument("--window", default="28d")
    p.add_argument("--trunk", default="", help="only the trunks whose name starts with this")
    p.add_argument("--json", help="also write the report to this file")
    args = p.parse_args()
    if not re.fullmatch(r"[A-Za-z0-9_]+", args.dataset):
        sys.exit("slo_report: --dataset: letters, digits and _ only")
    r = report(args.dataset, window_minutes(args.window), args.trunk)
    show(r)
    if args.json:
        with open(args.json, "w") as fh:
            json.dump(r, fh, indent=1)


if __name__ == "__main__":
    main()
