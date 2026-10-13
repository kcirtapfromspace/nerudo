#!/usr/bin/env python3
"""Approvals signed before the canonical rules digest still pass, on a deployed Worker.

f4c79e3 made the rules digest canonical. Production ran 07ae487 before it. 07ae487 signed
seals and releases over the SHA-256 of `serde_json::to_vec(rules)`. The new code signs the
canonical form, and its gate accepts each form in `Rules::digests()`. This script builds the
nerudo CLI of 07ae487, and uses it with the CLI of this checkout against the Worker:

  premise  07ae487's digest of the rules is a form that `Rules::digests()` accepts.
  (i)      a PR that needs one approval, sealed only by the old CLI, merges.
  (ii)     a PR that needs two approvals, sealed once by the old CLI and once by the new
           CLI, merges with both names in Sealed-By. Each Sealed-By signature verifies with
           the new code, over the digest form that its CLI signed.
  (c)      the old CLI seals through the new API as it is (it signs the digest that the
           Worker reports), and the PR merges.
  (v)      a release signed by the old CLI is cut.
  (iii)    the new `nerudo verify` passes on the whole trunk, with the release.
  (iv)     the old `nerudo verify`. 07ae487 checks a seal against its own digest form only,
           so it must refuse the seals over the canonical digest, and only those.
  probe    read only: what the new CLI would sign for a parked PR of a live trunk (default
           honojs-hono-1791769239, pr5392), and that the gate accepts that digest. It also
           verifies, with the new code, the Sealed-By signatures that landed on that trunk.

The 07ae487 Worker gave the CLI the 07ae487 digest (GET stats, GET releases/draft). The new
Worker gives the canonical digest. So the old CLI talks to the Worker through a local proxy
that gives the 07ae487 digest, as the 07ae487 Worker did. The proxy changes nothing else. A
helper built from 07ae487's nerudo-core computes that digest. A second proxy changes
nothing; it records what the other CLI runs send.

Writes: one new trunk, upgrade-<unix seconds>, and the fork of its agent. The probe sends
only GET requests, with no credentials. A scratch HOME holds the throwaway approver keys and
the agent's key; the script never reads ~/.nerudo.

  set -a; . cf/nerudo-worker/.prod.vars; set +a     # or NERUDO_VARS=<that file>
  python3 cf/scripts/upgrade_seals.py [--work DIR] [--old-src DIR] [--old-cli PATH] [--probe-only]

Each check prints PASS or FAIL. The exit status is 1 when a check fails.
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
CF = os.path.join(ROOT, "cf")
OLD = "07ae487"
UA = "nerudo-upgrade-seals/0.1"  # workers.dev refuses Python's default User-Agent
SECRET = re.compile(r"art_v\d+_[A-Za-z0-9]+(\?expires=\d+)?|(Bearer|Basic|token) [A-Za-z0-9+/=_.-]{16,}|://[^/@\s]+@"
                    r"|lsa1\.[A-Za-z0-9_]+\.\d+\.[0-9a-f]+")
TOKENS = []  # every credential the script holds; redact() takes each out
FAILED = []


def redact(text):
    text = SECRET.sub("***", str(text))
    for t in TOKENS:
        text = text.replace(t, "***")
    return text


def check(ok, what, detail=""):
    print(f"{'PASS' if ok else 'FAIL'} {what}" + (f"\n     {redact(detail)}" if detail and not ok else ""), flush=True)
    if not ok:
        FAILED.append(what)
    return ok


def load_config():
    """The Worker and the tokens: from the environment (set -a; . .prod.vars), or else from
    NERUDO_VARS or cf/nerudo-worker/.prod.vars of this checkout or of the main checkout."""
    env = {k: os.environ[k] for k in ("NERUDO_ADMIN_TOKEN", "NERUDO_AGENT_TOKEN", "NERUDO_WORKER") if os.environ.get(k)}
    if "NERUDO_ADMIN_TOKEN" not in env or "NERUDO_AGENT_TOKEN" not in env:
        common = subprocess.run(["git", "-C", ROOT, "rev-parse", "--path-format=absolute", "--git-common-dir"],
                                capture_output=True, text=True).stdout.strip()
        paths = [os.environ.get("NERUDO_VARS"), os.path.join(CF, "nerudo-worker", ".prod.vars"),
                 common and os.path.join(os.path.dirname(common), "cf", "nerudo-worker", ".prod.vars")]
        path = next((p for p in paths if p and os.path.exists(p)), None)
        if not path:
            sys.exit("no tokens: run `set -a; . cf/nerudo-worker/.prod.vars; set +a` first, or set NERUDO_VARS")
        env = {**dict(l.strip().split("=", 1) for l in open(path) if "=" in l and not l.startswith("#")), **env}
    env.setdefault("NERUDO_WORKER", "https://nerudo.patrickdeutsch.workers.dev")
    TOKENS.extend(env[k] for k in ("NERUDO_ADMIN_TOKEN", "NERUDO_AGENT_TOKEN"))
    return env


# ------------------------------------------------------------------ builds

# The digest of 07ae487: what its Worker reported, so what its CLI signed.
OLD_HELPER = r"""
use nerudo_core::Rules;
use std::io::Read;

