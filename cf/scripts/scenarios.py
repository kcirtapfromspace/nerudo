#!/usr/bin/env python3
"""Scenario tests for the nerudo Worker.

  approvals  ordinary changes wait for an allowance; a 2-landing budget lands exactly two;
             a CI edit waits for a human seal, signed with the nerudo CLI.
  quorum     several humans: a CI edit needs two maintainers (one key twice counts once,
             a key outside nerudo.toml is refused); changing the approver list takes the
             old quorum; the admin can't re-init the trunk's authority away, can pause it,
             and can break glass only for a change to nerudo.toml; `nerudo verify`
             re-checks every seal from outside.
  recovery   a crash between the push and the landing record still lands exactly once; a
             governance PR that lands through such a crash puts its rules in force.
  fencing    (Artifacts only) agent tokens can't write trunk or another agent's fork, a fork
             name that collides with a repo this trunk didn't make is refused, and an agent
             gets its fork token again only with its own key.
  context    an agent lands through `nerudo mcp` with context (secrets redacted). The
             sequencer keeps an immutable copy in its own context repo, and the landed commit's
             Context: trailer pins it; the agent force-pushing its own branch changes nothing,
             `nerudo verify` checks the pin, and context sent again is v2, not pinned.
  compare    a task asks for competing attempts: each PR gets a trial (CI as a dry run) and
             waits; a person picks one in the app, it merges, and the others close. A task
             whose pick policy is "smallest" picks by itself once every attempt is in.
  release    the sequencer drafts v0.1.0 from what merged; an agent proposes it; a forged
             signature, edited notes and a key outside nerudo.toml are refused; the
             approver signs with `nerudo release`, the tag is on the host, v0.1.0 can't be
             released twice, the next draft is v0.1.1, and `nerudo verify` checks the tag.
  hub        a trunk gets a name, ORG/REPO (`nerudo repo`): the org page (teams, people,
             agents) and the repo's summary read by name; a public repo reads with no key, a
             private one is hidden without the admin token; the trunk API and git work by
             name; bad and reserved names and callers without the admin token are refused;
             the name moves to another trunk, and goes.
  aliases    a governance PR renames an approver (same key): old approvals, tasks and
             events show the new name in the app; the admin can set and remove a former name.
  beaten     two agents change the same line from the same base. The second is closed for a
             conflict, and its receipt names the PR that beat it (number, agent, commit, its
             pinned context) and the conflicting lines; the MCP receipt shows that PR's plan.
  git        plain git: an anonymous clone of a private repo and a wrong password get 401;
             an agent clones with `nerudo git-credential`, which makes its fork with the
             agent token, keeps its key and gives git the key; `git push origin <branch>`
             opens a PR (linked to the task its name gives), which merges; a second commit
             on the merged branch opens a new PR (<branch id>..2), which merges with only
             the new line, and the branches view shows the branch as stale; a push to main
             or a tag is refused with the reason; a branch built on an older main merges
             with only its own diff.
  revisions  a PR's revisions (cf/scale/backlog.md, T-8 and T-9): on a paused trunk a new
             revision replaces the queued one, which never merges; an older revision and
             the same revision with another commit get 409; a parked revision is replaced
             too; a merged change id takes no new revision; while a revision is in the
             batch being pushed, a new one gets 409 (merging), and a git push waits for the
             batch and opens a new PR; plain git replaces a queued revision; `nerudo
             verify` finds each change id on main one time.
  ide_approval  the MCP tool request_approval: a client that declares elicitation gets it, and
             the person answers a form in it. An accept with approve ticked signs the same seal
             as `nerudo seal` (the PR merges and `nerudo verify` checks it); a decline signs
             nothing. A client without elicitation does not get the tool; a merged PR is refused.
  identity   each agent's own key comes with its first fork: the agent token alone (or
             another agent's key) can't get an agent's fork token; rotating revokes a key;
             the agent token can't act as a person (forks, cleanup, tasks, notes, git), nor
             as an agent that has a fork (land, claim, tasks, comments, reviews, reactions,
             cleanup, release proposals, notes, git push); the agent's key can; a new agent's
             first fork or first push takes the agent token; a PR's next revision comes only
             from its fork; an agent closes its own PR with its key; `nerudo mcp` without
             its key says what to do, and works after `nerudo agent-key`.
  clef       (not in the default run: calls the real model) [decide] with Cloudflare's Clef
             and clef-flash: every receipt keeps a real, calibrated answer; a PR that skips
             an auth check with a token in code waits for a person.

NERUDO_HOST=forgejo (default): against `wrangler dev`, Forgejo (cluster) as the git host.
NERUDO_HOST=artifacts: against the deployed Worker (cf/nerudo-worker/.prod.vars) with
Cloudflare Artifacts as the git host. The Trunk object creates and seeds the trunk repo
itself, so only it ever holds a token that can write trunk. Agents get a fork they can
write and a trunk token they can only read. The fencing scenario also squats a fork name
with `wrangler artifacts repos create`, so it needs your wrangler login. To run the
Artifacts scenarios against `wrangler dev` instead of the deployed Worker, set
NERUDO_VARS=cf/nerudo-worker/.dev.vars (wrangler dev reaches the real namespace).

A scratch HOME keeps the CLI's signing key and git config away from your real ones.
"""
import base64
import json
import os
import queue
import re
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
sys.path.insert(0, os.path.join(ROOT, "bench"))
from workload import init_tree  # noqa: E402

HOST = os.environ.get("NERUDO_HOST", "forgejo")
# The Forgejo API (or Gitea's, which is the same) and its token. cf/faults sets both for the
# Gitea it starts; the default is the cluster's Forgejo.
FORGEJO = os.environ.get("NERUDO_FORGEJO", "http://192.168.150.121:30300")
VARS = os.environ.get("NERUDO_VARS") or os.path.join(ROOT, "cf", "nerudo-worker", ".prod.vars" if HOST == "artifacts" else ".dev.vars")
DEV = dict(l.strip().split("=", 1) for l in open(VARS) if "=" in l)
WORKER = os.environ.get("NERUDO_CF_WORKER", DEV.get("NERUDO_WORKER", "http://127.0.0.1:8787"))
TOKEN = (os.environ.get("NERUDO_FORGEJO_TOKEN") or open(os.path.join(ROOT, "deploy", ".forgejo-token")).read().strip()) if HOST == "forgejo" else ""
UA = "nerudo-scenarios/0.1"
NAMESPACE = "nerudo"
SECRET = re.compile(r"art_v\d+_[A-Za-z0-9]+(\?expires=\d+)?|(Bearer|Basic|token) [A-Za-z0-9+/=_.-]{16,}|://[^/@\s]+@"
                    r"|lsa1\.[A-Za-z0-9_]+\.\d+\.[0-9a-f]+")
CLI = os.path.join(os.environ.get("CARGO_TARGET_DIR", os.path.join(ROOT, "cf", "target")), "debug", "nerudo")
SCRATCH = tempfile.mkdtemp(prefix="nerudo-scenarios-")
ENV = dict(os.environ, GIT_AUTHOR_NAME="agent", GIT_AUTHOR_EMAIL="agent@bench.invalid", GIT_COMMITTER_NAME="agent",
           GIT_COMMITTER_EMAIL="agent@bench.invalid", GIT_CONFIG_COUNT="2", GIT_CONFIG_KEY_0="commit.gpgsign",
           GIT_CONFIG_VALUE_0="false", GIT_CONFIG_KEY_1="credential.helper", GIT_CONFIG_VALUE_1="", PYTHONDONTWRITEBYTECODE="1", HOME=SCRATCH, GIT_TERMINAL_PROMPT="0")
BASIC = "Basic " + base64.b64encode(f"bench:{TOKEN}".encode()).decode() if TOKEN else ""


def redact(text):
    return SECRET.sub("***", text)


def sh(*cmd, cwd=None, inp=None):
    p = subprocess.run(cmd, cwd=cwd, env=ENV, capture_output=True, text=True, input=inp, timeout=120)
    if p.returncode:
        raise RuntimeError(redact(f"{' '.join(cmd)}: {p.stderr.strip()[:300]}"))
    return p.stdout.strip()


def header(auth):
    return f"http.extraHeader=Authorization: {auth}"


def api(trunk, method, path, body=None, admin=False, key=None):
    """`key`: an agent's own key; the request then acts for that agent only."""
    tok = DEV["NERUDO_ADMIN_TOKEN" if admin else "NERUDO_AGENT_TOKEN"]
    headers = {"Content-Type": "application/json", "Authorization": f"Bearer {tok}",
               "User-Agent": UA}  # Python's default UA is refused by workers.dev (error 1010)
    if key:
        headers["X-Nerudo-Agent-Key"] = key
    req = urllib.request.Request(f"{WORKER}/v1/trunks/{trunk}/{path}", method=method,
                                 data=json.dumps(body).encode() if body is not None else None, headers=headers)
    try:
        return json.loads(urllib.request.urlopen(req, timeout=90).read())
    except urllib.error.HTTPError as e:
        return {"http_error": e.code, "body": e.read().decode()}


INITS = {}  # trunk -> the init body, for re-init checks


def new_trunk(tag, rules="", files=None, **cfg):
    """`rules` is appended to the bench nerudo.toml: approvers, quorums, allowances.
    `files` (path -> text) are added to the seed tree."""
    name = f"scn-{tag}-{time.strftime('%H%M%S')}"
    seed = os.path.join(SCRATCH, name)
    os.makedirs(seed)
    init_tree(seed)
    for path, text in (files or {}).items():
        os.makedirs(os.path.dirname(os.path.join(seed, path)) or seed, exist_ok=True)
        open(os.path.join(seed, path), "w").write(text)
    sh("cp", os.path.join(ROOT, "cf", "fixtures", "bench-nerudo.toml"), os.path.join(seed, "nerudo.toml"))
    with open(os.path.join(seed, "nerudo.toml"), "a") as fh:
        fh.write("\n" + rules)
    if HOST == "artifacts":
        # The Trunk object creates the repo and pushes this tree as its first commit.
        files = {os.path.relpath(os.path.join(d, f), seed): open(os.path.join(d, f)).read()
                 for d, _, fs in os.walk(seed) for f in fs}
        body = {"trunk": name, "host": {"kind": "artifacts", "binding": "ARTIFACTS", "repo": name},
                "seed": {"files": files, "message": "seed\n"}}
        out = api(name, "POST", "init", {**body, **cfg}, admin=True)
        assert "head" in out, out
        INITS[name] = {k: v for k, v in body.items() if k != "seed"}
        return name
    else:
        req = urllib.request.Request(f"{FORGEJO}/api/v1/user/repos", method="POST",
                                     data=json.dumps({"name": name, "default_branch": "main"}).encode(),
                                     headers={"Authorization": f"token {TOKEN}", "Content-Type": "application/json"})
        urllib.request.urlopen(req).read()
        remote, seed_auth = f"{FORGEJO}/bench/{name}.git", BASIC
        body = {"trunk": name, "remote": remote, "authorization": BASIC,
                "host": {"kind": "forgejo", "api": f"{FORGEJO}/api/v1", "owner": "bench", "repo": name, "token": TOKEN}}
    sh("git", "init", "-q", "-b", "main", cwd=seed)
    sh("git", "add", "-A", cwd=seed)
    sh("git", "commit", "-q", "-m", "seed", cwd=seed)
    sh("git", "-c", header(seed_auth), "push", "-q", remote, "HEAD:refs/heads/main", cwd=seed)
    out = api(name, "POST", "init", {**body, **cfg}, admin=True)
    assert "head" in out, out
    INITS[name] = body
    return name


class Agent:
    def __init__(self, trunk, name):
        self.trunk = trunk
        self.name = name
        self.fork = api(trunk, "POST", "forks", {"agent": name})
        assert "remote" in self.fork, redact(json.dumps(self.fork))
        # The agent's own key comes with its first fork; its landings send it.
        self.key = self.fork.get("agent_key")
        self.dir = os.path.join(SCRATCH, f"{trunk}-{name}")
        # Trunk is read-only to agents: a read token from the sequencer (Artifacts), or the
        # bench login (Forgejo, where the agent token is not scoped).
        self.trunk_auth = self.fork.get("trunk_authorization") or BASIC
        sh("git", "-c", header(self.trunk_auth), "clone", "-q", self.fork["trunk"], self.dir)

    def change(self, cid, edit, base="origin/main", rev=1, task=None):
        """Build the change on `base` (latest trunk unless given) and ask to land it."""
        return self.send(self.prepare(cid, edit, base, rev, task))

    def send(self, body):
        return api(self.trunk, "POST", "land", body, key=self.key)

    def prepare(self, cid, edit, base="origin/main", rev=1, task=None):
        """Build the change and push it to the fork: the body that asks to land it."""
        sh("git", "-c", header(self.trunk_auth), "fetch", "-q", "origin", cwd=self.dir)
        sh("git", "switch", "-q", "--detach", base, cwd=self.dir)
        base = sh("git", "rev-parse", "HEAD", cwd=self.dir)
        edit(self.dir)
        sh("git", "add", "-A", cwd=self.dir)
        sh("git", "commit", "-q", "-m", f"{cid}: change", cwd=self.dir)
        commit = sh("git", "rev-parse", "HEAD", cwd=self.dir)
        sh("git", "-c", f"http.extraHeader=Authorization: {self.fork['authorization']}", "push", "-q",
           self.fork["remote"], f"HEAD:refs/heads/land/{cid}-{rev}", cwd=self.dir)
        body = {"change_id": cid, "rev": rev, "fork": self.fork["name"], "commit": commit, "base": base}
        if task:
            body["task"] = task
        return body


def wait(trunk, cid, want, timeout=30):
    end = time.time() + timeout
    while time.time() < end:
        st = api(trunk, "GET", f"changes/{cid}?wait=5")
        if st.get("state") == want:
            return st
        if st.get("state") in ("landed", "ejected") and st["state"] != want:
            raise AssertionError(f"{cid}: {st['state']} ({st.get('reason')}), wanted {want}")
    raise AssertionError(f"{cid}: still {st.get('state')} ({st.get('reason')}), wanted {want}")


def cli(*args, home=None, admin=False):
    """Run the nerudo CLI. `home` picks whose ~/.nerudo/key signs; `admin` gives it the
    admin token."""
    env = dict(ENV, NERUDO_WORKER=WORKER, NERUDO_TOKEN=DEV["NERUDO_AGENT_TOKEN"])
    if admin:
        env["NERUDO_ADMIN_TOKEN"] = DEV["NERUDO_ADMIN_TOKEN"]
    if home:
        os.makedirs(home, exist_ok=True)
        env["HOME"] = home
    if HOST == "forgejo":
        env["NERUDO_TRUNK_AUTHORIZATION"] = BASIC  # `verify` reads trunk with the bench login
    p = subprocess.run([CLI, *args], env=env, capture_output=True, text=True)
    if p.returncode:
        raise RuntimeError(redact(f"nerudo {' '.join(args)}: {p.stderr.strip()} {p.stdout.strip()[-400:]}"))
    return p.stdout.strip()


def approver(name, key, roles):
    return f'[[approvers]]\nname = "{name}"\nkey = "{key}"\nroles = {json.dumps(roles)}\n'


def edit_ci(d):
    p = os.path.join(d, "ci", "check.py")
    open(p, "a").write("# relax nothing, but this is still a governance edit\n")


def edit_rules(text):
    def f(d):
        open(os.path.join(d, "nerudo.toml"), "a").write(text)
    return f


def edit_target(cid, t):
    def f(d):
        p = os.path.join(d, "targets", t, "src.txt")
        lines = open(p).readlines()
        lines[0] = f"{cid} edited\n"
        open(p, "w").writelines(lines)
        open(os.path.join(d, "changes", f"{cid}.txt"), "w").write(cid + "\n")
    return f


