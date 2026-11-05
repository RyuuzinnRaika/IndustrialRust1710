#!/usr/bin/env bash
# Build, then run the verifier outside Cargo's run lock.
set -Eeuo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"
if [[ "$#" -eq 0 ]]; then
  set -- --automated-only
fi
cargo build --release -p industrial-verify
exec "$ROOT/target/release/industrial-verify" "$@"
