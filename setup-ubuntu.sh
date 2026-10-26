#!/usr/bin/env bash
# One-time installer/build for IndustrialRust1710 on Ubuntu.
set -Eeuo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

fail() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }
[[ -f Cargo.toml && -f config/whitelist.example.json ]] || fail "Run this script from the IndustrialRust1710 repository (missing Cargo.toml/config)."
command -v apt-get >/dev/null 2>&1 || fail "This installer requires Ubuntu/Debian apt-get."

if [[ "$(id -u)" -eq 0 ]]; then
  SUDO=()
elif command -v sudo >/dev/null 2>&1; then
  SUDO=(sudo)
else
  fail "sudo is required to install Ubuntu build dependencies."
fi

printf '\n[1/4] Installing Ubuntu build dependencies...\n'
"${SUDO[@]}" apt-get update
"${SUDO[@]}" apt-get install -y --no-install-recommends \
  ca-certificates curl git build-essential pkg-config libssl-dev

# rustup installs Cargo in ~/.cargo/bin; system packages may contain older Rust.
export PATH="$HOME/.cargo/bin:$PATH"
printf '\n[2/4] Installing/selecting stable Rust...\n'
if ! command -v rustup >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 --retry 3 -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal
  export PATH="$HOME/.cargo/bin:$PATH"
fi
command -v rustup >/dev/null 2>&1 || fail "rustup was not installed correctly."
rustup toolchain install stable --profile minimal
rustup default stable
command -v cargo >/dev/null 2>&1 || fail "Cargo is unavailable; check ~/.cargo/bin."

printf '\n[3/4] Preparing whitelist and compiling release binaries...\n'
if [[ -L whitelist.json ]]; then
  fail "Refusing a symlinked whitelist.json. Replace it with a regular file."
fi
if [[ ! -e whitelist.json ]]; then
  # Permissions: this file may contain private player names.
  install -m 600 config/whitelist.example.json whitelist.json
  printf 'Created whitelist.json. Replace RustTester with your Minecraft username before joining.\n'
else
  printf 'Keeping existing whitelist.json unchanged.\n'
fi
cargo build --release -p vanilla-server --bins

printf '\n[4/4] Checking that the server executable starts...\n'
BIN="$ROOT/target/release/vanilla-server"
[[ -x "$BIN" ]] || fail "Build completed but $BIN is missing."
"$BIN" --help >/dev/null
printf '\nSetup finished. Edit %s/whitelist.json, then start with:\n' "$ROOT"
printf '  ./run-ubuntu.sh\n'
printf 'Default listener: 127.0.0.1:25565 (local machine only).\n'
