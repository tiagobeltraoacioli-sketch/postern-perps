#!/bin/bash
# Host side. usage: clock-refused.sh <label>   the RELEASE node binary (no dev-clock feature) must refuse the flag.
D=$HOME/l2dexbridge-20261030/perps-r2-e2e
B=$D/target-$1/release/postern-perps-node
[ -x $B ] || { echo "no release binary"; exit 0; }
T=$(mktemp -d $D/run/clockneg.XXXXXX)
timeout 20 $B dev --data $T --listen 127.0.0.1:53590 --seed neg --dev-clock-offset-ms 1000 > $T/out.log 2>&1
rc=$?
if [ $rc -ne 0 ] && [ $rc -ne 124 ]; then echo "REFUSED (exit $rc): $(head -c 240 $T/out.log | tr '\n' ' ')"; else echo "STARTED (exit $rc): the release binary accepted the dev clock flag"; fi
