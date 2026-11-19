#!/bin/bash
# Host side. usage: validator-start.sh <label> <profile> <index> <fresh|keep> <validator args...>
#                   validator-start.sh --again <index>     restart with the recorded arguments, data kept
# One committee member as its own process (service "validator-<index>", data in data/val-<index>).
# UNAUDITED prototype, throwaway keys, loopback only.
D=$HOME/l2dexbridge-20261030/perps-r2-e2e
mkdir -p $D/run
if [ "$1" = --again ]; then
  [ -f $D/run/validator-$2.args ] || { echo "no recorded arguments for validator $2" >&2; exit 1; }
  mapfile -t A < $D/run/validator-$2.args
  set -- "${A[0]}" "${A[1]}" "$2" keep "${A[@]:2}"
fi
L=$1; P=$2; I=$3; MODE=$4; shift 4
case "$I" in [0-6]) ;; *) echo "index 0..6" >&2; exit 1 ;; esac
case "$D" in */perps-r2-e2e) ;; *) echo "refusing: $D" >&2; exit 1 ;; esac
if [ "$MODE" = fresh ]; then rm -rf "$D/data/val-$I"; fi
mkdir -p $D/data/val-$I
printf '%s\n' "$L" "$P" "$@" > $D/run/validator-$I.args
exec $D/bin/svc.sh start validator-$I $D/target-$L/$P/postern-perps-node validator --index $I --data $D/data/val-$I "$@"
