#!/usr/bin/env python3
"""Run one benchmark configuration (or a sweep) and append one JSON line per run.

  python3 bench/run.py --backend git --mode strict --n 1,2,4 --k 3 --reps 1 --ci 2

backend  where trunk lives: git (local bare repo), jj (local bare repo, jj client),
         forgejo (cluster), github, seq (local sequencer)
mode     strict | lax | pr | jj | seq   (see agents.py)
"""
import argparse
import json
import os
import re
import shutil
import socket
import statistics
import subprocess
import sys
import threading
import time
import traceback
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import agents  # noqa: E402
from workload import analyze_trunk, init_tree, make_spec  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RUNS = os.path.join(ROOT, "runs")
SEED = 20261007
FORGEJO = os.environ.get("NERUDO_FORGEJO", "http://192.168.150.121:30300")
GH_OWNER, GH_REPO = "kcirtapfromspace", "nerudo-bench"
RUN_DEADLINE_S = 900
CF_WORKER = os.environ.get("NERUDO_CF_WORKER", "http://127.0.0.1:8787")
ART_ACCOUNT, ART_NAMESPACE = os.environ.get("CLOUDFLARE_ACCOUNT_ID", ""), "nerudo"
SEQD = os.path.join(ROOT, "target", "release", "seqd")
SEED_ENV = dict(os.environ, GIT_AUTHOR_NAME="seed", GIT_AUTHOR_EMAIL="seed@bench.invalid",
                GIT_COMMITTER_NAME="seed", GIT_COMMITTER_EMAIL="seed@bench.invalid",
                GIT_AUTHOR_DATE="2026-10-07T00:00:00Z", GIT_COMMITTER_DATE="2026-10-07T00:00:00Z",
                PYTHONDONTWRITEBYTECODE="1", GIT_TERMINAL_PROMPT="0", **agents.NO_SIGN)


def sh(*cmd, cwd=None, env=None):
    p = subprocess.run(cmd, cwd=cwd, env=env or SEED_ENV, capture_output=True, text=True)
    if p.returncode:
        # A clone URL on the command line can carry a token: never let it into a log.
        raise RuntimeError(agents.redact(f"{' '.join(cmd)}: {p.stderr.strip()[:400]}"))
    return p.stdout


def read(path):
    with open(os.path.join(ROOT, path)) as fh:
        return fh.read().strip()


CF_RULES = os.path.join(ROOT, "cf", "fixtures", "bench-nerudo.toml")


def read_vars(name):
    path = os.path.join(ROOT, "cf", "nerudo-worker", name)
    return dict(l.strip().split("=", 1) for l in open(path) if "=" in l)


def artifacts_api(method, path, body=None):
    """Artifacts control plane, with the wrangler OAuth token (read fresh: wrangler refreshes it)."""
    if not ART_ACCOUNT:
        raise RuntimeError("set CLOUDFLARE_ACCOUNT_ID to the account that holds the Artifacts namespace")
    cfg = open(os.path.expanduser("~/.wrangler/config/default.toml")).read()
    oauth = re.search(r'^oauth_token = "(.*)"', cfg, re.M).group(1)
    req = urllib.request.Request(
        f"https://api.cloudflare.com/client/v4/accounts/{ART_ACCOUNT}/artifacts/namespaces/{ART_NAMESPACE}{path}",
        method=method, data=json.dumps(body).encode() if body is not None else None,
        headers={"Authorization": f"Bearer {oauth}", "Content-Type": "application/json"})
    out = json.loads(urllib.request.urlopen(req, timeout=60).read())
    if not out.get("success"):
        raise RuntimeError(f"artifacts {method} {path}: {out.get('errors')}")
    return out["result"]


def artifacts_token(repo, scope, ttl=7200):
    return "Bearer " + artifacts_api("POST", "/tokens", {"repo": repo, "scope": scope, "ttl": ttl})["plaintext"]


