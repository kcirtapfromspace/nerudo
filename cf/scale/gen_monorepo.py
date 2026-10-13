#!/usr/bin/env python3
"""Make a synthetic monorepo for the nerudo scale tests, with git fast-import.

    python3 -I cf/scale/gen_monorepo.py OUT --files 10000 --bytes 100M [--commits 20]

The tree is `svc/pkgNNN/<dir>/.../fNNNNN.ts`, plus the files that the landing rules use:
nerudo.toml, CHANGELOG.md (append rule), migrations/ (numbered files), deps/*.txt and
deps.lock (a derived file). The footprint unit is one package, `svc/*`. The text is lines
of pseudo source code: it compresses about as much as real source code.

The first commit holds the whole tree. Each later commit (--commits) changes --churn
files. The script prints one JSON line: the counts, the tip size and the head commit.
The same arguments always make the same repo.
"""
import argparse
import hashlib
import json
import os
import random
import subprocess
import sys

RULES = """# Landing rules for a synthetic monorepo (cf/scale/gen_monorepo.py).
[footprint]
units = ["svc/*"]

[[commutative]]
path = "CHANGELOG.md"
rule = "append"

[[derived]]
path = "deps.lock"
generator = "digest-index"
source = "deps"

[[allocate]]
dir = "migrations"
pending_prefix = "NEXT_"
width = 4

[seals]
governance = ["ci/**", "nerudo.toml"]
tests = ["tests/*.py"]
test_style = "python-unittest"
migrations = ["migrations/*.sql"]

[[checks]]
kind = "unique-numbering"
dir = "migrations"

[[checks]]
kind = "no-pending"
dir = "migrations"
prefix = "NEXT_"

[[checks]]
kind = "derived-consistent"
"""

WORDS = ("const let return if else for while async await import export from type interface class "
         "function new this null undefined true false string number boolean void Promise Map Set "
         "request response handler config store tree commit blob path entry value result error "
         "context state queue batch change trunk agent fork token remote fetch push pack object").split()


def size(s):
    units = {"K": 1 << 10, "M": 1 << 20, "G": 1 << 30}
    return int(float(s[:-1]) * units[s[-1].upper()]) if s[-1].upper() in units else int(s)


def line_pool(rng, n=200_000):
    pool = []
    for i in range(n):
        ws = rng.choices(WORDS, k=rng.randint(3, 9))
        indent = "  " * rng.randint(0, 4)
        pool.append(f"{indent}{ws[0]} {'_'.join(ws[1:3])}{i % 997}({', '.join(ws[3:])}) {{ {rng.randint(0, 99999)} }};\n")
    return pool


def digest_index(deps):
    """cf/nerudo-core/src/merge.rs digest_index: deps/*.txt lines sorted by name."""
    lines = [deps[n].strip() for n in sorted(deps)]
    out = f"# digest: {hashlib.sha256(chr(10).join(lines).encode()).hexdigest()}\n"
    return out + "".join(l + "\n" for l in lines)


