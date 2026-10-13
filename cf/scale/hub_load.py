#!/usr/bin/env python3
"""Rate ladder for the Hub Durable Object (cf/scale/hub.md).

  setup   make 8 trunks on Artifacts (ids <prefix>-<i>-<time>) and name them <org>/r<i>
          (private; `--public` for the app paths d and e)
  ladder  open-loop constant rate with vegeta, 60 s per step, on one path:
            base  GET /                                  the Worker only, no Durable Object
            a     GET /v1/repos/<org>/nope/stats         the Hub lookup only (404, no trunk)
            r     GET /v1/repos/<org>/nope-<n>/stats     as a, but a new unknown name each time
            b     GET /v1/trunks/{id}/stats              8 Trunk objects, no Hub
            c     GET /v1/repos/<org>/r<i>/stats         the Hub lookup, then 8 Trunk objects
            d     GET /v1/hub/repos/<org>/r<i>           a repo page of the web app: no key,
                                                         the Hub record and the trunk summary
            e     GET /v1/hub/trunks/{id}                the same by trunk id (/app/{trunk})
  report  join the step results into one JSON file
  tail    count the events of `wrangler tail --format json` by minute, path and outcome; and
          the Hub's own lookups (/repo/...) for each request by name

`--via driver` sends the load through hub_driver.js, a Worker inside Cloudflare that makes
`--n` calls at once to the Worker under test for each request it gets. The edge blocks one
computer at about 2000 requests/s ("Sorry, you have been blocked"), so above 1000/s the
ladder uses the driver. The rate is then the rate of calls to the Worker under test.

Only for a throwaway Worker (`--vars`: NERUDO_ADMIN_TOKEN, NERUDO_AGENT_TOKEN,
NERUDO_WORKER). The script refuses the live Worker. A watchdog reads the live Worker's
front page every 20 s and stops the load when it is not 200. A ledger in the scratch
directory counts every request sent, and no step starts that would pass `--cap`.

Run it with `python3 -I`, with vegeta on PATH (`nix shell nixpkgs#vegeta`).
"""
import argparse
import base64
import collections
import json
import os
import re
import resource
import socket
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
LIVE = "https://nerudo.patrickdeutsch.workers.dev/"
FORBIDDEN = ("nerudo.patrickdeutsch.workers.dev", "nerudo-web.", "nerudo-ci.")
UA = "nerudo-scale/0.1"  # workers.dev refuses some default user agents (error 1010)
TRUNKS = 8
# Paths where a 404 "unknown repo" is the expected answer.
UNKNOWN = ("a", "r")
# Tokens, and the driver's key in the URLs of vegeta's errors.
SECRET = re.compile(r"(Bearer|Basic) [A-Za-z0-9+/=_.-]{16,}|[0-9a-f]{64}|(?<=key=)[0-9a-f]{16,}")


def redact(text):
    return SECRET.sub("***", text)


def load_vars(path):
    v = dict(l.strip().split("=", 1) for l in open(path) if "=" in l)
    if any(f in v["NERUDO_WORKER"] for f in FORBIDDEN):
        sys.exit(f"refusing {v['NERUDO_WORKER']}: load only a throwaway Worker")
    return v


def call(v, method, path, body=None, admin=True):
    tok = v["NERUDO_ADMIN_TOKEN" if admin else "NERUDO_AGENT_TOKEN"]
    req = urllib.request.Request(f"{v['NERUDO_WORKER']}{path}", method=method,
                                 data=json.dumps(body).encode() if body is not None else None,
                                 headers={"Content-Type": "application/json", "Authorization": f"Bearer {tok}", "User-Agent": UA})
    try:
        return json.loads(urllib.request.urlopen(req, timeout=120).read())
    except urllib.error.HTTPError as e:
        return {"http_error": e.code, "body": redact(e.read().decode()[:400])}


