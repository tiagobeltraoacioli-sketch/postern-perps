#!/bin/bash
# Re-derives every signature of vectors/eip712.json with Foundry's cast (an independent EIP-712 and secp256k1
# implementation; RFC 6979 nonces, so signatures are byte-equal when the digest is equal) and recovers the signer.
# Usage: bash check-with-cast.sh [path-to-cast]. Prints one PASS/FAIL line per vector; exit 1 on any FAIL.
set -u
CAST=${1:-cast}
# macOS strips DYLD_* from the environment of /bin/bash; pass the Foundry libusb path as CAST_DYLD_LIBRARY_PATH.
[ -n "${CAST_DYLD_LIBRARY_PATH:-}" ] && export DYLD_LIBRARY_PATH="$CAST_DYLD_LIBRARY_PATH"
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
n=$(python3 -I -c 'import json,sys; print(len(json.load(open(sys.argv[1]))["observations"]))' "$here/eip712.json")
fail=0
for i in $(seq 0 $((n - 1))); do
  python3 -I -c 'import json,sys; v=json.load(open(sys.argv[1]))["observations"][int(sys.argv[2])]; json.dump(v["typedData"], open(sys.argv[3],"w")); print(v["name"], v["throwawayScalar"], v["signature"], v["signer"])' "$here/eip712.json" "$i" "$tmp/td.json" > "$tmp/f"
  read -r name sk want signer < "$tmp/f"
  got=$("$CAST" wallet sign --private-key "$sk" --data --from-file "$tmp/td.json" 2>&1)
  rec=$("$CAST" wallet verify --address "$signer" --data --from-file "$tmp/td.json" "$got" 2>&1 | head -1)
  if [ "$got" = "$want" ]; then echo "PASS $name signature $got ($rec)"; else echo "FAIL $name cast=$got ours=$want"; fail=1; fi
done
"$CAST" --version | head -1
exit $fail
