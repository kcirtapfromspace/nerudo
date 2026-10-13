"""Level-0 review gates: deterministic checks that need no judgment.

Every gate runs from the *trusted* definition on base, never from files in the change:
a change that edits ci/check.py must not get to decide how it is tested.

  tests        the full suite passes (trusted runner)
  mutation     mutate the lines the change added in ledger/*.py; tests must kill most mutants
  ratchet      pre-existing tests were not removed, skipped, or had assertions changed
  governance   the change does not touch CI, landing rules, or workflow config
  irreversible destructive SQL in a change whose Intent doesn't declare it irreversible

tests/mutation failures send the change back to the agent. ratchet/governance/irreversible
hits are seal boundaries: they go to a human regardless of what reviewers say.
"""
import difflib
import os
import re
import shutil
import subprocess
import tempfile

ENV = dict(os.environ, PYTHONDONTWRITEBYTECODE="1")
TRUSTED_RUNNER = ["python3", "-m", "unittest", "discover", "-s", "tests", "-t", ".", "-q"]
MUTATION_THRESHOLD = 0.70   # fixed before looking at defect results; see report caveats
GOVERNANCE = re.compile(r"^(ci/|nerudo\.toml$|\.github/)")
DESTRUCTIVE = re.compile(r"\b(drop\s+(table|column)|delete\s+from|truncate)\b", re.I)


def materialize(tree, root):
    for p, txt in tree.items():
        full = os.path.join(root, p)
        os.makedirs(os.path.dirname(full), exist_ok=True)
        with open(full, "w") as fh:
            fh.write(txt)


def unified_diff(base, tree):
    out = []
    for p in sorted(set(base) | set(tree)):
        a, b = base.get(p), tree.get(p)
        if a == b:
            continue
        out.extend(difflib.unified_diff(
            (a or "").splitlines(keepends=True), (b or "").splitlines(keepends=True),
            fromfile=f"a/{p}" if a is not None else "/dev/null",
            tofile=f"b/{p}" if b is not None else "/dev/null"))
    return "".join(line if line.endswith("\n") else line + "\n" for line in out)


def changed_paths(base, tree):
    return sorted(p for p in set(base) | set(tree) if base.get(p) != tree.get(p))


def run_tests(root, timeout=60):
    try:
        p = subprocess.run(TRUSTED_RUNNER, cwd=root, capture_output=True, text=True, env=ENV,
                           timeout=timeout)
        return p.returncode == 0
    except subprocess.TimeoutExpired:
        return False


# ------------------------------------------------------------------ mutation

_OPS = [
    (re.compile(r"<="), "<"), (re.compile(r">="), ">"),
    (re.compile(r"(?<![<>=!-])<(?![<=])"), "<="), (re.compile(r"(?<![<>=!-])>(?![>=])"), ">="),
    (re.compile(r"=="), "!="), (re.compile(r"!="), "=="),
    (re.compile(r" \+ "), " - "), (re.compile(r" - "), " + "),
    (re.compile(r"\bTrue\b"), "False"), (re.compile(r"\bFalse\b"), "True"),
    (re.compile(r"(?<![\w.])(\d+)(?![\w.])"), None),   # integer constant -> n + 1
]


def _mutants_for_line(line):
    stripped = line.strip()
    if (not stripped or stripped.startswith(("#", "def ", "class ", "import ", "from ", '"""', "'"))
            or stripped.startswith(("@",))):
        return []
    indent = line[: len(line) - len(line.lstrip())]
    if stripped.startswith("raise "):
        return [indent + "pass\n"]
    out = []
    code = line.split("#", 1)[0]
    for rx, rep in _OPS:
        m = rx.search(code)
        if not m:
            continue
        new = (str(int(m.group(1)) + 1) if rep is None else rep)
        out.append(line[: m.start()] + new + line[m.end():])
    if stripped.startswith("return ") and stripped != "return None":
        out.append(indent + "return None\n")
    return out


def added_lines(base_text, new_text):
    """Line numbers (0-based) in new_text that the change added or modified."""
    sm = difflib.SequenceMatcher(a=(base_text or "").splitlines(), b=new_text.splitlines())
    idx = []
    for tag, _, _, j1, j2 in sm.get_opcodes():
        if tag in ("replace", "insert"):
            idx.extend(range(j1, j2))
    return idx


