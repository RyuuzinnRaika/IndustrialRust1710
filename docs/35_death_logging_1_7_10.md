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

死亡回数のスコアボード記録後、**死亡ドロップのコミット前**に、確定した死亡をセッション内で1回記録します。死亡ドロップの保存が失敗しても、先に死亡ログを残すことを優先します。プロセスの異常終了を挟んだ全ファイル横断のexactly-once保証ではありません。ワールド保存ファイルが `world.irw` の場合、診断ログは同階層の `world.deaths.jsonl` です。次元ごとに別のワールド保存ファイルを使用している場合、ログも対応するワールド側に出ます。

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
- D04は**観測可能な死亡事実の情報追加のみ**です。まだ識別できないMobの攻撃者や複数の環境要因を推測することはせず、個別に作業中のD01/D02/D03との統合が必要です。


## D02: 死亡ログの同期と末尾復旧（部分対応）

- 死亡イベントは、死亡ドロップの保存処理より前にコンソールとJSONLへ書き込みます。ドロップ保存失敗により死亡原因が未記録になる問題を軽減します。
- JSONLの1行追記後に `sync_data()` を実行し、OSへディスク同期を要求します。ログ書き込みに失敗した場合は可能な範囲で追記前の長さに戻します。追記や同期のエラーはコンソールへ通知し、死亡処理は止めません。
- 次回の追記時、ファイル末尾が改行で終わらない場合のみ最大2048バイトを検査します。末尾が**有効なJSON**なら区切りの改行を補い、途中で途切れた**不正なJSON**ならその末尾断片だけ削除します。正常な過去行には触れません。
- 末尾が上限を超え、正常な行境界を見つけられない場合は、自動切り捨てをせずエラーを返し、管理者が調査できる状態を維持します。
- ログへのシリアライズ・書き込みはサーバープロセス内で直列化します。**異なるサーバープロセスが同じログを更新する場合は保護されません**。

### 未完了の重要課題

この変更のみでは、プレイヤーHPファイル、死亡回数スコアボード、ログ、アイテムドロップが**1つの原子的トランザクション**になるわけではありません。サーバー異常終了時の重複カウントや取りこぼしを完全になくすには、永続的イベントID、処理済みチェック、再実行可能なoutbox/redo、故障注入テストが別途必要です。

### D02検証項目

- [ ] 正常に死亡すると、JSONLの各行は有効で、末尾が改行となる
- [ ] 正常なJSONレコードの末尾だけ改行がない場合、レコードを維持して新規行を追記する
- [ ] 最後のJSON行が途中で途切れた場合、正常行だけを保存し新しい死亡記録を追加する
- [ ] 2048バイトを超える不明な末尾を勝手に削除しない
- [ ] ログ先がディレクトリであるなどI/O障害時もパニックしない
- [ ] ドロップ保存失敗時の死亡ログ、リスポーン、スコアを実機で確認する
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
