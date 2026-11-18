#!/bin/bash
# Host side. usage: oracle-prepare.sh <label> <fresh|keep>     prints the oracle committee as 0xA,0xB,...
# fresh: new THROWAWAY oracle keys (perps-oracle committee-gen, 0600, data/oracle-keys) and a new ingest token
# (32 random bytes as hex, 0600, run/oracle-token). keep: reuses both (a restart on the same genesis).
# Nothing secret is printed. UNAUDITED prototype. Copyright (c) 2026 Bloch Inc. All rights reserved.
set -e
D=$HOME/l2dexbridge-20261030/perps-r2-e2e
L=$1; MODE=$2
O=$D/target-$L/release/perps-oracle
[ -x $O ] || { echo "no perps-oracle binary for $L (e2e/build.sh)" >&2; exit 1; }
case "$D" in */perps-r2-e2e) ;; *) echo "refusing: $D" >&2; exit 1 ;; esac
mkdir -p $D/run $D/data
if [ "$MODE" = fresh ] || [ ! -f $D/data/oracle-keys/committee.json ]; then
  rm -rf $D/data/oracle-keys
  $O committee-gen --n 7 --quorum 5 --out $D/data/oracle-keys > $D/run/committee-gen.out 2> $D/run/committee-gen.err
  (umask 077; head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > $D/run/oracle-token)
fi
python3 - $D/data/oracle-keys/committee.json <<'PY'
import json, sys
c = json.load(open(sys.argv[1]))
s = c.get("signers") or c.get("oracleSigners") or c.get("committee") or c.get("addresses")
assert isinstance(s, list) and len(s) == 7, "committee.json: no list of 7 signers"
print(",".join(x if isinstance(x, str) else x["address"] for x in s))
PY
