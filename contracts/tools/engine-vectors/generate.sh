#!/bin/bash
# Copyright (c) 2026 Bloch Inc.
# PROTOTYPE, UNAUDITED. Regenerates contracts/test/vectors/engine/*.json from the real engine.
# Rust never builds on the Mac: run this ON THE BUILD HOST, from this directory, with your own target directory:
#
#   CARGO_TARGET_DIR=<scratch>/target ~/l2dexbridge-20261030/build-slot.sh ./generate.sh <out-dir>
#
# The vault address is the one `script/local_deploy.py` gets on a fresh anvil (deployer = anvil account 0, nonce 1);
# `test/EngineVectors.t.sol` deploys the vault at the same address, so one set of vectors serves forge and anvil.
set -euo pipefail
out="${1:?usage: generate.sh <out-dir>}"
vault=0xe7f1725E7734CE288F8367e1Bb143E90bb3F0512
cargo build --release --locked -j 6
bin="${CARGO_TARGET_DIR:-target}/release/perps-vault-engine-vectors"
mkdir -p "$out"
for story in normal insurance backstop shortfall; do
  "$bin" --story "$story" --vault "$vault" --chain-id 700770 --t0 1800000000 > "$out/$story.json"
  # determinism: a second run gives the same bytes
  "$bin" --story "$story" --vault "$vault" --chain-id 700770 --t0 1800000000 | cmp - "$out/$story.json"
  echo "$story: $(wc -c < "$out/$story.json") bytes"
done
