#!/usr/bin/env bash
# Local-only verification wrapper. Explicit bash invocation needs no executable bit.
set -Eeuo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
if [[ "$#" -eq 0 ]]; then
  set -- --automated-only
fi
exec cargo run --release -p industrial-verify -- "$@"
