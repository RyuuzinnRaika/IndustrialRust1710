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

**制限**: 攻撃者はサーバーが取得した確実な情報だけを使用します。原因が観測できない場合は `Alex died` を使います。

## GameRule

- `/gamerule showDeathMessages true` （初期値）: 全プレイヤーへチャット通知
- `/gamerule showDeathMessages false`: チャット通知のみ停止

この設定を `false` にしても、管理者向けコンソール・JSONLログは出力します。以前のVersion 1のgamerules保存ファイルに当該フィールドがない場合は `true` として読み込み、移行時の読み取りエラーを防ぎます。

## 管理者向けログ

死亡回数のスコアボード記録と死亡ドロップのコミット後、死亡確定**1回につき1エントリ**を出力します。ワールド保存ファイルが `world.irw` の場合、診断ログは同階層の `world.deaths.jsonl` です。次元ごとに別のワールド保存ファイルを使用している場合、ログも対応するワールド側に出ます。

コンソール:
```text
[death] player=Alex cause=fall dimension=0 x=0.50 y=64.00 z=0.50 health_before=3.0 applied_damage=3.0 tick=900 message="Alex fell from a high place"
```

JSONL (1行1JSON):
```json
{"event":"player_death","schema_version":1,"timestamp_ms":1790000000000,"player":"Alex","cause":"fall","death_message":"Alex fell from a high place","dimension":0,"position":{"x":0.5,"y":64.0,"z":0.5},"block_position":{"x":0,"y":64,"z":0},"health_before":3.0,"applied_damage":3.0,"server_tick":900}
```

`dimension` はバニラの値（ネザー -1 / オーバーワールド 0 / エンド 1）。`health_before` は致死ダメージ直前のHP、`applied_damage` はHPへ**実際に適用された**ダメージです（超過ダメージは含まれません）。`timestamp_ms` はUnix時刻のミリ秒、`server_tick` はサーバーのTickです。復旧・再接続で原因が追跡不能の場合は `unknown`、HP情報は `null` になります。

ログ保存が失敗しても、死亡処理とリスポーンを止めません。コンソールには `DEATH LOG WRITE ERROR` が表示されます。ユーザー入力をJSONに格納するので、名前などの改行はエスケープされ、ファイルへの任意行注入を避けます。コンソール出力中のプレイヤー名と死亡メッセージも改行・制御文字をエスケープし、偽のログ行が混入しないようにします。IPアドレス・認証トークンは記録しません。

### 詳細フィールド（D04）

- `schema_version` は新形式で **1**。以前のJSONLにこのフィールドがない場合は、旧形式として解釈してください。過去ログの書き換えは行いません。
- `death_message` はゲーム内へ送るのと同じ死亡メッセージ文字列（`showDeathMessages=false` でも監査用に保存）。
- `position` は小数点を含むプレイヤー位置、`block_position` は各座標を床関数で整数化したブロック位置です。例えば `x=-0.01` のブロック座標は `-1` で、0にはなりません。
- 壊れた/非有限の座標からブロック値を推測しないため、該当する `block_position` 成分は `null` とします。
- D04は死亡事実の情報追加、D03は混合環境ダメージの分類、D01はサーバーが確認したMob・飛び道具・爆発の出所を追加します。D02の耐障害性とexactly-onceは別の残課題です。

## ローカルでの検証 (GitHub Actions不要)

```powershell
cargo fmt --all -- --check
cargo test -p vanilla-server
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p vanilla-server
Get-Content .\world.deaths.jsonl -Tail 20
```

ワールドファイル名は実際の `--world` パスへ読み替えてください。サバイバルで落下・溺水・空腹・炎上・矢・Mob・エンダーパールの各死亡を再現し、(1)チャット通知が1回のみ、(2)JSONLに各死因・死亡メッセージ・整数ブロック座標が残る、(3)ゲームルールがfalseでもログは残る、(4)死亡回数とドロップが従来どおり、(5)リスポーン後に新しい死亡が記録できることを確認してください。

**注意**: これは**死亡ログの詳細化**であり、未実装の被ダメージ源を実装する変更ではありません。大きな高負荷検証を始める前にワールドのバックアップを取ってください。


## D03: environmental source aggregation

Hazard ticks are bounded to four steps per owning session. The existing code
returned only the **last** observed source, which could falsely claim a burn
caused a death when drowning and lava dealt damage in the same batch.

`HazardAttribution` now tracks either no source, exactly one consistent
source, or a genuinely mixed set of sources. Combining repeated damage from
the same source retains that source; combining different sources uses
`mixed_environmental` and the death chat text:
`Alex died from multiple environmental hazards`.

This classification is conservative. When the engine aggregates multiple
environmental hits in one health update it cannot prove which component
crossed zero first without changing the combat timing model. A mixed event
therefore avoids making an unsupported claim rather than inventing a source.

Local acceptance tests:
- [ ] Run the combined lava-feet/water-eye Rust unit test.
- [ ] Drowning alone remains `drowning` across the 300-air-tick window.
- [ ] Mixed exposure is `mixed_environmental`, not `combat_or_explosion`.
- [ ] Repeated same-source hits do not become mixed.
- [ ] No extra HP loss or delayed/duplicated health packets.

## D01: Mob・飛び道具・爆発の発生元

- Mob近接攻撃：Mob種別とサーバーが採番したEntity ID
- 飛び道具：発射時点の射手のMob種別とEntity IDを保存（射手が消滅しても引き継ぐ）
- 爆発：TNT、Creeper、Ghast Fireball、Wither Skullの発生元
- Wither状態異常：近接攻撃や飛び道具の致死ダメージと区別
- 異なる攻撃元が同一HP更新に混在する場合：単独の攻撃者を推測せず `mixed_damage`

JSONLに `source_type` と `source_entity_id` が追加されます。確認できない値はnullです。既存のD03の `mixed_environmental` とD04の `schema_version`、`death_message`、`block_position` は維持します。Entity IDはサーバー内部の識別子で、アカウントUUIDではありません。

### D01受入条件

- [ ] Zombieに倒されると `was slain by Zombie` とMob Entity IDを正確に記録
- [ ] Skeletonが消滅したあとでも矢の射手を記録
- [ ] TNTとCreeperの爆発発生元を区別し、同時に異なる攻撃元があれば `mixed_damage` として扱う
- [ ] 10tick被ダメージ猶予と既存の攻撃アニメーションを維持し、被ダメージ0の場合は死亡原因を捏造しない
- [ ] 過去のMobセーブに `shooter_kind` がなくても `#[serde(default)]` で読み込み可能
- [ ] `cargo fmt --all -- --check`、単体テスト、Clippy、リリースビルド、Minecraft 1.7.10実機で確認

PvPの攻撃者を近くのプレイヤー名から推測しません。完全に重なったプレイヤー同士での飛び道具のターゲット判定は別タスクです。GitHub Actionsは起動しません。
