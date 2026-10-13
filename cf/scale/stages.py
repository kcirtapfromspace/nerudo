#!/usr/bin/env python3
"""Paused-queue bursts on a deployed nerudo Worker with Artifacts, and where each round of the
landing loop spends its time (GET .../timings). Results: cf/scale/stages.md.

  NERUDO_VARS=<vars file> python3 -I cf/scale/stages.py --sizes 64,128 [--cap 64] [--agents 4]
      [--readers] [--tag T] --out FILE.json

The vars file gives NERUDO_WORKER (the Worker URL) and its NERUDO_ADMIN_TOKEN and
NERUDO_AGENT_TOKEN. Each burst makes a new trunk `stages-<tag><n>-<time>` (an Artifacts repo,
and one fork for each agent), pauses it, queues N changes that are disjoint (each adds
changes/<id>.txt), and resumes it. The agents push all their commits in one git push, then
send the land requests in parallel, so the preparation is short.

With --readers, other clients read the same trunk while the queue drains: GET queue at about
20 requests per second, and 50 long polls (GET changes/<id>?wait=60) that start again until
their change lands.

The script makes load, so it refuses a Worker that is not nerudo-scale-*. Every 2 minutes
it reads the front page of the live Worker (one GET). If the reply is not HTTP 200, it stops.
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor

os.environ.setdefault("NERUDO_HOST", "artifacts")
HERE = os.path.dirname(os.path.abspath(__file__))
CF = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(CF, "scripts"))
import scenarios as S  # noqa: E402  (reads NERUDO_VARS at import)

LIVE = "https://nerudo.patrickdeutsch.workers.dev/"
STOP = threading.Event()
HEALTH = []
STAGES = ["head_ms", "load_ms", "fetch_ms", "gate_ms", "divert_ms", "pick_ms", "apply_ms", "ci_ms", "pack_ms",
          "push_ms", "record_ms"]


def log(msg):
    print(time.strftime("%H:%M:%S"), msg, flush=True)


def live_ok():
    code = subprocess.run(["curl", "-s", "-o", "/dev/null", "-w", "%{http_code}", "--max-time", "20", LIVE],
                          capture_output=True, text=True).stdout.strip()
    HEALTH.append({"at": time.strftime("%H:%M:%S"), "status": code})
    if code != "200":
        log(f"STOP: the live Worker answered {code}")
        STOP.set()
    return code == "200"


def health_loop():
    while not STOP.wait(120):
        live_ok()


def new_trunk(tag, cap):
    name = f"stages-{tag}-{time.strftime('%H%M%S')}"
    seed = os.path.join(S.SCRATCH, name)
    os.makedirs(seed)
    S.init_tree(seed)
    shutil.copy(os.path.join(CF, "fixtures", "bench-nerudo.toml"), os.path.join(seed, "nerudo.toml"))
    files = {os.path.relpath(os.path.join(d, f), seed): open(os.path.join(d, f)).read()
             for d, _, fs in os.walk(seed) for f in fs}
    body = {"trunk": name, "host": {"kind": "artifacts", "binding": "ARTIFACTS", "repo": name},
            "seed": {"files": files, "message": "seed\n"}}
    if cap:
        body["batch_cap"] = cap
    out = S.api(name, "POST", "init", body, admin=True)
    assert "head" in out, S.redact(json.dumps(out))
    return name


def new_agent(trunk, name):
    """A new agent with its fork and a clone of trunk. Artifacts sometimes answers a clone
    with HTTP 500; then the next try uses a new agent name (the fork of the first stays)."""
    for attempt in range(3):
        try:
            return S.Agent(trunk, name + "r" * attempt)
        except RuntimeError as e:
            log(f"  agent {name}: {str(e)[-120:]}")
    raise RuntimeError(f"agent {name}: 3 tries failed")


def prepare(agent, ids):
    """Commit one change for each id on trunk's head, push them all in one git push, and
    return the land requests."""
    d = agent.dir
    base = S.sh("git", "rev-parse", "HEAD", cwd=d)
    bodies, refs = [], []
    for cid in ids:
        S.sh("git", "switch", "-q", "--detach", base, cwd=d)
        with open(os.path.join(d, "changes", f"{cid}.txt"), "w") as fh:
            fh.write(cid + "\n")
        S.sh("git", "add", "-A", cwd=d)
        S.sh("git", "commit", "-q", "-m", f"{cid}: change", cwd=d)
        commit = S.sh("git", "rev-parse", "HEAD", cwd=d)
        refs.append(f"{commit}:refs/heads/land/{cid}-1")
        bodies.append({"change_id": cid, "rev": 1, "fork": agent.fork["name"], "commit": commit, "base": base})
    for i in range(0, len(refs), 64):
        S.sh("git", "-c", f"http.extraHeader=Authorization: {agent.fork['authorization']}", "push", "-q",
             agent.fork["remote"], *refs[i:i + 64], cwd=d)
    return [(agent, b) for b in bodies]


class Readers:
    """GET queue at about `qps`, and `polls` long polls on changes that are still queued."""

    def __init__(self, trunk, cids, qps=20, polls=50):
        self.trunk, self.cids, self.qps, self.polls = trunk, cids, qps, polls
        self.done = threading.Event()
        self.lat = {"queue": [], "change": []}
        self.errors = {"queue": 0, "change": 0}
        self.lock = threading.Lock()
        self.threads = []

    def call(self, kind, path):
        t0 = time.time()
        out = S.api(self.trunk, "GET", path)
        ms = (time.time() - t0) * 1000
        with self.lock:
            self.lat[kind].append(ms)
            if "http_error" in out:
                self.errors[kind] += 1
        return out

    def queue_loop(self, rate):
        while not self.done.is_set() and not STOP.is_set():
            t0 = time.time()
            try:
                self.call("queue", "queue")
            except Exception:
                with self.lock:
                    self.errors["queue"] += 1
            time.sleep(max(0.0, 1 / rate - (time.time() - t0)))

    def poll_loop(self, cid):
        while not self.done.is_set() and not STOP.is_set():
            try:
                st = self.call("change", f"changes/{cid}?wait=60")
            except Exception:
                with self.lock:
                    self.errors["change"] += 1
                time.sleep(1)
                continue
            if st.get("state") in ("landed", "ejected"):
                return

    def start(self):
        workers = 4
        for _ in range(workers):
            self.threads.append(threading.Thread(target=self.queue_loop, args=(self.qps / workers,), daemon=True))
        # The changes that land last wait longest.
        for cid in self.cids[-self.polls:]:
            self.threads.append(threading.Thread(target=self.poll_loop, args=(cid,), daemon=True))
        for t in self.threads:
            t.start()

    def stop(self):
        self.done.set()
        for t in self.threads:
            t.join(timeout=70)

    def summary(self):
        def q(v, f):
            v = sorted(v)
            return round(v[min(len(v) - 1, int(len(v) * f))], 1) if v else None
        return {k: {"calls": len(v), "errors": self.errors[k], "p50_ms": q(v, .5), "p95_ms": q(v, .95),
                    "max_ms": round(max(v), 1) if v else None} for k, v in self.lat.items()}


def full_log(trunk):
    out, since = [], 0
    while True:
        page = S.api(trunk, "GET", f"log?since={since}")
        if not isinstance(page, list) or not page:
            return out
        out += page
        since = page[-1]["seq"]


def burst(tag, n, agents_n, cap, readers):
    t = new_trunk(tag, cap)
    log(f"{t}: N={n}, {agents_n} agents, batch_cap {cap or 'default (16)'}{', readers' if readers else ''}")
    agents = [new_agent(t, f"a{i:02d}") for i in range(agents_n)]
    ids = [f"g{i}" for i in range(n)]
    share = {a.name: ids[i::agents_n] for i, a in enumerate(agents)}
    out = S.api(t, "POST", "pause", {"reason": "stages: queue the changes first"}, admin=True)
    assert "paused" in out, out
    t0 = time.time()
    with ThreadPoolExecutor(agents_n) as pool:
        work = [x for xs in pool.map(lambda a: prepare(a, share[a.name]), agents) for x in xs]
    prep_s = time.time() - t0
    errors = []

    def land(pair):
        agent, body = pair
        st = S.api(t, "POST", "land", body, key=agent.key)
        if st.get("state") != "queued":
            errors.append({"change_id": body["change_id"], "reply": S.redact(json.dumps(st))[:200]})
    with ThreadPoolExecutor(16) as pool:
        list(pool.map(land, work))
    land_s = time.time() - t0 - prep_s
    if len(errors) > 0.05 * n:
        STOP.set()
        raise RuntimeError(f"{len(errors)} of {n} land requests failed: {errors[:3]}")
    before = S.api(t, "GET", "stats")
    assert before.get("queued") == n - len(errors), before
    log(f"  prepared in {prep_s:.1f} s, queued in {land_s:.1f} s")
    rd = Readers(t, ids) if readers else None
    if rd:
        rd.start()
        time.sleep(2)
    resume_ms = time.time() * 1000
    out = S.api(t, "POST", "resume", {}, admin=True)
    assert "paused" in out, out
    samples, end = [], time.time() + 900
    while time.time() < end and not STOP.is_set():
        st = S.api(t, "GET", "stats")
        el = time.time() * 1000 - resume_ms
        samples.append({"s": round(el / 1000, 2), "landed": st.get("landed"), "queued": st.get("queued")})
        if st.get("queued") == 0:
            break
        time.sleep(1)
    client_drain = (time.time() * 1000 - resume_ms) / 1000
    if rd:
        rd.stop()
    stats = S.api(t, "GET", "stats")
    rounds = S.api(t, "GET", "timings").get("rounds", [])
    entries = full_log(t)
    landed_at = [e["landed_at_ms"] for e in entries]
    start_ms = min((r["alarm_ms"] for r in rounds), default=None)
    server_drain = (max(landed_at) - start_ms) / 1000 if landed_at and start_ms else None
    first_landing = (min(landed_at) - start_ms) / 1000 if landed_at and start_ms else None
    ejected = sum(stats.get(k, 0) for k in ("ejected_conflict", "ejected_check", "ejected_other", "ejected_ci"))
    row = {"label": f"{tag} N={n}", "trunk": t, "n": n, "agents": agents_n, "batch_cap": cap or 16, "readers": bool(readers),
           "prep_s": round(prep_s, 1), "land_errors": errors, "landed": stats.get("landed"), "ejected": ejected,
           "log_entries": len(entries), "batches": stats.get("batches"), "git_requests": stats.get("git_requests"),
           "server_drain_s": round(server_drain, 2) if server_drain else None,
           "server_first_landing_s": round(first_landing, 2) if first_landing is not None else None,
           "client_drain_s": round(client_drain, 2), "rounds": rounds, "samples": samples,
           "reader_stats": rd.summary() if rd else None}
    log(f"  landed {row['landed']}/{n}, ejected {ejected}, {row['batches']} batches, server drain {row['server_drain_s']} s "
        f"(first landing {row['server_first_landing_s']} s), client {row['client_drain_s']} s, git requests {row['git_requests']}")
    print_stages(rounds)
    if rd:
        log(f"  readers: {json.dumps(rd.summary())}")
    return row


def print_stages(rounds):
    if not rounds:
        return
    tot = {k: sum(r.get(k, 0) for r in rounds) for k in STAGES + ["round_ms", "events_ms"]}
    fetched = sum(r["fetched"] for r in rounds)
    log("  stage totals (s): " + ", ".join(f"{k[:-3]} {v / 1000:.2f}" for k, v in tot.items()) + f"; fetches {fetched}")
    for r in rounds if len(rounds) <= 6 else rounds[:3] + [{"...": ""}] + rounds[-2:]:
        if "..." in r:
            log("   ...")
            continue
        log(f"   batch {r['batch']}: queued {r['queued']} fetched {r['fetched']} ({r['fetch_ms']:.0f} ms, max {r['fetch_max_ms']:.0f}) "
            f"load {r['load_ms']:.0f} gate {r['gate_ms']:.0f} pick {r['pick_ms']:.0f} apply {r['apply_ms']:.0f} "
            f"pack {r['pack_ms']:.0f} push {r['push_ms']:.0f} record {r['record_ms']:.0f} (events {r['events_ms']:.0f}) "
            f"round {r['round_ms']:.0f} landed {r['landed']}")


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--sizes", default="64,128")
    p.add_argument("--agents", type=int, default=4)
    p.add_argument("--cap", type=int, default=0, help="batch_cap; 0 keeps the default (16)")
    p.add_argument("--readers", action="store_true")
    p.add_argument("--tag", default="b")
    p.add_argument("--out", required=True)
    args = p.parse_args()
    if S.HOST != "artifacts" or "://nerudo-scale-" not in S.WORKER:
        sys.exit(f"stages: refusing to load {S.WORKER}: only a nerudo-scale-* Worker with Artifacts")
    if not live_ok():
        sys.exit("stages: the live Worker is not healthy; not starting")
    threading.Thread(target=health_loop, daemon=True).start()
    rows = []
    try:
        for i, n in enumerate(int(x) for x in args.sizes.split(",")):
            if STOP.is_set():
                break
            rows.append(burst(f"{args.tag}{i}", n, args.agents, args.cap, args.readers))
    finally:
        STOP.set()
        with open(args.out, "w") as fh:
            json.dump({"worker": S.WORKER.split("//")[1].split(".")[0], "argv": sys.argv[1:], "bursts": rows,
                       "live_health": HEALTH}, fh, indent=1)
        log(f"wrote {args.out}")


if __name__ == "__main__":
    main()