fn main() {
    let mut text = String::new();
    std::io::stdin().read_to_string(&mut text).unwrap();
    match Rules::parse(&text) {
        Ok(r) => println!("{}", r.digest()),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
"""

# The digests of this checkout, and what its gate and its verifier say of one seal.
# In: {"rules": toml, "spec"?: SealSpec, "signature"?: hex, "commit"?: hex, "reasons"?: [..], "now_ms"?: n}
NEW_HELPER = r"""
use nerudo_core::quorum::seal_problem;
use nerudo_core::{auth, ChangeInput, Rules, SealSpec};
use nerudo_git::Oid;
use serde_json::{json, Value};
use std::io::Read;

fn main() {
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s).unwrap();
    let q: Value = serde_json::from_str(&s).unwrap();
    let rules = Rules::parse(q["rules"].as_str().unwrap_or_default()).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1)
    });
    let digests = rules.digests();
    let keys = rules.approvers.iter().filter(|a| !a.key.is_empty()).count();
    let mut out = json!({ "digest": rules.digest(), "digests": digests, "keys": keys });
    if let Some(spec) = q.get("spec").filter(|v| !v.is_null()) {
        let spec: SealSpec = serde_json::from_value(spec.clone()).unwrap();
        // The Worker's check_signed and its tally (seal_problem) both ask rules.accepts.
        out["accepts"] = json!(rules.accepts(&spec.rules_digest));
        out["form"] = json!(digests.iter().position(|d| *d == spec.rules_digest));
        if let Some(sig) = q["signature"].as_str() {
            // As verify_landing does: trailers leave the digest out, so try each form.
            let signed = auth::verify_any(&spec.approver, sig, &digests, |d| SealSpec { rules_digest: d.to_string(), ..spec.clone() }.signing_bytes());
            out["signed_form"] = json!(signed.as_ref().ok().and_then(|d| digests.iter().position(|x| x == d)));
            out["signed_error"] = json!(signed.err());
        }
        if let Some(commit) = q["commit"].as_str() {
            let oid = Oid::from_hex(commit).expect("commit");
            let change = ChangeInput { change_id: spec.change_id.clone(), rev: spec.rev, commit: oid, base: oid, trailers: vec![] };
            let reasons: Vec<String> = serde_json::from_value(q["reasons"].clone()).unwrap_or_default();
            out["seal_problem"] = json!(seal_problem(&rules, &spec, &change, &reasons, q["now_ms"].as_i64().unwrap_or(0)));
        }
    }
    println!("{out}");
}
"""


def run(cmd, cwd=None, env=None, inp=None, timeout=900):
    p = subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, text=True, input=inp, timeout=timeout)
    if p.returncode:
        raise RuntimeError(redact(f"{' '.join(cmd)}: {p.stderr.strip()[-600:]}"))
    return p.stdout.strip()


def build_env():
    """cargo and git as the user runs them, with no nerudo token."""
    return {k: v for k, v in os.environ.items() if not k.startswith("NERUDO_")}


def old_tree(work, given):
    """cf/ of 07ae487, from `git archive` (no worktree)."""
    if given:
        return given
    dest = os.path.join(work, "old07")
    if not os.path.exists(os.path.join(dest, "cf", "Cargo.toml")):
        os.makedirs(dest, exist_ok=True)
        archive = subprocess.Popen(["git", "-C", ROOT, "archive", OLD, "cf"], stdout=subprocess.PIPE)
        subprocess.run(["tar", "-x", "-C", dest], stdin=archive.stdout, check=True)
        if archive.wait():
            sys.exit(f"git archive {OLD} failed")
    return dest


def build_helper(work, name, src_cf, source, deps):
    """A one-file crate that uses the nerudo-core of `src_cf`, pinned by its Cargo.lock."""
    d = os.path.join(work, "helpers", name)
    os.makedirs(os.path.join(d, "src"), exist_ok=True)
    lines = [f'nerudo-core = {{ path = "{src_cf}/nerudo-core" }}'] + deps
    toml = f'[package]\nname = "{name}"\nversion = "0.0.0"\nedition = "2021"\npublish = false\n\n[dependencies]\n' + "\n".join(lines) + "\n\n[workspace]\n"
    open(os.path.join(d, "Cargo.toml"), "w").write(toml)
    open(os.path.join(d, "src", "main.rs"), "w").write(source)
    if not os.path.exists(os.path.join(d, "Cargo.lock")):
        shutil.copy(os.path.join(src_cf, "Cargo.lock"), os.path.join(d, "Cargo.lock"))
    env = dict(build_env(), CARGO_TARGET_DIR=os.path.join(d, "target"))
    try:
        run(["cargo", "build", "-q", "--offline"], cwd=d, env=env)
    except RuntimeError:
        run(["cargo", "build", "-q"], cwd=d, env=env)
    return os.path.join(d, "target", "debug", name)


class Tools:
    def __init__(self, work, old_src, old_cli):
        old_cf = os.path.join(old_tree(work, old_src), "cf")
        print(f"building: the CLI of {OLD} (from git archive), the CLI of this checkout, and two digest helpers", flush=True)
        if not old_cli:
            run(["cargo", "build", "-q", "-p", "nerudo-cli"], cwd=old_cf, env=build_env())
            old_cli = os.path.join(old_cf, "target", "debug", "nerudo")
        run(["cargo", "build", "-q", "-p", "nerudo-cli"], cwd=CF, env=build_env())
        self.old_cli = old_cli
        self.new_cli = os.path.join(CF, "target", "debug", "nerudo")
        self.old_digest_bin = build_helper(work, "upgrade-seals-old", old_cf, OLD_HELPER, [])
        self.new_digest_bin = build_helper(work, "upgrade-seals-new", CF, NEW_HELPER,
                                           [f'nerudo-git = {{ path = "{CF}/nerudo-git" }}', 'serde_json = "1"'])

    def old_digest(self, rules):
        return run([self.old_digest_bin], inp=rules)

    def new(self, rules, **q):
        return json.loads(run([self.new_digest_bin], inp=json.dumps({"rules": rules, **q})))


# ------------------------------------------------------------------ the Worker


class Worker:
    def __init__(self, cfg):
        self.url = cfg["NERUDO_WORKER"].rstrip("/")
        self.admin = cfg["NERUDO_ADMIN_TOKEN"]
        self.agent = cfg["NERUDO_AGENT_TOKEN"]

    def api(self, trunk, method, path, body=None, admin=False):
        headers = {"Content-Type": "application/json", "User-Agent": UA,
                   "Authorization": f"Bearer {self.admin if admin else self.agent}"}
        req = urllib.request.Request(f"{self.url}/v1/trunks/{trunk}/{path}", method=method,
                                     data=json.dumps(body).encode() if body is not None else None, headers=headers)
        try:
            return json.loads(urllib.request.urlopen(req, timeout=90).read())
        except urllib.error.HTTPError as e:
            return {"http_error": e.code, "body": redact(e.read().decode())}

    def public(self, trunk, path):
        """The app API with no credentials: what anyone reads of a public repo."""
        req = urllib.request.Request(f"{self.url}/t/{trunk}/api/{path}", headers={"User-Agent": UA})
        return json.loads(urllib.request.urlopen(req, timeout=90).read())

    def wait(self, trunk, cid, want, timeout=90):
        end, st = time.time() + timeout, {}
        while time.time() < end:
            st = self.api(trunk, "GET", f"changes/{cid}?wait=5")
            if st.get("state") == want or st.get("state") in ("landed", "ejected"):
                break
        return st


class Proxy(ThreadingHTTPServer):
    """Forwards each request to the Worker. With `swap` (canonical -> 07ae487 digest), it gives
    the 07ae487 digest in GET stats and GET releases/draft, as the 07ae487 Worker did. It
    records each body sent with POST. It logs nothing, so no token reaches the output."""

    def __init__(self, upstream, swap=None):
        super().__init__(("127.0.0.1", 0), ProxyHandler)
        self.upstream, self.swap = upstream, swap
        self.posted, self.swapped, self.unswapped = [], 0, []
        threading.Thread(target=self.serve_forever, daemon=True).start()

    @property
    def url(self):
        return f"http://127.0.0.1:{self.server_address[1]}"


class ProxyHandler(BaseHTTPRequestHandler):
    def do_GET(self):
        self.forward()

    def do_POST(self):
        self.forward()

    def forward(self):
        s = self.server
        n = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(n) if n else None
        if self.command == "POST" and body:
            s.posted.append((urllib.parse.urlparse(self.path).path, json.loads(body)))
        keep = ("authorization", "content-type", "user-agent", "x-nerudo-agent-key", "accept")
        headers = {k: v for k, v in self.headers.items() if k.lower() in keep}
        req = urllib.request.Request(s.upstream + self.path, data=body, method=self.command, headers=headers)
        try:
            r = urllib.request.urlopen(req, timeout=120)
            status, data, ctype = r.status, r.read(), r.headers.get("Content-Type")
        except urllib.error.HTTPError as e:
            status, data, ctype = e.code, e.read(), e.headers.get("Content-Type")
        path = urllib.parse.urlparse(self.path).path
        if s.swap and self.command == "GET" and status == 200 and (path.endswith("/stats") or path.endswith("/releases/draft")):
            v = json.loads(data)
            if v.get("rules_digest") == s.swap[0]:
                v["rules_digest"] = s.swap[1]
                s.swapped += 1
            else:
                s.unswapped.append(v.get("rules_digest"))
            data = json.dumps(v).encode()
        self.send_response(status)
        self.send_header("Content-Type", ctype or "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):
        pass


# ------------------------------------------------------------------ the live test

RULES = """# Rules of a throwaway trunk for cf/scripts/upgrade_seals.py.
[footprint]
units = ["docs/*", "ci/*"]

