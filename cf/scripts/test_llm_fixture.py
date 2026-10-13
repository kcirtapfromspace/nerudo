#!/usr/bin/env python3
"""Run a fixture checkout's unit tests and, optionally, the trusted final oracle."""

import argparse
from pathlib import Path
import subprocess
import sys


def main():
    fixture = Path(__file__).resolve().parents[1] / "fixtures" / "llm-concurrency"
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=fixture, help="checkout whose source and tests to check")
    parser.add_argument("--acceptance", action="store_true", help="also run the protected final acceptance oracle")
    parser.add_argument("--with-discount", action="store_true", help="require the optional Gemini discount task")
    args = parser.parse_args()
    repo = args.repo.resolve()
    if args.with_discount and not args.acceptance:
        parser.error("--with-discount requires --acceptance")
    if not repo.is_dir():
        parser.error(f"checkout does not exist: {repo}")
    tests = sorted(repo.joinpath("tests").glob("*.test.mjs"))
    if not tests:
        parser.error(f"no runtime test files found in {repo / 'tests'}")
    try:
        result = subprocess.run(
            ["node", "--experimental-strip-types", "--test", *(str(path) for path in tests)],
            cwd=repo,
            check=False,
        )
        if result.returncode:
            return result.returncode
        if args.acceptance:
            command = ["node", "--experimental-strip-types", str(fixture / "checks" / "acceptance.mjs"), "--repo", str(repo)]
            if args.with_discount:
                command.append("--with-discount")
            return subprocess.run(command, cwd=repo, check=False).returncode
    except FileNotFoundError:
        print("Node is required to run the TypeScript fixture checks.", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
