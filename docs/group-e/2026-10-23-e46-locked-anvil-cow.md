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

## 2026-10-23 追補: 同一リージョン内の複数チャンク一括COW

`legacy_region_rewrite::rewrite_chunks_create_only` と
`rewrite_chunks_under_lock` を追加した。従来の単一チャンクAPIは
一括APIに委譲し、既存の利用契約を維持する。

- 1回の呼び出しで更新可能なのは同じ `r.X.Z.mca` に存在するチャンク最大1024件。
- 空集合、重複座標、異なるリージョン、対象の存在しないチャンク、座標不一致を含む不正NBT、シンボリックリンクは更新前に拒否する。
- 変更しないチャンクのNBTバイト列を保持し、全チャンクの読み戻しを検証してから公開する。
- `rewrite_chunks_under_lock` は `session.lock` を公開直前に再検証し、既存リージョンの内容が事前読取から変化していれば置換しない。
- 2チャンク一括更新、未知タグ保持、隣接する未変更チャンク保持、無効バッチ時の元ファイル保護をRustテストで検証する。

**注意:** このAPIはライブのdirty-chunk flushに組み込んでいない。
領域をまたぐ原子的コミット、TileEntity／Entity／プレイヤーの保存、
停電・Windowsロック競合、実機検証は未完了。Anvilを主保存形式に
切り替えるには別の設計・実装・試験が必要である。
