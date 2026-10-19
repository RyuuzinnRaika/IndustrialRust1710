# IndustrialRust1710

**Minecraft Java Edition 1.7.10（Protocol 5）向けRustネイティブサーバーの研究開発プロジェクト。**

> 現行のソースツリーにはPumpkin由来のGPLv3実装を含みません。現状は開発用PoCです。バニラ1.7.10向けLogin/Playの基礎は追加済みですが、完成したMinecraftサーバーではありません。Mod JAR実行・Java→Rust変換は今は対象外です。

## 新規: Rustネイティブのバニラ最小サーバー

ModとConfig処理は後回しにし、Java Edition 1.7.10 **バニラクライアント専用**でLogin/Play・移動に応じた5×5チャンク配信・Creative設置/破壊・専用形式への保存を実装中です。`vanilla-server` はStatus専用の `legacy-status-server` とは別バイナリです。

~~~bash
cp config/whitelist.example.json whitelist.json
# whitelist.json の name を参加するプレイヤー名に変更
cargo run --release -p vanilla-server -- --bind 127.0.0.1:25565 --world ./world/flat.irw --whitelist ./whitelist.json
~~~

ユーザー名ホワイトリスト方式・最大8人・既存ワールド非互換の**試作**です（本人確認なし）。詳細と未実装範囲: [バニラ最小サーバー](docs/14_バニラ最小サーバー.md)。

## Anvil形式への安全な書き出し（試験版）

ゲーム本体の保存形式は引き続き独自`IRW1`です。Anvil出力は**既存ワールドを上書きしない別のスナップショット**です。

```bash
cargo run --release -p vanilla-server -- --world ./world/flat.irw --export-anvil ./exported-vanilla-world
```

出力先のフォルダーがすでに存在する場合は安全のため失敗します。生成範囲は初期5×5列＋変更のあるチャンク列（上限256）です。これだけでバニラ1.7.10の完全なワールド互換やサバイバル動作が保証されるわけではありません。

### 書き出したスナップショットの読み戻し

独立した変換処理で、**このサーバーが生成したフラットワールドのAnvilスナップショットのみ**を新規IRW1ファイルへ読み戻せます。元の世界や既存の保存先は上書きしません。

```bash
cargo run --release -p vanilla-server -- --import-anvil ./exported-vanilla-world --world ./world/restored.irw
```

他のMinecraftワールドやForgeワールドのAnvilファイルを汎用的に読み込む機能ではありません。エンティティ/タイルエンティティ・未知のブロック/不正なチャンクは安全のため取り込みを拒否します。

## 現時点で動作する機能

| コンポーネント | 機能 | 限界 |
|---|---|---|
| `legacy-protocol` | VarInt、フレーム・文字列の境界チェック | Play/FMLは未対応 |
| `legacy-status-server` | 1.7.10クライアントのサーバー一覧向けStatus/Ping | Loginは明示拒否 |
| `vanilla-server` | ホワイトリスト入場判定、20Hz Tick時計、Login/Play、平坦ワールドの動的チャンク配信、限定Creative操作、ブロック保存とgzip NBTプレイヤー位置保存、独立Anvil書き出し/限定読み戻し、8人までの同期・共有チャット | 本人確認なし、既存ワールド非互換、サバイバルなし |
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

## ライセンスと著作権

**現行のソースツリーでは、PumpkinからコピーしたGPLv3コードを除去済みです。**
NBTの入出力は [fastnbt](https://github.com/owengage/fastnbt)（**MIT OR Apache-2.0**、Cargoで取得する外部依存）と、プロジェクト固有の連携処理を利用します。
利用している第三者ライブラリのライセンスは [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) に記録します。リポジトリの新規オリジナルコード全体のライセンスはまだ確定していません。

重要: **過去のGitコミット/PRにはPumpkin由来GPLv3コードが残っています。** 今回の通常の削除コミットで過去の配布履歴やライセンス義務まで消えたわけではありません。現在のソースとGit履歴を混同しないこと。状況は [ライセンス・移行監査](docs/15_Pumpkin導入監査.md) を参照してください。

Minecraft/Forge/既存ModのJARおよびゲームアセットはコピーしません。