def setup(args):
    v = load_vars(args.vars)
    sys.path.insert(0, os.path.join(ROOT, "bench"))
    from workload import init_tree
    import shutil
    import tempfile
    stamp = time.strftime("%H%M%S")
    org = args.org
    out = {"org": org, "repos": []}
    for i in range(TRUNKS):
        trunk = f"{args.prefix}-{i}-{stamp}"
        seed = tempfile.mkdtemp(prefix=f"{trunk}-")
        init_tree(seed)
        shutil.copy(os.path.join(ROOT, "cf", "fixtures", "bench-nerudo.toml"), os.path.join(seed, "nerudo.toml"))
        files = {os.path.relpath(os.path.join(d, f), seed): open(os.path.join(d, f)).read()
                 for d, _, fs in os.walk(seed) for f in fs}
        body = {"trunk": trunk, "host": {"kind": "artifacts", "binding": "ARTIFACTS", "repo": trunk},
                "seed": {"files": files, "message": "seed\n"}}
        r = call(v, "POST", f"/v1/trunks/{trunk}/init", body)
        assert "head" in r, r
        r = call(v, "PUT", f"/v1/hub/repos/{org}/r{i}", {"trunk": trunk, "public": args.public})
        assert r.get("trunk") == trunk, r
        out["repos"].append({"name": f"{org}/r{i}", "trunk": trunk})
        print(f"{org}/r{i} -> {trunk}", flush=True)
    json.dump(out, open(args.out, "w"), indent=1)


def targets(v, path, org, repos, via, n):
    if via == "driver":
        url = f"{v['NERUDO_DRIVER']}/?key={v['NERUDO_DRIVER_KEY']}&path={path}&n={n}"
        return json.dumps({"method": "GET", "url": url, "header": {"User-Agent": [UA]}}) + "\n"
    auth = {"Authorization": [f"Bearer {v['NERUDO_AGENT_TOKEN']}"], "User-Agent": [UA]}
    w = v["NERUDO_WORKER"]
    # vegeta takes the targets in turn: on path r, each name comes again after 50,000 requests.
    run = time.strftime("%H%M%S")
    urls = {
        "base": [f"{w}/"],
        "a": [f"{w}/v1/repos/{org}/nope/stats"],
        "r": [f"{w}/v1/repos/{org}/nope-{run}-{i}/stats" for i in range(50_000)],
        "b": [f"{w}/v1/trunks/{r['trunk']}/stats" for r in repos],
        "c": [f"{w}/v1/repos/{r['name']}/stats" for r in repos],
        "d": [f"{w}/v1/hub/repos/{r['name']}" for r in repos],
        "e": [f"{w}/v1/hub/trunks/{r['trunk']}" for r in repos],
    }[path]
    # The app paths have no key, as a page view of someone not signed in.
    hdr = {"User-Agent": [UA]} if path in ("base", "d", "e") else auth
    return "".join(json.dumps({"method": "GET", "url": u, "header": hdr}) + "\n" for u in urls)


def live_status():
    p = subprocess.run(["curl", "-s", "-o", "/dev/null", "-m", "15", "-w", "%{http_code}", LIVE], capture_output=True, text=True)
    return p.stdout.strip()


class Ledger:
    def __init__(self, path, cap):
        self.path, self.cap = path, cap
        self.sent = json.load(open(path))["sent"] if os.path.exists(path) else 0

    def reserve(self, n):
        if self.sent + n > self.cap:
            sys.exit(f"stop: {self.sent} sent + {n} would pass the cap of {self.cap}")

    def add(self, n):
        self.sent += n
        json.dump({"sent": self.sent}, open(self.path, "w"))


def classify(code, body, headers):
    """Who answered: the Worker (JSON or its plain text), the Worker's uncaught exception
    (Cloudflare's 1101 page), the edge (rate limit, challenge, other Cloudflare error pages),
    or the client (status 0)."""
    h = {k.lower(): v for k, v in (headers or {}).items()}
    if code == 0:
        return "client"
    if "cf-mitigated" in h:
        return "edge_challenge"
    if "error code: 1015" in body or "1015" in body[:2000] and "rate limited" in body.lower():
        return "edge_rate_limit"
    if "1101" in body and "Worker threw exception" in body:
        return "worker_exception_1101"
    if "1102" in body and "exceeded resource limits" in body:
        return "worker_resources_1102"
    if "<html" in body.lower() and "cloudflare" in body.lower():
        return "edge_page"
    if code == 429:
        return "edge_or_worker_429"
    if body.startswith("{"):
        return "worker_json"
    return "worker_text"


def pct(xs, q):
    return float(xs[min(len(xs) - 1, int(q * len(xs)))]) if xs else None


