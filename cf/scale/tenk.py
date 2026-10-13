#!/usr/bin/env python3
"""Control of the 1,000 to 10,000 trunk test (cf/scale/10k.md).

The load comes from the driver Worker (driver-10k), inside Cloudflare. This script only sends
the plan, reads the status of the shards, reads the live Workers, and collects the reports.
It sends a few requests each second, far below the limit of 200 requests/s for one computer.

  run      --k K --rate R --secs S --per P --tag T [--first 0] [--drain 300] [--poll 10] [--out raw.json]
           Use K trunks of the state file (repos-load setup), from trunk FIRST. Shard i sends the changes of
           trunks i*P .. i*P+P-1. Stop all shards at the first HTTP 429, when more than 5% of the
           changes in one poll interval failed, at --max-fail (50) failures in one poll interval, or
           when a live Worker does not answer 200.
  collect  --tag T --shards N --out raw.json    Read the reports again.
  kick     --tag T --shards N                   Make shards whose alarm was lost write their report.
  summary  --out summary.json raw-*.json        One row for each run, from the reports.
  verify   --k K --sample N --cli PATH --out F  `nerudo verify` on N random trunks of the first K.
  setup    --out F [--group 500]                 Times of init and fork, in groups of trunks.
  cost     --start ISO --end ISO --acct-file F   Usage of the two Workers from the GraphQL Analytics API,
           and its price. It reads the wrangler OAuth token from the wrangler config each time.

--vars (default $SCR/scale.vars): NERUDO_WORKER, NERUDO_AGENT_TOKEN, DRIVER_KEY,
DRIVER_URL. --state: the state file of repos-load (fork tokens and agent keys). Keep both out
of the repo. Run with `python3 -I`.
"""
import argparse
import concurrent.futures as cf
import json
import math
import os
import random
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request

LIVE = ["https://nerudo.patrickdeutsch.workers.dev/", "https://nerudo-web.patrickdeutsch.workers.dev/"]
FORBIDDEN = ("nerudo.patrickdeutsch.workers.dev", "nerudo-web.", "nerudo-ci.")
UA = "nerudo-scale-10k/0.1"  # workers.dev refuses some default user agents (error 1010)
SECRET = re.compile(r"(Bearer|Basic) [A-Za-z0-9+/=_.-]{16,}|art_v[A-Za-z0-9_?=.-]+|lsa1\.[A-Za-z0-9_.-]+|[0-9a-f]{64}|[0-9a-f]{32}(?=\.artifacts\.)")
FAN = 100  # shards in one request to the driver


def redact(text):
    return SECRET.sub("***", str(text))


def log(msg):
    print(time.strftime("%H:%M:%S"), msg, flush=True)


def load_vars(path):
    v = dict(l.strip().split("=", 1) for l in open(path) if "=" in l)
    for k in ("NERUDO_WORKER", "DRIVER_URL"):
        if any(f in v.get(k, "") for f in FORBIDDEN):
            sys.exit(f"refusing {v[k]}: only throwaway Workers")
    return v


