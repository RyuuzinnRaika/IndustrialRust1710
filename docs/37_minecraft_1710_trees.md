# Minecraft Java 1.7.10 natural trees — IndustrialRust1710

## Scope

The IRW4 Overworld now has **six** wood species and **twelve** procedural silhouettes.
It uses Minecraft Java 1.7.10 block IDs/data values, but **does not reproduce
Mojang's 1.7.10 biome GenLayer or population RNG byte-for-byte**. It is a
deterministic geometry/biome approximation. It does not change IRW1–3.

| Wood species | Shapes | Log ID:DV | Leaves ID:DV | Target biomes (legacy ID) |
|---|---|---|---|---|
| Oak | ordinary, branching/fancy, swamp | 17:0 | 18:0 | Plains 1 (sparse), Forest 4, Forest Hills 18, Swampland 6, Roofed Forest 29 (mixed) |
| Spruce | conical spruce, tall pine, 2×2 mega spruce/pine | 17:1 | 18:1 | Taiga 5, Cold Taiga 30, Mega Taiga 32 |
| Birch | upright, rounded canopy | 17:2 | 18:2 | Forest 4 (mixed), Birch Forest 27 |
| Jungle | upright small, 2×2 giant with branches and vines | 17:3 | 18:3 | Jungle 21 |
| Acacia | leaning, sideways logs, flattened split canopy | 162:0 | 161:0 | Savanna 35 |
| Dark oak | 2×2 trunk with wide canopy | 162:1 | 161:1 | Roofed Forest 29 |

## Shape and placement rules

- Oak and birch: approximately 4–8 trunk blocks with a layered irregular crown; rare larger branching oak occurs in roofed forest.
- Spruce and pine: vertical 1×1 trunks; conical spruce needles versus long bare pine trunks with a compact top. Mega variants have 2×2 trunks approximately 18–28 blocks tall.
- Jungle: ordinary 1×1 trees or a 2×2 giant approximately 18–29 blocks tall with branches and hanging vines.
- Acacia: 1×1 stem bends sideways near the top; horizontal wood uses 1.7.10 orientation metadata 0x4/0x8; canopy spreads along branches.
- Dark oak: 2×2 trunk approximately 6–9 blocks tall, dense overlapping crowns; four ground positions must have matching surface heights.
- Swamp oak: short oak with hanging vines. Plains have isolated starter oaks; savannas have isolated acacias.
- Surface-root minimum is sea level (Y=62); plains/savanna supplementary trees require Y>=64. Roots in the spawn-safe 24×24 area are suppressed.
- Generated leaf metadata **does not set 0x4**. In Java 1.7.10 that bit means permanent/player-placed leaves. Leaves use 18 for old species and 161 for acacia/dark oak; logs use 17 and 162 respectively.
- Natural trees are seeded by world seed and absolute coordinates. A 13×13 candidate search allows tree crowns to cross chunk boundaries without seams; point and cached-column generation use identical precedence.
- A species-specific conservative horizontal footprint (2–6 blocks) skips trees that cannot possibly contribute to a queried point or column. This preserves candidate ordering and all reachable tree voxels while avoiding unnecessary canopy geometry checks. A regression test inspects all twelve shapes across multiple variations; actual client-side performance still needs measurement.

## Sapling growth and bone meal (new)

The `ecology.rs` sapling growth path now reuses the natural-tree
`worldgen_trees.rs` silhouettes. This avoids growing every species as an
identical four-block oak with a square crown.

| Sapling data value (ID 6) | Single sapling | Complete 2×2 of matching saplings |
|---|---|---|
| 0 Oak | Oak or rare fancy/branching oak | Each grows as an individual oak |
| 1 Spruce | Conical spruce or pine | Mega spruce or mega pine, 2×2 trunk |
| 2 Birch | Upright birch | Each grows as an individual birch |
| 3 Jungle | Small jungle tree | Mega jungle, 2×2 trunk and branches |
| 4 Acacia | Bent acacia, sideways log metadata | Each grows as an individual acacia |
| 5 Dark oak | Does not grow | Dark oak, 2×2 trunk |

- A four-sapling variant is found from any selected sapling within the
  2×2 square. All four must have the matching species metadata; stage bits
  (`0x8`) do not change the species.
- Trunk, canopy, lateral branches and vines use the same procedural voxel
  geometry as the natural tree species, including log axis metadata and
  naturally decaying leaves.
- A growth attempt first validates **every proposed cell**. An existing
  structure or other sapling outside the roots blocks the entire attempt,
  leaving all saplings and adjacent player blocks untouched.
