# E45/E47: 複数 Anvil region と native sidecar の1世代redoトランザクション

関連: [Issue #225](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/225)

## この変更で実装した範囲

- `journal::commit` は2〜8ファイルを一つのredo世代として書き込める。従来のファイル単位上限 (region 8MiB, その他1MiB)、journal総計64MiB、対象パス検証は維持する。
- `legacy_region_rewrite::rewrite_regions_with_sidecars_under_lock_expected` は、**存在する**複数regionに対する既存chunk変更とnative JSONを、一つのredo journalにまとめる。読み取り時のregionイメージを必須とし、コミット直前のバイト一致・session.lock・NBT検証に失敗した場合は確定しない。
- `legacy_tile_rewrite::rewrite_chests_with_sidecar_under_lock` は、同じワールドの異なるregionにあるChest複数個をまとめて更新できる。native sidecar上のSlot/Count/Item一致を検証し、未知NBTのある既存アイテムをnative Stackへロスあり変換することを拒否する。
- パッチ未対象のregion・chunk・TileEntityのNBT情報は、既存のCOW処理が保持する。
- 新規テスト: 複数region + chest sidecar更新、古いregion snapshotの拒否、8参加者のredo、2 regionとsidecarの途中停止後の再生。

## 非対応・禁止事項

**E46は完了していない。** このブランチのAPIはオプトインの保存基盤であり、ゲームループの全保存経路をAnvilへ変更していない。通常のワールドは引き続きIRWが主保存形式となる。新規region/チャンク生成、全dimension整合、プレイヤーNBT、Mob、全コンテナ、level.dat、未知NBTを含む移行を無条件にサポートするものではない。

**E45/E47も完了していない。** オンラインのChest/Furnace/Hopper/Brewing/Dispenser等の各操作は、全てこのAPIに経由されるわけではない。複数regionのChest保存APIの単体試験PASSを、ゲーム操作全体の原子性やアイテム二重付与対策の完了判定にしない。単独の `rewrite_inventories_under_lock` は引き続きregionをまたぐ変更を拒否する。

**E42は未検証。** `tools/e42_acceptance.py` の9ケース受入レポートは、実際に無改造Minecraft Java 1.7.10で実施・証跡を収集して `--require-pass` を通す必要がある。Rustの単体試験、CLIの動作、あるいはCIを実機PASSの証拠にしない。

## 検証コマンド

```bash
cargo fmt --all -- --check
cargo test -p vanilla-server --all-targets
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
python3 tools/e42_acceptance.py verify --report e42-test/acceptance.json --evidence-root e42-test --require-pass
```

最後のE42コマンドは、レポートと実機証跡が存在する検証環境でのみ実行する。未実行状態なら不合格が正しい。

## 次の実装ゲート

1. 起動・tick・変更通知・停止・再起動をAnvil主保存で実行する専用ランタイムと、IRW→Anvilの**ロスレス/拒否**判定を作る。
2. 新規チャンク・複数dimension・全TileEntity・全Entity/プレイヤー・level.datの保存/復元と、複数region/sidecarにわたるトランザクション統合。
3. 書き込み障害/異常終了/未知タグ保持の障害注入、E42の実クライアント9ケース、E48の1/2/4/8人負荷。
