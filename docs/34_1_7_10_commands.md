# Minecraft Java Edition 1.7.10 command support

Commands here target **Java 1.7.10 (Protocol 5)**, not the latest Java/Bedrock syntax.
Modern commands such as `/fill`, `/execute`, `/locate`, `/title` and `/worldborder` were not in 1.7.10.

References:
- Official Minecraft command overview: https://www.minecraft.net/en-us/article/minecraft-commands
- Minecraft Wiki command history (version checks): https://minecraft.wiki/w/Commands

## In-game commands

All players: `/help`, `/?`, `/list`, `/me <action>`,
`/tell <player> <message>` (aliases `/msg`, `/w`).
Chat permissions and existing per-session spam limits also apply to `/me` and private messages.
Private messages are delivered to the target only (and echoed to the sender).

**Verified OP only (online-mode):**
- Level 2+: `/time set <ticks|day|night>`, `/time add <ticks>`, `/time query daytime`,
  `/weather <clear|rain|thunder>`, legacy `/weather set <...>`,
  `/tp <x> <y> <z>`, `/tp <player> <x> <y> <z>` (also `/teleport`),
  `/gamemode <survival|creative|0|1> [player]`,
  `/give <player> <item|numeric-id> [amount=1] [data=0]`
- Level 3+: existing `/kick`, `/ban`, `/pardon`, `/ban-ip` and related actions
- Level 4: existing `/op`, `/deop`, `/save`, `/permission`

Numeric and `~`-relative teleport coordinates are supported. Coordinates must stay
within the server's validated world bounds. Hardcore mode rejects gamemode changes.
Give supports up to 64 items per call (including splitting unstackable items), checks
legacy item/data validity, and rejects full inventories rather than dropping or duplicating items.
A subset of common `minecraft:` item names is supported; all supported vanilla numeric
IDs remain available. Full registry coverage, arbitrary NBT and selector arguments are **not implemented**.

Offline-mode usernames are unverified and **cannot run OP commands**, even if
`/op` is issued from the host console. This is intentional security behavior.

## Host console

Console commands are prefixed by `/` and include `/help`, `/list`, `/say`, `/me`,
`/tell`, `/msg`, `/w`, `/give`, `/tp`, `/teleport`,
`/gamemode`, `/difficulty`, `/gamerule`, `/time`, `/weather`,
`/scoreboard`, `/save`, `/op`, `/deop`, `/kick`, `/ban`,
`/pardon`, `/whitelist`, `/permission`, and `/stop`, with existing
server-specific subsets for the larger admin commands.

All console commands remain audit logged; state-changing commands refuse execution
if the audit log is unavailable. Commands affecting a live player's inventory,
gamemode, or position are queued to that player's session thread.

## Verification

`cargo fmt --all -- --check`
`cargo test -p vanilla-server`
`cargo clippy --workspace --all-targets -- -D warnings`

Automated unit tests cover command parsing, permission bypass separation,
legacy time aliases, relative coordinate validation and give argument bounds.
For acceptance, test with **two vanilla 1.7.10 clients** and verify that
an offline-mode player gets permission denied for `/give`, `/tp`,
`/gamemode` regardless of their displayed username.

## Phase 2: /clear and /setblock

- `/clear [player] [item] [data=-1] [maxCount=-1]`: on a verified OP
  or as `/clear <player> ...` on the host console. No item means all normal
  player inventory items; Ender Chest and external storage remain untouched.
  `maxCount=0` is read-only counting. Writes are owned, saved and resynced
  by the target player's session.
- `/setblock <x> <y> <z> <block> [data=0] [replace|keep]`: accepts
  absolute coordinates from console; in-game OPs may use `~` offsets.
  This phase intentionally supports a curated whitelist of ordinary terrain
  blocks and numeric IDs only. It rejects overwriting existing TileEntities,
  blocks requiring live tile data, and `destroy` mode (drops are not yet
  transaction-safe). The world is saved before a Protocol 5 block-change packet
  reaches players in the same dimension.
- No target selectors such as `@a` or `@p` yet; name-only targets are used.

These limits prevent accidental inventory loss and incorrect 1.7.10 block NBT.

## Phase 3: /effect and /summon (Java 1.7.10)

- `/effect <player> <id|name> [seconds=30] [amplifier=0]`, and
  `/effect <player> clear`. Seconds `0` removes just that effect.
  Currently accepted IDs are `1` Speed, `10` Regeneration, `17`
  Hunger, `19` Poison: these use the existing per-session status-effect
  clock and Protocol 5 add/remove packets. Durations are bounded to
  1200 seconds, amplifier to 0..3. The server does not persist effects on logout.
  Unsupported effects and 1.8+ particle visibility arguments are rejected.
