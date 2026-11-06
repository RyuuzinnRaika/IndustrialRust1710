# Survival block mining and item pickup integrity (2026-11-06)

## Problem and scope

Reviewing the native 1.7.10 server after a report of unexpected block
breaking and item collection revealed a concrete consistency defect:
the ordinary survival mining path persisted tool wear, then saved the
world, then saved the ground entities as separate operations. If the
last save failed, a block could disappear without its drop; a failure
or interrupted run between earlier writes could leave disagreeing
durability/world/drop state. Player Q/Ctrl+Q drops also saved the inventory
before persisting the dropped item.

This change **does not assert those failure conditions occurred during
the user's earlier gameplay sessions**. It fixes the observable code-level
consistency risk; packet evidence for a specific mining/collection symptom
has not yet been provided.

## Behavior changed

- Ordinary survival C07 Start/Finish (including instant-break and paired
  bed/door parts) stages the world removal, all generated drops, and any
  worn tool state. It commits the snapshots through the same recoverable
  `journal::commit` mechanism already used for container mining and
  inventory pickup. No in-memory state or spawn packet is published until
  the commit completes.
- C07 player Q/Ctrl+Q action stages the hotbar debit together with the
  ground entity in **one journal transaction**. A failed ground write
  cannot silently debit the hotbar.
- Successful ground-item pickups already atomically journal the player
  inventory and remaining ground entity. When `IR_TRACE_MINING=1`,
  a successful pickup now emits a bounded summary containing item ID,
  entity ID, amount credited, remaining amount and number of affected
  slots. Committed ordinary mining similarly emits a drop count.
- Protocol 5 regression test
  `survival_mined_block_drop_pickup_persists_without_duplication` exercises
  an actual native TCP server, starts/finishes mining, observes
  Spawn Object / Block Change / Collect Item / Destroy Entity / Set Slot,
  verifies a single persistent inventory item and an empty ground pool
  after logout, and logs in again. It uses only a disposable flat world.

## How to validate on Windows

```powershell
cargo fmt --all -- --check
cargo test -p vanilla-server --test protocol5_e2e survival_mined_block_drop_pickup_persists_without_duplication -- --nocapture
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
.\verify-windows.ps1
```

For gameplay reproduction in a **new disposable** IRW4 world, rather
than on your existing saved worlds, set `IR_TRACE_MINING=1` and start
the server with an unused world path. On Windows PowerShell 5.1,
native stderr should be captured with `cmd.exe /d /c '... 2>&1'` when
piping to `Tee-Object`. Do not post unredacted logs publicly.

## Remaining gameplay limitations (not fixed here)

`GroundItems` currently **does not simulate item gravity, block
collision, fluids or merging**, and pickup eligibility uses a fixed
750 ms delay rather than distinct vanilla 1.7.10 source-specific
delays. Thus blocks destroyed overhead can leave floating drops
instead of items settling naturally. Those require separate item
entity physics, packet-position updates, persistence and acceptance
tests. This patch must not be described as complete 1.7.10 drop
physics fidelity. Existing saved worlds must not be migrated or
rewritten for testing.

This code path is important for item conservation; **hold the PR
until a Windows cargo fmt, workspace test and gameplay check**, or
report the checks explicitly as NOT_RUN. GitHub Actions Rust CI stays off.
