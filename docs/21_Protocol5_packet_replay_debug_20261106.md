# Protocol 5 packet replay diagnostics (Minecraft Java 1.7.10)

This adds **real TCP, real native Rust server** regression tests to
`apps/vanilla-server/tests/protocol5_e2e.rs`. No mocks and no Java dependency.
The test sends only small, sanitized Protocol 5 packet inputs; it does not
load, print, or upload the original PCAP. It creates and removes a disposable
world, and does not access any existing user world or GitHub Actions.

## Run on Windows

Open PowerShell at the repository root (with a current local branch and the
Rust toolchain available):

```powershell
cargo test -p vanilla-server --test protocol5_e2e protocol5_replay_ -- --nocapture
```

This command selects the two new replay tests. To see the complete standard
verification summary, including those tests, run:

```powershell
.\verify-windows.ps1
```

The verification report writes its test output to
`target/verification/<commit>-<timestamp>-<pid>/tests.log`. If a replay test
fails, Rust's test harness includes its bounded `PROTOCOL5_*_REPLAY` trace
and the server stderr tail in this log. For a successful targeted test, use
`--nocapture` to print the same replay trace.

To save a standalone copy of a targeted replay's output, run in PowerShell:

```powershell
cargo test -p vanilla-server --test protocol5_e2e protocol5_replay_ -- --nocapture 2>&1 |
    Tee-Object -FilePath protocol5-replay-local.log
```

**Do not commit** `protocol5-replay-local.log` or a raw PCAP: diagnostic logs
can contain environment paths and other debugging details.

## Replay 1: Jump input / authoritative state

- Start a new survival server and join as a disposable test player.
- Replay 12 sanitized C04 packets at approximately 50 ms intervals with the
  Y samples in `docs/20_Protocol5_jump_zombie_trace_20261106.md`.
- OnGround is false during flight and true only on the final landing packet.
- Send a C01 `/list` chat barrier to ensure the server has processed all
  preceding positions on that TCP stream.
- Record S08 position corrections, S06 health updates, packet types and
  lengths, and the saved player position after a fresh login.

Expected: **no S08 correction**, **no HP loss**, and **saved feet Y=64.0**.

A failing assertion prints a short trace like:

```text
EXPECT: no S08 correction, no S06 damage, saved feet Y=64.0
C04 jump step=01 feet_y=64.419999987 onGround=false
...
C04 jump step=12 feet_y=64.000000000 onGround=true
C01 /list: wait for server barrier
S08 position correction: x=0.50000, feet_y=64.000000000, z=0.50000
RELOGIN saved feet=(0.50000, 64.000000000, 0.50000)
```

An S08 line represents an unexpected correction and triggers a test failure;
the example is illustrative, not a claimed result.

## Replay 2: Attack packet / combat response

- Spawn a synthetic zombie in the test world.
- Send a Protocol 5 C02 Use Entity packet with `action=1`, targeting
  the just-spawned entity ID.
- Read the actual server's S1A Entity Status (hurt=2), S1C Entity Metadata
  (health watcher), and S12 Entity Velocity packets, regardless of their
  arrival order. Respond to S00 KeepAlive while waiting.

Expected: **zombie hurt status=2**, **HP=19.0** after a bare-hand hit, and
**at least one knockback velocity packet**.

The log shows packet IDs, sizes, expected/observed health, and numeric
velocity components but no raw login data, addresses, credentials, or
full packet payloads.

## Scope, failure interpretation and limitations

These tests distinguish Protocol 5 wire input from real server output.
The PCAP-derived Y values are representative, *not* a raw replay of all
client traffic. Network timeouts, loaded world state and server tick
scheduling can still expose legitimate bugs or test flakiness; investigate
failures rather than weakening assertions.

A passing replay demonstrates those selected cases on the native server; it
does **not** establish full Minecraft Java 1.7.10 gameplay compatibility.
E42 manual client acceptance remains **NOT_RUN** until independently observed.
