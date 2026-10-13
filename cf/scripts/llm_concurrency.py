#!/usr/bin/env python3
"""Real, concurrent LLM workflows against one live NERUDO trunk.

No model-generated changes are fabricated by this harness. Each provider has an
independent clone and NERUDO identity. Runtime acceptance checks are external
evidence; the deployed sequencer's CI remains its TypeScript checker.
"""
from __future__ import annotations

import argparse
import concurrent.futures
import datetime as dt
import json
import os
from pathlib import Path
import queue
import re
import shutil
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
import urllib.parse

ROOT = Path(__file__).resolve().parents[2]
FIXTURE = ROOT / "cf/fixtures/llm-concurrency"
DEFAULT_VARS = Path.home() / ".config/nerudo/evidence.vars"
def cli_path(env=None):
    """Use an explicit binary first, then Cargo's target directory, then the local default."""
    env = os.environ if env is None else env
    if env.get("NERUDO_CLI"):
        return Path(env["NERUDO_CLI"]).expanduser()
    target = Path(env["CARGO_TARGET_DIR"]).expanduser() if env.get("CARGO_TARGET_DIR") else ROOT / "cf/target"
    return target / "debug/nerudo"


CLI = cli_path()
PROVIDERS = ("claude", "codex", "gemini", "antigravity", "ollama", "hermes")
READ_TOOLS = {"rules", "in_flight", "context", "status", "receipt", "tasks", "mentions"}
MCP_TOOLS = READ_TOOLS | {"sync", "claim", "land", "comment"}
SECRET = re.compile(r"art_v\d+_[A-Za-z0-9]+(?:\?expires=\d+)?|lsa1\.[A-Za-z0-9_]+\.\d+\.[0-9a-f]+|(?:Bearer|Basic) [A-Za-z0-9+/=_.-]{16,}")


def variables(path: Path) -> dict[str, str]:
    """Read dotenv assignments as data, never execute a shell file."""
    values = {}
    if path.exists():
        for line in path.read_text().splitlines():
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, value = line.removeprefix("export ").split("=", 1)
            values[key.strip()] = value.strip().strip("\"'")
    for key in ("NERUDO_WORKER", "NERUDO_ADMIN_TOKEN", "NERUDO_AGENT_TOKEN"):
        if os.environ.get(key):
            values[key] = os.environ[key]
    return values


def redact(value, env=None):
    text = json.dumps(value, ensure_ascii=False) if not isinstance(value, str) else value
    for key, secret in (env or os.environ).items():
        if ("TOKEN" in key or "KEY" in key or "SECRET" in key) and len(secret) >= 8:
            text = text.replace(secret, "[redacted]")
    return SECRET.sub("[redacted]", text)


def save(path: Path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2) + "\n")
    path.chmod(0o600)


