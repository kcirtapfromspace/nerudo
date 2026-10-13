#!/usr/bin/env python3
"""Land changes on a big trunk on a deployed throwaway Worker, and measure.

    python3 -I cf/scale/mono_bench.py --vars SCALE_VARS --repo DIR --name mono-XXX --out RUN.json

DIR is a repo from gen_monorepo.py. The script:
 1. makes the Artifacts repo NAME in the namespace (--namespace, default nerudo-scale) and
    pushes DIR's main to it, if the repo does not exist yet;
 2. redeploys the Worker (--config), so that the trunk starts in a new isolate and the
    WebAssembly memory it reports is its own;
 3. inits trunk NAME on that repo, gets a fork for one agent, and lands one change (base =
    head), then a batch of --batch changes in different packages (base = the old head);
 4. records, for each step: the time, the HTTP status or the change's state and reason,
    and the trunk's stats (wasm_memory_bytes is the peak of the isolate's linear memory).
    `wrangler tail` runs during the test; its events with an outcome other than "ok"
    (exceededMemory, exceededCpu, exception) go into the report.

SCALE_VARS holds NERUDO_ADMIN_TOKEN, NERUDO_AGENT_TOKEN and NERUDO_WORKER. Tokens never
go to stdout or to the report. Run with python3 -I.
"""
import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

WRANGLER = os.environ.get("WRANGLER", "wrangler")
UA = "nerudo-scale/0.1"
SECRET = re.compile(r"art_v\d+_[A-Za-z0-9_]+(\?expires=\d+)?|(Bearer|Basic|token) [A-Za-z0-9+/=_.?-]{16,}|lsa1\.[A-Za-z0-9_]+\.\d+\.[0-9a-f]+"
                    r"|[0-9a-f]{32}(?=\.artifacts)|(?<=/accounts/)[0-9a-f]{32}")


def redact(t):
    return SECRET.sub("***", t)