- `/summon <entity> [x y z]`: supports 1.7.10 capitalized entity names such
  as `Zombie`, `Cow`, `Pig`, `Creeper`, `Skeleton`, and a selected
  list of other existing Mob types. Supports flat and natural Overworld
  worlds; world collision, capacity, Peaceful and position limits are enforced.
  Entities are saved before the spawn packet is broadcast. Commands from
  the host console require absolute coordinates; verified OP players can
  use `~` coordinates or omit the position. Bosses, NBT payloads, selectors,
  and entities without existing AI are not yet supported.

These commands share the existing verified-OP level-2 gate and console audit log.

## Sequential command roadmap (#649): items 01 and 02

- **01 /enchant** `/enchant <player> <numericId> [level=1]` works from the
  host console and verified in-game OPs. IDs must be in the 1.7.10 registry;
  enchantability, compatibility, level ceilings and pre-existing enchants
  are validated using the same rules as anvil operations. Held-item NBT
  keeps unknown tags and prior damage; the target's own session stages
  the edit and saves the inventory **before** sending the Protocol 5 set-slot
  packet. No XP is charged for a command enchant.
- **02 /testforblock** `/testforblock <x> <y> <z> <block> [data=-1]`
  uses read-only authoritative world data. Data `-1` matches any
  metadata; values 0..15 match exactly. Players can specify `~`
  relative coordinates; console only accepts absolute coordinates.
  Query failure does not mutate world or inventory. Numeric ID and
  legacy name validation deliberately reject newer 1.8+ blocks.
- Neither command yet implements arbitrary target selectors, new
  blockstate syntax or command-block success-count integration.
- Planned ordered follow-up: https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/649

## Roadmap item 03: /seed

- `/seed` displays the actual persisted IRW world generation seed to
  verified in-game OP level 2 or the host console. It uses the active
  world state, not an untrusted player packet or inferred default.
- Flat IRW1 worlds have no persisted seed. The command reports an error
  rather than showing a fictional value; the save format is not rewritten.
- Invalid or extra arguments are rejected. Tests cover both seeded and
  legacy flat worlds.

## Roadmap items 05–07: world spawn, player gamerule, player difficulty (Draft)

The following changes are implemented in a separate branch and **must not be
reported as merged or validated** until Rust tests, local release build,
native 1.7.10 sessions, and the prior item 04 are checked.

### 05 `/setworldspawn [x y z]`

- Present in Minecraft Java **1.7.2+**. `/setworldspawn` uses the verified
  level-2 OP's current Overworld block location. `/setworldspawn x y z`
  supports 1.7.10 `~` relative coordinates for in-game senders; console
  needs three absolute coordinates.
- No 1.16+ yaw argument or 1.13+ local `^` coordinates.
- The world spawn is stored as an atomic, bounded `.worldspawn.json`
  **IRW sidecar** beside the server's active world file; unlike native Anvil,
  IRW has no writable `level.dat` spawn metadata yet. The original save
  format remains unchanged.
- Save completes before a Protocol 5 **S05 Spawn Position (0x05)** packet is
  broadcast. New joins get the stored value, and default world-spawn
  respawns use it when a safe surface is available. Missing values retain
  legacy origin (0, surface, 0). Invalid/corrupt sidecars fail closed.
- Invalid positions, other dimensions, and malformed console forms are
  rejected. Existing explicit bed/command-spawn overrides are not changed.

### 06 `/gamerule <name> [true|false]` (in-game verified OP)

- OP level 2 can read/change the same authoritative rules as the host console:
  `doDaylightCycle`, `keepInventory`, `doMobSpawning`.
- Not-yet-implemented legacy rules are rejected rather than advertised as
  working. Only exact `true`/`false` accepted.
- Uses the same file-backed mutation helper as console; writes to disk
  before publishing the shared state, including the live daylight clock.

### 07 `/difficulty <0|1|2|3|peaceful|easy|normal|hard>`

- OP level 2 uses the same bounds, durable override, Mob lock and AtomicU8
  update as the console. Hardcore remains hard (3).
- Tick-side Vitals and Mob logic consult the live difficulty; on Protocol 5
  the difficulty in S01 Join Game and S07 Respawn reflects it on new
  join/respawn, not via an invented 1.8+ packet.
- `/difficulty query` is a host-console maintenance extension only, not
  advertised as a vanilla 1.7.10 client command.

Manual acceptance: restart and confirm each setting persists; compare
console and verified in-game OP behavior; test permission-denied in
offline-mode; reject malformed arguments, unknown gamerules, and non-finite
spawn coordinates; verify S05 spawn packets, compass behavior and survival
respawn with no bed; check hardcore lock and live Mob changes.

Sources: https://mcasset.cloud/1.7.10-pre4/assets/minecraft/lang/en_US.lang
and https://minecraft.wiki/w/Commands/setworldspawn