def event(path: Path, value: dict, env=None):
    path.parent.mkdir(parents=True, exist_ok=True)
    record = {"at": dt.datetime.now(dt.timezone.utc).isoformat(), "at_s": time.time(), **value}
    fd = os.open(path, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
    try:
        os.write(fd, (redact(record, env) + "\n").encode())
    finally:
        os.close(fd)


def api(settings, trunk, method, path, body=None, *, admin=False):
    token = settings["NERUDO_ADMIN_TOKEN" if admin else "NERUDO_AGENT_TOKEN"]
    url = settings["NERUDO_WORKER"].rstrip("/") + path if path.startswith("/") else settings["NERUDO_WORKER"].rstrip("/") + f"/v1/trunks/{trunk}/{path}"
    request = urllib.request.Request(url, method=method,
        data=json.dumps(body).encode() if body is not None else None,
        headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json", "User-Agent": "nerudo-llm-evidence/1"})
    try:
        with urllib.request.urlopen(request, timeout=90) as response:
            return json.load(response)
    except urllib.error.HTTPError as exc:
        raise RuntimeError(redact(f"HTTP {exc.code}: {exc.read().decode()}", settings)) from None


def git_env():
    return {**os.environ, "GIT_TERMINAL_PROMPT": "0", "GIT_CONFIG_COUNT": "2",
            "GIT_CONFIG_KEY_0": "commit.gpgsign", "GIT_CONFIG_VALUE_0": "false",
            "GIT_CONFIG_KEY_1": "credential.helper", "GIT_CONFIG_VALUE_1": ""}


def shell(args, cwd=None, env=None, timeout=120):
    result = subprocess.run(args, cwd=cwd, env=env or git_env(), text=True,
                            capture_output=True, timeout=timeout)
    if result.returncode:
        raise RuntimeError(redact(result.stderr or result.stdout, env)[-3000:])
    return result.stdout.strip()


class MCP:
    """Small synchronous client for the existing newline-JSON stdio server."""
    def __init__(self, workspace, env, cli=CLI):
        self.proc = subprocess.Popen([str(cli), "mcp"], cwd=workspace, env=env,
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.DEVNULL, text=True, bufsize=1)
        self.lines = queue.Queue()
        self.ids = 0
        self.lock = threading.Lock()
        threading.Thread(target=self._read, daemon=True).start()
        self.request("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                     "clientInfo": {"name": "nerudo-llm-evidence", "version": "1"}})
        self.notify("notifications/initialized", {})
        self.tools = self.request("tools/list", {})["tools"]

    def _read(self):
        for line in self.proc.stdout:
            self.lines.put(line)
        self.lines.put(None)

    def notify(self, method, params):
        self.proc.stdin.write(json.dumps({"jsonrpc": "2.0", "method": method, "params": params}) + "\n")
        self.proc.stdin.flush()

    def request(self, method, params, timeout=180):
        with self.lock:
            self.ids += 1
            ident = self.ids
            self.proc.stdin.write(json.dumps({"jsonrpc": "2.0", "id": ident, "method": method, "params": params}) + "\n")
            self.proc.stdin.flush()
            deadline = time.monotonic() + timeout
            while True:
                try:
                    line = self.lines.get(timeout=max(0.01, deadline - time.monotonic()))
                except queue.Empty:
                    raise TimeoutError(f"NERUDO MCP {method} timed out") from None
                if line is None:
                    raise RuntimeError("NERUDO MCP exited")
                message = json.loads(line)
                if message.get("id") != ident:
                    continue
                if "error" in message:
                    raise RuntimeError(str(message["error"]))
                return message["result"]

    def call(self, name, arguments):
        return self.request("tools/call", {"name": name, "arguments": arguments})

    def close(self):
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()


def checks(workspace):
    paths = sorted(str(p.relative_to(workspace)) for p in (workspace / "tests").glob("*.test.mjs"))
    if not paths:
        return {"ok": False, "error": "No runtime tests found"}
    p = subprocess.run(["node", "--experimental-strip-types", "--test", *paths], cwd=workspace,
                       capture_output=True, text=True, timeout=60, env=git_env())
    return {"ok": p.returncode == 0, "exit_code": p.returncode, "output": (p.stdout + p.stderr)[-10000:]}


def function(name, description, properties=None, required=None, readonly=False):
    return {"type": "function", "function": {"name": name, "description": description,
            "parameters": {"type": "object", "properties": properties or {}, "required": required or []},
            "x-read-only": readonly}}


class Tools:
    def __init__(self, manifest, provider, settings):
        self.manifest = manifest
        self.provider = provider
        self.run_dir = Path(manifest["run_dir"])
        self.workspace = self.run_dir / "workspaces" / provider
        self.env = {k: v for k, v in git_env().items() if k != "NERUDO_ADMIN_TOKEN"}
        self.env.update(NERUDO_WORKER=settings["NERUDO_WORKER"], NERUDO_TOKEN=settings["NERUDO_AGENT_TOKEN"],
                        NERUDO_TRUNK=manifest["trunk"], NERUDO_AGENT=provider,
                        NERUDO_REPO=str(self.workspace))
        self.env.pop("NERUDO_AGENT_TOKEN", None)
        self.mcp = MCP(self.workspace, self.env, Path(manifest["cli"]))
        self.log = self.run_dir / "events" / f"{provider}.jsonl"
        self.first_land = True
        self.catalog = []
        for tool in self.mcp.tools:
            if tool["name"] in MCP_TOOLS:
                self.catalog.append({"type": "function", "function": {
                    "name": "nerudo_" + tool["name"], "description": tool["description"],
                    "parameters": tool["inputSchema"], "x-read-only": tool["name"] in READ_TOOLS}})
        string = {"type": "string"}
        self.catalog += [
            function("list_files", "List tracked repository files", readonly=True),
            function("read_file", "Read one repository file", {"path": string}, ["path"], True),
            function("write_file", "Write one src/ or tests/ file", {"path": string, "content": string}, ["path", "content"]),
            function("git_status", "Show git status", readonly=True),
            function("git_diff", "Show staged and unstaged changes", readonly=True),
            function("git_start", "Start a new task branch from nerudo/main", {"branch": string}, ["branch"]),
            function("git_commit", "Commit src/ and tests/ changes with a why body", {"message": string}, ["message"]),
            function("run_checks", "Run the real local Node runtime tests", readonly=True),
        ]

    def emit(self, value):
        event(self.log, {"provider": self.provider, **value}, self.env)

    def path(self, raw, write=False):
        path = (self.workspace / raw).resolve()
        rel = path.relative_to(self.workspace.resolve())
        if any(p.startswith(".") for p in rel.parts):
            raise ValueError("Private/configuration files are outside the tool scope")
        if write and (not rel.parts or rel.parts[0] not in ("src", "tests")):
            raise ValueError("Only src/ and tests/ may be edited")
        return path

    def barrier(self):
        ready = self.run_dir / "ready"
        ready.mkdir(exist_ok=True)
        save(ready / f"{self.provider}.json", {"at_s": time.time()})
        expected = set(self.manifest["providers"])
        deadline = time.monotonic() + self.manifest["barrier_timeout"]
        while time.monotonic() < deadline:
            if expected <= {p.stem for p in ready.glob("*.json")}:
                self.emit({"kind": "barrier_released", "all_ready": True})
                return
            time.sleep(0.2)
        self.emit({"kind": "barrier_released", "all_ready": False, "reason": "timeout"})

    def execute(self, name, arguments):
        begin = time.monotonic()
        self.emit({"kind": "tool_start", "tool": name, "arguments": arguments})
        try:
            result = self._execute(name, arguments)
        except Exception as exc:
            result = {"ok": False, "error": redact(f"{type(exc).__name__}: {exc}", self.env)}
        self.emit({"kind": "tool_end", "tool": name, "seconds": time.monotonic() - begin, "result": result})
        return result

    def _execute(self, name, a):
        if name.startswith("nerudo_"):
            native = name.removeprefix("nerudo_")
            if native not in MCP_TOOLS:
                raise ValueError("Tool not allowed")
            a = dict(a)
            if "repo" in a:
                if Path(a["repo"]).resolve() != self.workspace.resolve():
                    raise ValueError("MCP tools must use this provider's checkout")
            a["repo"] = str(self.workspace)
            if native == "land":
                if len(a.get("context", "").strip()) < 40:
                    return {"isError": True, "content": [{"type": "text", "text": "Include a substantive plan, decisions, and rejected alternatives in context."}]}
                local = checks(self.workspace)
                self.emit({"kind": "candidate_checks", **local})
                if not local["ok"]:
                    return {"isError": True, "content": [{"type": "text", "text": "Candidate runtime tests failed; fix before landing.\n" + local["output"]}]}
                if self.first_land:
                    self.first_land = False
                    self.emit({"kind": "candidate_ready", "head": shell(["git", "rev-parse", "HEAD"], self.workspace)})
                    self.barrier()
            return self.mcp.call(native, a)
        if name == "list_files":
            return shell(["git", "ls-files"], self.workspace).splitlines()
        if name == "read_file":
            return {"path": a["path"], "content": self.path(a["path"]).read_text()[:20000]}
        if name == "write_file":
            path = self.path(a["path"], True)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(a["content"])
            return {"ok": True, "path": a["path"]}
        if name == "git_status":
            return shell(["git", "status", "--short"], self.workspace)
        if name == "git_diff":
            return shell(["git", "diff", "nerudo/main", "--", "src", "tests"], self.workspace)[-20000:]
        if name == "git_start":
            if not re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9._/-]{0,60}", a["branch"]):
                raise ValueError("Invalid branch name")
            return shell(["git", "switch", "-c", a["branch"], "nerudo/main"], self.workspace)
        if name == "git_commit":
            shell(["git", "add", "--", "src", "tests"], self.workspace)
            return shell(["git", "commit", "-m", a["message"]], self.workspace)
        if name == "run_checks":
            return checks(self.workspace)
        raise ValueError("Unknown tool")


def mcp_response(result):
    # Application data (read_file includes a string named content) is not an
    # MCP CallToolResult. Preserve only actual block lists from the Rust server.
    if isinstance(result, dict) and isinstance(result.get("content"), list):
        return result
    return {"content": [{"type": "text", "text": json.dumps(result)}]}


def proxy(manifest, provider):
    # The CLI's tool process receives only agent authority. It never reads the
    # production variable file or loads the administrator token.
    settings = {"NERUDO_WORKER": os.environ["NERUDO_WORKER"],
                "NERUDO_AGENT_TOKEN": os.environ["NERUDO_TOKEN"]}
    tools = Tools(manifest, provider, settings)
    try:
        for line in sys.stdin:
            message = json.loads(line)
            if "id" not in message:
                continue
            method = message.get("method")
            if method == "initialize":
                result = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
                          "serverInfo": {"name": "nerudo-evidence", "version": "1"}}
            elif method == "tools/list":
                result = {"tools": [{"name": t["function"]["name"], "description": t["function"]["description"],
                          "inputSchema": t["function"]["parameters"]} for t in tools.catalog]}
            elif method == "tools/call":
                params = message["params"]
                result = tools.execute(params["name"], params.get("arguments", {}))
                result = mcp_response(result)
            elif method == "ping":
                result = {}
            else:
                print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32601, "message": "Unknown method"}}), flush=True)
                continue
            print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": result}), flush=True)
    finally:
        tools.mcp.close()


