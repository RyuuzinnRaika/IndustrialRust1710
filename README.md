# IndustrialRust1710

**Minecraft Java Edition 1.7.10（Protocol 5）向けRustネイティブサーバーの研究開発プロジェクト。**

> **GPLv3 / Pumpkin由来コードを一部含む**。現状は開発用PoCです。バニラ1.7.10向けLogin/Playの基礎は追加済みですが、完成したMinecraftサーバーではありません。Mod JAR実行・Java→Rust変換は今は対象外です。

## 新規: Rustネイティブのバニラ最小サーバー

ModとConfig処理は後回しにし、Java Edition 1.7.10 **バニラクライアント専用**でLogin/Play・小規模平坦チャンク・Creative設置/破壊・専用形式への保存を実装中です。`vanilla-server` はStatus専用の `legacy-status-server` とは別バイナリです。

~~~bash
cp config/whitelist.example.json whitelist.json
# whitelist.json の name を参加するプレイヤー名に変更
cargo run --release -p vanilla-server -- --bind 127.0.0.1:25565 --world ./world/flat.irw --whitelist ./whitelist.json
~~~

ユーザー名ホワイトリスト方式・1人のみ・既存ワールド非互換の**試作**です（本人確認なし）。詳細と未実装範囲: [バニラ最小サーバー](docs/14_バニラ最小サーバー.md)。

## 現時点で動作する機能

| コンポーネント | 機能 | 限界 |
|---|---|---|
| `legacy-protocol` | VarInt、フレーム・文字列の境界チェック | Play/FMLは未対応 |
| `legacy-status-server` | 1.7.10クライアントのサーバー一覧向けStatus/Ping | Loginは明示拒否 |
| `vanilla-server` | ホワイトリスト入場判定、20Hz Tick時計、Login/Play、固定平坦ワールド、限定Creative操作、ブロック保存とgzip NBTプレイヤー位置保存 | 1人/ユーザー名偽装可能、既存ワールド非互換、サバイバルなし |
| `industrial-core` | 決定的Tick、接続グラフキャッシュ、エネルギー配分 | Minecraftゲーム本体とは未接続 |
| `industrial-bench` | 仮想エネルギー網の合成ベンチマーク | 実ワールドのTPSではない |
| `mod-analyzer` | JAR・ClassFile解析、Modpack ZIP/フォルダのJSON監査 | Modの実行・変換・互換性判定ではない |

## 準備と検証

Rustのstableツールチェーンが必要です。

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings

# Status専用サーバー。localhost:25565に待ち受けます。
cargo run -p legacy-status-server -- --bind 127.0.0.1:25565

# 別のターミナルから実行
python3 tests/protocol5_smoke.py 127.0.0.1 25565

# 仮想工場ネットワークの合成ベンチマーク
cargo run --release -p industrial-bench -- 10000 200

# Modを実行せずにJAR内部を読み取る
cargo run -p mod-analyzer -- /path/to/mod.jar

# 1.7.10サーバーのZIPを展開せずに読み取り専用監査
cargo run -p mod-analyzer -- --modpack /path/to/1.7.10modServerSkyBlock.zip --json modpack-report.json

# 展開済みのサーバーディレクトリ (mods/ と mods/1.7.10/ を探す)
cargo run -p mod-analyzer -- --modpack /path/to/server --json modpack-report.json
```

GitHub Actionsではコンパイル・単体テスト・Clippy・実TCPのStatus/Ping/Login拒否テストを実行します。旧Forge 1.7.10の実機クライアントによる接続テストは未実施です。

解析レポートにはMod ID（mcmod.info記載分）、CoreModのmanifest設定、参考となるクラス数、Forge/FML参照と読み取りエラーを保存します。**@ModアノテーションからのID・依存解決、Mod実行、JavaクラスのRust変換、Forge接続の実動作は未対応**です。ユーザー所有のMod/ワールドのJAR等をGitに追加しないでください。

## 実装ロードマップ

1. 1.7.10 Vanillaのログイン・ホワイトリスト・Play通信を安定化（進行中）
2. 20TPSワールドロジック、チャンク生成・保存、NBT/Anvilを実装
3. 複数プレイヤー・インベントリ・クラフト・サバイバル・Mob・物理・レッドストーンを順次実装
4. 1.7.10バニラクライアントの実機総合テストと耐久テスト
5. **Mod/Forge/Config、Java→Rustの変換はユーザー指定により保留**

Mod互換を広く保証することはできません。CoreMod、リフレクション、Javaクラス変換、Forgeライフサイクル等の互換性は検証と実装が必要です。

## ドキュメント

- [要件定義](docs/01_要件定義.md)
- [Mod静的解析PoC](docs/12_Mod静的解析PoC.md)
- [提供Modpack初期監査](docs/13_提供Modpack初期監査.md)

## ライセンス・権利

Pumpkin由来のNBT実装を取り込んだため、統合して配布する本プロジェクトは **GPL-3.0-only** に従います（[LICENSE](LICENSE)）。元のPumpkin作者・貢献者の権利は引き続き保持されます。参照・コピーしたファイルと元コミットは [UPSTREAM.md](crates/pumpkin-nbt/UPSTREAM.md) に記録しています。第三者のライセンスや著作権表示は削除しません。

Minecraft/Forge/既存ModのJAR、ゲームアセットなどを無断で再配布しません。PumpkinのBedrock/ゲームアセット等は取り込んでいません。

実装評価とバージョン差分は [Pumpkin採用監査](docs/15_Pumpkin導入監査.md) を参照してください。
