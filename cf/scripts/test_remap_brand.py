"""Migration contract tests using a simulated forge; no external mutations."""
import contextlib
import importlib.util
import io
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("remap_brand", Path(__file__).with_name("remap-brand.py"))
remap = importlib.util.module_from_spec(spec)
spec.loader.exec_module(remap)


class MigrationTests(unittest.TestCase):
    def test_private_repo_keeps_the_same_read_key_before_and_after_rename(self):
        row = {"org": "landseq", "repo": "private", "trunk": "stable", "public": False, "description": "private work", "upstream": None, "created_ms": 7}
        reads = []

        class Forge:
            def __init__(self, worker, token):
                pass

            def call(self, method, path, body=None, key=None):
                if path == "/v1/hub/orgs":
                    return {"repos": [row], "users": [], "orgs": [{"org": "nerudo"}]}
                if path.endswith("/share"):
                    return {"url": "https://example.invalid/t/stable?k=" + "0" * 32}
                if path.endswith("/branches"):
                    if key != "0" * 32:
                        raise RuntimeError("wrong read credential")
                    reads.append(key)
                    return {"head": "unchanged", "forks": []}
                if path == "/v1/hub/repos/nerudo/private" and method == "PUT":
                    return {**row, **body, "org": "nerudo"}
                raise AssertionError((method, path))

        with patch.object(remap, "Forge", Forge), patch.object(remap, "environment", return_value={"NERUDO_ADMIN_TOKEN": "synthetic"}), patch("sys.argv", ["remap-brand.py", "--apply"]), contextlib.redirect_stdout(io.StringIO()):
            remap.main()
        self.assertEqual(reads, ["0" * 32, "0" * 32])

    def test_preflight_rejects_an_occupied_destination(self):
        with self.assertRaises(ValueError):
            remap.plan({"repos": [{"org": "landseq", "repo": "landseq", "trunk": "ours"}, {"org": "nerudo", "repo": "nerudo", "trunk": "other"}]})


if __name__ == "__main__":
    unittest.main()