def prepare(args, settings):
    stamp = args.run_id or dt.datetime.now(dt.timezone.utc).strftime("%Y%m%d-%H%M%S")
    if not re.fullmatch(r"\d{8}-\d{6}", stamp):
        raise ValueError("run-id must have format YYYYMMDD-HHMMSS")
    trunk = "llm-" + stamp
    run_dir = (ROOT / "runs/llm-concurrency" / stamp).resolve()
    run_dir.mkdir(parents=True, mode=0o700, exist_ok=bool(args.run_id))
    tasks = json.loads((FIXTURE / "TASKS.json").read_text())
    tasks["antigravity"] = tasks["gemini"]
    tasks["hermes"] = tasks["ollama"]
    task_roles = {p: {"hermes": "ollama", "antigravity": "gemini"}.get(p, p) for p in args.providers}
    # A three-client run with a Google client can still cover the same complete
    # price/cart/receipt journey when the local model server is unavailable.
    if not {"ollama", "hermes"} & set(args.providers):
        for p in ("gemini", "antigravity"):
            if p in args.providers:
                task_roles[p] = "ollama"
    files = {str(p.relative_to(FIXTURE)): p.read_text() for p in FIXTURE.rglob("*") if p.is_file()}
    # The sealed in-repo brief must agree with the actual provider assignments,
    # including a Google client serving receipts or Hermes replacing Ollama.
    files["TASKS.json"] = json.dumps({p: tasks[task_roles[p]] for p in args.providers}, indent=2) + "\n"
    pubkey = shell([str(CLI), "pubkey"])
    files["nerudo.toml"] += f'\n[[approvers]]\nname = "kcirtapfromspace"\nkey = "{pubkey}"\nroles = ["maintainer"]\n'
    if args.run_id:
        seeded = api(settings, trunk, "GET", "stats")
        if seeded.get("seq", 0) != 0:
            raise ValueError("Preparation can only resume before any change has landed")
    else:
        seeded = api(settings, trunk, "POST", "init", {"trunk": trunk,
              "host": {"kind": "artifacts", "binding": "ARTIFACTS", "repo": trunk},
              "seed": {"files": files, "message": "Seed real LLM concurrency evidence workload\n"},
              "batch_cap": 16, "ci_ms": args.ci_ms}, admin=True)
    if "head" not in seeded:
        raise RuntimeError("Sequencer did not return a seed head")
    repo_name = f"concurrency-{stamp}"
    api(settings, trunk, "PUT", f"/v1/hub/repos/nerudo/{repo_name}",
        {"trunk": trunk, "public": True, "description": "Real LLM workflows on one trunk; independent clones, preserved context, and measured recovery."}, admin=True)
    task_ids = {}
    # Tasks written by a person use the app API and that person's existing
    # invitation key. The agent API deliberately cannot impersonate a person.
    invite = api(settings, trunk, "POST", "invite", {"user": "kcirtapfromspace"}, admin=True)
    invite_url = invite.get("app") or invite.get("url")
    invite_query = urllib.parse.parse_qs(urllib.parse.urlparse(invite_url).query)
    personal_key = invite_query["k"][0]
    for provider in args.providers:
        task = tasks[task_roles[provider]]
        body = task.get("body") or "Acceptance criteria:\n" + "\n".join("- " + item for item in task["acceptance"])
        request = urllib.request.Request(settings["NERUDO_WORKER"].rstrip("/") + f"/t/{trunk}/api/tasks",
            method="POST", data=json.dumps({"title": task["title"], "body": body,
                    "priority": 2, "labels": ["llm-evidence", provider]}).encode(),
            headers={"Content-Type": "application/json", "X-NERUDO-Key": personal_key,
                     "X-NERUDO-User": "kcirtapfromspace", "User-Agent": "nerudo-llm-evidence/1"})
        with urllib.request.urlopen(request, timeout=60) as response:
            out = json.load(response)
        task_ids[provider] = out["id"]
    manifest = {"version": 1, "trunk": trunk, "run_dir": str(run_dir), "vars_file": str(args.vars.resolve()),
        "cli": str(CLI), "providers": args.providers, "tasks": task_ids, "task_roles": task_roles, "base_head": seeded["head"],
        "repo": f"nerudo/{repo_name}", "url": settings["NERUDO_WORKER"].rstrip("/") + f"/nerudo/{repo_name}",
        "git_url": settings["NERUDO_WORKER"].rstrip("/") + f"/nerudo/{repo_name}.git",
        "ollama_url": args.ollama_url, "ollama_model": args.ollama_model,
        "timeout": args.timeout, "barrier_timeout": args.barrier_timeout, "created_at": dt.datetime.now(dt.timezone.utc).isoformat()}
    save(run_dir / "manifest.json", manifest)
    for provider in args.providers:
        workspace = run_dir / "workspaces" / provider
        shell(["git", "clone", "--quiet", manifest["git_url"], str(workspace)])
        shell(["git", "config", "user.name", provider], workspace)
        shell(["git", "config", "user.email", provider + "@nerudo.invalid"], workspace)
        t = Tools(manifest, provider, settings)
        try:
            result = t.execute("nerudo_sync", {})
            if result.get("isError"):
                raise RuntimeError(str(result))
        finally:
            t.mcp.close()
    return manifest


