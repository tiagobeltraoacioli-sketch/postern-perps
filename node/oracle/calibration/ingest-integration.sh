#!/bin/bash
# Integration of the oracle feeder with 431's node (ingest): run in ~/l2dexbridge-20261030/perps-r2-oracle/run/int
set -u
cd ~/l2dexbridge-20261030/perps-r2-oracle/run/int
VAULT=0x798b19857aeb26aea5277777c5e91d75c45d2753
PORT=53230
cat > tape.jsonl <<'T'
{"p":{"1":"67250","2":"3480","3":"182.4"},"repeat":6,"note":"calm: INIT -> active on the third round"}
{"ramp":{"1":"65232.5"},"rounds":6,"note":"BTC -3% in 6 rounds"}
{"p":{"2":"3100"},"repeat":4,"note":"ETH gap -10.9%: JUMP halt, resumed on the third agreeing round"}
{"p":{"1":"65300"},"override":{"6":{"1":"70000"},"5":{"1":"50000"}},"repeat":2,"note":"two lying signers"}
{"p":{"1":"65310"},"signers":[0,1,2,3,4],"repeat":2,"note":"quorum only"}
{"silent":5,"note":"15 s silent: STALE"}
{"p":{"1":"65320"},"repeat":5,"note":"back"}
T
rm -rf data; mkdir data
nohup ./node dev --data data --listen 127.0.0.1:$PORT --engine real --oracle-committee-file keys/committee.json --oracle-token-file tok > node.log 2>&1 < /dev/null &
echo $! > node.pid
sleep 3
( while kill -0 $(cat node.pid) 2>/dev/null; do
    for s in BTC-USD ETH-USD; do
      echo "{\"t\":$(date +%s%3N),\"sym\":\"$s\",\"oracle\":$(curl -s -m 2 http://127.0.0.1:$PORT/v1/oracle/$s || echo null)}"
    done
    sleep 1.5
  done ) > poll.jsonl 2>/dev/null &
echo $! > poll.pid
./perps-oracle replay-feed --tape tape.jsonl --vault $VAULT --key-dir keys --node http://127.0.0.1:$PORT --token-file tok --out feed-updates.jsonl 2> feed-updates.err
echo "updates exit=$?" > result.txt
# per-signer mode, 5 rounds
printf '{"p":{"1":"65330","2":"3101","3":"182.5"},"repeat":5}\n' > tape2.jsonl
./perps-oracle replay-feed --tape tape2.jsonl --vault $VAULT --key-dir keys --node http://127.0.0.1:$PORT --token-file tok --mode observations --strict --out feed-obs.jsonl 2> feed-obs.err
echo "observations exit=$?" >> result.txt
# refusals: wrong token, wrong vault (other deployment), a committee the genesis does not name
(umask 077; echo wrong-token-0123456789abcdef > badtok)
printf '{"p":{"1":"65340"}}\n' > tape3.jsonl
./perps-oracle replay-feed --tape tape3.jsonl --vault $VAULT --key-dir keys --node http://127.0.0.1:$PORT --token-file badtok --out feed-badtok.jsonl 2> feed-badtok.err
./perps-oracle replay-feed --tape tape3.jsonl --vault 0x00000000000000000000000000000000000000d1 --key-dir keys --node http://127.0.0.1:$PORT --token-file tok --out feed-badvault.jsonl 2> feed-badvault.err
./perps-oracle replay-feed --tape tape3.jsonl --vault $VAULT --node-dev-seed dev --node http://127.0.0.1:$PORT --token-file tok --out feed-badkeys.jsonl 2> feed-badkeys.err
sleep 2
curl -s -m 2 http://127.0.0.1:$PORT/v1/oracle/BTC-USD > final-btc.json
curl -s -m 2 http://127.0.0.1:$PORT/v1/tickers > final-tickers.json
curl -s -m 2 http://127.0.0.1:$PORT/v1/oracle/rounds/latest > final-rounds.json
kill $(cat poll.pid) 2>/dev/null
kill -TERM $(cat node.pid) 2>/dev/null
sleep 2
kill -0 $(cat node.pid) 2>/dev/null && echo "node still alive" >> result.txt || echo "node stopped" >> result.txt
echo DONE >> result.txt
