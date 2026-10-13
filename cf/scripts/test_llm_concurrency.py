"""Checks for evidence integrity and the harness's scoped file tools."""
import tempfile
import json
from pathlib import Path
import unittest
import os
import subprocess
import sys
from unittest.mock import patch

from llm_concurrency import ROOT, Tools, cli_path, max_overlap, mcp_response, redact, variables


class EvidenceTests(unittest.TestCase):
    def test_cli_path_honors_explicit_binary_before_cargo_target(self):
        self.assertEqual(cli_path({"NERUDO_CLI": "/tmp/nerudo-release", "CARGO_TARGET_DIR": "/tmp/cargo-shared"}),
                         Path("/tmp/nerudo-release"))
        self.assertEqual(cli_path({"CARGO_TARGET_DIR": "/tmp/cargo-shared"}), Path("/tmp/cargo-shared/debug/nerudo"))
        self.assertEqual(cli_path({}), ROOT / "cf/target/debug/nerudo")

    def test_file_contents_are_wrapped_in_mcp_text_blocks(self):
        data = {"path": "src/price.ts", "content": "export function price() {}\n"}
        response = mcp_response(data)
        self.assertIsInstance(response["content"], list)
        self.assertEqual(response["content"][0]["type"], "text")
        self.assertEqual(json.loads(response["content"][0]["text"]), data)
        native = {"content": [{"type": "text", "text": "LANDED"}], "isError": False}
        self.assertEqual(mcp_response(native), native)

    def test_overlap_counts_concurrent_intervals(self):
        self.assertEqual(max_overlap([
            {"started_s": 0, "ended_s": 4},
            {"started_s": 1, "ended_s": 5},
            {"started_s": 2, "ended_s": 3},
            {"started_s": 5, "ended_s": 6},
        ]), 3)

    def test_adjacent_sessions_are_not_concurrent(self):
        self.assertEqual(max_overlap([
            {"started_s": 0, "ended_s": 1},
            {"started_s": 1, "ended_s": 2},
        ]), 1)

    def test_redaction_removes_environment_and_fork_tokens(self):
        token = "private-value-1234"
        result = redact(token + " art_v1_abcdefghi", {"NERUDO_TOKEN": token})
        self.assertNotIn(token, result)
        self.assertNotIn("art_v1_", result)

    def test_dotenv_is_data_and_preserves_environment_override(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "vars"
            path.write_text('NERUDO_WORKER="https://example.com"\nNERUDO_AGENT_TOKEN=$(touch bad)\n')
            with patch.dict("os.environ", {"NERUDO_WORKER": "https://override.example"}):
                value = variables(path)
            self.assertEqual(value["NERUDO_WORKER"], "https://override.example")
            self.assertEqual(value["NERUDO_AGENT_TOKEN"], "$(touch bad)")
            self.assertFalse((Path(directory) / "bad").exists())

    def test_mutating_run_refuses_production_target_before_clients_start(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "evidence.vars"
            path.write_text("NERUDO_WORKER=https://nerudo.patrickdeutsch.workers.dev\n")
            environment = {k: v for k, v in os.environ.items() if not k.startswith(("NERUDO_", "LANDSEQ_"))}
            result = subprocess.run([sys.executable, "-B", str(Path(__file__).with_name("llm_concurrency.py")),
                                     "run", "--vars", str(path)],
                                    env=environment, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 2)
            self.assertIn("Mutating workflows require a dedicated", result.stderr)
            self.assertNotIn("Providers not ready", result.stderr)

    def test_file_tools_cannot_escape_or_edit_governance(self):
        with tempfile.TemporaryDirectory() as directory:
            tools = object.__new__(Tools)
            tools.workspace = Path(directory)
            for path, write in (("../outside", False), (".git/config", False),
                                ("checks/acceptance.mjs", True), ("nerudo.toml", True)):
                with self.assertRaises(ValueError):
                    tools.path(path, write)
            self.assertEqual(tools.path("src/receipt.ts", True), (Path(directory) / "src/receipt.ts").resolve())


if __name__ == "__main__":
    unittest.main()
