#!/usr/bin/env python3
"""Paused-queue bursts with `[ci] typecheck` on a deployed nerudo Worker with Artifacts: the
drain time, the CI runs for each landing, and the stage timings of each round. Results:
cf/scale/ci.md.

  NERUDO_VARS=<vars file> python3 -I cf/scale/ci.py --sizes 64,128,512 --caps 16,64 \\
      --rates 0,0.01,0.05 [--files 0] [--agents 4] [--tag T] --out FILE.json

The vars file gives NERUDO_WORKER (the Worker URL) and its NERUDO_ADMIN_TOKEN and
NERUDO_AGENT_TOKEN. Each burst makes a new trunk `ciq-<tag><n>-<time>` (an Artifacts repo,
and one fork for each agent) with a small TypeScript project and `[ci] typecheck`. --files
adds that many more modules to the project. The script pauses the trunk, queues N changes
that are disjoint, and resumes it. Each change adds src/changes/<id>.ts. A known fraction of
the changes (--rates; at least 1 when the rate is not 0) has a type error (TS2322): these
must be ejected as ci_failed, and every other change must land once.

The script makes load, so it refuses a Worker that is not nerudo-scale-*. Every 2 minutes it
reads the front page of the live Worker (one GET). If the reply is not HTTP 200, it stops.
"""
import argparse
import json
import os
import random
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor

os.environ.setdefault("NERUDO_HOST", "artifacts")
HERE = os.path.dirname(os.path.abspath(__file__))
CF = os.path.dirname(HERE)
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(CF, "scripts"))
import stages as T  # noqa: E402  (imports scenarios, which reads NERUDO_VARS)

S = T.S
log = T.log
CI_RULES = '[ci]\ntypecheck = { tsconfig = "tsconfig.json" }\n'
TSCONFIG = json.dumps({"compilerOptions": {"strict": True, "target": "es2022", "module": "es2022", "lib": ["es2022"],
                                           "skipLibCheck": True}, "include": ["src"]})
PRICE = "export function price(cents: number): string {\n  return (cents / 100).toFixed(2);\n}\n"


def module(i):
    """One module of the larger project: an interface, a class and two functions, about 1 KB.
    Each module imports the one before it, so the checker reads them all."""
    prev = f"import {{ Item{i - 1}, total{i - 1} }} from './m{i - 1}';\n" if i else ""
    field = f"  prev: Item{i - 1};\n" if i else ""
    tail = f"total{i - 1}(items.map((x) => x.prev))" if i else "0"
    return (f"import {{ price }} from '../price';\n{prev}"
            f"export interface Item{i} {{\n  id: number;\n  name: string;\n  cents: number;\n  tags: string[];\n{field}}}\n"
            f"export class Cart{i} {{\n  private items: Item{i}[] = [];\n  add(item: Item{i}): this {{\n    this.items.push(item);\n"
            f"    return this;\n  }}\n  label(): string {{\n    return this.items.map((x) => `${{x.name}}: ${{price(x.cents)}}`).join(', ');\n"
            f"  }}\n}}\nexport function total{i}(items: Item{i}[]): number {{\n  return items.reduce((s, x) => s + x.cents, 0) + {tail};\n}}\n"
            f"export const tags{i} = (items: Item{i}[]): Set<string> => new Set(items.flatMap((x) => x.tags));\n")


def new_trunk(tag, cap, files):
    name = f"ciq-{tag}-{time.strftime('%H%M%S')}"
    seed = os.path.join(S.SCRATCH, name)
    os.makedirs(seed)
    S.init_tree(seed)
    tree = {os.path.relpath(os.path.join(d, f), seed): open(os.path.join(d, f)).read()
            for d, _, fs in os.walk(seed) for f in fs}
    tree["nerudo.toml"] = open(os.path.join(CF, "fixtures", "bench-nerudo.toml")).read() + "\n" + CI_RULES
    tree["tsconfig.json"] = TSCONFIG
    tree["src/price.ts"] = PRICE
    tree["src/index.ts"] = "import { price } from './price';\nexport const total = price(250);\n"
    for i in range(files):
        tree[f"src/mod/m{i}.ts"] = module(i)
    body = {"trunk": name, "host": {"kind": "artifacts", "binding": "ARTIFACTS", "repo": name},
            "seed": {"files": tree, "message": "seed\n"}, "batch_cap": cap}
    out = S.api(name, "POST", "init", body, admin=True)
    assert "head" in out, S.redact(json.dumps(out))
    return name


