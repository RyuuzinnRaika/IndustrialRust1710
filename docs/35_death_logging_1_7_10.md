# 死亡ログと死亡メッセージ (Java 1.7.10 / Protocol 5)

この文書は死亡時の**ゲーム内通知**と、管理者向けの**コンソール・ファイルログ**を説明します。

## 表示例

- 落下: `Alex fell from a high place`
- 溺水: `Alex drowned`
- 窒息: `Alex suffocated in a wall`
- マグマ: `Alex tried to swim in lava`
- 炎上: `Alex burned to death`
- 空腹: `Alex starved to death`
- ディスペンサーの矢: `Alex was shot by a dispenser`
- エンダーパール: `Alex died after using an ender pearl`
- Mob・飛び道具・爆発が混在する被ダメージ経路: `Alex was killed in combat or an explosion`

**制限**: 既存のMob被ダメージ集約処理は攻撃者ID・Mob種別・TNT/クリーパーの区別を返さないため、個別の攻撃者名を推測しません。この種類の正確な attribution は将来の別対応が必要です。原因が観測できない場合は `Alex died` を使います。

## GameRule

- `/gamerule showDeathMessages true` （初期値）: 全プレイヤーへチャット通知
- `/gamerule showDeathMessages false`: チャット通知のみ停止

この設定を `false` にしても、管理者向けコンソール・JSONLログは出力します。以前のVersion 1のgamerules保存ファイルに当該フィールドがない場合は `true` として読み込み、移行時の読み取りエラーを防ぎます。

## 管理者向けログ

死亡回数のスコアボード記録と死亡ドロップのコミット後、死亡確定**1回につき1エントリ**を出力します。ワールド保存ファイルが `world.irw` の場合、診断ログは同階層の `world.deaths.jsonl` です。次元ごとに別のワールド保存ファイルを使用している場合、ログも対応するワールド側に出ます。

コンソール:
```text
[death] player=Alex cause=fall dimension=0 x=0.50 y=64.00 z=0.50 health_before=3.0 applied_damage=3.0 tick=900
```

JSONL (1行1JSON):
```json
{"event":"player_death","timestamp_ms":1790000000000,"player":"Alex","cause":"fall","dimension":0,"position":{"x":0.5,"y":64.0,"z":0.5},"health_before":3.0,"applied_damage":3.0,"server_tick":900}
```

`dimension` はバニラの値（ネザー -1 / オーバーワールド 0 / エンド 1）。`health_before` は致死ダメージ直前のHP、`applied_damage` はHPへ**実際に適用された**ダメージです（超過ダメージは含まれません）。`timestamp_ms` はUnix時刻のミリ秒、`server_tick` はサーバーのTickです。復旧・再接続で原因が追跡不能の場合は `unknown`、HP情報は `null` になります。

ログ保存が失敗しても、死亡処理とリスポーンを止めません。コンソールには `DEATH LOG WRITE ERROR` が表示されます。ユーザー入力をJSONに格納するので、名前などの改行はエスケープされ、ファイルへの任意行注入を避けます。IPアドレス・認証トークンは記録しません。

## ローカルでの検証 (GitHub Actions不要)

```powershell
cargo fmt --all -- --check
cargo test -p vanilla-server
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p vanilla-server
Get-Content .\world.deaths.jsonl -Tail 20
```

ワールドファイル名は実際の `--world` パスへ読み替えてください。サバイバルで落下・溺水・空腹・炎上・矢・Mob・エンダーパールの各死亡を再現し、(1)チャット通知が1回のみ、(2)JSONLに各死因と座標が残る、(3)ゲームルールがfalseでもログは残る、(4)死亡回数とドロップが従来どおり、(5)リスポーン後に新しい死亡が記録できることを確認してください。

**注意**: これは**死亡ログの詳細化**であり、未実装の被ダメージ源を実装する変更ではありません。大きな高負荷検証を始める前にワールドのバックアップを取ってください。


## D02: fail-safe logging improvements (partial)

The death announcement and diagnostic append happen **before** the potentially
failing death-drop journal transaction. A failed inventory/drop commit therefore
does not skip the first death message and diagnostic log.

A successful JSONL append now calls `sync_data()` before returning, and a
short/failed `write_all` attempts to truncate the partial suffix. A separate
test simulates an unwritable diagnostic destination. The diagnostic log is still
best-effort: an I/O error is reported to the console and does not block
ordinary death-drop/respawn processing.

**Not yet complete:** This is not a multi-file transaction. A power failure
after player health becomes zero but before scoreboard/death-log processing can
still lose the death-log event, and a crash between multiple durable files can
leave a temporarily inconsistent score. Strict cross-restart exactly-once
semantics require an outbox/checkpoint protocol keyed by a durable unique death
ID and redo/recovery logic shared with the player, scoreboard and inventory
journal. Do not mark D02 fully accepted until this has been implemented and
tested with fault injection.

### D02 validation

- [ ] Fail a death-drop commit and verify that the death log still exists.
- [ ] Fail the death-log destination and verify that ordinary death/respawn
      continues without a process crash (error visible to an administrator).
