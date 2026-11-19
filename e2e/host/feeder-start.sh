#!/bin/bash
# Host side. usage: feeder-start.sh <label> <tape-name> <replay-feed args...>   the tape (JSONL) is read from stdin
#                   feeder-start.sh --tape <tape-name>     same arguments as the last start, another tape from stdin
# Stops the running feeder (by PID) and starts `perps-oracle replay-feed` on the new tape as service "feeder".
# UNAUDITED prototype. Copyright (c) 2026 Bloch Inc. All rights reserved.
D=$HOME/l2dexbridge-20261030/perps-r2-e2e
if [ "$1" = --tape ]; then
  [ -f $D/run/feeder.args ] || { echo "no recorded feeder arguments" >&2; exit 1; }
  mapfile -t A < $D/run/feeder.args
  set -- "${A[0]}" "$2" "${A[@]:1}"
fi
L=$1; T=$2; shift 2
case "$T" in *[!a-z0-9-]*|"") echo "tape name: a-z 0-9 -" >&2; exit 2 ;; esac
cat > $D/run/tape-$T.jsonl
[ -s $D/run/tape-$T.jsonl ] || { echo "empty tape" >&2; exit 1; }
printf '%s\n' "$L" "$@" > $D/run/feeder.args
$D/bin/svc.sh stop feeder TERM > /dev/null
exec $D/bin/svc.sh start feeder $D/target-$L/release/perps-oracle replay-feed --tape $D/run/tape-$T.jsonl --token-file $D/run/oracle-token --key-dir $D/data/oracle-keys --out $D/run/feeder-$T.out.jsonl "$@"
