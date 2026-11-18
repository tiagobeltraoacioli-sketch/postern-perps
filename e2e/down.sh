#!/bin/bash
# Stops everything e2e/up.sh started, by PID only (never by name or pattern). Safe to run twice.
# UNAUDITED prototype. Copyright (c) 2026 Bloch Inc. All rights reserved.
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd); . "$HERE/config.sh"
RUN=$HERE/.run
e2e_ssh "~/$E2E_SCRATCH/bin/svc.sh stop-all" || echo "[down] host not reachable: host services NOT stopped" >&2
for f in "$RUN"/*.pid; do
  [ -e "$f" ] || continue
  n=$(basename "$f" .pid); p=$(cat "$f")
  # only a live process that is still our child command (ssh, node) is signalled
  if kill -0 "$p" 2>/dev/null; then
    kill "$p" 2>/dev/null
    for _ in $(seq 1 50); do kill -0 "$p" 2>/dev/null || break; sleep 0.1; done
    kill -0 "$p" 2>/dev/null && kill -KILL "$p" 2>/dev/null
    echo "[down] $n: stopped pid $p"
  else
    echo "[down] $n: not running"
  fi
  rm -f "$f"
done