- Natural growth requires light level 9 or greater above the sapling.
  Each sapling is randomly selected at the default rate of three positions
  per 16³ section per tick, then has a **1/7** chance to advance its stage.
  This produces approximately **8 minutes per stage / 16 minutes to the
  first tree-generation attempt** when simulation runs at 20 TPS. The
  implementation uses a sampled one-second approximation rather than
  vanilla's exact per-section RNG. An overloaded ecology budget can slow it.
- Each sapling has two invisible stages (metadata bit `0x8`). The **first**
  successful growth attempt advances stage 0 to 1; the **next** attempts
  tree generation. Bone meal bypasses the light requirement but succeeds
  on just **45%** of uses, not on every use.
- A valid bone-meal use on a sapling consumes **one bone meal even when**
  the 45% roll fails or a stage-1 tree is obstructed. An inventory-only
  failure is journaled atomically with the unchanged world snapshot.
  Unsupported ground rejects the use without spending an item.
- Successful tree growth can replace natural grass, ferns, flowers,
  vines, snow layers and natural leaves, but not hard blocks, other
  saplings, logs or player-placed persistent leaves. Grass and farmland
  immediately beneath the new trunk roots become dirt in the same edit.
- The previous 128-cell special-item transaction limit remains intact for
  all non-tree interactions. **Only bone meal applied to a sapling**
  allows a bounded 2,048-cell atomic growth batch, for giant spruce/jungle
  trees; any excess or collision aborts the full transaction.
- World seed and current server tick deterministically choose each grown
  tree's shape. This approximates the vanilla species and placement behavior,
  but is not Mojang's original 1.7.10 PRNG.
- Leaf sapling drops use the pre-flattening species metadata:
  oak, spruce, birch, acacia and dark oak have a 1/20 chance, while
  **jungle leaves have only a 1/40 chance**. Oak and dark-oak apples
  continue to use the existing 1/200 approximation.
- Regression tests now cover species IDs and metadata, giant 2×2
  trunks, obstruction safety, replaceable foliage/soil changes, natural
  random-tick rate, 45% bone-meal attempts, two growth stages, drop
  probabilities and transaction sizing. These tests still require local
  execution with Rust; no Actions/Rust CI were used.

## Leaf decay speed (Java 1.7.10 defaults)

Natural leaves do not disappear as soon as the trunk is cut. The Overworld
block worker now tests a **deterministically sampled subset once per second**
with a probability of 60/4096 (= 20 TPS * 3 random block selections per
16³ section) per registered leaf per second. This is a close approximation to
vanilla's mean 68.27-second interval between random ticks per block; it is
**not a fixed 68-second removal timer**. Some leaves disappear quickly while
others may take several minutes.

- The authoritative tuning constant is
  `LEAF_RANDOM_TICK_SPEED: u64 = 3` in
  `apps/vanilla-server/src/world/ecology.rs`.
- Leaves (both legacy ID 18 and newer ID 161) **only disappear** when no
  connected trunk exists within four face-adjacent leaf steps. Supported
  leaves persist indefinitely.
- Bit `0x4` marks permanent/user-placed leaves, which never decay even after
  the trunk is removed. Log changes wake nearby leaves (4-block radius).
- The vanilla `0x8` "check decay" metadata is represented by the
  server's in-memory ecology wake queue, which is rebuilt for edited leaves
  and for generated leaves near persisted removed logs after loading.
  Vanilla chunk-level random position sampling is not implemented globally.
- Processing is bounded by the world tick budget, so sustained heavy load
  can increase actual delay relative to the average.
- Leaf decay also requires the server's 20 TPS block worker to run and
  does not continue while the server is stopped.
- All ordinary edits, two-block atomic edits and committed world-tick
  mutations now propagate leaf wakeups after a removed trunk or leaf.
  Chained disconnections wake adjacent leaves rather than performing a
  9×9×9 neighborhood scan for every decayed leaf.
- Added source-level regression cases for batched trunk removals,
  neighboring leaf propagation and failed-transaction rollback. These
  tests have not been executed yet.
- No GitHub Actions / Rust CI were run for this change. Unit tests were
  authored but not executed.

## Hanging vines and leaf-decay drops (additional implementation)

- Jungle and swamp vines (block 106) use Java 1.7.10's four direction
  flags: south `0x1`, west `0x2`, north `0x4`, east `0x8`.