def paths(a):
    """The file paths, in a fixed order: packages, then directories `depth` levels deep with
    `fanout` children each, files spread evenly over the leaf directories."""
    leaves = []
    for p in range(a.packages):
        dirs = [f"svc/pkg{p:03d}"]
        for level in range(a.depth):
            dirs = [f"{d}/{'dmn'[level % 3]}{c:02d}" for d in dirs for c in range(a.fanout)]
        leaves.extend(dirs)
    exts = (".ts", ".ts", ".ts", ".tsx", ".json", ".md")
    return [f"{leaves[i % len(leaves)]}/f{i:06d}{exts[i % len(exts)]}" for i in range(a.files)]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("out")
    ap.add_argument("--files", type=int, required=True)
    ap.add_argument("--bytes", type=size, required=True, help="tip size of the svc/ files, e.g. 100M")
    ap.add_argument("--packages", type=int, default=0, help="default: files/400, at least 4")
    ap.add_argument("--depth", type=int, default=2, help="directory levels under each package")
    ap.add_argument("--fanout", type=int, default=4, help="subdirectories per level")
    ap.add_argument("--commits", type=int, default=20, help="commits after the first")
    ap.add_argument("--churn", type=int, default=20, help="files changed by each later commit")
    ap.add_argument("--seed", type=int, default=1)
    a = ap.parse_args()
    a.packages = a.packages or max(4, a.files // 400)
    rng = random.Random(a.seed)
    pool = line_pool(rng)
    mean = sum(map(len, pool[:2000])) / 2000
    per_file = max(1, int(a.bytes / a.files / mean))
    files = paths(a)

    def content(i, version):
        r = random.Random(a.seed * 1_000_003 + i * 7919 + version)
        k = max(1, int(per_file * r.uniform(0.3, 1.7)))
        return f"// {files[i]} v{version}\n" + "".join(r.choices(pool, k=k))

    os.makedirs(a.out, exist_ok=False)
    env = dict(os.environ, GIT_TERMINAL_PROMPT="0")
    subprocess.run(["git", "init", "-q", "-b", "main", a.out], check=True, env=env)
    fi = subprocess.Popen(["git", "-C", a.out, "fast-import", "--quiet", "--done"], stdin=subprocess.PIPE, env=env)
    w = fi.stdin.write
    mark = [0]

    def blob(data):
        b = data.encode()
        mark[0] += 1
        w(b"blob\nmark :%d\ndata %d\n" % (mark[0], len(b)) + b + b"\n")
        return mark[0]

    t0 = 1_760_000_000
    tip_bytes = 0

    def commit(n, msg, ops):
        w(b"commit refs/heads/main\n")
        w(b"author gen <gen@scale.invalid> %d +0000\ncommitter gen <gen@scale.invalid> %d +0000\n" % (t0 + n, t0 + n))
        m = msg.encode()
        w(b"data %d\n" % len(m) + m + b"\n")
        for path, mk in ops:
            w(b"M 100644 :%d %s\n" % (mk, path.encode()))
        w(b"\n")

    ops = []
    for i, p in enumerate(files):
        text = content(i, 0)
        tip_bytes += len(text)
        ops.append((p, blob(text)))
    deps = {f"dep{d:02d}.txt": f"dep{d:02d} 1.{d}.0\n" for d in range(20)}
    extra = {
        "nerudo.toml": RULES,
        "README.md": f"# synthetic monorepo\n\n{a.files} files in {a.packages} packages.\n",
        "CHANGELOG.md": "# Changelog\n",
        "migrations/0001_init.sql": "create table t (id int);\n",
        "ci/check.py": "print('ok')\n",
        "deps.lock": digest_index(deps),
        **{f"deps/{n}": t for n, t in deps.items()},
    }
    for p, t in extra.items():
        ops.append((p, blob(t)))
    commit(0, f"synthetic monorepo: {a.files} files\n", ops)
    version = {}
    for c in range(1, a.commits + 1):
        ops = []
        for i in rng.sample(range(a.files), min(a.churn, a.files)):
            version[i] = version.get(i, 0) + 1
            ops.append((files[i], blob(content(i, version[i]))))
        commit(c, f"history {c}\n", ops)
    w(b"done\n")
    fi.stdin.close()
    if fi.wait():
        sys.exit("fast-import failed")
    for i, v in version.items():
        tip_bytes += len(content(i, v)) - len(content(i, 0))
    tip_bytes += sum(len(t) for t in extra.values())
    head = subprocess.run(["git", "-C", a.out, "rev-parse", "main"], check=True, capture_output=True, text=True).stdout.strip()
    subprocess.run(["git", "-C", a.out, "repack", "-adq", "--window=10", "--depth=20"], check=True, env=env)
    pack = sum(os.path.getsize(os.path.join(dp, f)) for dp, _, fs in os.walk(os.path.join(a.out, ".git", "objects", "pack")) for f in fs if f.endswith(".pack"))
    dirs = len({p.rsplit("/", 1)[0] for p in files})
    print(json.dumps({"out": os.path.basename(a.out), "files": a.files + len(extra), "dirs": dirs, "packages": a.packages,
                      "depth": a.depth + 2, "commits": a.commits + 1, "tip_bytes": tip_bytes, "pack_bytes": pack, "head": head}))


if __name__ == "__main__":
    main()