def step(v, args, org, repos, rate, ledger, outdir):
    driver = args.via == "driver"
    outer = rate // args.n if driver else rate
    ledger.reserve(rate * args.duration + (outer * args.duration if driver else 0))
    st = live_status()
    if st != "200":
        sys.exit(f"stop: the live Worker answered {st}")
    tfile = os.path.join(outdir, f"targets-{args.path}.jsonl")
    with open(os.open(tfile, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600), "w") as fh:
        fh.write(targets(v, args.path, org, repos, args.via, args.n))
    tag = f"{args.label}-{args.path}{'-driver' if driver else ''}-{rate}"
    raw = os.path.join(outdir, f"{tag}.bin")
    # IPv4 only: this computer has an IPv6 address but no IPv6 route ("no route to host").
    host = urllib.parse.urlsplit(v["NERUDO_DRIVER" if driver else "NERUDO_WORKER"]).hostname
    ips = sorted({a[4][0] for a in socket.getaddrinfo(host, 443, socket.AF_INET, socket.SOCK_STREAM)})
    cmd = ["vegeta", "attack", *(f"-connect-to={host}:443:{ip}:443" for ip in ips), "-format=json", f"-targets={tfile}", f"-rate={outer}/s", f"-duration={args.duration}s",
           "-timeout=15s", "-http2=false", "-keepalive=true", "-workers=64", f"-max-workers={args.max_workers}",
           f"-output={raw}"]
    t0 = time.time()
    proc = subprocess.Popen(cmd, stderr=subprocess.PIPE, text=True)
    stopped = []

    def watchdog():
        while proc.poll() is None:
            time.sleep(20)
            if proc.poll() is None:
                s = live_status()
                if s != "200":
                    stopped.append(s)
                    proc.kill()
                    return

    threading.Thread(target=watchdog, daemon=True).start()
    _, err = proc.communicate()
    rep = json.loads(subprocess.run(["vegeta", "report", "-type=json", raw], capture_output=True, text=True, check=True).stdout)
    ledger.add(rep["requests"] * (1 + args.n if driver else 1))
    if stopped:
        sys.exit(f"stop: the live Worker answered {stopped[0]} during the step; load stopped")
    # Every result: per-second counts, and a few bodies of each class that is not a 200.
    enc = subprocess.run(["vegeta", "encode", "--to", "json", raw], capture_output=True, text=True, check=True).stdout
    classes, samples, per_sec = collections.Counter(), {}, collections.defaultdict(collections.Counter)
    # Through the driver: the calls it made, by the second its request started.
    inner, in_samples, in_sec, in_ms = collections.Counter(), {}, collections.defaultdict(collections.Counter), []
    start = None
    for line in enc.splitlines():
        r = json.loads(line)
        body = base64.b64decode(r.get("body") or b"").decode("utf-8", "replace")
        ok = 200 <= r["code"] < 300 or (args.path in UNKNOWN and r["code"] == 404 and "unknown repo" in body)
        cls = "ok" if ok else classify(r["code"], body, {k: ";".join(x) for k, x in (r.get("headers") or {}).items()})
        classes[cls] += 1
        ts = r["timestamp"]
        sec = int(time.mktime(time.strptime(ts[:19], "%Y-%m-%dT%H:%M:%S")))
        start = sec if start is None else min(start, sec)
        per_sec[sec][cls] += 1
        if driver:
            if cls == "ok":
                d = json.loads(body)
                for code, c in d["codes"].items():
                    ic = "ok" if code == "200" or (args.path in UNKNOWN and code == "404") else f"http_{code}" if code != "0" else "threw"
                    inner[ic] += c
                    in_sec[sec][ic] += c
                in_ms.extend(d["ms"])
                for code, b in d["errors"]:
                    k = "ok" if args.path in UNKNOWN and code == 404 else f"http_{code}" if code else "threw"
                    if k != "ok" and len(in_samples.setdefault(k, [])) < 3:
                        in_samples[k].append({"code": code, "body": redact(re.sub(r"\s+", " ", b)[:300])})
            else:
                inner["driver_failed"] += args.n
                in_sec[sec]["driver_failed"] += args.n
        if cls != "ok" and len(samples.setdefault(cls, [])) < 3:
            samples[cls].append({"code": r["code"], "error": redact(r.get("error", "")[:200]),
                                 "body": redact(re.sub(r"\s+", " ", body)[:300]),
                                 "cf_ray": (r.get("headers") or {}).get("Cf-Ray", [""])[0]})
    lat = rep["latencies"]
    good = sum(c for k, c in classes.items() if k == "ok")
    res = {
        "path": args.path, "label": args.label, "offered_rps": rate, "duration_s": args.duration,
        "requests": rep["requests"], "attack_rps": round(rep["rate"], 1), "ok": good,
        "ok_rps": round(good / max(rep["duration"] / 1e9, 1e-9), 1), "ok_ratio": round(good / max(rep["requests"], 1), 4),
        "latency_ms": {k: round(lat[k] / 1e6, 1) for k in ("50th", "95th", "99th", "max", "mean")},
        "status_codes": rep["status_codes"], "classes": dict(classes), "samples": samples,
        "vegeta_errors": [redact(e)[:200] for e in rep.get("errors", [])[:5]],
        "per_second": [{"t": s - start, **c} for s, c in sorted(per_sec.items())],
        "wall_s": round(time.time() - t0, 1), "live_status_before": st,
        "vegeta_stderr": redact(err.strip())[-300:],
    }
    if driver:
        # The numbers that count are the calls to the Worker under test.
        in_ms.sort()
        calls, ok = sum(inner.values()), inner["ok"]
        res["outer"] = {k: res.pop(k) for k in ("attack_rps", "ok_rps", "ok_ratio", "latency_ms", "status_codes", "classes", "samples", "per_second")}
        res["outer"]["requests"], res["outer"]["ok"] = res["requests"], res["ok"]
        res.update({
            "via": "driver", "n": args.n, "requests": calls, "ok": ok, "attack_rps": round(calls / args.duration, 1),
            "ok_rps": round(ok / args.duration, 1), "ok_ratio": round(ok / max(calls, 1), 4),
            "latency_ms": {"50th": pct(in_ms, 0.5), "95th": pct(in_ms, 0.95), "99th": pct(in_ms, 0.99),
                           "max": float(in_ms[-1]) if in_ms else None, "mean": round(sum(in_ms) / max(len(in_ms), 1), 1)},
            "status_codes": {k: c for k, c in inner.items()}, "classes": dict(inner), "samples": in_samples,
            "per_second": [{"t": s - start, **c} for s, c in sorted(in_sec.items())],
        })
    json.dump(res, open(os.path.join(outdir, f"{tag}.json"), "w"), indent=1)
    print(f"{tag}: sent {res['attack_rps']}/s, ok {res['ok_rps']}/s ({res['ok_ratio']:.2%}), "
          f"p50 {res['latency_ms']['50th']} p95 {res['latency_ms']['95th']} p99 {res['latency_ms']['99th']} ms, "
          f"classes {res['classes']}, outer {dict(classes)}; ledger {ledger.sent}", flush=True)
    return res