[seals]
governance = ["ci/**", "nerudo.toml"]
approval = ["docs/**"]

[seals.require]
governance = {{ roles = ["maintainer"], count = 2 }}
approval = {{ roles = ["maintainer"], count = 1 }}

[[approvers]]
name = "upg-a"
key = "{a}"
roles = ["maintainer"]

[[approvers]]
name = "upg-b"
key = "{b}"
roles = ["maintainer"]
"""

SEED = {
    "README.md": "A throwaway trunk: approvals signed by nerudo 07ae487 against the canonical digest.\n",
    "docs/one.md": "one\n",
    "docs/two.md": "two\n",
    "ci/check.sh": "#!/bin/sh\necho ok\n",
}
AGENT = "upg_agent"


class Live:
    def __init__(self, tools, worker, work):
        self.t, self.w, self.work = tools, worker, work
        self.base = {k: v for k, v in build_env().items() if not k.startswith("GIT_") and k != "XDG_CONFIG_HOME"}
        # git: no system or user config, no credential helper, no signing.
        self.base.update(GIT_CONFIG_NOSYSTEM="1", GIT_TERMINAL_PROMPT="0", GIT_AUTHOR_NAME=AGENT,
                         GIT_AUTHOR_EMAIL=f"{AGENT}@bench.invalid", GIT_COMMITTER_NAME=AGENT,
                         GIT_COMMITTER_EMAIL=f"{AGENT}@bench.invalid", GIT_CONFIG_COUNT="2",
                         GIT_CONFIG_KEY_0="commit.gpgsign", GIT_CONFIG_VALUE_0="false",
                         GIT_CONFIG_KEY_1="credential.helper", GIT_CONFIG_VALUE_1="")
        self.home = {n: os.path.join(work, f"home-{n}") for n in ("a", "b", "agent")}
        for h in self.home.values():
            os.makedirs(h, exist_ok=True)
        self.repo = os.path.join(work, "repo")

    def cli(self, binary, *args, home, worker=None, ok=True):
        """A nerudo CLI as an approver runs it: their HOME (key), the agent token, a Worker URL."""
        env = dict(self.base, HOME=self.home[home], NERUDO_WORKER=worker or self.w.url, NERUDO_TOKEN=self.w.agent)
        p = subprocess.run([binary, *args], env=env, capture_output=True, text=True, timeout=600)
        if ok and p.returncode:
            raise RuntimeError(redact(f"nerudo {' '.join(args)}: {p.stderr.strip()} {p.stdout.strip()[-400:]}"))
        return p if not ok else p.stdout.strip()

    def git(self, *args):
        return run(["git", *args], cwd=self.repo, env=dict(self.base, HOME=self.home["agent"]))

    def mcp(self, *calls):
        """`nerudo mcp` (new CLI) for the agent: it makes the agent's fork, keeps the agent's
        key in the agent's HOME, and sends it with each request."""
        env = dict(self.base, HOME=self.home["agent"], NERUDO_WORKER=self.w.url, NERUDO_TOKEN=self.w.agent, NERUDO_REPO=self.repo)
        lines = [{"jsonrpc": "2.0", "id": 0, "method": "initialize",
                  "params": {"protocolVersion": "2025-06-18", "clientInfo": {"name": AGENT}, "capabilities": {}}}]
        lines += [{"jsonrpc": "2.0", "id": i + 1, "method": "tools/call", "params": {"name": n, "arguments": a}}
                  for i, (n, a) in enumerate(calls)]
        p = subprocess.run([self.t.new_cli, "mcp", "--trunk", self.trunk, "--agent", AGENT], env=env, capture_output=True,
                           text=True, input="".join(json.dumps(l) + "\n" for l in lines), timeout=600)
        out = {m["id"]: m for m in map(json.loads, p.stdout.splitlines()) if "id" in m}
        res = []
        for i in range(1, len(calls) + 1):
            r = out.get(i, {}).get("result") or {}
            text = "".join(c.get("text", "") for c in r.get("content", []))
            if r.get("isError") or not r:
                raise RuntimeError(redact(f"mcp {calls[i - 1][0]}: {text or p.stderr[-400:]}"))
            res.append(text)
        return res

    def open_pr(self, cid, path, text):
        self.git("switch", "-q", "-c", cid, "nerudo/main")
        with open(os.path.join(self.repo, path), "a") as f:
            f.write(text)
        self.git("add", "-A")
        self.git("commit", "-q", "-m", f"{cid}: edit {path}\n\nWhy: cf/scripts/upgrade_seals.py needs a PR that waits for approval.")
        [out] = self.mcp(("land", {"change_id": cid, "wait_s": 60}))
        st = self.w.api(self.trunk, "GET", f"changes/{cid}")
        check("PARKED FOR A HUMAN" in out and st.get("state") == "awaiting_seal",
              f"the agent opened {cid} through nerudo mcp; it waits for {[q['need'] for q in (st.get('reason') or {}).get('quorum', [])]} approval(s): {st.get('seal_reasons')}",
              out[-400:])
        return st

    def seal(self, binary, cid, home, proxy):
        """Seal with `binary` through `proxy`. Returns the CLI's answer and the spec it posted."""
        before = len(proxy.posted)
        out = json.loads(self.cli(binary, "seal", "--trunk", self.trunk, "--change", cid, "--yes", home=home, worker=proxy.url).splitlines()[-1])
        spec = next(b["spec"] for p, b in proxy.posted[before:] if p.endswith("/seals"))
        return out, spec

    def trailers(self, commit):
        body = self.git("log", "-1", "--format=%B", commit)
        return [l.split(": ", 1) for l in body.splitlines() if ": " in l]

    def check_sealed_by(self, cid, rec):
        """Each Sealed-By on the landed commit, checked with the new code: the digest form its
        signature covers (0: canonical; 1: 07ae487)."""
        tr = self.trailers(rec["trunk_commit"])
        get = lambda k: [v for t, v in tr if t == k]
        forms = {}
        for line in get("Sealed-By"):
            name, key, expires, sig = line.split()
            spec = {"trunk": self.trunk, "change_id": get("Change-Id")[0], "rev": int(get("Revision")[0]), "commit": get("Change-Commit")[0],
                    "rules_digest": "", "reasons": get("Seal-Reason"), "expires_at_ms": int(expires), "approver": key}
            forms[name] = self.t.new(self.rules, spec=spec, signature=sig).get("signed_form")
        return forms

    def run(self):
        t, w = self.t, self.w
        # upg-a's key is made and used by the old CLI, upg-b's by the new one. A rerun in the
        # same --work keeps them.
        key = lambda b, h: self.cli(b, "pubkey" if os.path.exists(os.path.join(self.home[h], ".nerudo", "key")) else "keygen", home=h)
        keys = {"a": key(t.old_cli, "a"), "b": key(t.new_cli, "b")}
        self.rules = RULES.format(a=keys["a"], b=keys["b"])
        d = t.new(self.rules)
        canonical, forms, legacy = d["digest"], d["digests"], t.old_digest(self.rules)
        if not check(legacy == forms[1] and canonical == forms[0],
                     f"premise: {OLD}'s digest of the trunk's rules ({legacy[:12]}) is digests()[1]; the canonical one is {canonical[:12]}",
                     json.dumps({"legacy": legacy, "digests": forms})):
            return

        self.trunk = f"upgrade-{int(time.time())}"
        files = dict(SEED, **{"nerudo.toml": self.rules})
        out = w.api(self.trunk, "POST", "init", {"trunk": self.trunk, "host": {"kind": "artifacts", "binding": "ARTIFACTS", "repo": self.trunk},
                                                 "seed": {"files": files, "message": "seed\n"}}, admin=True)
        if "head" not in out:
            check(False, f"create trunk {self.trunk}", json.dumps(out))
            return
        print(f"trunk: {self.trunk} (throwaway; delete it after the run)", flush=True)
        reported = w.api(self.trunk, "GET", "stats").get("rules_digest")
        check(reported == canonical, f"the Worker reports the canonical digest {canonical[:12]} (GET stats)", str(reported))

        legacy_proxy, plain_proxy = Proxy(w.url, swap=(canonical, legacy)), Proxy(w.url)
        try:
            self.live(canonical, legacy, legacy_proxy, plain_proxy)
        finally:
            for p in (legacy_proxy, plain_proxy):
                p.shutdown()
                p.server_close()

    def live(self, canonical, legacy, legacy_proxy, plain_proxy):
        t, w, T = self.t, self.w, self.trunk
        os.makedirs(self.repo, exist_ok=True)
        self.git("init", "-q", "-b", "work")
        self.mcp(("sync", {}))
        key_file = os.path.join(self.home["agent"], ".nerudo", "agents", T, AGENT)
        check(os.path.exists(key_file), f"the agent {AGENT} got its own key with its fork; nerudo mcp sends it (X-Nerudo-Agent-Key)")
        self.open_pr("p1", "docs/one.md", "one, edited\n")
        self.open_pr("p2", "ci/check.sh", "echo still ok\n")
        self.open_pr("p3", "docs/two.md", "two, edited\n")

        # (i) Only the old CLI seals p1, over the digest that 07ae487 signed.
        out, spec = self.seal(t.old_cli, "p1", "a", legacy_proxy)
        st = w.wait(T, "p1", "landed")
        check(spec["rules_digest"] == legacy != canonical and out.get("complete") is True and st.get("state") == "landed" and st.get("sealed_by") == ["upg-a"],
              f"(i) p1 needs 1 approval; the {OLD} CLI sealed it over the {OLD} digest {legacy[:12]}; it merged (seq {st.get('seq')}), sealed by {st.get('sealed_by')}",
              json.dumps({"posted_digest": spec["rules_digest"], "answer": out, "receipt": st.get("reason")}))

        # (ii) p2 needs 2: the old CLI over the old digest, then the new CLI over the canonical one.
        out_a, spec_a = self.seal(t.old_cli, "p2", "a", legacy_proxy)
        half = w.api(T, "GET", "changes/p2")
        out_b, spec_b = self.seal(t.new_cli, "p2", "b", plain_proxy)
        st = w.wait(T, "p2", "landed")
        self.mcp(("sync", {}))
        signed = self.check_sealed_by("p2", st) if st.get("state") == "landed" else {}
        check(spec_a["rules_digest"] == legacy and spec_b["rules_digest"] == canonical and out_a.get("complete") is False
              and half.get("state") == "awaiting_seal" and out_b.get("complete") is True and st.get("state") == "landed"
              and st.get("sealed_by") == ["upg-a", "upg-b"] and signed == {"upg-a": 1, "upg-b": 0},
              f"(ii) p2 needs 2 approvals; the {OLD} CLI sealed it ({legacy[:12]}: 1 of 2, still waiting), the new CLI sealed it "
              f"({canonical[:12]}); it merged (seq {st.get('seq')}). Sealed-By: upg-a over digests()[{signed.get('upg-a')}], "
              f"upg-b over digests()[{signed.get('upg-b')}], both verified by the new code",
              json.dumps({"digests": [spec_a["rules_digest"], spec_b["rules_digest"]], "answers": [out_a, out_b], "sealed_by": st.get("sealed_by"), "signed": signed}))

        # (c) The old CLI against the new API as it is: it signs the digest the Worker reports.
        out, spec = self.seal(t.old_cli, "p3", "a", plain_proxy)
        st = w.wait(T, "p3", "landed")
        check(spec["rules_digest"] == canonical and st.get("state") == "landed" and st.get("sealed_by") == ["upg-a"],
              f"(c) with no proxy change, the {OLD} CLI fetches the change and GET stats, signs the canonical digest and posts the seal; p3 merged (seq {st.get('seq')})",
              json.dumps({"posted_digest": spec["rules_digest"], "answer": out, "state": st.get("state")}))

        # (v) The old CLI signs a release over the old digest.
        before = len(legacy_proxy.posted)
        rel = self.cli(t.old_cli, "release", "--trunk", T, "--yes", home="a", worker=legacy_proxy.url)
        posted = [b["spec"]["rules_digest"] for p, b in legacy_proxy.posted[before:] if p.endswith("/releases")]
        check(posted == [legacy] and "released v0.1.0" in rel,
              f"(v) the {OLD} CLI signed release v0.1.0 over the {OLD} digest; the sequencer cut the tag",
              json.dumps({"posted": posted, "out": rel[-300:]}))
        check(not legacy_proxy.unswapped and legacy_proxy.swapped >= 3,
              f"the proxy gave the {OLD} digest {legacy_proxy.swapped} times, and the Worker reported no other digest",
              json.dumps(legacy_proxy.unswapped))

        # (iii) The new verifier, on the whole trunk.
        new = self.cli(t.new_cli, "verify", "--trunk", T, home="b", ok=False)
        text = new.stdout
        check(new.returncode == 0 and "3 landings needed seals" in text and "sealed by upg-a, upg-b" in text and "1 release tag" in text,
              "(iii) the new nerudo verify passes: 3 sealed landings (1 with 2 seals), release v0.1.0, landing log",
              text[-1500:] + new.stderr[-300:])
        for line in text.splitlines():
            if "sealed by" in line or "v0.1.0" in line:
                print(f"     {line.strip()}")

        # (iv) The old verifier: it knows only its own digest form.
        old = self.cli(t.old_cli, "verify", "--trunk", T, home="a", ok=False)
        text = old.stdout
        commit = {c: w.api(T, "GET", f"changes/{c}")["trunk_commit"][:7] for c in ("p1", "p2", "p3")}
        refused = sorted(l.strip() for l in text.splitlines() if re.match(r"\s+[0-9a-f]{7}: ", l))
        want = sorted([f"{commit['p2']}: upg-b's seal: bad signature", f"{commit['p3']}: upg-a's seal: bad signature"])
        sealed_ok = any(l.strip().startswith(f"{commit['p1']} approval, sealed by upg-a") for l in text.splitlines())
        release_ok = re.search(r"✓.*1 release tag", text) is not None
        check(old.returncode != 0 and refused == want and sealed_ok and release_ok and "pass the trunk checks" in text,
              f"(iv) the {OLD} nerudo verify fails, as it must: it checks a seal against the {OLD} digest only. It accepts p1 and the "
              f"release (signed over that digest) and refuses exactly the 2 seals over the canonical digest: "
              f"{'; '.join(refused)}",
              text[-1500:] + old.stderr[-300:])