def task_prompt(manifest, provider):
    role = manifest.get("task_roles", {}).get(provider, {"antigravity": "gemini", "hermes": "ollama"}.get(provider, provider))
    task = json.loads((FIXTURE / "TASKS.json").read_text())[role]
    body = task.get("body") or "Acceptance criteria:\n" + "\n".join("- " + item for item in task["acceptance"])
    if provider == "ollama":
        return f"""Implement task {manifest['tasks'][provider]}: {task['title']}.
{body}

You own one isolated clone on shared trunk {manifest['trunk']}. Inspect actual source; never edit .gitignore, governance files or checks/. Call nerudo_claim(task='{manifest['tasks'][provider]}'), nerudo_sync, then git_start for a fresh branch from nerudo/main. After starting the branch, read BOTH src/price.ts and the existing tests/price.test.mjs; write tools become available after these reads. Follow its node:test/node:assert style. Use the existing shared API and its exported types/helpers; don't replace it with duplicated arithmetic.
Write src/receipt.ts and meaningful tests/receipt.test.mjs. Run run_checks, then git_commit with why in its message. The adapter runs genuine read-only planner/reviewer subagents.
Call nerudo_land(change_id='ollama-feature', task='{manifest['tasks'][provider]}', context='Markdown with Plan, Decisions and Rejected alternatives', wait_s=120). Include your actual decisions and checks, not this placeholder text. This first land call may wait at the test barrier.
If rejected, read nerudo_context for the named other change, sync, rebuild your own change on current nerudo/main, test and resubmit the SAME change_id. Stop only after your actual LANDED receipt, or a human-approval blocker. Tool-free promises do not complete this task.
"""
    return f"""You are the {provider} coding workflow on shared NERUDO repo {manifest['repo']}.
Implement task {manifest['tasks'][provider]}: {task['title']}.
{body}

Several real LLM workflows start from the same main. Use nerudo_claim with task='{manifest['tasks'][provider]}', then nerudo_sync, nerudo_rules, and nerudo_in_flight. Start your own branch from nerudo/main.
If native subagent tools are available, ask a read-only planner and a read-only reviewer to help; only you edit/commit/land. Ollama's planner and reviewer are managed by its adapter.
Implement actual code and meaningful runtime tests. Run checks. Do not edit governance files, checks/, AGENTS.md, or another task's requirements. Never read credential/configuration files, display tokens, alter user settings, or push main. Do not stage untracked local agent configuration. Stage only src/ and tests/.
Commit with why in its body. Call nerudo_land with change_id='{provider}-feature', task='{manifest['tasks'][provider]}', context as Markdown with Plan, Decisions and Rejected alternatives, and wait_s=120.
The first land call deliberately waits for other providers to finish coding; stay in that call, do not terminate it. This is a documented test barrier, not normal product latency.
If the receipt is EJECTED/CLOSED, inspect the named other change's nerudo_context, sync the new main, preserve the other agent's work, rebuild your own change from nerudo/main (or rebase/cherry-pick only your task), test, commit and land again with the SAME change_id. Do not abandon on the first conflict. You may update consumer call sites needed for integration. Do not claim success without a landed receipt.
After landing, sync and inspect status. End with your receipt and what you learned from other agents. Stop if approval is needed; do not sign or bypass it.
"""