def seed_push(url, trunk, rdir, rules=None, env=None):
    seed = os.path.join(rdir, "seed")
    os.makedirs(seed)
    init_tree(seed)
    if rules:
        shutil.copy(rules, os.path.join(seed, "nerudo.toml"))
    sh("git", "init", "-q", "-b", "main", cwd=seed)
    sh("git", "add", "-A", cwd=seed)
    sh("git", "commit", "-q", "-m", "seed", cwd=seed)
    sh("git", "push", "-q", url, f"HEAD:refs/heads/{trunk}", cwd=seed, env=env)


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Run:
    def __init__(self, backend, mode, n, k, rep, ci, label, allow_backwards=False,
                 jj_layout="workspaces", mirror=None):
        self.backend, self.mode, self.n, self.k, self.rep, self.ci = backend, mode, n, k, rep, ci
        self.allow_backwards = allow_backwards
        self.jj_layout = jj_layout
        self.mirror = mirror
        self.mirror_url = None
        self.mirror_trunk = None
        self.mirror_seed_via_api = False
        self.agent_pretest = False
        self.cf_host = "forgejo"
        self.git_env = SEED_ENV  # env for the harness's own git calls on trunk (clone, analysis)
        self.label = label
        stamp = time.strftime("%H%M%S")
        self.run_id = f"{label}-n{n}-r{rep}-{stamp}"
        self.rdir = os.path.join(RUNS, self.run_id)
        os.makedirs(self.rdir)
        self.trunk = "main"
        self.forge = None
        self.seq_proc = None
        self.seq_url = None

    # ---- remote
    def setup_remote(self):
        if self.backend in ("git", "jj", "seq"):
            self.url = os.path.join(self.rdir, "remote.git")
            sh("git", "init", "-q", "--bare", "-b", "main", self.url)
        elif self.backend == "cf" and self.cf_host == "artifacts":
            name = self.run_id.replace(".", "-")
            self.url = artifacts_api("POST", "/repos", {"name": name, "default_branch": "main"})["remote"]
            seed_push(self.url, self.trunk, self.rdir, rules=CF_RULES,
                      env=agents.with_header(SEED_ENV, self.url, artifacts_token(name, "write", ttl=600)))
            self.git_env = agents.with_header(SEED_ENV, self.url, artifacts_token(name, "read"))
            self.start_cf_artifacts(name)
            return
        elif self.backend == "cf":
            token = read("deploy/.forgejo-token")
            name = self.run_id.replace(".", "-")
            req = urllib.request.Request(f"{FORGEJO}/api/v1/user/repos", method="POST",
                                         data=json.dumps({"name": name, "default_branch": "main"}).encode(),
                                         headers={"Authorization": f"token {token}",
                                                  "Content-Type": "application/json"})
            urllib.request.urlopen(req, timeout=30).read()
            self.url = f"http://bench:{token}@{FORGEJO.split('//')[1]}/bench/{name}.git"
            seed_push(self.url, self.trunk, self.rdir, rules=CF_RULES)
            self.start_cf(name, token)
            return
        elif self.backend == "forgejo":
            token = read("deploy/.forgejo-token")
            name = self.run_id.replace(".", "-")
            req = urllib.request.Request(f"{FORGEJO}/api/v1/user/repos", method="POST",
                                         data=json.dumps({"name": name, "default_branch": "main"}).encode(),
                                         headers={"Authorization": f"token {token}",
                                                  "Content-Type": "application/json"})
            urllib.request.urlopen(req, timeout=30).read()
            self.url = f"http://bench:{token}@{FORGEJO.split('//')[1]}/bench/{name}.git"
            self.forge = agents.Forgejo(FORGEJO, token, "bench", name)
        elif self.backend == "github":
            self.trunk = f"run/{self.run_id}"
            self.url = f"git@github.com:{GH_OWNER}/{GH_REPO}.git"
            token = subprocess.run(["gh", "auth", "token"], capture_output=True, text=True).stdout.strip()
            self.forge = agents.GitHub(token, GH_OWNER, GH_REPO)
        seed_push(self.url, self.trunk, self.rdir)
        if self.backend == "seq" and self.mirror:
            self.setup_mirror()
        if self.backend == "seq":
            self.start_seqd()

    def setup_mirror(self):
        """The forge as a downstream mirror: only the sequencer writes its trunk."""
        if self.mirror == "forgejo":
            token = read("deploy/.forgejo-token")
            name = self.run_id.replace(".", "-")
            req = urllib.request.Request(f"{FORGEJO}/api/v1/user/repos", method="POST",
                                         data=json.dumps({"name": name, "default_branch": "main"}).encode(),
                                         headers={"Authorization": f"token {token}",
                                                  "Content-Type": "application/json"})
            urllib.request.urlopen(req, timeout=30).read()
            self.mirror_url = f"http://bench:{token}@{FORGEJO.split('//')[1]}/bench/{name}.git"
            self.mirror_trunk = "main"
        else:
            self.mirror_url = f"git@github.com:{GH_OWNER}/{GH_REPO}.git"
            self.mirror_trunk = f"run/{self.run_id}"
        seed = os.path.join(self.rdir, "seed")
        try:
            sh("git", "push", "-q", self.mirror_url, f"HEAD:refs/heads/{self.mirror_trunk}", cwd=seed)
        except RuntimeError:
            if self.mirror != "github":
                raise
            # GitHub's push path can fail while its REST API works. The seed commit is
            # deterministic and already on GitHub, so create the mirror branch via the API.
            sha = sh("git", "rev-parse", "HEAD", cwd=seed).strip()
            sh("gh", "api", "-X", "POST", f"repos/{GH_OWNER}/{GH_REPO}/git/refs",
               "-f", f"ref=refs/heads/{self.mirror_trunk}", "-f", f"sha={sha}")
            self.mirror_seed_via_api = True

    def start_cf(self, name, token):
        """Initialize a Trunk Durable Object (local workerd) over the Forgejo trunk repo."""
        env = dict(l.strip().split("=", 1) for l in open(os.path.join(ROOT, "cf", "nerudo-worker", ".dev.vars")) if "=" in l)
        self.cf_url = f"{CF_WORKER}/v1/trunks/{name}"
        self.cf_token = env["NERUDO_AGENT_TOKEN"]
        basic = "Basic " + __import__("base64").b64encode(f"bench:{token}".encode()).decode()
        body = {"trunk": name, "remote": f"{FORGEJO}/bench/{name}.git", "authorization": basic,
                "host": {"kind": "forgejo", "api": f"{FORGEJO}/api/v1", "owner": "bench", "repo": name, "token": token},
                "ci_ms": int(self.ci * 1000), "batch_cap": 16}
        req = urllib.request.Request(f"{self.cf_url}/init", data=json.dumps(body).encode(), method="POST",
                                     headers={"Content-Type": "application/json",
                                              "Authorization": f"Bearer {env['NERUDO_ADMIN_TOKEN']}"})
        urllib.request.urlopen(req, timeout=60).read()

    def start_cf_artifacts(self, name):
        """Initialize a Trunk Durable Object on the deployed Worker over an Artifacts trunk.
        No trunk credential is sent: the Durable Object mints its own write token."""
        env = read_vars(".prod.vars")
        self.cf_url = f"{env['NERUDO_WORKER']}/v1/trunks/{name}"
        self.cf_token = env["NERUDO_AGENT_TOKEN"]
        body = {"trunk": name, "remote": self.url,
                "host": {"kind": "artifacts", "binding": "ARTIFACTS", "repo": name},
                "ci_ms": int(self.ci * 1000), "batch_cap": 16}
        req = urllib.request.Request(f"{self.cf_url}/init", data=json.dumps(body).encode(), method="POST",
                                     headers={"Content-Type": "application/json", "User-Agent": agents.CF_UA,
                                              "Authorization": f"Bearer {env['NERUDO_ADMIN_TOKEN']}"})
        urllib.request.urlopen(req, timeout=60).read()

    def cf_call(self, method, path, body=None):
        req = urllib.request.Request(f"{self.cf_url}/{path}", method=method,
                                     data=json.dumps(body).encode() if body is not None else None,
                                     headers={"Content-Type": "application/json", "Authorization": f"Bearer {self.cf_token}",
                                              "User-Agent": agents.CF_UA})
        return json.loads(urllib.request.urlopen(req, timeout=60).read())

    def start_seqd(self):
        port = free_port()
        self.seq_url = f"http://127.0.0.1:{port}"
        log = open(os.path.join(self.rdir, "seqd.log"), "w")
        self.seq_proc = subprocess.Popen(
            [SEQD, "--repo", self.url, "--work", os.path.join(self.rdir, "seq-work"),
             "--listen", f"127.0.0.1:{port}", "--trunk", self.trunk,
             "--ci-seconds", str(self.ci), "--batch-cap", "16",
             *(["--mirror-url", self.mirror_url, "--mirror-ref", f"refs/heads/{self.mirror_trunk}"]
               if self.mirror_url else [])],
            stdout=log, stderr=subprocess.STDOUT, env=SEED_ENV)
        for _ in range(100):
            try:
                urllib.request.urlopen(f"{self.seq_url}/stats", timeout=1).read()
                return
            except OSError:
                time.sleep(0.1)
        raise RuntimeError("seqd did not start")

    # ---- workspaces
    def setup_workspaces(self):
        ctx = agents.Ctx(self.trunk, self.ci, forge=self.forge, seq_url=self.seq_url,
                         run_id=self.run_id, allow_backwards=self.allow_backwards,
                         cf_url=getattr(self, "cf_url", None), cf_token=getattr(self, "cf_token", None),
                         agent_pretest=self.agent_pretest)
        self.ctx = ctx
        if self.mode == "jj":
            ws = []
            if self.jj_layout == "clones":
                for i in range(self.n):
                    p = os.path.join(self.rdir, f"w{i:02d}")
                    sh("jj", "git", "clone", "--quiet", self.url, p)
                    ws.append(agents.JjAgent(i, p, ctx))
            else:
                repo = os.path.join(self.rdir, "jjrepo")
                sh("jj", "git", "clone", "--quiet", self.url, repo)
                for i in range(self.n):
                    p = os.path.join(self.rdir, f"w{i:02d}")
                    sh("jj", "-R", repo, "workspace", "add", "--quiet", "--name", f"w{i:02d}", p)
                    ws.append(agents.JjAgent(i, p, ctx))
            self.analysis_repo = os.path.join(self.rdir, "analysis.git")
            return ws
        clone = os.path.join(self.rdir, "clone")
        sh("git", "clone", "-q", "--single-branch", "-b", self.trunk, self.url, clone, env=self.git_env)
        sh("git", "-C", clone, "config", "gc.auto", "0")
        ws = []
        for i in range(self.n):
            p = os.path.join(self.rdir, f"w{i:02d}")
            sh("git", "-C", clone, "worktree", "add", "-q", "--detach", p, f"origin/{self.trunk}")
            a = agents.GitAgent(i, p, ctx)
            if self.backend == "cf":
                a.fork = self.cf_call("POST", "forks", {"agent": f"a{i:02d}"})
                if a.fork.get("trunk_authorization"):
                    # Each agent reads trunk with its own read-only token; it can't write trunk.
                    a.env = agents.with_header(a.env, self.url, a.fork["trunk_authorization"])
            ws.append(a)
        self.analysis_repo = os.path.join(self.rdir, "analysis.git")
        return ws

    # ---- execute
    def execute(self):
        self.setup_remote()
        ws = self.setup_workspaces()
        specs = {i: [make_spec(SEED, i, j) for j in range(self.k)] for i in range(self.n)}
        land = agents.STRATEGIES[self.mode]
        results, lock = [], threading.Lock()
        barrier = threading.Barrier(self.n + 1)

        def worker(a):
            barrier.wait()
            for spec in specs[a.idx]:
                start = time.time()
                try:
                    ready, landed = land(a, spec)
                    rec = {"cid": spec.cid, "ok": True, "start": start, "ready": ready, "landed": landed}
                except Exception as e:  # noqa: BLE001 - record and keep the run going
                    a.c.errors.append(f"{type(e).__name__}: {e}"[:300])
                    rec = {"cid": spec.cid, "ok": False, "start": start}
                with lock:
                    results.append(rec)

        threads = [threading.Thread(target=worker, args=(a,), daemon=True) for a in ws]
        for t in threads:
            t.start()
        barrier.wait()
        t0 = time.time()
        self.ctx.deadline = t0 + RUN_DEADLINE_S
        for t in threads:
            t.join(timeout=3600)
        wall = time.time() - t0
        return self.summarize(ws, specs, results, wall)

    def summarize(self, ws, specs, results, wall):
        sh("git", "init", "-q", "--bare", self.analysis_repo)
        src, ref = self.url, self.trunk
        self.mirror_behind = None
        if self.mirror_url:
            for _ in range(240):
                st = json.loads(urllib.request.urlopen(f"{self.seq_url}/stats", timeout=5).read())
                self.mirror_behind = st["landed"] - st["mirror"]["mirrored_seq"]
                if self.mirror_behind <= 0:
                    break
                time.sleep(0.25)
            # Analyze what consumers of the mirror see -- but only if it caught up. A stalled
            # mirror push is lag, not loss, so fall back to the sequencer's own trunk.
            if self.mirror_behind <= 0:
                src, ref = self.mirror_url, self.mirror_trunk
        sh("git", "-C", self.analysis_repo, "fetch", "-q", src,
           f"+refs/heads/{ref}:refs/heads/trunk", env=self.git_env)
        all_specs = [s for v in specs.values() for s in v]
        landed = {r["cid"] for r in results if r["ok"]}
        trunk = analyze_trunk(self.analysis_repo, "trunk", all_specs, landed)
        counters = {f: sum(getattr(a.c, f) for a in ws) for f in agents.Counters.FIELDS}
        lat = sorted(r["landed"] - r["ready"] for r in results if r["ok"])
        q = (lambda p: lat[min(len(lat) - 1, int(p * len(lat)))]) if lat else (lambda p: None)
        n_landed = len(landed)
        rec = {
            "run_id": self.run_id, "label": self.label, "backend": self.backend, "mode": self.mode,
            "n": self.n, "jj_layout": self.jj_layout if self.mode == "jj" else None,
            "k": self.k, "rep": self.rep, "ci_seconds": self.ci, "wall_s": round(wall, 3),
            "expected": len(all_specs), "landed": n_landed,
            "throughput_per_min": round(60 * n_landed / wall, 2) if wall else 0,
            "lat_p50": q(0.5), "lat_p95": q(0.95), "lat_mean": statistics.mean(lat) if lat else None,
            "rejects_per_landing": round(counters["push_rejects"] / n_landed, 3) if n_landed else None,
            "ci_runs_per_landing": round(counters["ci_runs"] / n_landed, 3) if n_landed else None,
            **counters, **trunk,
            "timed_out": any("deadline" in e for a in ws for e in a.c.errors),
            "errors_sample": [agents.redact(e) for a in ws for e in a.c.errors][:5],
            "fetch_errors_sample": [e for a in ws for e in a.c.fetch_stderr][:5],
            "error_count": sum(len(a.c.errors) for a in ws),
        }
        if self.backend == "cf":
            st = self.cf_call("GET", "stats")
            rec["seq"] = {k: st.get(k) for k in ("batches", "batch_sizes", "ci_runs", "git_requests", "pushes",
                                                 "ejected_conflict", "ejected_check", "ejected_other", "external_moves",
                                                 "recovered_attempts", "seq")}
            agent_ci = counters["ci_runs"]
            rec["ci_runs_per_landing"] = round((agent_ci + st.get("ci_runs", 0)) / n_landed, 3) if n_landed else None
            rec["agent_pretest"] = self.agent_pretest
            rec["cf_host"] = self.cf_host
            # Git HTTP requests per landing: smart HTTP is two requests per fetch or push.
            agent_git = 2 * (counters["fetches"] + counters["push_attempts"])
            rec["git_http_per_landing"] = round((agent_git + st.get("git_requests", 0)) / n_landed, 2) if n_landed else None
        if self.seq_proc:
            try:
                rec["seq"] = json.loads(urllib.request.urlopen(f"{self.seq_url}/stats", timeout=5).read())
                rec["mirror"] = self.mirror
                rec["mirror_caught_up"] = (self.mirror_behind or 0) <= 0
                rec["mirror_seed_via_api"] = self.mirror_seed_via_api
                rec["mirror_failures"] = rec["seq"].get("mirror", {}).get("failures")
                rec["mirror_behind"] = max(0, self.mirror_behind or 0)
                lags = sorted(rec["seq"].get("mirror", {}).pop("lags", []) or [])
                if lags:
                    rec["mirror_lag_p50"] = lags[len(lags) // 2]
                    rec["mirror_lag_p95"] = lags[min(len(lags) - 1, int(0.95 * len(lags)))]
                rec["ci_runs_per_landing"] = round(
                    (counters["ci_runs"] + rec["seq"]["ci_runs"]) / n_landed, 3) if n_landed else None
            finally:
                self.seq_proc.terminate()
        return rec


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--backend", required=True, choices=["git", "jj", "forgejo", "github", "seq", "cf"])
    ap.add_argument("--mode", required=True, choices=list(agents.STRATEGIES))
    ap.add_argument("--n", default="1,2,4,8")
    ap.add_argument("--k", type=int, default=3)
    ap.add_argument("--reps", type=int, default=1)
    ap.add_argument("--ci", type=float, default=2.0)
    ap.add_argument("--out", default=os.path.join(ROOT, "results", "runs.jsonl"))
    ap.add_argument("--label")
    ap.add_argument("--jj-layout", default="workspaces", choices=["workspaces", "clones"])
    ap.add_argument("--cf-host", default="forgejo", choices=["forgejo", "artifacts"],
                    help="cf only: forgejo = local workerd over Forgejo; artifacts = the deployed Worker over Cloudflare Artifacts")
    ap.add_argument("--agent-pretest", action="store_true",
                    help="cf only: agents also run CI before submitting (advisory)")
    ap.add_argument("--mirror", choices=["forgejo", "github"],
                    help="seq only: push trunk to this forge as a downstream mirror")
    ap.add_argument("--allow-backwards", action="store_true",
                    help="jj only: agent follows jj's --allow-backwards hint")
    args = ap.parse_args()
    label = args.label or f"{args.backend}-{args.mode}"
    for rep in range(args.reps):
        for n in [int(x) for x in args.n.split(",")]:
            run = Run(args.backend, args.mode, n, args.k, rep, args.ci, label, args.allow_backwards,
                      args.jj_layout, args.mirror)
            run.agent_pretest = args.agent_pretest
            run.cf_host = args.cf_host
            try:
                rec = run.execute()
            except Exception:  # noqa: BLE001
                traceback.print_exc()
                rec = {"run_id": run.run_id, "label": label, "backend": args.backend, "mode": args.mode, "n": n,
                       "k": args.k, "rep": rep, "ci_seconds": args.ci, "failed": True}
            with open(args.out, "a") as fh:
                fh.write(json.dumps(rec) + "\n")
            keys = ("n", "landed", "expected", "wall_s", "throughput_per_min", "rejects_per_landing",
                    "ci_runs_per_landing", "lat_p50", "lost_changes", "dup_migrations",
                    "broken_commits", "fetch_errors", "error_count")
            print(label, {k: rec.get(k) for k in keys}, flush=True)


if __name__ == "__main__":
    main()
