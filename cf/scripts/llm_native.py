#!/usr/bin/env python3
"""Private adapters for Claude, Codex, Gemini, Antigravity and Hermes workflows.

The caller supplies an isolated workload checkout and the complete child
environment. Hermes can privately read the existing local runtime's key from
its metadata. This module never edits global CLI configuration, installs a
CLI, or decides whether an agent's change actually landed. A zero
exit code means only that the native workflow completed; the orchestrator must
verify its NERUDO receipt and the resulting repository.

CLI overrides may be executable paths or shell-split command strings, e.g.
NERUDO_GEMINI_BIN='npx --no-install @google/gemini-cli@<pinned-version>'.
These strings are executed as argument lists, never through a shell.
"""

from __future__ import annotations

import base64
from contextlib import contextmanager
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import subprocess
import tempfile
import time
from urllib.parse import quote, urlsplit


_SENSITIVE_NAME = re.compile(
    r"(?:^|_)(?:TOKEN|SECRET|PASSWORD|KEY|AUTH|AUTHORIZATION|CREDENTIALS?)(?:$|_)", re.I
)
_SECRET_PATTERN = re.compile(
    r"art_v\d+_[A-Za-z0-9]+(?:\?expires=\d+)?"
    r"|lsa1\.[A-Za-z0-9_]+\.\d+\.[0-9a-f]+"
    r"|(?:Bearer|Basic|token) [A-Za-z0-9+/=_.-]{16,}"
    r"|(?<=://)[^/@\s]+@"
)
_PROVIDERS = {"claude", "codex", "gemini", "antigravity", "hermes"}
_HERMES_MODEL = "Qwen3.8-Flash-Next-UD-IQ4_XS"
_HERMES_ENDPOINT = "http://127.0.0.1:18434/v1"
_HERMES_MIN_CONTEXT_LENGTH = 64000
_CLAUDE_TOOLS = "Read,Glob,Grep,Edit,Write,Bash,Agent"
_CLAUDE_ALLOWED = ",".join([
    "Read", "Glob", "Grep", "Edit", "Write", "Agent", "mcp__nerudo__*",
    "Bash(git *)", "Bash(python *)", "Bash(python3 *)", "Bash(pytest *)",
    "Bash(node *)", "Bash(npm test *)", "Bash(npm run *)",
])
_CLAUDE_AGENTS = {
    "evidence-planner": {
        "description": "Read-only planner for a coding task sharing main with other agents.",
        "prompt": (
            "Inspect the task and repository. Recommend the smallest correct change, "
            "tests and likely overlap with concurrent work. Do not edit, run commands, "
            "or call NERUDO mutation tools. Return your plan to the parent."
        ),
        "tools": ["Read", "Glob", "Grep"],
        "model": "inherit",
        "permissionMode": "dontAsk",
    },
    "evidence-reviewer": {
        "description": "Read-only reviewer of implementation correctness and concurrency recovery.",
        "prompt": (
            "Inspect the task, current files and tests. Look for incorrect behavior, "
            "lost concurrent changes and missing coverage. Do not edit, run commands, "
            "or call NERUDO mutation tools. Return concrete findings to the parent."
        ),
        "tools": ["Read", "Glob", "Grep"],
        "model": "inherit",
        "permissionMode": "dontAsk",
    },
}


def _secret_values(env: dict[str, str]) -> list[str]:
    values = set()
    for name, value in env.items():
        if value and (_SENSITIVE_NAME.search(name) or
                      (name.startswith("GIT_CONFIG_VALUE_") and "Authorization:" in value)):
            values.add(value)
            values.add(json.dumps(value, ensure_ascii=False)[1:-1])
            values.add(quote(value, safe=""))
            values.add(base64.b64encode(value.encode()).decode())
    return sorted(values, key=len, reverse=True)


def _redact(value: str, secrets: list[str]) -> str:
    for secret in secrets:
        value = value.replace(secret, "[REDACTED]")
    return _SECRET_PATTERN.sub("[REDACTED]", value)


def _write_private(path: Path, text: str) -> None:
    """Create a new owner-only file without following an existing symlink."""
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    flags |= getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(path, flags, 0o600)
    with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
        stream.write(text)


