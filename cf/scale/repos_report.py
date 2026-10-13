#!/usr/bin/env python3
"""Summarize repos-load runs (cf/scale/repos.md).

  python3 -I cf/scale/repos_report.py OUT.json RAW1.json [RAW2.json ...]

Each RAW file is the output of `repos-load run`. OUT gets one summary per run, with a row
per trunk, and the script prints the tables of repos.md. Times on the server (submitted_ms,
landed_at_ms) give latency and landing rates; times on this Mac give the offered rate and
how late each change was sent.
"""
import json
import sys
from collections import Counter, defaultdict

# Landings in the first WARMUP seconds after the first submission are not in the steady rate.
WARMUP_S = 30


def pct(xs, p):
    if not xs:
        return None
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(round(p / 100 * (len(xs) - 1))))]


def dist(xs, scale=1.0, nd=0):
    if not xs:
        return None
    return {k: round(pct(xs, p) / scale, nd) for k, p in (("p50", 50), ("p95", 95), ("p99", 99), ("max", 100))}


def summarize(path):
    d = json.load(open(path))
    k, rate, secs = d["k"], d["rate_per_repo"], d["secs"]
    changes = d["changes"]
    sent = [c for c in changes if "start" in c]
    ok = [c for c in changes if "submitted_ms" in c]
    errors = Counter(c["error"] for c in changes if "error" in c)
    land = {}  # (repo, cid) -> (seq, batch, landed_at_ms)
    log_errors = 0
    for r, entries in enumerate(d["logs"]):
        for e in entries:
            if isinstance(e, dict):
                log_errors += 1
                continue
            land[(r, e[0])] = (e[1], e[2], e[3])
    lat, lat_repo, landed_at, batches = [], defaultdict(list), defaultdict(list), defaultdict(Counter)
    for c in ok:
        e = land.get((c["repo"], c["cid"]))
        if e:
            ms = e[2] - c["submitted_ms"]
            lat.append(ms)
            lat_repo[c["repo"]].append(ms)
            landed_at[c["repo"]].append(e[2])
            batches[c["repo"]][e[1]] += 1
    not_landed = Counter()
    for c in ok:
        if (c["repo"], c["cid"]) not in land:
            rc = c.get("receipt", {})
            kind = (rc.get("reason") or {}).get("kind") if isinstance(rc.get("reason"), dict) else None
            not_landed[f"{rc.get('state', 'unknown')}:{kind}"] += 1
    sub = [c["submitted_ms"] for c in ok]
    s0, s1 = (min(sub), max(sub)) if sub else (0, 0)
    w0 = s0 + WARMUP_S * 1000
    window_s = max(1e-9, (s1 - w0) / 1000)
    steady = {r: sum(1 for t in ts if w0 <= t <= s1) / window_s for r, ts in landed_at.items()}
    agg_rate = sum(steady.values())
    submit_rate = sum(1 for t in sub if w0 <= t <= s1) / window_s
    # Latency by minute of submission: does it grow while the load holds?
    by_min = defaultdict(list)
    for c in ok:
        e = land.get((c["repo"], c["cid"]))
        if e:
            by_min[int((c["submitted_ms"] - s0) // 60000)].append(e[2] - c["submitted_ms"])
    pre, post = d["pre"], d["polls"][-1]["repos"]
    delta = lambda key: sum(p.get(key, 0) - q.get(key, 0) for p, q in zip(post, pre))
    landed_delta = delta("landed")
    sizes = [n for r in batches.values() for n in r.values()]
    per_repo = []
    for r in range(k):
        lr = lat_repo.get(r, [])
        per_repo.append({
            "trunk": d["trunks"][r],
            "accepted": sum(1 for c in ok if c["repo"] == r),
            "landed": len(lr),
            "steady_landings_per_s": round(steady.get(r, 0.0), 4),
            "latency_ms": dist(lr),
            "batches": len(batches.get(r, {})),
            "git_requests_per_landing": round((post[r].get("git_requests", 0) - pre[r].get("git_requests", 0)) / max(1, post[r].get("landed", 0) - pre[r].get("landed", 0)), 2),
            "fetch_retries": post[r].get("fetch_retries", 0) - pre[r].get("fetch_retries", 0),
            "ejected": post[r].get("ejected", 0) - pre[r].get("ejected", 0),
            "wasm_bytes": post[r].get("wasm_bytes"),
            "landed_total_after": post[r].get("landed"),
        })
    repo_rates = [p["steady_landings_per_s"] for p in per_repo]
    repo_p95 = [p["latency_ms"]["p95"] for p in per_repo if p["latency_ms"]]
    polls = len(d["polls"]) * k + k
    return {
        "tag": d["tag"], "k": k, "offered_per_repo_per_s": rate, "secs": secs, "threads": d["threads"],
        "aborted": d["aborted"],
        "offered": d["offered"], "offered_per_s": round(d["offered"] / secs, 3),
        "sent": len(sent), "accepted": len(ok), "client_errors": dict(errors),
        "error_rate": round((len(changes) - len(ok)) / max(1, len(changes)), 4),
        "send_lag_ms": dist([c["start"] - c["due"] for c in sent]),
        "push_ms": dist([c["push_ms"] for c in sent if "push_ms" in c]),
        "land_call_ms": dist([c["land_ms"] for c in sent if "land_ms" in c]),
        "achieved_submit_per_s": round(submit_rate, 3),
        "landed": len(lat), "not_landed": dict(not_landed), "log_errors": log_errors,
        "steady_window_s": round(window_s, 1),
        "steady_landings_per_s": round(agg_rate, 3),
        "per_repo_landings_per_s": {"min": min(repo_rates), "median": pct(repo_rates, 50), "max": max(repo_rates)} if repo_rates else None,
        "latency_ms": dist(lat),
        "latency_p95_by_minute_ms": {m: round(pct(v, 95)) for m, v in sorted(by_min.items())},
        "latency_p50_by_minute_ms": {m: round(pct(v, 50)) for m, v in sorted(by_min.items())},
        "per_repo_latency_p95_ms": {"min": min(repo_p95), "median": pct(repo_p95, 50), "max": max(repo_p95)} if repo_p95 else None,
        "batch_size": {"mean": round(sum(sizes) / len(sizes), 2), "max": max(sizes), "hist": dict(sorted(Counter(sizes).items()))} if sizes else None,
        "git_requests_per_landing": round(delta("git_requests") / max(1, landed_delta), 2),
        "fetch_retries": delta("fetch_retries"), "ejected": delta("ejected"), "external_moves": delta("external_moves"),
        "drain_s": round((d["drained_at"] - d["sent_end"]) / 1000, 1) if d["drained_at"] else None,
        "max_wasm_bytes": max((p.get("wasm_bytes") or 0) for p in post),
        "max_backlog": max(p["backlog"] for p in d["polls"]),
        "health": [h["status"] for h in d["health"]],
        "requests": {
            # Worker (and Durable Object) requests this client made; alarms are extra DO work.
            "worker_land": len([c for c in sent if "land_ms" in c]),
            "worker_stats": polls,
            "artifacts_client": 2 * len(sent) + 0,
            "artifacts_server": delta("git_requests"),
            "alarms_at_least": delta("batches"),
        },
        "per_repo": per_repo,
    }


def table(rows):
    print("| run | K | offered/s (per trunk) | accepted | errors | landed | steady landings/s | per trunk min-max | latency p50/p95/p99 ms | batch mean/max | git req/landing | drain s |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|")
    for s in rows:
        pr = s["per_repo_landings_per_s"] or {}
        lat = s["latency_ms"] or {}
        b = s["batch_size"] or {}
        print(f"| {s['tag']} | {s['k']} | {s['offered_per_s']} ({s['offered_per_repo_per_s']}) | {s['accepted']}/{s['offered']} | "
              f"{sum(s['client_errors'].values())} | {s['landed']} | {s['steady_landings_per_s']} | {pr.get('min')}-{pr.get('max')} | "
              f"{lat.get('p50')}/{lat.get('p95')}/{lat.get('p99')} | {b.get('mean')}/{b.get('max')} | {s['git_requests_per_landing']} | {s['drain_s']} |")


def main():
    out, raws = sys.argv[1], sys.argv[2:]
    rows = [summarize(p) for p in raws]
    try:
        old = json.load(open(out))
    except (OSError, ValueError):
        old = {}
    runs = {r["tag"]: r for r in old.get("runs", [])}
    for r in rows:
        runs[r["tag"]] = r
    old["runs"] = sorted(runs.values(), key=lambda r: (r["k"], r["offered_per_repo_per_s"], r["tag"]))
    with open(out, "w") as fh:
        json.dump(old, fh, indent=1)
        fh.write("\n")
    table(old["runs"])


if __name__ == "__main__":
    main()
