#!/usr/bin/env python3
"""The latency of a type check through the CI service binding (cf/scale/ci.md), measured with
the driver Worker ci_driver.js. Times are on this computer, around one request to the driver;
the round trip of a request that does nothing is measured too, and subtracted in ci.md.

  python3 -I cf/scale/ci_latency.py --url https://<driver>.workers.dev/measure \\
      --token-file <file with DRIVER_TOKEN=...> --out FILE.json

It sends one request at a time (approximately 2 each second at most).
"""
import argparse
import json
import statistics
import time
import urllib.request
from urllib.parse import urlsplit


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--url", required=True)
    p.add_argument("--token-file", required=True)
    p.add_argument("--out", required=True)
    args = p.parse_args()
    target = urlsplit(args.url)
    hostname = target.hostname or ""
    if target.scheme != "https" or not hostname.startswith("nerudo-scale-") or not hostname.endswith(".workers.dev"):
        raise SystemExit("ci_latency: only a dedicated HTTPS nerudo-scale-* driver Worker on workers.dev")
    tok = open(args.token_file).read().split("=", 1)[1].strip()

    def call(body):
        req = urllib.request.Request(args.url, method="POST", data=json.dumps(body).encode(), headers={
            "Authorization": f"Bearer {tok}", "Content-Type": "application/json", "User-Agent": "nerudo-scale-ciq/0.1"})
        t0 = time.time()
        urllib.request.urlopen(req, timeout=300).read()
        return round((time.time() - t0) * 1000)

    res = {"rtt": [call({"noop": True}) for _ in range(5)], "size": {}, "conc": {}, "bad": {}, "overlap": {}}
    for n in (0, 10, 50, 100, 200, 400, 800):
        call({"files": n})  # the first call of a size warms the isolates
        res["size"][n] = [call({"files": n}) for _ in range(5)]
        print("size", n, res["size"][n], flush=True)
    for n in (10, 400):
        for k in (2, 4, 6):
            res["conc"][f"{n}x{k}"] = {"seq": [call({"files": n, "checks": k}) for _ in range(3)],
                                       "par": [call({"files": n, "checks": k, "parallel": True}) for _ in range(3)]}
            print("checks at once", n, k, res["conc"][f"{n}x{k}"], flush=True)
    for n in (10, 400):
        res["bad"][n] = [call({"files": n, "bad": True}) for _ in range(5)]
        for label, body in [("check", {"files": n}), ("wait800", {"files": -1, "wait_ms": 800}),
                            ("both", {"files": n, "wait_ms": 800})]:
            res["overlap"][f"{n}-{label}"] = [call(body) for _ in range(5)]
            print("overlap", n, label, statistics.median(res["overlap"][f"{n}-{label}"]), flush=True)
    with open(args.out, "w") as fh:
        json.dump(res, fh, indent=1)


if __name__ == "__main__":
    main()