def _toml(value: object) -> str:
    # All values used here are JSON-compatible scalars/lists/inline tables.
    if isinstance(value, dict):
        return "{" + ",".join(f"{json.dumps(k)}={_toml(v)}" for k, v in value.items()) + "}"
    if isinstance(value, list):
        return "[" + ",".join(_toml(item) for item in value) + "]"
    return json.dumps(value, ensure_ascii=False)


def _mcp_env_names(env: dict[str, str]) -> list[str]:
    # Forward NERUDO credentials explicitly; unrelated model/API credentials
    # are not needed by the MCP proxy. Values remain in the process environment.
    return sorted(name for name in env if re.fullmatch(r"[A-Za-z_][A-Za-z_0-9]*", name) and (
        name.startswith(("NERUDO_", "GIT_", "PYTHON")) or
        name in {"PATH", "HOME", "TMPDIR", "HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY"}
    ) and not name.startswith("NERUDO_HERMES_"))


def _cli_command(provider: str, env: dict[str, str]) -> list[str] | None:
    override = env.get(f"NERUDO_{provider.upper()}_BIN")
    # Preserve spaces in an exact existing executable path before shell-splitting.
    if override and Path(override).is_file():
        command = [override]
    else:
        command = shlex.split(override) if override else ["agy" if provider == "antigravity" else provider]
    if not command:
        return None
    executable = shutil.which(command[0], path=env.get("PATH", os.defpath))
    if executable is None and not override and provider == "antigravity" and env.get("HOME"):
        home = Path(env["HOME"])
        for candidate in (home / ".local/bin/agy", home / ".cache/nerudo-concurrency/tools/antigravity/agy"):
            executable = shutil.which(str(candidate), path=env.get("PATH", os.defpath))
            if executable:
                break
    if executable is None and not override and provider == "hermes" and env.get("HOME"):
        executable = shutil.which(str(Path(env["HOME"]) / ".local/bin/hermes"),
                                  path=env.get("PATH", os.defpath))
    if executable is None and not override and provider == "gemini":
        # Use a prepared, pinned npm cache directly; never fetch/install while
        # a concurrency run is starting. This matches `npx --no-install`.
        cache = Path(env.get("HOME", "")) / ".npm" / "_npx"
        version = env.get("NERUDO_GEMINI_VERSION", "0.63.0")
        for package in sorted(cache.glob("*/node_modules/@google/gemini-cli/package.json")):
            try:
                metadata = json.loads(package.read_text())
            except (OSError, ValueError):
                continue
            if metadata.get("version") == version:
                candidate = package.parents[2] / ".bin" / "gemini"
                executable = shutil.which(str(candidate), path=env.get("PATH", os.defpath))
                if executable:
                    break
    if executable is None:
        return None
    return [executable, *command[1:]]


def _gemini_oauth_source(env: dict[str, str]) -> Path | None:
    home = env.get("GEMINI_CLI_HOME") or env.get("HOME")
    source = Path(home) / ".gemini" if home else None
    return source if source and (source / "oauth_creds.json").is_file() else None


def hermes_runtime_environment(env: dict[str, str]) -> dict[str, str]:
    """Read the existing runtime route/key privately, without starting a server.

    Call after managed startup so a rotated local key is current. Never print
    this dictionary: the adapter only passes it as a child process environment.
    """
    resolved = dict(env)
    if resolved.get("NERUDO_HERMES_API_KEY"):
        return resolved
    home = resolved.get("HERMES_HOME")
    if not home and resolved.get("HOME"):
        home = str(Path(resolved["HOME"]) / ".hermes")
    if not home:
        return resolved
    try:
        state = json.loads((Path(home) / "runtimes/llamacpp/server.json").read_text())
    except (OSError, ValueError):
        return resolved
    key, endpoint = state.get("api_key"), state.get("base_url")
    if not isinstance(key, str) or not key or not isinstance(endpoint, str):
        return resolved
    explicit_endpoint = resolved.get("NERUDO_HERMES_BASE_URL")
    if explicit_endpoint and explicit_endpoint.rstrip("/") != endpoint.rstrip("/"):
        return resolved
    resolved["NERUDO_HERMES_API_KEY"] = key
    resolved.setdefault("NERUDO_HERMES_BASE_URL", endpoint)
    return resolved


