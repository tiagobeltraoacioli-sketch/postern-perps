#!/bin/bash
# usage: sync.sh <label> [git-ref] [tree-dir]   copies the tracked tree at git-ref (default HEAD) of tree-dir (default: this
# worktree) to the host scratch as src-<label>, and the host scripts. No build. UNAUDITED prototype.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd); . "$HERE/config.sh"
L=$1; REF=${2:-HEAD}; TREE=${3:-$HERE/..}
e2e_ssh "mkdir -p ~/$E2E_SCRATCH/src-$L ~/$E2E_SCRATCH/run ~/$E2E_SCRATCH/bin && find ~/$E2E_SCRATCH/src-$L -mindepth 1 -maxdepth 1 -exec rm -rf {} +"
git -C "$TREE" archive "$REF" -- engine node compliance contracts/test/vectors docs | e2e_ssh "tar -x -C ~/$E2E_SCRATCH/src-$L"
tar -C "$HERE/host" -cf - . | e2e_ssh "tar -x -C ~/$E2E_SCRATCH/bin && chmod +x ~/$E2E_SCRATCH/bin/*.sh"
git -C "$TREE" rev-parse "$REF" | e2e_ssh "cat > ~/$E2E_SCRATCH/src-$L/.commit"
echo "synced $L at $(git -C "$TREE" rev-parse --short "$REF")"
