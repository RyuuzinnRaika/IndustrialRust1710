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

### D02 still outstanding

This outbox is saved after the player HP transition is written and after the
existing scoreboard death increment in the session. **A crash between these
operations can still leave a missing death event or an inconsistent score.**
The outbox protects only events whose preparation completed. Achieving strict
cross-file exactly-once requires a single redo transaction for **player HP,
scoreboard and the event outbox**, plus tested power-failure recovery. The
current improvement must **not** be represented as full D02 completion.

### Manual and local tests

- [ ] \`cargo fmt --all -- --check\`
- [ ] \`cargo test -p vanilla-server death_log\`
- [ ] \`cargo clippy --workspace --all-targets -- -D warnings\`
- [ ] Kill the server after outbox preparation but before/after JSONL write,
      restart and verify exactly one \`event_id\` row.
- [ ] Verify two successive deaths of one player produce two different IDs.
- [ ] Verify an invalid outbox fails closed, without modifying world files.
- [ ] Verify existing legacy death rows lacking \`event_id\` remain readable.
- [ ] Check TPS cost as JSONL approaches 64 MiB.
