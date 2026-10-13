"""Agent strategies. Every strategy lands one change spec and records what it cost.

strict    regenerate on the latest trunk, re-run CI, compare-and-swap push to trunk
lax       CI once on own base, then merge trunk in without re-testing and push
            (what a merge button does when "require branch up to date" is off)
pr        forge PR + server-side merge API (the forge is the coordination point)
jj        strict, but through jj workspaces sharing one jj repo
seq       CI once on own base, push to refs/changes/<id>/<rev>, ask the sequencer to land it
"""
import json
import os
import random
import re
import subprocess
import threading
import time
import urllib.error
import urllib.request

from workload import apply_spec, regen_lock

LOCK_ERR = re.compile(r"cannot lock ref|Unable to create .*\.lock|unable to update local ref|"
                      r"could not lock config|shallow file has changed", re.I)
URL_CREDS = re.compile(r"://[^/@\s]+@")  # strip tokens from URLs before persisting stderr
SECRETS = re.compile(r"art_v\d+_[A-Za-z0-9]+(\?expires=\d+)?|(Bearer|Basic|token) [A-Za-z0-9+/=_.-]{16,}")
CF_UA = "nerudo-bench/0.1"  # workers.dev refuses Python's default User-Agent (error 1010)


def redact(text):
    """Remove tokens (URL credentials, Artifacts tokens, auth headers) before persisting text."""
    return SECRETS.sub("***", URL_CREDS.sub("://***@", text))


def with_header(env, url, authorization):
    """`env` plus an Authorization header git sends only to `url`: nothing is written to git config."""
    i = int(env.get("GIT_CONFIG_COUNT", "0"))
    return dict(env, GIT_CONFIG_COUNT=str(i + 1), **{f"GIT_CONFIG_KEY_{i}": f"http.{url}.extraHeader",
                                                      f"GIT_CONFIG_VALUE_{i}": f"Authorization: {authorization}"})
REJECT = re.compile(r"\[rejected\]|non-fast-forward|fetch first|stale info|"
                    r"unexpectedly moved on the remote|failed to push some refs|"
                    r"cannot lock ref .*: is at", re.I)
MAX_ATTEMPTS = 400
# The user's global config signs commits with their GPG key; benchmark commits must not.
# Also clear credential helpers: the user's config has `store` (plaintext ~/.git-credentials)
# and `osxkeychain`, and URLs here carry throwaway tokens that must not be persisted.
NO_SIGN = {"GIT_CONFIG_COUNT": "3", "GIT_CONFIG_KEY_0": "commit.gpgsign", "GIT_CONFIG_VALUE_0": "false",
           "GIT_CONFIG_KEY_1": "tag.gpgsign", "GIT_CONFIG_VALUE_1": "false",
           "GIT_CONFIG_KEY_2": "credential.helper", "GIT_CONFIG_VALUE_2": ""}


class Counters:
    FIELDS = ("fetch_errors", "push_attempts", "push_rejects", "push_errors", "ci_runs",
              "ci_failures", "resolutions", "api_calls", "api_retries", "ejections",
              "jj_errors", "jj_stale", "sideways_pushes", "regenerations", "fetches")

    def __init__(self):
        for f in self.FIELDS:
            setattr(self, f, 0)
        self.errors = []
        self.fetch_stderr = []  # retried fetch failures; not counted in error_count

    def as_dict(self):
        return {f: getattr(self, f) for f in self.FIELDS}


class Ctx:
    """Per-run settings shared by all agents."""
    def __init__(self, trunk, ci_seconds, t_gen=0.2, t_resolve=0.3, forge=None, seq_url=None,
                 run_id="", allow_backwards=False, cf_url=None, cf_token=None, agent_pretest=False):
        self.trunk = trunk
        self.cf_url, self.cf_token, self.agent_pretest = cf_url, cf_token, agent_pretest
        self.allow_backwards = allow_backwards
        self.deadline = float("inf")
        self.ci_seconds = ci_seconds
        self.t_gen = t_gen
        self.t_resolve = t_resolve
        self.forge = forge
        self.seq_url = seq_url
        self.run_id = run_id


class DeadlineExceeded(RuntimeError):
    pass