- [ ] Crash/restart after each checkpoint and confirm no loss/duplication
      once durable event IDs/redo recovery are implemented.
- [ ] Confirm `sync_data` and retry I/O do not cause pathological 20TPS stalls
      during bursts of deaths.


## D02 continuation: durable last-event outbox and crash replay

A per-player last-death outbox is now saved atomically before the JSONL
append. Files live next to a player's saved NBT, for example
\`players/alex.death-outbox.json\` or
\`players/verified/<uuid>.death-outbox.json\`.

Each generated death event has a random 128-bit \`event_id\` stored both in the
outbox and its JSONL entry. On startup the server scans only those two player
directories, validates the bounded JSON outboxes, and appends events missing
from their corresponding world/dimension death logs. Repeated recovery does
not duplicate an event ID; a torn trailing JSONL line is removed and replaced
by a complete copy of the durable outbox event.

The append path is **serialized**, scans at most 64 MiB of existing log data,
and invokes \`sync_data()\` after each new row. The 64 MiB bound is intentional:
if reached, manual log rotation is required rather than allocating
unbounded memory or silently duplicating old IDs.

Failure policy: invalid/symlinked/outsize outboxes and corrupt complete
JSONL rows cause recovery to fail closed, with a visible startup error rather
than discarding evidence. Normal death/respawn remains best-effort if the
diagnostic outbox cannot be created during play.

### D02 atomic death-accounting improvement

For the seven identified fatal-damage paths, the first HP transition to zero
now uses the existing bounded \`pending-txn.json\` redo journal to commit
**three participants in one transaction**:

1. The player's gzip-NBT position/vitals (including 0 HP)
2. The player's new \`*.death-outbox.json\` event ID and full death event
3. The world scoreboard image with the incremented \`deathCount\` objective

The scoreboard is updated in memory and broadcast **only after** the redo
commit succeeds. A repeated dead-session loop cannot increment deathCount
again, and the JSONL log replays the committed outbox ID once. The journal
target whitelist now admits **only identity-scoped** player death outboxes and
root-level scoreboard files; symlinks remain prohibited.

If a fatal journal application fails, disconnect-time player NBT save is
suppressed so it cannot overwrite prepared redo targets before recovery.

The fallback for any *unrecognized future* fatal damage path still uses the
legacy separate scoreboard+outbox sequence; it must eventually be replaced by
the shared fatal transaction wrapper. The entire change requires **Rust
formatting, unit/integration tests, failpoint-based recovery and a 1.7.10
native client run before being considered accepted**. Source-only inspection
does not establish full D02 completion.

### Manual and local tests

- [ ] \`cargo fmt --all -- --check\`
- [ ] \`cargo test -p vanilla-server death_log\`
- [ ] \`cargo clippy --workspace --all-targets -- -D warnings\`
- [ ] Kill the server after outbox preparation but before/after JSONL write,
      restart and verify exactly one \`event_id\` row.
- [ ] Verify two successive deaths of one player produce two different IDs.
- [ ] Verify an invalid outbox fails closed, without modifying world files.
- [ ] Verify existing legacy death rows lacking \`event_id\` remain readable.
- [ ] Power-fail between prepare, journal rename, each target rename, and
      journal cleanup; recover and verify NBT HP/outbox/deathCount agree.
- [ ] Repeated fatal loop and reconnect cannot increase deathCount twice.
- [ ] The old, uncategorized fatal-path fallback is replaced by the same redo
      transaction before D02 is marked fully complete.
- [ ] Check TPS cost as JSONL approaches 64 MiB.


## D04 integration / D05 cactus-death implementation

The D01 typed Mob / projectile / TNT attribution and D03 mixed-hazard
classification are now integrated with D02's durable three-part death
transaction in a **separate unmerged branch**. Native protocol tests have not
been run here. Do not merge solely based on source inspection.

For D05, cactus damage was added to \`Hazards::tick_detailed\`, using a
bounded player bounding-box overlap against cactus block ID **81**, so touching
the side matters even when the player feet are not inside a cactus block.
It deals 1 raw HP with the existing armor-adjusted damage path, at most once
every 10 server ticks, and is reported as \`cactus\`. A fatal encounter reports:
\`Alex was pricked to death\`.

The cactus source merges into \`mixed_environmental\` when a separate
environmental hit is also applied in the same hazard batch; it never
arbitrarily replaces the first source.

### D05 implementation status (source-level; not yet tested)

- **Void**: negative-Y movement, -64 boundary, server-owned 4 HP/10 tick
  fatal source. Moving and respawning in the void require real client tests.
- **Cactus**: AABB contact and single-hazard/mixed-hazard classification.
- **Falling anvil**: server world records bounded landing damage and routes it
  to the victim's owning session. Flight origins are now durably co-saved in
  the IRW world snapshot (TNT5 trailer) so reloads preserve distance.
  The falling block still uses an emulated block-to-block model rather than
  a true 1.7.10 falling-block entity.
- **Lightning**: bounded natural storm strike generation, Protocol 5 S2C
  Spawn Global Entity (0x2C) for client visualization, and server-owned 5 HP
  direct-hit damage. Subsequent burning/charged-creeper effects and exact
  vanilla per-chunk probabilities need further implementation.
- **Magic**: authorized `/effect <player> 7 [seconds] [amplifier]` applies
  6, 12, 24 or 48 instant HP damage once, with `magic` source.
  Splash potion ownership and other indirect magic remain incomplete.
- **/kill**: verified OP level 2 or host console requests a target-owned kill,
  which follows the same durable HP/deathCount/event/respawn pipeline.
  Supports Survival and Creative; entity selectors and mob killing still
  require dedicated command handling.

This work does not create a PR because the repository's
\`.github/workflows/ops-python.yml\` is configured to launch GitHub Actions
on every pull-request creation/synchronization. Branch-only pushes to this
non-default, non-release branch avoid those workflow triggers.

## 2026-10-28: D04 reintegration against main and D05 void/anvil work

- The project death pipeline was transplanted onto a new branch based on
  current `main`, retaining newer gameplay and command changes. No PR
  is opened because unrelated workflows launch on pull_request.
- Legacy 1.7.10 void damage begins below Y=-64, removing the incorrect
  Y=0 physics floor. A bounded negative-Y position is now allowed; the
  session inflicts **4 armor-bypassing health points per 10 ticks** and
  records `void` / `<player> fell out of the world`. The current
  implementation applies only to Survival; Creative and riding-player
  interactions need additional tests/implementation.
- Existing block-based gravity has a tracked in-memory anvil origin and emits
  one landing event with `min(2*(blocks_fallen-1),40)` damage. The event
  is delivered only to intersecting players in the active dimension; the
  authenticated owning session performs the health mutation, death
  transaction, health packet, and `falling_anvil` death message.
- The world still emulates falling blocks as block-to-block moves,
  not Protocol 5 falling-block entities. A later change saves and loads
  anvil flight origin heights together with the IRW world image in a
  bounded TNT5 extension. This fixes the previously documented loss of
  distance across restart but does **not** implement full vanilla falling
  physics, damage caps across helmet/armor, or falling-block network entities.

### Acceptance tests before integration

- [ ] `cargo fmt --all -- --check` — not run here
- [ ] `cargo test -p vanilla-server` — not run here
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` — not run here
- [ ] `cargo build --release -p vanilla-server` — not run here
- [ ] Protocol 5 client: test Y=-0.5, -63.5, -64.1, and death after
      the fifth 4-point void hit; test respawn in the original dimension.