def ladder(args):
    v = load_vars(args.vars)
    named = json.load(open(args.repos))
    org, repos = named["org"], named["repos"]
    soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
    resource.setrlimit(resource.RLIMIT_NOFILE, (min(max(soft, 20000), hard), hard))
    os.makedirs(args.outdir, exist_ok=True)
    ledger = Ledger(os.path.join(args.outdir, "ledger.json"), args.cap)
    for rate in [int(r) for r in args.rates.split(",")]:
        res = step(v, args, org, repos, rate, ledger, args.outdir)
        bad = res["requests"] - res["ok"]
        outer = res.get("outer", res)["classes"]
        if any(k.startswith("edge") for k in outer):
            sys.exit("stop: Cloudflare edge errors (rule 8); see the samples")
        mine = ("ok", "worker_exception_1101", "worker_json", "worker_text", "threw", "driver_failed") + tuple(k for k in res["classes"] if k.startswith("http_"))
        other = sum(c for k, c in res["classes"].items() if k not in mine)
        if any(k.startswith("edge") for k in res["classes"]):
            sys.exit("stop: Cloudflare edge errors (rule 8); see the samples")
        if other > 0.05 * res["requests"]:
            sys.exit("stop: more than 5% errors that are not the Worker's own answer; see the samples")
        if bad > 0.05 * res["requests"] and not args.past_ceiling:
            print("ceiling: more than 5% errors; the ladder stops here", flush=True)
            return
        time.sleep(args.pause)