def approvals():
    pk = cli("keygen")
    trunk = new_trunk("approvals", approver("you", pk, ["maintainer"]) + "\n[allowances]\nrequired = true\n")
    a = Agent(trunk, "a00")
    print(" submitted c1:", a.change("c1", edit_target("c1", "t01"))["state"])
    st = wait(trunk, "c1", "awaiting_allowance")
    print(" c1 parked:", st["reason"])
    out = json.loads(cli("allowance", "--trunk", trunk, "--units", "targets/*,changes/*", "--max", "2", "--ttl", "1h"))
    print(" signed allowance", out["allowance"], "requeued", out["requeued"])
    print(" c1:", wait(trunk, "c1", "landed")["state"])
    a.change("c2", edit_target("c2", "t02"))
    print(" c2:", wait(trunk, "c2", "landed")["state"])
    a.change("c3", edit_target("c3", "t03"))
    st = wait(trunk, "c3", "awaiting_allowance")
    print(" c3 parked after the budget of 2 was spent:", st["reason"]["kind"])
    a.change("g1", edit_ci)
    st = wait(trunk, "g1", "awaiting_seal")
    print(" g1 parked:", st["seal_reasons"])
    print(" seal:", cli("seal", "--trunk", trunk, "--change", "g1", "--yes").splitlines()[-1])
    st = wait(trunk, "g1", "landed")
    print(" g1:", st["state"], "seq", st["seq"])
    stats = api(trunk, "GET", "stats")
    assert stats["landed"] == 3 and stats["seq"] == 3, stats
    log = api(trunk, "GET", "log")
    assert [e["change_id"] for e in log] == ["c1", "c2", "g1"], log
    # The admin closes c3 for its author: it waits for a person. The agent token alone names
    # no agent, so it can't close a PR (identity: an agent closes its own with its key); a
    # landed PR can't be closed.
    assert api(trunk, "POST", "changes/c3/close", {"as": "a00"}).get("http_error") == 403
    assert api(trunk, "POST", "changes/c1/close", {"as": "a00"}, admin=True).get("http_error") == 409
    closed = api(trunk, "POST", "changes/c3/close", {"as": "a00", "reason": "the budget is spent"}, admin=True)
    assert closed["state"] == "ejected" and closed["reason"] == {"kind": "closed", "by": "a00", "detail": "the budget is spent"}, closed
    assert api(trunk, "GET", "changes/c3")["state"] == "ejected" and api(trunk, "GET", "stats")["ejected_other"] == 1
    print(" the admin closed c3 for a00 (it waited for an allowance); the agent token alone can't close it (403); "
          "the landed c1 can't be closed (409)")
    print(" approvals: ok (3 landed, c3 closed, log chain", log[-1]["hash"][:12] + ")")


def quorum():
    home = {n: os.path.join(SCRATCH, f"home-{n}") for n in ("alice", "bob", "carol", "mallory")}
    key = {n: cli("keygen", home=h) for n, h in home.items()}
    rules = (approver("alice", key["alice"], ["maintainer"]) + approver("bob", key["bob"], ["maintainer"])
             + approver("carol", key["carol"], ["dba"])
             + '\n[seals.require]\ngovernance = { roles = ["maintainer"], count = 2 }\n'
             + 'irreversible = { roles = ["dba"], count = 1 }\n')
    trunk = new_trunk("quorum", rules)
    a = Agent(trunk, "a00")

    def seal(cid, who):
        return json.loads(cli("seal", "--trunk", trunk, "--change", cid, "--yes", home=home[who]).splitlines()[-1])

    def refused(cid, who):
        try:
            seal(cid, who)
        except RuntimeError as e:
            return str(e)
        raise AssertionError(f"{who}'s seal on {cid} was accepted")

    # A CI edit needs two maintainers.
    a.change("g1", edit_ci)
    st = wait(trunk, "g1", "awaiting_seal")
    assert st["reason"]["quorum"] == [{"kind": "governance", "need": 2, "roles": ["maintainer"], "have": []}], st["reason"]
    assert "not an approver" in refused("g1", "mallory")
    assert seal("g1", "carol")["quorum"][0]["have"] == [], "the DBA counted as a maintainer"
    assert seal("g1", "alice")["complete"] is False
    again = seal("g1", "alice")
    assert again["quorum"][0]["have"] == ["alice"] and not again["complete"], again
    assert api(trunk, "GET", "changes/g1")["state"] == "awaiting_seal"
    assert seal("g1", "bob")["complete"] is True
    print(" g1 (CI edit): carol (dba) and alice twice weren't enough; landed after bob, seq", wait(trunk, "g1", "landed")["seq"])

    # Adding an approver is a governance change: the current list decides, not the newcomer.
    a.change("r1", edit_rules(approver("mallory", key["mallory"], ["maintainer"])))
    wait(trunk, "r1", "awaiting_seal")
    assert "not an approver" in refused("r1", "mallory")
    seal("r1", "alice"), seal("r1", "bob")
    wait(trunk, "r1", "landed")
    assert "mallory" in api(trunk, "GET", "stats")["approvers"]
    a.change("g2", edit_ci)
    wait(trunk, "g2", "awaiting_seal")
    seal("g2", "mallory"), seal("g2", "alice")
    print(" r1 (adds mallory): sealed by alice and bob; then g2 landed with mallory's seal, seq", wait(trunk, "g2", "landed")["seq"])

    # The admin token can't change who approves or what the trunk is.
    body = INITS[trunk]
    assert api(trunk, "POST", "init", {**body, "approvers": [key["mallory"]]}, admin=True).get("http_error") == 400
    other = {**body, "host": {**body["host"], "repo": body["host"]["repo"] + "x"}}
    assert api(trunk, "POST", "init", other, admin=True).get("http_error") == 409
    assert "head" in api(trunk, "POST", "init", {**body, "ci_ms": 0}, admin=True), "re-init of ci_ms refused"

    # Pause holds landings; resume lets them go.
    api(trunk, "POST", "pause", {"reason": "scenario"}, admin=True)
    a.change("c1", edit_target("c1", "t01"))
    time.sleep(4)
    assert api(trunk, "GET", "changes/c1")["state"] == "queued"
    api(trunk, "POST", "resume", {}, admin=True)
    wait(trunk, "c1", "landed")
    print(" admin: re-init with approvers 400, with another repo 409; c1 held while paused, landed on resume")

    # Break-glass lands a rules change only, and says so on trunk.
    a.change("g3", edit_ci)
    wait(trunk, "g3", "awaiting_seal")
    api(trunk, "POST", "break-glass", {"change_id": "g3", "rev": 1, "reason": "try to skip CI review"}, admin=True)
    st = wait(trunk, "g3", "awaiting_seal")
    end = time.time() + 20
    while not st["reason"].get("break_glass_refused") and time.time() < end:
        st = api(trunk, "GET", "changes/g3")
    assert st["state"] == "awaiting_seal" and st["reason"]["break_glass_refused"], st
    a.change("r2", edit_rules("# recovered by the admin\n"))
    wait(trunk, "r2", "awaiting_seal")
    api(trunk, "POST", "break-glass", {"change_id": "r2", "rev": 1, "reason": "two maintainers left"}, admin=True)
    wait(trunk, "r2", "landed")
    print(" break-glass: refused for g3 (CI edit), landed r2 (nerudo.toml only)")

    out = cli("verify", "--trunk", trunk)
    lines = [l.strip() for l in out.splitlines()]
    for want in ("governance, sealed by alice, bob", "governance, sealed by alice, mallory", "break-glass: two maintainers left"):
        assert any(l.endswith(want) for l in lines), out  # carol's seal on g1 didn't count, so it isn't listed
    print(" verify:", out.splitlines()[2].strip())
    print(" quorum: ok")


def recovery():
    home = os.path.join(SCRATCH, "home-recovery")
    pk = cli("keygen", home=home)
    trunk = new_trunk("recovery", approver("you", pk, ["maintainer"]), debug_crash_after_push=2)
    a = Agent(trunk, "a00")
    a.change("r1", edit_target("r1", "t04"))
    st = wait(trunk, "r1", "landed", timeout=40)
    stats = api(trunk, "GET", "stats")
    check = os.path.join(SCRATCH, f"{trunk}-check")
    sh("git", "-c", header(a.trunk_auth), "clone", "-q", a.fork["trunk"], check)
    ids = sh("git", "log", "--format=%(trailers:key=Change-Id,valueonly)", cwd=check).split()
    assert ids.count("r1") == 1, ids
    assert stats["recovered_attempts"] == 1 and stats["landed"] == 1 and stats["seq"] == 1, stats
    print(f" r1: crash after push, reconciled from trunk: landed once as seq {st['seq']}, recovered_attempts=1")

    # A governance PR that merges through a crash: the rules it lands are in force from its
    # landing on, as for a batch recorded at once.
    def rename(d):
        p = os.path.join(d, "nerudo.toml")
        text = open(p).read()
        assert 'name = "you"' in text
        open(p, "w").write(text.replace('name = "you"', 'name = "kim"'))

    a.change("g1", rename)
    wait(trunk, "g1", "awaiting_seal")
    cli("seal", "--trunk", trunk, "--change", "g1", "--yes", home=home)
    st = wait(trunk, "g1", "landed", timeout=40)
    after = api(trunk, "GET", "stats")
    assert after["recovered_attempts"] == 2 and after["seq"] == 2, after
    assert after["approvers"] == ["kim"] and after["rules_digest"] != stats["rules_digest"], after
    # The next PR merges under those rules, and `nerudo verify` checks every landing.
    a.change("r2", edit_target("r2", "t05"))
    wait(trunk, "r2", "landed", timeout=40)
    cli("verify", "--trunk", trunk)
    print(f" g1: renamed the approver through a crash (seq {st['seq']}): the sequencer has kim's rules; r2 merged; verify ok")
    print(" recovery: ok")


def authority_recovery():
    """Rules and receipts survive both crash windows, including one with no intent left."""
    home = os.path.join(SCRATCH, "home-authority-recovery")
    pk = cli("keygen", home=home)
    for window, hook in (("push", "debug_crash_after_push"), ("record", "debug_crash_after_record")):
        trunk = new_trunk(f"authority-{window}", approver("you", pk, ["maintainer"]), **{hook: 1})
        a = Agent(trunk, "a00")
        old_digest = api(trunk, "GET", "stats")["rules_digest"]

        def guard(d):
            path = os.path.join(d, "nerudo.toml")
            text = open(path).read().replace('[seals]\n', '[seals]\napproval = ["targets/t05/**"]\n')
            open(path, "w").write(text)

        a.change("guard", guard)
        wait(trunk, "guard", "awaiting_seal")
        cli("seal", "--trunk", trunk, "--change", "guard", "--yes", home=home)
        wait(trunk, "guard", "landed", timeout=60)
        stats = api(trunk, "GET", "stats")
        assert stats["rules_digest"] != old_digest, stats
        assert stats["recovered_attempts"] == (1 if window == "push" else 0), stats
        a.change("guarded", edit_target("guarded", "t05"))
        pending = wait(trunk, "guarded", "awaiting_seal", timeout=60)
        assert any(r.startswith("approval:") for r in pending["seal_reasons"]), pending
        assert api(trunk, "GET", "stats")["landed"] == 1
        cli("seal", "--trunk", trunk, "--change", "guarded", "--yes", home=home)
        wait(trunk, "guarded", "landed", timeout=60)
        assert [e["change_id"] for e in api(trunk, "GET", "log")] == ["guard", "guarded"]
        assert "landed changes, each on trunk once" in cli("verify", "--trunk", trunk)
    print(" authority_recovery: ok (both crash windows retain new authority; guarded work requires its seal)")


def allowance_recovery():
    """A failed candidate releases the only signed budget slot to a parked valid change."""
    home = os.path.join(SCRATCH, "home-allowance-recovery")
    pk = cli("keygen", home=home)
    trunk = new_trunk("allowance-recovery", approver("you", pk, ["maintainer"])
                      + '\n[allowances]\nrequired = true\n[ci]\ntypecheck = { tsconfig = "tsconfig.json" }\n',
                      files={"tsconfig.json": TSCONFIG, "src/base.ts": "export const base = 1;\n"},
                      debug_crash_after_push=1)
    a = Agent(trunk, "a00")
    api(trunk, "POST", "pause", {"reason": "queue both before reserving the one budget slot"}, admin=True)
    cli("allowance", "--trunk", trunk, "--units", "src/**", "--max", "1", "--ttl", "1h", home=home)
    a.change("bad", ts_file("src/bad.ts", "export const bad: number = 'bad';\n"))
    a.change("good", ts_file("src/good.ts", "export const good: number = 1;\n"))
    api(trunk, "POST", "resume", {}, admin=True)
    bad = wait(trunk, "bad", "ejected", timeout=60)
    assert bad["reason"]["kind"] == "ci_failed", bad
    wait(trunk, "good", "landed", timeout=60)
    assert [e["change_id"] for e in api(trunk, "GET", "log")] == ["good"]
    # Releasing a reservation must not reset spent budget or admit out-of-budget work.
    a.change("extra", ts_file("src/extra.ts", "export const extra = 1;\n"))
    wait(trunk, "extra", "awaiting_allowance", timeout=60)
    stats = api(trunk, "GET", "stats")
    assert stats["landed"] == 1 and stats["ejected_ci"] == 1, stats
    assert stats["recovered_attempts"] == 1, stats
    assert stats["awaiting_allowance"] >= 2, "the valid candidate must actually have waited for the reserved slot"
    assert "landed changes, each on trunk once" in cli("verify", "--trunk", trunk)
    print(" allowance_recovery: ok (failed candidate wakes valid work; after-push recovery charges once; spent budget stays spent)")


TSCONFIG = json.dumps({"compilerOptions": {"strict": True, "target": "es2022", "module": "es2022",
                                          "lib": ["es2022"], "skipLibCheck": True}, "include": ["src"]})
PRICE_1 = "export function price(cents: number): string {\n  return (cents / 100).toFixed(2);\n}\n"
PRICE_2 = ("export function price(cents: number, currency: string): string {\n"
           "  return `${(cents / 100).toFixed(2)} ${currency}`;\n}\n")


def ts_file(path, text):
    def f(d):
        os.makedirs(os.path.dirname(os.path.join(d, path)), exist_ok=True)
        open(os.path.join(d, path), "w").write(text)
    return f


def add_currency(d):
    """Change price() to take a currency, and update every caller this agent can see."""
    open(os.path.join(d, "src", "price.ts"), "w").write(PRICE_2)
    for name in os.listdir(os.path.join(d, "src")):
        p = os.path.join(d, "src", name)
        text = open(p).read()
        if name != "price.ts" and "price(" in text and "'EUR'" not in text:
            open(p, "w").write(re.sub(r"price\(([^()]*(?:\([^()]*\))?[^()]*)\)", r"price(\1, 'EUR')", text))


def add_cart(d):
    """Add a caller of price(), written against the signature this agent can see."""
    two = "currency" in open(os.path.join(d, "src", "price.ts")).read()
    call = "price(n * 100, 'EUR')" if two else "price(n * 100)"
    ts_file("src/cart.ts", f"import {{ price }} from './price';\nexport const line = (n: number) => {call};\n")(d)


def ci():
    """Real CI: each batch is type-checked by tsc (ts-rust) in the nerudo-ci Worker."""
    trunk = new_trunk("ci", rules='[ci]\ntypecheck = { tsconfig = "tsconfig.json" }\n', files={
        "tsconfig.json": TSCONFIG, "src/price.ts": PRICE_1,
        "src/index.ts": "import { price } from './price';\nexport const total = price(250);\n"})
    a, b, c = Agent(trunk, "a00"), Agent(trunk, "a01"), Agent(trunk, "a02")
    seed = api(trunk, "GET", "trunk")["head"]
    # Both start from the seed: each change passes on its own base; together they don't.
    a.change("sig", add_currency, base=seed)
    b.change("cart", add_cart, base=seed)
    out = {cid: wait_done(trunk, cid) for cid in ("sig", "cart")}
    failed = [cid for cid, st in out.items() if st["state"] == "ejected"]
    assert len(failed) == 1, {k: (v["state"], v.get("reason")) for k, v in out.items()}
    loser = failed[0]
    reason = out[loser]["reason"]
    assert reason["kind"] == "ci_failed" and any(d["code"] == "TS2554" for d in reason["diagnostics"]), reason
    first = [d for d in reason["diagnostics"] if d["code"] == "TS2554"][0]
    # The receipt names the change that moved trunk under it.
    other = {"sig": "cart", "cart": "sig"}[loser]
    assert reason.get("moved") and reason["moved"][0]["change_id"] == other, reason.get("moved")
    # Its agent builds the change again on the new trunk, as it would after a conflict.
    {"sig": a, "cart": b}[loser].change(loser, {"sig": add_currency, "cart": add_cart}[loser], rev=2)
    st = wait(trunk, loser, "landed", timeout=60)
    assert api(trunk, "GET", f"changes/{loser}?rev=2")["state"] == "landed"
    # A plain type error, and a tree the checker itself can't finish.
    c.change("bad", ts_file("src/bad.ts", "export const n: number = 'one';\n"))
    bad = wait(trunk, "bad", "ejected", timeout=60)["reason"]
    assert bad["kind"] == "ci_failed" and bad["diagnostics"][0]["code"] == "TS2322", bad
    c.change("deep", ts_file("src/deep.ts", "export const x = " + " + ".join(["1"] * 4000) + ";\n"))
    deep = wait(trunk, "deep", "ejected", timeout=60)["reason"]
    assert deep["kind"] == "ci_crashed", deep
    c.change("good", ts_file("src/good.ts", "import { price } from './price';\nexport const ok = price(1, 'EUR');\n"))
    wait(trunk, "good", "landed", timeout=60)
    stats = api(trunk, "GET", "stats")
    assert stats["ejected_ci"] == 3 and stats["landed"] == 3, stats
    print(f" ci: ok ({loser} passed on its own base but failed on trunk: {first['code']} {first['file']}:{first['line']} "
          f"{first['text']}; its receipt named {other} as the change that moved trunk; rev 2 on the new trunk landed as "
          f"seq {st['seq']}; a type error and a checker crash were "
          f"ejected; {stats['ci_runs']} CI runs for {stats['landed']} landings and {stats['ejected_ci']} CI ejections)")


