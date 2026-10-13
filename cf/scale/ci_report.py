#!/usr/bin/env python3
"""Collect the CI bursts (cf/scale/ci.py) into one evidence file and print the tables of
cf/scale/ci.md.

  python3 -I cf/scale/ci_report.py --out cf/evidence/scale-ci-20261012.json \\
      --run before=A.json,B.json --run after=C.json [--latency LAT.json] [--overlap OV.json]

Each --run is a label and the reports of cf/scale/ci.py for one build of the Worker. The
evidence file keeps every round of every burst, without the clock values, and no URLs or
tokens.
"""
import argparse
import json

STAGES = ["fetch_ms", "gate_ms", "apply_ms", "ci_ms", "pack_ms", "push_ms", "record_ms"]


def burst_summary(b):
    rs = b["rounds"]
    tot = {k: round(sum(r.get(k, 0) for r in rs) / 1000, 2) for k in STAGES + ["round_ms"]}
    pushed = [r for r in rs if r.get("batch")]
    return {
        "n": b["n"], "batch_cap": b["batch_cap"], "rate": b["rate"], "bad": len(b["bad"]), "files": b.get("files", 0),
        "landed": b["landed"], "ejected_ci": b["ejected_ci"], "ejected_other": b["ejected_other"],
        "wrong": len(b["wrong"]), "duplicates": b["duplicates"], "pushes": b["batches"], "rounds": len(rs),
        "ci_runs": b["ci_runs"], "ci_runs_per_landing": b["ci_runs_per_landing"],
        "drain_s": b["server_drain_s"], "client_drain_s": b["client_drain_s"],
        "landings_per_s": round(b["landed"] / b["server_drain_s"], 1) if b.get("server_drain_s") else None,
        # The gate fetches all queued commits in the first round; Artifacts takes 0.4 to 10 s
        # for it. The rest of the drain is what CI and the pushes cost.
        "fetch_s": tot["fetch_ms"],
        "drain_after_fetch_s": round(b["server_drain_s"] - tot["fetch_ms"], 2) if b.get("server_drain_s") else None,
        "ci_ahead_hits": sum(1 for r in rs if r.get("ci_ahead_hit")),
        "stage_s": tot,
        "push_mean_ms": round(sum(r["push_ms"] for r in pushed) / len(pushed)) if pushed else None,
        "receipt_ci_runs": b["receipt_ci_runs"],
    }


def tail_events(path):
    """The events of a `wrangler tail --format json` file (JSON objects one after the other)."""
    text, dec, i, out = open(path).read(), json.JSONDecoder(), 0, []
    while True:
        while i < len(text) and text[i].isspace():
            i += 1
        if i >= len(text):
            return out
        e, i = dec.raw_decode(text, i)
        out.append(e)


def compact(rounds):
    t0 = rounds[0]["alarm_ms"] if rounds else 0
    return [{k: v for k, v in r.items() if k not in ("alarm_ms", "at_ms")} | {"at_s": round((r["at_ms"] - t0) / 1000, 2)}
            for r in rounds]


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--run", action="append", default=[], help="label=report.json[,report.json...]")
    p.add_argument("--latency", help="CI latency against project size (the driver Worker)")
    p.add_argument("--overlap", help="a check during an I/O wait (the driver Worker)")
    p.add_argument("--tail", action="append", default=[],
                   help="label=FILE: `wrangler tail --format json` of the CI Worker during bursts")
    p.add_argument("--out", required=True)
    args = p.parse_args()
    out = {"runs": {}}
    for spec in args.run:
        label, files = spec.split("=", 1)
        bursts = [b for f in files.split(",") for b in json.load(open(f))["bursts"]]
        out["runs"][label] = [{"summary": burst_summary(b), "trunk": b["trunk"], "bad_ids": b["bad"],
                               "samples": b["samples"], "rounds": compact(b["rounds"])} for b in bursts]
    if args.latency:
        out["ci_latency"] = json.load(open(args.latency))
    if args.overlap:
        out["ci_overlap"] = json.load(open(args.overlap))
    for spec in args.tail:
        label, path = spec.split("=", 1)
        out.setdefault("ci_tail", {})[label] = [{"at_ms": e.get("eventTimestamp"), "cpu_ms": e.get("cpuTime"),
                                                 "wall_ms": e.get("wallTime"), "outcome": e.get("outcome")}
                                                for e in tail_events(path)]
    with open(args.out, "w") as fh:
        json.dump(out, fh, indent=1)
    for label, rows in out["runs"].items():
        print(f"\n{label}\n")
        print("| N | Files | Cap | Bad | Landed | Ejected (CI) | Pushes | CI runs | CI runs per landing | Drain (s) | Fetch (s) | Drain after fetch (s) | Landings/s |")
        print("|---|---|---|---|---|---|---|---|---|---|---|---|---|")
        for r in rows:
            s = r["summary"]
            print(f"| {s['n']} | {s['files']} | {s['batch_cap']} | {s['bad']} | {s['landed']} | {s['ejected_ci']} | {s['pushes']} | "
                  f"{s['ci_runs']} | {s['ci_runs_per_landing']} | {s['drain_s']} | {s['fetch_s']} | {s['drain_after_fetch_s']} | {s['landings_per_s']} |"
                  + (f" WRONG {s['wrong']}" if s["wrong"] else "") + (f" DUP {s['duplicates']}" if s["duplicates"] else ""))


if __name__ == "__main__":
    main()
