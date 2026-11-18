#!/bin/bash
# Host side. usage: node-start.sh <label> <profile debug|release> <fresh|keep> <node args...>
# Starts the sequencer process of the built tree <label> as service "node" (data in data/seq). "fresh" wipes the data
# directory first; "keep" restarts on the existing log (recovery). UNAUDITED prototype, throwaway keys, loopback only.
D=$HOME/l2dexbridge-20261030/perps-r2-e2e
L=$1; P=$2; MODE=$3; shift 3
[ -x $D/target-$L/$P/postern-perps-node ] || { echo "no binary for $L/$P" >&2; exit 1; }
if [ "$MODE" = fresh ]; then
  case "$D" in */perps-r2-e2e) rm -rf "$D/data/seq" ;; *) echo "refusing to wipe $D" >&2; exit 1 ;; esac
fi
mkdir -p $D/data/seq
exec $D/bin/svc.sh start node $D/target-$L/$P/postern-perps-node dev --data $D/data/seq "$@"