- A vine face remains while backed by an opaque wall or inherited from
  an appropriately oriented vine above. A solid ceiling may also support
  hanging vines with metadata `0`.
- Breaking a backing wall clears the unsupported face bits. When no
  wall/ceiling/upper-vine attachment remains, the vine detaches.
  Unsupported vines cannot spread during their pending removal tick.
- Existing edited vines and sapling-grown vines enter the ecology and
  support queues. Nearby generated vines are awakened by tree log edits.
  Saved edited vines are requeued at world load.
- A random vine tick has an additional `1/4` chance of attempting to
  spread, using the same batched random sampling as leaves. Downward vines
  inherit supported faces; upward and horizontal attempts require a
  real attachment and stop spreading into dense local vine patches.
  This is a bounded **1.7.10-inspired approximation**, not the exact
  Mojang spread RNG or all of its branch-selection logic.
- Natural leaf decay performs **independent** sapling (oak/spruce/birch/
  acacia/dark oak 1/20, jungle 1/40) and apple (oak/dark oak 1/200)
  rolls. Consequently one naturally decaying oak/dark-oak leaf can yield
  both items. No modern stick drops are added for the 1.7.10 target.
  As before, pending drops publish only after the world save succeeds
  and share the worker's 256-drop per-tick bound.
- The legacy `additional_drop` helper remains for ordinary non-leaf
  block bonuses, while the live manual foliage mining path now uses the
  independent two-drop logic (or collects leaves/vines with shears).
- Regression tests cover vine face stripping, full detachment, hanging
  support, downward growth, restart restoration, leaves dropping saplings
  after a durable tick, and joint apple/sapling probabilities. Tests
  were authored but not executed in Rust; offline Python calculations
  confirmed the selected deterministic sampling cases.

## Live natural vines, hand-mining foliage and tall chunk coverage

- While a Protocol 5 chunk is encoded on an immutable world snapshot,
  the encoder collects block ID 106 positions in the same pass that builds
  block ID/metadata bytes. The live world reuses those coordinates to
  register naturally generated vines for support checks and random-tick
  spreading, **without re-running the tree generator under the world lock**.
- Each expanded Overworld chunk reports at most 1,024 vines. A bounded
  oldest-first registry holds at most 8,192 recent naturally generated
  vine positions. Eviction never unregisters player-edited plants.
  Freshly streamed chunks restore eligibility after a restart.
- The expanded Overworld now includes chunk section 9 (Y=144..159)
  in the normal client block/metadata/light payload. Giant jungle and
  spruce crowns can reach Y=158, so the previous nine-section mask
  could hide the upper part of otherwise real trees. Other dimensions
  retain their existing section masks.
- Manually destroyed leaves now use the same **independent** sapling and
  apple rolls as natural leaf decay. One ordinary oak/dark-oak leaf may
  drop both; a full ground-item capacity check covers all staged drops.
- Valid shears (item 359) instantly harvest old/new leaves (18/161)
  preserving their wood subtype, or collect vines (106) as an item.
  Shears lose one durability point on committed harvest, with the
  normal 238-use limit. Without shears, vines do not drop themselves.
  Leaf/vine harvest does not also roll ordinary saplings/apples when
  shears are used.
- Regression tests cover manually mined double-drops, shears metadata
  and wear, chunk-encoder vine collection, observation eviction and
  upper tree section delivery. **Rust compilation, cargo tests and
  real client validation remain pending.**

## Saturated ground items: bounded retry and backpressure

- Natural leaf decay already saves the world before generating a ground
  item. Previously the world worker used `drain_drops` and silently lost
  the remaining items if the 512-entity ground-item registry filled.
- The live server now **peeks** at the in-memory world-drop queue, stages
  up to **32 distinct-position item entities** in one validated ground
  snapshot, saves that snapshot once, and only then acknowledges the
  exact saved queue prefix. Spawn packets follow a successful save.
- When the ground-item pool is full, the persisted world queue remains
  unchanged and the worker checks for space at most once per second.
  If a two-file journal write fails, the worker **halts** so startup
  recovery can complete the transaction before any further world writes.
  A storage failure must never be followed by an unsafe in-process retry.
- When 1,536 or more pending drops remain, new random natural-leaf
  destruction is deferred. This prevents further leaf-decay drops from
  accumulating without bound during a prolonged full-pool condition.
  Leaf decay resumes after enough queued drops have been persisted.