def prepare(agent, ids, bad):
    """Commit one change for each id on trunk's head, push them all, return the land requests."""
    d = agent.dir
    S.sh("git", "-c", S.header(agent.trunk_auth), "fetch", "-q", "origin", cwd=d)
    S.sh("git", "switch", "-q", "--detach", "origin/main", cwd=d)
    base = S.sh("git", "rev-parse", "HEAD", cwd=d)
    bodies, refs = [], []
    for cid in ids:
        S.sh("git", "switch", "-q", "--detach", base, cwd=d)
        os.makedirs(os.path.join(d, "src", "changes"), exist_ok=True)
        text = (f"export const {cid}: number = 'bad';\n" if cid in bad
                else f"import {{ price }} from '../price';\nexport const {cid} = price({len(cid)});\n")
        with open(os.path.join(d, "src", "changes", f"{cid}.ts"), "w") as fh:
            fh.write(text)
        S.sh("git", "add", "-A", cwd=d)
        S.sh("git", "commit", "-q", "-m", f"{cid}: change", cwd=d)
        commit = S.sh("git", "rev-parse", "HEAD", cwd=d)
        refs.append(f"{commit}:refs/heads/land/{cid}-1")
        bodies.append({"change_id": cid, "rev": 1, "fork": agent.fork["name"], "commit": commit, "base": base})
    for i in range(0, len(refs), 64):
        S.sh("git", "-c", f"http.extraHeader=Authorization: {agent.fork['authorization']}", "push", "-q",
             agent.fork["remote"], *refs[i:i + 64], cwd=d)
    return [(agent, b) for b in bodies]


def api(*args, **kw):
    """S.api, tried again on a lost connection (this computer's network, not the Worker). A land
    request sent again for the same revision gets the same answer."""
    for attempt in range(4):
        try:
            return S.api(*args, **kw)
        except OSError as e:
            if attempt == 3:
                raise
            log(f"  {args[1]} {args[2]}: {e}; again in 2 s")
            time.sleep(2)


class Rounds:
    """GET timings keeps the last 64 rounds: read it every few seconds and keep every round."""

    def __init__(self, trunk):
        self.trunk, self.seen, self.done = trunk, {}, threading.Event()
        self.thread = threading.Thread(target=self.loop, daemon=True)

    def poll(self):
        for r in api(self.trunk, "GET", "timings").get("rounds", []):
            self.seen[(r["alarm_ms"], r["at_ms"])] = r

    def loop(self):
        while not self.done.wait(3):
            try:
                self.poll()
            except Exception as e:  # a reader error must not stop the burst
                log(f"  timings: {e}")

    def stop(self):
        self.done.set()
        self.thread.join(timeout=30)
        self.poll()
        return [self.seen[k] for k in sorted(self.seen)]


