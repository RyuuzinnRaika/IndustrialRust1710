# E46: session-bound World → Anvil block write-through (opt-in)

## 追加API

`World::update_and_flush_existing_anvil(path, x, y, z, block)` は、**明示的に呼び出した場合だけ**既存のAnvilチャンクと同じ世代のIRWスナップショットへ1ブロック変更を書き込みます。

1. `session.lock` が束縛されたOverworldのWorldだけを受け付ける。
2. メモリ上のIRWバイト列と現在のIRWファイルが同一であることを確認する。
3. 対象Anvil領域がregular fileであり最大8MiBであること、対象チャンクが存在することを確認する。
4. **変更前ブロックのID・metadataが、AnvilとIRWで一致する場合だけ**次へ進む。既存Anvil内容が異なる場合は失敗。
5. 別スナップショットとして次のIRWを作り、既存の`flush_irw_block_edits_under_lock_expected`経由でAnvil＋IRWを同じredoトランザクションとして確定する。
6. コミット成功後に限り、メモリ上のWorld変更と照明更新・重力・ブロックtickを反映する。

`World::save` と `World::encoded` は、セッション束縛済みワールドに未復旧 `*.pending-txn.json` がある場合、続けて保存することを拒否する。これにより、失敗後に旧IRWだけを上書きする事故を防ぐ。

## 安全上の未実装部分

- 通常のネットワークゲームプレイはこれまで通りIRW保存。**Anvilを主保存にするランタイムフラグはまだ追加していない。**
- 新規チャンク・ネザー・エンド・マルチブロック変更・コンテナやMob・プレイヤーのトランザクションはこの関数の対象外。
- 非同期tickによるIRW単独保存経路を全てAnvil経路へ切り替える前に、互換性と故障注入による復旧テストが必要。
- バイト単位のIRW一致を要求するため、古い世代のファイルに対する変更はfail-closedとなる。
- E42で要求する無改造Minecraft 1.7.10クライアントの9ケース実機試験は別途必要。

## テスト

`cargo test --workspace --all-targets` で実際のAnvil領域とIRWの同世代保存、再書込み、ファイル不一致、pending redo、セッション未束縛時の拒否を回帰検証する。
