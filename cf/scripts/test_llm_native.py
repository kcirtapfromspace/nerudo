#!/usr/bin/env python3
"""Offline adapter tests: these never invoke a model or a live NERUDO server."""

import json
import os
from pathlib import Path
import shlex
import stat
import sys
import tempfile
import time
import unittest

from llm_native import _antigravity_permissions, hermes_runtime_environment, run_native


FAKE_CLI = '''
import json, os, pathlib, subprocess, sys, time
prompt = sys.stdin.read()
if os.environ.get("FAKE_SLEEP"):
    child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
    pathlib.Path(os.environ["FAKE_CHILD_PID"]).write_text(str(child.pid))
    print("started", flush=True)
    time.sleep(60)
config_path = os.environ.get("GEMINI_CLI_SYSTEM_SETTINGS_PATH")
if "--mcp-config" in sys.argv:
    config_path = sys.argv[sys.argv.index("--mcp-config") + 1]
if pathlib.Path(".agents/mcp_config.json").is_file():
    config_path = ".agents/mcp_config.json"
if os.environ.get("HERMES_HOME"):
    config_path = str(pathlib.Path(os.environ["HERMES_HOME"]) / "config.yaml")
config = json.loads(pathlib.Path(config_path).read_text()) if config_path else None
if os.environ.get("GEMINI_CLI_HOME"):
    state = pathlib.Path(os.environ["GEMINI_CLI_HOME"]) / "session.json"
    state.write_text(json.dumps({"secret": os.environ["NERUDO_AGENT_TOKEN"]}))
oauth = pathlib.Path(os.environ.get("GEMINI_CLI_HOME", "/nonexistent")) / ".gemini/oauth_creds.json"
print(json.dumps({"type":"init", "model":"fake-test-model", "argv":sys.argv[1:],
                  "config":config, "prompt":prompt, "token":os.environ["NERUDO_AGENT_TOKEN"],
                  "author":os.environ["GIT_AUTHOR_NAME"], "oauth_private_exists":oauth.is_file()}))
print(os.environ["NERUDO_AGENT_TOKEN"], file=sys.stderr)
if os.environ.get("NERUDO_HERMES_API_KEY"):
    print(os.environ["NERUDO_HERMES_API_KEY"], file=sys.stderr)
sys.exit(int(os.environ.get("FAKE_EXIT", "0")))
'''


class NativeAdapterTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="nerudo-native-test-")
        self.root = Path(self.scratch.name).resolve()
        self.workspace = self.root / "checkout"
        self.workspace.mkdir()
        self.output = self.root / "private"
        self.fake = self.root / "fake cli.py"
        self.fake.write_text(FAKE_CLI)
        self.env = {"PATH": os.environ["PATH"], "HOME": str(self.root),
                    "NERUDO_AGENT_TOKEN": 'fake-token-with-quote-"-123456789',
                    "NERUDO_AGENT_KEY": "fake-agent-key-123456789",
                    "GIT_AUTHOR_NAME": "agent", "GIT_AUTHOR_EMAIL": "agent@example.invalid"}
        self.fake_command = shlex.join([sys.executable, str(self.fake)])
        for provider in ("claude", "codex", "gemini", "antigravity", "hermes"):
            self.env[f"NERUDO_{provider.upper()}_BIN"] = self.fake_command

    def tearDown(self):
        self.scratch.cleanup()

    def run_provider(self, provider, **kwargs):
        return run_native(provider, self.workspace, "implement the assigned task",
                          [sys.executable, "stdio_proxy.py"], kwargs.pop("env", self.env),
                          self.output, **kwargs)

    def assert_private_and_redacted(self, result):
        root = Path(result["output_dir"])
        self.assertEqual(stat.S_IMODE(root.stat().st_mode), 0o700)
        for path in root.rglob("*"):
            if path.is_file():
                self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
                text = path.read_text()
                self.assertNotIn(self.env["NERUDO_AGENT_TOKEN"], text)
                self.assertNotIn(json.dumps(self.env["NERUDO_AGENT_TOKEN"])[1:-1], text)
                self.assertNotIn(self.env["NERUDO_AGENT_KEY"], text)

    def test_claude_scoped_mcp_subagents_and_redaction(self):
        result = self.run_provider("claude", model="requested-test-model")
        self.assertEqual((result["status"], result["exit_code"]), ("completed", 0))
        self.assertEqual(result["model"], "fake-test-model")
        output = json.loads(Path(result["stdout_path"]).read_text())
        self.assertEqual(output["author"], "agent")  # AUTHOR is not an auth token.
        self.assertEqual(output["token"], "[REDACTED]")
        argv = output["argv"]
        for flag in ("--strict-mcp-config", "--no-session-persistence", "--agents"):
            self.assertIn(flag, argv)
        self.assertEqual(argv[argv.index("--permission-mode") + 1], "dontAsk")
        self.assertEqual(output["config"]["mcpServers"]["nerudo"]["cwd"], str(self.workspace))
        agents = json.loads((Path(result["output_dir"]) / "agents.json").read_text())
        self.assertEqual(set(agents), {"evidence-planner", "evidence-reviewer"})
        for agent in agents.values():
            self.assertEqual(set(agent["tools"]), {"Read", "Glob", "Grep"})
        self.assert_private_and_redacted(result)

    def test_codex_bounded_config_and_environment_names_only(self):
        result = self.run_provider("codex")
        self.assertEqual(result["status"], "completed")
        output = json.loads(Path(result["stdout_path"]).read_text())
        argv = output["argv"]
        self.assertIn("--ignore-user-config", argv)
        self.assertIn("--ephemeral", argv)
        self.assertNotIn("--dangerously-bypass-approvals-and-sandbox", argv)
        config = next(item for item in argv if item.startswith("permissions.nerudo-evidence="))
        self.assertIn('".git"="write"', config)
        self.assertIn('"enabled"=true', config)
        mcp = next(item for item in argv if item.startswith("mcp_servers="))
        self.assertIn("NERUDO_AGENT_TOKEN", mcp)
        self.assertIn('"default_tools_approval_mode"="approve"', mcp)
        self.assertTrue(mcp.startswith('mcp_servers={"nerudo"='))
        self.assertNotIn(self.env["NERUDO_AGENT_TOKEN"], mcp)
        self.assert_private_and_redacted(result)

    def test_gemini_ephemeral_config_references_and_state_redaction(self):
        env = {**self.env, "GEMINI_API_KEY": "fake-gemini-key-123456789"}
        result = self.run_provider("gemini", env=env)
        self.assertEqual(result["status"], "completed")
        output = json.loads(Path(result["stdout_path"]).read_text())
        config = output["config"]
        self.assertEqual(config["mcpServers"]["nerudo"]["env"]["NERUDO_AGENT_TOKEN"],
                         "${NERUDO_AGENT_TOKEN}")
        self.assertEqual(config["security"]["auth"]["selectedType"], "gemini-api-key")
        self.assertTrue(config["advanced"]["ignoreLocalEnv"])
        self.assert_private_and_redacted(result)

    def test_missing_gemini_auth_is_blocked_without_starting_cli(self):
        result = self.run_provider("gemini")
        self.assertEqual(result["status"], "blocked")
        self.assertIsNone(result["exit_code"])
        self.assertEqual(Path(result["stdout_path"]).read_text(), "")
        self.assertIn("GEMINI_API_KEY", result["error"])

    def test_gemini_uses_private_existing_oauth_copy_then_removes_it(self):
        source = self.root / ".gemini/oauth_creds.json"
        source.parent.mkdir()
        credential = '{"refresh_token":"private-oauth-token-not-in-environment"}'
        source.write_text(credential)
        result = self.run_provider("gemini")
        self.assertEqual(result["status"], "completed")
        output = json.loads(Path(result["stdout_path"]).read_text())
        self.assertTrue(output["oauth_private_exists"])
        self.assertEqual(output["config"]["security"]["auth"]["selectedType"], "oauth-personal")
        self.assertEqual(source.read_text(), credential)
        self.assertFalse((Path(result["output_dir"]) / "gemini-home/.gemini/oauth_creds.json").exists())
        for path in Path(result["output_dir"]).rglob("*"):
            if path.is_file():
                self.assertNotIn("private-oauth-token-not-in-environment", path.read_text())
        self.assert_private_and_redacted(result)

    def test_antigravity_scoped_config_is_excluded_and_removed(self):
        info = self.workspace / ".git/info"
        info.mkdir(parents=True)
        exclude = info / "exclude"
        exclude.write_text("existing-pattern\n")
        result = self.run_provider("antigravity", model="requested-test-model")
        self.assertEqual(result["status"], "completed")
        output = json.loads(Path(result["stdout_path"]).read_text())
        self.assertIn("--permission-profile", output["argv"])
        self.assertIn("--log-file", output["argv"])
        self.assertIn("--input-format", output["argv"])
        self.assertEqual(json.loads(output["prompt"])["message"]["content"], "implement the assigned task")
        self.assertEqual(set(output["config"]["mcpServers"]), {"nerudo"})
        self.assertEqual(exclude.read_text(), "existing-pattern\n")
        self.assertFalse((self.workspace / ".agents").exists())
        self.assert_private_and_redacted(result)

    def test_antigravity_preserves_existing_project_config(self):
        directory = self.workspace / ".agents"
        directory.mkdir()
        config = directory / "mcp_config.json"
        config.write_text('{"existing":"config"}')
        result = self.run_provider("antigravity")
        self.assertEqual(result["status"], "blocked")
        self.assertIn("fresh checkout", result["error"])
        self.assertEqual(config.read_text(), '{"existing":"config"}')

    def test_antigravity_only_grants_session_data_and_its_own_key_file(self):
        permissions = _antigravity_permissions(
            {**self.env, "NERUDO_TRUNK": "org/repo", "NERUDO_AGENT": "antigravity"},
            self.output, self.root / "evidence")
        filesystem = permissions["filesystem"]
        self.assertEqual(filesystem[str(self.root / ".gemini/antigravity-cli/brain")], "write")
        self.assertEqual(filesystem[str(self.root / ".gemini/antigravity-cli/conversations")], "write")
        self.assertEqual(filesystem[str(self.root / ".gemini/antigravity-cli/mcp/nerudo")], "write")
        self.assertEqual(filesystem[str(self.root / "evidence/events/antigravity.jsonl")], "write")
        self.assertEqual(filesystem[str(self.root / "evidence/ready/antigravity.json")], "write")
        self.assertEqual(filesystem[str(self.root / ".nerudo/agents/org_repo/antigravity")], "write")
        for protected in (".gemini", ".gemini/antigravity-cli", ".gemini/config",
                          ".gemini/antigravity-cli/mcp", ".gemini/antigravity-cli/mcp/other-server",
                          ".nerudo/agents", ".nerudo/agents/org_repo",
                          ".nerudo/agents/org_repo/claude", "evidence/events/codex.jsonl"):
            self.assertNotIn(str(self.root / protected), filesystem)

    def test_hermes_private_profile_existing_endpoint_and_native_delegation(self):
        key = "fake-llamacpp-key-private-123456789"
        result = self.run_provider("hermes", env={**self.env, "NERUDO_HERMES_API_KEY": key,
                                                 "NERUDO_HERMES_CONTEXT_LENGTH": "65536"})
        self.assertEqual(result["status"], "completed")
        output = json.loads(Path(result["stdout_path"]).read_text())
        config = output["config"]
        self.assertEqual(config["model"]["default"], "Qwen3.8-Flash-Next-UD-IQ4_XS")
        self.assertEqual(config["model"]["provider"], "custom")
        self.assertEqual(config["model"]["base_url"], "http://127.0.0.1:18434/v1")
        self.assertEqual(config["model"]["key_env"], "NERUDO_HERMES_API_KEY")
        self.assertEqual(config["model"]["context_length"], 65536)
        self.assertFalse(config["local_runtime"]["enabled"])
        self.assertEqual(config["delegation"]["max_concurrent_children"], 1)
        self.assertEqual(config["delegation"]["oneshot_max_children"], 2)
        self.assertNotIn("NERUDO_HERMES_API_KEY", config["mcp_servers"]["nerudo"]["env"])
        argv = output["argv"]
        self.assertEqual(argv[argv.index("--toolsets") + 1], "nerudo,delegation")
        self.assertEqual(config["mcp_single_query_discovery_timeout"], 30)
        self.assertEqual(config["tools"]["tool_search"]["enabled"], "off")
        self.assertEqual(argv[argv.index("--query-file") + 1], "-")
        self.assertIn("delegate_task", output["prompt"])
        self.assertIn("prompt_only", result["subagent_readonly_policy"])
        for path in Path(result["output_dir"]).rglob("*"):
            if path.is_file(): self.assertNotIn(key, path.read_text())
        self.assert_private_and_redacted(result)

    def test_hermes_rejects_context_below_native_minimum_before_starting(self):
        with self.assertRaisesRegex(ValueError, "at least 64,000"):
            self.run_provider("hermes", env={**self.env, "NERUDO_HERMES_API_KEY": "fake-key",
                                             "NERUDO_HERMES_CONTEXT_LENGTH": "16384"})

    def test_hermes_missing_local_runtime_key_blocks_without_inference(self):
        result = self.run_provider("hermes")
        self.assertEqual(result["status"], "blocked")
        self.assertIn("NERUDO_HERMES_API_KEY", result["error"])

    def test_hermes_privately_reads_fresh_runtime_key_without_mutating_input(self):
        state = self.root / ".hermes/runtimes/llamacpp/server.json"
        state.parent.mkdir(parents=True)
        key = "runtime-key-after-restart-private"
        state.write_text(json.dumps({"api_key": key, "base_url": "http://127.0.0.1:18434/v1"}))
        resolved = hermes_runtime_environment(self.env)
        self.assertEqual(resolved["NERUDO_HERMES_API_KEY"], key)
        self.assertNotIn("NERUDO_HERMES_API_KEY", self.env)
        result = self.run_provider("hermes")
        self.assertEqual(result["status"], "completed")
        for path in Path(result["output_dir"]).rglob("*"):
            if path.is_file(): self.assertNotIn(key, path.read_text())
        mismatch = hermes_runtime_environment({**self.env, "NERUDO_HERMES_BASE_URL": "http://127.0.0.1:9999/v1"})
        self.assertNotIn("NERUDO_HERMES_API_KEY", mismatch)

    def test_nonzero_exit_is_failed(self):
        result = self.run_provider("codex", env={**self.env, "FAKE_EXIT": "7"})
        self.assertEqual((result["status"], result["exit_code"]), ("failed", 7))
        self.assert_private_and_redacted(result)

    def test_timeout_kills_mcp_children_and_keeps_partial_output(self):
        child_pid = self.root / "child.pid"
        result = self.run_provider("claude", timeout=1,
                                   env={**self.env, "FAKE_SLEEP": "1", "FAKE_CHILD_PID": str(child_pid)})
        self.assertEqual(result["status"], "timeout")
        self.assertIn("started", Path(result["stdout_path"]).read_text())
        pid = int(child_pid.read_text())
        # A briefly unreaped zombie is already terminated; process state makes
        # this portable on Unix without requiring the child's parent to reap it.
        import subprocess
        for _ in range(20):
            state = subprocess.run(["ps", "-p", str(pid), "-o", "stat="],
                                   capture_output=True, text=True).stdout.strip()
            if not state or state.startswith("Z"):
                break
            time.sleep(0.05)
        self.assertTrue(not state or state.startswith("Z"), state)
        self.assert_private_and_redacted(result)

    def test_credentials_in_command_and_logs_in_checkout_are_refused(self):
        with self.assertRaisesRegex(ValueError, "credential"):
            run_native("codex", self.workspace, "task", ["mcp", self.env["NERUDO_AGENT_TOKEN"]],
                       self.env, self.output)
        with self.assertRaisesRegex(ValueError, "outside"):
            run_native("codex", self.workspace, "task", ["mcp"], self.env,
                       self.workspace / "public-logs")


if __name__ == "__main__":
    unittest.main()