def ci_red():
    """Trunk itself fails the type check: the sequencer ejects nobody, and a batch that
    makes trunk pass lands."""
    trunk = new_trunk("ci-red", rules='[ci]\ntypecheck = { tsconfig = "tsconfig.json" }\n', files={
        "tsconfig.json": TSCONFIG, "src/price.ts": PRICE_1, "src/bad.ts": "export const n: number = 'one';\n"})
    a, b = Agent(trunk, "a00"), Agent(trunk, "a01")
    a.change("c1", edit_target("c1", "t01"))
    time.sleep(10)
    st = api(trunk, "GET", "changes/c1")
    assert st["state"] == "queued", (st["state"], st.get("reason"))
    b.change("fix", ts_file("src/bad.ts", "export const n: number = 1;\n"))
    landed = {cid: wait(trunk, cid, "landed", timeout=90) for cid in ("c1", "fix")}
    stats = api(trunk, "GET", "stats")
    assert stats["ejected_ci"] == 0 and stats["landed"] == 2, stats
    print(f" ci_red: ok (trunk failed the type check: c1 stayed queued, nobody was ejected; with the fix it "
          f"landed as seq {landed['c1']['seq']} and the fix as seq {landed['fix']['seq']})")


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


def web(url, path="", form=None):
    """GET a page, or POST a form, at `path` under a share or personal link. Returns
    (status, Location header, body text); redirects are not followed."""
    base, query = url.split("?", 1)
    root = base.split("/t/")[0] + "/t/" + base.split("/t/")[1].split("/")[0]
    target = f"{root}/{path}?{query}" if path else f"{root}?{query}"
    data = urllib.parse.urlencode(form).encode() if form is not None else None
    req = urllib.request.Request(target, data=data, method="POST" if form is not None else "GET", headers={"User-Agent": UA})
    try:
        r = urllib.request.build_opener(_NoRedirect).open(req, timeout=60)
        return r.status, r.headers.get("location"), r.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, e.headers.get("location"), e.read().decode()


def social():
    """Conversations: comments, @mentions, @team mentions, review requests and reactions,
    from an agent over the API and from a person over a personal link."""
    home = {n: os.path.join(SCRATCH, f"home-social-{n}") for n in ("alice", "dana")}
    key = {n: cli("keygen", home=h) for n, h in home.items()}
    rules = (approver("alice", key["alice"], ["maintainer"]) + approver("dana", key["dana"], ["dba"])
             + '\n[seals.require]\ngovernance = { roles = ["maintainer"], count = 1 }\n')
    trunk = new_trunk("social", rules)
    a = Agent(trunk, "a00")
    # A CI edit parks for a seal, and its quorum's team is asked to review it.
    a.change("g1", edit_ci)
    wait(trunk, "g1", "awaiting_seal")
    thread = api(trunk, "GET", "comments?change=g1")
    assert [(r["reviewer"], r["by"]) for r in thread["review_requests"]] == [("@maintainer", "nerudo")], thread
    # A person's notes: the admin reads them; the agent token reads only agents' notes.
    assert any(n["kind"] == "review_request" and n["change_id"] == "g1" for n in api(trunk, "GET", "notes?who=alice", admin=True))
    r = api(trunk, "GET", "notes?who=alice")
    assert r.get("http_error") == 403, r
    # The agent mentions a person and a team; an unknown name is only text.
    c = api(trunk, "POST", "comments", {"change_id": "g1", "as": "a00",
                                        "body": "Raised the CI timeout. @alice can you seal? @dba no SQL here :eyes: @nobody"}, key=a.key)
    assert c.get("author") == "a00" and c.get("agent") is True, c
    alice, dana = api(trunk, "GET", "notes?who=alice", admin=True), api(trunk, "GET", "notes?who=dana", admin=True)
    assert any(n["kind"] == "mention" and n["from"] == "a00" for n in alice), alice
    assert any(n["kind"] == "team_mention" and n["team"] == "dba" for n in dana), dana
    assert api(trunk, "GET", "notes?who=nobody", admin=True) == []
    assert api(trunk, "GET", "notes?who=nobody").get("http_error") == 400
    bad = api(trunk, "POST", "comments", {"change_id": "g1", "as": "nobody", "body": "hi"})
    assert bad.get("http_error") == 400, bad
    assert api(trunk, "POST", "reviews", {"change_id": "g1", "reviewer": "dana", "as": "a00"}, key=a.key).get("requested") == "dana"
    # dana answers on the web with her personal link: a form post, then 303 back.
    me = api(trunk, "POST", "invite", {"user": "dana"}, admin=True)["url"]
    back = f"/t/{trunk}/change/g1"
    st, loc, _ = web(me, "act/comment", {"change": "g1", "body": "@a00 fine from the DB side :+1:", "back": back})
    assert (st, loc) == (303, back), (st, loc)
    assert any(n["from"] == "dana" and n["kind"] == "mention" for n in api(trunk, "GET", "notes?who=a00", key=a.key))
    st, _, _ = web(me, "act/react", {"change": "g1", "emoji": "🎉", "back": back})
    assert st == 303 and api(trunk, "GET", "comments?change=g1")["reactions"].get("🎉") == ["dana"]
    web(me, "act/react", {"change": "g1", "emoji": "", "other": ":tada:", "back": back})  # the same emoji: off again
    assert "🎉" not in api(trunk, "GET", "comments?change=g1")["reactions"]
    st, _, _ = web(me, "act/review", {"change": "g1", "reviewer": "@maintainer", "back": back})
    assert st == 303
    # The page shows the thread; a read-only link can't post; a forged personal link can't open.
    st, _, page = web(me, "change/g1")
    assert st == 200 and "fine from the DB side 👍" in page and 'class="mention' in page and "you are <b>dana</b>" in page, st
    share = api(trunk, "POST", "share", {}, admin=True)["url"]
    assert web(share, "act/comment", {"change": "g1", "body": "hi"})[0] == 403
    assert web(me.replace("u=dana", "u=alice"))[0] == 403
    st, _, inbox = web(api(trunk, "POST", "invite", {"user": "alice"}, admin=True)["url"], "inbox")
    assert st == 200 and "Review requests for you" in inbox and "g1" in inbox
    print(" social: ok (automatic review request to @maintainer; the agent's @alice and @dba reached alice and dana; "
          "dana commented, reacted and asked for a review from her personal link, each a 303; read-only and forged links refused)")


def branches():
    """Working branches: stale refs (landed, or ejected and replaced) are listed and deleted;
    a parked change's ref stays. Branches pushed with plain git too: merged or closed is
    stale, and a branch that moved past its PR's commit stays. Also history, a file's
    history, and the tree at a landing."""
    if HOST != "artifacts":
        print(" branches: skipped (the Forgejo stand-in's pre-receive hook refuses ref deletions)")
        return
    home = os.path.join(SCRATCH, "home-branches-alice")
    rules = approver("alice", cli("keygen", home=home), ["maintainer"]) + '\n[seals.require]\ngovernance = { roles = ["maintainer"], count = 1 }\n'
    trunk = new_trunk("branches", rules)
    a, b = Agent(trunk, "a00"), Agent(trunk, "a01")
    seed = api(trunk, "GET", "trunk")["head"]
    b.change("e1", edit_target("e1", "t01"))
    wait(trunk, "e1", "landed")
    a.change("e2", edit_target("e2", "t01"), base=seed)  # the same line as e1, from the old trunk
    wait(trunk, "e2", "ejected")
    a.change("e2", edit_target("e2", "t01"), rev=2)
    wait(trunk, "e2", "landed")
    a.change("g1", edit_ci)
    wait(trunk, "g1", "awaiting_seal")

    # Branches that a00 pushes with plain git, each built on the newest main: p1 merges; p2
    # waits for an approval and a00 closes it; p3 merges, and then a00 pushes one more
    # commit to p3 straight to its fork, so p3 has work that no revision of the PR has.
    url = f"{WORKER}/v1/trunks/{trunk}/git"

    def commit(edit, message):
        sh("git", "-c", header(a.trunk_auth), "fetch", "-q", "origin", cwd=a.dir)
        sh("git", "switch", "-q", "--detach", "origin/main", cwd=a.dir)
        edit(a.dir)
        sh("git", "add", "-A", cwd=a.dir)
        sh("git", "commit", "-q", "-m", message, cwd=a.dir)

    for branch, edit, want in (("p1", edit_target("p1", "t03"), "landed"), ("p2", edit_ci, "awaiting_seal"),
                               ("p3", edit_target("p3", "t04"), "landed")):
        commit(edit, f"{branch}: change")
        rc, _, err = git_cmd("git", "push", url, f"HEAD:refs/heads/{branch}", cwd=a.dir, user="a00", token=a.key)
        assert rc == 0 and "Opened pull request #" in err, err
        wait(trunk, branch, want, timeout=90)
    closed = api(trunk, "POST", "changes/p2/close", {"reason": "not needed"}, key=a.key)
    assert closed.get("state") == "ejected" and closed["reason"]["kind"] == "closed", closed
    edit_target("p3-more", "t04")(a.dir)
    sh("git", "add", "-A", cwd=a.dir)
    sh("git", "commit", "-q", "-m", "p3: more", cwd=a.dir)
    sh("git", "-c", header(a.fork["authorization"]), "push", "-q", a.fork["remote"], "HEAD:refs/heads/p3", cwd=a.dir)

    me = api(trunk, "POST", "invite", {"user": "alice"}, admin=True)["url"]
    st, _, page = web(me, "branches")
    assert st == 200 and page.count(">stale<") == 5 and "Delete 4 stale refs" in page and "Delete 1 stale ref" in page, page[-2000:]
    rows = {r["name"]: r for f in app_get(me, "branches")["forks"] if f["agent"] == "a00" for r in f["refs"]["ok"]}
    assert rows["refs/heads/p1"]["change"] == {"change_id": "p1", "rev": 1, "state": "landed"} and rows["refs/heads/p1"]["stale"], rows
    assert rows["refs/heads/p2"]["change"]["state"] == "ejected" and rows["refs/heads/p2"]["stale"], rows
    assert rows["refs/heads/p3"]["change"] is None and not rows["refs/heads/p3"]["stale"], rows
    assert not rows["refs/heads/main"]["stale"] and not rows["refs/heads/land/g1-1"]["stale"], rows
    out = api(trunk, "POST", "cleanup", {"as": "a00"}, key=a.key)
    assert out == {"deleted": 4}, out
    refs = sh("git", "-c", header(a.fork["authorization"]), "ls-remote", a.fork["remote"], cwd=a.dir)
    names = [l.split()[1] for l in refs.splitlines()]
    assert "refs/heads/land/g1-1" in names and not any(n.startswith("refs/heads/land/e2-") for n in names), names
    assert "refs/heads/p3" in names and "refs/heads/main" in names and "refs/heads/p1" not in names and "refs/heads/p2" not in names, names
    st, loc, _ = web(me, "act/cleanup", {"agent": "a01"})
    assert st == 303, st
    assert api(trunk, "POST", "cleanup", {"as": "a01"}, key=b.key) == {"deleted": 0}
    st, _, page = web(me, "branches")
    assert st == 200 and ">stale<" not in page
    st, _, hist = web(me, "history")
    assert st == 200 and "landing/1?" in hist and "landing/2?" in hist, hist[-800:]
    st, _, fh = web(me + "&path=targets/t01/src.txt", "history")
    assert st == 200 and fh.count("/landing/") >= 2, fh[-800:]
    st, _, tree = web(me + "&at=1", "tree/targets/t01/src.txt")
    assert st == 200 and "e1 edited" in tree and "e2 edited" not in tree, tree[-800:]
    print(" branches: ok (5 stale refs listed: landed, ejected then replaced, and plain git branches merged or closed; "
          "cleanup deleted a00's 4 over the API and a01's over the web; g1's parked ref and p3, which moved past its PR, "
          "stayed; history, a file's history and the tree at landing 1 are right)")


def mcp(trunk, agent, repo, calls, key=None):
    """Run `nerudo mcp` the way a coding agent's client does: initialize, then each tool
    call in `calls` [(name, arguments)]. Returns the text of each result, and its isError.
    `key`: the agent's key (NERUDO_AGENT_KEY); without it, the server reads its key file."""
    env = dict(ENV, NERUDO_WORKER=WORKER, NERUDO_TOKEN=DEV["NERUDO_AGENT_TOKEN"], NERUDO_REPO=repo)
    if key:
        env["NERUDO_AGENT_KEY"] = key
    lines = [{"jsonrpc": "2.0", "id": 0, "method": "initialize",
              "params": {"protocolVersion": "2025-06-18", "clientInfo": {"name": "scenario"}, "capabilities": {}}}]
    lines += [{"jsonrpc": "2.0", "id": i + 1, "method": "tools/call", "params": {"name": n, "arguments": a}}
              for i, (n, a) in enumerate(calls)]
    p = subprocess.run([CLI, "mcp", "--trunk", trunk, "--agent", agent], env=env, capture_output=True, text=True,
                       input="".join(json.dumps(l) + "\n" for l in lines), timeout=300)
    out = {m["id"]: m for m in map(json.loads, p.stdout.splitlines()) if "id" in m}
    res = []
    for i in range(1, len(calls) + 1):
        r = out.get(i, {}).get("result") or {}
        res.append(("".join(c.get("text", "") for c in r.get("content", [])), r.get("isError")))
    return res


def app_get(link, path):
    """GET the app API with a personal link's key and user."""
    q = dict(urllib.parse.parse_qsl(urllib.parse.urlparse(link).query))
    trunk = urllib.parse.urlparse(link).path.split("/")[2]
    req = urllib.request.Request(f"{WORKER}/t/{trunk}/api/{path}",
                                 headers={"X-Nerudo-Key": q["k"], "X-Nerudo-User": q.get("u", ""), "User-Agent": UA})
    return json.loads(urllib.request.urlopen(req, timeout=90).read())


