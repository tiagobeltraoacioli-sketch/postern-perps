#!/bin/bash
# Supervised ssh tunnel of the e2e stack: restarts ssh whenever it exits (a network drop), logs every restart with
# its time, and stops its ssh child when it is stopped itself (down.sh signals THIS script's PID only).
# usage: tunnel-loop.sh <forward options...>     UNAUDITED prototype. Copyright (c) 2026 Bloch Inc. All rights reserved.
HERE=$(cd "$(dirname "$0")" && pwd); . "$HERE/config.sh"
child=
trap '[ -n "$child" ] && kill "$child" 2>/dev/null; exit 0' TERM INT
n=0
while :; do
  ssh -N -o BatchMode=yes -o ExitOnForwardFailure=yes -o ServerAliveInterval=15 -o ServerAliveCountMax=8 -o TCPKeepAlive=yes -o ConnectTimeout=20 -i "$E2E_SSH_KEY" "$@" "$E2E_HOST" &
  child=$!
  wait "$child"
  n=$((n + 1))
  echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) TUNNEL RESTART $n (ssh exited $?)" | tee -a "$HERE/.run/tunnel-restarts.log"
  sleep 2
done
