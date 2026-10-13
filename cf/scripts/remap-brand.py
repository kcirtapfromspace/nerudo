#!/usr/bin/env python3
"""Preview or apply the hosted forge's brand mappings without replacing Git data."""
import argparse
import json
import os
from pathlib import Path
from urllib.error import HTTPError
from urllib.parse import parse_qs, quote, urlsplit
from urllib.request import Request, urlopen


def environment(path):
    values = {}
    if path:
        for line in Path(path).read_text().splitlines():
            if "=" in line and not line.lstrip().startswith("#"):
                key, value = line.strip().removeprefix("export ").split("=", 1)
                values[key] = value.strip().strip("\"'")
    values.update(os.environ)
    for key, value in list(values.items()):
        if key.startswith("LANDSEQ_"):
            values.setdefault("NERUDO_" + key[8:], value)
    return values


def plan(directory):
    repos = directory.get("repos", [])
    destinations = {(r["org"], r["repo"]): r for r in repos}
    changes = []
    if any(u["user"] == "nerudo" for u in directory.get("users", [])):
        raise ValueError("nerudo is already a user; cannot move the organization into its name")
    for old in repos:
        if old["org"] != "landseq":
            continue
        name = "nerudo" if old["repo"] == "landseq" else old["repo"]
        target = destinations.get(("nerudo", name))
        if target and target["trunk"] != old["trunk"]:
            raise ValueError(f"nerudo/{name} belongs to a different trunk; no changes applied")
        changes.append((old, name))
    return changes


class Forge:
    def __init__(self, worker, token):
        self.worker = worker.rstrip("/")
        self.token = token

    def call(self, method, path, body=None, key=None):
        headers = {"User-Agent": "NERUDO-brand-migration/1.0", "Accept": "application/json"}
        if self.token:
            headers["Authorization"] = "Bearer " + self.token
        if key:
            headers["X-Nerudo-Key"] = key
        if body is not None:
            headers["Content-Type"] = "application/json"
        request = Request(self.worker + path, data=None if body is None else json.dumps(body).encode(), headers=headers, method=method)
        try:
            with urlopen(request, timeout=120) as response:
                return json.load(response)
        except HTTPError as error:
            # Never log request headers, bearer tokens, or response bodies containing keys.
            raise RuntimeError(f"{method} {path}: HTTP {error.code}") from None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--vars", help="Existing vars file; accepts either generation of variable names")
    parser.add_argument("--worker", help="Sequencer URL (defaults to NERUDO_WORKER or its legacy value)")
    parser.add_argument("--apply", action="store_true", help="Apply the reviewed mappings with the admin token")
    args = parser.parse_args()
    config = environment(args.vars)
    worker = args.worker or config.get("NERUDO_WORKER", "https://nerudo.patrickdeutsch.workers.dev")
    token = config.get("NERUDO_ADMIN_TOKEN", "")
    if args.apply and not token:
        parser.error("--apply requires NERUDO_ADMIN_TOKEN (or its legacy value)")
    forge = Forge(worker, token)
    directory = forge.call("GET", "/v1/hub/orgs")
    changes = plan(directory)  # Validate every destination before making any mutation.
    for old, name in changes:
        visibility = "public" if old["public"] else "private"
        print(f"{old['org']}/{old['repo']} -> nerudo/{name} ({visibility}, same trunk {old['trunk']})")
    if not args.apply:
        print(f"Preview: {len(changes)} mappings. No changes applied.")
        return
    if not changes:
        print("Already remapped. No changes applied.")
        return
    old_org = next((o for o in directory.get("orgs", []) if o["org"] == "landseq"), {})
    if not any(o["org"] == "nerudo" for o in directory.get("orgs", [])):
        forge.call("PUT", "/v1/hub/orgs/nerudo", {"title": "NERUDO", "description": old_org.get("description") or "Good work takes form."})
    for old, name in changes:
        trunk = quote(old["trunk"], safe="")
        # A bearer token administers metadata; a share key passes the repo's read gate.
        share = forge.call("POST", f"/v1/trunks/{trunk}/share", {})
        share_key = parse_qs(urlsplit(share["url"]).query).get("k", [None])[0]
        if not share_key:
            raise RuntimeError("share response has no read key; stop before renaming")
        before = forge.call("GET", f"/t/{trunk}/api/branches", key=share_key)
        payload = {key: old.get(key) for key in ("trunk", "public", "description", "upstream")}
        path = f"/v1/hub/repos/nerudo/{quote(name, safe='')}"
        new = forge.call("PUT", path, payload)
        for field in ("trunk", "public", "description", "upstream", "created_ms"):
            if new.get(field) != old.get(field):
                raise RuntimeError(f"nerudo/{name}: {field} changed; stop and inspect before continuing")
        after = forge.call("GET", f"/t/{trunk}/api/branches", key=share_key)
        if before != after:
            raise RuntimeError(f"nerudo/{name}: refs changed during migration; inspect concurrent writes before continuing")
        print(f"Verified nerudo/{name}: metadata and refs preserved")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, RuntimeError) as error:
        raise SystemExit(str(error)) from None