def _in_docstring_mask(lines):
    mask, inside = [], False
    for line in lines:
        s = line.strip()
        starts = s.startswith(('"""', "'''"))
        mask.append(inside or starts)
        if starts:
            q = s[:3]
            if not (len(s) > 3 and s.endswith(q)) and s.count(q) == 1:
                inside = not inside
        elif inside and (s.endswith('"""') or s.endswith("'''")):
            inside = False
    return mask


def mutation_score(base, tree, root):
    """Returns (score or None if nothing to mutate, killed, total)."""
    killed = total = 0
    for p in changed_paths(base, tree):
        if not (p.startswith("ledger/") and p.endswith(".py")) or p not in tree:
            continue
        lines = tree[p].splitlines(keepends=True)
        doc = _in_docstring_mask(lines)
        for i in added_lines(base.get(p), tree[p]):
            if doc[i]:
                continue
            for mutant in _mutants_for_line(lines[i]):
                total += 1
                mutated = "".join(lines[:i] + [mutant] + lines[i + 1:])
                with open(os.path.join(root, p), "w") as fh:
                    fh.write(mutated)
                if not run_tests(root, timeout=20):
                    killed += 1
        with open(os.path.join(root, p), "w") as fh:
            fh.write(tree[p])
    return (killed / total if total else None), killed, total


# ------------------------------------------------------------------ ratchet / governance / irreversible

TEST_DEF = re.compile(r"^    def (test_\w+)\(", re.M)
SKIP = re.compile(r"@unittest\.(skip|skipIf|skipUnless|expectedFailure)\b")


def _test_bodies(text):
    bodies, cur, name = {}, [], None
    for line in text.splitlines():
        m = re.match(r"^    def (test_\w+)\(", line)
        if m or line.startswith("class ") or line.startswith("if __name__"):
            if name:
                bodies[name] = cur
            name, cur = (m.group(1) if m else None), []
        elif name:
            cur.append(line.strip())
    if name:
        bodies[name] = cur
    return bodies


def ratchet(base, tree):
    """Findings for weakened, skipped or removed pre-existing tests."""
    found = []
    for p in changed_paths(base, tree):
        if not p.startswith("tests/") or p not in base:
            continue
        old, new = base[p], tree.get(p, "")
        removed = set(TEST_DEF.findall(old)) - set(TEST_DEF.findall(new))
        found += [f"{p}: removed {t}" for t in sorted(removed)]
        if len(SKIP.findall(new)) > len(SKIP.findall(old)):
            found.append(f"{p}: added a skip decorator")
        ob, nb = _test_bodies(old), _test_bodies(new)
        for t in sorted(set(ob) & set(nb)):
            oa = [l for l in ob[t] if "assert" in l]
            na = [l for l in nb[t] if "assert" in l]
            if oa != na:
                found.append(f"{p}: changed assertions in {t}")
    return found


def governance(base, tree):
    return [p for p in changed_paths(base, tree) if GOVERNANCE.match(p)]


def irreversible(base, tree, intent):
    if "irreversible" in intent.reversibility.lower() and "not" not in intent.reversibility.lower():
        return []
    hits = []
    for p in changed_paths(base, tree):
        if p.endswith(".sql") and p in tree:
            for i in added_lines(base.get(p), tree[p]):
                line = tree[p].splitlines()[i]
                if DESTRUCTIVE.search(line.split("--", 1)[0]):
                    hits.append(f"{p}: {line.strip()}")
    return hits


def run_gates(base, change):
    root = tempfile.mkdtemp(prefix="gate-")
    try:
        materialize(change.files, root)
        tests_ok = run_tests(root)
        score, killed, total = mutation_score(base, change.files, root) if tests_ok else (None, 0, 0)
        return {
            "tests_ok": tests_ok,
            "mutation_score": score, "mutants_killed": killed, "mutants": total,
            "mutation_ok": score is None or score >= MUTATION_THRESHOLD,
            "ratchet": ratchet(base, change.files),
            "governance": governance(base, change.files),
            "irreversible": irreversible(base, change.files, change.intent),
        }
    finally:
        shutil.rmtree(root, ignore_errors=True)
