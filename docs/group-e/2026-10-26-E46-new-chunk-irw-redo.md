# E46: 新規Anvilチャンク生成とIRW世代の同一redo保存（opt-in）

## 実装

- `World::update_many_and_create_missing_anvil_chunk(path, changes)`: フラットOverworld限定。未編集の**同じ未生成チャンク**に対し1～32ブロックの変更を行い、Minecraft Java 1.7.10のAnvil NBTと次のIRWを同一redoトランザクションで公開する。
- `anvil_export::nbt_for_chunk` を内部再利用し、生成されたチャンクの `LightPopulated=0` として**ライト計算は未完了**であることを明示する。
- `legacy_region_rewrite::insert_irw_chunks_under_lock_expected`: `session.lock`、元のIRWバイト列、リージョンの元イメージを照合し、変更前に既存Anvilチャンクの有無をチェックする。既存チャンクが存在すれば上書きは行わず失敗させる。
- 既存の作成専用機構 `Region::create` とcopy-on-write staging、共通redoを使用。旧リージョンにある未知NBT・他のチャンクはそのまま保持する。
- 新規チャンクの元IRWに編集が残っている場合は拒否する。TileEntityなどの別保管データを現在はまとめて移行できないため、フラットで未編集のチャンクに制限する。

## テスト

- 既存リージョン内で欠落したチャンクの追加と周辺チャンク元NBTの保持。
- 存在しないリージョンを新規作成し負座標チャンクを保存。
- 既存チャンクを置き換える要求、重複座標、複数チャンク要求、範囲外座標、不正IDを拒否。
- 古いIRWスナップショットと過去に編集されたIRWチャンクを拒否。失敗時の元IRW・リージョンを不変に保つ。

## 今後必要な実装

このAPIは**明示的な呼び出しのみ**。ライブサーバーは依然IRW主保存であり、Anvilへ自動切替しない。生成済みネイティブコンテナ・Mob・プレイヤーデータ、複数新規チャンクの一括生成、照明計算、ネザー／エンド、強制終了後の再生、E42の無改造Java 1.7.10クライアント9ケース試験がE46完了までに必要。
