#!/usr/bin/env python3
"""Regenerate deps.lock from deps/*.txt.

The digest header covers every entry, so any two changes that add dependencies
conflict textually on the lockfile -- like a real lockfile, and unlike a plain list.
"""
import hashlib
import os
import sys


def render_entries(entries):
    """entries: {filename: content}. Returns the lockfile text."""
    lines = [entries[name].strip() for name in sorted(entries)]
    digest = hashlib.sha256("\n".join(lines).encode()).hexdigest()
    return f"# digest: {digest}\n" + "".join(line + "\n" for line in lines)


def render(root):
    deps = os.path.join(root, "deps")
    entries = {}
    for name in os.listdir(deps):
        if name.endswith(".txt"):
            with open(os.path.join(deps, name)) as fh:
                entries[name] = fh.read()
    return render_entries(entries)


if __name__ == "__main__":
    root = sys.argv[1] if len(sys.argv) > 1 else "."
    with open(os.path.join(root, "deps.lock"), "w") as fh:
        fh.write(render(root))
