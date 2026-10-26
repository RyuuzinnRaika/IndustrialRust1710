# E46: 複数dirtyブロックを複数Anvilリージョンへ原子的保存

前提: #342（IRW＋Anvil redo基盤）、#349（Worldの単一ブロックwrite-through）がmainに統合済み。

## 新たに追加した内部API

`World::update_many_and_flush_existing_anvil(world_irw_path, changes)`

- 1回で1～32ブロックを処理。通常の単一ブロックAPIも同じ処理経路を共有する。
- 1回のredoで最大7既存Anvilリージョン＋IRWスナップショットを原子的に発行する（既存のredo 8参加者上限）。
- 変更前のIRWファイルがメモリ上のIRWと一致するか検証。触れるすべての既存Anvilチャンクを読み込み、**変更前の各ブロックIDとmetadataをIRWと照合**する。
- 重複座標、無効座標、未生成チャンク、未束縛session.lock、対応外Dimension、元ブロックの不一致、8参加者超過を拒否する。
- すべての事前検査に成功してからredoを発行。失敗した場合メモリ上のブロック変更・wake queueの更新を行わない。
- 正常コミット後だけ照明通知、重力更新、近傍ブロック更新、葉の更新を反映。変更なしの場合はfalseを返してディスクを更新しない。

## テスト範囲

- 同じチャンクへの複数変更。
- 複数チャンクと正・負座標をまたぐ複数regionへの同一redo保存。
- 変更予定の2番目のチャンクが実データと一致しないとき、変更予定の1番目も含め保存しない。
- 重複、無効入力、未生成チャンクは元のregionとIRWに変更を加えない。

## 残っている未完了ゲート

**ライブAnvil主保存は引き続き未有効化。** 一般の`update_many_and_save`とtickワーカーはIRW主保存のまま。新規チャンク生成、照明再計算、Mob・全TileEntity・プレイヤー状態、各Dimension、Anvil読込主系、電源断回復、無改造Minecraft Java 1.7.10実機9ケースの完了が必要。

運用前確認：`cargo fmt --all -- --check`、`cargo test --workspace --all-targets`、`cargo clippy --workspace --all-targets -- -D warnings`、Releaseビルド。
