# Group C: Mob / AI / combat (21–30)

Target: Java Edition 1.7.10, Protocol 5, server-authoritative, Rust-only.
This file distinguishes implemented behavior from incomplete compatibility. CI passing does **not** imply exact Minecraft behavior.

## Current implementation (PR #62)

| ID | Current result | Missing before acceptance |
|---|---|---|
| 21 P0 | Natural spawn checks sky/block light derived from the same chunk arrays clients receive, a valid supported position, player exclusion (24 blocks), bounded hostile/passive caps, IRW3 terrain height/biome, caves, 128-block despawn. No arbitrary starter entities. | Difficulty other than normal, complete 1.7.10 biome/entity weighted tables, 32–128 random despawn, spawn groups, spawn budgets indexed by loaded chunk. Night sky subtraction is a simplified phase. |
| 22 P1 | Zombie tracking, close-range attack, daylight HP loss, byte-typed baby metadata and faster baby movement. | Actual fire status/duration and protection from helmets, door breaking, villagers/infection, all reinforced zombie variants. |
| 23 P1 | Skeleton sight check before shooting, midrange positioning/retreat, wall-aware arrows and swept collision, sunlight HP loss, authoritative death/XP. | Detailed bow draw/ballistics, equipment enchantments, armor and pickup behavior. |
| 24 P1 | Multi-player blast damage by entity ID, attenuation by distance and block exposure, Mob blast deaths, block shielding, bounded block damage and persisted common drops. | Exact vanilla resistance raymarch/knockback, explosion sound/particles/drop probability, multi-object transaction. |
| 25 P1 | Spider wall climbing, daylight neutrality, cave-spider Protocol 5 spawn, server HP poison pulses, collision/attack. | Exact climbing AABB, webs, vanilla potion-effect packet and difficulty-specific poison timer. |
| 26 P1 | Cow/pig/sheep/chicken breeding and growth, 5-minute breeding cooldown, chicken eggs every 6k–12k ticks. | Sheep wool regrowth and shearing, milking cows with buckets, variable loot quantities and cooking/fire modifiers. |
| 27 P1 | Not implemented. | Enderman teleportation, block carrying, slime size/splitting, witch potions. |
| 28 P1 | Not implemented. | Nether dimension persistence/transport, ghast/blaze/pigman/magma-cube AI. |
| 29 P1 | Not implemented. | Village discovery, villager trades/professions, iron golem spawning. |
| 30 P2 | Not implemented. | End/Wither boss phases, damage patterns, drops, completion/reset persistence. |

## Server guarantees covered

- The game state owns Entity IDs; spawn, movement, HP/damage, death and XP broadcasts are generated on the server.
- Arrow target identity is keyed by player Entity ID rather than coordinate, avoiding duplicate damage when two players overlap.
- Common explosion calculations use world blocks; a client cannot declare what was hit or damaged.
- Entity and projectile loops have bounded budgets, and the shared Mob collection is protected by one mutex.

## Verification

```sh
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p vanilla-server
```

Required live checks: two vanilla 1.7.10 clients at the same coordinates under arrow fire; torch and nighttime spawn near chunk boundaries; sheltered player inside a creeper blast; cave-spider poison expiry; zombie baby metadata; cave mob spawn and clean despawn; offline save/restart.

**Important:** Nether, village trading and boss phases require dimension and entity-protocol work. They must not be described as complete until implemented and tested.
