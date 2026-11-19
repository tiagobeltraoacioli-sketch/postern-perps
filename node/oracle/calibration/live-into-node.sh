#!/bin/bash
# The LIVE feeder (real public venues, read-only) into 431's node ingest on loopback: 25 rounds.
# Run in ~/l2dexbridge-20261030/perps-r2-oracle/run/int after int.sh (same keys, token, node binary).
set -u
cd ~/l2dexbridge-20261030/perps-r2-oracle/run/int
VAULT=0x798b19857aeb26aea5277777c5e91d75c45d2753
PORT=53230
rm -rf data-live live; mkdir data-live live
nohup ./node dev --data data-live --listen 127.0.0.1:$PORT --engine real --oracle-committee-file keys/committee.json --oracle-token-file tok > live/node.log 2>&1 < /dev/null &
echo $! > node.pid
sleep 3
( while kill -0 $(cat node.pid) 2>/dev/null; do
    for s in BTC-USD ETH-USD SOL-USD; do
      echo "{\"t\":$(date +%s%3N),\"sym\":\"$s\",\"oracle\":$(curl -s -m 2 http://127.0.0.1:$PORT/v1/oracle/$s || echo null)}"
    done
    sleep 1.5
  done ) > live/poll.jsonl 2>/dev/null &
echo $! > poll.pid
../perps-oracle-net run --key-dir keys --vault $VAULT --node http://127.0.0.1:$PORT --token-file tok --rounds 25 --out live/run.jsonl 2> live/run.err
echo "run exit=$?" > live/result.txt
sleep 2
curl -s -m 2 http://127.0.0.1:$PORT/v1/oracle/rounds/latest > live/final-rounds.json
curl -s -m 2 http://127.0.0.1:$PORT/v1/tickers > live/final-tickers.json
kill $(cat poll.pid) 2>/dev/null
kill -TERM $(cat node.pid) 2>/dev/null
sleep 2
kill -0 $(cat node.pid) 2>/dev/null && echo "node still alive" >> live/result.txt || echo "node stopped" >> live/result.txt
echo DONE >> live/result.txt
