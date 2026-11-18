#!/bin/bash
# usage: e2e/integrate.sh [branch-suffix...]     default: engine node oracle contracts frontend e2e
# (Re)creates the LOCAL integration worktree ~/worktrees/perps-e2e-r2-int-20261118 on the local branch `e2e-int-r2`
# = main + origin's round-2 branch of each suffix (+ this worktree's HEAD for `e2e`). It is the tree the stack is
# built from before a merge into main, and the tree the whole test set runs on. Never pushed.
# UNAUDITED prototype. Copyright (c) 2026 Bloch Inc. All rights reserved.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$HOME/postern-perps
INT=$HOME/worktrees/perps-e2e-r2-int-20261118
[ $# -gt 0 ] || set -- engine node oracle contracts frontend e2e
git -C "$REPO" fetch -q origin
if [ -d "$INT" ]; then
  git -C "$INT" merge --abort 2>/dev/null || true
  git -C "$INT" checkout -q -B e2e-int-r2 main
else
  git -C "$REPO" worktree add -q -B e2e-int-r2 "$INT" main
fi
for s in "$@"; do
  if [ "$s" = e2e ]; then ref=$(git -C "$HERE/.." rev-parse HEAD); else ref=origin/codex/perps-$s-r2-20261118; fi
  git -C "$REPO" rev-parse -q --verify "$ref^{commit}" > /dev/null || { echo "[int] $s: no branch yet, skipped"; continue; }
  if git -C "$INT" merge-base --is-ancestor "$ref" HEAD; then echo "[int] $s: nothing new"; continue; fi
  if git -C "$INT" -c user.name="Postern Labs" -c user.email=noreply@posternlabs.com merge -q --no-ff --no-edit "$ref" > /dev/null 2>&1; then
    echo "[int] $s: merged $(git -C "$REPO" rev-parse --short "$ref")"
  else
    echo "[int] $s: CONFLICT merging $(git -C "$REPO" rev-parse --short "$ref"):"; git -C "$INT" diff --name-only --diff-filter=U; git -C "$INT" merge --abort; exit 1
  fi
done
echo "[int] tree $INT at $(git -C "$INT" rev-parse --short HEAD)"
