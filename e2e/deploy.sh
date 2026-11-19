# Sourced by up.sh (anvil mode). Deploys mock bUSDC + the unchanged PerpsVault on the local anvil with the node's dev
# committee (5 of 7, throwaway keys of the seed) through 433's contracts/deploy/local.sh, from the Mac through the
# tunnel. Sets VAULT and TOKEN; writes .run/deployment.json and .run/committee.json. UNAUDITED prototype.
"$NODE_BIN" --input-type=module -e "
import { nodeDevCommittee } from '$HERE/lib/keys.mjs';
const v = nodeDevCommittee('$E2E_SEED', 'validator', 7).map((k) => k.address);
process.stdout.write(JSON.stringify({ validators: v, threshold: 5 }) + '\n');" > "$RUN/committee.json"
[ -x "$TREE/contracts/deploy/local.sh" ] || { echo "no contracts/deploy/local.sh in $TREE (433's local deployment)" >&2; exit 1; }
rm -f "$RUN/deployment.json"
( cd "$TREE/contracts" && FOUNDRY_DIR="$FOUNDRY_BIN" nice -n 10 bash deploy/local.sh "http://127.0.0.1:$M_ANVIL" "$RUN/committee.json" "$RUN/deployment.json" ) > "$RUN/deploy.log" 2>&1 \
  || { echo "vault deployment failed:" >&2; tail -15 "$RUN/deploy.log" >&2; exit 1; }
VAULT=$("$NODE_BIN" -e 'console.log(JSON.parse(require("fs").readFileSync(process.argv[1])).vault.toLowerCase())' "$RUN/deployment.json")
TOKEN=$("$NODE_BIN" -e 'console.log(JSON.parse(require("fs").readFileSync(process.argv[1])).token.toLowerCase())' "$RUN/deployment.json")
say "vault $VAULT and mock bUSDC $TOKEN deployed on the local anvil (chain $E2E_CHAIN_ID)"