- **Implemented cross-restart crash recovery:** before a block update
  is saved, its resulting item drops are appended to the bounded IRW
  world-drop backlog and persisted in that *same* atomic world write.
  The backwards-compatible **TNT5** trailer carries up to 2,048 drops
  (each stores coordinates, item ID, count and metadata). The existing
  TNT1–TNT4 trailers and IRW1–IRW4 generation identities remain readable
  **by this updated server**. Older server executables do **not** understand
  TNT5 and may refuse to open saves written by the new code; preserve a
  full pre-upgrade backup before deploying or attempting a downgrade.
  Malformed counts, truncated entries, invalid stacks and trailing
  data are rejected rather than silently losing items.
- Delivery to `world.drops.json` is a **two-file redo transaction**:
  stage 1–32 ground entities and the corresponding IRW backlog removal,
  write both snapshots through the existing `pending-txn.json` journal,
  then publish both in-memory states and network entity spawns only
  after commit succeeds. On crash during an applied journal, startup
  runs `journal::recover` before loading the world or accepting players.
  Replaying a transaction is idempotent, preventing both duplicate
  item entities and disappearing items in the two files.
- World-tick failures undo both proposed block edits and appended
  pending drops. A full queue rejects further unsafe commits instead
  of truncating persisted items. A prepared-journal error stops the
  live world worker until restart/recovery instead of continuing with
  an inconsistent in-memory snapshot.
- The durable queue uses a compact format for existing generated
  non-NBT drops. Items carrying arbitrary NBT are rejected for now;
  any future NBT-bearing world-drop producer must extend the format.
  The existing redo journal's per-file size limit still applies to
  oversized worlds. Back up existing saves before first rollout.
- All breakable drops are staged without the former silent 256-item
  truncation. A valid 300-block support-break tick persists all 300
  drops. If a tick would exceed the 2,048-drop durable queue limit,
  **the whole update is rejected and rolled back** rather than saving
  fewer items than the blocks destroyed.
- Added regression tests for 2 drops at distinct locations, 40-item
  partial batches (32 + 8), full 512-slot storage, invalid target
  refusal, random leaf backpressure, IRW1–IRW4 drop roundtrips,
  TNT5 corruption detection, 300-item complete saves, 2,049-item
  rollback, and crash replay with one item exactly.
  **These tests have not been executed with cargo or on a live client.**

## Compatibility and testing

Changes are currently on branch feat/vanilla-1710-tree-varieties-20261028.
No GitHub Actions or Rust CI were invoked. Source-level unit tests cover IDs,
trunk thickness, acacia bends, natural-leaf metadata and negative-coordinate
chunk-edge equivalence, **but they were not executed in this session**.

Suggested local commands in a Rust-enabled environment:

    cargo fmt --all -- --check
    cargo test -p vanilla-server --bin vanilla-server pending_world_drops_roundtrip_across_irw_versions
    cargo test -p vanilla-server --bin vanilla-server interrupted_world_drop_handoff_replays_exactly_one_entity
    cargo test -p vanilla-server --bin vanilla-server high_volume_support_breaks_do_not_silently_truncate_world_drops
    cargo test -p vanilla-server --bin vanilla-server over_capacity_support_breaks_roll_back_every_block_and_drop

The server is a **binary crate** (no library target); do not use the
`--lib` selector for these tests. Windows PowerShell can run the curated
local test suite without triggering any GitHub CI workflow:

    powershell.exe -ExecutionPolicy Bypass -File .\scripts\verify_tree_world_drops_local.ps1

Optional `-CheckFormat` checks formatting locally; optional `-AllTests`
runs the complete vanilla-server binary test suite. The default only
runs targeted tests and checks that every named test actually exists
before treating the run as successful. No tests were run when authoring
these changes.

Important: IRW4 base terrain is coordinate-generated. Changing its generator
can change **unmodified blocks in existing IRW4 worlds** when chunks are re-read.
Player edits remain in the sparse edit map, which is not a full migration.
Back up existing world data and preferably assess these trees on a **new test world**
before enabling in a long-lived survival world.

Current limitations requiring follow-up verification:

1. Natural tree shape and biome composition are approximated, not seed-identical to Mojang's 1.7.10 generator.
2. Sapling trees now share natural-tree silhouettes, but tree generation is not byte-for-byte Mojang's 1.7.10 random algorithm. A complete growth/bonemeal/growth-time compatibility audit is still outstanding.
3. Natural tree/structure clashes, canopy decay, vine growth details, manual leaf drop behavior and multiplayer client visuals need live Java 1.7.10 validation.
4. Existing IRW4 generation needs a format-version gate or migration strategy before safe general rollout.