def context():
    """Agent context: kept in Artifacts on the agent's fork, linked from the landed commit."""
    home = os.path.join(SCRATCH, "home-context-alice")
    trunk = new_trunk("context", approver("alice", cli("keygen", home=home), ["maintainer"]))
    fork = api(trunk, "POST", "forks", {"agent": "c1"})
    repo = os.path.join(SCRATCH, f"{trunk}-mcp")
    auth = fork.get("trunk_authorization") or BASIC
    sh("git", "-c", header(auth), "clone", "-q", fork["trunk"], repo)
    fake = "ghp_" + "Z" * 30  # a token-shaped string: it must never reach the fork
    plan = ("## Plan\nRaise t03's first line.\n\n## Decisions\n- Keep the old text in the changelog.\n\n"
            f"## Rejected\n- Editing t04 too: not needed.\n\nI used Authorization: Bearer {fake} to test, and {fake} again.\n")

    def commit(cid, t):
        edit_target(cid, t)(repo)
        sh("git", "add", "-A", cwd=repo)
        sh("git", "commit", "-q", "-m", f"{cid}: edit {t}\n\nWhy: the scenario says so.", cwd=repo)

    sh("git", "switch", "-q", "-c", "x1", "origin/main", cwd=repo)
    commit("x1", "t03")
    # c1 has a fork, so nerudo mcp needs c1's key to get it again.
    key = fork["agent_key"]
    (landed, err), (read, err2) = mcp(trunk, "c1", repo, [
        ("land", {"change_id": "x1", "context": plan, "wait_s": 60}),
        ("context", {"change_id": "x1"}),
    ], key=key)
    assert not err and "LANDED" in landed and "nerudo/context" in landed, redact(landed)
    assert key not in landed and key not in read, "the agent key reached a tool result"
    assert not err2 and "## Decisions" in read and "Keep the old text" in read and fake not in read and "[redacted]" in read, read
    rec = api(trunk, "GET", "changes/x1")
    ctx = rec["req"].get("context_commit")
    assert rec["state"] == "landed" and ctx and len(ctx) == 40, rec
    refs = sh("git", "-c", header(fork["authorization"]), "ls-remote", fork["remote"], "refs/heads/nerudo/context", cwd=repo)
    assert refs.split()[0] == ctx, refs
    # The sequencer's copy: v1 in its own repo, pinned by the landed commit, sourced from the fork.
    got = api(trunk, "GET", "context/x1")
    record = f"{trunk}--nerudo-context"
    [v1] = got["versions"]
    assert got["repo"] == record and got["immutable"] and v1["pinned"] and v1["sent"] and got["path"] == "changes/x1/rev-1/v1.md", got
    assert f"source: {fork['name']}@{ctx}" in got["text"] and "change: x1" in got["text"] and fake not in got["text"], got["text"]
    sh("git", "-c", header(auth), "fetch", "-q", "origin", cwd=repo)
    body = sh("git", "log", "-1", "--format=%B", rec["trunk_commit"], cwd=repo)
    assert f"Context: {record}@{v1['commit']}" in body and "Why: the scenario says so." in body, body
    pinned_text = got["text"]
    # A second change from the same agent: the context branch keeps both, as history.
    sh("git", "switch", "-q", "-c", "x2", "origin/main", cwd=repo)
    commit("x2", "t05")
    (landed2, err), = mcp(trunk, "c1", repo, [("land", {"change_id": "x2", "context": "## Plan\nSecond change.\n", "wait_s": 60})], key=key)
    assert not err and "LANDED" in landed2, landed2
    me = api(trunk, "POST", "invite", {"user": "alice"}, admin=True)["url"]
    hist = app_get(me, "agent/c1/context")
    assert len(hist["commits"]) == 2 and hist["commits"][1]["paths"] == ["changes/x1/rev-1.md"], hist
    one = app_get(me, f"agent/c1/context?commit={hist['commits'][0]['commit']}&path=changes/x1/rev-1.md")
    assert "Keep the old text" in one["file"]["text"], one
    assert app_get(me, "context/x2")["text"].count("Second change") == 1
    change = app_get(me, "change/x1")
    assert change["change"]["context_commit"] == ctx and change["revisions"][0]["context"] is True, change["change"]
    # A change with no context has none, and its commit no trailer; a made-up context commit is refused.
    sh("git", "switch", "-q", "-c", "x3", "origin/main", cwd=repo)
    commit("x3", "t07")
    (landed3, err), = mcp(trunk, "c1", repo, [("land", {"change_id": "x3", "wait_s": 60})], key=key)
    assert not err and "LANDED" in landed3 and "nerudo/context" not in landed3, landed3
    assert api(trunk, "GET", "context/x3")["text"] is None
    head = sh("git", "rev-parse", "HEAD", cwd=repo)
    bad = api(trunk, "POST", "land", {"change_id": "x4", "rev": 1, "fork": fork["name"], "commit": head,
                                      "base": head, "context_commit": "ab" * 20}, key=key)
    assert bad.get("http_error") == 400 and "not in fork" in bad.get("body", ""), bad
    # The agent rewrites its own context branch: the pinned copy and verify don't move.
    push_fork = lambda spec: sh("git", "-c", header(fork["authorization"]), "push", "-q", fork["remote"], spec, cwd=repo)
    empty = sh("git", "mktree", cwd=repo, inp="")
    rewritten = sh("git", "commit-tree", empty, "-m", "rewritten", cwd=repo)
    push_fork(f"+{rewritten}:refs/heads/nerudo/context")
    again = api(trunk, "GET", "context/x1")
    assert again["text"] == pinned_text and again["versions"][0]["commit"] == v1["commit"], again
    out = cli("verify", "--trunk", trunk)
    assert "Context: trailers resolve to immutable copies" in out, out[-600:]
    # Context sent again for the same revision is v2: kept, never overwriting v1, not pinned.
    newer = "---\nchange: x1\nrevision: 1\n---\n\n## Plan\nUpdated after the merge.\n"
    index = os.path.join(SCRATCH, "ctx.index")
    blob = sh("git", "hash-object", "-w", "--stdin", cwd=repo, inp=newer)
    env = dict(ENV, GIT_INDEX_FILE=index)
    for args in (["read-tree", "--empty"], ["update-index", "--add", "--cacheinfo", f"100644,{blob},changes/x1/rev-1.md"]):
        subprocess.run(["git", *args], cwd=repo, env=env, check=True, capture_output=True)
    tree = subprocess.run(["git", "write-tree"], cwd=repo, env=env, check=True, capture_output=True, text=True).stdout.strip()
    second = sh("git", "commit-tree", tree, "-p", rewritten, "-m", "context: x1 rev 1, after the merge", cwd=repo)
    push_fork(f"{second}:refs/heads/nerudo/context")
    r = api(trunk, "POST", "land", {"change_id": "x1", "rev": 1, "fork": fork["name"], "commit": rec["req"]["commit"],
                                    "base": rec["req"]["base"], "context_commit": second}, key=key)
    assert r.get("state") == "landed", r
    both = api(trunk, "GET", "context/x1")
    assert [(v["v"], v["pinned"], v["sent"]) for v in both["versions"]] == [(1, True, True), (2, False, False)], both["versions"]
    assert both["v"] == 1 and both["text"] == pinned_text, both
    v2 = api(trunk, "GET", "context/x1?v=2")
    assert "Updated after the merge." in v2["text"] and "version: v2" in v2["text"] and f"@{second}" in v2["text"], v2
    assert "Context: trailers resolve to immutable copies" in cli("verify", "--trunk", trunk)
    print(f" context: ok (x1 landed through nerudo mcp with context; the token in it was redacted; the sequencer copied "
          f"it to {record} as v1, and the landed commit's Context: trailer pins that copy; the agent force-pushed its own "
          "branch and the copy and `nerudo verify` held; context sent again became v2, not pinned; the MCP tool and the "
          "app API read it; no context, no trailer; a made-up context commit was refused)")


def wait_done(trunk, cid, timeout=60):
    """Wait until the change lands or is ejected; return its receipt."""
    end = time.time() + timeout
    while time.time() < end:
        st = api(trunk, "GET", f"changes/{cid}?wait=5")
        if st.get("state") in ("landed", "ejected"):
            return st
    raise AssertionError(f"{cid}: still {st.get('state')} ({st.get('reason')})")


def app_api(trunk, link, method, path, body=None):
    """The app's JSON API, as the holder of this share or personal link."""
    q = urllib.parse.parse_qs(link.split("?", 1)[1])
    headers = {"Content-Type": "application/json", "User-Agent": UA, "X-Nerudo-Key": q["k"][0]}
    if q.get("u"):
        headers["X-Nerudo-User"] = q["u"][0]
    req = urllib.request.Request(f"{WORKER}/t/{trunk}/api/{path}", method=method,
                                 data=json.dumps(body).encode() if body is not None else None, headers=headers)
    try:
        return json.loads(urllib.request.urlopen(req, timeout=90).read())
    except urllib.error.HTTPError as e:
        return {"http_error": e.code, "body": e.read().decode()}


def compare():
    """Competing attempts for one task, compared side by side; one ships."""
    home = os.path.join(SCRATCH, "home-compare-alice")
    rules = '[ci]\ntypecheck = { tsconfig = "tsconfig.json" }\n' + approver("alice", cli("keygen", home=home), ["maintainer"])
    trunk = new_trunk("compare", rules, files={
        "tsconfig.json": TSCONFIG, "src/price.ts": PRICE_1,
        "src/index.ts": "import { price } from './price';\nexport const total = price(250);\n"})
    me = api(trunk, "POST", "invite", {"user": "alice"}, admin=True)["url"]
    task = app_api(trunk, me, "POST", "tasks", {"title": "Format prices with two decimals", "attempts": 3, "status": "todo"})
    assert task.get("attempts") == 3 and task.get("id"), task
    tid = task["id"]
    names = ["a00", "a01", "a02", "a03"]
    agents = [Agent(trunk, n) for n in names]
    for ag in agents[:3]:
        assert api(trunk, "POST", f"tasks/{tid}/claim", {"as": ag.name}, key=ag.key).get("id") == tid
    full = api(trunk, "POST", f"tasks/{tid}/claim", {"as": "a03"}, key=agents[3].key)
    assert full.get("http_error") == 409 and "attempts" in full["body"], full
    small = "export const format = (n: number): string => n.toFixed(2);\n"
    big = ("export function format(n: number, currency = 'EUR'): string {\n"
           "  const fixed = n.toFixed(2);\n  return `${fixed} ${currency}`;\n}\n")
    broken = "export const format = (n: number): string => n.toFixed('2');\n"
    # One attempt lands through `nerudo mcp` with its plan as context: Compare shows the plan.
    a0 = agents[0]
    sh("git", "switch", "-q", "-c", "small", "origin/main", cwd=a0.dir)
    ts_file("src/format.ts", small)(a0.dir)
    sh("git", "add", "-A", cwd=a0.dir)
    sh("git", "commit", "-q", "-m", "small: format with toFixed(2)", cwd=a0.dir)
    plan = ("## Plan\nformat() is toFixed(2), in one line.\n\n## Decisions\n- No currency: receipts print it already.\n\n"
            "## Rejected\n- Intl.NumberFormat: more than receipts need.\n")
    (sent, err), = mcp(trunk, "a00", a0.dir, [("land", {"change_id": "small", "task": tid, "context": plan, "wait_s": 60})], key=a0.key)
    assert not err and "WAITING FOR A PICK" in sent, sent
    for ag, cid, text in zip(agents[1:3], ("big", "broken"), (big, broken)):
        ag.change(cid, ts_file("src/format.ts", text), task=tid)
    trials = {cid: wait(trunk, cid, "awaiting_pick", timeout=60) for cid in ("small", "big", "broken")}
    assert all(t["reason"]["kind"] == "awaiting_pick" for t in trials.values()), trials
    cmp = app_api(trunk, me, "GET", f"task/{tid}/compare")
    by = {x["change_id"]: x for x in cmp["attempts"]}
    assert [by[c]["trial"]["passed"] for c in ("small", "big", "broken")] == [True, True, False], cmp
    assert by["broken"]["trial"]["ci"]["diagnostics"][0]["code"] == "TS2345", by["broken"]["trial"]
    assert by["small"]["files"][0]["path"] == "src/format.ts" and by["big"]["trial"]["adds"] > by["small"]["trial"]["adds"], by
    # Each attempt names its context: the agent's commit, and the version of the sequencer's copy.
    v = by["small"]["context_version"]
    assert len(by["small"]["context_commit"] or "") == 40 and v == (1 if HOST == "artifacts" else None), by["small"]
    assert by["big"]["context_commit"] is None and by["big"]["context_version"] is None, by["big"]
    ctx = app_api(trunk, me, "GET", f"context/small?rev=1" + (f"&v={v}" if v else ""))
    assert "format() is toFixed(2)" in ctx["text"] and "Intl.NumberFormat" in ctx["text"], ctx
    assert api(trunk, "GET", "stats")["landed"] == 0, "an attempt merged before a pick"
    # A read-only link can't pick, and an attempt that failed its checks can't be picked.
    share = api(trunk, "POST", "share", {}, admin=True)["url"]
    refused = app_api(trunk, share, "POST", f"task/{tid}/pick", {"change": "small"})
    assert refused.get("http_error") == 403, refused
    bad = app_api(trunk, me, "POST", f"task/{tid}/pick", {"change": "broken"})
    assert bad.get("http_error") == 400 and "failed its checks" in bad["body"], bad
    picked = app_api(trunk, me, "POST", f"task/{tid}/pick", {"change": "small"})
    assert picked.get("picked") == "small" and sorted(picked["closed"]) == ["big", "broken"], picked
    st = wait(trunk, "small", "landed", timeout=60)
    for cid in ("big", "broken"):
        rec = api(trunk, "GET", f"changes/{cid}")
        assert rec["state"] == "ejected" and rec["reason"]["kind"] == "not_picked", rec
    done = app_api(trunk, me, "GET", f"task/{tid}")["task"]
    assert done["status"] == "done" and done["picked"] == "small" and done["picked_by"] == "alice", done
    assert app_api(trunk, me, "POST", f"task/{tid}/pick", {"change": "big"}).get("http_error") == 400
    refs = {f["agent"]: f["refs"]["ok"] for f in app_api(trunk, me, "GET", "branches")["forks"]}
    assert any(r["name"].endswith("land/big-1") and r["stale"] for r in refs["a01"]), refs["a01"]
    # The "smallest" policy picks by itself once every attempt is in.
    auto = app_api(trunk, me, "POST", "tasks", {"title": "Round prices", "attempts": 2, "pick": "smallest", "status": "todo"})
    for ag in agents[:2]:
        api(trunk, "POST", f"tasks/{auto['id']}/claim", {"as": ag.name}, key=ag.key)
    agents[0].change("round-long", ts_file("src/round.ts", "export function round(n: number): number {\n"
                                                       "  const scaled = n * 100;\n  return Math.round(scaled) / 100;\n}\n"),
                     task=auto["id"])
    wait(trunk, "round-long", "awaiting_pick", timeout=60)
    agents[1].change("round-short", ts_file("src/round.ts", "export const round = (n: number) => Math.round(n * 100) / 100;\n"),
                     task=auto["id"])
    wait(trunk, "round-short", "landed", timeout=60)
    lost = api(trunk, "GET", "changes/round-long")
    assert lost["state"] == "ejected" and lost["reason"]["kind"] == "not_picked", lost
    t2 = app_api(trunk, me, "GET", f"task/{auto['id']}")["task"]
    assert t2["picked_by"] == "nerudo (smallest)", t2
    stats = api(trunk, "GET", "stats")
    assert stats["not_picked"] == 3 and stats["landed"] == 2, stats
    print(f" compare: ok (3 attempts on {tid}: trials passed, passed, failed TS2345; a 4th claim refused; nothing merged "
          f"before the pick; a read-only link and the failed attempt could not pick; alice picked small, it merged as seq "
          f"{st['seq']}, big and broken closed as not picked and big's branch is stale; small's plan was beside its trial "
          f"(context v{v}); the smallest policy shipped round-short)")


def fencing():
    if HOST != "artifacts":
        print(" fencing: skipped (Forgejo forks use the bench login, which isn't scoped)")
        return
    trunk = new_trunk("fencing")
    a, b = Agent(trunk, "a00"), Agent(trunk, "a01")
    sh("git", "commit", "-q", "--allow-empty", "-m", "try to write trunk directly", cwd=a.dir)

    def push(auth, url):
        p = subprocess.run(["git", "-c", header(auth), "push", "-q", url, "HEAD:refs/heads/main"],
                           cwd=a.dir, env=ENV, capture_output=True, text=True, timeout=60)
        return p.returncode == 0
    head = api(trunk, "GET", "stats")["head"]
    assert not push(a.trunk_auth, a.fork["trunk"]), "trunk read token pushed to trunk"
    assert not push(a.fork["authorization"], a.fork["trunk"]), "fork token pushed to trunk"
    assert not push(a.fork["authorization"], b.fork["remote"]), "fork token pushed to another agent's fork"
    assert push(a.fork["authorization"], a.fork["remote"]), "fork token couldn't push to its own fork"
    assert api(trunk, "GET", "stats")["head"] == head, "trunk moved"
    # Squat the fork name agent a02 would get. The object must never hand out a token for a
    # repo it didn't make: it skips the taken name and makes a fresh fork.
    squat = f"{trunk}--a02"
    p = subprocess.run(["npx", "--yes", "wrangler@4.148.0", "artifacts", "repos", "create", squat, "--namespace", NAMESPACE],
                       cwd=os.path.join(ROOT, "cf", "nerudo-worker"), capture_output=True, text=True, timeout=120)
    assert p.returncode == 0, redact(p.stderr[-300:])
    r = api(trunk, "POST", "forks", {"agent": "a02"})
    assert r.get("name") and r["name"] != squat and not r["remote"].endswith(f"/{squat}.git"), redact(json.dumps(r))
    assert api(trunk, "POST", "forks", {"agent": "a-02"}).get("http_error") == 400, "agent name with '-' accepted"
    # A fork token comes again only with the agent's own key: never with the shared agent
    # token alone, and never with another agent's key.
    again = api(trunk, "POST", "forks", {"agent": "a00"}, key=a.key)
    assert again.get("remote") == a.fork["remote"], redact(json.dumps(again))
    assert api(trunk, "POST", "forks", {"agent": "a00"}).get("http_error") == 403, "the agent token alone got a00's fork token"
    assert api(trunk, "POST", "forks", {"agent": "a00"}, key=b.key).get("http_error") == 403, "a01's key got a00's fork token"
    print(f" fencing: ok (agent tokens can't write trunk or other forks; squatted {squat} skipped for {r['name']};"
          " re-asking for your own fork needs your key: the agent token alone or another agent's key gets 403)")


