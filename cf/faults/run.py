#!/usr/bin/env python3
"""Fault tests for the nerudo Worker on this machine (cf/faults/README.md).

Run it with `nix run .#faults` from cf/. The harness starts these local processes:

  toxiproxy  sits between every client and the git host, to add delay and reset connections
  Gitea      the git host (the Forgejo API; Forgejo is marked broken on Darwin in nixpkgs)
  wrangler   `wrangler dev` with three Workers: nerudo, a CI stub, and the real nerudo-ci
  control    a small HTTP server in this process: it tells the CI stub what to do, and
             counts each CI request

Cases:

  parity     existing scenarios from scripts/scenarios.py, against this stack
  ci_down    CI answers 503 for D seconds on K trunks, then comes back
  ci_hang    each CI request hangs for H seconds, then CI comes back
  git_flaky  the git host adds delay and resets connections while changes land
  git_hang   the git host stops answering, first its replies and then its requests
  kill9      SIGKILL the Worker processes while a queue drains, then start them again
  growth     the time to drain N queued disjoint changes, for N that doubles

Each case checks hard invariants: no change is ejected for an infrastructure fault, each
change lands exactly once, and `nerudo verify` passes. Each case also records measurements.
Some measurements have a budget that the code misses today. These are known gaps (KNOWN_GAPS):
they do not fail the run unless you give --strict. When a known gap passes, the report says so.

This is version 1, and it is not hermetic. It uses the Worker build and the CLI from the
working tree, and it builds them with your cargo and worker-build if they are missing or older
than the sources. The local runtime does not enforce Cloudflare's limits (128 MB per isolate,
requests per object, Artifacts). Measure those on a deployed Worker.
"""
import argparse
import collections
import http.server
import importlib
import json
import math
import os
import random
import secrets
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor

HERE = os.path.dirname(os.path.abspath(__file__))
CF = os.path.dirname(HERE)
ROOT = os.path.dirname(CF)

KNOWN_GAPS = {}
# Words that would show, in the queue or a change, that CI is the reason for the wait.
CI_DOWN_WORDS = ("ci_unavailable", "ci_unreachable", "unreachable", "ci_down", "ci_error", "ci down")
# The deadlines of the Worker (ci.rs TIMEOUT_MS, transport.rs ANSWER_MS), in seconds.
CI_DEADLINE_S = 25
GIT_DEADLINE_S = 30

CI_RULES = '[ci]\ntypecheck = { tsconfig = "tsconfig.json" }\n'
TS_FILES = {
    "tsconfig.json": json.dumps({"compilerOptions": {"strict": True, "target": "es2022", "module": "es2022",
                                                     "lib": ["es2022"], "skipLibCheck": True}, "include": ["src"]}),
    "src/price.ts": "export function price(cents: number): string {\n  return (cents / 100).toFixed(2);\n}\n",
}


def log(msg=""):
    print(msg, flush=True)


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def fetch_json(method, url, body=None, headers=None, timeout=30):
    req = urllib.request.Request(url, method=method, data=json.dumps(body).encode() if body is not None else None,
                                 headers={"Content-Type": "application/json", **(headers or {})})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        raw = r.read()
    return json.loads(raw) if raw.strip() else None


def newest_mtime(*dirs):
    newest = 0.0
    for d in dirs:
        for base, _, files in os.walk(d):
            for f in files:
                if f.endswith((".rs", ".toml")):
                    newest = max(newest, os.path.getmtime(os.path.join(base, f)))
    return newest


def ensure_built(no_build):
    """The Worker build and the CLI come from the working tree (version 1 is not hermetic)."""
    sources = newest_mtime(*(os.path.join(CF, c) for c in ("nerudo-worker", "nerudo-core", "nerudo-git")))
    wasm = os.path.join(CF, "nerudo-worker", "build", "index_bg.wasm")
    cli = os.path.join(os.environ.get("CARGO_TARGET_DIR", os.path.join(CF, "target")), "debug", "nerudo")
    cli_sources = max(sources, newest_mtime(os.path.join(CF, "nerudo-cli")))
    jobs = []
    if not os.path.exists(wasm) or os.path.getmtime(wasm) < sources:
        jobs.append((["worker-build", "--release"], os.path.join(CF, "nerudo-worker")))
    if not os.path.exists(cli) or os.path.getmtime(cli) < cli_sources:
        jobs.append((["cargo", "build", "-q", "-p", "nerudo-cli"], CF))
    for cmd, cwd in jobs:
        if no_build:
            sys.exit(f"faults: {cwd} needs `{' '.join(cmd)}` (it is missing or older than the sources)")
        log(f"build: {' '.join(cmd)} (in {os.path.relpath(cwd, ROOT)})")
        try:
            subprocess.run(cmd, cwd=cwd, check=True, stdout=subprocess.DEVNULL)
        except FileNotFoundError:
            sys.exit(f"faults: `{cmd[0]}` is not on PATH. Version 1 builds with your Rust toolchain.")


