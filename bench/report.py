#!/usr/bin/env python3
"""Aggregate results/runs.jsonl into the data blob the report page renders."""
import json
import os
import statistics
import sys
from collections import defaultdict

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# label -> (panel, model, display name). Model picks color + marker; it follows the
# coordination model, so "push to trunk" is the same color on every backend.
SERIES = {
    "git-strict": ("local", "strict", "Rebase, retest, push"),
    "git-lax": ("local", "merge", "Merge without retest"),
    "seq": ("local", "seq", "Sequencer"),
    "jj-clones": ("jj", "strict", "jj clones: rebase, retest, push"),
    "jj-workspaces": ("jj", "jjws", "jj workspaces (one shared repo)"),
    "forgejo-strict": ("forgejo", "strict", "Push to main"),
    "forgejo-pr": ("forgejo", "merge", "PR + merge API"),
    "seq-forgejo-mirror": ("forgejo", "seq", "Sequencer, Forgejo as mirror"),
    "cf-seq": ("forgejo", "cf", "Sequencer on a Durable Object (local workerd)"),
    "cf-artifacts": ("cloudflare", "cf", "Sequencer on a Durable Object, Artifacts trunk and forks"),
    "github-strict": ("github", "strict", "Push to main"),
    "github-pr": ("github", "merge", "PR + merge API"),
    "seq-github-mirror": ("github", "seq", "Sequencer, GitHub as mirror"),
}
# jj panel borrows the local sequencer as its reference line; the live Cloudflare panel
# borrows the same Durable Object code under local workerd.
PANEL_REFS = {"jj": ["seq"], "cloudflare": ["cf-seq"]}
# label -> (panel, display name): in the correctness and run tables, not charted.
TABLE_ONLY = {"jj-workspaces-allow-backwards": ("jj", "jj workspaces, agent follows --allow-backwards hint"),
              "cf-seq-pretest": ("forgejo", "Durable Object sequencer, agents also pre-test"),
              "cf-artifacts-pretest": ("cloudflare", "Durable Object sequencer on Artifacts, agents also pre-test")}
HAZARDS = {"jj-workspaces-allow-backwards"}  # shows what a tool's hint leads to; not a careful configuration

METRICS = ["throughput_per_min", "ci_runs_per_landing", "rejects_per_landing", "lat_p50", "lat_p95", "git_http_per_landing"]


def mean(xs):
    xs = [x for x in xs if x is not None]
    return statistics.mean(xs) if xs else None


def main(path):
    runs = [json.loads(line) for line in open(path) if line.strip()]
    runs = [r for r in runs if not r.get("failed")]
    groups = defaultdict(list)
    for r in runs:
        groups[(r["label"], r["n"])].append(r)

    points = defaultdict(list)
    for (label, n), rs in sorted(groups.items(), key=lambda kv: (kv[0][0], kv[0][1])):
        p = {"n": n, "reps": len(rs)}
        for m in METRICS:
            p[m] = mean(r.get(m) for r in rs)
            p[m + "_reps"] = [r.get(m) for r in rs]
        p["landed"] = sum(r["landed"] for r in rs)
        p["expected"] = sum(r["expected"] for r in rs)
        if rs[0].get("seq"):
            sizes = [s for r in rs for s in r["seq"]["batch_sizes"]]
            p["avg_batch"] = statistics.mean(sizes) if sizes else None
            p["max_batch"] = max(sizes) if sizes else None
        lags = [r.get("mirror_lag_p50") for r in rs if r.get("mirror_lag_p50") is not None]
        if lags:
            p["mirror_lag_p50"] = mean(lags)
            p["mirror_lag_p95"] = mean(r.get("mirror_lag_p95") for r in rs)
        points[label].append(p)

    correctness = []
    for label, (panel, name) in [(k, (v[0], v[2])) for k, v in SERIES.items()] + list(TABLE_ONLY.items()):
        rs = [r for r in runs if r["label"] == label]
        if not rs:
            continue
        correctness.append({
            "label": label,
            "panel": panel,
            "name": name,
            "runs": len(rs),
            "max_n": max(r["n"] for r in rs),
            "landed": sum(r["landed"] for r in rs),
            "expected": sum(r["expected"] for r in rs),
            "lost": sum(r.get("lost_changes", 0) for r in rs),
            "dups": sum(r.get("dup_migrations", 0) for r in rs),
            "broken": sum(r.get("broken_commits", 0) for r in rs),
            "trunk_commits": sum(r.get("trunk_commits", 0) for r in rs),
            "timeouts": sum(1 for r in rs if r.get("timed_out")),
            "sideways": sum(r.get("sideways_pushes", 0) for r in rs),
            "fetch_errors": sum(r.get("fetch_errors", 0) for r in rs),
            "stale": sum(r.get("jj_stale", 0) for r in rs),
            "pushes": sum(r.get("push_attempts", 0) for r in rs),
            "rejects": sum(r.get("push_rejects", 0) for r in rs),
            "ci_runs": sum(r.get("ci_runs", 0) + (r.get("seq") or {}).get("ci_runs", 0) for r in rs),
            "api_calls": sum(r.get("api_calls", 0) for r in rs),
            "mirror_behind": sum(r.get("mirror_behind") or 0 for r in rs),
            "hazard": label in HAZARDS,
        })

    table = [{k: r.get(k) for k in (
        "label", "n", "k", "rep", "wall_s", "landed", "expected", "throughput_per_min",
        "ci_runs_per_landing", "rejects_per_landing", "lat_p50", "lat_p95", "lost_changes",
        "dup_migrations", "broken_commits", "fetch_errors", "timed_out", "mirror_lag_p50", "git_http_per_landing")}
        for r in runs]

    review = None
    rpath = os.path.join(ROOT, "results", "review", "summary.json")
    if os.path.exists(rpath):
        with open(rpath) as fh:
            review = json.load(fh)
        p1 = os.path.join(ROOT, "results", "review", "summary-pass1.json")
        if os.path.exists(p1):
            with open(p1) as fh:
                first = json.load(fh)["reps"][0]
            review["pass1"] = {"reviewers": [{k: r[k] for k in ("name", "clean_stopped", "clean", "defects_stopped", "defects")}
                                             for r in first["reviewers"]],
                               "policies": [{k: p[k] for k in ("name", "defects_escaped", "clean_landed")}
                                            for p in first["policies"]]}
    return {
        "review": review,
        "series": {k: {"panel": v[0], "model": v[1], "name": v[2], "points": points.get(k, [])}
                   for k, v in SERIES.items() if points.get(k)},
        "panel_refs": PANEL_REFS,
        "correctness": correctness,
        "runs": table,
        "ci_seconds": sorted({r["ci_seconds"] for r in runs}),
        "n_runs": len(runs),
    }


if __name__ == "__main__":
    src = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "results", "runs.jsonl")
    data = main(src)
    tmpl = open(os.path.join(ROOT, "bench", "report_template.html")).read()
    out = os.path.join(ROOT, "results", "report.html")
    with open(out, "w") as fh:
        fh.write(tmpl.replace("/*__DATA__*/null", json.dumps(data, separators=(",", ":"))))
    print(out, f"{data['n_runs']} runs, {len(data['series'])} series")