def run_one(manifest, provider, settings):
    sys.path.insert(0, str(Path(__file__).parent))
    tools = None
    start = time.time()
    try:
        tools = Tools(manifest, provider, settings)
        # Prime the exact agent-key path outside a native client's OS sandbox.
        # This setup step is logged distinctly from model-initiated tool use.
        tools.emit({"kind": "harness_setup", "operation": "sync"})
        setup = tools.mcp.call("sync", {"repo": str(tools.workspace)})
        if setup.get("isError"):
            raise RuntimeError("Provider checkout synchronization failed")
        start = time.time()
        tools.emit({"kind": "workflow_start"})
        prompt = task_prompt(manifest, provider)
        if provider == "ollama":
            from llm_ollama import run_ollama
            result = run_ollama(tools.workspace, prompt, tools.catalog, tools.execute, tools.emit,
                  url=manifest["ollama_url"], model=manifest["ollama_model"], timeout=manifest["timeout"])
        else:
            from llm_native import run_native
            command = [sys.executable, str(Path(__file__).resolve()), "proxy", "--manifest",
                       str(Path(manifest["run_dir"]) / "manifest.json"), "--provider", provider]
            native_env = dict(tools.env)
            if provider == "hermes" and manifest.get("hermes_context_length"):
                native_env["NERUDO_HERMES_CONTEXT_LENGTH"] = str(manifest["hermes_context_length"])
            result = run_native(provider, tools.workspace, prompt, command, native_env,
                                Path(manifest["run_dir"]) / "logs" / provider, timeout=manifest["timeout"])
        try:
            receipt = api(settings, manifest["trunk"], "GET", f"changes/{provider}-feature")
        except RuntimeError as exc:
            receipt = {"state": "not_submitted", "error": str(exc)}
        result.update(provider=provider, started_s=start, ended_s=time.time(), receipt=receipt,
                      landed=receipt.get("state") == "landed")
    except Exception as exc:
        result = {"provider": provider, "status": "failed", "error": redact(str(exc), tools.env if tools else settings),
                  "started_s": start, "ended_s": time.time(), "landed": False}
    finally:
        if tools:
            tools.emit({"kind": "workflow_end"})
            tools.mcp.close()
    save(Path(manifest["run_dir"]) / f"{provider}.json", json.loads(redact(result, tools.env if tools else settings)))
    return result


