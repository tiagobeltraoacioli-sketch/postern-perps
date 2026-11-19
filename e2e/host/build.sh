#!/bin/bash
# Host side. usage: build.sh <label> <what...>   what: node-debug | node-release | engine-release | oracle-release
# Builds from $D/src-<label> into $D/target-<label> through the shared build-slot limiter. Writes $D/build-<label>.done|failed.
# UNAUDITED prototype. Copyright (c) 2026 Bloch Inc. All rights reserved.
D=$HOME/l2dexbridge-20261030/perps-r2-e2e
L=$1; shift
export PATH=$HOME/.cargo/bin:$PATH CARGO_TARGET_DIR=$D/target-$L
LOG=$D/build-$L.log
rm -f $D/build-$L.done $D/build-$L.failed
: > $LOG
rc=0
for w in "$@"; do
  case $w in
    node-debug)     F=dev-oracle; grep -q '^dev-clock' $D/src-$L/node/Cargo.toml && F=dev-oracle,dev-clock   # the dev clock too, where the tree has it
                    (cd $D/src-$L/node && $HOME/l2dexbridge-20261030/build-slot.sh cargo build -j 6 --locked --features $F --bins) >> $LOG 2>&1 || rc=1 ;;
    node-release)   (cd $D/src-$L/node && $HOME/l2dexbridge-20261030/build-slot.sh cargo build -j 6 --locked --release --bins) >> $LOG 2>&1 || rc=1 ;;
    engine-release) (cd $D/src-$L/engine && $HOME/l2dexbridge-20261030/build-slot.sh cargo build -j 6 --release --bins) >> $LOG 2>&1 || rc=1 ;;
    oracle-release) (cd $D/src-$L/node/oracle && $HOME/l2dexbridge-20261030/build-slot.sh cargo build -j 6 --release --locked -p postern-perps-oracle-feeder --bins) >> $LOG 2>&1 || rc=1 ;;
    *) echo "unknown target $w" >> $LOG; rc=1 ;;
  esac
done
if [ $rc = 0 ]; then touch $D/build-$L.done; else touch $D/build-$L.failed; fi