def decide():
    """A decision model in the gate ([decide], provider "fixed" so the run is repeatable): it
    asks for a review that no path rule asks for, the receipt keeps the probabilities and the
    threshold, one seal lands the change, and `nerudo verify` checks that seal. On a trunk
    where the model is sure a change is routine, nothing is added."""
    home = os.path.join(SCRATCH, "home-decide-you")
    pk = cli("keygen", home=home)
    review = '\n[decide]\nprovider = "fixed"\nfixed = { merge = 0.10, review = 0.87, sign = 0.03 }\n'
    trunk = new_trunk("decide", approver("you", pk, ["maintainer"]) + review)
    a = Agent(trunk, "a00")
    a.change("d1", edit_target("d1", "t01"))
    st = wait(trunk, "d1", "awaiting_seal")
    want = "review: decision model chose review, p 0.87 >= 0.80 (fixed/fixed)"
    assert st["seal_reasons"] == [want], st["seal_reasons"]
    d = st["decisions"][0]
    assert (d["point"], d["outcome"], d["basis"], d["threshold"], d["probabilities"]["review"]) == ("risk", "review", "chose", 0.8, 0.87), d
    print(" d1 parked:", st["seal_reasons"][0])
    cli("seal", "--trunk", trunk, "--change", "d1", "--yes", home=home)
    st = wait(trunk, "d1", "landed")
    assert st["decisions"][0]["outcome"] == "review" and st["sealed_by"] == ["you"], st
    print(" d1 landed after one seal, seq", st["seq"], "; the receipt keeps the decision")
    print(" verify:", cli("verify", "--trunk", trunk).splitlines()[-1].strip())

    sure = '\n[decide]\nprovider = "fixed"\nfixed = { merge = 0.95, review = 0.04, sign = 0.01 }\n'
    trunk = new_trunk("decide-merge", approver("you", pk, ["maintainer"]) + sure)
    Agent(trunk, "a00").change("m1", edit_target("m1", "t02"))
    st = wait_done(trunk, "m1")
    assert st["state"] == "landed" and not st["seal_reasons"] and st["decisions"][0]["outcome"] == "merge", st
    print(" decide: ok (review asked and sealed; a confident merge added nothing)")


def clef():
    """[decide] with provider "clef": Cloudflare's decision model on the AI binding, once as
    the default model and once as clef-flash. Not in the default run: it calls the real model,
    which bills per use and does not answer the same way each time. Each receipt keeps a real
    answer (never an error) with calibrated probabilities. A PR that skips an auth check and
    puts a token in code is never merged without a person; a one-line text edit goes where
    the thresholds send it."""
    def risky(d):
        os.makedirs(os.path.join(d, "app"), exist_ok=True)
        open(os.path.join(d, "app", "auth.py"), "w").write(
            # A made-up value: the check is that the context copy redacts it (nothing real).
            'ADMIN_TOKEN = "f3c9a1d07b2e4c58a9e1d6b3c7f20a4e"\n\n\n'
            "def is_admin(request):\n"
            "    # The check is off until the demo is over.\n"
            "    return True\n")

    def settled(trunk, cid, timeout=60):
        end = time.time() + timeout
        while time.time() < end:
            st = api(trunk, "GET", f"changes/{cid}?wait=5")
            if st.get("state") in ("landed", "ejected", "awaiting_seal"):
                return st
        raise AssertionError(f"{cid}: still {st.get('state')} ({st.get('reason')})")

    for model in ["", "clef-flash"]:
        cfg = f'\n[decide]\nprovider = "clef"\n' + (f'model = "{model}"\n' if model else "")
        trunk = new_trunk("clef" + (f"-{model}" if model else ""), cfg)
        a = Agent(trunk, "a00")
        for cid, edit in [("risky", risky), ("routine", edit_target("routine", "t01"))]:
            t0 = time.time()
            a.change(cid, edit)
            st = settled(trunk, cid)
            d = st["decisions"][0]
            assert d["provider"] == "clef" and d["basis"] in ("chose", "unsure"), d
            assert d["calibrated"] and abs(sum(d["probabilities"].values()) - 1) < 0.01, d
            p = " ".join(f"{k} {v:.2f}" for k, v in d["probabilities"].items())
            print(f" {model or 'clef'} {cid}: {d['outcome']} ({d['basis']}; {p}; confidence {d['confidence']:.2f};"
                  f" model {d['model']}) -> {st['state']} in {time.time() - t0:.1f} s")
            if cid == "risky":
                assert d["outcome"] != "merge" and st["state"] == "awaiting_seal", st
    print(" clef: ok (real answers on every receipt; the risky PR waits for a person)")


def release():
    home = os.path.join(SCRATCH, "home-release-you")
    other = os.path.join(SCRATCH, "home-release-mallory")
    pk = cli("keygen", home=home)
    cli("keygen", home=other)
    trunk = new_trunk("release", approver("you", pk, ["maintainer"]))
    a = Agent(trunk, "a00")
    for cid, t in (("c1", "t01"), ("c2", "t02")):
        a.change(cid, edit_target(cid, t))
        wait(trunk, cid, "landed")
    d = api(trunk, "GET", "releases/draft")
    assert d["tag"] == "v0.1.0" and d["seq"] == 2 and d["stats"]["prs"] == 2 and d["previous"] is None, d
    print(" draft:", d["tag"], "at landing", d["seq"], f"({d['stats']['prs']} PRs, {d['files']['files']} files)")

    # An agent proposes it; the approver finds it in their inbox.
    assert api(trunk, "POST", "releases/propose", {"by": "a00"}).get("http_error") == 403, "the agent token proposed as a00"
    p = api(trunk, "POST", "releases/propose", {"by": "a00"}, key=a.key)
    assert p.get("proposed") == "v0.1.0" and p.get("notified") == ["you"], p
    print(" a00 proposed it; notified:", p["notified"])

    spec = {k: d[k] for k in ("tag", "commit", "seq", "title", "notes_digest", "previous", "rules_digest")}
    spec.update(trunk=trunk, prerelease=False, expires_at_ms=int(time.time() * 1000) + 600_000, approver=pk)
    forged = api(trunk, "POST", "releases", {"spec": spec, "signature": "00" * 64, "notes": d["notes"]})
    assert forged.get("http_error") == 403 and "signature" in forged["body"], forged
    edited = api(trunk, "POST", "releases", {"spec": spec, "signature": "00" * 64, "notes": d["notes"] + "\nmore"})
    assert edited.get("http_error") == 400 and "digest" in edited["body"], edited
    try:
        cli("release", "--trunk", trunk, "--yes", home=other)
        raise AssertionError("a key outside nerudo.toml released")
    except RuntimeError as e:
        assert "not an approver" in str(e), e
    print(" refused: a forged signature (403), edited notes (400), a key outside nerudo.toml (403)")

    out = cli("release", "--trunk", trunk, "--yes", home=home)
    assert "released v0.1.0" in out, out
    print(" " + out.splitlines()[-1])
    refs = sh("git", "-c", header(a.trunk_auth), "ls-remote", a.fork["trunk"], "refs/tags/*")
    tag_oid = next(l.split()[0] for l in refs.splitlines() if l.endswith("refs/tags/v0.1.0"))
    rels = api(trunk, "GET", "releases")
    assert rels["releases"][0]["tag"] == "v0.1.0" and rels["releases"][0]["tag_object"] == tag_oid and rels["proposals"] == [], rels
    again = api(trunk, "POST", "releases", {"spec": spec, "signature": "00" * 64, "notes": d["notes"]})
    assert again.get("http_error") == 409, again
    print(" refs/tags/v0.1.0 is on Artifacts:", tag_oid[:12], "· the proposal is cleared · v0.1.0 can't be released twice")

    a.change("c3", edit_target("c3", "t03"))
    wait(trunk, "c3", "landed")
    d2 = api(trunk, "GET", "releases/draft")
    assert d2["tag"] == "v0.1.1" and d2["previous"] == "v0.1.0" and d2["stats"]["prs"] == 1, d2
    v = cli("verify", "--trunk", trunk)
    assert "1 release tag" in v and "v0.1.0" in v, v
    print(" next draft:", d2["tag"], "· nerudo verify:", next(l.strip() for l in v.splitlines() if "release tag" in l))


def worker(method, path, body=None, token="admin", key=None):
    """Any path of the Worker, as the admin, an agent ("agent") or nobody (None). Returns the
    JSON, {"status", "text"} for another type, or {"http_error", "body"}. `key`: an agent's own key."""
    headers = {"Content-Type": "application/json", "User-Agent": UA}
    if token:
        headers["Authorization"] = "Bearer " + DEV["NERUDO_ADMIN_TOKEN" if token == "admin" else "NERUDO_AGENT_TOKEN"]
    if key:
        headers["X-Nerudo-Agent-Key"] = key
    req = urllib.request.Request(f"{WORKER}{path}", method=method, headers=headers,
                                 data=json.dumps(body).encode() if body is not None else None)
    try:
        r = urllib.request.urlopen(req, timeout=90)
        text = r.read().decode()
        return json.loads(text) if "json" in (r.headers.get("content-type") or "") else {"status": r.status, "text": text}
    except urllib.error.HTTPError as e:
        return {"http_error": e.code, "body": e.read().decode()}


def hub():
    """Names: a trunk is a repo ORG/REPO on the Hub, public or private."""
    pk = cli("keygen", home=os.path.join(SCRATCH, "home-hub-you"))
    trunk = new_trunk("hub", approver("you", pk, ["maintainer"]))
    org = f"scn-{time.strftime('%H%M%S')}"
    name = f"{org}/widgets"

    # Refusals: reserved and bad names, a trunk that does not exist, no admin token.
    for path, want in (("/v1/hub/repos/v1/widgets", "reserved"), ("/v1/hub/repos/Bad_Org/widgets", "org or user name"),
                       (f"/v1/hub/repos/{org}/widgets.git", "repo name")):
        r = worker("PUT", path, {"trunk": trunk})
        assert r.get("http_error") == 400 and want in r["body"], (path, r)
    r = worker("PUT", f"/v1/hub/repos/{name}", {"trunk": "scn-no-such-trunk"})
    assert r.get("http_error") == 404, r
    for tok in ("agent", None):
        assert worker("PUT", f"/v1/hub/repos/{name}", {"trunk": trunk}, token=tok).get("http_error") == 401
        assert worker("PUT", f"/v1/hub/orgs/{org}", {"title": "x"}, token=tok).get("http_error") == 401
    print(" refused: a reserved name, a bad org, a repo ending in .git (400), an unknown trunk (404), no admin token (401)")

    # Name it with the CLI; the org comes with it.
    out = cli("repo", "--trunk", trunk, "--as", name, "--description", "Widgets, for the hub scenario", admin=True)
    link = out.splitlines()[-1]
    assert f"is trunk {trunk} (public)" in out and link.endswith(f"/{name}"), out
    cli("org", "--org", org, "--title", "Scenario org", admin=True)
    a = Agent(trunk, "a00")
    a.change("c1", edit_target("c1", "t01"))
    wait(trunk, "c1", "landed")
    print(" named", name, "->", trunk, "· link", link)

    # Read by name, with no key.
    o = worker("GET", f"/v1/hub/orgs/{org}", token=None)
    assert o["title"] == "Scenario org" and [r["repo"] for r in o["repos"]] == ["widgets"], o
    s = o["repos"][0]["summary"]
    assert s["trunk"] == trunk and s["public"] and s["stats"]["merged"] == 1 and s["agents"] == ["a00"], s
    assert s["people"] == {"you": ["maintainer"]} and s["updated_ms"], s
    assert o["teams"] == [{"team": "maintainer", "members": ["you"], "repos": ["widgets"]}], o["teams"]
    assert o["people"] == [{"name": "you", "roles": ["maintainer"], "repos": ["widgets"], "user": False, "joined": True}], o["people"]
    assert o["agents"] == [{"name": "a00", "repos": ["widgets"]}], o["agents"]
    orgs = worker("GET", "/v1/hub/orgs", token=None)["orgs"]
    assert {"org": org, "title": "Scenario org", "description": "", "repos": 1} in orgs, orgs
    r = worker("GET", f"/v1/hub/repos/{name}", token=None)
    assert r["trunk"] == trunk and r["description"] == "Widgets, for the hub scenario" and r["summary"]["head"], r
    assert worker("GET", f"/v1/hub/trunks/{trunk}", token=None)["repo"] == "widgets"
    st = worker("GET", f"/t/{trunk}/api/state", token=None)
    assert st["trunk"] == trunk and st["me"] is None, st
    assert worker("GET", f"/t/{trunk}", token=None).get("status") == 200
    acted = worker("POST", f"/t/{trunk}/api/comment", {"thread": "c1", "body": "hi"}, token=None)
    assert acted.get("http_error") == 403, acted
    print(" public, with no key: the org (teams, people, agents), the repo's summary (1 merged), the reverse")
    print("   lookup, the app API and the pages; acting needs a personal link (403)")

    # The trunk API and git, by name.
    assert worker("GET", f"/v1/repos/{name}/stats", token="agent")["landed"] == 1
    c = worker("POST", f"/v1/repos/{name}/comments", {"change_id": "c1", "body": "by name", "as": "a00"}, token="agent", key=a.key)
    assert c.get("body") == "by name", c
    assert worker("GET", f"/v1/repos/{org}/nope/stats", token="agent").get("http_error") == 404
    refs = worker("GET", f"/{name}.git/info/refs?service=git-upload-pack", token=None)
    assert refs.get("body") != "repository not found\n", refs
    missing = worker("GET", f"/{org}/nope.git/info/refs?service=git-upload-pack", token=None)
    assert missing == {"http_error": 404, "body": "repository not found\n"}, missing
    assert json.loads(cli("status", "--trunk", name))["landed"] == 1
    print(" by name: GET /v1/repos/{org}/{repo}/stats, a POST through it, `nerudo status --trunk ORG/REPO`;")
    print("   git reaches the trunk (HTTP", refs.get("http_error") or refs.get("status"), "there); an unknown repo is 404")

    # Private: hidden without the admin token. The share link still reads; links use the trunk id.
    out = cli("repo", "--trunk", name, "--as", name, "--private", admin=True)
    assert "(private)" in out and f"/{trunk}?k=" in out.splitlines()[-1], redact(out)
    assert worker("GET", f"/t/{trunk}/api/state", token=None).get("http_error") == 403
    assert worker("GET", f"/v1/hub/repos/{name}", token=None).get("http_error") == 404
    assert worker("GET", f"/v1/hub/orgs/{org}", token=None).get("http_error") == 404
    assert worker("GET", f"/v1/hub/repos/{name}")["public"] is False
    share = api(trunk, "POST", "share", {}, admin=True)
    assert app_api(trunk, share["url"], "GET", "state")["trunk"] == trunk
    print(" private: 404 by name and 403 with no key; the admin sees it; the share link reads; links use the trunk id")

    # The name moves to a new trunk: the old trunk loses it and is private. Then it goes.
    trunk2 = new_trunk("hub2", approver("you", pk, ["maintainer"]))
    moved = worker("PUT", f"/v1/hub/repos/{name}", {"trunk": trunk2, "public": True})
    assert moved["trunk"] == trunk2 and moved["summary"]["trunk"] == trunk2, moved
    assert worker("GET", f"/v1/hub/trunks/{trunk}").get("http_error") == 404
    assert json.loads(cli("status", "--trunk", name))["landed"] == 0
    gone = worker("DELETE", f"/v1/hub/repos/{name}")
    assert gone == {"deleted": name, "trunk": trunk2}, gone
    assert worker("GET", f"/v1/hub/repos/{name}").get("http_error") == 404
    assert worker("GET", f"/t/{trunk2}/api/state", token=None).get("http_error") == 403
    print(f" moved to {trunk2} (the old trunk has no name), then deleted (the trunk stays, private)")


