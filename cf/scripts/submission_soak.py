#!/usr/bin/env python3
"""Bounded eight-client submission soak on a dedicated Cloudflare deployment.

All outbound HTTP, including Git and the independent CLI audit, passes through
one paced local proxy. Worker CI is the real integrated TypeScript check;
the final Node runtime oracle is external acceptance evidence.
"""
from __future__ import annotations

import argparse
import concurrent.futures
import datetime as dt
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

from llm_concurrency import git_env, redact, variables

ROOT = Path(__file__).resolve().parents[2]
EXPECTED_HOST = "nerudo-scale-submission-20261013.patrickdeutsch.workers.dev"
MAX_CHANGES = 250
RULES = '''[footprint]
units = ["src/changes/*", "src/shared.ts"]
[seals]
governance = ["nerudo.toml", "tsconfig.json"]
[ci]
typecheck = { tsconfig = "tsconfig.json" }
'''


def validate_target(worker):
    url = urllib.parse.urlsplit(worker)
    if url.scheme != "https" or url.hostname != EXPECTED_HOST or url.path not in ("", "/"):
        raise ValueError("This proof only mutates the isolated submission Worker")
    return worker.rstrip("/")


class Pace:
    def __init__(self, rate=3, clock=time.monotonic, sleep=time.sleep):
        if not 0 < rate <= 3:
            raise ValueError("HTTP rate must be positive and at most 3 requests/s")
        self.interval, self.clock, self.sleep = 1 / rate, clock, sleep
        self.lock, self.next_at, self.starts = threading.Lock(), 0.0, []

    def take(self):
        with self.lock:
            now = self.clock()
            if self.next_at > now:
                self.sleep(self.next_at - now)
            started = self.clock()
            self.starts.append(started)
            self.next_at = started + self.interval


class Network:
    def __init__(self, settings, pace):
        self.settings, self.pace = settings, pace
        self.routes, self.lock = {}, threading.Lock()
        self.errors, self.requests, self.consecutive = [], [], 0
        self.stop = threading.Event()
        class NoRedirect(urllib.request.HTTPRedirectHandler):
            def redirect_request(self, *_args, **_kwargs):
                return None
        self.opener = urllib.request.build_opener(NoRedirect)
        outer = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_GET(self):
                self.forward()

            def do_POST(self):
                self.forward()

            def forward(self):
                route, _, suffix = self.path.lstrip("/").partition("/")
                if route not in outer.routes:
                    self.send_error(404)
                    return
                remote, auth = outer.routes[route]
                headers = {k: v for k, v in self.headers.items() if k.lower() in ("accept", "content-type", "git-protocol", "x-nerudo-agent-key")}
                headers["Authorization"] = auth
                body = self.rfile.read(int(self.headers.get("Content-Length", 0))) if self.command == "POST" else None
                try:
                    status, content_type, data = outer.request(self.command, remote + "/" + suffix, body, headers)
                    # The CLI discovers trunk's Git URL itself. Keep that independent read
                    # throttled too, rather than letting it bypass the proxy.
                    if status == 200 and suffix.split("?", 1)[0].endswith("/trunk"):
                        info = json.loads(data)
                        if isinstance(info, dict) and info.get("remote"):
                            info["remote"] = outer.route(info["remote"], info.get("authorization") or "")
                            info["authorization"] = ""
                            data = json.dumps(info).encode()
                    self.send_response(status)
                    self.send_header("Content-Type", content_type)
                    self.send_header("Content-Length", str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)
                except Exception:
                    self.send_error(502, "isolated proof upstream unavailable")

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def route(self, remote, auth):
        with self.lock:
            ident = str(len(self.routes))
            self.routes[ident] = (remote.rstrip("/"), auth)
        return f"http://127.0.0.1:{self.server.server_port}/{ident}"

    def request(self, method, url, body=None, headers=None):
        self.pace.take()
        start = time.monotonic()
        req = urllib.request.Request(url, method=method, data=body,
            headers={"User-Agent": "nerudo-submission-soak/1", **(headers or {})})
        try:
            try:
                response = self.opener.open(req, timeout=35)
            except urllib.error.HTTPError as exc:
                response = exc
            with response:
                data = response.read((4 << 20) + 1)
                if len(data) > 4 << 20:
                    raise ValueError("Fixture response exceeds 4 MiB safety bound")
                status, content_type = response.status, response.headers.get("Content-Type", "application/octet-stream")
            with self.lock:
                self.requests.append({"method": method, "status": status, "seconds": round(time.monotonic() - start, 3)})
                self.consecutive = self.consecutive + 1 if status >= 500 else 0
                if status >= 500:
                    self.errors.append({"status": status})
                if self.consecutive >= 4 or len(self.errors) >= 20:
                    self.stop.set()
            return status, content_type, data
        except Exception as exc:
            with self.lock:
                self.errors.append({"error": type(exc).__name__})
                self.consecutive += 1
                if self.consecutive >= 4 or len(self.errors) >= 20:
                    self.stop.set()
            raise

    def api(self, trunk, method, path, body=None, admin=False, key=None):
        endpoint = path if path.startswith("/") else f"/v1/trunks/{trunk}/{path}"
        headers = {"Authorization": "Bearer " + self.settings["NERUDO_ADMIN_TOKEN" if admin else "NERUDO_AGENT_TOKEN"], "Content-Type": "application/json"}
        if key:
            headers["X-Nerudo-Agent-Key"] = key
        status, _, data = self.request(method, self.settings["NERUDO_WORKER"] + endpoint,
                                     json.dumps(body).encode() if body is not None else None, headers)
        if status != 200:
            raise RuntimeError(redact(f"HTTP {status}: {data.decode(errors='replace')[:500]}", self.settings))
        return json.loads(data)


