#!/usr/bin/env python3
"""What the git server of a repo supports for partial reads (protocol v2 upload-pack).

    python3 -I cf/scale/probe_artifacts.py <remote-url> <token-file> > probe.json

<token-file> is JSON {"token": "..."} (the Authorization is "Bearer <token>"). The repo must
have at least two commits on main, and a directory at the top of the tree. The script
prints JSON: the advertised capabilities, and for each request the HTTP status, the error
text if any, and the objects in the pack by type. It never prints the token.
"""
import json
import subprocess
import sys
import urllib.error
import urllib.request
import zlib

UA = "nerudo-scale-probe/0.1"
TYPES = {1: "commit", 2: "tree", 3: "blob", 4: "tag", 6: "ofs_delta", 7: "ref_delta"}


def pkt(s):
    b = s.encode() if isinstance(s, str) else s
    return b"%04x" % (len(b) + 4) + b


class NotPkt(Exception):
    pass


def pkts(data):
    """Split pkt-lines. Yields bytes, or None for flush/delim/response-end."""
    i = 0
    while i + 4 <= len(data):
        try:
            n = int(data[i:i + 4], 16)
        except ValueError:
            raise NotPkt(data[i:i + 300].decode(errors="replace"))
        if n < 4:
            yield None
            i += 4
            continue
        yield data[i + 4:i + n]
        i += n


class Remote:
    def __init__(self, url, token):
        self.url = url.rstrip("/")
        self.auth = f"Bearer {token}"

    def req(self, method, path, body=None, headers=None):
        h = {"Authorization": self.auth, "User-Agent": UA, "Git-Protocol": "version=2", **(headers or {})}
        r = urllib.request.Request(self.url + path, data=body, method=method, headers=h)
        try:
            with urllib.request.urlopen(r, timeout=120) as resp:
                return resp.status, resp.read()
        except urllib.error.HTTPError as e:
            return e.code, e.read()

    def caps(self):
        st, body = self.req("GET", "/info/refs?service=git-upload-pack")
        return st, [p.decode().strip() for p in pkts(body) if p is not None and not p.startswith(b"#")]

    def command(self, cmd, args):
        body = pkt(f"command={cmd}\n") + pkt("agent=nerudo-scale-probe\n") + b"0001"
        for a in args:
            body += pkt(a + "\n")
        body += b"0000"
        return self.req("POST", "/git-upload-pack", body, {"Content-Type": "application/x-git-upload-pack-request",
                                                         "Accept": "application/x-git-upload-pack-result"})


def unpack(resp):
    """The sections of a v2 fetch response, and the pack's objects by type."""
    sections, pack, cur, err = [], b"", None, None
    for p in pkts(resp):
        if p is None:
            continue
        if cur == "packfile":
            if p[0] == 1:
                pack += p[1:]
            elif p[0] == 3:
                err = p[1:].decode(errors="replace").strip()
            continue
        t = p.decode(errors="replace").strip()
        if t in ("acknowledgments", "shallow-info", "wanted-refs", "packfile-uris", "packfile"):
            cur = t
            sections.append(t)
        elif t.startswith("ERR"):
            err = t
        elif cur == "shallow-info":
            sections.append(t.split(" ")[0])
    return sections, err, count(pack) if pack else None, len(pack)


def count(pack):
    assert pack[:4] == b"PACK", pack[:16]
    n = int.from_bytes(pack[8:12], "big")
    pos, out = 12, {}
    for _ in range(n):
        b = pack[pos]
        pos += 1
        typ = (b >> 4) & 7
        while b & 0x80:
            b = pack[pos]
            pos += 1
        if typ == 6:
            b = pack[pos]
            pos += 1
            while b & 0x80:
                b = pack[pos]
                pos += 1
        elif typ == 7:
            pos += 20
        d = zlib.decompressobj()
        d.decompress(pack[pos:])
        pos = len(pack) - len(d.unused_data)
        out[TYPES[typ]] = out.get(TYPES[typ], 0) + 1
    return out


