#!/bin/bash
# Host side. usage: node-start.sh <label> <profile debug|release> <fresh|keep> <node args...>
#                   node-start.sh --again            restart with the recorded arguments on the existing data (recovery)
# Starts the sequencer process of the built tree <label> as service "node" (data in data/seq). "fresh" wipes the data
# directory first; "keep" restarts on the existing log. UNAUDITED prototype, throwaway keys, loopback only.
D=$HOME/l2dexbridge-20261030/perps-r2-e2e
mkdir -p $D/run
if [ "$1" = --again ]; then
  [ -f $D/run/node.args ] || { echo "no recorded node arguments" >&2; exit 1; }
  mapfile -t A < $D/run/node.args
  set -- "${A[0]}" "${A[1]}" keep "${A[@]:2}"
fi
L=$1; P=$2; MODE=$3; shift 3
[ -x $D/target-$L/$P/postern-perps-node ] || { echo "no binary for $L/$P" >&2; exit 1; }
if [ "$MODE" = fresh ]; then
  case "$D" in */perps-r2-e2e) rm -rf "$D/data/seq" ;; *) echo "refusing to wipe $D" >&2; exit 1 ;; esac
fi
mkdir -p $D/data/seq
printf '%s\n' "$L" "$P" "$@" > $D/run/node.args
exec $D/bin/svc.sh start node $D/target-$L/$P/postern-perps-node dev --data $D/data/seq "$@"