def command(args, cwd=None, env=None, timeout=90):
    proc = subprocess.run(args, cwd=cwd, env=env or git_env(), capture_output=True, text=True, timeout=timeout)
    if proc.returncode:
        raise RuntimeError(redact(proc.stderr + proc.stdout, env)[-1500:])
    return proc.stdout.strip()


def evaluate(records, entries, trailers, files, stats):
    landed = [r for r in records if r["receipt"].get("state") == "landed"]
    ids = [r["change_id"] for r in landed]
    log_ids = [e["change_id"] for e in entries]
    failures = []
    for cid in ids:
        if log_ids.count(cid) != 1 or trailers.count(cid) != 1 or cid not in files:
            failures.append(cid)
    if set(log_ids) != set(ids) or len(log_ids) != len(set(log_ids)):
        failures.append("receipt/log mismatch")
    if set(files) - {"HEAD"} != set(ids):
        failures.append("receipt/file mismatch")
    return {"landed": len(ids), "exactly_once": not failures, "failures": failures,
            "head_matches": stats.get("head") == files.get("HEAD")}


def run(args):
    settings = variables(args.vars)
    settings["NERUDO_WORKER"] = validate_target(settings["NERUDO_WORKER"])
    if not 30 <= args.seconds <= 600 or args.clients != 8 or not 1 <= args.max_changes <= MAX_CHANGES:
        raise ValueError("Use eight clients, 30–600 seconds and at most 250 submitted revisions")
    work = Path(tempfile.mkdtemp(prefix="nerudo-submission-soak-"))
    pace, net = Pace(args.rate), None
    net = Network(settings, pace)
    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%d-%H%M%S")
    trunk, repo = "soak-" + stamp, "submission-soak-" + stamp
    records, samples, errors, lock = [], [], [], threading.Lock()
    counter = 0
    try:
        seed = net.api(trunk, "POST", "init", {"trunk": trunk,
            "host": {"kind": "artifacts", "binding": "ARTIFACTS", "repo": trunk},
            "seed": {"files": {"nerudo.toml": RULES,
                "tsconfig.json": json.dumps({"compilerOptions": {"strict": True, "target": "es2022", "module": "es2022", "lib": ["es2022"], "skipLibCheck": True}, "include": ["src"]}),
                "src/shared.ts": 'export const marker: string = "seed";\n'}, "message": "Seed bounded submission proof\n"},
            "batch_cap": 8, "ci_ms": 0}, admin=True)
        net.api(trunk, "PUT", f"/v1/hub/repos/nerudo/{repo}", {"trunk": trunk, "public": True,
            "description": "Bounded eight-client soak: real TypeScript CI, conflicts, recovery and independent audit."}, admin=True)
        agents = []
        for i in range(8):
            fork = net.api(trunk, "POST", "forks", {"agent": f"soak{i}"})
            directory = work / f"a{i}"
            read = net.route(fork["trunk"], fork["trunk_authorization"])
            write = net.route(fork["remote"], fork["authorization"])
            command(["git", "clone", "-q", read, str(directory)])
            command(["git", "config", "user.name", f"soak{i}"], directory)
            command(["git", "config", "user.email", f"soak{i}@nerudo.invalid"], directory)
            agents.append((directory, write, fork["name"], fork["agent_key"]))
        start, barrier = time.monotonic(), threading.Barrier(8)
        deadline = start + args.seconds

        def client(i):
            nonlocal counter
            directory, write, fork, key = agents[i]
            n = 0
            while time.monotonic() < deadline and not net.stop.is_set():
                cid, rev = f"a{i}-c{n}", 1
                hot, invalid = n % 3 == 0, i == 0 and n == 1
                while rev <= 3 and time.monotonic() < deadline and not net.stop.is_set():
                    with lock:
                        if counter >= args.max_changes:
                            return
                        counter += 1
                    try:
                        command(["git", "fetch", "-q", "origin"], directory)
                        command(["git", "switch", "-q", "--detach", "origin/main"], directory)
                        base = command(["git", "rev-parse", "HEAD"], directory)
                        path = directory / "src/changes" / (cid + ".ts")
                        path.parent.mkdir(exist_ok=True)
                        path.write_text(f'export const value: {"number" if invalid and rev == 1 else "string"} = "{cid}";\n')
                        if hot:
                            (directory / "src/shared.ts").write_text(f'export const marker: string = "{cid}";\n')
                        command(["git", "add", "-A"], directory)
                        command(["git", "commit", "-q", "-m", f"{cid}: bounded soak revision {rev}"], directory)
                        commit = command(["git", "rev-parse", "HEAD"], directory)
                        if n == 0 and rev == 1:
                            barrier.wait(timeout=120)
                        command(["git", "push", "-q", write, f"HEAD:refs/heads/land/{cid}-{rev}"], directory)
                        body = {"change_id": cid, "rev": rev, "fork": fork, "commit": commit, "base": base}
                        submitted = time.monotonic()
                        net.api(trunk, "POST", "land", body, key=key)
                        retried = n % 4 == 0
                        if retried:
                            duplicate = net.api(trunk, "POST", "land", body, key=key)
                            if duplicate.get("req", {}).get("commit") != commit:
                                raise AssertionError("Idempotent retry changed the submitted commit")
                        receipt = net.api(trunk, "GET", f"changes/{cid}?rev={rev}&wait=10")
                        for _ in range(5):
                            if receipt.get("state") in ("landed", "ejected"):
                                break
                            receipt = net.api(trunk, "GET", f"changes/{cid}?rev={rev}&wait=10")
                        with lock:
                            records.append({"client": i, "change_id": cid, "rev": rev, "base": base, "commit": commit,
                                "hot": hot, "deliberate_type_error": invalid and rev == 1, "idempotent_retry": retried,
                                "at_s": round(submitted - start, 3), "latency_s": round(time.monotonic() - submitted, 3), "receipt": receipt})
                        kind = (receipt.get("reason") or {}).get("kind")
                        if receipt.get("state") == "ejected" and kind in ("conflict", "ci_failed"):
                            rev += 1
                            time.sleep(1 + i / 4)
                            continue
                        break
                    except Exception as exc:
                        with lock:
                            errors.append({"client": i, "change_id": cid, "error": redact(str(exc), settings)})
                        if len(errors) >= 12:
                            net.stop.set()
                        break
                n += 1
                net.stop.wait(min(max(0, start + n * 60 - time.monotonic()), max(0, deadline - time.monotonic())))

        def reader():
            while time.monotonic() < deadline and not net.stop.is_set():
                try:
                    stats = net.api(trunk, "GET", "stats")
                    with lock:
                        samples.append({"at_s": round(time.monotonic() - start, 3), "head": stats.get("head"),
                            "landed": stats.get("landed"), "queued": stats.get("queued"), "ci_runs": stats.get("ci_runs")})
                except Exception as exc:
                    with lock:
                        errors.append({"reader": redact(str(exc), settings)})
                net.stop.wait(min(15, max(0, deadline - time.monotonic())))

        with concurrent.futures.ThreadPoolExecutor(max_workers=9) as pool:
            jobs = [pool.submit(client, i) for i in range(8)] + [pool.submit(reader)]
            for job in jobs:
                job.result()
        elapsed = time.monotonic() - start
        stats, entries = net.api(trunk, "GET", "stats"), net.api(trunk, "GET", "log?since=0&limit=250")
        final = work / "final"
        read_url = command(["git", "remote", "get-url", "origin"], agents[0][0])
        command(["git", "clone", "-q", read_url, str(final)])
        head = command(["git", "rev-parse", "HEAD"], final)
        trailers = command(["git", "log", "--format=%(trailers:key=Change-Id,valueonly)"], final).splitlines()
        files = {p.stem: p.read_text() for p in (final / "src/changes").glob("*.ts")}
        files["HEAD"] = head
        audit = evaluate(records, entries, trailers, files, stats)
        runtime_code = 'import fs from "node:fs"; import {pathToFileURL} from "node:url"; const root=process.argv[1]; for(const f of fs.readdirSync(root+"/src/changes")){const m=await import(pathToFileURL(root+"/src/changes/"+f)); if(m.value!==f.slice(0,-3))throw Error(f);} const s=await import(pathToFileURL(root+"/src/shared.ts")); if(typeof s.marker!=="string")throw Error("marker"); console.log("external runtime oracle passed");'
        runtime = command(["node", "--experimental-strip-types", "--input-type=module", "-e", runtime_code, str(final)])
        env = {k: v for k, v in git_env().items() if "TOKEN" not in k and "SECRET" not in k and "API_KEY" not in k}
        env.update(NERUDO_WORKER=net.route(settings["NERUDO_WORKER"], "Bearer " + settings["NERUDO_AGENT_TOKEN"]), NERUDO_TOKEN=settings["NERUDO_AGENT_TOKEN"])
        verified = subprocess.run([str(args.cli), "verify", "--trunk", trunk], env=env, capture_output=True, text=True, timeout=180)
        conflicts = [r for r in records if (r["receipt"].get("reason") or {}).get("kind") == "conflict"]
        ci_rejections = [r for r in records if (r["receipt"].get("reason") or {}).get("kind") == "ci_failed"]
        evidence = {"created_at": dt.datetime.now(dt.timezone.utc).isoformat(), "worker": settings["NERUDO_WORKER"],
            "backend_version": args.backend_version,
            "url": args.site.rstrip("/") + "/nerudo/" + repo,
            "repo": "nerudo/" + repo, "trunk": trunk, "seed_head": seed["head"], "final_head": head,
            "local_audit_checkout": str(final),
            "fixture": "small zero-dependency TypeScript; shared hot file and independent proof files",
            "clients": 8, "target_seconds": args.seconds, "elapsed_seconds": round(elapsed, 3), "rate_cap": args.rate,
            "max_submitted_revisions": args.max_changes, "submitted_revisions": counter, "simulated_ci_ms": 0,
            "stats": stats, "records": records, "reader_samples": samples, "errors": errors, "network_errors": net.errors,
            "http_requests": len(net.requests), "minimum_http_start_gap": round(min((b-a for a,b in zip(pace.starts, pace.starts[1:])), default=0), 6),
            "conflict_rejections": len(conflicts), "ci_rejections": len(ci_rejections), "independent_git_audit": audit,
            "external_runtime_acceptance": {"ok": True, "output": runtime},
            "nerudo_verify": {"ok": verified.returncode == 0, "output": redact(verified.stdout + verified.stderr, settings)},
            "limitations": ["Eight clients on one small fixture, not production capacity.", "Runtime acceptance is external; deployed CI is TypeScript checking.", "Initial same-base conflict barrier is deliberate."]}
        evidence["success"] = (elapsed >= args.seconds and not net.stop.is_set() and not errors and audit["exactly_once"]
            and audit["head_matches"] and verified.returncode == 0 and bool(conflicts) and bool(ci_rejections)
            and len({s["head"] for s in samples}) > 1)
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(redact(evidence, settings) + "\n")
        print(json.dumps({"success": evidence["success"], "evidence": str(args.out), "repo": evidence["repo"],
            "landed": audit["landed"], "conflicts": len(conflicts), "ci_rejections": len(ci_rejections), "seconds": evidence["elapsed_seconds"]}), flush=True)
        return evidence
    except Exception as exc:
        failure = {"success": False, "worker": settings["NERUDO_WORKER"], "trunk": trunk,
                   "backend_version": args.backend_version, "error": redact(str(exc), settings),
                   "submitted_revisions": counter, "records": records, "reader_samples": samples,
                   "errors": errors, "network_errors": net.errors}
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(redact(failure, settings) + "\n")
        print(json.dumps({"success": False, "evidence": str(args.out), "error": failure["error"]}), flush=True)
        return failure
    finally:
        net.server.shutdown()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--vars", type=Path, required=True)
    parser.add_argument("--cli", type=Path, default=Path("/Users/thinkstudio/landseq/cf/target/debug/nerudo"))
    parser.add_argument("--seconds", type=int, default=600)
    parser.add_argument("--clients", type=int, default=8)
    parser.add_argument("--rate", type=float, default=3)
    parser.add_argument("--max-changes", type=int, default=250)
    parser.add_argument("--backend-version", required=True)
    parser.add_argument("--site", default="https://nerudo-submission-web.patrickdeutsch.workers.dev")
    parser.add_argument("--out", type=Path, default=ROOT / "cf/evidence/submission-soak-20261013.json")
    result = run(parser.parse_args())
    raise SystemExit(0 if result["success"] else 1)


if __name__ == "__main__":
    main()
