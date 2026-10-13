"""Synthetic workload shared by every backend: same repo, same seeded change specs.

A change spec is deterministic from (seed, agent, index). Agents re-apply the spec to
whatever base they are on, so a retry is "regenerate on the new base" rather than a
textual rebase -- the cattle-not-pets workspace model, applied identically everywhere.
"""
import hashlib
import importlib.util
import os
import random
import re
import shutil
import subprocess
from dataclasses import dataclass

HERE = os.path.dirname(os.path.abspath(__file__))
TEMPLATE = os.path.join(HERE, "repo_template")

N_TARGETS = 40
LINES_PER_TARGET = 30
ZIPF_S = 1.1
P_CHANGELOG = 0.5
P_DEP = 0.3
P_MIGRATION = 0.2

_spec = importlib.util.spec_from_file_location("gen_lock", os.path.join(TEMPLATE, "ci", "gen_lock.py"))
gen_lock = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gen_lock)
_spec = importlib.util.spec_from_file_location("check", os.path.join(TEMPLATE, "ci", "check.py"))
check = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(check)

ZIPF_W = [1 / (i + 1) ** ZIPF_S for i in range(N_TARGETS)]


@dataclass(frozen=True)
class Spec:
    cid: str          # stable change id, e.g. a03c01
    agent: int
    index: int
    target: int
    line: int
    changelog: bool
    dep: bool
    migration: bool

    @property
    def subject(self):
        return f"{self.cid}: edit t{self.target:02d}:{self.line}"


def make_spec(seed, agent, index):
    h = hashlib.sha256(f"{seed}:{agent}:{index}".encode()).digest()
    rng = random.Random(h)
    return Spec(
        cid=f"a{agent:02d}c{index:02d}", agent=agent, index=index,
        target=rng.choices(range(N_TARGETS), weights=ZIPF_W)[0],
        line=rng.randrange(LINES_PER_TARGET),
        changelog=rng.random() < P_CHANGELOG,
        dep=rng.random() < P_DEP,
        migration=rng.random() < P_MIGRATION,
    )


def init_tree(root):
    """Write the initial repo content into an empty directory."""
    shutil.copytree(os.path.join(TEMPLATE, "ci"), os.path.join(root, "ci"),
                    ignore=shutil.ignore_patterns("__pycache__"))
    for t in range(N_TARGETS):
        d = os.path.join(root, "targets", f"t{t:02d}")
        os.makedirs(d)
        with open(os.path.join(d, "src.txt"), "w") as fh:
            fh.writelines(f"t{t:02d} line {i}\n" for i in range(LINES_PER_TARGET))
    shutil.copy(os.path.join(TEMPLATE, "nerudo.toml"), root)
    os.makedirs(os.path.join(root, "deps"))
    with open(os.path.join(root, "deps", "base.txt"), "w") as fh:
        fh.write("base==1.0\n")
    with open(os.path.join(root, "deps.lock"), "w") as fh:
        fh.write(gen_lock.render(root))
    os.makedirs(os.path.join(root, "migrations"))
    with open(os.path.join(root, "migrations", "0001_init.sql"), "w") as fh:
        fh.write("create table users(id int);\n")
    os.makedirs(os.path.join(root, "changes"))
    with open(os.path.join(root, "changes", ".keep"), "w") as fh:
        fh.write("")
    with open(os.path.join(root, "CHANGELOG.md"), "w") as fh:
        fh.write("# Changelog\n")
    # Best-practice git config for the vanilla baselines: union-merge the changelog.
    with open(os.path.join(root, ".gitattributes"), "w") as fh:
        fh.write("CHANGELOG.md merge=union\n")
    with open(os.path.join(root, ".gitignore"), "w") as fh:
        fh.write("__pycache__/\n")


def next_migration_number(root):
    nums = [int(m.group(1)) for f in os.listdir(os.path.join(root, "migrations"))
            if (m := check.MIGRATION.fullmatch(f))]
    return max(nums, default=0) + 1


def apply_spec(root, spec, migrations="numbered"):
    """Apply a change spec to a checkout. migrations='numbered' picks the next number at
    this base (what an agent does today); 'next' writes NEXT_<name>.sql for the sequencer
    to number at landing time (the derived-value contract)."""
    path = os.path.join(root, "targets", f"t{spec.target:02d}", "src.txt")
    with open(path) as fh:
        lines = fh.readlines()
    lines[spec.line] = f"{spec.cid} edited t{spec.target:02d} line {spec.line}\n"
    with open(path, "w") as fh:
        fh.writelines(lines)
    with open(os.path.join(root, "changes", f"{spec.cid}.txt"), "w") as fh:
        fh.write(f"{spec.cid}\n")
    if spec.changelog:
        with open(os.path.join(root, "CHANGELOG.md"), "a") as fh:
            fh.write(f"- {spec.cid}\n")
    if spec.dep:
        with open(os.path.join(root, "deps", f"{spec.cid}.txt"), "w") as fh:
            fh.write(f"dep-{spec.cid}==1.0\n")
        regen_lock(root)
    if spec.migration:
        if migrations == "next":
            name = f"NEXT_{spec.cid}.sql"
        else:
            name = f"{next_migration_number(root):04d}_{spec.cid}.sql"
        with open(os.path.join(root, "migrations", name), "w") as fh:
            fh.write(f"-- {spec.cid}\nalter table users add column c_{spec.cid} int;\n")


def regen_lock(root):
    with open(os.path.join(root, "deps.lock"), "w") as fh:
        fh.write(gen_lock.render(root))


# ---------- post-run analysis of a trunk, straight from git objects ----------

def _git(repo, *args):
    return subprocess.run(["git", "-C", repo, *args], check=True, capture_output=True, text=True).stdout


def analyze_trunk(repo, ref, specs, believed_landed):
    """repo: any git dir that has `ref`. Returns correctness metrics for the trunk."""
    commits = _git(repo, "rev-list", "--first-parent", "--reverse", ref).split()
    broken = 0
    first_broken = None
    blobs = {}  # deps files never change once written, so cache by blob id
    for c in commits[1:]:  # skip the initial commit
        names = _git(repo, "ls-tree", "--name-only", f"{c}:migrations").split()
        deps = {}
        for line in _git(repo, "ls-tree", f"{c}:deps").splitlines():
            meta, name = line.split("\t", 1)
            oid = meta.split()[2]
            if oid not in blobs:
                blobs[oid] = _git(repo, "cat-file", "blob", oid)
            deps[name] = blobs[oid]
        lock = _git(repo, "show", f"{c}:deps.lock")
        errs = check.problems(names, lock, gen_lock.render_entries(deps), on_trunk=True)
        if errs:
            broken += 1
            first_broken = first_broken or errs[0]
    tip = commits[-1]
    present = set(_git(repo, "ls-tree", "--name-only", f"{tip}:changes").split())
    lost = sorted(s.cid for s in specs if s.cid in believed_landed and f"{s.cid}.txt" not in present)
    missing = sorted(s.cid for s in specs if f"{s.cid}.txt" not in present)
    mig = _git(repo, "ls-tree", "--name-only", f"{tip}:migrations").split()
    nums = [m.group(1) for f in mig if (m := check.MIGRATION.fullmatch(f))]
    dups = len(nums) - len(set(nums))
    return {
        "trunk_commits": len(commits) - 1,
        "broken_commits": broken,
        "broken_example": first_broken,
        "lost_changes": len(lost),
        "lost_example": lost[:3],
        "missing_changes": len(missing),
        "dup_migrations": dups,
    }
