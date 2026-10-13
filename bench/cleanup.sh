#!/usr/bin/env bash
# Tear down everything the benchmark created outside the source tree. Run it yourself;
# each step prints what it removes.
set -u
cd "$(dirname "$0")/.."

echo "== local run directories (scratch repos, worktrees, jj workspaces)"
du -sh runs target-dev 2>/dev/null
rm -rf runs target-dev

echo "== Forgejo on the Talos cluster (namespace nerudo-bench, node-local data)"
kubectl --context admin@turingpi delete namespace nerudo-bench --wait=false

echo "== GitHub repo kcirtapfromspace/nerudo-bench"
echo "   needs the delete_repo scope once: gh auth refresh -h github.com -s delete_repo"
gh repo delete kcirtapfromspace/nerudo-bench --yes
