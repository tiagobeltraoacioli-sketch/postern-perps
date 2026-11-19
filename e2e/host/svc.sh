#!/bin/bash
# Host side process control for the e2e stack. PID files only: nothing is ever killed by name or pattern, and a PID
# is signalled only if its environment still carries the marker this script gave it (a reused PID is never touched).
#   svc.sh start <name> <command...>     log run/<name>.log, pid run/<name>.pid; refuses if <name> is alive
#   svc.sh stop <name> [SIGNAL]          default TERM; waits 15 s, then KILL
#   svc.sh alive <name>                  exit 0 if running
#   svc.sh status                        one line per known service
#   svc.sh stop-all                      stops every service with a pid file (the node by INT: its clean shutdown; the others by TERM)
# UNAUDITED prototype. Copyright (c) 2026 Bloch Inc. All rights reserved.
D=$HOME/l2dexbridge-20261030/perps-r2-e2e
R=$D/run
mkdir -p $R
mine() { # pid name -> 0 if the process exists and carries our marker
  [ -n "$1" ] && [ -r /proc/$1/environ ] && tr '\0' '\n' < /proc/$1/environ | grep -qx "E2E_SVC=$2"
}
alive() { local p; p=$(cat $R/$1.pid 2>/dev/null); mine "$p" "$1"; }
stop() {
  local n=$1 sig=${2:-TERM} p
  p=$(cat $R/$n.pid 2>/dev/null)
  if ! mine "$p" "$n"; then rm -f $R/$n.pid; echo "$n: not running"; return 0; fi
  kill -$sig $p
  for _ in $(seq 1 150); do mine "$p" "$n" || break; sleep 0.1; done
  if mine "$p" "$n"; then kill -KILL $p; sleep 0.3; echo "$n: killed (KILL) pid $p"; else echo "$n: stopped ($sig) pid $p"; fi
  rm -f $R/$n.pid
}
case $1 in
  start)
    n=$2; shift 2
    if alive $n; then echo "$n: already running pid $(cat $R/$n.pid)" >&2; exit 1; fi
    E2E_SVC=$n setsid nohup "$@" >> $R/$n.log 2>&1 < /dev/null &
    echo $! > $R/$n.pid
    sleep 0.5
    if alive $n; then echo "$n: started pid $(cat $R/$n.pid)"; else echo "$n: FAILED to start; tail of log:" >&2; tail -5 $R/$n.log >&2; exit 1; fi ;;
  stop) stop $2 $3 ;;
  alive) alive $2 ;;
  status) for f in $R/*.pid; do [ -e "$f" ] || continue; n=$(basename $f .pid); if alive $n; then echo "$n: running pid $(cat $f)"; else echo "$n: dead (stale pid file)"; fi; done ;;
  stop-all) for f in $R/*.pid; do [ -e "$f" ] || continue; n=$(basename $f .pid); case $n in build-*|test-*) continue ;; node) stop $n INT ;; *) stop $n TERM ;; esac; done ;;
  *) echo "usage: svc.sh start|stop|alive|status|stop-all" >&2; exit 2 ;;
esac
