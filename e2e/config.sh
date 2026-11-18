# e2e stack configuration (sourced by every script). UNAUDITED prototype: local processes, throwaway keys only.
# Copyright (c) 2026 Bloch Inc. All rights reserved.
# The build host and the ssh identity file are operator facts, never committed: e2e/local.env (git-ignored; see
# local.env.example) or the environment sets E2E_HOST (user@address) and E2E_SSH_KEY (path of the identity file).
_cfg_dir=$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)
[ -f "$_cfg_dir/local.env" ] && . "$_cfg_dir/local.env"
: "${E2E_HOST:?set E2E_HOST in e2e/local.env (see local.env.example)}"
: "${E2E_SSH_KEY:?set E2E_SSH_KEY in e2e/local.env (see local.env.example)}"
E2E_SCRATCH='l2dexbridge-20261030/perps-r2-e2e'   # relative to the host's $HOME
# host ports (loopback only): 53500-53599
H_API=53500
H_ANVIL=53510
H_PROXY=53511
H_FEEDER=53530
# Mac ports (loopback only): 49900-49999
M_API=49900
M_ANVIL=49901
M_PROXY=49902
M_FRONTEND=49920
M_MOCK=49930
E2E_SEED=${E2E_SEED:-e2e-r2}
E2E_CHAIN_ID=700770
TOOLS=$HOME/.local/share/dex-tools
NODE_BIN=${NODE_BIN:-$TOOLS/node/node-v24.21.0-darwin-x64/bin/node}
FOUNDRY_BIN=${FOUNDRY_BIN:-$TOOLS/foundry}
e2e_ssh() { ssh -o BatchMode=yes -o ConnectTimeout=20 -i "$E2E_SSH_KEY" "$E2E_HOST" "$@"; }
