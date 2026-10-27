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
