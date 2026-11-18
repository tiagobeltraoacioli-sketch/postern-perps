#!/bin/bash
# Starts the local PosternDEX Perpetuals stack. UNAUDITED prototype: local processes, loopback only, throwaway keys.
#
#   e2e/up.sh [--label L] [--tree DIR] [--l2 anvil|fake] [--oracle replay|dev] [--local-validators N] [--frontend] [--keep]
#
# Build host (loopback): L2 (anvil chain 700770 + the pass-through proxy as second endpoint), the node
# (sequencer; validators in process and/or as separate processes), the oracle feeder. Mac: one ssh tunnel, the
# frontend dev server. Everything started is recorded by PID (Mac: e2e/.run/*.pid; host: run/*.pid) and stopped by
# e2e/down.sh. The tree <label> must have been built first: e2e/build.sh <label> [git-ref].
# --tree is the Mac checkout of the same tree (contracts, frontend, mock); default the integration worktree.
# --keep restarts the node on its existing data directory (recovery) instead of a fresh one.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd); . "$HERE/config.sh"
LABEL=main; L2=anvil; ORACLE=replay; LOCALV=7; FRONTEND=0; KEEP=fresh
TREE=$HOME/worktrees/perps-e2e-r2-int-20261118; [ -d "$TREE" ] || TREE=$(cd "$HERE/.." && pwd)
while [ $# -gt 0 ]; do
  case $1 in
    --label) LABEL=$2; shift 2 ;;
    --l2) L2=$2; shift 2 ;;
    --oracle) ORACLE=$2; shift 2 ;;
    --local-validators) LOCALV=$2; shift 2 ;;
    --frontend) FRONTEND=1; shift ;;
    --keep) KEEP=keep; shift ;;
    --tree) TREE=$2; shift 2 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done
RUN=$HERE/.run; mkdir -p "$RUN"
S="~/$E2E_SCRATCH"
say() { echo "[up] $*"; }
mac_alive() { [ -f "$RUN/$1.pid" ] && kill -0 "$(cat "$RUN/$1.pid")" 2>/dev/null; }
mac_start() { # name command...
  local n=$1; shift
  if mac_alive $n; then echo "$n already running (pid $(cat "$RUN/$n.pid")): run e2e/down.sh first" >&2; exit 1; fi
  nohup nice -n 10 "$@" >> "$RUN/$n.log" 2>&1 < /dev/null &
  echo $! > "$RUN/$n.pid"
}
wait_http() { # url seconds what
  for _ in $(seq 1 $(( $2 * 2 ))); do curl -fsS -m 3 -o /dev/null "$1" 2>/dev/null && return 0; sleep 0.5; done
  echo "timeout waiting for $3 ($1)" >&2; return 1
}

e2e_ssh "test -f $S/build-$LABEL.done" || { echo "tree '$LABEL' is not built on the host: e2e/build.sh $LABEL" >&2; exit 1; }
tar -C "$HERE/host" -cf - . | e2e_ssh "mkdir -p $S/bin && tar -x -C $S/bin && chmod +x $S/bin/*.sh $S/bin/*.py"

# ---- tunnel (Mac -> host; and, for the fake L2 only, host -> Mac) ----
FWD="-L 127.0.0.1:$M_API:127.0.0.1:$H_API -L 127.0.0.1:$M_ANVIL:127.0.0.1:$H_ANVIL -L 127.0.0.1:$M_PROXY:127.0.0.1:$H_PROXY"
[ "$L2" = fake ] && FWD="$FWD -R 127.0.0.1:53512:127.0.0.1:49912 -R 127.0.0.1:53513:127.0.0.1:49913"
mac_start tunnel ssh -N -o BatchMode=yes -o ExitOnForwardFailure=yes -o ServerAliveInterval=30 -i "$E2E_SSH_KEY" $FWD "$E2E_HOST"
sleep 2; mac_alive tunnel || { echo "tunnel failed:" >&2; tail -3 "$RUN/tunnel.log" >&2; exit 1; }
say "tunnel up: API http://127.0.0.1:$M_API, L2 http://127.0.0.1:$M_ANVIL"

NODE_ARGS="--listen 127.0.0.1:$H_API --seed $E2E_SEED --block-ms 200 --checkpoint-interval 10 --dev-default-country ZZ"
PROFILE=release
VAULT=; TOKEN=

