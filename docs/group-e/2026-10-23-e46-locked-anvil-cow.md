# E46 / FS08: session.lock下のAnvilリージョンcopy-on-write更新

この変更は**既存.mcaリージョン1件に含まれる既存チャンク1件**を、安全な独立 API で更新する中間実装です。**IRWベースのメインワールド保存経路をAnvilへ切り替えるものではありません**。

## 安全条件
1. `WorldSessionLock::acquire(world_dir)` で、対象ワールドを明示してセッションロックを取得する。
2. Dimension(Overworld/Nether/End)とチャンク座標から対象の`region/r.X.Z.mca`を内部導出し、任意の外部パスを受け付けない。
3. `legacy_region::inspect_region` / `legacy_nbt::parse_chunk`で元のリージョンと更新NBTを検証する。
4. 同じリージョンの別の新規ステージディレクトリに既存全チャンクを書き出し、復元して照合する。触っていないチャンクはuncompressed NBTバイト列を保持する。
5. 保存直前にsession.lockを再検証し、元リージョンがバイト単位で変更されていないことを確認した上で、同一ファイルシステム内のrenameで置換する。失敗したときには元ファイルを維持する。
6. シンボリックリンクのリージョンファイルや、不正NBT、破れたセッションロックを拒否する。

## 制約と将来工程
- オプトインの内部`rewrite_chunk_under_lock`のみ。オンラインワールドのdirty-chunk flush、プレイヤー・コンテナ・Mob・Dimensionのマルチファイル永続化、ジャーナルとの統合は未実装。
- 他プロセスがRustのadvisory lockを無視して同時書き込みした場合、原子的rename単体では競合を完全には防げない。外部Javaサーバーとの同時起動は禁止。
- 電源遮断試験、Windowsファイルロック、複数チャンクの更新、全未知NBTタグの互換性と本家1.7.10のGUI実機検証を経るまでは、.mcaのライブ主保存形式は有効化しない。

## 受入テスト
```bash
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p vanilla-server -p legacy-status-server
```

テストでは既存リージョンの更新、未変更チャンク・未知NBTの保持、不正NBT拒否、失効したsession.lock拒否、シンボリックリンク拒否を検証する。長時間の実データ試験は未実施。