def max_overlap(results):
    edges = [(r["started_s"], 1) for r in results] + [(r["ended_s"], -1) for r in results]
    active = maximum = 0
    for _, delta in sorted(edges, key=lambda x: (x[0], x[1])):
        active += delta
        maximum = max(maximum, active)
    return maximum


def run_all(manifest, settings):
    with concurrent.futures.ThreadPoolExecutor(max_workers=len(manifest["providers"])) as pool:
        futures = {pool.submit(run_one, manifest, p, settings): p for p in manifest["providers"]}
        results = []
        for future in concurrent.futures.as_completed(futures):
            result = future.result()
            results.append(result)
            print(f"{result['provider']}: {'landed' if result['landed'] else result.get('status', 'failed')}", flush=True)
    return verify_all(manifest, settings)


def verify_all(manifest, settings):
    # An independently retried workflow may have replaced an earlier blocked
    # result while the other providers were still running.
    run_dir = Path(manifest["run_dir"])
    results = [json.loads((run_dir / f"{p}.json").read_text()) for p in manifest["providers"]]
    final = run_dir / "final"
    if final.exists():
        shell(["git", "fetch", "--quiet", "origin"], final)
        shell(["git", "switch", "--detach", "origin/main"], final)
    else:
        shell(["git", "clone", "--quiet", manifest["git_url"], str(final)])
    runtime = checks(final)
    # Execute the trusted local fixture oracle against the final fetched tree,
    # rather than executing an oracle obtained from an agent's repository.
    command = ["node", "--experimental-strip-types", str(FIXTURE / "checks/acceptance.mjs"),
               "--repo", str(final)]
    roles = manifest.get("task_roles", {p: "gemini" if p == "antigravity" else p for p in manifest["providers"]})
    if "gemini" in roles.values():
        command.append("--with-discount")
    acceptance = subprocess.run(command, cwd=final, text=True, capture_output=True, timeout=60)
    verify_env = {**git_env(), "NERUDO_WORKER": settings["NERUDO_WORKER"], "NERUDO_AGENT_TOKEN": settings["NERUDO_AGENT_TOKEN"]}
    verify_env.pop("NERUDO_ADMIN_TOKEN", None)
    verified = subprocess.run([manifest["cli"], "verify", "--trunk", manifest["trunk"]], env=verify_env,
                              capture_output=True, text=True, timeout=180)
    stats = api(settings, manifest["trunk"], "GET", "stats")
    evidence = {"repo": manifest["repo"], "url": manifest["url"], "base_head": manifest["base_head"],
        "final_head": shell(["git", "rev-parse", "HEAD"], final), "providers": results,
        "max_overlapping_workflows": max_overlap(results), "runtime_tests": runtime,
        "acceptance": {"ok": acceptance.returncode == 0, "output": acceptance.stdout + acceptance.stderr},
        "nerudo_verify": {"ok": verified.returncode == 0, "output": redact(verified.stdout + verified.stderr, verify_env)},
        "stats": stats, "live_head_matches": stats.get("head") == shell(["git", "rev-parse", "HEAD"], final),
        "limitations": ["The initial landing barrier is artificial and logged separately.",
            "Local runtime tests and final acceptance are external checks, not deployed Worker runtime CI.",
            "Workflow overlap establishes real agent concurrency; it does not establish server capacity."]}
    evidence["success"] = (len(results) >= 3 and evidence["max_overlapping_workflows"] >= 3
                          and all(r["landed"] and r.get("status") == "completed" for r in results)
                          and evidence["live_head_matches"]
                          and runtime["ok"] and acceptance.returncode == 0 and verified.returncode == 0)
    save(run_dir / "evidence.json", json.loads(redact(evidence, settings)))
    print(json.dumps({"success": evidence["success"], "url": manifest["url"],
          "evidence": str(run_dir / "evidence.json"), "overlap": evidence["max_overlapping_workflows"]}, indent=2))
    return evidence