class Control:
    """Tells the CI stub what to do (ci-stub.js), and records the time of each CI request."""

    def __init__(self, port):
        self.mode = "pass"
        self.hits = []
        self.lock = threading.Lock()
        outer = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                with outer.lock:
                    outer.hits.append(time.time())
                    mode = outer.mode
                self.send_response(200)
                self.end_headers()
                self.wfile.write(mode.encode())

            def log_message(self, *a):
                pass

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def set(self, mode):
        with self.lock:
            self.mode = mode

    def since(self, t):
        with self.lock:
            return [h for h in self.hits if h >= t]


class Stack:
    """The local processes. Each runs in its own process group, so a kill reaches its children."""

    def __init__(self, run, seed):
        self.run = run
        self.seed = seed
        self.procs = {}
        self.port = {n: free_port() for n in ("toxi", "git", "gitea", "worker", "control")}
        self.git_url = f"http://127.0.0.1:{self.port['git']}"  # through toxiproxy
        self.worker_url = f"http://127.0.0.1:{self.port['worker']}"
        self.admin = "faults-admin-" + secrets.token_hex(12)
        self.agent = "faults-agent-" + secrets.token_hex(12)

    def spawn(self, name, cmd, cwd=None, env=None):
        out = open(os.path.join(self.run, f"{name}.log"), "a")
        self.procs[name] = subprocess.Popen(cmd, cwd=cwd or self.run, env=env, stdout=out, stderr=subprocess.STDOUT,
                                            start_new_session=True)

    def kill(self, name):
        p = self.procs.pop(name, None)
        if p and p.poll() is None:
            try:
                os.killpg(p.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            p.wait()

    def stop(self):
        for name in list(self.procs):
            self.kill(name)
        if hasattr(self, "control"):
            self.control.server.shutdown()

    def start(self):
        self.control = Control(self.port["control"])
        self.start_toxiproxy()
        self.start_gitea()
        self.write_worker_configs()
        self.start_worker()

    # ------------------------------------------------------------------ toxiproxy

    def toxi(self, method, path, body=None):
        return fetch_json(method, f"http://127.0.0.1:{self.port['toxi']}{path}", body)

    def start_toxiproxy(self):
        self.spawn("toxiproxy", ["toxiproxy-server", "-host", "127.0.0.1", "-port", str(self.port["toxi"]),
                                 "-seed", str(self.seed)])
        self.wait_for(lambda: self.toxi("GET", "/version"), "toxiproxy")
        self.toxi("POST", "/proxies", {"name": "git", "listen": f"127.0.0.1:{self.port['git']}",
                                       "upstream": f"127.0.0.1:{self.port['gitea']}", "enabled": True})

    def toxic(self, name, kind, attributes, stream="downstream", toxicity=1.0):
        self.toxi("POST", "/proxies/git/toxics", {"name": name, "type": kind, "stream": stream,
                                                  "toxicity": toxicity, "attributes": attributes})

    def untoxic(self, name):
        self.toxi("DELETE", f"/proxies/git/toxics/{name}")

    def clear_toxics(self):
        for t in self.toxi("GET", "/proxies/git/toxics") or []:
            self.toxi("DELETE", f"/proxies/git/toxics/{t['name']}")

    # ------------------------------------------------------------------ Gitea

    def start_gitea(self):
        d = os.path.join(self.run, "gitea")
        os.makedirs(os.path.join(d, "home"))
        ini = os.path.join(d, "app.ini")
        # ROOT_URL is the toxiproxy address, so each clone URL that Gitea gives goes through it.
        with open(ini, "w") as fh:
            fh.write(f"""APP_NAME = nerudo-faults
RUN_MODE = prod
WORK_PATH = {d}
[server]
HTTP_ADDR = 127.0.0.1
HTTP_PORT = {self.port['gitea']}
ROOT_URL = {self.git_url}/
DISABLE_SSH = true
OFFLINE_MODE = true
LFS_START_SERVER = false
[database]
DB_TYPE = sqlite3
PATH = {d}/gitea.db
[repository]
ROOT = {d}/repos
DEFAULT_BRANCH = main
[security]
INSTALL_LOCK = true
SECRET_KEY = {secrets.token_hex(16)}
[service]
DISABLE_REGISTRATION = true
[log]
MODE = file
LEVEL = Warn
ROOT_PATH = {d}/log
[cron]
ENABLED = false
""")
        env = dict(os.environ, HOME=os.path.join(d, "home"), GIT_CONFIG_NOSYSTEM="1")
        base = ["gitea", "--config", ini, "--work-path", d]
        run = lambda *a: subprocess.run([*base, *a], env=env, cwd=d, check=True, capture_output=True, text=True).stdout
        run("migrate")
        run("admin", "user", "create", "--admin", "--username", "bench", "--password", secrets.token_hex(12),
            "--email", "bench@bench.invalid", "--must-change-password=false")
        self.gitea_token = run("admin", "user", "generate-access-token", "--username", "bench", "--scopes", "all",
                               "--raw").strip().splitlines()[-1]
        self.spawn("gitea", [*base, "web"], cwd=d, env=env)
        self.wait_for(lambda: fetch_json("GET", f"{self.git_url}/api/v1/version"), "Gitea")

    # ------------------------------------------------------------------ the Workers

    def write_worker_configs(self):
        """The deployed config without its remote bindings (Artifacts, Workers AI), and with
        CI bound to the stub. `main` is the working tree's build; there is no build command."""
        self.cfg_dir = os.path.join(self.run, "nerudo")
        stub_dir = os.path.join(self.run, "stub")
        os.makedirs(self.cfg_dir)
        os.makedirs(stub_dir)
        with open(os.path.join(self.cfg_dir, "wrangler.toml"), "w") as fh:
            fh.write(f"""name = "nerudo"
main = "{os.path.join(CF, 'nerudo-worker', 'build', 'index.js')}"
compatibility_date = "2026-10-01"

[[durable_objects.bindings]]
name = "TRUNK"
class_name = "Trunk"

[[durable_objects.bindings]]
name = "HUB"
class_name = "Hub"

[[migrations]]
tag = "v1"
new_sqlite_classes = ["Trunk"]

[[migrations]]
tag = "v2"
new_sqlite_classes = ["Hub"]

[[services]]
binding = "CI"
service = "nerudo-faults-ci"

[vars]
NERUDO_WEB = "{self.worker_url}"
""")
        self.vars = os.path.join(self.cfg_dir, ".dev.vars")
        with open(self.vars, "w") as fh:
            fh.write(f"NERUDO_ADMIN_TOKEN={self.admin}\nNERUDO_AGENT_TOKEN={self.agent}\nNERUDO_WORKER={self.worker_url}\n")
        with open(os.path.join(stub_dir, "wrangler.toml"), "w") as fh:
            fh.write(f"""name = "nerudo-faults-ci"
main = "{os.path.join(HERE, 'ci-stub.js')}"
compatibility_date = "2026-10-01"

[[services]]
binding = "REAL"
service = "nerudo-ci"

[vars]
CONTROL = "http://127.0.0.1:{self.port['control']}"
""")
        self.configs = [os.path.join(self.cfg_dir, "wrangler.toml"), os.path.join(stub_dir, "wrangler.toml"),
                        os.path.join(CF, "nerudo-ci", "wrangler.toml")]

    def start_worker(self):
        cmd = ["wrangler", "dev"]
        for c in self.configs:
            cmd += ["-c", c]
        cmd += ["--ip", "127.0.0.1", "--port", str(self.port["worker"]), "--persist-to", os.path.join(self.run, "state"),
                "--show-interactive-dev-session=false"]
        env = dict(os.environ, WRANGLER_SEND_METRICS="false", WRANGLER_LOG_PATH=os.path.join(self.run, "wrangler-logs"))
        self.spawn("worker", cmd, cwd=self.cfg_dir, env=env)
        self.wait_for(lambda: fetch_json("GET", f"{self.worker_url}/v1/trunks/faults-ready/stats",
                                   headers={"Authorization": f"Bearer {self.agent}"}), "wrangler dev", timeout=120)

    def worker_log_lines(self):
        try:
            return open(os.path.join(self.run, "worker.log"), errors="replace").read().splitlines()
        except FileNotFoundError:
            return []

    def wait_for(self, probe, what, timeout=60):
        end = time.time() + timeout
        while time.time() < end:
            try:
                probe()
                return
            except Exception:
                time.sleep(0.3)
        sys.exit(f"faults: {what} did not start in {timeout} s (logs in {self.run})")


class Report:
    def __init__(self):
        self.checks = []
        self.cases = {}

    def check(self, case, what, ok, gap=None, **data):
        status = "pass" if ok else ("gap" if gap else "FAIL")
        self.checks.append({"case": case, "check": what, "status": status, "gap": gap, **data})
        mark = {"pass": "ok  ", "gap": "GAP ", "FAIL": "FAIL"}[status]
        log(f"  {mark} {what}" + (f"  [known gap: {KNOWN_GAPS[gap]}]" if status == "gap" else ""))
        if ok and gap:
            log(f"       known gap {gap} passed: if this holds, remove it from KNOWN_GAPS")
        return ok


# ---------------------------------------------------------------------- helpers on scenarios.py

S = None  # scripts/scenarios.py, imported after the stack has started (it reads the env at import)
SEQ = collections.Counter()


def trunk(tag, **kw):
    """A new trunk. scenarios.new_trunk names it by tag and second, so each tag is unique."""
    SEQ[tag] += 1
    return S.new_trunk(f"{tag}{SEQ[tag]}", **kw)


def add_file(cid):
    """A change that only adds changes/<cid>.txt: changes like it are disjoint."""
    def f(d):
        with open(os.path.join(d, "changes", f"{cid}.txt"), "w") as fh:
            fh.write(cid + "\n")
    return f


def pause(t):
    out = S.api(t, "POST", "pause", {"reason": "faults: queue the changes first"}, admin=True)
    assert "paused" in out, out


def resume(t):
    out = S.api(t, "POST", "resume", {}, admin=True)
    assert "paused" in out, out


def submit(t, agents, ids):
    """Each agent sends its share of `ids`, the agents in parallel."""
    share = {a.name: ids[i::len(agents)] for i, a in enumerate(agents)}

    def send(a):
        for cid in share[a.name]:
            st = a.change(cid, add_file(cid))
            assert st.get("state") == "queued", (cid, st)
    with ThreadPoolExecutor(len(agents)) as pool:
        list(pool.map(send, agents))


def safe_api(t, method, path, **kw):
    try:
        return S.api(t, method, path, **kw)
    except Exception as e:  # the Worker can be down (kill9) or slow (git_flaky)
        return {"error": str(e)}


def settle(t, ids, timeout):
    """Wait until no change in `ids` is queued or parked. Returns {cid: receipt}."""
    end = time.time() + timeout
    while time.time() < end:
        q = safe_api(t, "GET", "queue")
        if isinstance(q.get("changes"), list) and not {c["change_id"] for c in q["changes"]} & set(ids):
            break
        time.sleep(0.5)
    return {cid: safe_api(t, "GET", f"changes/{cid}") for cid in ids}


def full_log(t):
    """The whole landing log. The API gives at most 500 entries for each request."""
    out = []
    while True:
        page = S.api(t, "GET", f"log?since={len(out)}")
        if not isinstance(page, list) or not page:
            return out
        out += page


def audit(rep, case, t, agent, ids, label, timeout=120, gap=None):
    """Wait up to `timeout` s for `ids` to settle, then check the hard invariants: no change
    ejected or still waiting, each landed change on main once and in the log once, no other
    change on main, and verify passes. `gap` names a known gap for ejections of that kind."""
    st = settle(t, ids, timeout)
    blamed = {cid: r["reason"] for cid, r in st.items() if r.get("state") == "ejected" and isinstance(r.get("reason"), dict)}
    kinds = {r.get("kind") for r in blamed.values()}
    known = gap and kinds == {gap.split(".")[-1]}
    rep.check(case, f"{label}: no change ejected for the fault" + (f" ({len(blamed)} ejected: {', '.join(sorted(kinds))})" if blamed else ""),
              not blamed, gap=gap if known else None,
              blamed={cid: {"kind": r.get("kind"), "detail": str(r.get("detail", ""))[:200]} for cid, r in blamed.items()})
    waiting = [cid for cid, r in st.items() if r.get("state") not in ("landed", "ejected")]
    rep.check(case, f"{label}: no change still waiting", not waiting, waiting=waiting[:10])
    d = tempfile.mkdtemp(prefix="audit-", dir=S.SCRATCH)
    S.sh("git", "-c", S.header(agent.trunk_auth), "clone", "-q", agent.fork["trunk"], d)
    on_main = collections.Counter(S.sh("git", "log", "--format=%(trailers:key=Change-Id,valueonly)", cwd=d).split())
    logged = collections.Counter(e["change_id"] for e in full_log(t))
    want = {cid: 1 if st[cid].get("state") == "landed" else 0 for cid in ids}
    wrong = {cid: {"want": n, "on_main": on_main[cid], "in_log": logged[cid]} for cid, n in want.items()
             if on_main[cid] != n or logged[cid] != n}
    rep.check(case, f"{label}: each landed change on main once and in the log once, no other on main", not wrong, wrong=wrong)
    try:
        S.cli("verify", "--trunk", t)
        ok, err = True, ""
    except RuntimeError as e:
        ok, err = False, str(e)[-300:]
    rep.check(case, f"{label}: nerudo verify passes", ok, error=err)
    return st


# ---------------------------------------------------------------------- cases

def case_parity(stack, rep, args):
    for name in args.scenarios.split(","):
        log(f" scenario {name}")
        try:
            getattr(S, name)()
            rep.check("parity", f"scenario {name}", True)
        except Exception as e:
            rep.check("parity", f"scenario {name}", False, error=S.redact(f"{type(e).__name__}: {e}")[-400:])


def backoff_budget(seconds):
    """CI requests one trunk may make in an outage of `seconds`, with retries that double
    from 1 s up to 30 s: about log2 of the outage, plus one each 30 s, plus 2 to spare."""
    return 2 + math.ceil(math.log2(max(seconds, 2))) + seconds / 30


def case_ci_down(stack, rep, args):
    for k in [int(x) for x in args.ci_down_k.split(",")]:
        label = f"K={k}"
        log(f" {label}: {k} trunk{'s' if k != 1 else ''}, each with one change; CI answers 503 for {args.ci_down_seconds} s")
        trunks = []
        for i in range(k):
            t = trunk("cidown", rules=CI_RULES, files=TS_FILES)
            a = S.Agent(t, "a00")
            pause(t)
            submit(t, [a], [f"d{i}"])
            trunks.append((t, a, f"d{i}"))
        lines_before = len(stack.worker_log_lines())
        stack.control.set("status:503")
        t0 = time.time()
        for t, _, _ in trunks:
            resume(t)
        time.sleep(args.ci_down_seconds / 2)
        t, _, cid = trunks[0]
        seen = json.dumps([safe_api(t, "GET", "queue"), safe_api(t, "GET", f"changes/{cid}"),
                           safe_api(t, "GET", "summary")]).lower()
        time.sleep(max(0, args.ci_down_seconds - (time.time() - t0)))
        outage = time.time() - t0
        hits = len(stack.control.since(t0))
        stack.control.set("pass")
        back = time.time()
        loop_errors = sum("landing loop" in l for l in stack.worker_log_lines()[lines_before:])
        per_trunk = hits / k
        rep.cases.setdefault("ci_down", []).append({"k": k, "outage_s": round(outage, 1), "ci_requests": hits,
                                                    "per_trunk": round(per_trunk, 1),
                                                    "per_trunk_per_min": round(per_trunk / outage * 60, 1),
                                                    "landing_loop_errors_logged": loop_errors})
        budget = backoff_budget(outage)
        rep.check("ci_down", f"{label}: {per_trunk:.0f} CI requests per trunk in {outage:.0f} s "
                  f"({hits} in all; budget {budget:.0f} per trunk)", per_trunk <= budget,
                  per_trunk=per_trunk, budget=budget)
        rep.check("ci_down", f"{label}: the queue or the change says that CI is unreachable",
                  any(w in seen for w in CI_DOWN_WORDS))
        landed_ms = []
        for t, a, cid in trunks:
            st = audit(rep, "ci_down", t, a, [cid], f"{label} {t}", timeout=60)
            if st[cid].get("landed_ms"):
                landed_ms.append(st[cid]["landed_ms"] / 1000)
        stalls = {t: safe_api(t, "GET", "queue").get("stall") for t, _, _ in trunks}
        rep.check("ci_down", f"{label}: no queue shows a stall after CI came back",
                  not any(stalls.values()), stalls={t: v for t, v in stalls.items() if v})
        if landed_ms:
            rec = max(landed_ms) - back
            rep.cases["ci_down"][-1]["recovery_s"] = round(rec, 1)
            rep.check("ci_down", f"{label}: all landed {rec:.1f} s after CI came back (budget 30 s)", rec <= 30)


def case_ci_hang(stack, rep, args):
    h = args.ci_hang_seconds
    log(f" one trunk, one change; each CI request hangs for {h} s, for {h + 5} s in all")
    t = trunk("cihang", rules=CI_RULES, files=TS_FILES)
    a = S.Agent(t, "a00")
    pause(t)
    submit(t, [a], ["h0"])
    stack.control.set(f"hang:{h}")
    t0 = time.time()
    resume(t)
    # After the first deadline, the queue says why it waits.
    time.sleep(min(CI_DEADLINE_S + 5, h))
    seen = json.dumps(safe_api(t, "GET", "queue")).lower()
    time.sleep(max(0, h + 5 - (time.time() - t0)))
    hits = stack.control.since(t0)
    stack.control.set("pass")
    back = time.time()
    st = audit(rep, "ci_hang", t, a, ["h0"], t, timeout=h + 60)
    # With a timeout of T s, the Worker gives up on a hung check after T s and asks again;
    # without one, it asks again only when the hang ends.
    gaps = [b - a for a, b in zip(hits, hits[1:])]
    first = gaps[0] if gaps else None
    rep.cases["ci_hang"] = {"hang_s": h, "requests_during_hang": len(hits), "seconds_between_requests": [round(g, 1) for g in gaps]}
    # The deadline, then the first interval (1 s to 1.25 s), and some time for the request.
    rep.check("ci_hang", f"the Worker gave up on a hung CI request after its deadline of {CI_DEADLINE_S} s "
              f"(next request after {first:.0f} s, budget 30 s)" if first else "the Worker gave up on a hung CI "
              f"request within 30 s (no second request in {h + 5} s)", first is not None and first <= 31)
    rep.check("ci_hang", "during the hang, the queue says that CI is unavailable and that it gave no answer",
              "ci_unavailable" in seen and "no answer" in seen)
    if st["h0"].get("landed_ms"):
        rec = st["h0"]["landed_ms"] / 1000 - back
        rep.cases["ci_hang"]["recovery_s"] = round(rec, 1)
        rep.check("ci_hang", f"landed {rec:.1f} s after CI came back (budget {h + 30} s)", rec <= h + 30)


def case_git_flaky(stack, rep, args):
    """One change per batch, so each landing is its own push. The faults are on the replies
    only (downstream): first each connection is reset for a short outage, so each run has an
    effect; then a reply waits 200 ms, and a connection picked at random is reset after
    100 ms. A push then reaches Gitea, but its reply is lost: the Worker cannot know if the
    push happened. Exactly once must still hold."""
    n = args.flaky_changes
    log(f" one trunk, {n} changes, one per batch; the git host resets each connection for {args.flaky_outage:g} s, "
        f"then its replies wait 200 ms and {args.flaky_toxicity:.0%} of connections are reset after 100 ms, "
        f"for up to {args.flaky_seconds} s in all")
    t = trunk("flaky", batch_cap=1)
    agents = [S.Agent(t, "a00"), S.Agent(t, "a01"), S.Agent(t, "a02")]
    ids = [f"f{i}" for i in range(n)]
    pause(t)
    submit(t, agents, ids)
    lines_before = len(stack.worker_log_lines())
    stack.toxic("latency", "latency", {"latency": 200, "jitter": 0})
    stack.toxic("outage", "reset_peer", {"timeout": 0})
    t0 = time.time()
    resume(t)
    time.sleep(args.flaky_outage)
    stack.untoxic("outage")
    stack.toxic("reset", "reset_peer", {"timeout": 100}, toxicity=args.flaky_toxicity)
    under = settle(t, ids, max(1, args.flaky_seconds - args.flaky_outage))
    fault_s = time.time() - t0
    landed_under = sum(r.get("state") == "landed" for r in under.values())
    stack.clear_toxics()
    st = audit(rep, "git_flaky", t, agents[0], ids, t)
    stats = S.api(t, "GET", "stats")
    fetch_failed = sum(isinstance(r.get("reason"), dict) and r["reason"].get("kind") == "fetch_failed" for r in st.values())
    lines = stack.worker_log_lines()[lines_before:]
    errors = [l for l in lines if "landing loop" in l]
    # The gate keeps a change queued when a fetch from its fork fails for a reason that can pass.
    retries = sum("fetch_retry" in l for l in lines)
    rep.cases["git_flaky"] = {"changes": n, "fault_s": round(fault_s, 1), "landed_under_fault": landed_under,
                              "landing_loop_errors_logged": len(errors),
                              "error_kinds": collections.Counter(e.split("landing loop:")[-1].strip()[:80]
                                                                 for e in errors).most_common(5),
                              "fetch_failed_ejections": fetch_failed,
                              "fetch_retries_logged": retries,
                              "fetch_retries": stats.get("fetch_retries"),
                              "external_moves": stats.get("external_moves"),
                              "recovered_attempts": stats.get("recovered_attempts"),
                              "batches": stats.get("batches")}
    effects = len(errors) + fetch_failed + retries + (stats.get("recovered_attempts") or 0)
    rep.check("git_flaky", f"the faults had an effect: {landed_under}/{n} landed in {fault_s:.0f} s; {len(errors)} "
              f"landing-loop errors, {retries} fetches tried again, {fetch_failed} fetch_failed ejections, "
              f"{stats.get('recovered_attempts')} attempts recovered", effects > 0,
              note="with no effect, the case tests nothing: raise --flaky-outage or --flaky-toxicity")
    rep.check("git_flaky", "no lost reply was taken for someone else's move of main (external_moves = 0)",
              stats.get("external_moves", 0) == 0, external_moves=stats.get("external_moves"))


def case_git_hang(stack, rep, args):
    """The git host stops answering for longer than the deadline of the Worker, two times. A
    `timeout` toxic with timeout 0 drops all data on one direction and keeps the connection
    open. With no deadline, a request waits until the connection closes: here when the toxic
    is removed (toxiproxy then closes it), on a real host maybe never. So the checks that
    fail with no deadline are the stall in the queue and the deadline in the log.

      replies   the requests reach Gitea, so a push lands, but its reply never comes
      requests  the requests never reach Gitea, so a push does not land

    One change per batch with a simulated CI of 1.5 s, so a push is in progress when the
    fault starts. Exactly once must hold, and the queue must drain soon after each fault."""
    h = args.hang_seconds
    log(f" one trunk, {args.hang_changes} changes, one per batch; the git host drops its replies for {h} s, "
        f"then its requests for {h} s (deadline {GIT_DEADLINE_S} s)")
    t = trunk("hang", ci_ms=1500, batch_cap=1)
    agents = [S.Agent(t, "a00"), S.Agent(t, "a01")]
    ids = [f"n{i}" for i in range(args.hang_changes)]
    pause(t)
    submit(t, agents, ids)
    lines_before = len(stack.worker_log_lines())
    resume(t)
    rows = []
    for stream in ("downstream", "upstream"):
        name = {"downstream": "replies", "upstream": "requests"}[stream]
        before = safe_api(t, "GET", "stats").get("landed", 0)
        end = time.time() + 30
        while time.time() < end and safe_api(t, "GET", "stats").get("landed", 0) == before:
            time.sleep(0.1)
        stack.toxic("hang", "timeout", {"timeout": 0}, stream=stream)
        t0 = time.time()
        kinds = set()
        while time.time() - t0 < h:
            st = (safe_api(t, "GET", "queue").get("stall") or {}).get("kind")
            if st:
                kinds.add(st)
            time.sleep(1)
        stack.untoxic("hang")
        back = time.time()
        landed_at_back = safe_api(t, "GET", "stats").get("landed", 0)
        # The next change after the fault lands, or all have landed.
        end = time.time() + GIT_DEADLINE_S + 60
        while time.time() < end:
            n = safe_api(t, "GET", "stats").get("landed", 0)
            if n > landed_at_back or n >= len(ids):
                break
            time.sleep(0.2)
        rec = time.time() - back
        rows.append({"fault": name, "hang_s": round(back - t0, 1), "stall_kinds": sorted(kinds),
                     "landed_before": before, "landed_at_back": landed_at_back, "next_landing_after_s": round(rec, 1)})
        rep.check("git_hang", f"{name}: during the fault the queue showed a stall of the git host ({', '.join(sorted(kinds)) or 'none'})",
                  "git_unavailable" in kinds)
        rep.check("git_hang", f"{name}: the next landing came {rec:.0f} s after the git host answered again "
                  f"(budget {GIT_DEADLINE_S + 60} s)", rec <= GIT_DEADLINE_S + 60)
    st = audit(rep, "git_hang", t, agents[0], ids, t)
    stats = S.api(t, "GET", "stats")
    lines = stack.worker_log_lines()[lines_before:]
    deadlines = sum(("no answer in" in l or "the answer stopped" in l) for l in lines)
    rep.cases["git_hang"] = {"changes": len(ids), "faults": rows, "deadline_lines_logged": deadlines,
                             "landing_loop_errors_logged": sum("landing loop" in l for l in lines),
                             "fetch_retries": stats.get("fetch_retries"), "recovered_attempts": stats.get("recovered_attempts"),
                             "pushes_again": stats.get("pushes_again"), "external_moves": stats.get("external_moves"),
                             "batches": stats.get("batches"), "landed": sum(r.get("state") == "landed" for r in st.values())}
    rep.check("git_hang", f"the Worker stopped hung git requests at its deadline ({deadlines} log lines)", deadlines > 0)
    rep.check("git_hang", "no lost reply was taken for someone else's move of main (external_moves = 0)",
              stats.get("external_moves", 0) == 0, external_moves=stats.get("external_moves"))


def case_kill9(stack, rep, args):
    """One change per batch with a short simulated CI, so the processes are often in a push
    or a landing record when they die. A seeded random delay moves the kill each rep."""
    n = 10
    rng = random.Random(args.seed)
    for r in range(args.kill_reps):
        target = 1 + 2 * r
        t = trunk("kill", ci_ms=150, batch_cap=1)
        a = S.Agent(t, "a00")
        ids = [f"k{i}" for i in range(n)]
        pause(t)
        submit(t, [a], ids)
        resume(t)
        end = time.time() + 60
        while time.time() < end and safe_api(t, "GET", "stats").get("landed", 0) < target:
            time.sleep(0.05)
        time.sleep(rng.uniform(0, 0.3))
        before = safe_api(t, "GET", "stats")
        log(f" rep {r + 1}: SIGKILL the Worker processes after {before.get('landed')} of {n} landed")
        stack.kill("worker")
        stack.start_worker()
        out = settle(t, ids, 60)
        stalled = [cid for cid, rr in out.items() if rr.get("state") not in ("landed", "ejected")]
        nudged = False
        if stalled:
            # Record the stall, then wake the object, so the audit still checks exactly-once.
            nudged = True
            resume(t)
            settle(t, ids, 60)
        stats = S.api(t, "GET", "stats")
        rep.cases.setdefault("kill9", []).append({"rep": r + 1, "landed_before_kill": before.get("landed"),
                                                  "stalled_after_restart": len(stalled), "nudged": nudged,
                                                  "recovered_attempts": stats.get("recovered_attempts")})
        rep.check("kill9", f"rep {r + 1}: the queue drained after the restart with no help", not stalled,
                  stalled=stalled)
        audit(rep, "kill9", t, a, ids, f"rep {r + 1} {t}")


def case_growth(stack, rep, args):
    sizes = [int(x) for x in args.growth.split(",")]
    rows = []
    for n in sizes:
        t = trunk("growth")
        agents = [S.Agent(t, f"a{i:02d}") for i in range(4)]
        ids = [f"g{i}" for i in range(n)]
        pause(t)
        submit(t, agents, ids)
        t0 = time.time() * 1000
        resume(t)
        st = audit(rep, "growth", t, agents[0], ids, f"N={n} {t}", timeout=300)
        done = [r["landed_ms"] for r in st.values() if r.get("landed_ms")]
        stats = S.api(t, "GET", "stats")
        drain = (max(done) - t0) / 1000 if len(done) == n else None
        # Stage timings of each round of the landing loop (cf/scale/stages.md).
        rounds = S.api(t, "GET", "timings").get("rounds", [])
        rows.append({"n": n, "drain_s": round(drain, 2) if drain else None, "batches": stats.get("batches"),
                     "git_requests": stats.get("git_requests"), "rounds": rounds})
        log(f"   N={n}: drained in {drain:.2f} s, {stats.get('batches')} batches" if drain else f"   N={n}: did not drain")
    rep.cases["growth"] = rows
    for a, b in zip(rows, rows[1:]):
        if a["drain_s"] and b["drain_s"] and b["n"] == 2 * a["n"]:
            ratio = b["drain_s"] / a["drain_s"]
            # Linear growth gives ×2. The live stress run on Artifacts of 2026-10-12 gave ×3.4
            # for 64→128; this budget catches that pattern here.
            rep.check("growth", f"N {a['n']}→{b['n']}: drain time ×{ratio:.1f} (budget ×3.0)", ratio <= 3.0,
                      ratio=round(ratio, 2))


CASES = {"parity": case_parity, "ci_down": case_ci_down, "ci_hang": case_ci_hang, "git_flaky": case_git_flaky,
         "git_hang": case_git_hang, "kill9": case_kill9, "growth": case_growth}


def versions():
    def first_line(*cmd):
        try:
            out = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
            return (out.stdout or out.stderr).strip().splitlines()[0]
        except Exception as e:
            return f"unknown ({e})"
    rev = first_line("git", "-C", ROOT, "rev-parse", "--short", "HEAD")
    dirty = bool(subprocess.run(["git", "-C", ROOT, "status", "--porcelain"], capture_output=True, text=True).stdout.strip())
    return {"commit": rev + ("+dirty" if dirty else ""), "wrangler": first_line("wrangler", "--version"),
            "gitea": first_line("gitea", "--version"), "toxiproxy": first_line("toxiproxy-server", "-version"),
            "git": first_line("git", "--version"), "python": sys.version.split()[0]}


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--cases", default=",".join(CASES), help="comma-separated, from: " + ", ".join(CASES))
    p.add_argument("--scenarios", default="approvals,ci,recovery,authority_recovery,allowance_recovery,ci_red", help="for parity (Artifacts-only ones fail)")
    p.add_argument("--ci-down-k", default="1,8", help="trunk counts for ci_down")
    p.add_argument("--ci-down-seconds", type=int, default=20)
    p.add_argument("--ci-hang-seconds", type=int, default=45)
    p.add_argument("--flaky-changes", type=int, default=12)
    p.add_argument("--flaky-outage", type=float, default=3, help="seconds that the git host resets each connection, "
                   "before the random resets")
    p.add_argument("--flaky-toxicity", type=float, default=0.4)
    p.add_argument("--flaky-seconds", type=int, default=45, help="how long the git faults stay on")
    p.add_argument("--hang-changes", type=int, default=6)
    p.add_argument("--hang-seconds", type=int, default=GIT_DEADLINE_S + 10,
                   help="how long each git fault of git_hang stays on; more than the deadline of the Worker")
    p.add_argument("--kill-reps", type=int, default=3)
    p.add_argument("--growth", default="32,64,128", help="queue sizes for growth")
    p.add_argument("--seed", type=int, default=1, help="the random seed for toxiproxy and kill9")
    p.add_argument("--strict", action="store_true", help="known gaps fail the run too")
    p.add_argument("--no-build", action="store_true", help="never build; stop if the build is missing or old")
    p.add_argument("--evidence", help="where to write the JSON report (default: the temp directory)")
    p.add_argument("--keep", action="store_true", help="keep the run directory also when no check failed")
    args = p.parse_args()
    unknown = set(args.cases.split(",")) - set(CASES)
    if unknown:
        p.error(f"unknown cases: {', '.join(sorted(unknown))}")

    ensure_built(args.no_build)
    run = tempfile.mkdtemp(prefix="nerudo-faults-")
    log(f"run directory (logs, state): {run}")
    stack = Stack(run, args.seed)
    rep = Report()
    started = time.strftime("%Y-%m-%dT%H:%M:%S%z")
    try:
        stack.start()
        os.environ.update(NERUDO_HOST="forgejo", NERUDO_FORGEJO=stack.git_url, NERUDO_FORGEJO_TOKEN=stack.gitea_token,
                          NERUDO_VARS=stack.vars, NERUDO_CF_WORKER=stack.worker_url)
        global S
        sys.path.insert(0, os.path.join(CF, "scripts"))
        # scenarios.py makes its scratch directory (clones, homes) at import: put it in the run.
        tempfile.tempdir = run
        S = importlib.import_module("scenarios")
        tempfile.tempdir = None
        log(f"stack: worker {stack.worker_url}, git host {stack.git_url} (toxiproxy → Gitea :{stack.port['gitea']})")
        for name in args.cases.split(","):
            log(f"== {name}")
            t = time.time()
            try:
                CASES[name](stack, rep, args)
            except Exception as e:
                rep.check(name, "the case ran to its end", False, error=S.redact(f"{type(e).__name__}: {e}")[-500:])
            finally:
                stack.control.set("pass")
                try:
                    stack.clear_toxics()
                except Exception:
                    pass
            log(f"   ({time.time() - t:.0f} s)")
    finally:
        stack.stop()

    count = collections.Counter(c["status"] for c in rep.checks)
    log(f"\n{count['pass']} passed, {count['gap']} known gaps, {count['FAIL']} failed")
    for c in rep.checks:
        if c["status"] != "pass":
            log(f"  {c['status']:4} {c['case']}: {c['check']}")
    evidence = args.evidence or os.path.join(tempfile.gettempdir(), f"nerudo-faults-{time.strftime('%Y%m%d-%H%M%S')}.json")
    os.makedirs(os.path.dirname(os.path.abspath(evidence)), exist_ok=True)
    # Before the file exists: an evidence file in the repo would make the tree look changed.
    vers = versions()
    with open(evidence, "w") as fh:
        json.dump({"started": started, "versions": vers, "args": vars(args), "known_gaps": KNOWN_GAPS,
                   "cases": rep.cases, "checks": rep.checks}, fh, indent=2, default=str)
        fh.write("\n")
    log(f"evidence: {os.path.abspath(evidence)}")
    if count["FAIL"] or args.keep:
        log(f"logs and state: {run}")
    else:
        shutil.rmtree(run, ignore_errors=True)
    sys.exit(1 if count["FAIL"] or (args.strict and count["gap"]) else 0)


if __name__ == "__main__":
    main()
