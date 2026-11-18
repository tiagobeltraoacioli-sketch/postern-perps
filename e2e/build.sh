#!/bin/bash
# usage: e2e/build.sh <label> [git-ref] [tree-dir]   syncs the tree to the host and builds node (debug with the dev
# oracle, and release) and the engine CLI there through the shared build-slot limiter; waits for the result.
# Rust never builds on the Mac. UNAUDITED prototype. Copyright (c) 2026 Bloch Inc. All rights reserved.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd); . "$HERE/config.sh"
L=$1
bash "$HERE/sync.sh" "$@"
e2e_ssh "cd ~/$E2E_SCRATCH && rm -f build-$L.done build-$L.failed && (setsid nohup bin/build.sh $L node-debug node-release engine-release > /dev/null 2>&1 < /dev/null & echo \$! > run/build-$L.pid)"
echo "building $L on the host (log: ~/$E2E_SCRATCH/build-$L.log)"
while :; do
  r=$(e2e_ssh "cd ~/$E2E_SCRATCH && (test -f build-$L.done && echo done) || (test -f build-$L.failed && echo failed) || echo wait")
  [ "$r" = done ] && { echo "build $L: ok"; exit 0; }
  [ "$r" = failed ] && { echo "build $L: FAILED"; e2e_ssh "tail -30 ~/$E2E_SCRATCH/build-$L.log"; exit 1; }
  sleep 15
done
