#!/bin/bash
# Host side. usage: clock-advance.sh <new-offset-ms>
# The dev engine clock is a START-TIME offset: to move it, every process of the deployment is stopped and started
# again with the same larger offset, on its existing data. Rewrites the value after --dev-clock-offset-ms in the
# recorded arguments of the sequencer and of every separate validator, then restarts them (PID files only).
# UNAUDITED prototype. Copyright (c) 2026 Bloch Inc. All rights reserved.
D=$HOME/l2dexbridge-20261030/perps-r2-e2e
N=$1
case "$N" in ''|*[!0-9]*) echo "offset: milliseconds" >&2; exit 2 ;; esac
for i in 0 1 2 3 4 5 6; do [ -f $D/run/validator-$i.pid ] && $D/bin/svc.sh stop validator-$i TERM > /dev/null; done
$D/bin/svc.sh stop node INT > /dev/null
for f in $D/run/node.args $D/run/validator-[0-6].args; do
  [ -f "$f" ] || continue
  python3 - "$f" "$N" <<'PY'
import sys
p, n = sys.argv[1], sys.argv[2]
a = open(p).read().split("\n")
out, hit = [], False
for x in a:
    parts = x.split(" ")
    for k, w in enumerate(parts):
        if w == "--dev-clock-offset-ms" and k + 1 < len(parts):
            parts[k + 1] = n; hit = True
    out.append(" ".join(parts))
for k, w in enumerate(out):
    if w == "--dev-clock-offset-ms" and k + 1 < len(out):
        out[k + 1] = n; hit = True
assert hit, "no --dev-clock-offset-ms in " + p
open(p, "w").write("\n".join(out))
PY
done
$D/bin/node-start.sh --again || exit 1
for f in $D/run/validator-[0-6].args; do [ -f "$f" ] || continue; i=${f##*validator-}; i=${i%.args}; $D/bin/validator-start.sh --again $i || exit 1; done
echo "clock offset now $N ms"
