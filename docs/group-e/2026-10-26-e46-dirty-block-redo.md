# E46: IRWスナップショットとAnvil dirty-block差分の同一redo保存

## 追加した内部API

- `legacy_chunk_patch::patch_chunk`: 既存Anvil NBTへ絶対座標の変更ブロック（block ID + metadata）のみ反映し、その他のNBT（未知タグ、TileEntity、Entities、未変更section）を保持する。
- 新しいsectionが必要なら生成し、変更列のHeightMapを再計算する。LightPopulated=0として再照明が必要であることを示す（照明処理自体は未実装）。
- ID >255のmoddedブロックとTileEntity座標への更新、境界外座標、不正NBT、重複更新を拒否する。
- `legacy_region_rewrite::flush_irw_block_edits_under_lock_expected`: 既存chunkの明示的なblock変更とIRWの次世代snapshotを、session.lock・regionバイト一致・旧IRW一致の検証後に1つのredoへ保存する。
- 失敗時にregionかIRWの片方だけを公開しない。redoが公開された後の失敗は既存の起動時リカバリで同じ世代を再生する。

## 注意: ライブAnvil主保存はまだ有効化しない

この内部APIは既存Anvil chunkに限定する。新規chunkの生成はPR #324のAPIが担い、生成した基準NBTとゲーム側の変更履歴の一致検証を行うまではライブのdirty-chunk-workerへは接続しない。
IRWに無い未知NBTの維持が必要なため、全ワールドNBTをIRWから再生成する方式は採用しない。
全コンテナとプレイヤーを含む各保存ルート、ライト更新、Dimensionごとのredo、9ケースの無改造Java 1.7.10クライアントによる実機総合試験、強制終了と停電試験がE46完了前に必要。

## CI

```bash
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p vanilla-server -p legacy-status-server
```
