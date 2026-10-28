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

**死亡回数スコアボードの保存前**、かつ**死亡ドロップのコミット前**に、確定した死亡をセッション内で1回記録します。スコアボードの保存失敗時も、診断ログへ到達することを優先します。死亡ドロップの保存が失敗しても、先に死亡ログを残すことを優先します。プロセスの異常終了を挟んだ全ファイル横断のexactly-once保証ではありません。ワールド保存ファイルが `world.irw` の場合、診断ログは同階層の `world.deaths.jsonl` です。次元ごとに別のワールド保存ファイルを使用している場合、ログも対応するワールド側に出ます。

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


## D02: 死亡ログの同期と末尾復旧（部分対応）

- 死亡イベントは、死亡ドロップの保存処理より前にコンソールとJSONLへ書き込みます。ドロップ保存失敗により死亡原因が未記録になる問題を軽減します。
- JSONLの1行追記後に `sync_data()` を実行し、OSへディスク同期を要求します。ログ書き込みに失敗した場合は可能な範囲で追記前の長さに戻します。追記や同期のエラーはコンソールへ通知し、死亡処理は止めません。
- 次回の追記時、ファイル末尾が改行で終わらない場合のみ最大2048バイトを検査します。末尾が**有効なJSON**なら区切りの改行を補い、途中で途切れた**不正なJSON**ならその末尾断片だけ削除します。正常な過去行には触れません。
- 末尾が上限を超え、正常な行境界を見つけられない場合は、自動切り捨てをせずエラーを返し、管理者が調査できる状態を維持します。
- ログへのシリアライズ・書き込みはサーバープロセス内で直列化します。**異なるサーバープロセスが同じログを更新する場合は保護されません**。

### D02追加: イベントIDと再追記の重複抑制

- 新しい死亡ログの各JSONに `event_id`（例: `d2-0123456789abcdef0123456789abcdef`）が追加されます。プレイヤーから渡された文字列ではなく、サーバー側で生成する識別子です。
- コンソールにも同一の `event_id` を表示し、死亡時の症状とJSONLを突き合わせられるようにします。
- 同じ `event_id` を明示して再追記した場合、現在のJSONLの末尾**最大256KiB**を調べ、プレイヤー・死因・次元・サーバーtickが一致するイベントがあれば再追記しません。IDを異なるイベントで使い回した場合はエラーにします。
- 正常な死亡イベントJSONの最後の改行欠落は復旧しますが、死亡イベントではない別種類のJSONは自動修復しません。
- 過去のJSONLには `event_id` がありません。旧形式を自動で書き換えず、新規死亡記録から付加します。

**保証範囲に注意**: これは直近ログの重複再追記対策です。256KiBより古い重複や、プロセス再起動時に発行済みIDを復元できない場合、スコアボード/インベントリ/ドロップを跨ぐ重複は防げません。現状はログIDをプレイヤーの永続状態に保存しておらず、再起動後に同じ死亡イベントが再実行される仕組みも未実装です。これを完全なexactly-once機構と解釈してはいけません。

### D02補強: イベントID衝突の厳密検査とログ優先順位

- 同じ `event_id` が見つかった場合、プレイヤー名・死因・次元・tickだけでなく、**攻撃元のEntity ID、飛び道具の種別、座標、HP、ダメージなど、保存内容全体**が一致することを要求します。再追記時に新しく付けられる `timestamp_ms` の差だけは無視します。
- 同じIDで内容が異なる場合、あるいは直近256KiBのJSONLに同じIDが既に2回以上ある場合は、重複として隠さずエラーを報告します。
- 直近の検査窓に破損した**改行終端済み**JSONL行が存在する場合は、読み飛ばして追記することをやめ、原因調査のためエラーにします。末尾の未完了行は従来の限定的な復旧機能で扱います。
- 死亡診断ログは死亡回数・ドロップ保存より前に書き込みます。診断ログ保存に失敗してもゲーム進行は継続し、死亡回数の保存結果が成功した後にスコア通知・死亡チャットを送ります。

**留意点**: ログ先行により、ログは残っていてもスコアボード保存が失敗するケースがあり得ます。この変更は分散トランザクションを実現するものではありません。イベントIDはプレイヤーNBTやスコアボードに永続リンクされておらず、再起動時に死亡回数・アイテムドロップを自動で補正する処理もありません。
### 未完了の重要課題

この変更のみでは、プレイヤーHPファイル、死亡回数スコアボード、ログ、アイテムドロップが**1つの原子的トランザクション**になるわけではありません。サーバー異常終了時の重複カウントや取りこぼしを完全になくすには、永続的イベントID、処理済みチェック、再実行可能なoutbox/redo、故障注入テストが別途必要です。

### D02検証項目

- [ ] 正常に死亡すると、JSONLの各行は有効で、末尾が改行となる
- [ ] 正常なJSONレコードの末尾だけ改行がない場合、レコードを維持して新規行を追記する
- [ ] 最後のJSON行が途中で途切れた場合、正常行だけを保存し新しい死亡記録を追加する
- [ ] 2048バイトを超える不明な末尾を勝手に削除しない
- [ ] ログ先がディレクトリであるなどI/O障害時もパニックしない
- [ ] ドロップ保存失敗時の死亡ログ、リスポーン、スコアを実機で確認する
- [ ] 同じ `event_id` による再追記は1件のままになり、別プレイヤーへのID再利用は拒否される
- [ ] 完全な最終行から改行が消えても、復旧後のID再追記で重複しない
- [ ] 死亡イベントではないJSONが末尾にあると自動変換・削除しない
- [ ] 同一ID・同一tickでも、射手ID、座標、被ダメージが異なれば衝突エラーとなる
- [ ] 改行終端済みの破損JSON行・同一IDの既存二重記録は隠蔽しない
- [ ] スコアボードI/Oを故障注入しても、先に死亡ログが保存されることを確認
- [ ] `cargo fmt --all -- --check`、`cargo test -p vanilla-server`、Clippy、Windows 1.7.10実機試験をローカルで行う

GitHub ActionsのRust CIは起動・再実行しません。ログの耐障害性を確認する際には必ずワールドのコピーで検証してください。

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

JSONLに `source_type`、`source_entity_id`、`projectile_type` が追加されます。`source_type` は発生元Mobや爆発元の種別、`source_entity_id` はサーバーで確認したID、`projectile_type` は矢・火球などの飛び道具の種類です。確認できない値はnullです。コンソールの死亡ログにも発生元情報を表示します。既存のD03の `mixed_environmental` とD04の `schema_version`、`death_message`、`block_position` は維持します。Entity IDはサーバー内部の識別子で、アカウントUUIDではありません。

### D01受入条件

- [ ] Zombieに倒されると `was slain by Zombie` とMob Entity IDを正確に記録
- [ ] Skeletonが消滅したあとでも矢の射手を記録
- [ ] TNTとCreeperの爆発発生元を区別し、同時に異なる攻撃元があれば `mixed_damage` として扱う
- [ ] 10tick被ダメージ猶予と既存の攻撃アニメーションを維持し、被ダメージ0の場合は死亡原因を捏造しない
- [ ] 過去のMobセーブに `shooter_kind` がなくても `#[serde(default)]` で読み込み可能
- [ ] `cargo fmt --all -- --check`、単体テスト、Clippy、リリースビルド、Minecraft 1.7.10実機で確認

PvPの攻撃者を近くのプレイヤー名から推測しません。完全に重なったプレイヤー同士での飛び道具のターゲット判定は別タスクです。GitHub Actionsは起動しません。
