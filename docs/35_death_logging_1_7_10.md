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


## D01: attacker and explosion provenance (follow-up)

The server now keeps the identity of the actual origin alongside each pending damage event:
- Direct melee: Mob species and server-owned entity ID.
- Projectiles: projectile kind plus firing Mob species/entity ID captured **at launch**, not inferred from who is nearby at death. This survives shooter despawn while the projectile is in flight.
- Explosions: Creeper, Ghast Fireball, Wither Skull and TNT, retaining their source entity IDs when available.
- Wither effect: distinct from melee and projectile impacts.
- **Concurrent, different damage origins**: `mixed_damage` rather than claiming an arbitrary Mob killed the player.

The death JSONL schema includes nullable `source_type` and `source_entity_id` fields. Source IDs identify the **in-world entity** and are not Mojang account IDs, and `source_type` names a server-observed species or explosion origin; neither is supplied by the untrusted client.

Example:
```json
{"event":"player_death","player":"Alex","cause":"mob","source_type":"Zombie","source_entity_id":500002,"dimension":0}
```
(The example omits required timestamp/position/damage fields for readability.)

**Limitations**: Multiple different hits coalesced into one HP update remain `mixed_damage`, not a single credited killer. Explicit player-vs-player damage attribution is not available until the PvP damage path exists. This change does not invent modded entity names. Projectiles continue to use the existing position-based target selection; disambiguation for two players overlapping at the same coordinates remains a separate gameplay fix.

### Additional acceptance criteria

- [ ] Mob melee death emits `was slain by Zombie` and a matching entity ID.
- [ ] Skeleton arrow death retains the original shooter ID even if the skeleton despawns before impact.
- [ ] TNT/creeper explosions report separate types; concurrent blast and projectile become `mixed_damage`.
- [ ] Invulnerability grace suppresses both damage and corresponding false death attribution.
- [ ] Saved 1.7.10 Mob snapshots without `shooter_kind` load via Serde defaults.
- [ ] `cargo fmt --all -- --check`, `cargo test -p vanilla-server`, `cargo clippy --workspace --all-targets -- -D warnings`, release build and native 1.7.10 tests **locally**.

GitHub Actions Rust CI must remain disabled. Do not launch or rerun it.
