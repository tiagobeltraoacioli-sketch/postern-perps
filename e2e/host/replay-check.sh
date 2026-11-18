#!/bin/bash
# Host side. usage: replay-check.sh <label> <node-url> <vault> <oracle-signers-csv> <quorum> [genesis-json-file]
# Independent replay: downloads the node's published input log, rebuilds the engine genesis from the compliance
# files with the engine CLI (dev overrides: launch seeds 0, as node/src/engine/real.rs states), replays every block
# with the engine CLI (a separate binary from the node) and compares the state root and log hash of EVERY height
# with what the node published. Prints one JSON line. UNAUDITED prototype.
D=$HOME/l2dexbridge-20261030/perps-r2-e2e
L=$1; URL=$2; VAULT=$3; SIGNERS=$4; Q=$5; GEN=${6:-}
W=$D/run/replay; rm -rf $W; mkdir -p $W
E=$D/target-$L/release/engine
[ -x $E ] || { echo '{"ok":false,"error":"no engine CLI built for this label"}'; exit 1; }
curl -fsS -m 120 "$URL/v1/blocks/stream?from=1" > $W/blocks.ndjson || { echo '{"ok":false,"error":"cannot download the block stream"}'; exit 1; }
if [ -n "$GEN" ]; then cp "$GEN" $W/genesis.json; else
  $E genesis $D/src-$L/compliance/markets.json $D/src-$L/compliance/risk.json --vault $VAULT --oracle-signers $SIGNERS --quorum $Q --backstop-seed 0 > $W/genesis.raw 2> $W/genesis.err || { echo "{\"ok\":false,\"error\":\"engine genesis failed: $(head -c 200 $W/genesis.err | tr -d '\"\n')\"}"; exit 1; }
  python3 - $W <<'PY'
import json, sys
w = sys.argv[1]
g = json.load(open(w + "/genesis.raw"))
g["launchInsuranceMin"] = "0"; g["launchBackstopMin"] = "0"
json.dump(g, open(w + "/genesis.json", "w"), separators=(",", ":"))
PY
fi
python3 - $W <<'PY'
import json, sys
w = sys.argv[1]
with open(w + "/blocks.ndjson") as f, open(w + "/log.jsonl", "w") as o, open(w + "/expect.txt", "w") as x:
    for line in f:
        if not line.strip():
            continue
        b = json.loads(line)
        o.write(json.dumps({"height": str(b["height"]), "time": str(b["time"]), "inputs": b["inputsEncoded"]}) + "\n")
        x.write("%d %s %s\n" % (b["height"], b["stateRoot"], b["logHash"]))
PY
$E replay $W/genesis.json $W/log.jsonl > $W/replay.out 2> $W/replay.err; rc=$?
python3 - $W $rc <<'PY'
import json, sys
w, rc = sys.argv[1], int(sys.argv[2])
exp = [l.split() for l in open(w + "/expect.txt")]
got = [l.split()[:3] for l in open(w + "/replay.out") if l.strip()]
n = min(len(exp), len(got))
first_bad = next((exp[i][0] for i in range(n) if exp[i] != got[i]), None)
ok = rc == 0 and len(exp) == len(got) and first_bad is None and n > 0
err = open(w + "/replay.err").read().strip()[:300]
print(json.dumps({"ok": ok, "blocks": len(exp), "replayed": len(got), "firstMismatch": first_bad, "engineExit": rc,
                  "lastHeight": int(exp[-1][0]) if exp else 0, "lastStateRoot": exp[-1][1] if exp else None, "engineError": err or None}))
PY