def check_deadline(a):
    if time.time() > a.ctx.deadline:
        raise DeadlineExceeded("run deadline exceeded")


class GitAgent:
    def __init__(self, idx, path, ctx):
        self.idx, self.path, self.ctx = idx, path, ctx
        self.c = Counters()
        name = f"agent-{idx:02d}"
        self.env = dict(os.environ, GIT_AUTHOR_NAME=name, GIT_COMMITTER_NAME=name,
                        GIT_AUTHOR_EMAIL=f"{name}@bench.invalid",
                        GIT_COMMITTER_EMAIL=f"{name}@bench.invalid",
                        GIT_TERMINAL_PROMPT="0", PYTHONDONTWRITEBYTECODE="1", **NO_SIGN, JJ_USER=name, JJ_EMAIL=f"{name}@bench.invalid")

    def run(self, *cmd, check=True, env=None):
        p = subprocess.run(cmd, cwd=self.path, env=env or self.env, capture_output=True, text=True,
                           timeout=600)
        if check and p.returncode:
            raise RuntimeError(f"{' '.join(cmd)} -> {p.returncode}: {p.stderr.strip()[:400]}")
        return p

    def git(self, *args, check=True):
        return self.run("git", *args, check=check)

    def fetch(self):
        for i in range(60):
            self.c.fetches += 1
            p = self.git("fetch", "-q", "origin", check=False)
            if p.returncode == 0:
                return
            self.c.fetch_errors += 1
            self.c.fetch_stderr.append(redact(p.stderr.strip())[:200])
            if not LOCK_ERR.search(p.stderr) and i > 5:
                raise RuntimeError(f"fetch failed: {p.stderr.strip()[:300]}")
            time.sleep(random.uniform(0.02, 0.1) * (1 + i / 10))
        raise RuntimeError("fetch: gave up after lock contention")

    def trunk_ref(self):
        return f"origin/{self.ctx.trunk}"

    def commit(self, subject):
        self.git("add", "-A")
        self.git("commit", "-q", "-m", subject)
        return self.git("rev-parse", "HEAD").stdout.strip()

    def ci(self, on_trunk=False):
        env = dict(self.env, CI_SECONDS=str(self.ctx.ci_seconds), CI_MAIN="1" if on_trunk else "0")
        self.c.ci_runs += 1
        p = self.run("python3", "ci/check.py", check=False, env=env)
        if p.returncode:
            self.c.ci_failures += 1
        return p.returncode == 0

    def push(self, refspec):
        """Returns 'ok', 'reject' (lost the race) or 'error'."""
        self.c.push_attempts += 1
        p = self.git("push", "-q", "--porcelain", "origin", refspec, check=False)
        if p.returncode == 0:
            return "ok"
        if REJECT.search(p.stderr + p.stdout):
            self.c.push_rejects += 1
            return "reject"
        self.c.push_errors += 1
        self.c.errors.append(p.stderr.strip()[:200])
        return "error"

    def resolve_conflicts(self, mine):
        """Resolve a merge in progress the way an agent would, without re-testing:
        regenerate the lockfile, keep our side of any content conflict."""
        self.c.resolutions += 1
        time.sleep(self.ctx.t_resolve)
        files = self.git("diff", "--name-only", "--diff-filter=U").stdout.split()
        for f in files:
            if f == "deps.lock":
                continue
            self.git("checkout", mine, "--", f)
        regen_lock(self.path)
        self.git("add", "-A")
        self.git("commit", "-q", "--no-edit")


# ---------------------------------------------------------------- strategies

def land_strict(a, spec):
    time.sleep(a.ctx.t_gen)
    ready = None
    for _ in range(MAX_ATTEMPTS):
        check_deadline(a)
        a.fetch()
        a.git("switch", "-q", "--detach", a.trunk_ref())
        apply_spec(a.path, spec, "numbered")
        a.commit(spec.subject)
        if ready is not None:
            a.c.regenerations += 1
        if not a.ci():
            continue
        ready = ready or time.time()
        r = a.push(f"HEAD:refs/heads/{a.ctx.trunk}")
        if r == "ok":
            return ready, time.time()
        if r == "error":
            time.sleep(0.5)
    raise RuntimeError("strict: too many attempts")