def add_workflow(manifest, provider, settings):
    """Join another configured provider to an existing evidence repository."""
    if provider in manifest["providers"]:
        raise ValueError("Provider is already configured")
    if provider not in ("gemini", "antigravity"):
        raise ValueError("Only the optional Google discount workflow may be added")
    if {"gemini", "antigravity"} & set(manifest["providers"]):
        raise ValueError("The discount task already has a Google workflow")
    task = json.loads((FIXTURE / "TASKS.json").read_text())["gemini"]
    invite = api(settings, manifest["trunk"], "POST", "invite", {"user": "kcirtapfromspace"}, admin=True)
    query = urllib.parse.parse_qs(urllib.parse.urlparse(invite.get("app") or invite.get("url")).query)
    request = urllib.request.Request(settings["NERUDO_WORKER"].rstrip("/") + f"/t/{manifest['trunk']}/api/tasks",
        method="POST", data=json.dumps({"title": task["title"],
            "body": "Acceptance criteria:\n" + "\n".join("- " + i for i in task["acceptance"]),
            "priority": 2, "labels": ["llm-evidence", provider]}).encode(),
        headers={"Content-Type": "application/json", "X-NERUDO-Key": query["k"][0],
                 "X-NERUDO-User": "kcirtapfromspace", "User-Agent": "nerudo-llm-evidence/1"})
    with urllib.request.urlopen(request, timeout=60) as response:
        task_id = json.load(response)["id"]
    workspace = Path(manifest["run_dir"]) / "workspaces" / provider
    shell(["git", "clone", "--quiet", manifest["git_url"], str(workspace)])
    shell(["git", "config", "user.name", provider], workspace)
    shell(["git", "config", "user.email", provider + "@nerudo.invalid"], workspace)
    manifest["providers"].append(provider)
    manifest["tasks"][provider] = task_id
    manifest.setdefault("task_roles", {p: "gemini" if p == "antigravity" else p for p in manifest["providers"]})[provider] = "gemini"
    save(Path(manifest["run_dir"]) / "manifest.json", manifest)
    return manifest