def post(url, key, body, timeout=120):
    req = urllib.request.Request(url, method="POST", data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json", "Authorization": f"Bearer {key}", "User-Agent": UA})
    try:
        return json.loads(urllib.request.urlopen(req, timeout=timeout).read())
    except urllib.error.HTTPError as e:
        return {"http_error": e.code, "body": redact(e.read().decode(errors="replace")[:400])}
    except Exception as e:  # no answer
        return {"http_error": 0, "body": redact(e)}


def live_status(url):
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    try:
        r = urllib.request.urlopen(req, timeout=20)
        body = r.read(2000).decode(errors="replace")
        return 403 if "you have been blocked" in body.lower() else r.status
    except urllib.error.HTTPError as e:
        return e.code
    except Exception:
        return 0


def fan(v, op, bodies, workers=4):
    """POST /fan/op for each shard name in bodies, FAN shards to one request. {name: answer}."""
    names = list(bodies)
    groups = [names[i:i + FAN] for i in range(0, len(names), FAN)]
    out = {}

    def one(g):
        return post(f"{v['DRIVER_URL']}/fan/{op}", v["DRIVER_KEY"], {"shards": {n: bodies[n] for n in g}})

    with cf.ThreadPoolExecutor(workers) as ex:
        for g, res in zip(groups, ex.map(one, groups)):
            if "http_error" in res:
                for n in g:
                    out[n] = {"status": 0, "error": f"driver: {res}"}
            else:
                out.update(res)
    return out


def shard_names(tag, n):
    return [f"{tag}/{i}" for i in range(n)]


def plan_shards(args, state):
    repos = [r for r in state if r.get("ok")][args.first:args.first + args.k]
    if len(repos) < args.k:
        sys.exit(f"the state file has only {len(repos)} ready trunks")
    n = math.ceil(args.k / args.per)
    bodies = {}
    for i, name in enumerate(shard_names(args.tag, n)):
        seats = []
        for g in range(i * args.per, min((i + 1) * args.per, args.k)):
            r = repos[g]
            f = r["forks"][0]
            seats.append({"g": g, "trunk": r["trunk"], "fork": f["name"], "remote": f["remote"], "authorization": f["authorization"],
                          "key": f["agent_key"], "base": f["base"]})
        bodies[name] = seats
    return bodies


def aggregate(statuses):
    agg = {"phases": {}, "sent": 0, "accepted": 0, "not_sent": 0, "errors": {}, "lag_max": 0, "queued": 0, "stops": {}, "no_answer": 0}
    for name, s in statuses.items():
        b = s.get("body") if isinstance(s, dict) else None
        if not isinstance(b, dict) or s.get("status") != 200:
            agg["no_answer"] += 1
            continue
        ph = b.get("phase", "?")
        agg["phases"][ph] = agg["phases"].get(ph, 0) + 1
        for k in ("sent", "accepted", "not_sent"):
            agg[k] += b.get(k) or 0
        for k, c in (b.get("errors") or {}).items():
            agg["errors"][k] = agg["errors"].get(k, 0) + c
        agg["lag_max"] = max(agg["lag_max"], b.get("lag_max") or 0)
        agg["queued"] += b.get("queued") or 0
        if b.get("stop"):
            agg["stops"][b["stop"]] = agg["stops"].get(b["stop"], 0) + 1
    return agg


def stop_all(v, names, tag, reason):
    log(f"STOP all shards: {reason}")
    res = fan(v, "stop", {n: {"tag": tag, "reason": reason} for n in names}, workers=8)
    bad = [n for n, r in res.items() if r.get("status") != 200]
    if bad:
        log(f"  stop did not reach {len(bad)} shards; again")
        fan(v, "stop", {n: {"tag": tag, "reason": reason} for n in bad}, workers=8)


def run(args):
    v = load_vars(args.vars)
    state = json.load(open(args.state))
    for u in LIVE:
        s = live_status(u)
        if s != 200:
            sys.exit(f"{u} answers {s}; no load")
    seats = plan_shards(args, state)
    names = list(seats)
    lead = max(args.lead, args.prep + 10) + len(names) / FAN * 1.5
    t0 = (time.time() + lead) * 1000.0
    plan = {"tag": args.tag, "t0_ms": t0, "rate": args.rate, "secs": args.secs, "k": args.k, "drain_s": args.drain, "poll_s": args.poll,
            "prep_s": args.prep}
    log(f"run {args.tag}: K={args.k} rate={args.rate}/s per trunk for {args.secs}s; {len(names)} shards of {args.per}; t0 in {lead:.0f}s")
    res = fan(v, "start", {n: dict(plan, seats=seats[n]) for n in names}, workers=4)
    bad = {n: r for n, r in res.items() if r.get("status") != 200}
    if bad:
        log(f"start failed on {len(bad)} shards, e.g. {redact(list(bad.items())[:2])}")
        stop_all(v, names, args.tag, "start failed")
        sys.exit(1)
    log(f"  all {len(names)} shards planned")
    monitor, health = [], []
    prev, stopped = None, None
    next_live = time.time()
    deadline = t0 / 1000 + args.secs + args.drain + 180
    kicked = False
    while True:
        time.sleep(max(1.0, args.poll - (time.time() % args.poll)))
        now = time.time()
        if now >= next_live:
            st = [live_status(u) for u in LIVE]
            health.append({"t": round(now * 1000 - t0), "status": st})
            next_live = now + 60
            if any(s != 200 for s in st) and not stopped:
                stopped = f"a live Worker answered {st}"
                stop_all(v, names, args.tag, stopped)
        statuses = fan(v, "status", {n: {"tag": args.tag} for n in names}, workers=8)
        agg = aggregate(statuses)
        agg["t"] = round(now * 1000 - t0)
        monitor.append(agg)
        errs = sum(agg["errors"].values())
        line = (f"t={agg['t'] / 1000:6.0f}s phases={agg['phases']} sent={agg['sent']} acc={agg['accepted']} errs={errs} "
                f"not_sent={agg['not_sent']} lag_max={agg['lag_max']}ms queued={agg['queued']} no_answer={agg['no_answer']}")
        if agg["errors"]:
            line += f" {agg['errors']}"
        if agg["stops"]:
            line += f" stops={list(agg['stops'].items())[:3]}"
        log(line)
        if not stopped:
            if any(k.endswith("_429") for k in agg["errors"]):
                stopped = "HTTP 429"
            elif prev and agg["sent"] - prev["sent"] >= 100:
                d_err = errs - sum(prev["errors"].values())
                # 5% of the interval, or --max-fail failures: at 2,000 changes/s, 5% cannot see a burst.
                if d_err * 20 > agg["sent"] - prev["sent"] or d_err >= args.max_fail:
                    stopped = f"{d_err} of {agg['sent'] - prev['sent']} changes failed in {args.poll}s"
            if stopped:
                stop_all(v, names, args.tag, stopped)
        prev = agg
        ended = sum(agg["phases"].get(p, 0) for p in ("done", "failed"))
        if ended == len(names):
            break
        if not kicked and now > t0 / 1000 + args.secs + 120 and agg["phases"].get("running"):
            log("kick shards that are still running")
            fan(v, "kick", {n: {"tag": args.tag} for n in names}, workers=8)
            kicked = True
        if now > deadline:
            log("deadline: not all shards are done")
            break
    collect_into(v, args.tag, names, args.out, {"plan": plan, "per": args.per, "shards": len(names), "monitor": monitor,
                                                 "health": health, "stopped": stopped})


def collect_into(v, tag, names, out, extra):
    reports = fan(v, "report", {n: {"tag": tag} for n in names}, workers=4)
    raw = dict(extra, tag=tag, reports={n: r.get("body") for n, r in reports.items()})
    with open(out, "w") as fh:
        json.dump(raw, fh)
    got = sum(1 for r in raw["reports"].values() if isinstance(r, dict) and r.get("trunks") is not None)
    log(f"wrote {out}: {got} of {len(names)} shard reports")


def collect(args):
    v = load_vars(args.vars)
    names = shard_names(args.tag, args.shards)
    collect_into(v, args.tag, names, args.out, {"recollected": True})


def kick(args):
    v = load_vars(args.vars)
    res = fan(v, "kick", {n: {"tag": args.tag} for n in shard_names(args.tag, args.shards)})
    log(f"kick {args.tag}: {sorted((n, r.get('body')) for n, r in res.items())[:10]}")


# ---------------------------------------------------------------- summary

def hist_add(into, h):
    for k, c in (h or {}).items():
        into[int(k)] = into.get(int(k), 0) + c


def hist_pct(h, p):
    total = sum(h.values())
    if not total:
        return None
    want = p / 100 * total
    acc = 0
    for k in sorted(h):
        acc += h[k]
        if acc >= want:
            return k
    return max(h)


def summarize(raw, singles):
    reps = [r for r in raw["reports"].values() if isinstance(r, dict) and r.get("trunks") is not None]
    plan = raw.get("plan") or (reps[0] if reps else {})
    k, rate, secs = plan.get("k"), plan.get("rate"), plan.get("secs")
    t0 = plan.get("t0_ms") or (reps[0]["t0_ms"] if reps else 0)
    tot = {x: sum(r.get(x) or 0 for r in reps) for x in ("offered", "sent", "accepted", "landed", "not_sent")}
    errors = {}
    for r in reps:
        for e, c in (r.get("errors") or {}).items():
            errors[e] = errors.get(e, 0) + c
    failed = sum(c for e, c in errors.items() if e not in ("not_sent", "lost"))
    lat, push_h, land_h, lag, landed_s, batches = {}, {}, {}, {}, {}, {}
    lat_min = {}
    for r in reps:
        hist_add(lat, r["lat"])
        hist_add(push_h, r["push"])
        hist_add(land_h, r["land"])
        hist_add(lag, r["lag"])
        hist_add(landed_s, r["landed_s"])
        hist_add(batches, r["batch_sizes"])
        for m, h in (r.get("lat_min") or {}).items():
            hist_add(lat_min.setdefault(int(m), {}), h)
    firsts = [r["first_submitted_ms"] for r in reps if r.get("first_submitted_ms")]
    lasts = [r["last_submitted_ms"] for r in reps if r.get("last_submitted_ms")]
    window = None
    if firsts and lasts:
        # From 30 s after the first submit to the last submit, but not after the planned end of
        # admission: an agent that is late sends after it, and only a few trunks land then.
        ws = math.ceil((min(firsts) + 30000 - t0) / 1000)
        we = min(math.floor((max(lasts) - t0) / 1000), int(secs or 0))
        if we > ws:
            n = sum(c for s, c in landed_s.items() if ws <= s < we)
            window = {"from_s": ws, "to_s": we, "landings": n, "rate": round(n / (we - ws), 3),
                      "landed_after_admission": sum(c for s, c in landed_s.items() if s >= int(secs or 0))}
    trunks = [t for r in reps for t in r["trunks"]]
    win_len = (secs or 0) - 30
    per_trunk = sorted((t["in_window"] / win_len if win_len > 0 else 0) for t in trunks)
    p50s = sorted(t["p50"] for t in trunks if t.get("p50") is not None)
    git = sum(t.get("git_requests") or 0 for t in trunks)
    landed_delta = sum(t.get("landed_delta") or 0 for t in trunks)
    nb = sum(batches.values())
    drains = [(r.get("drained_at"), r.get("sent_end")) for r in reps]
    single = singles.get(rate)
    out = {
        "tag": raw["tag"], "k": k, "rate_per_trunk": rate, "secs": secs, "shards": raw.get("shards"), "per_shard": raw.get("per"),
        "offered": tot["offered"], "sent": tot["sent"], "accepted": tot["accepted"], "landed": tot["landed"], "not_sent": tot["not_sent"],
        "errors": errors, "failed": failed, "error_rate": round(failed / tot["sent"], 5) if tot["sent"] else None,
        "offered_rate": round(k * rate, 3) if k and rate else None,
        "landed_rate": window["rate"] if window else None, "window": window,
        "expected_k_times_single": round(k * single, 3) if single and k else None,
        "measured_over_expected": round(window["rate"] / (k * single), 3) if single and k and window else None,
        "latency_ms": {p: hist_pct(lat, p) for p in (50, 95, 99, 100)},
        "latency_by_minute": {m: {"p50": hist_pct(h, 50), "p95": hist_pct(h, 95), "n": sum(h.values())} for m, h in sorted(lat_min.items())},
        "push_ms": {p: hist_pct(push_h, p) for p in (50, 95, 99)}, "land_ms": {p: hist_pct(land_h, p) for p in (50, 95, 99)},
        "send_lag_ms": {p: hist_pct(lag, p) for p in (50, 99, 100)},
        "sent_late_1s": sum(c for x, c in lag.items() if x >= 1000),
        "batch_mean": round(sum(s * c for s, c in batches.items()) / nb, 3) if nb else None, "batch_max": max(batches) if batches else None,
        "git_requests_per_landing": round(git / landed_delta, 3) if landed_delta else None,
        "slowest_trunk_rate": round(per_trunk[0], 3) if per_trunk else None,
        "trunk_rate_p1": round(per_trunk[len(per_trunk) // 100], 3) if per_trunk else None,
        "trunk_p50_ms": {"min": p50s[0], "median": p50s[len(p50s) // 2], "max": p50s[-1]} if p50s else None,
        "fetch_retries": sum(t.get("fetch_retries") or 0 for t in trunks), "ejected": sum(t.get("ejected") or 0 for t in trunks),
        "external_moves": sum(t.get("external_moves") or 0 for t in trunks),
        "wasm_bytes_max": max((t.get("wasm_bytes") or 0 for t in trunks), default=0),
        "all_drained": all(d is not None for d, _ in drains),
        "drain_ms_max": max(((d - s) for d, s in drains if d is not None and s is not None), default=None),
        "missing": sum(len(r.get("missing") or []) for r in reps),
        "receipts": [x for r in reps for x in (r.get("receipts") or [])][:20],
        "stops": sorted({r["stop"] for r in reps if r.get("stop")})[:5],
        "stopped": raw.get("stopped"),
        "samples": {e: s for r in reps for e, s in (r.get("samples") or {}).items()},
        "reports": len(reps),
        "health": raw.get("health"),
        "queue_max": max((p.get("q", 0) for r in reps for p in (r.get("polls") or [])), default=0),
    }
    slow = sorted(trunks, key=lambda t: (t["in_window"], -(t.get("p50") or 0)))[:10]
    out["slowest_trunks"] = [{x: t.get(x) for x in ("trunk", "in_window", "p50", "p99", "landed", "errors", "git_requests", "batches")} for t in slow]
    if (k or 0) <= 1000:
        out["trunk_rows"] = [[t["trunk"], t["offered"], t["accepted"], t["landed"], t["in_window"], t.get("p50"), t.get("p99"), t.get("git_requests"),
                              t.get("batches"), t.get("fetch_retries")] for t in sorted(trunks, key=lambda t: t["g"])]
        out["trunk_row_fields"] = ["trunk", "offered", "accepted", "landed", "in_window", "p50_ms", "p99_ms", "git_requests", "batches", "fetch_retries"]
    return out


def summary(args):
    raws = [json.load(open(p)) for p in args.raw]
    singles = {}
    for raw in raws:
        row = summarize(raw, singles)
        if row["k"] == 1 and row["landed_rate"]:
            singles.setdefault(row["rate_per_trunk"], row["landed_rate"])
    rows = [summarize(raw, singles) for raw in raws]
    old = json.load(open(args.out)) if os.path.exists(args.out) else {}
    keep = {r["tag"]: r for r in old.get("runs", [])}
    for r in rows:
        keep[r["tag"]] = r
    old["runs"] = list(keep.values())
    with open(args.out, "w") as fh:
        json.dump(old, fh, indent=1)
    for r in rows:
        print(json.dumps({x: r[x] for x in ("tag", "k", "rate_per_trunk", "offered", "accepted", "landed", "failed", "error_rate", "landed_rate",
                                           "measured_over_expected", "latency_ms", "send_lag_ms", "git_requests_per_landing", "slowest_trunk_rate",
                                           "all_drained", "drain_ms_max", "stopped")}))


# ---------------------------------------------------------------- verify and setup

def verify(args):
    v = load_vars(args.vars)
    state = [r for r in json.load(open(args.state)) if r.get("ok")][:args.k]
    rnd = random.Random(args.seed)
    pick = sorted(rnd.sample(range(len(state)), min(args.sample, len(state))))
    home = os.path.join(os.path.dirname(os.path.abspath(args.vars)), "home")
    os.makedirs(home, exist_ok=True)
    env = dict(os.environ, HOME=home, GIT_TERMINAL_PROMPT="0", NERUDO_WORKER=v["NERUDO_WORKER"], NERUDO_TOKEN=v["NERUDO_AGENT_TOKEN"])
    env.pop("NERUDO_ADMIN_TOKEN", None)

    def one(i):
        trunk = state[i]["trunk"]
        p = subprocess.run([args.cli, "verify", "--trunk", trunk], env=env, capture_output=True, text=True, timeout=600)
        tail = redact((p.stdout + p.stderr)[-600:])
        m = re.search(r"(\d+) landed changes, each on trunk once", p.stdout)
        return {"trunk": trunk, "exit": p.returncode, "landed": int(m.group(1)) if m else None, "tail": tail if p.returncode else ""}

    with cf.ThreadPoolExecutor(4) as ex:
        res = list(ex.map(one, pick))
    ok = sum(1 for r in res if r["exit"] == 0)
    out = {"k": args.k, "sample": len(res), "passed": ok, "seed": args.seed, "trunks": res}
    old = json.load(open(args.out)) if os.path.exists(args.out) else {}
    old[args.tag] = out
    with open(args.out, "w") as fh:
        json.dump(old, fh, indent=1)
    log(f"verify {args.tag}: {ok} of {len(res)} passed; {[r['trunk'] for r in res if r['exit']]}")


def setup_stats(args):
    state = json.load(open(args.state))
    state.sort(key=lambda r: r["trunk"])
    groups = []
    for i in range(0, len(state), args.group):
        g = state[i:i + args.group]
        init = sorted(r["init_ms"] for r in g if r.get("init_ms") is not None)
        fork = sorted(f["fork_ms"] for r in g for f in r.get("forks") or [])
        retried = sum(1 for r in g for f in r.get("forks") or [] if f.get("fork_errors"))

        def p(xs, q):
            return round(xs[min(len(xs) - 1, int(q / 100 * len(xs)))]) if xs else None

        groups.append({"from": g[0]["trunk"], "to": g[-1]["trunk"], "trunks": len(g), "failed": sum(1 for r in g if not r.get("ok")),
                       "init_p50": p(init, 50), "init_p95": p(init, 95), "init_max": round(init[-1]) if init else None,
                       "fork_p50": p(fork, 50), "fork_p95": p(fork, 95), "fork_max": round(fork[-1]) if fork else None, "fork_retried": retried,
                       "errors": [redact(r.get("error")) for r in g if not r.get("ok")][:5]})
    out = {"trunks": len(state), "ok": sum(1 for r in state if r.get("ok")), "forks": sum(len(r.get("forks") or []) for r in state), "groups": groups}
    with open(args.out, "w") as fh:
        json.dump(out, fh, indent=1)
    for g in groups:
        print(json.dumps(g))


SCRIPTS = ("nerudo-scale-10k", "nerudo-scale-10k-driver")
PRICE = {"worker_req": 0.30e-6, "do_req": 0.15e-6, "gb_s": 12.50e-6, "row_w": 1.00e-6, "row_r": 0.001e-6, "cpu_ms": 0.02e-6}


def cf_api(path, body=None):
    cfg = open(os.path.expanduser("~/.wrangler/config/default.toml")).read()
    tok = re.search(r'^oauth_token\s*=\s*"([^"]+)"', cfg, re.M).group(1)
    req = urllib.request.Request(f"https://api.cloudflare.com/client/v4/{path}", method="POST" if body else "GET",
                                 data=json.dumps(body).encode() if body else None,
                                 headers={"Authorization": f"Bearer {tok}", "Content-Type": "application/json"})
    return json.loads(urllib.request.urlopen(req, timeout=60).read())


def cost(args):
    acct = open(args.acct_file).read().strip()
    ns = {n["id"]: f"{n['script']}/{n['class']}" for n in cf_api(f"accounts/{acct}/workers/durable_objects/namespaces?per_page=1000")["result"]
          if n.get("script") in SCRIPTS}
    q = """query($acct: String!, $start: Time!, $end: Time!, $ns: [string!], $scripts: [string!]) { viewer { accounts(filter: {accountTag: $acct}) {
      p: durableObjectsPeriodicGroups(limit: 100, filter: {datetimeMinute_geq: $start, datetimeMinute_lt: $end, namespaceId_in: $ns}) {
        sum { rowsWritten rowsRead activeTime cpuTime subrequests exceededMemoryErrors exceededCpuErrors fatalInternalErrors } dimensions { namespaceId } }
      i: durableObjectsInvocationsAdaptiveGroups(limit: 100, filter: {datetimeMinute_geq: $start, datetimeMinute_lt: $end, namespaceId_in: $ns}) {
        sum { requests errors } dimensions { namespaceId status } }
      w: workersInvocationsAdaptive(limit: 100, filter: {datetime_geq: $start, datetime_lt: $end, scriptName_in: $scripts}) {
        sum { requests errors subrequests cpuTimeUs } dimensions { scriptName status } }
    } } }"""
    r = cf_api("graphql", {"query": q, "variables": {"acct": acct, "start": args.start, "end": args.end, "ns": list(ns), "scripts": list(SCRIPTS)}})
    if r.get("errors"):
        sys.exit(redact(r["errors"]))
    a = r["data"]["viewer"]["accounts"][0]
    out = {"start": args.start, "end": args.end, "objects": {}, "workers": {}}
    for g in a["p"]:
        o = out["objects"].setdefault(ns[g["dimensions"]["namespaceId"]], {"requests": 0, "by_status": {}})
        o.update({k: v for k, v in g["sum"].items()})
        o["gb_s"] = round(g["sum"]["activeTime"] / 1e6 * 0.125, 1)
    for g in a["i"]:
        o = out["objects"].setdefault(ns[g["dimensions"]["namespaceId"]], {"requests": 0, "by_status": {}})
        o["requests"] += g["sum"]["requests"]
        o["by_status"][g["dimensions"]["status"]] = o["by_status"].get(g["dimensions"]["status"], 0) + g["sum"]["requests"]
    for g in a["w"]:
        w = out["workers"].setdefault(g["dimensions"]["scriptName"], {"requests": 0, "errors": 0, "subrequests": 0, "cpu_ms": 0, "by_status": {}})
        for k in ("requests", "errors", "subrequests"):
            w[k] += g["sum"][k]
        w["cpu_ms"] += g["sum"]["cpuTimeUs"] / 1000
        w["by_status"][g["dimensions"]["status"]] = w["by_status"].get(g["dimensions"]["status"], 0) + g["sum"]["requests"]
    usd = 0.0
    for o in out["objects"].values():
        usd += o["requests"] * PRICE["do_req"] + o.get("gb_s", 0) * PRICE["gb_s"] + o.get("rowsWritten", 0) * PRICE["row_w"] + o.get("rowsRead", 0) * PRICE["row_r"]
    for w in out["workers"].values():
        usd += w["requests"] * PRICE["worker_req"] + w["cpu_ms"] * PRICE["cpu_ms"]
    out["usd_list_price"] = round(usd, 3)
    print(json.dumps(out, indent=1))
    if args.out != "out.json":
        old = json.load(open(args.out)) if os.path.exists(args.out) else {}
        old[args.tag] = out
        with open(args.out, "w") as fh:
            json.dump(old, fh, indent=1)


def main():
    scr = os.environ.get("SCR", ".")
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["run", "collect", "kick", "summary", "verify", "setup", "cost"])
    ap.add_argument("raw", nargs="*")
    ap.add_argument("--vars", default=os.path.join(scr, "scale.vars"))
    ap.add_argument("--state", default=os.path.join(scr, "state.json"))
    ap.add_argument("--k", type=int, default=1)
    ap.add_argument("--rate", type=float, default=1.0)
    ap.add_argument("--secs", type=float, default=300)
    ap.add_argument("--per", type=int, default=4)
    ap.add_argument("--first", type=int, default=0)
    ap.add_argument("--tag", default="t")
    ap.add_argument("--drain", type=float, default=300)
    ap.add_argument("--poll", type=float, default=10)
    ap.add_argument("--max-fail", type=int, default=50)
    ap.add_argument("--lead", type=float, default=25)
    ap.add_argument("--prep", type=float, default=15)
    ap.add_argument("--shards", type=int, default=1)
    ap.add_argument("--sample", type=int, default=20)
    ap.add_argument("--group", type=int, default=500)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--cli")
    ap.add_argument("--start")
    ap.add_argument("--end")
    ap.add_argument("--acct-file")
    ap.add_argument("--out", default="out.json")
    args = ap.parse_args()
    {"run": run, "collect": collect, "kick": kick, "summary": summary, "verify": verify, "setup": setup_stats, "cost": cost}[args.cmd](args)


if __name__ == "__main__":
    main()
