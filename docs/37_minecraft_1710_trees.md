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

## Compatibility and testing

Changes are currently on branch feat/vanilla-1710-tree-varieties-20261028.
No GitHub Actions or Rust CI were invoked. Source-level unit tests cover IDs,
trunk thickness, acacia bends, natural-leaf metadata and negative-coordinate
chunk-edge equivalence, **but they were not executed in this session**.

Suggested local commands in a Rust-enabled environment:

    cargo fmt --all -- --check
    cargo test -p vanilla-server --lib worldgen_trees
    cargo test -p vanilla-server --lib worldgen
    cargo test -p vanilla-server --lib ecology

Important: IRW4 base terrain is coordinate-generated. Changing its generator
can change **unmodified blocks in existing IRW4 worlds** when chunks are re-read.
Player edits remain in the sparse edit map, which is not a full migration.
Back up existing world data and preferably assess these trees on a **new test world**
before enabling in a long-lived survival world.

Current limitations requiring follow-up verification:

1. Natural tree shape and biome composition are approximated, not seed-identical to Mojang's 1.7.10 generator.
2. Saplings and bone meal still use the older ecology growth system; especially 2×2 giant spruce/jungle growth is not yet shared with these natural-tree shapes.
3. Natural tree/structure clashes, drop rates, canopy decay, vine attachments and multiplayer client visuals need live Java 1.7.10 validation.
4. Existing IRW4 generation needs a format-version gate or migration strategy before safe general rollout.
