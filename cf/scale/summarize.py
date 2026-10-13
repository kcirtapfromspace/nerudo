#!/usr/bin/env python3
"""Collect the stage-timing runs into one evidence file (cf/scale/stages.md).

  python3 -I cf/scale/summarize.py --out cf/evidence/scale-stages-20261012.json \\
      --local before=LOCAL.json [--local after=...] --live before=LIVE.json [--live after=...] \\
      [--tail before=TAIL.jsonl ...]

LOCAL files are reports of the fault harness (cf/faults/run.py, case growth). LIVE files come
from cf/scale/stages.py. TAIL files are `wrangler tail --format json` of the Worker during the
live runs: the script keeps only the CPU and wall time of each alarm. It writes no trunk
URLs, tokens or account names.
"""
import argparse
import json

STAGES = ["head_ms", "load_ms", "fetch_ms", "gate_ms", "divert_ms", "pick_ms", "apply_ms", "ci_ms", "pack_ms",
          "push_ms", "record_ms", "events_ms"]


def summary(rounds):
    tot = {k: round(sum(r.get(k, 0) for r in rounds) / 1000, 2) for k in STAGES + ["round_ms"]}
    fetched = sum(r.get("fetched", 0) for r in rounds)
    pushes = sum(1 for r in rounds if r.get("batch"))
    first = rounds[0] if rounds else {}
    return {
        "rounds": len(rounds), "batches_pushed": pushes, "fetch_requests": fetched,
        "stage_s": tot,
        "fetch_mean_ms": round(sum(r.get("fetch_ms", 0) for r in rounds) / fetched, 1) if fetched else None,
        "fetch_max_ms": max((r.get("fetch_max_ms", 0) for r in rounds), default=None),
        "push_mean_ms": round(sum(r.get("push_ms", 0) for r in rounds if r.get("batch")) / pushes, 1) if pushes else None,
        "first_round_ms": first.get("round_ms"), "first_round_fetch_ms": first.get("fetch_ms"),
        "later_round_mean_ms": round(sum(r["round_ms"] for r in rounds[1:]) / (len(rounds) - 1), 1) if len(rounds) > 1 else None,
        "later_gate_mean_ms": round(sum(r["gate_ms"] for r in rounds[1:]) / (len(rounds) - 1), 1) if len(rounds) > 1 else None,
        "pack_bytes_first_last": [first.get("pack_bytes"), rounds[-1].get("pack_bytes")] if rounds else None,
    }


def compact(rounds):
    """The rounds without the clock values, which say nothing outside this run."""
    return [{k: v for k, v in r.items() if k not in ("alarm_ms", "at_ms")} | {"at_s": round((r["at_ms"] - rounds[0]["alarm_ms"]) / 1000, 2)}
            for r in rounds]


def tail_alarms(path):
    raw, dec, i, out = open(path).read(), json.JSONDecoder(), 0, []
    while i < len(raw):
        while i < len(raw) and raw[i].isspace():
            i += 1
        if i >= len(raw):
            break
        e, i = dec.raw_decode(raw, i)
        if "scheduledTime" in (e.get("event") or {}):
            out.append({"outcome": e.get("outcome"), "cpu_ms": e.get("cpuTime"), "wall_ms": e.get("wallTime"),
                        "exceptions": len(e.get("exceptions") or [])})
    return out


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--out", required=True)
    p.add_argument("--local", action="append", default=[], help="label=report.json")
    p.add_argument("--live", action="append", default=[], help="label=stages.json")
    p.add_argument("--tail", action="append", default=[], help="label=tail.jsonl")
    p.add_argument("--note", action="append", default=[])
    args = p.parse_args()
    out = {"notes": args.note, "local": [], "live": [], "alarms": {}}
    for spec in args.local:
        label, path = spec.split("=", 1)
        rep = json.load(open(path))
        for row in rep["cases"].get("growth", []):
            out["local"].append({"build": label, "host": "wrangler dev + Gitea (cf/faults)", "n": row["n"], "agents": 4,
                                 "batch_cap": 16, "drain_s": row["drain_s"], "batches": row["batches"],
                                 "git_requests": row.get("git_requests"), "summary": summary(row["rounds"]),
                                 "rounds": compact(row["rounds"])})
        out.setdefault("local_versions", {})[label] = rep.get("versions")
    for spec in args.live:
        label, path = spec.split("=", 1)
        for b in json.load(open(path))["bursts"]:
            rounds = b["rounds"]
            out["live"].append({
                "build": label, "host": "deployed Worker + Artifacts", "label": b["label"], "n": b["n"], "agents": b["agents"],
                "batch_cap": b["batch_cap"], "readers": b["readers"], "landed": b["landed"], "ejected": b["ejected"],
                "log_entries": b["log_entries"], "batches": b["batches"], "git_requests": b["git_requests"],
                "server_drain_s": b["server_drain_s"], "server_first_landing_s": b["server_first_landing_s"],
                "client_drain_s": b["client_drain_s"], "reader_stats": b.get("reader_stats"),
                "summary": summary(rounds), "rounds": compact(rounds)})
    for spec in args.tail:
        label, path = spec.split("=", 1)
        out["alarms"][label] = tail_alarms(path)
    with open(args.out, "w") as fh:
        json.dump(out, fh, indent=1)
        fh.write("\n")
    # A Markdown table for cf/scale/stages.md: times in s, from the rounds (server clock).
    print("| Build | Host | N | Forks | Cap | Readers | Drain | First landing | Fetch requests | Fetch | Push | Rest |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|")
    for r in out["local"] + out["live"]:
        s = r["summary"]
        st = s["stage_s"]
        first = r.get("server_first_landing_s", s["first_round_ms"] / 1000 if s["first_round_ms"] else None)
        rest = max(st["round_ms"] - st["fetch_ms"] - st["push_ms"], 0.0)
        host = "local" if r in out["local"] else "Artifacts"
        print(f"| {r['build']} | {host} | {r['n']} | {r['agents']} | {r['batch_cap']} | {'yes' if r.get('readers') else 'no'} "
              f"| {r.get('server_drain_s') or r.get('drain_s')} | {first:.2f} | {s['fetch_requests']} | {st['fetch_ms']:.2f} "
              f"| {st['push_ms']:.2f} | {rest:.2f} |")


if __name__ == "__main__":
    main()