# ------------------------------------------------------------------ the read-only probe


def probe(tools, w, trunk, cid):
    """What the new CLI would sign for `cid`, from GETs with no credentials. `nerudo seal`
    signs GET stats' rules_digest: the canonical digest of nerudo.toml at main (the gate's
    stored rules are those)."""
    rules = w.public(trunk, "tree?path=nerudo.toml")["text"]
    ch = w.public(trunk, f"change/{cid}")
    c = ch["change"]
    commit = next(x["sha"] for x in ch["commits"] if x["rev"] == c["rev"])
    d = tools.new(rules)
    legacy = tools.old_digest(rules)
    spec = {"trunk": trunk, "change_id": cid, "rev": c["rev"], "commit": commit, "rules_digest": d["digest"],
            "reasons": c["seal_reasons"], "expires_at_ms": int(time.time() * 1000) + 30 * 60_000, "approver": ""}
    gate = tools.new(rules, spec=spec, commit=commit, reasons=c["seal_reasons"], now_ms=int(time.time() * 1000))
    old = tools.new(rules, spec=dict(spec, rules_digest=legacy))
    # The Worker's own answer: GET release/draft gives the digest of the same rules, and
    # anyone may read it on a public repo. GET stats, which `nerudo seal` reads, needs a token.
    reported = w.public(trunk, "release/draft").get("rules_digest")
    check(reported == d["digest"], f"probe: the Worker reports the digest {str(reported)[:12]} for {trunk} (GET release/draft): "
          f"the canonical digest of nerudo.toml at main", json.dumps({"reported": reported, "local": d["digest"]}))
    print(f"probe {trunk} #{c.get('number')} {cid} ({c['state']}, sealed by {c['sealed_by']}): the new CLI would sign\n"
          f"     {json.dumps({k: v for k, v in spec.items() if k != 'approver'})}")
    check(c["state"] == "awaiting_seal" and gate["accepts"] and gate["form"] == 0 and old["accepts"] and old["form"] == 1,
          f"probe: the digest the new CLI would sign ({d['digest'][:12]}) is digests()[0], and the {OLD} digest of the same rules "
          f"({legacy[:12]}) is digests()[1]: the gate accepts both. accepted set: {[x[:12] for x in d['digests']]}",
          json.dumps({"gate": gate, "old": old}))
    # seal_problem checks the change and commit, then the digest, then the signer. With no
    # approver key in nerudo.toml, the signer is the first check to fail.
    want = "signer is not an approver in nerudo.toml" if d["keys"] == 0 else None
    check(gate["seal_problem"] == want,
          f"probe: the gate's seal_problem passes the change, commit and digest checks; then: {gate['seal_problem'] or 'nothing'}"
          + (" (no approver in its nerudo.toml has a key yet)" if d["keys"] == 0 else ""),
          json.dumps(gate))

    # The seals that landed on this trunk, checked again with the new code.
    state = w.public(trunk, "state")
    rows, bad = [], []
    for l in sorted((l for l in state["landings"] if l.get("sealed_by")), key=lambda l: l["seq"]):
        if l["seq"] < 2:
            continue
        land = w.public(trunk, f"change/{l['change_id']}")["landing"]
        parent_rules = w.public(trunk, f"tree?path=nerudo.toml&at={l['seq'] - 1}")["text"]
        old_d = tools.old_digest(parent_rules)
        for line in land["sealed_by"]:
            name, key, expires, sig = line.split()
            spec = {"trunk": trunk, "change_id": l["change_id"], "rev": l["rev"], "commit": land["change_commit"], "rules_digest": "",
                    "reasons": land["seal_reasons"], "expires_at_ms": int(expires), "approver": key}
            r = tools.new(parent_rules, spec=spec, signature=sig)
            form = r["signed_form"]
            if form is None:
                bad.append(f"seq {l['seq']} {l['change_id']} {name}: {r['signed_error']}")
            else:
                rows.append((l["seq"], l["change_id"], name, form, r["digests"][form] == old_d))
    by_form = {}
    for _, _, _, form, is_old in rows:
        by_form.setdefault(form, []).append(is_old)
    summary = ", ".join(f"{len(v)} over digests()[{f}]" + (f" (the {OLD} digest)" if all(v) else "") for f, v in sorted(by_form.items()))
    check(rows and not bad,
          f"probe: the new verifier accepts all {len(rows)} Sealed-By signatures on {trunk}: {summary}",
          "; ".join(bad))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--work", help="scratch directory (default: a new temporary one)")
    ap.add_argument("--old-src", help=f"a directory that holds cf/ of {OLD} (default: git archive into --work)")
    ap.add_argument("--old-cli", help=f"the nerudo CLI built from {OLD} (default: build it from --old-src)")
    ap.add_argument("--probe-only", action="store_true", help="only the read-only probe; no trunk")
    ap.add_argument("--probe-trunk", default="honojs-hono-1791769239")
    ap.add_argument("--probe-change", default="pr5392")
    a = ap.parse_args()
    work = a.work or tempfile.mkdtemp(prefix="nerudo-upgrade-")
    os.makedirs(work, exist_ok=True)
    cfg = load_config()
    w = Worker(cfg)
    print(f"worker {w.url}, work dir {work}", flush=True)
    try:
        tools = Tools(work, a.old_src, a.old_cli)
        bench = open(os.path.join(CF, "fixtures", "bench-nerudo.toml")).read()
        check(tools.old_digest(bench) == tools.new(bench)["digests"][1],
              f"premise: the {OLD} helper's digest of fixtures/bench-nerudo.toml is its digests()[1], as tests/digest.rs pins")
        if not a.probe_only:
            Live(tools, w, work).run()
        probe(tools, w, a.probe_trunk, a.probe_change)
    except Exception as e:  # a crash is a failure, and its message may hold a token
        check(False, f"unexpected error: {type(e).__name__}", str(e))
    print(f"\n{'FAIL' if FAILED else 'PASS'}: {len(FAILED)} check(s) failed" if FAILED else "\nPASS: every check passed")
    sys.exit(1 if FAILED else 0)


if __name__ == "__main__":
    main()