def _merge_into_trunk(a, mine):
    """On the latest trunk, merge `mine` with trunk as first parent. No re-test."""
    a.git("switch", "-q", "--detach", a.trunk_ref())
    p = a.git("merge", "-q", "--no-ff", "--no-edit", "-m", f"Merge {mine[:10]}", mine, check=False)
    if p.returncode:
        a.resolve_conflicts(mine)


def land_lax(a, spec):
    time.sleep(a.ctx.t_gen)
    a.fetch()
    a.git("switch", "-q", "--detach", a.trunk_ref())
    apply_spec(a.path, spec, "numbered")
    mine = a.commit(spec.subject)
    a.ci()
    ready = time.time()
    for _ in range(MAX_ATTEMPTS):
        check_deadline(a)
        a.fetch()
        _merge_into_trunk(a, mine)
        r = a.push(f"HEAD:refs/heads/{a.ctx.trunk}")
        if r == "ok":
            return ready, time.time()
        if r == "error":
            time.sleep(0.5)
    raise RuntimeError("lax: too many attempts")


def land_pr(a, spec):
    forge = a.ctx.forge
    time.sleep(a.ctx.t_gen)
    a.fetch()
    a.git("switch", "-q", "--detach", a.trunk_ref())
    apply_spec(a.path, spec, "numbered")
    head = a.commit(spec.subject)
    a.ci()
    ready = time.time()
    branch = f"pr/{a.ctx.run_id}/{spec.cid}"
    while a.push(f"HEAD:refs/heads/{branch}") != "ok":
        check_deadline(a)
        time.sleep(1)
    number = forge.create_pr(a, branch, spec.subject)
    for _ in range(MAX_ATTEMPTS):
        check_deadline(a)
        status = forge.merge_pr(a, number)
        if status == "merged":
            return ready, time.time()
        if status == "conflict":
            # "Update branch": merge trunk into the PR branch locally, resolve, push.
            a.fetch()
            a.git("switch", "-q", "--detach", head)
            p = a.git("merge", "-q", "--no-edit", a.trunk_ref(), check=False)
            if p.returncode:
                a.resolve_conflicts(head)
            head = a.git("rev-parse", "HEAD").stdout.strip()
            a.push(f"HEAD:refs/heads/{branch}")
        else:
            a.c.api_retries += 1
            time.sleep(random.uniform(0.3, 1.0))
    raise RuntimeError("pr: too many attempts")


class JjAgent(GitAgent):
    def jj(self, *args):
        """Run jj; on a stale working copy do what jj's hint says (update-stale) and retry."""
        for _ in range(20):
            p = self.run("jj", "--no-pager", "--color=never", *args, check=False)
            if "working copy is stale" not in p.stderr:
                return p
            self.c.jj_stale += 1
            self.run("jj", "--no-pager", "workspace", "update-stale", check=False)
        return p

    def commit_id(self):
        return self.jj("log", "--no-graph", "-r", "@", "-T", "commit_id").stdout.strip()


