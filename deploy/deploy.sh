#!/usr/bin/env bash
# Builds a static Linux binary on this machine and installs it on the VPS.
# Usage: deploy/deploy.sh user@host   (needs cargo-zigbuild and zig on PATH)
set -euo pipefail
HOST="${1:?usage: deploy/deploy.sh user@host}"
cd "$(dirname "$0")/.."
cargo zigbuild --release --target x86_64-unknown-linux-musl
BIN=target/x86_64-unknown-linux-musl/release/scalper
ssh "$HOST" 'mkdir -p ~/updown-scalper'
scp "$BIN" "$HOST:~/updown-scalper/scalper.new"
scp deploy/ensure_running.sh "$HOST:~/updown-scalper/ensure_running.sh"
# Swap the binary, then restart so the watchdog launches the new one.
ssh "$HOST" 'cd ~/updown-scalper && chmod +x scalper.new ensure_running.sh && mv scalper.new scalper \
  && (pkill -u "$USER" -x scalper || true) && sleep 1 && ./ensure_running.sh \
  && sleep 2 && pgrep -a -x scalper'
LOCAL=$(shasum -a 256 "$BIN" | cut -d" " -f1)
REMOTE=$(ssh "$HOST" 'sha256sum ~/updown-scalper/scalper | cut -d" " -f1')
[ "$LOCAL" = "$REMOTE" ] && echo "deployed, checksums match: $LOCAL" || { echo "CHECKSUM MISMATCH"; exit 1; }
