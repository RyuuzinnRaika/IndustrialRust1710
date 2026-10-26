#!/usr/bin/env bash
# Start the real Minecraft 1.7.10 server (not the status-only server).
set -Eeuo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

if [[ ! -f whitelist.json || -L whitelist.json ]]; then
  echo "Missing/invalid whitelist.json. Run ./setup-ubuntu.sh and configure it first." >&2
  exit 1
fi

BIN="$ROOT/target/release/vanilla-server"
if [[ ! -x "$BIN" ]]; then
  echo 'Release binary missing; compiling vanilla-server...' >&2
  export PATH="$HOME/.cargo/bin:$PATH"
  if ! command -v cargo >/dev/null 2>&1; then
    echo 'Cargo not found. Run ./setup-ubuntu.sh first.' >&2
    exit 1
  fi
  cargo build --release -p vanilla-server --bins
fi

printf 'Starting IndustrialRust1710 (Minecraft Java 1.7.10). Press Ctrl+C to stop.\n'
# The binary defaults to 127.0.0.1:25565 when no server.properties exists.
# Arguments are forwarded, e.g. --bind 127.0.0.1:25566.
exec "$BIN" --whitelist "$ROOT/whitelist.json" "$@"
