#!/usr/bin/env bash
# Builds the ts-rust WebAssembly module that nerudo-ci uses, from the pinned commit, and
# writes it to cf/nerudo-ci/vendor/ts-rust/ts_rust.wasm.
#
# needs: git, rustup (target wasm32-wasip1), wasm-opt from binaryen 132 or later, node
set -euo pipefail
commit=887da59f7f02e91d362304b9e36d5f64ba4f7295
cf="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
out="$cf/nerudo-ci/vendor/ts-rust/ts_rust.wasm"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

rustup target add wasm32-wasip1
git -c credential.helper= clone --quiet --filter=blob:none https://github.com/pingdotgg/ts-rust.git "$work/ts-rust"
git -C "$work/ts-rust" checkout --quiet "$commit"
bash "$work/ts-rust/scripts/wasm/build.sh" "$out"
shasum -a 256 "$out"