RAW_HELPER = """!f() { test "$1" = get && printf 'username=%s\\npassword=%s\\n' "$NERUDO_GIT_USER" "$NERUDO_GIT_TOKEN"; }; f"""


def git_cmd(*cmd, cwd=None, user=None, token=None, raw=False):
    """Run the real git CLI against the sequencer. With `user`, `nerudo git-credential`
    answers git with that name and a password made from `token` (an agent's key, never the
    agent token), from the environment, never from argv; it finds the trunk in git's path
    (credential.useHttpPath). `raw`: a plain helper gives git `token` itself, to test what
    the sequencer takes."""
    env = {k: v for k, v in ENV.items() if not k.startswith("NERUDO_")}
    env["NERUDO_WORKER"] = WORKER
    if user:
        env.update(GIT_CONFIG_COUNT="4", GIT_CONFIG_KEY_2="credential.helper",
                   GIT_CONFIG_VALUE_2=RAW_HELPER if raw else f"!{CLI} git-credential",
                   GIT_CONFIG_KEY_3="credential.useHttpPath", GIT_CONFIG_VALUE_3="true",
                   NERUDO_GIT_USER=user, NERUDO_GIT_TOKEN=token)
    p = subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, text=True, timeout=300)
    return p.returncode, redact(p.stdout), redact(p.stderr)


def git():
    trunk = new_trunk("git", approver("you", cli("keygen", home=os.path.join(SCRATCH, "home-git-you")), ["maintainer"]))
    url = f"{WORKER}/v1/trunks/{trunk}/git"
    d = os.path.join(SCRATCH, f"{trunk}-git")
    token = DEV["NERUDO_AGENT_TOKEN"]
    invite = api(trunk, "POST", "invite", {"user": "you"}, admin=True)["url"]
    your_key = dict(urllib.parse.parse_qsl(urllib.parse.urlparse(invite).query))["k"]

    rc, _, err = git_cmd("git", "clone", "-q", url, d + "-anon")
    assert rc and ("Username" in err or "401" in err), err
    rc, _, err = git_cmd("git", "clone", "-q", url, d + "-wrong", user="a07", token="not-the-token")
    assert rc and "Authentication failed" in err, err
    rc, _, err = git_cmd("git", "ls-remote", url, user="you", token=token)
    assert rc and "is a person" in err, err
    rc, _, err = git_cmd("git", "ls-remote", url, user="you", token=token, raw=True)
    assert rc and "Authentication failed" in err, err
    print(" refused: an anonymous clone of a private repo, a wrong password, a person with the agent token (the helper "
          "gives git nothing; the sequencer answers 401)")

    # The helper makes a07's fork with the agent token, keeps a07's key, and gives git the key.
    rc, _, err = git_cmd("git", "clone", "-q", url, d, user="a07", token=token)
    assert rc == 0, err
    seed = sh("git", "rev-parse", "origin/main", cwd=d)
    kept = os.path.join(SCRATCH, ".nerudo", "agents", trunk, "a07")
    key = open(kept).read().strip()
    assert key.startswith("lsa1.a07.0.") and os.stat(kept).st_mode & 0o777 == 0o600, oct(os.stat(kept).st_mode)
    print(" cloned with nerudo git-credential: main at", seed[:7] + "; it made a07's fork and kept a07's key (0600)")

    task = api(trunk, "POST", "tasks", {"as": "a07", "title": "Say hello in t05"}, key=key)["id"]
    branch = f"{task.lower()}/hello"
    cid = branch.replace("/", "-")

    def edit(path, line, text):
        p = os.path.join(d, path)
        lines = open(p).readlines()
        lines[line] = text + "\n"
        open(p, "w").writelines(lines)

    sh("git", "switch", "-q", "-c", branch, cwd=d)
    edit("targets/t05/src.txt", 0, "hello from plain git")
    sh("git", "commit", "-q", "-am", "t05: say hello", cwd=d)
    rc, _, err = git_cmd("git", "push", "origin", branch, cwd=d, user="a07", token=token)
    assert rc == 0 and "Opened pull request #" in err and f"linked to task {task}" in err, err
    print(" git push origin " + branch + "\n" + "\n".join("   " + l for l in err.strip().splitlines()))
    st = wait(trunk, cid, "landed", timeout=90)
    assert st["req"]["rev"] == 1 and st["req"]["task"] == task and st["req"]["branch"] == branch and st["req"]["base"] == seed, st
    number = st["number"]
    print(f" #{number} {cid}: landed as seq {st['seq']}, linked to {task}")

    # A second commit on the merged branch, to the same file. A merged PR takes no new commits,
    # so the push opens a new PR, change id <branch id>..2. Its base is the merged commit of
    # the first PR (on main as the commit it landed as), so only the new line merges.
    edit("targets/t05/src.txt", 1, "and hello again")
    sh("git", "commit", "-q", "-am", "t05: say hello again", cwd=d)
    rc, _, err = git_cmd("git", "push", "origin", branch, cwd=d, user="a07", token=token)
    assert rc == 0 and "Opened pull request #" in err and f"Pull request #{number} for {branch} is merged" in err, err
    print(" git push origin " + branch + " (merged)\n" + "\n".join("   " + l for l in err.strip().splitlines()))
    rev1 = st["req"]["commit"]
    st = wait(trunk, cid + "..2", "landed", timeout=90)
    assert st["req"]["rev"] == 1 and st["number"] != number and st["req"]["base"] == rev1 and st["size"]["files"] == 1, st
    assert st["req"]["branch"] == branch and api(trunk, "GET", f"changes/{cid}")["req"]["rev"] == 1, st
    second = st["number"]
    print(f" #{second} {cid}..2: a new PR for the merged branch, based on #{number}'s commit, landed as seq {st['seq']}")
    rows = {r["name"]: r for f in app_get(invite, "branches")["forks"] if f["agent"] == "a07" for r in f["refs"]["ok"]}
    assert rows[f"refs/heads/{branch}"] == {"name": f"refs/heads/{branch}", "oid": st["req"]["commit"], "stale": True,
                                            "change": {"change_id": cid + "..2", "rev": 1, "state": "landed"}}, rows
    print(f" the branches view: {branch} carries #{second}, merged, so it is stale")

    rc, _, err = git_cmd("git", "push", "origin", "HEAD:main", cwd=d, user="a07", token=token)
    assert rc and "[remote rejected]" in err and "main moves only through the merge queue" in err, err
    print(" git push origin HEAD:main\n" + "\n".join("   " + l for l in err.strip().splitlines()))
    rc, _, err = git_cmd("git", "push", "origin", "HEAD:refs/tags/v9.9.9", cwd=d, user="a07", token=token)
    assert rc and "releases are tags that the sequencer signs" in err, err
    print(" refused: a push to main and a tag push, with the lines that say why")

    rc, _, err = git_cmd("git", "push", "origin", "--delete", branch, cwd=d, user="a07", token=token)
    assert rc == 0 and f"Pull request #{second} is unchanged" in err, err
    print(f" deleted {branch} on the fork; #{second} is unchanged")

    # A person, with their personal key, pushes a branch built on the seed, two merges
    # behind main: the walk finds the seed as its base, so the PR merges only its own file.
    sh("git", "switch", "-q", "-c", "old-base", seed, cwd=d)
    edit("targets/t09/src.txt", 3, "from an old main")
    sh("git", "commit", "-q", "-am", "t09: from an old main", cwd=d)
    rc, _, err = git_cmd("git", "push", "origin", "old-base", cwd=d, user="you", token=your_key)
    assert rc == 0 and "Opened pull request #" in err, err
    st = wait(trunk, "old-base", "landed", timeout=90)
    assert st["req"]["base"] == seed and st["req"]["fork"].endswith("--you") and st["size"]["files"] == 1, st
    rc, _, err = git_cmd("git", "fetch", "-q", "origin", cwd=d, user="you", token=your_key)
    assert rc == 0, err
    changed = sh("git", "diff", "--name-only", "origin/main~1", "origin/main", cwd=d).split()
    t05 = sh("git", "show", "origin/main:targets/t05/src.txt", cwd=d).splitlines()[:2]
    assert changed == ["targets/t09/src.txt"] and t05 == ["hello from plain git", "and hello again"], (changed, t05)
    print(" old-base (you, personal key): built on the seed, base found by the walk; landed changing only", changed[0])
    print(" git: ok")


def users():
    """A user: one name and one sign-in key on every repo that lists them, and repos of their own."""
    stamp = time.strftime("%H%M%S")
    user, org = f"scn-u{stamp}", f"scn-uo{stamp}"
    home = os.path.join(SCRATCH, f"home-users-{stamp}")
    pk = cli("keygen", home=home)
    other = cli("keygen", home=os.path.join(SCRATCH, f"home-users-other-{stamp}"))
    one = new_trunk("users1", approver(user, pk, ["maintainer"]))
    two = new_trunk("users2", approver(user, pk, ["reviewer"]))
    three = new_trunk("users3", approver(user, other, ["maintainer"]))  # the same name, another key
    four = new_trunk("users4", approver("old-name", pk, ["maintainer"]))  # the key, under another name

    out = cli("user", "--name", user, "--title", "Scenario user", "--key", "mine", home=home, admin=True)
    link = out.splitlines()[-1]
    q = dict(urllib.parse.parse_qsl(urllib.parse.urlparse(link).query))
    assert f"user {user}, key {pk[:12]}" in out and "/login?" in link and q["u"] == user and len(q["k"]) == 32, redact(out)
    key = q["k"]
    for name, trunk in ((f"{org}/one", one), (f"{user}/two", two), (f"{org}/three", three), (f"{org}/four", four)):
        cli("repo", "--trunk", trunk, "--as", name, admin=True)
    print(f" user {user} (its key is this machine's); repos {org}/one, three, four and {user}/two")

    # One set of names for orgs and users. A user's repo makes no org.
    assert worker("PUT", f"/v1/hub/orgs/{user}", {"title": "x"}).get("http_error") == 409
    assert worker("PUT", f"/v1/hub/users/{org}", {}).get("http_error") == 409
    assert worker("PUT", f"/v1/hub/users/{user}", {"key": "not-hex"}).get("http_error") == 400
    assert worker("PUT", f"/v1/hub/users/{user}", {}, token="agent").get("http_error") == 401
    names = [o["org"] for o in worker("GET", "/v1/hub/orgs", token=None)["orgs"]]
    assert org in names and user not in names, names
    print(" refused: an org with a user's name and a user with an org's name (409), a bad key (400), no admin (401)")

    def as_user(k):
        h = {"X-Nerudo-User": user, "X-Nerudo-Key": k, "User-Agent": UA}
        def get(path):
            try:
                return json.loads(urllib.request.urlopen(urllib.request.Request(f"{WORKER}{path}", headers=h), timeout=60).read())
            except urllib.error.HTTPError as e:
                return {"http_error": e.code}
        return get

    me, wrong = as_user(key), as_user("0" * 32)
    assert me("/v1/hub/me")["user"] == user and wrong("/v1/hub/me").get("http_error") == 401
    s1, s2 = me(f"/t/{one}/api/state"), me(f"/t/{two}/api/state")
    assert s1["me"] == {"name": user, "roles": ["maintainer"]} and s2["me"] == {"name": user, "roles": ["reviewer"]}, (s1["me"], s2["me"])
    # Another key under the user's name is another person: the sign-in key reads the public
    # repo as anyone does, and acts as nobody.
    assert worker("GET", f"/v1/hub/repos/{org}/three", token=None)["public"]
    s3 = me(f"/t/{three}/api/state")
    assert s3["me"] is None, s3["me"]
    s4 = me(f"/t/{four}/api/state")
    assert s4["me"] is None, s4["me"]
    assert wrong(f"/t/{one}/api/state").get("http_error") == 403
    print(" one sign-in key: the user is maintainer on one and reviewer on two; on three (the same name, another")
    print("   key) and four (the key, as old-name) it reads as anyone; a wrong key is refused")

    p = worker("GET", f"/v1/hub/orgs/{user}", token=None)
    assert p["kind"] == "user" and p["title"] == "Scenario user" and p["key"] == pk, p
    assert [r["repo"] for r in p["repos"]] == ["two"], p["repos"]
    on = {f"{r['org']}/{r['repo']}": (r["roles"], r["as"], r["verified"]) for r in p["on"]}
    assert on == {f"{org}/one": (["maintainer"], None, True), f"{user}/two": (["reviewer"], None, True),
                  f"{org}/three": (["maintainer"], None, False), f"{org}/four": (["maintainer"], "old-name", True)}, on
    assert p["orgs"] == [{"org": org, "title": ""}], p["orgs"]
    assert p["teams"] == [{"org": org, "team": "maintainer", "repos": ["four", "one"]}], p["teams"]
    o = worker("GET", f"/v1/hub/orgs/{org}", token=None)
    assert o["kind"] == "org" and {x["name"]: x["user"] for x in o["people"]} == {user: True, "old-name": False}, o["people"]
    print(f" profile /{user}: owns two; on one, two, four (as old-name, same key) and three (another key, not this user);")
    print(f"   org {org}; team maintainer in one and four")

    # git with the sign-in key, as on a git host: one password for every repo.
    d = os.path.join(SCRATCH, f"{one}-user")
    rc, _, err = git_cmd("git", "clone", "-q", f"{WORKER}/{org}/one.git", d, user=user, token=key)
    assert rc == 0, err
    sh("git", "switch", "-q", "-c", "from-the-user", cwd=d)
    open(os.path.join(d, "targets", "t03", "src.txt"), "a").write("from the user\n")
    sh("git", "commit", "-q", "-am", "t03: from the user", cwd=d)
    rc, _, err = git_cmd("git", "push", "origin", "from-the-user", cwd=d, user=user, token=key)
    assert rc == 0 and "Opened pull request #" in err, err
    st = wait(one, "from-the-user", "landed", timeout=90)
    # three is public, so git reads it with no password; a push there asks for one, and the
    # sign-in key is not the password of three's person of that name.
    rc, _, err = git_cmd("git", "push", f"{WORKER}/{org}/three.git", "HEAD:refs/heads/from-the-user", cwd=d, user=user, token=key)
    assert rc and "Authentication failed" in err, err
    print(f" git: cloned and pushed {org}/one with the sign-in key (landed as seq {st['seq']}); a push to three is refused")
    print(" users: ok")


def aliases():
    """A governance PR gives an approver a new name with the same key. The app then shows the
    old name on old records (approvals, tasks, events) as the new one; text that people wrote
    stays as written. The admin can point a former name at a person, and remove it."""
    home = os.path.join(SCRATCH, "home-aliases")
    pk = cli("keygen", home=home)
    trunk = new_trunk("aliases", approver("you", pk, ["maintainer"]))
    a = Agent(trunk, "a00")
    you = api(trunk, "POST", "invite", {"user": "you"}, admin=True)["url"]
    task = app_api(trunk, you, "POST", "tasks", {"title": "Give the approver a real name", "body": "approved by you, as written"})
    assert "id" in task, task

    def seal(cid):
        return json.loads(cli("seal", "--trunk", trunk, "--change", cid, "--yes", home=home).splitlines()[-1])

    a.change("g1", edit_ci)
    wait(trunk, "g1", "awaiting_seal")
    seal("g1")
    wait(trunk, "g1", "landed")

    def rename(d):
        p = os.path.join(d, "nerudo.toml")
        text = open(p).read()
        assert 'name = "you"' in text
        open(p, "w").write(text.replace('name = "you"', 'name = "kim"'))

    a.change("r1", rename)
    wait(trunk, "r1", "awaiting_seal")
    seal("r1")
    wait(trunk, "r1", "landed")
    share = api(trunk, "POST", "share", {}, admin=True)["url"]

    def names():
        st = app_api(trunk, share, "GET", "state")
        sealed = {l["change_id"]: l["sealed_by"] for l in st["landings"]}
        t = app_api(trunk, share, "GET", f"task/{task['id']}")["task"]
        wrote = [e["text"] for e in st["events"] if e.get("task") == task["id"] and e["kind"] == "task"]
        return st["people"], sealed, t, wrote

    people, sealed, t, wrote = names()
    assert [p["name"] for p in people] == ["kim"], people
    assert sealed["g1"] == ["kim"] and sealed["r1"] == ["kim"], sealed
    assert t["created_by"] == "kim" and t["body"] == "approved by you, as written", t
    assert wrote and all(w.startswith("kim ") for w in wrote), wrote
    g1 = app_api(trunk, share, "GET", "change/g1")
    checks = json.dumps(g1["checks"])
    assert "signed by kim" in checks and "signed by you" not in checks, checks
    tl = g1["timeline"]
    assert tl and all(l.get("who") != "you" and not l["text"].startswith("you ") for l in tl), tl
    print(" r1 renamed you to kim: g1 and r1 show approved by kim, T-1 written by kim, its body unchanged")

    # The admin points a former name at a person, and removes it.
    assert api(trunk, "POST", "aliases", {"from": "kim", "to": "x"}).get("http_error") == 404
    assert api(trunk, "POST", "aliases", {"from": "you", "to": "you"}, admin=True).get("http_error") == 400
    assert api(trunk, "POST", "aliases", {"from": "you", "to": ""}, admin=True)["aliases"] == {}
    assert names()[2]["created_by"] == "you"
    assert api(trunk, "POST", "aliases", {"from": "you", "to": "kim"}, admin=True)["aliases"] == {"you": "kim"}
    assert names()[2]["created_by"] == "kim"
    print(" admin: removed the alias (T-1 written by you again), set it again; agents can't set one")
    print(" aliases: ok")


