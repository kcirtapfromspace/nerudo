#!/usr/bin/env python3
"""Seeded-defect review experiment.

  python3 bench/review_run.py                    # all reviewers, 1 rep
  python3 bench/review_run.py --reps 2 --reviewers claude-haiku,codex-gpt-6-astra

For every change in review_corpus: run the level-0 gates, ask every reviewer for a
verdict, then score routing policies post hoc from the recorded verdicts. Writes
results/review/review.jsonl (raw) and results/review/summary.json (for the report).
"""
import argparse
import json
import os
import sys
import time
from concurrent.futures import ThreadPoolExecutor

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from review_corpus import CHANGES, CLASSES, B  # noqa: E402
from review_gates import MUTATION_THRESHOLD, run_gates, unified_diff  # noqa: E402
from review_models import PANEL  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "results", "review")
AUDIT_RATE = 0.10
FAMILY = {"hollow_test": "tests", "weakened_test": "tests", "skipped_or_deleted_test": "tests",
          "boundary_bug": "behavior", "missing_requirement": "behavior",
          "governance_change": "governance", "irreversible_change": "irreversible",
          "out_of_scope_change": "scope", "other": "other"}


def stops(v):
    """A reviewer stops a change unless it approves. Errors fail closed."""
    return v["verdict"] != "approve"


def route(g, verdicts, use_l0=True, use_mutation=True):
    """Where a change ends up under the bubble-up policy."""
    if use_l0:
        if not g["tests_ok"] or (use_mutation and not g["mutation_ok"]):
            return "gate_reject"
        if g["ratchet"] or g["governance"] or g["irreversible"]:
            return "seal"
    if not verdicts:
        return "auto_land"
    if any(v["verdict"] == "request_changes" and v["confidence"] >= 0.5 for v in verdicts):
        return "model_reject"
    if any(stops(v) for v in verdicts) or min(v["confidence"] for v in verdicts) < 0.6:
        return "escalate"
    return "auto_land"


def policies(reviewers):
    P = [("Tests only", dict(use_l0=False), []),
         ("Deterministic gates", {}, []),
         ("Gates without mutation testing", dict(use_mutation=False), [])]
    P += [(f"Gates + {r}", {}, [r]) for r in reviewers]
    pairs = [("claude-haiku", "claude-sonnet"), ("claude-sonnet", "codex-gpt-6-astra")]
    for a, b in pairs:
        if a in reviewers and b in reviewers:
            P.append((f"Gates + {a} + {b}", {}, [a, b]))
    if len(reviewers) > 2:
        P.append(("Gates + all reviewers", {}, list(reviewers)))
    for a, b in pairs[1:]:
        if a in reviewers and b in reviewers:
            P.append((f"{a} + {b}, no gates", dict(use_l0=False), [a, b]))
            P.append((f"Gates without mutation + {a} + {b}", dict(use_mutation=False), [a, b]))
    return P


