# Protocol 5 movement and zombie-combat reference (2026-11-06)

## Provenance and privacy

Based on a user-supplied `.pcapng` inspected off-repository. The raw
capture contains unrelated local/network traffic and must **not** be committed.
Only anonymized measurements below and the tiny feet-Y trace in the Rust
regression test are retained. No client name, authentication data, addresses,
or full packet payloads are retained.

The file spans about 53 seconds. The reassembled Minecraft-shaped TCP stream
is about 46 seconds long, has no observed TCP byte gaps, and is consistent
with Minecraft Java 1.7.10 / Protocol 5 packet IDs. Login negotiation was not
present in the captured portion, so the trace alone does not cryptographically
prove the executable/client version.

## Observed packet facts

- Serverbound 0x04 (position): 469 packets; 0x06 (position + look): 191;
  0x03 (onGround only): 203. Active movement packets occur about every
  **50 milliseconds / 20 ticks per second**.
- A complete flat-ground jump (normalized from feet Y=68.0 to Y=64.0)
  produces the feet-Y samples:
  `64.0, 64.419999986887, 64.753199980521, 65.001335979112,`
  `65.166109260938, 65.249187078745, 65.249187078745,`
  `65.170787077219, 65.015555072702, 64.785027703789,`
  `64.480710876332, 64.104080378093, 64.0`.
  The peak is **1.249187 blocks** above the starting ground; onGround
  changes from true to false at launch and returns to true at landing.
- Starting vertical motion is about +0.42 blocks/tick, then +0.3332,
  +0.248136, etc. Subsequent vertical velocity is consistent with
  `(velocity - 0.08) * 0.98`; the trajectory is client-simulated and
  delivered as Protocol 5 position packets, not a serverbound jump button.
- The same session contained repeated jumps during combat and clientbound
  health updates of 17, 14, 11, 12, 9, 6, 3, and 0 HP. A Zombie death
  message was observed. One serverbound Use Entity attack and one swing
  were captured; damage source for each earlier health update cannot be
  uniquely proven from health packets alone.

## Implementation decisions

- Preserve the existing client-predicted jump contract in
  `physics::valid_movement` and pin its acceptance and fall-damage behavior
  to the anonymized packet trace; **do not invent an additional server jump
  impulse on C03/C04**.
- For zombies, replace the generic 1.6-block 3D sphere with the legacy
  EntityAIAttackOnCollide reach check: squared feet distance must be at most
  `(zombie_width * 2)^2 + player_width` (adult width 0.6, player width 0.6).
  This **narrows** overly generous reach, including some jump-apex hits;
  it does not interpret vertical hitbox overlap as permission to hit.
  Preserve the 20-tick bite interval, difficulty scaling, and per-player
  damage grace. Other Mob kinds retain their preexisting reach pending
  species-specific review. Add tests for valid airborne contact, an airborne
  miss that the old 1.6 sphere incorrectly accepted, and bite intervals.
- These are **bounded fidelity improvements**, not proof of full 1.7.10
  combat compatibility. AI awareness, wall occlusion, enchantments, armor,
  multiple attackers, hit invulnerability details, knockback, and the
  unmodified client's end-to-end E42 observations require separate evidence.

## Reproduction gates

Run on a machine with the Rust toolchain, using the patch commit SHA:

```powershell
cargo fmt --all -- --check
cargo test -p vanilla-server --bin vanilla-server vanilla_1710_packet_trace_jump_apex_and_landing_are_accepted
cargo test -p vanilla-server --bin vanilla-server zombie_melee_reaches_jump_apex_and_respects_bite_timing
cargo test -p vanilla-server --bin vanilla-server zombie_melee_cannot_hit_above_its_body_or_beyond_horizontal_reach
.\verify-windows.ps1
```

Use a fresh disposable test world for human E42 observations. Never load,
migrate, truncate or overwrite an existing player world for these tests.

Historical references: Minecraft 1.7.10 packet ID map
(https://gist.github.com/leafypout/5d2d966450055ca686cf96979c08794d),
1.7.10-era living-entity hit behavior
(https://skmedix.github.io/ForgeJavaDocs/javadoc/forge/1.7.10-10.13.4.1614/net/minecraft/entity/EntityLivingBase.html).