# ---- L2 ----
if [ "$L2" = anvil ]; then
  # 4 slots per epoch: `finalized` trails `latest` by 8 blocks (8 s at 1 s blocks), so "seen, not yet final" is observable
  e2e_ssh "$S/bin/svc.sh start anvil \$HOME/.foundry/bin/anvil --host 127.0.0.1 --port $H_ANVIL --chain-id $E2E_CHAIN_ID --block-time 1 --slots-in-an-epoch 4 --silent && $S/bin/svc.sh start proxy python3 $S/bin/rpc-proxy.py http://127.0.0.1:$H_ANVIL $H_PROXY"
  wait_http "http://127.0.0.1:$M_PROXY/__fault" 20 "L2 proxy"
  . "$HERE/deploy.sh"   # sets VAULT, TOKEN; writes .run/deployment.json
  NODE_ARGS="$NODE_ARGS --l2 http://127.0.0.1:$H_ANVIL,http://127.0.0.1:$H_PROXY --vault $VAULT --token $TOKEN"
else
  NODE_ARGS="$NODE_ARGS --l2 http://127.0.0.1:53512,http://127.0.0.1:53513"
fi

# ---- oracle ----
if [ "$ORACLE" = dev ]; then
  NODE_ARGS="$NODE_ARGS --dev-oracle"; PROFILE=debug   # the dev oracle exists only in debug builds (3ce7e64 gate)
else
  . "$HERE/oracle.sh" prepare   # token file + committee on the host; appends to NODE_ARGS
fi
[ "$LOCALV" != 7 ] && NODE_ARGS="$NODE_ARGS --local-validators $LOCALV"

# ---- node ----
e2e_ssh "$S/bin/node-start.sh $LABEL $PROFILE $KEEP $NODE_ARGS"
wait_http "http://127.0.0.1:$M_API/v1/status" 60 "node API"
STATUS=$(curl -fsS "http://127.0.0.1:$M_API/v1/status")
NODE_VAULT=$(printf '%s' "$STATUS" | "$NODE_BIN" -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>console.log(JSON.parse(s).network.vault))')
# the engine's signing domain must be bound to the vault just deployed; a node that ignored --vault is refused here
if [ -n "$VAULT" ] && [ "$NODE_VAULT" != "$VAULT" ]; then echo "the node's vault $NODE_VAULT is not the deployed vault $VAULT (does this tree's node take --vault?)" >&2; exit 1; fi
VAULT=$NODE_VAULT
say "node up (tree $LABEL, $PROFILE, engine vault $VAULT)"

if [ "$L2" = fake ]; then
  DEP=$("$NODE_BIN" "$HERE/rehearsal/accounts.mjs" --fake-l2-args)
  mac_start fake-l2-a "$NODE_BIN" "$TREE/node/mock/conformance/fake-l2.mjs" --port 49912 --vault "$VAULT" $DEP
  mac_start fake-l2-b "$NODE_BIN" "$TREE/node/mock/conformance/fake-l2.mjs" --port 49913 --vault "$VAULT" $DEP
  say "fake L2 up twice (stand-in: fixed deposits, no vault contract)"
fi
[ "$ORACLE" = replay ] && . "$HERE/oracle.sh" start

# ---- separate validator processes (indices LOCALV..6) ----
if [ "$LOCALV" != 7 ]; then
  for i in $(seq "$LOCALV" 6); do
    e2e_ssh "$S/bin/validator-start.sh $LABEL $PROFILE $i fresh --seed $E2E_SEED --sequencer http://127.0.0.1:$H_API ${VAULT:+--vault $VAULT} $( [ "$L2" = anvil ] && echo --l2 http://127.0.0.1:$H_ANVIL,http://127.0.0.1:$H_PROXY || echo --l2 http://127.0.0.1:53512,http://127.0.0.1:53513 )"
  done
fi

# ---- frontend dev server ----
if [ "$FRONTEND" = 1 ]; then . "$HERE/frontend.sh"; fi

cat > "$RUN/stack.json" <<JSON
{"label": "$LABEL", "tree": "$TREE", "commit": "$(e2e_ssh "cat $S/src-$LABEL/.commit")", "l2": "$L2", "oracle": "$ORACLE", "localValidators": $LOCALV,
 "seed": "$E2E_SEED", "api": "http://127.0.0.1:$M_API", "l2Rpc": "http://127.0.0.1:$M_ANVIL", "l2Proxy": "http://127.0.0.1:$M_PROXY",
 "hostApi": "http://127.0.0.1:$H_API", "vault": "$VAULT", "token": "$TOKEN", "frontend": $( [ "$FRONTEND" = 1 ] && echo "\"http://127.0.0.1:$M_FRONTEND\"" || echo null ),
 "oracleCommittee": $( [ -n "${ORACLE_LIST:-}" ] && printf '["%s"]' "$(printf '%s' "$ORACLE_LIST" | sed 's/,/","/g')" || echo null ), "oracleQuorum": 5,
 "startedAt": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"}
JSON
say "stack up; description in $RUN/stack.json; stop with e2e/down.sh"