def land_jj(a, spec):
    """allow_backwards=False is jj's safe default. True models an agent that follows the
    'Use --allow-backwards to allow it' hint jj prints when the bookmark can't move."""
    trunk = a.ctx.trunk
    time.sleep(a.ctx.t_gen)
    ready = None
    for _ in range(MAX_ATTEMPTS):
        check_deadline(a)
        p = a.jj("git", "fetch")
        if p.returncode:
            a.c.fetch_errors += 1
            a.c.errors.append(p.stderr.strip()[:200])
            time.sleep(random.uniform(0.05, 0.2))
            continue
        p = a.jj("new", f"{trunk}@origin")
        if p.returncode:
            a.c.jj_errors += 1
            a.c.errors.append(p.stderr.strip()[:200])
            continue
        apply_spec(a.path, spec, "numbered")
        a.jj("describe", "-m", spec.subject)
        if ready is not None:
            a.c.regenerations += 1
        if not a.ci():
            continue
        ready = ready or time.time()
        mine = a.commit_id()
        a.c.push_attempts += 1
        if a.ctx.allow_backwards:
            p = a.jj("bookmark", "set", trunk, "-r", "@", "--allow-backwards")
        else:
            # Careful recipe: snap the local bookmark to the remote's position first (clears
            # conflicts left by other workspaces, never discards remote work), then move it
            # forward. Without this, one concurrent `bookmark set` wedges every agent.
            a.jj("bookmark", "set", trunk, "-r", f"{trunk}@origin", "--allow-backwards")
            p = a.jj("bookmark", "set", trunk, "-r", "@")
        if p.returncode:
            a.c.push_rejects += 1        # refused to move sideways: someone landed first
            continue
        p = a.jj("git", "push", "--bookmark", trunk)
        out = p.stderr + p.stdout
        if p.returncode == 0 and mine[:12] in out:
            if "move sideways" in out:
                a.c.sideways_pushes += 1  # overwrote whatever was on trunk
            return ready, time.time()
        if REJECT.search(out) or "conflicted" in out or "Nothing changed" in out:
            a.c.push_rejects += 1
        else:
            a.c.push_errors += 1
            a.c.errors.append(out.strip()[:200])
        time.sleep(random.uniform(0.02, 0.1))
    raise RuntimeError("jj: too many attempts")


def land_seq(a, spec):
    time.sleep(a.ctx.t_gen)
    rev = 0
    ready = None
    while rev < 50:
        check_deadline(a)
        rev += 1
        a.fetch()
        base = a.git("rev-parse", a.trunk_ref()).stdout.strip()
        a.git("switch", "-q", "--detach", base)
        apply_spec(a.path, spec, "next")
        commit = a.commit(spec.subject)
        if rev > 1:
            a.c.regenerations += 1
        if not a.ci():
            continue
        ready = ready or time.time()
        ref = f"refs/changes/{spec.cid}/{rev}"
        while a.push(f"HEAD:{ref}") != "ok":
            check_deadline(a)
            time.sleep(0.2)
        body = {"change_id": spec.cid, "rev": rev, "commit": commit, "base": base,
                "subject": spec.subject, "ref": ref}
        _http(a, "POST", f"{a.ctx.seq_url}/land", body)
        while True:
            check_deadline(a)
            st = _http(a, "GET", f"{a.ctx.seq_url}/changes/{spec.cid}?rev={rev}&wait=30")
            if st["state"] == "landed":
                return ready, time.time()
            if st["state"] == "ejected":
                a.c.ejections += 1
                break
    raise RuntimeError("seq: too many revisions")


def _http(a, method, url, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method,
                                 headers={"Content-Type": "application/json"})
    a.c.api_calls += 1
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.loads(r.read() or b"{}")


def _cf(a, method, path, body=None):
    """Call the nerudo Worker (Trunk Durable Object) as an agent."""
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(f"{a.ctx.cf_url}/{path}", data=data, method=method, headers={
        "Content-Type": "application/json", "Authorization": f"Bearer {a.ctx.cf_token}", "User-Agent": CF_UA})
    a.c.api_calls += 1
    with urllib.request.urlopen(req, timeout=90) as r:
        return json.loads(r.read() or b"{}")


def land_cf(a, spec):
    """Sequencer on Cloudflare: push to your own fork, ask the Trunk Durable Object to land,
    wait for the receipt. CI runs once per batch inside the sequencer; the agent's own test
    run is optional (ctx.agent_pretest) because only the sequencer's evidence counts."""
    time.sleep(a.ctx.t_gen)
    rev, ready = 0, None
    fork = a.fork
    while rev < 50:
        check_deadline(a)
        rev += 1
        a.fetch()
        base = a.git("rev-parse", a.trunk_ref()).stdout.strip()
        a.git("switch", "-q", "--detach", base)
        apply_spec(a.path, spec, "next")
        commit = a.commit(spec.subject)
        if rev > 1:
            a.c.regenerations += 1
        if a.ctx.agent_pretest and not a.ci():
            continue
        ready = ready or time.time()
        ref = f"refs/heads/land/{spec.cid}-{rev}"
        for _ in range(20):
            check_deadline(a)
            a.c.push_attempts += 1
            p = a.git("-c", f"http.extraHeader=Authorization: {fork['authorization']}", "push", "-q",
                      fork["remote"], f"HEAD:{ref}", check=False)
            if p.returncode == 0:
                break
            a.c.push_errors += 1
            a.c.errors.append(p.stderr.strip()[:200])
            time.sleep(0.5)
        _cf(a, "POST", "land", {"change_id": spec.cid, "rev": rev, "fork": fork["name"],
                                "commit": commit, "base": base})
        while True:
            check_deadline(a)
            st = _cf(a, "GET", f"changes/{spec.cid}?rev={rev}&wait=30")
            if st["state"] == "landed":
                return ready, time.time()
            if st["state"] == "ejected":
                a.c.ejections += 1
                break
            if st["state"] in ("awaiting_seal", "awaiting_allowance"):
                raise RuntimeError(f"{spec.cid} parked: {st.get('reason')}")
    raise RuntimeError("cf: too many revisions")


