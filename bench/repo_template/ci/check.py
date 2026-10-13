#!/usr/bin/env python3
"""Simulated CI for the benchmark repo.

Sleeps CI_SECONDS to stand in for build+test cost, then runs the checks that the
allocation and derived-file races break. CI_MAIN=1 means "this tree is trunk", where
pending NEXT_ migrations are not allowed.
"""
import os
import re
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from gen_lock import render  # noqa: E402

MIGRATION = re.compile(r"(\d{4})_(.+)\.sql")


def problems(names, lock_text, lock_expected, on_trunk):
    errs = []
    seen = {}
    for name in sorted(names):
        m = MIGRATION.fullmatch(name)
        if m:
            seen.setdefault(m.group(1), []).append(name)
        elif name.startswith("NEXT_") and name.endswith(".sql"):
            if on_trunk:
                errs.append(f"pending migration on trunk: {name}")
        else:
            errs.append(f"bad migration name: {name}")
    for num, files in seen.items():
        if len(files) > 1:
            errs.append(f"duplicate migration number {num}: {files}")
    if lock_text != lock_expected:
        errs.append("deps.lock is stale")
    return errs


def main():
    time.sleep(float(os.environ.get("CI_SECONDS", "0")))
    with open("deps.lock") as fh:
        lock_text = fh.read()
    errs = problems(os.listdir("migrations"), lock_text, render("."),
                    os.environ.get("CI_MAIN") == "1")
    for e in errs:
        print(e)
    sys.exit(1 if errs else 0)


if __name__ == "__main__":
    main()
