#!/usr/bin/env bash
# Standalone, read-only GitHub fetch + local Rust verification.
# No GitHub Actions, API token, PR checkout, automatic approval or merge.
set -Eeuo pipefail
umask 077

usage() {
  echo "Usage: local-rust-ci.sh [--once|--if-changed]" >&2
  echo "  --once        Verify a fresh, trusted main checkout (default)" >&2
  echo "  --if-changed  Skip only when the same main SHA previously passed" >&2
}

mode="${1:---once}"
if [[ $# -gt 1 || "$mode" != "--once" && "$mode" != "--if-changed" ]]; then
  usage
  exit 64
fi

repo_url="https://github.com/RyuuzinnRaika/IndustrialRust1710.git"
# systemd services do not inherit interactive shell startup files.
export PATH="$HOME/.cargo/bin:$PATH"
ci_root="${IR_CI_ROOT:-$HOME/industrialrust1710-ci}"
if [[ "$ci_root" != /* || "$ci_root" == "/" || "$ci_root" == "$HOME" ]]; then
  echo "IR_CI_ROOT must be a dedicated absolute directory, not / or HOME." >&2
  exit 64
fi
for tool in flock git mktemp tee sha256sum date cargo rustc python3; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "Missing prerequisite: $tool" >&2
    exit 69
  }
done

mkdir -p -- "$ci_root"
ci_root="$(realpath -e -- "$ci_root")"
mkdir -p -- "$ci_root/reports"
exec 9>"$ci_root/.run.lock"
if ! flock -n 9; then
  echo "Another local Rust CI run is already active; skipping."
  exit 0
fi

# Do not fetch, check out, or execute PR branches on the persistent builder.
# Only the repository owner's main branch is eligible for this isolated job.
if [[ "$mode" == "--if-changed" ]]; then
  remote_sha="$(git ls-remote "$repo_url" refs/heads/main | cut -f1)"
  if [[ ! "$remote_sha" =~ ^[[:xdigit:]]{40}$ ]]; then
    echo "Could not determine the remote main commit." >&2
    exit 69
  fi
  if [[ -f "$ci_root/last-success-sha" ]]; then
    read -r previous_sha < "$ci_root/last-success-sha" || true
    if [[ "${previous_sha:-}" == "$remote_sha" ]]; then
      echo "main $remote_sha already passed local verification; skipping."
      exit 0
    fi
  fi
fi

work="$(mktemp -d "$ci_root/work.XXXXXXXX")"
cleanup() {
  rm -rf -- "$work"
}
trap cleanup EXIT

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
log="$ci_root/reports/$stamp-$$.log"
manifest="$ci_root/reports/$stamp-$$.txt"
start_time="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR="$ci_root/cargo-target"
export CARGO_TERM_COLOR=never

set +e
(
  set -Eeuo pipefail
  echo "IndustrialRust1710 standalone home Rust CI"
  echo "Started: $start_time"
  echo "Mode: $mode"
  echo "Remote: $repo_url (main only)"
  echo "CI directory: $ci_root"
  git clone --quiet --depth 1 --single-branch --branch main "$repo_url" "$work/repo"
  cd "$work/repo"
  sha="$(git rev-parse --verify HEAD)"
  [[ "$sha" =~ ^[[:xdigit:]]{40}$ ]]
  printf '%s\n' "$sha" > "$work/commit"
  echo "Commit: $sha"
  rustc --version
  cargo --version

  echo "=== 1. Rust formatting ==="
  cargo fmt --all -- --check
  echo "=== 2. Rust workspace regression tests ==="
  cargo test --workspace --all-targets
  echo "=== 3. Rust lint checks ==="
  cargo clippy --workspace --all-targets -- -D warnings
  echo "=== 4. Optimized binaries ==="
  cargo build --release -p vanilla-server -p legacy-status-server
  echo "=== 5. Independent Python verification tests ==="
  python3 -m unittest discover -s tools -p 'test_*.py' -v
  echo "=== 6. SHA256 checksums ==="
  sha256sum "$CARGO_TARGET_DIR/release/vanilla-server" \
            "$CARGO_TARGET_DIR/release/legacy-status-server"
) 2>&1 | tee -- "$log"
result=${PIPESTATUS[0]}
set -e

sha="unknown"
if [[ -f "$work/commit" ]]; then
  read -r sha < "$work/commit" || true
fi
status="FAIL"
if [[ "$result" == 0 ]]; then
  status="PASS"
  printf '%s\n' "$sha" > "$ci_root/last-success-sha.tmp"
  mv -f -- "$ci_root/last-success-sha.tmp" "$ci_root/last-success-sha"
fi

{
  printf 'project=IndustrialRust1710\n'
  printf 'mode=%s\n' "$mode"
  printf 'commit=%s\n' "$sha"
  printf 'status=%s\n' "$status"
  printf 'exit_code=%s\n' "$result"
  printf 'started_utc=%s\n' "$start_time"
  printf 'finished_utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf 'rustc=%s\n' "$(rustc --version)"
  printf 'cargo=%s\n' "$(cargo --version)"
  printf 'log=%s\n' "$log"
} > "$manifest"

cp -- "$manifest" "$ci_root/reports/latest.txt"
echo "Local CI: $status; commit=$sha; report=$manifest; log=$log"
exit "$result"