STRATEGIES = {"strict": land_strict, "lax": land_lax, "pr": land_pr, "jj": land_jj, "seq": land_seq, "cf": land_cf}


# ---------------------------------------------------------------- forge clients

class _Throttle:
    """Global spacing between write API calls (GitHub secondary limit is ~80/min)."""
    def __init__(self, min_interval):
        self.min_interval, self.lock, self.last = min_interval, threading.Lock(), 0.0

    def wait(self):
        with self.lock:
            delay = self.last + self.min_interval - time.time()
            if delay > 0:
                time.sleep(delay)
            self.last = time.time()


class Forge:
    def __init__(self, api, token, owner, repo, auth_scheme, min_interval):
        self.api, self.token, self.owner, self.repo = api, token, owner, repo
        self.auth_scheme = auth_scheme
        self.throttle = _Throttle(min_interval)

    def _call(self, a, method, path, body=None):
        self.throttle.wait()
        a.c.api_calls += 1
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(f"{self.api}{path}", data=data, method=method, headers={
            "Authorization": f"{self.auth_scheme} {self.token}", "Content-Type": "application/json",
            "Accept": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                raw = r.read()
                return r.status, json.loads(raw) if raw else {}
        except urllib.error.HTTPError as e:
            raw = e.read()
            try:
                return e.code, json.loads(raw)
            except ValueError:
                return e.code, {"message": raw.decode(errors="replace")[:300]}

    def create_pr(self, a, branch, title):
        for _ in range(20):
            code, body = self._call(a, "POST", f"/repos/{self.owner}/{self.repo}/pulls",
                                    {"head": branch, "base": a.ctx.trunk, "title": title})
            if code in (200, 201):
                return body["number"]
            a.c.api_retries += 1
            a.c.errors.append(f"create_pr {code}: {str(body)[:150]}")
            time.sleep(2)
        raise RuntimeError("create_pr failed")


class Forgejo(Forge):
    def __init__(self, base, token, owner, repo):
        super().__init__(f"{base}/api/v1", token, owner, repo, "token", 0.0)

    def merge_pr(self, a, number):
        code, body = self._call(a, "POST", f"/repos/{self.owner}/{self.repo}/pulls/{number}/merge",
                                {"Do": "merge"})
        if code == 200:
            return "merged"
        code, pr = self._call(a, "GET", f"/repos/{self.owner}/{self.repo}/pulls/{number}")
        if pr.get("merged"):
            return "merged"
        if pr.get("mergeable") is False and pr.get("state") == "open":
            return "conflict"
        return "retry"


class GitHub(Forge):
    def __init__(self, token, owner, repo):
        super().__init__("https://api.github.com", token, owner, repo, "Bearer", 0.8)

    def merge_pr(self, a, number):
        code, body = self._call(a, "PUT", f"/repos/{self.owner}/{self.repo}/pulls/{number}/merge",
                                {"merge_method": "merge"})
        if code == 200 and body.get("merged"):
            return "merged"
        code, pr = self._call(a, "GET", f"/repos/{self.owner}/{self.repo}/pulls/{number}")
        if pr.get("merged"):
            return "merged"
        if pr.get("mergeable") is False and pr.get("mergeable_state") == "dirty":
            return "conflict"
        return "retry"