def report(args):
    steps = [json.load(open(os.path.join(args.outdir, f))) for f in os.listdir(args.outdir)
             if re.fullmatch(r"[a-z0-9]+-(base|a|r|b|c|d|e)(-driver)?-\d+\.json", f)]
    steps.sort(key=lambda s: (s["label"], s.get("via", "direct"), s["path"], s["offered_rps"]))
    for s in steps:
        s.pop("vegeta_stderr", None)
    meta = json.load(open(args.meta)) if args.meta else {}
    json.dump({**meta, "steps": steps}, open(args.out, "w"), indent=1)
    print(f"{len(steps)} steps -> {args.out}")


def tail(args):
    raw = open(args.file).read()
    dec, i, events = json.JSONDecoder(), 0, []
    while True:
        while i < len(raw) and raw[i].isspace():
            i += 1
        if i >= len(raw):
            break
        e, i = dec.raw_decode(raw, i)
        events.append(e)
    rows, minutes = collections.Counter(), collections.defaultdict(collections.Counter)
    for e in events:
        req = (e.get("event") or {}).get("request") or {}
        path = re.sub(r"^https?://[^/]+", "", req.get("url", "")).split("?")[0]
        path = re.sub(r"nope-\d+-\d+", "nope-N", re.sub(r"[a-z]+-\d+-\d+", "T-N", re.sub(r"/r\d+(/|$)", r"/rN\1", path)))
        status = ((e.get("event") or {}).get("response") or {}).get("status")
        why = ";".join(str(x.get("message", ""))[:60] for x in e.get("exceptions", [])) or \
            ";".join(str(l.get("message"))[:60] for l in e.get("logs", []))
        minute = time.strftime("%H:%M", time.localtime(e["eventTimestamp"] / 1000))
        rows[(minute, path, e.get("outcome"), status, redact(why))] += 1
        minutes[minute][path] += 1
    out = {"events": len(events), "rows": [{"minute": k[0], "path": k[1], "outcome": k[2], "status": k[3], "why": k[4], "n": n}
                                           for k, n in sorted(rows.items())]}
    # The Hub object's own events (/repo/{org}/{repo}) for each sampled request by name.
    org = args.org
    out["hub_per_request"] = {m: {"by_name": c[f"/v1/repos/{org}/rN/stats"], "hub_lookups": c[f"/repo/{org}/rN"],
                                  "unknown_by_name": c[f"/v1/repos/{org}/nope/stats"], "unknown_hub_lookups": c[f"/repo/{org}/nope"],
                                  "app_pages": c[f"/v1/hub/repos/{org}/rN"] + c["/v1/hub/trunks/T-N"], "trunk_lookups": c["/trunk/T-N"]}
                              for m, c in sorted(minutes.items())}
    json.dump(out, open(args.out, "w"), indent=1) if args.out else None
    for r in out["rows"]:
        print(r)
    for m, c in out["hub_per_request"].items():
        print(m, c)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("setup")
    s.add_argument("--vars", required=True)
    s.add_argument("--out", required=True)
    s.add_argument("--org", default="hubscale")
    s.add_argument("--prefix", default="hub", help="trunk ids are <prefix>-<i>-<time>")
    s.add_argument("--public", action="store_true", help="public repos (for the app paths d and e)")
    l = sub.add_parser("ladder")
    l.add_argument("--vars", required=True)
    l.add_argument("--repos", required=True, help="the file that setup wrote")
    l.add_argument("--path", required=True, choices=["base", "a", "r", "b", "c", "d", "e"])
    l.add_argument("--label", required=True, help="before or after (the cache)")
    l.add_argument("--rates", default="250,500,1000,2000,4000")
    l.add_argument("--via", choices=["direct", "driver"], default="direct")
    l.add_argument("--n", type=int, default=40, help="calls per driver request")
    l.add_argument("--duration", type=int, default=60)
    l.add_argument("--pause", type=int, default=15, help="seconds between steps")
    l.add_argument("--max-workers", type=int, default=4000)
    l.add_argument("--cap", type=int, default=5_000_000, help="requests in all runs, at most")
    l.add_argument("--past-ceiling", action="store_true", help="climb on after a step with more than 5%% errors")
    l.add_argument("--outdir", required=True)
    r = sub.add_parser("report")
    r.add_argument("--outdir", required=True)
    r.add_argument("--meta")
    r.add_argument("--out", required=True)
    t = sub.add_parser("tail")
    t.add_argument("file")
    t.add_argument("--out")
    t.add_argument("--org", default="hubscale")
    args = ap.parse_args()
    {"setup": setup, "ladder": ladder, "report": report, "tail": tail}[args.cmd](args)


if __name__ == "__main__":
    main()
