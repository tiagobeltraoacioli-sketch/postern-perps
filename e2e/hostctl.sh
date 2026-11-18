#!/bin/bash
# usage: e2e/hostctl.sh <script-in-host-bin> [args...]    e.g. hostctl.sh svc.sh status
# Runs one of the host-side scripts of the e2e stack over ssh (used by the rehearsal to stop and restart processes).
# UNAUDITED prototype. Copyright (c) 2026 Bloch Inc. All rights reserved.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd); . "$HERE/config.sh"
s=$1; shift
case "$s" in *[!a-z.-]*|"") echo "bad script name" >&2; exit 2 ;; esac
q=""; for a in "$@"; do q="$q $(printf '%q' "$a")"; done
e2e_ssh "~/$E2E_SCRATCH/bin/$s$q"
