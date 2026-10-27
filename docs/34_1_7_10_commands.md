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

## Vanilla 1.7.10 Tab completion (Draft)

Press **Tab** in the vanilla Java 1.7.10 chat/command input. The unmodified
client sends C14 `Tab Complete` (play/serverbound packet `0x14`), containing
one Minecraft string. The server responds on the same player connection
with S3A `Tab Complete` (play/clientbound packet `0x3A`): a VarInt
candidate count and that many Minecraft strings. **There is no Brigadier
transaction ID or modern command-tree packet in Protocol 5.**

Implemented suggestions:

- Command names, including `/help`, `/tell`, `/msg`, `/w`, and
  currently implemented verified-OP commands, filtered by required level.
- Currently online player usernames for private messages and supported
  target-player admin commands (no arbitrary offline name guessing).
- Existing 1.7.10 command arguments such as `/gamemode creative`,
  `/time set day`, `/weather rain`, `/summon Zombie`,
  bounded common legacy item/block names, and `~` for relative coordinates.
- Ordinary chat username completion, as supported by the vanilla client.

No server-side command executes on Tab. Completion cannot authorize
privileged actions: OP level is loaded for the **verified UUID** of the
current session; offline-mode users are never treated as OP by name.
No 1.8+ command suggestions (e.g. `/fill` or `/execute`).
The server caps requests at 256 characters and responses at 32 suggestions,
with deduplicated, deterministic ordering. The vanilla client owns
display/cycling/selection; no new GUI or mod is required.

Regression checks: Rust unit tests cover command names, OP isolation,
partial username matching, argument keywords, invalid input, and the S3A
VarInt/string wire format. A native Protocol 5 TCP integration test sends
a real C14 and checks the S3A response, including that unauthorized
players cannot enumerate OP-only commands.

No GitHub Actions are required or invoked by this feature. Before merging,
run `cargo fmt --all -- --check`, `cargo test -p vanilla-server`,
`cargo clippy --workspace --all-targets -- -D warnings`, and verify the
actual Java 1.7.10 client's Tab suggestions on Windows.

Wire reference: https://prismarinejs.github.io/minecraft-data/protocol/pc/1.7/
