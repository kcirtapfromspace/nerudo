#!/usr/bin/env bash
# Full measurement sweep. Every run appends one JSON line to results/runs.jsonl as it
# finishes, so a partial sweep still leaves usable data.
set -u
cd "$(dirname "$0")/.."
OUT=results/runs.jsonl
CI=2.0
R() { python3 bench/run.py --ci "$CI" --out "$OUT" "$@"; }

# Local: trunk is a bare repo on this Mac; isolates the VCS client from the network.
R --backend git --mode strict --n 1,2,4,8,16,32 --k 3 --reps 2 --label git-strict
R --backend git --mode lax    --n 1,2,4,8,16,32 --k 3 --reps 2 --label git-lax
R --backend seq --mode seq    --n 1,2,4,8,16,32 --k 3 --reps 2 --label seq
R --backend jj  --mode jj --jj-layout clones     --n 1,2,4,8,16,32 --k 3 --reps 2 --label jj-clones
R --backend jj  --mode jj --jj-layout workspaces --n 1,2,4,8,16,32 --k 3 --reps 2 --label jj-workspaces
R --backend jj  --mode jj --jj-layout workspaces --allow-backwards --n 4,8,16 --k 3 --reps 1 \
  --label jj-workspaces-allow-backwards

# Forgejo on the Talos cluster (jetson-2, node-local disk).
R --backend forgejo --mode strict --n 1,2,4,8,16,32 --k 3 --reps 1 --label forgejo-strict
R --backend forgejo --mode pr     --n 1,2,4,8,16,32 --k 3 --reps 1 --label forgejo-pr

# GitHub, bounded: ~110 strict pushes + ~42 PRs. Merge queue is not available on a
# personal account, so PR-merge and direct push are the two GitHub-native paths.
R --backend github --mode strict --n 1,4,8  --k 2 --reps 1 --label github-strict
R --backend github --mode pr     --n 1,4,16 --k 2 --reps 1 --label github-pr

echo "sweep done"