- [ ] Protocol 5 client: place an anvil three blocks above a player and
      check correct single hit, no distant damage, proper death drops;
      repeat while another player stands in a different dimension.
- [ ] Test shutdown/reload while an anvil is falling; no phantom killer or
      duplicated hit.
- [ ] Retest movement rollback, teleport ACK, world save/reload and
      the existing 37+ protocol5 end-to-end tests.
- [ ] Verify no GitHub Actions were triggered; Actions are **not** a test
      destination for this project.

## 2026-10-28 continuation: D05 weather/effects/commands and D06 text

Implemented in the same feature branch:

- In-flight anvil height is part of the **same IRW save** as its new block
  position, not a non-transactional sidecar. TNT1..4 snapshots remain
  readable; TNT5 adds a bounded origin table. Corrupt/truncated tables fail
  closed. Restart-resume test added.
- During Overworld thunderstorms, one bounded random lightning trial is made
  per simulated world tick, with a strike near an online player. A source
  event is dispatched to each intersecting player session (not an untrusted
  network request); the 1.7.10 client is shown global-entity type 1.
  Lightning does not yet implement fire, mob conversions, or exact vanilla
  probability and should not be marked full behavioral parity.
- The command `/summon LightningBolt` is **not** implemented: that command
  target was not available in Minecraft Java 1.7.10 (added in 1.8).
- Effect ID 7 is **instant** harm, never queued as a continuing status
  effect. Fatal damage uses the same journaled scoreboard/HP/log transaction.
- Admin `/kill` uses authenticated OP authorization (or console) and a
  private owner-session mailbox. Creative respawn preserves game mode and
  avoids survival inventory/XP drops.
- D06 begins differentiating arrow, Ghast/Blaze fireballs, witch potion magic,
  Creeper, TNT and Wither Skull death text; exact item/killer ownership and
  recent-attack contextual variants are not finished.
- A post-development merge from `main` retained existing non-death world
  and player changes without using GitHub Actions.

**Unverified gates**: `cargo fmt --all -- --check`,
`cargo test -p vanilla-server`, Clippy, native 1.7.10 protocol client,
concurrent deaths, outbox failpoints, anvil restart, lightning packet decoding,
command authorization and Creative respawn. No test or CI pass is claimed.

**GitHub Actions must not be started**. No PR was opened for this branch
because existing repository workflows run on pull-request events.
