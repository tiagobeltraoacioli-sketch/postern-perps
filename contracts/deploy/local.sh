#!/bin/bash
# Copyright (c) 2026 Bloch Inc.
# PROTOTYPE, UNAUDITED. Local full-stack deployment on a PRIVATE anvil (never a real network):
#
#   contracts/deploy/local.sh <rpc-url> <committee.json> <out.json> [extra local_deploy.py options]
#
# committee.json = {"validators": [7 addresses], "threshold": 5, "unpauseThreshold": 6 (optional),
#                   "guardians": [3 addresses] (optional: generated at run time next to out.json, which must then
#                   be outside git or git-ignored)}
# out.json       = {chainId, token, vault, deployBlock, committee, threshold, forcedDelay, escapeDelay, largeDelay,
#                   abi: {vault: [...], token: [...]}, ...}; the directory of out.json also receives
#                   deployment.json and the two ABI files.
# Foundry: $FOUNDRY_DIR, else ~/.local/share/dex-tools/foundry, else ~/.foundry/bin, else PATH.
# Delays are the production-shape proposal (forcedDelay 2 d, escapeDelay 7 d): a rehearsal moves anvil's clock.
set -euo pipefail
if [ $# -lt 3 ]; then
  echo "usage: $0 <rpc-url> <committee.json> <out.json> [local_deploy.py options]" >&2
  exit 2
fi
here="$(cd "$(dirname "$0")" && pwd)"
rpc="$1"; committee="$2"; out="$3"; shift 3
dir="${FOUNDRY_DIR:-}"
if [ -z "$dir" ]; then
  for candidate in "$HOME/.local/share/dex-tools/foundry" "$HOME/.foundry/bin"; do
    if [ -x "$candidate/cast" ]; then dir="$candidate"; break; fi
  done
fi
mkdir -p "$(dirname "$out")"
exec python3 "$here/../script/local_deploy.py" --rpc "$rpc" --committee-file "$committee" --out-json "$out" \
  --out "$(dirname "$out")" ${dir:+--foundry-dir "$dir"} "$@"
