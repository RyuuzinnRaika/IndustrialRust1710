# Ground item gravity and collision (Minecraft Java 1.7.10 / Protocol 5)

## Scope and design

Stacked on the survival mining/pickup preservation patch **PR #857**. This
additional patch addresses the separate item-physics problem **Issue #856**.

- Server-owned dropped items now advance in the existing 20TPS world tick
  worker instead of staying permanently at the fixed spawn coordinate.
- Each tick uses the Minecraft 1.7.10 EntityItem's vertical gravity
  (approximately 0.04 blocks/tick²) and vertical drag (0.98).
- Collision uses the native Rust block-shape resolver used by players and
  mobs, with the item entity's approximately 0.25×0.25×0.25 AABB.
  Vertical movement sweeps in bounded 1/16-block increments to avoid
  passing through thin blocks or the floor.
- Item fixed-point positions (/32 blocks), IDs, item stacks and
  source-specific pickup delay categories are preserved. Motion velocity
  and subfixed fractional positions are runtime-only: a restart resumes
  from the last persisted fixed-point position without duplicate items.
- Changed positions are committed through the recoverable single-file
  drop sidecar journal **before** S18 Entity Teleport messages are sent.
  The same 20TPS tick catches up no more than 20 steps if the clock
  briefly falls behind; iteration is bounded by the existing 512-item cap.
- New drop physics does not rewrite existing worlds or change the item
  entity's ID, count or NBT. It is intended for disposable test worlds
  until Windows/client acceptance finishes.

Protocol 5 S18 packets use an Int entity ID, three fixed-point Int
coordinates, then yaw and pitch bytes (18 payload bytes; no onGround
boolean in 1.7.10). The server broadcasts a final updated location for
each moved item after the tick's durable write.

## Test coverage

Unit tests added to `apps/vanilla-server/src/ground_items.rs`:

- `falling_item_lands_on_solid_ground_and_emits_protocol5_s18`: spawns
  one item six blocks above a flat grass surface, steps 80 ticks,
  validates continuous downward movement, no item creation/loss, S18
  packet fields and final stable height, and checks no continued
  teleports after settling.
- `falling_item_reacts_to_mined_floor_without_tunnelling`: mines the
  top grass before a drop falls, checks landing on the lower dirt
  layer and serialized drop-position round-trip.
- Previous PR #857 covers block C07 mining, dropped entity spawn,
  pickup, no duplicate collection, inventory save/relogin and 10/40
  tick source-specific eligibility.

Run on Windows after fetching the **stacked** branch:

```powershell
git fetch origin
git switch --detach origin/feat/ground-item-gravity-collision-20261106
cargo fmt --all -- --check
cargo test -p vanilla-server falling_item_ -- --nocapture
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
.\verify-windows.ps1
```

Do **not** use a real saved player world for initial tests. The test
harness makes disposable worlds, and the user should reproduce
the high-altitude drop with a separate new debug world.

## Remaining scope and risks

This is an initial vertical-gravity and solid-collision implementation.
It does **not** yet model 1.7.10 horizontal throw impulses, floating
and currents in liquids, bounce/slipperiness, merge timing and item
lifespan. Full parity still needs real 1.7.10 client gameplay and
packet-capture comparison. High-item-count/20TPS performance and
Windows crash/recovery tests are still required before production use.

**Verification state on PR creation:** static source review only.
Rustfmt, Rust unit/integration tests, Clippy, Windows end-to-end test
and unmodified Java 1.7.10 client acceptance are **NOT_RUN** in the
GitHub connector environment. Do not launch GitHub Actions Rust CI.
This change is staged as Draft and must not be merged into main
before the parent PR passes its own save-safety review.
