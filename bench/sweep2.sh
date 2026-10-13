#!/usr/bin/env bash
# Second pass: the sequencer with each forge as a downstream mirror. The forge never
# coordinates here; it only receives trunk from the sequencer.
set -u
cd "$(dirname "$0")/.."
OUT=results/runs.jsonl
CI=2.0
R() { python3 bench/run.py --ci "$CI" --out "$OUT" "$@"; }

R --backend seq --mode seq --mirror forgejo --n 1,2,4,8,16,32 --k 3 --reps 1 --label seq-forgejo-mirror
R --backend seq --mode seq --mirror github  --n 1,4,16        --k 2 --reps 1 --label seq-github-mirror
echo "sweep2 done"