def doctor(args):
    report = {}
    for provider, command in (("claude", ["claude", "auth", "status"]), ("codex", ["codex", "login", "status"])):
        try:
            p = subprocess.run(command, capture_output=True, text=True, timeout=15)
            report[provider] = {"ready": p.returncode == 0, "status": redact(p.stdout + p.stderr)[:600]}
        except (FileNotFoundError, subprocess.TimeoutExpired) as exc:
            report[provider] = {"ready": False, "error": str(exc)}
    gemini_auth = bool(os.environ.get("GEMINI_API_KEY") or os.environ.get("GOOGLE_API_KEY")
                       or (Path.home() / ".gemini/oauth_creds.json").exists())
    report["gemini"] = {"ready": gemini_auth, "command": "npx --yes @google/gemini-cli@0.63.0", "authentication_present": gemini_auth}
    agy_installed = bool(shutil.which("agy") or os.environ.get("NERUDO_ANTIGRAVITY_BIN")
                         or (Path.home() / ".cache/nerudo-concurrency/tools/antigravity/agy").is_file())
    report["antigravity"] = {"ready": agy_installed,
                            "note": "Headless agy CLI supported; login must be checked separately"}
    try:
        from llm_native import hermes_runtime_environment
        hermes_env = hermes_runtime_environment(dict(os.environ))
        endpoint = hermes_env.get("NERUDO_HERMES_BASE_URL", "http://127.0.0.1:18434/v1")
        model = hermes_env.get("NERUDO_HERMES_MODEL", "Qwen3.8-Flash-Next-UD-IQ4_XS")
        key = hermes_env.get("NERUDO_HERMES_API_KEY")
        if not key:
            raise RuntimeError("Hermes runtime key unavailable; start the configured local server")
        request = urllib.request.Request(endpoint.rstrip("/") + "/models", headers={"Authorization": "Bearer " + key})
        with urllib.request.urlopen(request, timeout=5) as response:
            names = [m["id"] for m in json.load(response).get("data", [])]
        report["hermes"] = {"ready": bool(shutil.which("hermes") or os.environ.get("NERUDO_HERMES_BIN")) and model in names,
                            "endpoint": endpoint, "model": model, "catalog_available": True}
    except Exception as exc:
        report["hermes"] = {"ready": False, "error": type(exc).__name__, "note": "Hermes needs a running authenticated llama.cpp endpoint"}
    try:
        with urllib.request.urlopen(args.ollama_url.rstrip("/") + "/api/tags", timeout=5) as response:
            names = [m["name"] for m in json.load(response).get("models", [])]
        report["ollama"] = {"ready": args.ollama_model in names, "url": args.ollama_url, "models": names}
    except Exception as exc:
        report["ollama"] = {"ready": False, "error": str(exc)}
    print(json.dumps(report, indent=2))
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("doctor", "prepare", "run", "workflow", "add", "verify", "proxy"))
    parser.add_argument("--vars", type=Path, default=DEFAULT_VARS)
    parser.add_argument("--manifest", type=Path)
    parser.add_argument("--run-id", help="Resume preparation of a known run (YYYYMMDD-HHMMSS)")
    parser.add_argument("--provider", choices=PROVIDERS)
    parser.add_argument("--providers", type=lambda x: x.split(","), default=["claude", "codex", "antigravity"])
    parser.add_argument("--ollama-url", default="http://127.0.0.1:11434")
    parser.add_argument("--ollama-model", default="qwen3:1.7b")
    parser.add_argument("--timeout", type=int, default=900)
    parser.add_argument("--barrier-timeout", type=int, default=180)
    parser.add_argument("--ci-ms", type=int, default=0, help="Explicit simulated batch delay; default zero")
    args = parser.parse_args()
    if args.action in ("workflow", "add", "verify", "proxy") and not args.manifest:
        parser.error("This action requires --manifest for an existing evidence repo")
    if args.action in ("workflow", "add", "proxy") and not args.provider:
        parser.error("This action requires --provider")
    if args.timeout <= 0 or args.barrier_timeout <= 0 or args.ci_ms < 0:
        parser.error("Timeouts must be positive and simulated CI delay nonnegative")
    if args.action == "doctor":
        doctor(args)
        return
    if args.action == "proxy":
        proxy(json.loads(args.manifest.read_text()), args.provider)
        return
    settings = variables(args.vars)
    # Reproduction uses dedicated test Workers; the historical runs are not replayed
    # into a production deployment. This guard changes no recorded measurement.
    if args.action in ("prepare", "run", "workflow", "add"):
        target = urllib.parse.urlsplit(settings.get("NERUDO_WORKER", ""))
        hostname = target.hostname or ""
        if target.scheme != "https" or not hostname.endswith(".workers.dev") or not hostname.startswith(("nerudo-evidence-", "nerudo-scale-")):
            parser.error("Mutating workflows require a dedicated nerudo-evidence-* or nerudo-scale-* Worker URL")
    if args.manifest:
        manifest = json.loads(args.manifest.read_text())
    else:
        if len(set(args.providers)) != len(args.providers) or len(args.providers) < 3 or any(p not in PROVIDERS for p in args.providers):
            parser.error("Choose at least three distinct supported providers")
        if {"gemini", "antigravity"} <= set(args.providers):
            parser.error("Choose one Google client for the shared discount task")
        if {"hermes", "ollama"} <= set(args.providers):
            parser.error("Choose Hermes or Ollama for the shared receipt task")
        ready = doctor(args)
        missing = [p for p in args.providers if not ready[p]["ready"]]
        if missing:
            parser.error("Providers not ready: " + ", ".join(missing))
        manifest = prepare(args, settings)
        print("Manifest: " + str(Path(manifest["run_dir"]) / "manifest.json"), flush=True)
        print("Live repo: " + manifest["url"], flush=True)
    if args.action == "workflow":
        if args.provider not in manifest["providers"]:
            parser.error("Choose a provider configured in this manifest")
        result = run_one(manifest, args.provider, settings)
        print(json.dumps({"provider": result["provider"], "status": result.get("status"), "landed": result["landed"]}))
        return
    if args.action == "add":
        add_workflow(manifest, args.provider, settings)
        print("Added workflow: " + args.provider)
        return
    if args.action == "verify":
        result = verify_all(manifest, settings)
        if not result["success"]:
            sys.exit(1)
        return
    if args.action == "run":
        result = run_all(manifest, settings)
        if not result["success"]:
            sys.exit(1)


if __name__ == "__main__":
    main()
