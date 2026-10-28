# Local Windows PowerShell 5.1+ / PowerShell 7 validation for tree/drop changes.
# This script does NOT call GitHub, GitHub Actions or any CI service.
# Usage: powershell -ExecutionPolicy Bypass -File .\scripts\verify_tree_world_drops_local.ps1
# Optional: -CheckFormat, -AllTests
[CmdletBinding()]
param(
    [switch]$CheckFormat,
    [switch]$AllTests
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw 'Rust Cargo is not installed or is not on PATH. Install a local Rust toolchain first.'
}

Push-Location $root
try {
    if ($CheckFormat) {
        Write-Host 'Checking Rust formatting locally (no CI)...'
        & cargo fmt --all -- --check
        if ($LASTEXITCODE -ne 0) {
            throw "Local cargo fmt check failed (exit $LASTEXITCODE)"
        }
    }

    $filters = @(
        'pending_world_drops_roundtrip_across_irw_versions',
        'corrupt_tnt5_drop_backlogs_fail_closed',
        'worlds_without_pending_drops_retain_the_old_trailer_format',
        'high_volume_support_breaks_do_not_silently_truncate_world_drops',
        'over_capacity_support_breaks_roll_back_every_block_and_drop',
        'interrupted_world_drop_handoff_replays_exactly_one_entity',
        'queued_leaf_drops_at_distinct_positions_save_in_one_batch',
        'large_drop_backlog_commits_only_thirty_two_items_per_retry',
        'full_ground_pool_keeps_world_drops_until_space_is_available',
        'failed_ground_save_does_not_consume_pending_world_drops',
        'naturally_decaying_leaves_persist_drops_after_the_commit',
        'saturated_drop_backlog_defers_leaf_decay_without_destroying_the_leaf'
    )
    if ($AllTests) {
        Write-Host 'Running all local vanilla-server binary tests (no CI)...'
        & cargo test -p vanilla-server --bin vanilla-server
        if ($LASTEXITCODE -ne 0) {
            throw "Local cargo test failed (exit $LASTEXITCODE)"
        }
    }
    else {
        # Cargo test returns exit 0 even if a filter selects zero tests.
        # Query the compiled binary's inventory first to detect renamed,
        # skipped or missing regression cases instead of reporting success.
        $available = & cargo test -p vanilla-server --bin vanilla-server -- --list
        if ($LASTEXITCODE -ne 0) {
            throw "Could not list the local vanilla-server tests (exit $LASTEXITCODE)"
        }
        foreach ($filter in $filters) {
            $found = $available | Where-Object { $_.Contains($filter) }
            if (-not $found) {
                throw "Required regression test not found: $filter"
            }
            Write-Host "Running local test: $filter"
            & cargo test -p vanilla-server --bin vanilla-server $filter -- --nocapture
            if ($LASTEXITCODE -ne 0) {
                throw "Local test '$filter' failed (exit $LASTEXITCODE)"
            }
        }
    }
    Write-Host 'Local tree/drop tests passed. GitHub Actions were not invoked.'
}
finally {
    Pop-Location
}
