#!/usr/bin/env bash
# Cron watchdog: starts the forward test if it isn't running. Safe to run every minute.
# Low CPU and I/O priority so it never competes with other jobs on the server.
set -euo pipefail
DIR="$HOME/updown-scalper"
mkdir -p "$DIR/data" "$DIR/logs"
exec 9>"$DIR/watchdog.lock"
flock -n 9 || exit 0
# Match the exact process name: a -f pattern also matches shells whose command line mentions it.
if pgrep -u "$USER" -x scalper >/dev/null; then exit 0; fi
echo "$(date -u +%FT%TZ) starting scalper" >> "$DIR/logs/watchdog.log"
cd "$DIR"
nohup nice -n 10 ionice -c 3 "$DIR/scalper" run --data "$DIR/data" --raw-cap-gb 6 \
  >> "$DIR/logs/scalper.log" 2>&1 < /dev/null &