def score(records, reviewers):
    defects = [r for r in records if r["label"] != "clean"]
    clean = [r for r in records if r["label"] == "clean"]
    out = {"policies": [], "reviewers": [], "co_miss": [], "classes": CLASSES,
           "n_defects": len(defects), "n_clean": len(clean), "audit_rate": AUDIT_RATE,
           "mutation_threshold": MUTATION_THRESHOLD}
    for name, kw, revs in policies(reviewers):
        rows = []
        for r in records:
            vs = [r["reviews"][x] for x in revs]
            rows.append((r, route(r["gates"], vs, **kw)))
        d_out = [o for r, o in rows if r["label"] != "clean"]
        c_out = [o for r, o in rows if r["label"] == "clean"]
        escaped = [r["id"] for r, o in rows if r["label"] != "clean" and o == "auto_land"]
        by_class = {c: {"n": 0, "escaped": 0} for c in CLASSES}
        for r, o in rows:
            if r["label"] != "clean":
                by_class[r["label"]]["n"] += 1
                by_class[r["label"]]["escaped"] += o == "auto_land"
        human = sum(o in ("seal", "escalate") for _, o in rows) + AUDIT_RATE * sum(o == "auto_land" for _, o in rows)
        out["policies"].append({
            "name": name, "reviewers": revs, "use_l0": kw.get("use_l0", True),
            "use_mutation": kw.get("use_mutation", True),
            "defects_escaped": len(escaped), "escaped_ids": escaped,
            "clean_landed": sum(o == "auto_land" for o in c_out),
            "clean_stopped": sum(o != "auto_land" for o in c_out),
            "clean_rejected": sum(o in ("gate_reject", "model_reject") for o in c_out),
            "outcomes_defects": {k: d_out.count(k) for k in ("gate_reject", "seal", "model_reject", "escalate", "auto_land")},
            "outcomes_clean": {k: c_out.count(k) for k in ("gate_reject", "seal", "model_reject", "escalate", "auto_land")},
            "human_per_100": round(100 * human / len(rows), 1),
            "by_class": by_class,
        })
    for x in reviewers:
        vs = [(r, r["reviews"][x]) for r in records]
        dv = [(r, v) for r, v in vs if r["label"] != "clean"]
        cv = [v for r, v in vs if r["label"] == "clean"]
        caught = [(r, v) for r, v in dv if stops(v)]
        fam = [any(FAMILY.get(f.get("kind"), "other") == FAMILY[r["label"]] for f in v["findings"]) for r, v in caught]
        per_class = {c: {"n": 0, "stopped": 0} for c in CLASSES}
        for r, v in dv:
            per_class[r["label"]]["n"] += 1
            per_class[r["label"]]["stopped"] += stops(v)
        secs = [v.get("seconds", 0) for _, v in vs if v.get("ok")]
        cost = [v.get("cost_usd") for _, v in vs if v.get("cost_usd") is not None]
        out["reviewers"].append({
            "name": x, "model": next((v.get("model") for _, v in vs if v.get("model")), x),
            "defects_stopped": len(caught), "defects": len(dv),
            "right_reason": sum(fam), "clean_stopped": sum(stops(v) for v in cv), "clean": len(cv),
            "errors": sum(not v.get("ok") for _, v in vs),
            "mean_seconds": round(sum(secs) / len(secs), 1) if secs else None,
            "cost_usd": round(sum(cost), 3) if cost else None,
            "per_class": per_class,
        })
    for i, a in enumerate(reviewers):
        for b in reviewers[i + 1:]:
            both = [r["id"] for r in defects if not stops(r["reviews"][a]) and not stops(r["reviews"][b])]
            out["co_miss"].append({"a": a, "b": b, "both_missed": len(both), "ids": both,
                                   "a_missed": sum(not stops(r["reviews"][a]) for r in defects),
                                   "b_missed": sum(not stops(r["reviews"][b]) for r in defects)})
    # Mutation threshold sweep from recorded scores (the gate's own trade-off).
    sweep = []
    for th in [0.5, 0.6, 0.7, 0.8, 0.9]:
        def fails(r):
            s = r["gates"]["mutation_score"]
            return s is not None and s < th
        sweep.append({"threshold": th, "defects_rejected": sum(fails(r) for r in defects),
                      "clean_rejected": sum(fails(r) for r in clean)})
    out["mutation_sweep"] = sweep
    out["changes"] = [{"id": r["id"], "label": r["label"], "intent": r["intent"], "note": r["note"],
                       "mutation_score": r["gates"]["mutation_score"],
                       "seal": bool(r["gates"]["ratchet"] or r["gates"]["governance"] or r["gates"]["irreversible"]),
                       "verdicts": {x: r["reviews"][x]["verdict"] for x in reviewers}} for r in records]
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--reviewers", default=",".join(PANEL))
    ap.add_argument("--reps", type=int, default=1)
    ap.add_argument("--corpus", help="alternative corpus module path (e.g. the pass-1 corpus)")
    ap.add_argument("--tag", default="", help="suffix for output files, e.g. pass1")
    args = ap.parse_args()
    global CHANGES, B
    if args.corpus:
        import importlib.util
        spec = importlib.util.spec_from_file_location("review_corpus_alt", args.corpus)
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        CHANGES, B = mod.CHANGES, mod.B
    sfx = f"-{args.tag}" if args.tag else ""
    names = [n for n in args.reviewers.split(",") if n]
    os.makedirs(OUT, exist_ok=True)
    t0 = time.time()
    gates = {c.id: run_gates(B, c) for c in CHANGES}
    diffs = {c.id: unified_diff(B, c.files) for c in CHANGES}
    print(f"gates done in {time.time() - t0:.0f}s", flush=True)

    reviews = {}  # (name, cid, rep) -> verdict

    def run_reviewer(name):
        rv = PANEL[name]()
        jobs = [(c, rep) for rep in range(args.reps) for c in CHANGES]
        with ThreadPoolExecutor(rv.max_parallel) as ex:
            for (c, rep), v in zip(jobs, ex.map(lambda j: rv.review(j[0], diffs[j[0].id], j[1]), jobs)):
                reviews[(name, c.id, rep)] = v
        errs = sum(not reviews[(name, c.id, rep)].get("ok") for c, rep in jobs)
        print(f"{name}: {len(jobs)} reviews, {errs} errors, {time.time() - t0:.0f}s", flush=True)

    with ThreadPoolExecutor(len(names)) as ex:
        list(ex.map(run_reviewer, names))

    summaries = []
    with open(os.path.join(OUT, f"review{sfx}.jsonl"), "w") as fh:
        for rep in range(args.reps):
            records = []
            for c in CHANGES:
                rec = {"id": c.id, "rep": rep, "label": c.label, "intent": c.intent.id, "note": c.note,
                       "gates": gates[c.id], "reviews": {n: reviews[(n, c.id, rep)] for n in names}}
                records.append(rec)
                fh.write(json.dumps(rec) + "\n")
            s = score(records, names)
            s["rep"] = rep
            summaries.append(s)
    with open(os.path.join(OUT, f"summary{sfx}.json"), "w") as fh:
        json.dump({"reps": summaries, "reviewers": names, "tag": args.tag or "final",
                   "generated": time.strftime("%Y-%m-%d %H:%M")}, fh, indent=1)
    for p in summaries[0]["policies"]:
        print(f"{p['name']:<58} escaped {p['defects_escaped']:>2}/{summaries[0]['n_defects']}  "
              f"clean landed {p['clean_landed']:>2}/{summaries[0]['n_clean']}  human/100 {p['human_per_100']}")


if __name__ == "__main__":
    main()