class McpClient:
    """`nerudo mcp` driven one line at a time, as an interactive client: it can answer the
    server's own requests (elicitation/create) in the middle of a tool call. `elicitation`
    declares the client capability; `home` picks whose ~/.nerudo/key the server reads; `key`
    is the agent's key."""

    def __init__(self, trunk, agent, repo, elicitation=True, home=None, key=None):
        env = dict(ENV, NERUDO_WORKER=WORKER, NERUDO_TOKEN=DEV["NERUDO_AGENT_TOKEN"], NERUDO_REPO=repo)
        if home:
            env["HOME"] = home
        if key:
            env["NERUDO_AGENT_KEY"] = key
        self.p = subprocess.Popen([CLI, "mcp", "--trunk", trunk, "--agent", agent], env=env, text=True,
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        self.inbox = queue.Queue()
        threading.Thread(target=self._read, daemon=True).start()
        self.id = 0
        self.replies = {}  # responses that came while we waited for another one
        caps = {"elicitation": {}} if elicitation else {}
        self.init = self.result(self.request("initialize", {"protocolVersion": "2025-06-18", "clientInfo": {"name": "scenario"},
                                                          "capabilities": caps}))
        self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def _read(self):
        for line in self.p.stdout:
            self.inbox.put(json.loads(line))
        self.inbox.put(None)

    def send(self, msg):
        self.p.stdin.write(json.dumps(msg) + "\n")
        self.p.stdin.flush()

    def request(self, method, params=None):
        self.id += 1
        self.send({"jsonrpc": "2.0", "id": self.id, "method": method, "params": params or {}})
        return self.id

    def call(self, name, args):
        return self.request("tools/call", {"name": name, "arguments": args})

    def next(self, timeout=120):
        m = self.inbox.get(timeout=timeout)
        assert m is not None, "nerudo mcp exited"
        return m

    def asked(self):
        """Wait for the server's next request (the elicitation); keep responses."""
        while True:
            m = self.next()
            if "method" in m and "id" in m:
                return m
            if "id" in m:
                self.replies[m["id"]] = m

    def result(self, rid):
        """Wait for the response to `rid`. A request from the server here is a failure."""
        while rid not in self.replies:
            m = self.next()
            assert not ("method" in m and "id" in m), f"unexpected request from the server: {m}"
            if "id" in m:
                self.replies[m["id"]] = m
        return self.replies.pop(rid)["result"]

    def text(self, rid):
        r = self.result(rid)
        return "".join(c.get("text", "") for c in r["content"]), r["isError"]

    def answer(self, req, result):
        self.send({"jsonrpc": "2.0", "id": req["id"], "result": result})

    def tools(self):
        return [t["name"] for t in self.result(self.request("tools/list"))["tools"]]

    def close(self):
        self.p.stdin.close()
        self.p.wait(timeout=30)


def ide_approval():
    """Approve from the coding agent: the agent asks, only the person answers, and the
    signature is the one `nerudo seal` makes."""
    home = os.path.join(SCRATCH, "home-ide-you")
    pk = cli("keygen", home=home)
    trunk = new_trunk("ide", approver("you", pk, ["maintainer"]))
    a = Agent(trunk, "a00")
    a.change("c1", edit_target("c1", "t01"))
    wait(trunk, "c1", "landed")
    a.change("g1", edit_ci)
    st = wait(trunk, "g1", "awaiting_seal")
    print(" g1 (CI edit) parked:", st["seal_reasons"])

    # A client that can't show a form doesn't get the tool, and the tool refuses it.
    plain = McpClient(trunk, "a00", a.dir, elicitation=False, home=home, key=a.key)
    assert "request_approval" not in plain.tools() and "land" in plain.tools()
    text, err = plain.text(plain.call("request_approval", {"change": "g1"}))
    assert err and "nerudo seal" in text, text
    plain.close()

    c = McpClient(trunk, "a00", a.dir, home=home, key=a.key)
    assert "request_approval" in c.tools() and "request_approval" in c.init["instructions"]
    text, err = c.text(c.call("request_approval", {"change": "c1"}))
    assert err and '"landed"' in text and "not awaiting a seal" in text, text
    print(" no elicitation: the tool is not listed; c1 (merged) is refused:", text.split(".")[0])

    # Accept with approve ticked: the person's key signs, and g1 merges.
    rid = c.call("request_approval", {"change": "g1"})
    req = c.asked()
    assert req["method"] == "elicitation/create" and req["id"] != rid, req
    schema, message = req["params"]["requestedSchema"], req["params"]["message"]
    assert schema["required"] == ["approve"] and schema["properties"]["approve"]["type"] == "boolean", schema
    st = api(trunk, "GET", "changes/g1")
    digest = api(trunk, "GET", "stats")["rules_digest"]
    for want in (f"PR #{st['number']} (g1)", st["req"]["commit"], digest, "governance: 0 of 1 approvers (none yet): 1 more needed", "You sign as: you (maintainer)", "g1: change", "1 file, +1 −0", "ci/check.py +1 −0"):
        assert want in message, (want, message)
    # While the form is open: a ping is answered, a second request for g1 is refused at once,
    # and a notification and another tool call wait; nothing is dropped.
    ping = c.request("ping")
    dup = c.call("request_approval", {"change": "g1"})
    c.send({"jsonrpc": "2.0", "method": "notifications/roots/list_changed"})
    rec = c.call("receipt", {"change_id": "g1"})
    assert c.result(ping) == {}
    text, err = c.text(dup)
    assert err and "open already" in text, text
    assert rec not in c.replies, "the receipt call was answered before the person answered"
    c.answer(req, {"action": "accept", "content": {"approve": True}})
    text, err = c.text(rid)
    assert not err and text.startswith("APPROVED: you signed") and "governance: 1 of 1 approvers (you)" in text, text
    text, err = c.text(rec)
    assert not err and "g1" in text, text  # answered after the person answered
    st = wait(trunk, "g1", "landed")
    sh("git", "-c", header(a.trunk_auth), "fetch", "-q", "origin", cwd=a.dir)
    body = sh("git", "log", "-1", "--format=%B", st["trunk_commit"], cwd=a.dir)
    assert f"Sealed-By: you {pk} " in body, body
    out = cli("verify", "--trunk", trunk)
    assert any(l.strip().endswith("governance, sealed by you") for l in out.splitlines()), out
    print(" g1: accepted in the form; the seal is the CLI's (Sealed-By: you), merged as seq", st["seq"], "and verify passes")

    # Decline, or accept without approve ticked: nothing is signed, and g2 stays parked.
    a.change("g2", edit_ci)
    wait(trunk, "g2", "awaiting_seal")
    for answer in ({"action": "decline"}, {"action": "accept", "content": {"approve": False}}, {"action": "cancel"}):
        rid = c.call("request_approval", {"change": "g2"})
        c.answer(c.asked(), answer)
        text, err = c.text(rid)
        assert not err and text.startswith("NOT APPROVED") and "Nothing was signed" in text, text
    time.sleep(2)
    st = api(trunk, "GET", "changes/g2")
    assert st["state"] == "awaiting_seal" and st["reason"]["quorum"][0]["have"] == [], st
    c.close()
    print(" g2: declined, accepted without approve, and cancelled; no seal, still parked")

    # `nerudo inbox` in the checkout: g2 waits for you; it shows the size and the files, and
    # a yes signs through the same path as `nerudo seal`.
    env = dict(ENV, NERUDO_WORKER=WORKER, NERUDO_TOKEN=DEV["NERUDO_AGENT_TOKEN"], HOME=home)
    p = subprocess.run([CLI, "inbox", "--trunk", trunk], env=env, cwd=a.dir, input="y\n", capture_output=True, text=True, timeout=120)
    out = p.stdout
    assert p.returncode == 0 and "1 PR on" in out and "change g2 rev 1" in out, redact(out + p.stderr)
    assert "1 file, +1 −0" in out and "ci/check.py +1 −0" in out and "1 of 1 approvers (you)" in out, out
    print(" inbox: g2 with its files, sealed with y; merged as seq", wait(trunk, "g2", "landed")["seq"])
    print(" ide_approval: ok")



def beaten():
    """A conflict names what beat it: the PR that landed first, its pinned plan, and the
    conflicting lines. The MCP receipt and the app show them."""
    trunk = new_trunk("beaten")
    seed = api(trunk, "GET", "trunk")["head"]
    # The first agent lands through `nerudo mcp` with context, as in the context scenario.
    fork = api(trunk, "POST", "forks", {"agent": "w1"})
    repo = os.path.join(SCRATCH, f"{trunk}-w1")
    sh("git", "-c", header(fork.get("trunk_authorization") or BASIC), "clone", "-q", fork["trunk"], repo)
    sh("git", "switch", "-q", "-c", "w1", "origin/main", cwd=repo)
    edit_target("w1", "t03")(repo)
    sh("git", "add", "-A", cwd=repo)
    sh("git", "commit", "-q", "-m", "w1: t03 names its owner\n\nWhy: the report reads the first line.", cwd=repo)
    plan = ("## Plan\nThe first line of t03 names w1, because the report reads it.\n\n"
            "## Decisions\n- Keep it one line.\n\n## Rejected\n- A second line: the report reads one.\n")
    (landed, err), = mcp(trunk, "w1", repo, [("land", {"change_id": "w1", "context": plan, "wait_s": 60})], key=fork.get("agent_key"))
    assert not err and "LANDED" in landed, landed
    first = api(trunk, "GET", "changes/w1")
    sh("git", "-c", header(fork.get("trunk_authorization") or BASIC), "fetch", "-q", "origin", cwd=repo)
    pin = [l.split(": ", 1)[1] for l in sh("git", "log", "-1", "--format=%B", first["trunk_commit"], cwd=repo).splitlines()
           if l.startswith("Context: ")]
    assert len(pin) == 1, pin
    # The second agent changes the same line from the same base: it is closed for a conflict.
    loser = Agent(trunk, "l1")
    loser.change("l1", edit_target("l1", "t03"), base=seed)
    rec = wait(trunk, "l1", "ejected", timeout=60)
    r = rec["reason"]
    assert r["kind"] == "conflict" and r["paths"] == ["targets/t03/src.txt"], r
    [by] = r["beaten_by"]
    assert (by["change_id"], by["number"], by["agent"], by["commit"]) == ("w1", first["number"], "w1", first["trunk_commit"]), by
    assert by["context"] == pin[0] and by["paths"] == ["targets/t03/src.txt"], (by, pin)
    h = r["hunks"]["targets/t03/src.txt"]
    assert "<<<<<<< main\nw1 edited\n" in h and "l1 edited\n>>>>>>> this change" in h and len(h) <= 4096 + 8, h
    assert "t03 line 5" not in h, h  # only the conflict and two lines around it
    # The MCP receipt names the PR and shows its plan (not the rejected ideas) and the lines.
    (text, err), = mcp(trunk, "l1", repo, [("receipt", {"change_id": "l1"})], key=loser.key)
    assert not err and f"CLOSED: conflict with PR #{first['number']} (w1, " in text, text
    assert "The first line of t03 names w1" in text and "Keep it one line" in text and "A second line" not in text, text
    assert "<<<<<<< main" in text and "keep their decision" in text.lower(), text
    # The app shows the same receipt, and the check names the PR that beat it.
    share = api(trunk, "POST", "share", {}, admin=True)["url"]
    page = app_api(trunk, share, "GET", "change/l1")
    assert page["change"]["reason"]["beaten_by"][0]["number"] == first["number"], page["change"]["reason"]
    row = [c for c in page["checks"] if c["name"] == "Paths and conflicts"][0]
    assert row["status"] == "fail" and f"with #{first['number']}" in row["detail"], row
    # Rebuilt on the new main, it merges.
    loser.change("l1", edit_target("l1", "t03"), rev=2)
    st = wait(trunk, "l1", "landed", timeout=60)
    print(f" beaten: ok (l1 was closed for a conflict in targets/t03/src.txt; its receipt names PR #{first['number']} "
          f"(w1, {first['trunk_commit'][:7]}) with its pinned context {pin[0].split('@')[0]}, and {len(h)} bytes of "
          f"conflict lines; the MCP receipt showed w1's plan; rev 2 on the new main merged as seq {st['seq']})")



def identity():
    """Each agent has its own key, from its first fork. The shared agent token alone can't get
    another agent's fork token, act as a person, or send a revision of another agent's PR."""
    homes = {n: os.path.join(SCRATCH, f"home-identity-{n}") for n in ("kim", "lee")}
    rules = (approver("kim", cli("keygen", home=homes["kim"]), ["maintainer"])
             + approver("lee-ann", cli("keygen", home=homes["lee"]), ["maintainer"]))
    trunk = new_trunk("identity", rules)
    a, b = Agent(trunk, "a00"), Agent(trunk, "a01")
    assert (a.key or "").startswith("lsa1.a00.0.") and (b.key or "").startswith("lsa1.a01.0."), "no agent key with the first fork"
    remedy = f"nerudo agent-key --trunk {trunk} --agent a00"

    # a00's fork token: not for the agent token alone, nor for a01's key; for a00's key and the admin.
    r = api(trunk, "POST", "forks", {"agent": "a00"})
    assert r.get("http_error") == 403 and remedy in r["body"], redact(json.dumps(r))
    assert api(trunk, "POST", "forks", {"agent": "a00"}, key=b.key).get("http_error") == 403
    assert api(trunk, "POST", "forks", {"agent": "a00", "agent_key": b.key}).get("http_error") == 403
    for got in (api(trunk, "POST", "forks", {"agent": "a00"}, key=a.key),
                api(trunk, "POST", "forks", {"agent": "a00", "agent_key": a.key}),
                api(trunk, "POST", "forks", {"agent": "a00"}, admin=True)):
        assert got.get("remote") == a.fork["remote"] and got.get("agent_key") == a.key, redact(json.dumps(got))
    print(" a00's fork token: 403 for the agent token alone and for a01's key; given to a00's key (header or body) and the admin")

    # Rotate: the old key stops at once, for reads too. The CLI prints the key and saves it.
    assert "http_error" in api(trunk, "POST", "agents/a00/key"), "the agent token rotated a key"
    new = api(trunk, "POST", "agents/a00/key", admin=True)
    assert new.get("agent_key", "").startswith("lsa1.a00.1.") and new["agent_key"] != a.key, redact(json.dumps(new))
    old = api(trunk, "POST", "forks", {"agent": "a00"}, key=a.key)
    assert old.get("http_error") == 403 and remedy in old["body"], redact(json.dumps(old))
    assert api(trunk, "GET", "stats", key=a.key).get("http_error") == 403
    a.key = new["agent_key"]
    assert api(trunk, "POST", "forks", {"agent": "a00"}, key=a.key).get("remote") == a.fork["remote"]
    assert cli("agent-key", "--trunk", trunk, "--agent", "a00", admin=True) == a.key
    path = os.path.join(SCRATCH, ".nerudo", "agents", trunk, "a00")
    assert open(path).read().strip() == a.key and os.stat(path).st_mode & 0o777 == 0o600, oct(os.stat(path).st_mode)
    rotated = cli("agent-key", "--trunk", trunk, "--agent", "a00", "--rotate", admin=True)
    assert rotated.startswith("lsa1.a00.2.") and api(trunk, "GET", "stats", key=a.key).get("http_error") == 403
    a.key = rotated
    print(" rotate: keys of gen 0 and gen 1 stopped at once (403, reads too); nerudo agent-key printed the key and saved it (0600)")

    # People: the agent token can't take a person's name or a person's fork name (lee-ann: lee_ann).
    for name in ("kim", "lee_ann"):
        r = api(trunk, "POST", "forks", {"agent": name})
        assert r.get("http_error") == 403 and "person" in r["body"], (name, r)
    assert api(trunk, "POST", "forks", {"agent": "lee_ann"}, admin=True).get("name"), "the admin can't make a person's fork"
    for path_, body in (("cleanup", {"as": "lee_ann"}), ("cleanup", {"as": "kim"}), ("tasks", {"as": "lee_ann", "title": "x"}),
                        ("tasks/T-1/claim", {"as": "lee_ann"})):
        r = api(trunk, "POST", path_, body)
        assert r.get("http_error") == 403, (path_, body, r)
    for who in ("kim", "lee_ann"):
        assert api(trunk, "GET", f"notes?who={who}").get("http_error") == 403, who
    assert api(trunk, "GET", "notes?who=kim", admin=True) == []
    assert api(trunk, "GET", "notes?who=a00", key=b.key).get("http_error") == 403
    assert api(trunk, "GET", "notes?who=a01", key=b.key) == []
    url = f"{WORKER}/v1/trunks/{trunk}/git"
    rc, _, err = git_cmd("git", "ls-remote", url, user="lee_ann", token=DEV["NERUDO_AGENT_TOKEN"], raw=True)
    assert rc and "Authentication failed" in err, err
    rc, _, err = git_cmd("git", "ls-remote", url, user="a00", token=a.key, raw=True)
    assert rc == 0, err
    rc, _, err = git_cmd("git", "ls-remote", url, user="a01", token=a.key, raw=True)
    assert rc and "Authentication failed" in err, err
    print(" people: forks as kim or lee_ann, cleanup/tasks/claim as a person, and notes?who=<person> get 403 with the "
          "agent token; git as lee_ann with the agent token gets 401; a00's key is its git password, not a01's")

    # The agent token alone acts for no agent that has a fork. Each refusal says what to do.
    token = DEV["NERUDO_AGENT_TOKEN"]
    alone = (("land", {"change_id": "c8", "rev": 1, "fork": a.fork["name"], "commit": "ab" * 20, "base": "ab" * 20}),
             ("tasks/T-1/claim", {"as": "a00"}), ("tasks", {"as": "a00", "title": "x"}),
             ("comments", {"change_id": "c1", "as": "a00", "body": "I am a00"}),
             ("reviews", {"change_id": "c1", "reviewer": "kim", "as": "a00"}),
             ("reactions", {"change_id": "c1", "emoji": "🎉", "as": "a00"}),
             ("cleanup", {"as": "a00"}), ("releases/propose", {"by": "a00"}))
    for path_, body in alone:
        r = api(trunk, "POST", path_, body)
        assert r.get("http_error") == 403 and remedy in r["body"] and "X-Nerudo-Agent-Key" in r["body"], (path_, r)
    r = api(trunk, "GET", "notes?who=a00")
    assert r.get("http_error") == 403 and remedy in r["body"], r
    assert api(trunk, "POST", "cleanup", {"as": "a00"}, key=a.key) == {"deleted": 0}
    assert api(trunk, "GET", "notes?who=a00", key=a.key) == []
    print(" the agent token alone, as a00: land, claim, tasks, comment, review, react, cleanup, release proposal and "
          "notes?who=a00 get 403 with what to do; a00's key works")

    # Plain git: the agent token is not a00's password; a00's key is. A new agent's first push
    # with the agent token makes its fork; its next push needs its key.
    sh("git", "-c", header(a.trunk_auth), "fetch", "-q", "origin", cwd=a.dir)

    def commit(cid, t):
        sh("git", "switch", "-q", "--detach", "origin/main", cwd=a.dir)
        edit_target(cid, t)(a.dir)
        sh("git", "add", "-A", cwd=a.dir)
        sh("git", "commit", "-q", "-m", f"{cid}: by plain git", cwd=a.dir)

    def push(user, password, cid):
        return git_cmd("git", "push", url, f"HEAD:refs/heads/{cid}", cwd=a.dir, user=user, token=password, raw=True)
    commit("p1", "t04")
    rc, _, err = push("a00", token, "p1")
    assert rc and "403" in err and "not its password" in err and remedy in err, err
    rc, _, err = push("a00", a.key, "p1")
    assert rc == 0 and "Opened pull request #" in err, err
    commit("p2", "t06")
    rc, _, err = push("g1", token, "p2")
    assert rc == 0 and "Opened pull request #" in err and "This push made the fork of agent g1" in err, err
    assert f"nerudo agent-key --trunk {trunk} --agent g1" in err and "***" not in err, err  # redact() marks a key as ***
    rc, _, err = push("g1", token, "p2")
    assert rc and "403" in err and "not its password" in err, err
    assert wait(trunk, "p1", "landed", timeout=90)["req"]["fork"] == a.fork["name"]
    assert wait(trunk, "p2", "landed", timeout=90)["req"]["fork"].endswith("--g1")
    n = api(trunk, "POST", "forks", {"agent": "a05"})
    assert n.get("agent_key", "").startswith("lsa1.a05.0."), redact(json.dumps(n))
    print(" git: a00's push with the agent token got 403 and merged with a00's key; the first push of a new agent g1 with "
          "the agent token made its fork and said how to get its key, and its second push got 403; a new agent a05 "
          "got its fork and key from POST /forks with the agent token alone")

    # A PR stays with its fork: a01 can't send a revision of a00's c1, with or without its key.
    a.change("c1", edit_target("c1", "t01"))
    c1 = wait(trunk, "c1", "landed")
    steal = {"change_id": "c1", "rev": 2, "fork": b.fork["name"], "commit": c1["req"]["commit"], "base": c1["req"]["base"]}
    for key in (None, b.key):
        r = api(trunk, "POST", "land", steal, key=key)
        assert r.get("http_error") == 403 and "Only that fork" in r["body"], r
    r = api(trunk, "POST", "land", {**steal, "change_id": "c9", "fork": a.fork["name"]}, key=b.key)
    assert r.get("http_error") == 403 and "not its fork" in r["body"], r
    r = api(trunk, "POST", "comments", {"change_id": "c1", "as": "a00", "body": "I am a00"}, key=b.key)
    assert r.get("http_error") == 403, r
    assert api(trunk, "POST", "comments", {"change_id": "c1", "as": "a01", "body": "mine"}, key=b.key).get("author") == "a01"
    assert api(trunk, "GET", "changes/c1")["req"]["rev"] == 1
    print(" c1 (a00): a01 can't send revision 2 (403 with the agent token and with its key); a01's key can't land from "
          "a00's fork or comment as a00")

    # An agent closes its own parked PR with its key; not another agent, not the agent token alone.
    a.change("g1", edit_ci)
    wait(trunk, "g1", "awaiting_seal")
    assert api(trunk, "POST", "changes/g1/close", {"as": "a00"}).get("http_error") == 403
    assert api(trunk, "POST", "changes/g1/close", {}, key=b.key).get("http_error") == 403
    closed = api(trunk, "POST", "changes/g1/close", {"as": "kim", "reason": "not needed"}, key=a.key)
    assert closed.get("state") == "ejected" and closed["reason"] == {"kind": "closed", "by": "a00", "detail": "not needed"}, closed
    print(" g1 (a00, waiting for an approval): closed by a00 with its key; a01's key and the agent token alone got 403")

    # nerudo mcp for an agent with a fork and no key: it says what to do. With the key, it works.
    (text, err), = mcp(trunk, "a01", SCRATCH, [("mentions", {})])
    assert err and f"nerudo agent-key --trunk {trunk} --agent a01" in text and "lsa1." not in text, redact(text)
    print(" nerudo mcp as a01 without its key:", text.splitlines()[0][:120] + "...")
    cli("agent-key", "--trunk", trunk, "--agent", "a01", admin=True)
    (text, err), = mcp(trunk, "a01", SCRATCH, [("mentions", {})])
    assert not err and "a01" in text, redact(text)
    (text, err), = mcp(trunk, "a02", SCRATCH, [("mentions", {})])
    kept = os.path.join(SCRATCH, ".nerudo", "agents", trunk, "a02")
    assert not err and open(kept).read().startswith("lsa1.a02.0.") and os.stat(kept).st_mode & 0o777 == 0o600, redact(text)
    print(" after nerudo agent-key, a01's nerudo mcp works; a new agent a02's key was kept in ~/.nerudo/agents (0600)")
    print(" identity: ok")


def wait_rev(trunk, cid, rev, want, timeout=30):
    end = time.time() + timeout
    while time.time() < end:
        st = api(trunk, "GET", f"changes/{cid}?rev={rev}&wait=5")
        if st.get("state") == want:
            return st
    raise AssertionError(f"{cid} rev {rev}: still {st.get('state')} ({st.get('reason')}), wanted {want}")


def merging(trunk, cid, timeout=40):
    """Wait until a revision of `cid` is in the batch being pushed; return that revision."""
    end = time.time() + timeout
    while time.time() < end:
        for c in api(trunk, "GET", "queue").get("changes", []):
            if c["change_id"] == cid and c.get("merging"):
                return c["rev"]
        time.sleep(0.2)
    raise AssertionError(f"{cid} was never in a batch being pushed")


def kind(r):
    return r.get("http_error"), json.loads(r.get("body") or "{}").get("kind")


def revisions():
    """The revisions of a PR (cf/scale/backlog.md, T-8 and T-9). Each batch waits 5 s for its
    simulated CI, so a revision stays in the batch being pushed long enough to test the fence."""
    trunk = new_trunk("revisions", approver("kim", cli("keygen", home=os.path.join(SCRATCH, "home-revisions-kim")), ["maintainer"]),
                      ci_ms=5000)
    a = Agent(trunk, "a00")
    seed = api(trunk, "GET", "trunk")["head"]
    url = f"{WORKER}/v1/trunks/{trunk}/git"

    def push(branch, force=False):
        spec = f"{'+' if force else ''}HEAD:refs/heads/{branch}"
        return git_cmd("git", "push", url, spec, cwd=a.dir, user="a00", token=a.key, raw=True)

    def commit(name, t, on_main=True):
        if on_main:
            sh("git", "-c", header(a.trunk_auth), "fetch", "-q", "origin", cwd=a.dir)
            sh("git", "switch", "-q", "--detach", "origin/main", cwd=a.dir)
        edit_target(name, t)(a.dir)
        sh("git", "add", "-A", cwd=a.dir)
        sh("git", "commit", "-q", "-m", f"{name}: by plain git", cwd=a.dir)

    # Paused: revision 1 waits. Revision 3 (a gap is allowed) replaces it, and the loop closes
    # it on the paused trunk too. Revision 2 is then older; revision 3 with another commit is
    # not a retry.
    api(trunk, "POST", "pause", {"reason": "revisions scenario"}, admin=True)
    r1 = a.change("r1", edit_target("r1", "t01"))
    assert r1.get("state") == "queued", r1
    b3 = a.prepare("r1", edit_target("r1-v3", "t01"), rev=3)
    assert a.send(b3).get("state") == "queued"
    assert kind(a.change("r1", edit_target("r1-v2", "t01"), rev=2)) == (409, "older")
    assert kind(a.send({**b3, "commit": r1["req"]["commit"]})) == (409, "taken")
    st = wait_rev(trunk, "r1", 1, "ejected")
    assert st["reason"] == {"kind": "superseded", "rev": 3}, st
    assert a.send(b3).get("req", {}).get("commit") == b3["commit"], "revision 3 sent again is not idempotent"
    # Plain git: a second push to a waiting branch replaces its revision. A change is one commit
    # on main, so revision 2 is the commit made again (as with git commit --amend).
    commit("b1", "t03")
    rc, _, err = push("b1")
    assert rc == 0 and "Opened pull request #" in err, err
    commit("b1-v2", "t03")
    rc, _, err = push("b1", force=True)
    assert rc == 0 and "(revision 2)" in err and "Revision 2 replaces revision 1, which does not merge." in err, err
    assert wait_rev(trunk, "b1", 1, "ejected")["reason"] == {"kind": "superseded", "rev": 2}
    q = [(c["change_id"], c["rev"]) for c in api(trunk, "GET", "queue")["changes"]]
    assert sorted(q) == [("b1", 2), ("r1", 3)], q
    print(" paused: r1 rev 3 replaced rev 1 (superseded); rev 2 after rev 3 got 409 older, rev 3 with another commit 409 "
          "taken; a second git push to b1 replaced its rev 1; the queue holds only r1 rev 3 and b1 rev 2")
    api(trunk, "POST", "resume", {}, admin=True)
    assert wait(trunk, "r1", "landed", timeout=90)["req"]["rev"] == 3
    st = wait(trunk, "b1", "landed", timeout=90)
    assert st["req"]["rev"] == 2 and st["req"]["base"] == seed, st

    # A parked revision is replaced too; the approvals it needs are for the new commit.
    a.change("g1", edit_ci)
    wait(trunk, "g1", "awaiting_seal", timeout=60)
    a.change("g1", lambda d: open(os.path.join(d, "ci", "check.py"), "a").write("# revision 2\n"), rev=2)
    assert wait_rev(trunk, "g1", 1, "ejected")["reason"] == {"kind": "superseded", "rev": 2}
    assert wait(trunk, "g1", "awaiting_seal", timeout=60)["req"]["rev"] == 2
    (text, err), = mcp(trunk, "a00", a.dir, [("receipt", {"change_id": "g1", "rev": 1})], key=a.key)
    assert not err and "REPLACED" in text and "rev 2" in text, text
    # A merged change id takes no new revision.
    assert kind(a.change("r1", edit_target("r1-v4", "t01"), rev=4)) == (409, "merged")
    print(" g1 rev 2 replaced the parked rev 1 and waits for its own approval (MCP receipt: REPLACED); r1 rev 4 after "
          "the merge got 409 merged")

    # The fence: while f1 rev 1 is in the batch being pushed, rev 2 gets 409 merging. After
    # the merge it gets 409 merged.
    b2 = a.prepare("f1", edit_target("f1-v2", "t02"), rev=2)
    a.change("f1", edit_target("f1", "t02"))
    assert merging(trunk, "f1") == 1
    assert kind(a.send(b2)) == (409, "merging")
    assert wait(trunk, "f1", "landed", timeout=60)["req"]["rev"] == 1
    assert kind(a.send(b2)) == (409, "merged")
    # Plain git: a push while the branch's PR is in the batch waits for the batch. The PR
    # merged, so the push opens a new PR, based on the merged commit.
    commit("b2", "t05")
    rc, _, err = push("b2")
    assert rc == 0 and "Opened pull request #" in err, err
    assert merging(trunk, "b2") == 1
    commit("b2-more", "t06", on_main=False)
    rc, _, err = push("b2")
    assert rc == 0 and "This push waited for the batch with revision 1" in err and "for b2 is merged" in err, err
    print(" git push b2 while its PR was in the batch\n" + "\n".join("   " + l for l in err.strip().splitlines()))
    first = api(trunk, "GET", "changes/b2")
    st = wait(trunk, "b2..2", "landed", timeout=90)
    assert first["state"] == "landed" and st["req"]["base"] == first["req"]["commit"] and st["size"]["files"] == 2, (first, st)

    stats = api(trunk, "GET", "stats")
    summary = api(trunk, "GET", "summary")["stats"]
    assert stats["superseded"] == 3 and summary["closed"] == 0 and summary["open"] == 1, (stats, summary)
    out = cli("verify", "--trunk", trunk)
    assert "landed changes, each on trunk once" in out, out[-800:]
    print(" f1 rev 2 got 409 merging while rev 1 was in the batch, then 409 merged; 3 revisions superseded, 0 PRs closed; "
          "nerudo verify: each change id on main once")
    print(" revisions: ok")


if __name__ == "__main__":
    for name in sys.argv[1:] or ["approvals", "quorum", "recovery", "authority_recovery", "allowance_recovery", "fencing", "ci", "ci_red", "social", "branches", "decide", "context", "compare", "release", "hub", "git", "users", "aliases", "ide_approval", "beaten", "identity", "revisions"]:
        print(f"== {name}")
        globals()[name]()