def _workspace_permissions(extra_write: Path | None = None) -> dict:
    filesystem = {":workspace_roots": {".": "write", ".git": "write"}}
    if extra_write:
        filesystem[str(extra_write)] = "write"
    return {"extends": ":workspace", "filesystem": filesystem, "network": {"enabled": True}}


def _antigravity_permissions(env: dict[str, str], run_dir: Path,
                            evidence_dir: Path | None = None) -> dict:
    permissions = _workspace_permissions(run_dir)
    if env.get("HOME"):
        home = Path(env["HOME"]).resolve()
        # The language server stores each conversation and its working memory
        # here even in headless mode. Grant these data directories, never the
        # parent containing global settings/MCP/permission configuration.
        session_root = home / ".gemini" / "antigravity-cli"
        for name in ("brain", "conversations", "annotations", "implicit", "presence", "crashes", "cache"):
            permissions["filesystem"][str(session_root / name)] = "write"
        permissions["filesystem"][str(session_root / "mcp" / "nerudo")] = "write"
        if env.get("NERUDO_TRUNK") and env.get("NERUDO_AGENT"):
            # Match the Rust CLI's path normalization. Parent directories must
            # be primed outside the sandbox; other agents' key files stay read-only.
            part = lambda value: re.sub(r"[^A-Za-z0-9_-]", "_", value)
            key_file = home / ".nerudo" / "agents" / part(env["NERUDO_TRUNK"]) / part(env["NERUDO_AGENT"])
            permissions["filesystem"][str(key_file)] = "write"
    if evidence_dir:
        # The supplied proxy appends these exact evidence files outside the
        # checkout/native log directory. Other providers' files stay protected.
        permissions["filesystem"][str(evidence_dir / "events" / "antigravity.jsonl")] = "write"
        permissions["filesystem"][str(evidence_dir / "ready" / "antigravity.json")] = "write"
    return permissions


def _antigravity_evidence_dir(mcp_command: list[str]) -> Path | None:
    try:
        manifest = Path(mcp_command[mcp_command.index("--manifest") + 1]).resolve()
    except (ValueError, IndexError):
        return None
    return manifest.parent if manifest.name == "manifest.json" else None


def _prepare_antigravity_directories(env: dict[str, str], evidence_dir: Path | None) -> None:
    if env.get("HOME"):
        root = Path(env["HOME"]) / ".gemini" / "antigravity-cli"
        for relative in ("brain", "conversations", "annotations", "implicit", "presence",
                         "crashes", "cache", "mcp/nerudo"):
            (root / relative).mkdir(parents=True, mode=0o700, exist_ok=True)
    if evidence_dir:
        for name in ("events", "ready"):
            (evidence_dir / name).mkdir(mode=0o700, exist_ok=True)


@contextmanager
def _project_mcp(provider: str, workspace: Path, run_dir: Path):
    """Install Antigravity's temporary local config, excluded from commits."""
    if provider != "antigravity":
        yield
        return
    directory = workspace / ".agents"
    config = directory / "mcp_config.json"
    exclude = workspace / ".git/info/exclude"
    if directory.is_symlink() or config.exists() or config.is_symlink() or exclude.is_symlink():
        raise ValueError("Antigravity needs a fresh checkout with no existing .agents/mcp_config.json")
    if not (workspace / ".git").is_dir():
        raise ValueError("Antigravity needs a standalone Git clone for temporary configuration")
    original = exclude.read_bytes() if exclude.exists() else None
    original_mode = exclude.stat().st_mode & 0o777 if exclude.exists() else 0o600
    created_directory = not directory.exists()
    directory.mkdir(mode=0o700, exist_ok=True)
    try:
        _write_private(config, (run_dir / "antigravity-mcp.json").read_text())
        exclude.parent.mkdir(exist_ok=True)
        exclude.write_bytes((original or b"") + b"\n/.agents/mcp_config.json\n")
        yield
    finally:
        config.unlink(missing_ok=True)
        if original is None:
            exclude.unlink(missing_ok=True)
        else:
            exclude.write_bytes(original)
            exclude.chmod(original_mode)
        if created_directory:
            try:
                directory.rmdir()
            except OSError:
                pass