def burst(tag, n, agents_n, cap, rate, files, seed):
    t = new_trunk(tag, cap, files)
    ids = [f"c{i}" for i in range(n)]
    k = 0 if rate == 0 else max(1, round(n * rate))
    bad = set(random.Random(f"{seed}:{n}:{rate}").sample(ids, k))
    log(f"{t}: N={n}, cap {cap}, {k} with a type error ({rate:.0%}), {files} more modules, {agents_n} agents")
    agents = [T.new_agent(t, f"a{i:02d}") for i in range(agents_n)]
    share = {a.name: ids[i::agents_n] for i, a in enumerate(agents)}
    out = S.api(t, "POST", "pause", {"reason": "ci: queue the changes first"}, admin=True)
    assert "paused" in out, out
    t0 = time.time()
    with ThreadPoolExecutor(agents_n) as pool:
        work = [x for xs in pool.map(lambda a: prepare(a, share[a.name], bad), agents) for x in xs]
    prep_s = time.time() - t0
    errors = []

    def land(pair):
        agent, body = pair
        st = api(t, "POST", "land", body, key=agent.key)
        if st.get("state") != "queued":
            errors.append({"change_id": body["change_id"], "reply": S.redact(json.dumps(st))[:200]})
    with ThreadPoolExecutor(16) as pool:
        list(pool.map(land, work))
    if len(errors) > 0.05 * n:
        T.STOP.set()
        raise RuntimeError(f"{len(errors)} of {n} land requests failed: {errors[:3]}")
    before = S.api(t, "GET", "stats")
    assert before.get("queued") == n - len(errors), before
    log(f"  prepared in {prep_s:.1f} s")
    rounds = Rounds(t)
    rounds.thread.start()
    resume_ms = time.time() * 1000
    out = S.api(t, "POST", "resume", {}, admin=True)
    assert "paused" in out, out
    samples, end = [], time.time() + 900
    while time.time() < end and not T.STOP.is_set():
        st = api(t, "GET", "stats")
        samples.append({"s": round((time.time() * 1000 - resume_ms) / 1000, 2), "landed": st.get("landed"),
                        "queued": st.get("queued")})
        if st.get("queued") == 0:
            break
        time.sleep(1)
    client_drain = (time.time() * 1000 - resume_ms) / 1000
    rs = rounds.stop()
    stats = api(t, "GET", "stats")
    entries = T.full_log(t)
    # Every change is in one of two places: the log (landed once) or ejected for CI.
    landed_ids = [e["change_id"] for e in entries]
    states = {}
    with ThreadPoolExecutor(8) as pool:
        for cid, st in zip(ids, pool.map(lambda c: api(t, "GET", f"changes/{c}"), ids)):
            states[cid] = st
    wrong = []
    for cid in ids:
        st = states[cid]
        kind = (st.get("reason") or {}).get("kind")
        if cid in bad and not (st.get("state") == "ejected" and kind == "ci_failed"):
            wrong.append((cid, "bad", st.get("state"), kind))
        if cid not in bad and st.get("state") != "landed":
            wrong.append((cid, "good", st.get("state"), kind))
    dup = len(landed_ids) - len(set(landed_ids))
    landed_at = [e["landed_at_ms"] for e in entries]
    start_ms = min((r["alarm_ms"] for r in rs), default=None)
    server_drain = (max(landed_at) - start_ms) / 1000 if landed_at and start_ms else None
    ci_runs_landed = [states[c].get("ci_runs") for c in ids if states[c].get("state") == "landed"]
    row = {"label": f"{tag} N={n} cap={cap} rate={rate}", "trunk": t, "n": n, "agents": agents_n, "batch_cap": cap,
           "rate": rate, "bad": sorted(bad, key=lambda c: int(c[1:])), "files": files, "prep_s": round(prep_s, 1),
           "land_errors": errors, "landed": stats.get("landed"), "ejected_ci": stats.get("ejected_ci"),
           "ejected_other": sum(stats.get(k, 0) for k in ("ejected_conflict", "ejected_check", "ejected_other")),
           "ci_runs": stats.get("ci_runs"), "batches": stats.get("batches"), "git_requests": stats.get("git_requests"),
           "log_entries": len(entries), "duplicates": dup, "wrong": wrong,
           "ci_runs_per_landing": round(stats.get("ci_runs", 0) / stats["landed"], 3) if stats.get("landed") else None,
           "receipt_ci_runs": {str(v): ci_runs_landed.count(v) for v in sorted(set(ci_runs_landed), key=lambda x: x or 0)},
           "server_drain_s": round(server_drain, 2) if server_drain else None,
           "client_drain_s": round(client_drain, 2), "rounds": rs, "samples": samples}
    ci = sum(r.get("ci_ms", 0) for r in rs) / 1000
    push = sum(r.get("push_ms", 0) for r in rs) / 1000
    log(f"  landed {row['landed']}/{n - k}, ejected_ci {row['ejected_ci']}/{k}, other {row['ejected_other']}, "
        f"{row['batches']} batches, {row['ci_runs']} CI runs ({row['ci_runs_per_landing']} per landing), drain "
        f"{row['server_drain_s']} s (client {row['client_drain_s']} s), rounds {len(rs)}: ci {ci:.2f} s, push {push:.2f} s"
        + (f"; WRONG {wrong[:5]}" if wrong else "") + (f"; {dup} DUPLICATES" if dup else ""))
    return row


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--sizes", default="64,128,512")
    p.add_argument("--caps", default="16,64")
    p.add_argument("--rates", default="0,0.01,0.05")
    p.add_argument("--files", type=int, default=0, help="more modules in the project")
    p.add_argument("--agents", type=int, default=4)
    p.add_argument("--seed", default="ciq")
    p.add_argument("--tag", default="b")
    p.add_argument("--start", type=int, default=0, help="skip the first bursts of the matrix")
    p.add_argument("--out", required=True)
    args = p.parse_args()
    if S.HOST != "artifacts" or "://nerudo-scale-" not in S.WORKER:
        sys.exit(f"ci: refusing to load {S.WORKER}: only a nerudo-scale-* Worker with Artifacts")
    if not T.live_ok():
        sys.exit("ci: the live Worker is not healthy; not starting")
    threading.Thread(target=T.health_loop, daemon=True).start()
    rows = []
    i = 0
    try:
        for cap in (int(x) for x in args.caps.split(",")):
            for rate in (float(x) for x in args.rates.split(",")):
                for n in (int(x) for x in args.sizes.split(",")):
                    if T.STOP.is_set():
                        break
                    if i < args.start:
                        i += 1
                        continue
                    rows.append(burst(f"{args.tag}{i}", n, args.agents, cap, rate, args.files, args.seed))
                    i += 1
                    with open(args.out, "w") as fh:  # keep what is done if a later burst fails
                        json.dump({"worker": S.WORKER.split("//")[1].split(".")[0], "argv": sys.argv[1:], "bursts": rows,
                                   "live_health": T.HEALTH}, fh, indent=1)
    finally:
        T.STOP.set()
        with open(args.out, "w") as fh:
            json.dump({"worker": S.WORKER.split("//")[1].split(".")[0], "argv": sys.argv[1:], "bursts": rows,
                       "live_health": T.HEALTH}, fh, indent=1)
        log(f"wrote {args.out}")


if __name__ == "__main__":
    main()
