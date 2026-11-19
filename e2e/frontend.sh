# Sourced by up.sh (--frontend). Starts 434's dev server against the stack (frontend/scripts/dev-local.mjs).
# frontend/node_modules must exist in the tree (434's report: a symlink to the shared install). UNAUDITED prototype.
[ -f "$TREE/frontend/scripts/dev-local.mjs" ] || { echo "no frontend/scripts/dev-local.mjs in $TREE (434)" >&2; exit 1; }
[ -e "$TREE/frontend/node_modules" ] || { echo "frontend/node_modules missing in $TREE" >&2; exit 1; }
( cd "$TREE/frontend" && PATH="$(dirname "$NODE_BIN"):$PATH" mac_start frontend "$NODE_BIN" scripts/dev-local.mjs --api "http://127.0.0.1:$M_API" --l2 "http://127.0.0.1:$M_ANVIL" --vault "$VAULT" --token "$TOKEN" --chain "$E2E_CHAIN_ID" --port "$M_FRONTEND" )
wait_http "http://127.0.0.1:$M_FRONTEND/" 60 "frontend dev server"
say "frontend dev server on http://127.0.0.1:$M_FRONTEND"
