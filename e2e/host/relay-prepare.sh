#!/bin/bash
# Host side. usage: relay-prepare.sh <fresh|keep>
# The checkpoint relayer's gas-only THROWAWAY key: 32 random bytes as hex in run/relay-key (0600), generated at run
# time, never printed, never committed. keep: reuses the file. UNAUDITED prototype.
set -e
D=$HOME/l2dexbridge-20261030/perps-r2-e2e
mkdir -p $D/run
if [ "$1" = fresh ] || [ ! -f $D/run/relay-key ]; then
  rm -f $D/run/relay-key
  (umask 077; head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > $D/run/relay-key)
fi
chmod 600 $D/run/relay-key
