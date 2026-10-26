# E46: 新規Anvilリージョンと未生成チャンクの段階的redo保存

関連: Issue #225 / #231

## 今回の追加

- `legacy_region_rewrite::insert_regions_with_sidecars_under_lock_expected` に明示的な存在期待（`Some(region bytes)` または `None`）を渡す。
- `Some` は現在のregionのバイト一致を確認してから既存チャンク更新・新規チャンク追加をCOWステージングする。
- `None` はregionファイルが本当に存在しない場合だけ新規regionを生成する。
- 新旧regionとJSONサイドカーを2〜8参加者の既存redoジャーナルで発行する。ステージした全regionにsector/NBT検証を実施する。
- 従来の `rewrite_regions_with_sidecars_under_lock_expected` / `rewrite_chunks_create_only` は既存チャンクのみ更新する契約を維持する。

## 制限と安全条件

**E46の主保存切替は未完了。** 新規APIはオプトインの保存基盤であり、実ゲームのdirty-chunk保存、再接続時ロード、Player・Mob・全TileEntityの双方向同期は未接続。主保存は引き続きIRW。

- `session.lock` を保持した同一ワールドでのみ実行し、コミット障害時は既存redoの復旧まで書込みを停止する。
- 外部のJavaサーバ等がsession lockを無視して同時更新するワールドはサポートしない。
- 既存の1 region 8MiB/redo総計64MiB/8参加者制限を維持する。regionだけ1参加者のコミットは拒否する。
- 未変更チャンクの非圧縮NBTは検証・保持する。更新対象の完全NBTは呼び出し側が供給しなければならない。
- `cargo test` は無改造Minecraft Java 1.7.10実機E42のPASSを意味しない。

## 検証コマンド

```bash
cargo fmt --all -- --check
cargo test -p vanilla-server --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p vanilla-server
```