class Bench:
    def __init__(self, a):
        self.a = a
        self.vars = dict(l.strip().split("=", 1) for l in open(a.vars) if "=" in l)
        self.worker = self.vars["NERUDO_WORKER"].rstrip("/")
        self.tmp = tempfile.mkdtemp(prefix="mono-bench-")
        self.env = dict(os.environ, GIT_TERMINAL_PROMPT="0", GIT_AUTHOR_NAME="agent", GIT_AUTHOR_EMAIL="agent@scale.invalid",
                        GIT_COMMITTER_NAME="agent", GIT_COMMITTER_EMAIL="agent@scale.invalid", GIT_CONFIG_COUNT="2",
                        GIT_CONFIG_KEY_0="commit.gpgsign", GIT_CONFIG_VALUE_0="false",
                        GIT_CONFIG_KEY_1="credential.helper", GIT_CONFIG_VALUE_1="")
        self.report = {"name": a.name, "repo": a.repo_name, "steps": []}

    # ------------------------------------------------------------ helpers
    def wr(self, *args):
        p = subprocess.run([WRANGLER, *args], capture_output=True, text=True, cwd=self.tmp)
        if p.returncode:
            raise RuntimeError(redact(f"wrangler {' '.join(args[:3])}: {p.stderr[-600:]}"))
        out = p.stdout
        starts = [i for i in (out.find("{"), out.find("[")) if i >= 0]
        return json.loads(out[min(starts):]) if starts else out

    def git(self, *args, cwd=None, auth=None):
        pre = ["-c", f"http.extraHeader=Authorization: {auth}"] if auth else []
        p = subprocess.run(["git", *pre, *args], cwd=cwd, env=self.env, capture_output=True, text=True)
        if p.returncode:
            raise RuntimeError(redact(f"git {args[0]}: {p.stderr.strip()[-400:]}"))
        return p.stdout.strip()

    def api(self, method, path, body=None, admin=False, key=None, timeout=300):
        tok = self.vars["NERUDO_ADMIN_TOKEN" if admin else "NERUDO_AGENT_TOKEN"]
        h = {"Content-Type": "application/json", "Authorization": f"Bearer {tok}", "User-Agent": UA}
        if key:
            h["X-Nerudo-Agent-Key"] = key
        req = urllib.request.Request(f"{self.worker}/v1/trunks/{self.a.name}/{path}", method=method,
                                     data=json.dumps(body).encode() if body is not None else None, headers=h)
        try:
            with urllib.request.urlopen(req, timeout=timeout) as r:
                return r.status, json.loads(r.read())
        except urllib.error.HTTPError as e:
            return e.code, {"error": redact(e.read().decode(errors="replace")[:600])}
        except Exception as e:  # a timeout or a reset connection is a result too
            return 0, {"error": redact(f"{type(e).__name__}: {e}")}

    def stats(self):
        st, s = self.api("GET", "stats", admin=True)
        keep = ("wasm_memory_bytes", "objects_cached", "store_bytes", "ui_objects_cached", "landed", "batches", "batch_sizes", "git_requests",
                "queued", "ejected_conflict", "ejected_check", "ejected_other", "seq")
        return {k: s.get(k) for k in keep} if st == 200 else {"status": st, **s}

    def step(self, step, **kv):
        kv = {"step": step, **kv}
        self.report["steps"].append(kv)
        print(redact(json.dumps(kv)), flush=True)

    # ------------------------------------------------------------ phases
    def ensure_repo(self):
        try:
            info = self.wr("artifacts", "repos", "get", self.a.repo_name, "--namespace", self.a.namespace, "--json")
            existed = True
        except RuntimeError:
            self.wr("artifacts", "repos", "create", self.a.repo_name, "--namespace", self.a.namespace, "--default-branch", "main", "--json")
            info = self.wr("artifacts", "repos", "get", self.a.repo_name, "--namespace", self.a.namespace, "--json")
            existed = False
        self.remote = info["remote"]
        # A repo from an earlier run has its main already (its trunk may have moved it on).
        if existed and not self.a.push:
            self.step("repo", existing=True)
            return
        tok = self.wr("artifacts", "repos", "issue-token", self.a.repo_name, "--namespace", self.a.namespace, "--scope", "write",
                      "--ttl", "7200", "--json")
        tok = tok.get("plaintext") or tok.get("token")
        t0 = time.time()
        self.git("push", "-q", self.remote, "main:refs/heads/main", cwd=self.a.repo, auth=f"Bearer {tok}")
        self.step("repo", pushed_s=round(time.time() - t0, 1))

    def redeploy(self):
        if not self.a.config:
            return
        p = subprocess.run([WRANGLER, "deploy", "--config", self.a.config], capture_output=True, text=True, cwd=self.tmp)
        m = re.search(r"Current Version ID: (\S+)", p.stdout)
        if p.returncode or not m:
            raise RuntimeError(redact(p.stderr[-600:]))
        self.report["version"] = m.group(1)
        self.step("deploy", version=m.group(1))
        time.sleep(3)

    def tail(self):
        self.tail_file = open(os.path.join(self.tmp, "tail.jsonl"), "w")
        self.tail_proc = subprocess.Popen([WRANGLER, "tail", self.a.worker_name, "--format", "json"], stdout=self.tail_file,
                                          stderr=subprocess.DEVNULL, cwd=self.tmp)
        time.sleep(6)

    def tail_events(self):
        self.tail_proc.terminate()
        self.tail_proc.wait()
        self.tail_file.close()
        text = open(self.tail_file.name).read()
        events, dec, i = [], json.JSONDecoder(), 0
        while i < len(text):
            j = text.find("{", i)
            if j < 0:
                break
            try:
                ev, i = dec.raw_decode(text, j)
            except json.JSONDecodeError:
                i = j + 1
                continue
            req = (ev.get("event") or {}).get("request") or {}
            kind = "alarm" if "scheduledTime" in (ev.get("event") or {}) else "fetch"
            # CPU and wall time of every invocation: a landing runs in an alarm.
            self.report.setdefault("invocations", []).append({"kind": kind, "outcome": ev.get("outcome"), "cpu_ms": ev.get("cpuTime"),
                                                               "wall_ms": ev.get("wallTime"), "path": redact(req.get("url", "")).split("/v1/trunks/")[-1][:60]})
            if ev.get("outcome") not in (None, "ok") or ev.get("exceptions"):
                ex = [redact(f"{x.get('name')}: {x.get('message')}")[:300] for x in ev.get("exceptions", [])]
                logs = [redact(" ".join(map(str, l.get("message", []))))[:200] for l in ev.get("logs", []) if l.get("level") in ("error", "warn")]
                events.append({"outcome": ev.get("outcome"), "ms": ev.get("eventTimestamp"), "url": redact(req.get("url", ""))[-80:],
                               "exceptions": ex, "logs": logs[-5:], "kind": kind})
        return events

    def init(self):
        body = {"trunk": self.a.name, "remote": self.remote,
                "host": {"kind": "artifacts", "binding": "ARTIFACTS", "repo": self.a.repo_name}}
        t0 = time.time()
        st, out = self.api("POST", "init", body, admin=True, timeout=self.a.init_timeout)
        self.step("init", status=st, s=round(time.time() - t0, 1), objects=out.get("objects"), error=out.get("error"),
                  stats=self.stats())
        return st == 200

    def fork(self):
        st, f = self.api("POST", "forks", {"agent": self.a.agent})
        if st != 200:
            raise RuntimeError(f"fork: {st} {f}")
        self.fork_remote, self.fork_auth, self.key, self.fork_name = f["remote"], f["authorization"], f.get("agent_key"), f["name"]
        self.trunk_remote, self.trunk_auth = f.get("trunk"), f.get("trunk_authorization")
        self.step("fork", fork=self.fork_name)

    def change(self, cid, pkg, base, push=True):
        """One commit on `base`: an edit in package number `pkg` and a line appended to CHANGELOG.md.
        It goes to the fork's land/<cid>-1 branch, unless push is False."""
        repo = self.a.repo
        self.git("read-tree", base, cwd=repo)
        pkgs = self.git("ls-tree", "--name-only", base, "svc/", cwd=repo).splitlines()
        path = self.git("ls-tree", "-r", "--name-only", base, pkgs[(pkg + self.a.pkg_offset) % len(pkgs)], cwd=repo).splitlines()[0]
        text = subprocess.run(["git", "cat-file", "blob", f"{base}:{path}"], cwd=repo, capture_output=True, check=True).stdout
        lines = text.split(b"\n")
        # The trunk's name makes the edit unique: trunks on one repo do not make the same edit.
        lines[1] = f"export const changed_by_{cid} = \"{self.a.name}\";".encode()
        blob = subprocess.run(["git", "hash-object", "-w", "--stdin"], cwd=repo, input=b"\n".join(lines), capture_output=True, check=True).stdout.decode().strip()
        log = subprocess.run(["git", "cat-file", "blob", f"{base}:CHANGELOG.md"], cwd=repo, capture_output=True, check=True).stdout + f"- {cid}\n".encode()
        lblob = subprocess.run(["git", "hash-object", "-w", "--stdin"], cwd=repo, input=log, capture_output=True, check=True).stdout.decode().strip()
        self.git("update-index", "--cacheinfo", f"100644,{blob},{path}", cwd=repo)
        self.git("update-index", "--cacheinfo", f"100644,{lblob},CHANGELOG.md", cwd=repo)
        tree = self.git("write-tree", cwd=repo)
        commit = self.git("commit-tree", tree, "-p", base, "-m", f"{cid}: edit {path}", cwd=repo)
        self.edited = path
        if push:
            self.git("push", "-q", self.fork_remote, f"{commit}:refs/heads/land/{cid}-1", cwd=repo, auth=self.fork_auth)
        return commit

    def land(self, cid, commit, base):
        body = {"change_id": cid, "rev": 1, "fork": self.fork_name, "commit": commit, "base": base}
        return self.api("POST", "land", body, key=self.key)

    def wait(self, cids, timeout):
        end, out = time.time() + timeout, {}
        while time.time() < end and len(out) < len(cids):
            for c in cids:
                if c in out:
                    continue
                st, r = self.api("GET", f"changes/{c}?wait=5", timeout=60)
                if r.get("state") in ("landed", "ejected"):
                    out[c] = {"state": r["state"], "reason": r.get("reason"), "submitted_ms": r.get("submitted_ms"),
                              "landed_ms": r.get("landed_ms"), "batch": r.get("batch")}
        for c in cids:
            if c not in out:
                st, r = self.api("GET", f"changes/{c}", timeout=60)
                out[c] = {"state": r.get("state", f"http {st}"), "reason": r.get("reason") or r.get("error"), "timed_out": True}
        return out

    def verify(self, head, cids):
        """Read main back from Artifacts: every landed change is on it once, its edit and its
        changelog line are in the tree, and git finds every object of the new commits."""
        repo = self.a.repo
        try:
            self.git("fetch", "-q", self.trunk_remote, "+refs/heads/main:refs/remotes/trunk/main", cwd=repo, auth=self.trunk_auth)
        except RuntimeError as e:
            # git cannot read the repo (Artifacts HTTP 500 on very big trees): check the landing
            # log instead. Each change once in the log, and the log's last commit is trunk's head.
            st, log = self.api("GET", "log")
            st2, stats = self.api("GET", "stats", admin=True)
            ids = [x["change_id"] for x in log] if isinstance(log, list) else []
            ok = sorted(ids) == sorted(cids) and isinstance(log, list) and log and log[-1].get("commit_out") == stats.get("head")
            self.step("verify", ok=bool(ok), by="landing log (git fetch failed)", landed=ids, git_error=str(e)[-160:])
            return ok
        log = self.git("log", "--format=%(trailers:key=Change-Id,valueonly)", f"{head}..trunk/main", cwd=repo).split()
        changelog = self.git("show", "trunk/main:CHANGELOG.md", cwd=repo)
        missing = [c for c in cids if f"- {c}" not in changelog]
        p = subprocess.run(["git", "fsck", "--connectivity-only", "--no-dangling", "trunk/main"], cwd=repo, capture_output=True, text=True)
        ok = sorted(log) == sorted(cids) and not missing and p.returncode == 0
        self.step("verify", ok=ok, landed=log, changelog_missing=missing, fsck=p.returncode, fsck_err=redact(p.stderr[-300:]))
        return ok

    def singles(self, n):
        """n changes, one at a time, each on trunk's head after the one before: the latency of
        one landing, as an agent that is up to date sees it."""
        out = []
        for i in range(n):
            self.git("fetch", "-q", self.trunk_remote, "+refs/heads/main:refs/remotes/trunk/main", cwd=self.a.repo, auth=self.trunk_auth)
            head = self.git("rev-parse", "trunk/main", cwd=self.a.repo)
            cid = f"s{i}"
            c = self.change(cid, i, head)
            t0 = time.time()
            st, _ = self.land(cid, c, head)
            r = self.wait([cid], self.a.timeout)[cid]
            server = (r["landed_ms"] - r["submitted_ms"]) / 1000 if r.get("landed_ms") and r.get("submitted_ms") else None
            out.append({"cid": cid, "state": r["state"], "server_s": server, "client_s": round(time.time() - t0, 2)})
            self.step("single", **out[-1])
        landed = sorted(x["server_s"] for x in out if x["state"] == "landed" and x["server_s"] is not None)
        self.step("singles", n=n, landed=len(landed), median_s=landed[len(landed) // 2] if landed else None,
                  min_s=landed[0] if landed else None, max_s=landed[-1] if landed else None, stats=self.stats())

    def ui(self):
        """The web app's API on this trunk, with the share link: each read, its time and
        status. Then a plain `git push` of a branch to the trunk's git endpoint, which opens a
        PR from the agent's fork; it must land."""
        st, inv = self.api("POST", "share", {}, admin=True)
        if st != 200:
            self.step("ui", error=f"share: {st}")
            return
        q = dict(urllib.parse.parse_qsl(urllib.parse.urlparse(inv["url"]).query))
        auth = f"k={urllib.parse.quote(q.get('k', ''))}"
        path = getattr(self, "edited", "README.md")
        reads = [("state", "state"), ("change", "change/one"), ("tree", "tree"), ("tree_dir", "tree?path=" + path.rsplit("/", 1)[0]),
                 ("tree_file", "tree?path=" + path), ("history", "history"), ("history_file", "history?path=" + path),
                 ("release_draft", "release/draft")]
        out = {}
        for name, rel in reads:
            sep = "&" if "?" in rel else "?"
            url = f"{self.worker}/t/{self.a.name}/api/{rel}{sep}{auth}"
            t0 = time.time()
            try:
                with urllib.request.urlopen(urllib.request.Request(url, headers={"User-Agent": UA}), timeout=300) as r:
                    body = r.read()
                    out[name] = {"status": r.status, "s": round(time.time() - t0, 2), "bytes": len(body)}
                    v = json.loads(body)
                    if name == "change":
                        out[name]["files"] = len(v.get("files") or [])
                    if name == "tree_file":
                        out[name]["has_text"] = bool(v.get("text"))
                    if name == "history_file":
                        out[name]["landings"] = len(v.get("landings") or [])
            except urllib.error.HTTPError as e:
                out[name] = {"status": e.code, "s": round(time.time() - t0, 2), "error": redact(e.read().decode(errors="replace")[:200])}
            except Exception as e:
                out[name] = {"status": 0, "s": round(time.time() - t0, 2), "error": redact(str(e))[:200]}
        self.step("ui", reads=out, stats=self.stats())
        # Plain git push: the trunk's git endpoint passes the push to the fork and opens a PR.
        st, log = self.api("GET", "log")
        base = log[-1]["commit_out"] if isinstance(log, list) and log else self.git("rev-parse", "main", cwd=self.a.repo)
        try:
            self.git("cat-file", "-e", base, cwd=self.a.repo)
        except RuntimeError:
            self.step("git_push", skipped="the local repo lacks trunk's head")
            return
        commit = self.change("gp", 9, base, push=False)
        basic = "Basic " + __import__("base64").b64encode(f"{self.a.agent}:{self.key}".encode()).decode()
        t0 = time.time()
        p = subprocess.run(["git", "-c", f"http.extraHeader=Authorization: {basic}", "push", f"{self.worker}/v1/trunks/{self.a.name}/git",
                            f"{commit}:refs/heads/t-9/gp"], cwd=self.a.repo, env=self.env, capture_output=True, text=True)
        lines = [l for l in redact(p.stderr).splitlines() if l.startswith("remote:") and l.strip() != "remote:"]
        pushed_s = round(time.time() - t0, 2)
        res = self.wait(["t-9-gp"], self.a.timeout)["t-9-gp"]
        self.step("git_push", rc=p.returncode, s=pushed_s, remote=lines[:4], result=res, landed_s=round(time.time() - t0, 2))

    def run(self):
        self.ensure_repo()
        self.redeploy()
        self.tail()
        try:
            if not self.init():
                return
            self.fork()
            # Trunk's head as Artifacts has it (a repo from an earlier run has moved on). Artifacts
            # answers HTTP 500 to a git fetch of a very big repo: then the local main.
            try:
                self.git("fetch", "-q", self.trunk_remote, "+refs/heads/main:refs/remotes/trunk/main", cwd=self.a.repo, auth=self.trunk_auth)
                head = self.git("rev-parse", "trunk/main", cwd=self.a.repo)
            except RuntimeError as e:
                self.step("trunk_fetch", ok=False, error=str(e)[-200:])
                head = self.git("rev-parse", "main", cwd=self.a.repo)
            if self.a.singles:
                self.singles(self.a.singles)
                return
            # One change on head.
            c = self.change("one", 0, head)
            t0 = time.time()
            st, r = self.land("one", c, head)
            res = self.wait(["one"], self.a.timeout)
            self.step("land_one", submit_status=st, s=round(time.time() - t0, 1), result=res["one"], stats=self.stats())
            if self.a.batch:
                # A batch built on the old head: the sequencer must read that base too.
                cids = [f"b{i}" for i in range(self.a.batch)]
                commits = {cid: self.change(cid, i + 1, head) for i, cid in enumerate(cids)}
                t0 = time.time()
                for cid in cids:
                    self.land(cid, commits[cid], head)
                res = self.wait(cids, self.a.timeout)
                self.step("land_batch", n=len(cids), s=round(time.time() - t0, 1), results=res, stats=self.stats())
            self.verify(head, ["one"] + [f"b{i}" for i in range(self.a.batch)])
            if self.a.ui:
                self.ui()
        finally:
            self.report["tail"] = self.tail_events()
            self.step("tail", events=len(self.report["tail"]), outcomes=sorted({e["outcome"] for e in self.report["tail"]}))
            with open(self.a.out, "w") as f:
                json.dump(self.report, f, indent=1)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--vars", required=True)
    ap.add_argument("--repo", required=True)
    ap.add_argument("--name", required=True, help="trunk id and Artifacts repo name (mono-...)")
    ap.add_argument("--repo-name", dest="repo_name", help="the Artifacts repo, if not NAME: an untouched repo from an earlier run")
    ap.add_argument("--namespace", default="nerudo-scale")
    ap.add_argument("--push", action="store_true", help="push REPO's main also to a repo that exists")
    ap.add_argument("--config", help="wrangler config of the throwaway Worker; redeploy before the test")
    ap.add_argument("--worker-name", dest="worker_name", default="nerudo-scale-mono")
    ap.add_argument("--batch", type=int, default=5)
    ap.add_argument("--agent", default="a00", help="the agent's name; its fork is <repo>--<agent>")
    ap.add_argument("--pkg-offset", dest="pkg_offset", type=int, default=0,
                    help="edit packages from this number on: files that earlier trunks on the same repo did not edit")
    ap.add_argument("--ui", action="store_true", help="after the landings: the web app's reads and a plain git push")
    ap.add_argument("--singles", type=int, default=0, help="instead of one change and a batch: N changes one at a time, each on the new head")
    ap.add_argument("--timeout", type=int, default=300, help="seconds to wait for the changes to land")
    ap.add_argument("--init-timeout", dest="init_timeout", type=int, default=150)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    a.repo_name = a.repo_name or a.name
    assert a.name.startswith("mono-") and a.repo_name.startswith("mono-"), "trunk ids and repos start with mono-"
    Bench(a).run()


if __name__ == "__main__":
    main()
