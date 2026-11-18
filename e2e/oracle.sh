# Sourced by up.sh (--oracle replay): `prepare` before the node starts, `start` after it answers.
# 432's replay source through 431's authenticated ingest: a dev oracle committee of fresh THROWAWAY keys generated on
# the host at run time (perps-oracle committee-gen: 0600 files, never committed, never printed), a bearer token file
# generated on the host (never a flag value, never logged), and `perps-oracle replay-feed` playing a tape.
# UNAUDITED prototype. Copyright (c) 2026 Bloch Inc. All rights reserved.
case $1 in
  prepare)
    ORACLE_LIST=$(e2e_ssh "$S/bin/oracle-prepare.sh $LABEL $KEEP") || { echo "oracle committee generation failed" >&2; exit 1; }
    case "$ORACLE_LIST" in 0x*,0x*) ;; *) echo "unexpected committee list from the host: $ORACLE_LIST" >&2; exit 1 ;; esac
    NODE_ARGS="$NODE_ARGS --oracle-committee $ORACLE_LIST --oracle-quorum 5 --oracle-token-file \$HOME/$E2E_SCRATCH/run/oracle-token"
    ;;
  start)
    printf '%s\n' '{"p":{"1":"67250","2":"3480","3":"182.4"},"repeat":1000}' | e2e_ssh "$S/bin/feeder-start.sh $LABEL base --vault $VAULT --node http://127.0.0.1:$H_API --loop"
    for _ in $(seq 1 40); do
      IDX=$(curl -fsS -m 3 "http://127.0.0.1:$M_API/v1/markets/BTC-USD/ticker" | "$NODE_BIN" -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{try{console.log(JSON.parse(s).indexPrice)}catch{console.log("0")}})')
      [ "$IDX" != 0 ] && [ -n "$IDX" ] && break; sleep 1
    done
    if [ "$IDX" = 0 ] || [ -z "$IDX" ]; then echo "no index price 40 s after the feeder started (does this tree's node take --oracle-committee / --oracle-token-file?)" >&2; e2e_ssh "tail -3 $S/run/feeder.log" >&2; exit 1; fi
    say "oracle feeder up (replay tape through the authenticated ingest; BTC index $IDX)"
    ;;
esac
