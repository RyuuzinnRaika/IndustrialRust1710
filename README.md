# IndustrialRust1710

**Minecraft Java Edition 1.7.10（Protocol 5）向けRustネイティブサーバーの研究開発プロジェクト。**

> 現状は開発用PoCです。Minecraft内でプレイできるサーバー、Forge 1.7.10のMod JAR実行、Java→Rust変換は未実装です。Status/Pingへの応答は可能です。

## 現時点で動作する機能

| コンポーネント | 機能 | 限界 |
|---|---|---|
| `legacy-protocol` | VarInt、フレーム・文字列の境界チェック | Play/FMLは未対応 |
| `legacy-status-server` | 1.7.10クライアントのサーバー一覧向けStatus/Ping | Loginは明示拒否 |
| `industrial-core` | 決定的Tick、接続グラフキャッシュ、エネルギー配分 | Minecraftゲーム本体とは未接続 |
| `industrial-bench` | 仮想エネルギー網の合成ベンチマーク | 実ワールドのTPSではない |
| `mod-analyzer` | JAR・Java ClassFileの読み取り専用静的解析 | 実行・変換・安全性判定ではない |

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
```

GitHub Actionsではコンパイル・単体テスト・Clippy・実TCPのStatus/Ping/Login拒否テストを実行します。旧Forge 1.7.10の実機クライアントによる接続テストは未実施です。

## 実装ロードマップ

1. Rustでログイン・Play・最小の平坦ワールド表示
2. チャンク・NBT・Anvil保存・ブロック変更と再起動
3. Forge 1.7.10のFMLハンドシェイクとMod IDマッピング
4. Java ClassFile中間表現・限定的なRustへの部分コンパイル
5. 工業化Modを個別に対象選定し、機械・電力・配管の互換実装と実ワールド負荷比較

Mod互換を広く保証することはできません。CoreMod、リフレクション、Javaクラス変換、Forgeライフサイクル等の互換性は検証と実装が必要です。

## ドキュメント

- [要件定義](docs/01_要件定義.md)
- [Mod静的解析PoC](docs/12_Mod静的解析PoC.md)

## ライセンス・権利

Minecraft/Forge/既存ModのJAR、アセット、著作権保護されたコードを無断で再配布しません。リポジトリの新規コードに適用するライセンスは決定前です。
