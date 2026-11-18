#!/bin/bash
# usage: e2e/test-mac.sh [tree-dir] [suite...]     the Mac half of the whole test set of a tree (default: the
# integration worktree; suites default: e2e mock compliance geoblock forge frontend). One summary line per suite in
# e2e/.run/test-mac-<tree-commit>.txt, logs next to it. nice 10; the Mac is loaded: a suite that
# times out is rerun alone before it is called a failure. UNAUDITED prototype.
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd); . "$HERE/config.sh"
TREE=${1:-$HOME/worktrees/perps-e2e-r2-int-20261118}; shift || true
[ $# -gt 0 ] || set -- e2e mock compliance geoblock forge frontend
C=$(git -C "$TREE" rev-parse --short HEAD)
OUT=$HERE/.run/test-mac-$C; mkdir -p "$OUT"; R=$OUT.txt; : > "$R"
export PATH="$(dirname "$NODE_BIN"):$FOUNDRY_BIN:$PATH"
nt() { # name dir globs...: node --test
  local n=$1 d=$2; shift 2
  [ -d "$TREE/$d" ] || { echo "$n: ABSENT" >> "$R"; return; }
  (cd "$TREE/$d" && nice -n 10 node --test $@) > "$OUT/$n.log" 2>&1; local rc=$?
  echo "$n: tests $(grep -E '^ℹ tests' "$OUT/$n.log" | awk '{print $3}') pass $(grep -E '^ℹ pass' "$OUT/$n.log" | awk '{print $3}') fail $(grep -E '^ℹ fail' "$OUT/$n.log" | awk '{print $3}') skipped $(grep -E '^ℹ skipped' "$OUT/$n.log" | awk '{print $3}') rc $rc" >> "$R"
}
for s in "$@"; do
  case $s in
    e2e) nt e2e e2e 'test/*.test.mjs' ;;
    mock) nt mock node/mock 'test/*.test.mjs' ;;
    geoblock) nt geoblock-js compliance/geoblock ;;
    compliance)
      (cd "$TREE" && nice -n 10 python3 -I -m unittest discover -s compliance/tools -p 'test_*.py') > "$OUT/compliance.log" 2>&1; rc=$?
      echo "compliance: $(grep -E '^Ran ' "$OUT/compliance.log") $(tail -1 "$OUT/compliance.log") rc $rc" >> "$R" ;;
    forge)
      (cd "$TREE/contracts" && nice -n 10 env DYLD_LIBRARY_PATH="$TOOLS/libusb/lib" forge test --offline) > "$OUT/forge.log" 2>&1; rc=$?
      echo "forge: $(grep -E '^Ran [0-9]+ test suites' "$OUT/forge.log" | tail -1) rc $rc" >> "$R" ;;
    frontend)
      if [ ! -e "$TREE/frontend/node_modules" ]; then echo "frontend: node_modules missing (see 434's report)" >> "$R"; continue; fi
      (cd "$TREE/frontend" && nice -n 10 npm run check) > "$OUT/frontend.log" 2>&1; rc=$?
      echo "frontend: npm run check rc $rc; vitest: $(grep -E '^ +Tests +' "$OUT/frontend.log" | sed -E 's/ +/ /g' | paste -sd';' -)" >> "$R" ;;
    *) echo "$s: unknown suite" >> "$R" ;;
  esac
done
echo "tree $TREE at $C"; cat "$R"