def main():
    url, tokfile = sys.argv[1], sys.argv[2]
    r = Remote(url, json.load(open(tokfile))["token"])
    report = {}
    st, caps = r.caps()
    report["capabilities"] = {"status": st, "lines": caps}
    st, body = r.command("ls-refs", ["ref-prefix refs/heads/"])
    refs = {p.decode().split()[1]: p.decode().split()[0] for p in pkts(body) if p}
    head = refs["refs/heads/main"]
    # The commits and trees, from a full fetch: this is the baseline the others compare with.
    st, body = r.command("fetch", ["ofs-delta", "no-progress", f"want {head}", "done"])
    report["full"] = dict(zip(("sections", "error", "objects", "pack_bytes"), unpack(body)), status=st)
    # Object ids for the other requests, from a local clone of the same remote.
    ids = git_ids(url, json.load(open(tokfile))["token"])
    report["ids_note"] = "parent = main~1; subtree = a top-level directory of main; blob = a file in it"
    cases = {
        "deepen_1": [f"want {head}", "deepen 1"],
        "filter_blob_none": [f"want {head}", "filter blob:none"],
        "deepen_1_filter_blob_none": [f"want {head}", "deepen 1", "filter blob:none"],
        "deepen_1_filter_tree_0": [f"want {head}", "deepen 1", "filter tree:0"],
        "deepen_1_have_parent": [f"want {head}", f"have {ids['parent']}", "deepen 1"],
        "deepen_1_filter_blob_none_have_parent": [f"want {head}", f"have {ids['parent']}", "deepen 1", "filter blob:none"],
        "want_parent_commit_deepen_1": [f"want {ids['parent']}", "deepen 1"],
        "want_tree": [f"want {ids['subtree']}"],
        "want_tree_filter_blob_none": [f"want {ids['subtree']}", "filter blob:none"],
        "want_tree_filter_tree_0": [f"want {ids['subtree']}", "filter tree:0"],
        "want_tree_filter_tree_1": [f"want {ids['subtree']}", "filter tree:1"],
        "want_blob": [f"want {ids['blob']}"],
        "want_two_blobs": [f"want {ids['blob']}", f"want {ids['blob2']}"],
        "want_root_tree_have_head": [f"want {ids['root']}", f"have {head}"],
    }
    for name, args in cases.items():
        st, body = r.command("fetch", ["ofs-delta", "no-progress", *args, "done"])
        try:
            out = dict(zip(("sections", "error", "objects", "pack_bytes"), unpack(body)), status=st)
        except NotPkt as e:
            out = {"status": st, "error": str(e).strip(), "objects": None}
        if st != 200 or out["objects"] is None:
            out["body"] = body[:300].decode(errors="replace")
        report[name] = out
    if any(c.startswith("object-info") for c in caps):
        st, body = r.command("object-info", ["size", f"oid {ids['blob']}"])
        report["object_info"] = {"status": st, "lines": [p.decode().strip() for p in pkts(body) if p]}
    json.dump(report, sys.stdout, indent=1)
    print()


def git_ids(url, token):
    import os
    import tempfile
    d = tempfile.mkdtemp(prefix="probe-")
    env = dict(os.environ, GIT_TERMINAL_PROMPT="0")
    hdr = f"http.extraHeader=Authorization: Bearer {token}"
    subprocess.run(["git", "-c", "credential.helper=", "-c", hdr, "clone", "-q", "--bare", url, d], check=True, env=env, capture_output=True)

    def rev(x):
        return subprocess.run(["git", "-C", d, "rev-parse", x], check=True, capture_output=True, text=True).stdout.strip()

    ls = subprocess.run(["git", "-C", d, "ls-tree", "main"], check=True, capture_output=True, text=True).stdout.split("\n")
    sub = next(l for l in ls if " tree " in l)
    name = sub.split("\t")[1]
    blobs = subprocess.run(["git", "-C", d, "ls-tree", "-r", f"main:{name}"], check=True, capture_output=True, text=True).stdout.split("\n")
    blobs = [l.split()[2] for l in blobs if " blob " in l]
    return {"parent": rev("main~1"), "root": rev("main^{tree}"), "subtree": sub.split()[2], "blob": blobs[0], "blob2": blobs[-1]}


if __name__ == "__main__":
    main()