def _invocation(provider: str, executable: list[str], workspace: Path,
                mcp_command: list[str], env: dict[str, str], run_dir: Path,
                model: str | None, timeout: int = 900) -> tuple[list[str], dict[str, str], list[Path]]:
    child_env = dict(env)
    child_env.update({"NO_COLOR": "1", "GIT_TERMINAL_PROMPT": "0"})
    forwarded_names = _mcp_env_names(env)
    server = {"command": mcp_command[0], "args": mcp_command[1:], "cwd": str(workspace)}
    configs: list[Path] = []
    if provider == "claude":
        config = run_dir / "mcp.json"
        # Claude's stdio servers inherit the explicitly supplied environment.
        _write_private(config, json.dumps({"mcpServers": {"nerudo": server}}, indent=2) + "\n")
        agents = run_dir / "agents.json"
        _write_private(agents, json.dumps(_CLAUDE_AGENTS, indent=2) + "\n")
        configs.extend([config, agents])
        command = [*executable, "--print", "--output-format", "stream-json", "--verbose",
                   "--permission-mode", "dontAsk", "--permission-prompts", "none",
                   "--tools", _CLAUDE_TOOLS, "--allowedTools", _CLAUDE_ALLOWED,
                   "--mcp-config", str(config), "--strict-mcp-config",
                   "--setting-sources", "", "--no-session-persistence", "--no-chrome",
                   "--agents", str(agents), "--forward-subagent-text"]
        if model:
            command += ["--model", model]
    elif provider == "codex":
        server.update({"env_vars": forwarded_names, "required": True,
                       "startup_timeout_sec": 30, "tool_timeout_sec": 900,
                       # The supplied server is the orchestrator's scoped
                       # evidence proxy. Preapprove only this server's tools;
                       # never-ask otherwise soft-denies every MCP call.
                       "default_tools_approval_mode": "approve"})
        # Ignore global configuration while retaining the CLI's existing login.
        # A workspace profile explicitly grants this checkout's .git writes;
        # ordinary workspace-write protects .git and can prevent commits.
        permissions = _workspace_permissions()
        command = [*executable, "exec", "--ignore-user-config", "--ignore-rules",
                   "--ephemeral", "--json", "--color", "never",
                   "--cd", str(workspace), "-c", 'approval_policy="never"',
                   "-c", 'default_permissions="nerudo-evidence"',
                   "-c", "permissions.nerudo-evidence=" + _toml(permissions),
                   "-c", "log_dir=" + _toml(str(run_dir / "codex-log")),
                   "-c", "mcp_servers=" + _toml({"nerudo": server}), "-"]
        if model:
            command[-1:-1] = ["--model", model]
    elif provider == "antigravity":
        config = run_dir / "antigravity-mcp.json"
        _write_private(config, json.dumps({"mcpServers": {"nerudo": server}}, indent=2) + "\n")
        configs.append(config)
        sandbox = _cli_command("codex", env)
        evidence_dir = _antigravity_evidence_dir(mcp_command)
        _prepare_antigravity_directories(env, evidence_dir)
        # Antigravity has no documented per-run scoped permission settings.
        # Keep the entire process (and its MCP children) in a bounded OS
        # sandbox instead of editing global permissions or giving full access.
        command = [*sandbox, "sandbox", "--permission-profile", "nerudo-evidence",
                   "-c", "permissions.nerudo-evidence=" + _toml(_antigravity_permissions(env, run_dir, evidence_dir)),
                   "--cd", str(workspace), *executable, "--input-format", "stream-json",
                   "--output-format", "stream-json", "--mode", "accept-edits",
                   "--dangerously-skip-permissions", "--log-file", str(run_dir / "antigravity.log")]
        if model:
            command += ["--model", model]
    elif provider == "hermes":
        endpoint = env.get("NERUDO_HERMES_BASE_URL", _HERMES_ENDPOINT)
        url = urlsplit(endpoint)
        if (url.scheme not in {"http", "https"} or url.hostname not in {"localhost", "127.0.0.1", "::1"}
                or url.username or url.password or url.query or url.fragment):
            raise ValueError("Hermes requires a credential-free loopback URL for the existing llama.cpp server")
        private_home = run_dir / "hermes-home"
        private_home.mkdir(mode=0o700)
        config = private_home / "config.yaml"
        # 'custom' calls the existing OpenAI-compatible endpoint directly;
        # the managed 'llamacpp' alias could otherwise launch another runtime.
        server.update({"env": {name: "${" + name + "}" for name in forwarded_names},
                       "timeout": 900, "connect_timeout": 30, "trust": "full",
                       "supports_parallel_tool_calls": False,
                       "tools": {"resources": False, "prompts": False}})
        settings = {
            "model": {"default": model, "provider": "custom", "base_url": endpoint,
                      "api_mode": "chat_completions", "key_env": "NERUDO_HERMES_API_KEY"},
            "agent": {"max_turns": 120, "run_budget_seconds": timeout},
            "local_runtime": {"enabled": False},
            "mcp_discovery_timeout": 30,
            "mcp_single_query_discovery_timeout": 30,
            "tools": {"tool_search": {"enabled": "off"}},
            "terminal": {"backend": "local", "cwd": str(workspace)},
            "delegation": {"max_iterations": 25, "max_concurrent_children": 1,
                           "oneshot_max_children": 2, "max_spawn_depth": 1,
                           "orchestrator_enabled": False, "worktree_isolation": False},
            "mcp_servers": {"nerudo": server},
        }
        if env.get("NERUDO_HERMES_CONTEXT_LENGTH"):
            context_length = int(env["NERUDO_HERMES_CONTEXT_LENGTH"])
            if context_length < _HERMES_MIN_CONTEXT_LENGTH:
                raise ValueError("Hermes requires at least 64,000 context tokens; "
                                 "match the existing server's actual context capacity")
            settings["model"]["context_length"] = context_length
        # JSON is valid YAML, avoiding a new adapter dependency. No key values
        # or global settings are copied into this private profile.
        _write_private(config, json.dumps(settings, indent=2) + "\n")
        configs.append(config)
        for name in ("HERMES_PROFILE", "HERMES_CONFIG", "HERMES_ENV", "HERMES_CONFIG_PATH",
                     "HERMES_ENV_PATH", "HERMES_IGNORE_USER_CONFIG", "HERMES_SAFE_MODE"):
            child_env.pop(name, None)
        child_env.update({"HERMES_HOME": str(private_home), "HERMES_INFERENCE_MODEL": str(model),
                          "HERMES_INFERENCE_PROVIDER": "custom"})
        command = [*executable, "chat", "--query-file", "-", "--oneshot", "--format", "stream-json",
                   "--in", str(workspace), "--provider", "custom", "--model", str(model),
                   # Hermes's discovery filter uses the configured server name;
                   # registration later resolves this bare alias to mcp-nerudo.
                   "--toolsets", "nerudo,delegation", "--ignore-rules", "--source", "tool",
                   "--max-turns", "120", "--run-budget", str(timeout)]
    else:
        # Gemini sanitizes inherited MCP environments, so explicitly reference
        # only the required variables. The file never contains their values.
        server.update({"env": {name: "${" + name + "}" for name in forwarded_names},
                       "trust": True, "timeout": 900000})
        config = run_dir / "gemini-settings.json"
        settings = {
            "mcpServers": {"nerudo": server}, "mcp": {"allowed": ["nerudo"]},
            "general": {"enableAutoUpdate": False, "enableAutoUpdateNotification": False},
            "advanced": {"ignoreLocalEnv": True},
            "telemetry": {"enabled": False},
            "privacy": {"usageStatisticsEnabled": False},
        }
        # This adapter deliberately requires headless environment authentication.
        # Interactive Google login remains a user-operated setup step.
        if env.get("GEMINI_API_KEY"):
            settings["security"] = {"auth": {"selectedType": "gemini-api-key"}}
        elif env.get("GOOGLE_GENAI_USE_VERTEXAI", "").lower() in {"1", "true"}:
            settings["security"] = {"auth": {"selectedType": "vertex-ai"}}
        elif _gemini_oauth_source(env):
            settings["security"] = {"auth": {"selectedType": "oauth-personal"}}
        _write_private(config, json.dumps(settings, indent=2) + "\n")
        configs.append(config)
        private_home = run_dir / "gemini-home"
        private_home.mkdir(mode=0o700)
        oauth_source = _gemini_oauth_source(env)
        if settings.get("security", {}).get("auth", {}).get("selectedType") == "oauth-personal":
            auth_dir = private_home / ".gemini"
            auth_dir.mkdir(mode=0o700)
            for name in ("oauth_creds.json", "google_accounts.json"):
                source = oauth_source / name
                if source.is_file():
                    # Copy bytes without parsing or exposing credential values.
                    descriptor = os.open(auth_dir / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
                    with os.fdopen(descriptor, "wb") as target, source.open("rb") as original:
                        shutil.copyfileobj(original, target)
        child_env.update({"GEMINI_CLI_HOME": str(private_home),
                          "GEMINI_CLI_SYSTEM_SETTINGS_PATH": str(config),
                          "GEMINI_CLI_TRUST_WORKSPACE": "true",
                          "GEMINI_TELEMETRY_ENABLED": "false"})
        command = [*executable, "--prompt", "", "--output-format", "stream-json", "--approval-mode", "auto_edit",
                   "--allowed-mcp-server-names", "nerudo", "--extensions", "none",
                   "--allowed-tools", "ShellTool(git),ShellTool(python),"
                   "ShellTool(python3),ShellTool(pytest),ShellTool(node)"]
        if model:
            command += ["--model", model]
        # An empty explicit prompt selects headless mode; the real task is stdin.
    return command, child_env, configs


def _observed_model(stdout: str) -> str | None:
    for line in stdout.splitlines():
        try:
            event = json.loads(line)
        except (ValueError, TypeError):
            continue
        if isinstance(event, dict) and isinstance(event.get("model"), str):
            return event["model"]
        if isinstance(event, dict) and isinstance(event.get("init"), dict):
            if isinstance(event["init"].get("model"), str):
                return event["init"]["model"]
    return None


def _stop_process_group(process: subprocess.Popen) -> tuple[bytes, bytes]:
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        return process.communicate(timeout=5)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        return process.communicate()


def _sanitize_private_state(run_dir: Path, secrets: list[str]) -> None:
    """Constrain and redact any Gemini-created session/debug files as well."""
    for path in run_dir.rglob("*"):
        if path.is_symlink():
            continue
        if path.is_dir():
            path.chmod(0o700)
        elif path.is_file():
            path.chmod(0o600)
            try:
                original = path.read_text(encoding="utf-8")
            except (UnicodeError, OSError):
                continue
            sanitized = _redact(original, secrets)
            if sanitized != original:
                path.write_text(sanitized, encoding="utf-8")


def run_native(provider: str, workspace: Path, prompt: str, mcp_command: list[str],
               env: dict[str, str], output_dir: Path, timeout: int = 900,
               model: str | None = None) -> dict:
    """Run one native workflow; return private logs plus truthful execution status.

    Supported statuses are ``completed``, ``failed``, ``timeout`` and ``blocked``.
    An absent CLI (or Gemini headless credentials) returns ``blocked`` without
    inference. Credentials must stay in ``env``, never in ``mcp_command``.
    """
    if provider not in _PROVIDERS:
        raise ValueError(f"unsupported native provider: {provider}")
    if provider == "hermes":
        env = hermes_runtime_environment(env)
        model = model or env.get("NERUDO_HERMES_MODEL", _HERMES_MODEL)
    workspace = Path(workspace).resolve(strict=True)
    if not workspace.is_dir() or not mcp_command or timeout <= 0:
        raise ValueError("a checkout, nonempty MCP command and positive timeout are required")
    if not all(isinstance(item, str) and item for item in mcp_command):
        raise ValueError("MCP command must contain nonempty string arguments")
    secrets = _secret_values(env)
    if any(secret in argument for secret in secrets for argument in mcp_command):
        raise ValueError("MCP command contains a credential; supply it through env")
    output_dir = Path(output_dir).resolve()
    if output_dir == workspace or workspace in output_dir.parents:
        raise ValueError("private native logs must be outside the workload checkout")
    output_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
    run_dir = Path(tempfile.mkdtemp(prefix=f"{provider}-", dir=output_dir))
    stdout_path, stderr_path = run_dir / "stdout.jsonl", run_dir / "stderr.log"
    started = time.monotonic()
    result = {"provider": provider, "status": "blocked", "exit_code": None,
              "model": model, "requested_model": model, "observed_model": None,
              "runtime_seconds": 0.0, "runtime": 0.0, "output_dir": str(run_dir),
              "stdout_path": str(stdout_path), "stderr_path": str(stderr_path),
              "log_paths": {"stdout": str(stdout_path), "stderr": str(stderr_path)}}
    command = _cli_command(provider, env)
    if command is None:
        detail = f"{provider} CLI not found; set NERUDO_{provider.upper()}_BIN to a prepared executable"
    elif provider == "gemini" and not (env.get("GEMINI_API_KEY") or
          env.get("GOOGLE_GENAI_USE_VERTEXAI", "").lower() in {"1", "true"} or
          _gemini_oauth_source(env)):
        detail = "Gemini needs its existing Google login, GEMINI_API_KEY or configured Vertex AI credentials"
    elif provider == "antigravity" and _cli_command("codex", env) is None:
        detail = "Antigravity needs the installed Codex sandbox helper for isolated noninteractive permissions"
    elif provider == "hermes" and not env.get("NERUDO_HERMES_API_KEY"):
        detail = "Hermes needs NERUDO_HERMES_API_KEY in env for the existing local llama.cpp endpoint"
    else:
        detail = ""
    if detail:
        result["error"] = detail
        _write_private(stdout_path, "")
        _write_private(stderr_path, detail + "\n")
        return result
    command, child_env, configs = _invocation(
        provider, command, workspace, mcp_command, env, run_dir, model, timeout)
    if provider == "hermes":
        result["subagent_readonly_policy"] = "prompt_only; native children inherit the parent MCP write tools"
        result["inference_endpoint"] = env.get("NERUDO_HERMES_BASE_URL", _HERMES_ENDPOINT)
        result["runtime_management"] = "existing endpoint only; no server launch or model loading"
        prompt += (
            "\nHermes workflow: use native delegate_task for one planner before coding and one reviewer "
            "after the implementation, sequentially (one child at a time). Both must only inspect files "
            "through list_files/read_file/git_status/git_diff and return recommendations. They must not "
            "write, commit, sync, claim, comment or land. Only the parent performs those actions. "
            "This read-only restriction is prompt policy: Hermes children inherit the parent's MCP "
            "catalog, so do not describe their tools as technically read-only. All model requests use "
            "the already-running local endpoint; never launch or manage an inference server.\n"
        )
    invocation_path = run_dir / "invocation.json"
    _write_private(invocation_path, _redact(json.dumps({
        "provider": provider, "argv": command, "cwd": str(workspace),
        "prompt_source": "stdin-json" if provider == "antigravity" else "stdin",
        "model": model, "timeout_seconds": timeout,
        "mcp_environment_names": _mcp_env_names(env),
        "subagents_available": list(_CLAUDE_AGENTS) if provider == "claude" else [],
    }, indent=2) + "\n", secrets))
    result["log_paths"].update({"invocation": str(invocation_path),
                               "configs": [str(path) for path in configs]})
    stdout, stderr = b"", b""
    try:
        with _project_mcp(provider, workspace, run_dir):
            process = subprocess.Popen(command, cwd=workspace, env=child_env,
                                       stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                       stderr=subprocess.PIPE, start_new_session=True)
            input_data = prompt.encode() if provider != "antigravity" else (
                json.dumps({"event": "user", "message": {"content": prompt}}) + "\n").encode()
            try:
                stdout, stderr = process.communicate(input=input_data, timeout=timeout)
                result["status"] = "completed" if process.returncode == 0 else "failed"
            except subprocess.TimeoutExpired:
                stdout, stderr = _stop_process_group(process)
                result["status"] = "timeout"
                result["error"] = f"native workflow exceeded {timeout} seconds"
            result["exit_code"] = process.returncode
    except (OSError, ValueError) as error:
        result["status"] = "blocked" if isinstance(error, ValueError) else "failed"
        result["error"] = _redact(str(error), secrets)
        stderr = str(error).encode()
    finally:
        result["runtime_seconds"] = result["runtime"] = round(time.monotonic() - started, 3)
        stdout_text = stdout.decode("utf-8", errors="replace")
        result["observed_model"] = _observed_model(stdout_text)
        result["model"] = result["observed_model"] or model
        _write_private(stdout_path, _redact(stdout_text, secrets))
        _write_private(stderr_path, _redact(stderr.decode("utf-8", errors="replace"), secrets))
        if provider == "gemini":
            for name in ("oauth_creds.json", "google_accounts.json"):
                (run_dir / "gemini-home" / ".gemini" / name).unlink(missing_ok=True)
        _sanitize_private_state(run_dir, secrets)
    return result
