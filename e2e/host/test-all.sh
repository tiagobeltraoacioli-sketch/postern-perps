#!/bin/bash
# Host side. usage: test-all.sh <label>      the whole Rust test set of the tree src-<label>, through build-slot:
#   engine (release), node (release, dev oracle off), node (debug, dev oracle on), the release build with the dev
#   oracle MUST fail to compile (3ce7e64 gate), node/oracle workspace (release), compliance/geoblock/rust.
# Writes run/test-<label>.txt: one line per suite with pass/fail counts, then `test-<label>.done`.
# UNAUDITED prototype. Copyright (c) 2026 Bloch Inc. All rights reserved.
D=$HOME/l2dexbridge-20261030/perps-r2-e2e
L=$1
S=$D/src-$L; R=$D/run/test-$L.txt; LOGS=$D/run/test-$L; mkdir -p $LOGS
export PATH=$HOME/.cargo/bin:$PATH CARGO_TARGET_DIR=$D/target-$L PERPS_TEST_TMP=$D/run/tmp-$L
mkdir -p $PERPS_TEST_TMP
rm -f $D/run/test-$L.done; : > $R
slot() { $HOME/l2dexbridge-20261030/build-slot.sh "$@"; }
count() { # name logfile rc
  local p f i
  p=$(grep -E '^test result:' "$2" | sed -E 's/.* ([0-9]+) passed.*/\1/' | paste -sd+ | bc 2>/dev/null); p=${p:-0}
  f=$(grep -E '^test result:' "$2" | sed -E 's/.* ([0-9]+) failed.*/\1/' | paste -sd+ | bc 2>/dev/null); f=${f:-0}
  i=$(grep -E '^test result:' "$2" | sed -E 's/.* ([0-9]+) ignored.*/\1/' | paste -sd+ | bc 2>/dev/null); i=${i:-0}
  echo "$1: passed $p failed $f ignored $i rc $3" >> $R
}
suite() { # name dir command...
  local n=$1 d=$2; shift 2
  if [ ! -d "$S/$d" ]; then echo "$n: ABSENT ($d)" >> $R; return; fi
  (cd $S/$d && slot "$@") > $LOGS/$n.log 2>&1; count $n $LOGS/$n.log $?
}
suite engine engine cargo test --release -j 6 --no-fail-fast
suite node-release node cargo test --release --locked -j 6 --no-fail-fast
suite node-dev-oracle node cargo test --locked -j 6 --no-fail-fast --features dev-oracle
(cd $S/node && slot cargo build --release --locked -j 6 --features dev-oracle) > $LOGS/gate.log 2>&1
if [ $? -ne 0 ]; then echo "dev-oracle-release-gate: refused to compile (as required)" >> $R; else echo "dev-oracle-release-gate: COMPILED (gate broken)" >> $R; fi
if grep -q '^dev-clock' $S/node/Cargo.toml 2>/dev/null; then
  suite node-dev-clock node cargo test --locked -j 6 --no-fail-fast --features dev-oracle,dev-clock
  (cd $S/node && slot cargo build --release --locked -j 6 --features dev-clock) > $LOGS/gate-clock.log 2>&1
  if [ $? -ne 0 ]; then echo "dev-clock-release-gate: refused to compile (as required)" >> $R; else echo "dev-clock-release-gate: COMPILED (gate broken)" >> $R; fi
fi
if [ -f $S/node/tests/anvil.rs ]; then
  # the scenario against the real vault on a private anvil (ignored by default: it needs anvil and forge artefacts)
  (cd $S/node && PATH=$HOME/.foundry/bin:$PATH PERPS_ANVIL_DIR=$HOME/.foundry/bin PERPS_CONTRACTS_DIR=$S/contracts PERPS_ANVIL_PORT=53580 slot cargo test --locked -j 6 --features dev-oracle --test anvil -- --ignored) > $LOGS/node-anvil.log 2>&1; count node-anvil $LOGS/node-anvil.log $?
fi
suite oracle node/oracle cargo test --release --workspace -j 6 --no-fail-fast
suite geoblock-rust compliance/geoblock/rust cargo test --release -j 6 --no-fail-fast
touch $D/run/test-$L.done
