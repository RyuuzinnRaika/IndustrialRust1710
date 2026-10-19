# Pumpkin → IndustrialRust1710 移植監査（2026-10-19）

## upstream

- リポジトリ: https://github.com/pumpkin-mc/pumpkin
- 参照SHA: `c7a234ab8a3b3296d68a5f40411995920e11528a`（2026-10-18 23:04:39 UTC）
- upstreamのCargo workspace: **Rust 2024 / MSRV 1.96 / GPL-3.0**、複数のクレートとWASM/Bedrock依存。
- 現行Pumpkinは **最新Java/Bedrock版中心**。IndustrialRust1710の **Minecraft Java 1.7.10 / Protocol 5** とワイヤ形式・ブロックID・保存形式が一致しない。

## 詳細レビュー・採用判定

| 要素 | upstreamの位置 | 1.7.10への評価 | このPRでの対応 |
|---|---|---|---|
| Named Binary Tag (NBT) | `crates/pumpkin-nbt` | Java NBTの基本タグは世代共通。NBT本体は移植可能 | **実コードコピー、Cargo独立化、プレイヤー保存に接続** |
| Anvil region | `crates/pumpkin-world/src/chunk/format/anvil.rs` | 4KiB sector等のコンテナは参考になるが、現行chunk NBTと1.7.10のSections/Blocks/Data/Add形式は違う | **未コピー**。1.7.10向け独立変換が必要 |
| Protocol | `crates/pumpkin-protocol` | 最新版のログイン・チャンク・PlayパケットはProtocol 5と互換ではない | **未コピー**。IndustrialRust1710の`legacy-protocol`を維持 |
| Player & Inventory | `crates/pumpkin`, `crates/pumpkin-inventory` | 最新版のスロット/オフハンド/データコンポーネント等は1.7.10にはない | **未コピー**。段階移植対象 |
| Scheduler | `crates/pumpkin-scheduler` | 参考可能だが非同期実行モデルと現行20Hz時計が異なる | **未コピー**。ゲーム更新Tick整備後に再評価 |
| Authentication | `crates/pumpkin-auth` | 現代のMicrosoft/Mojang認証と1.7.10の歴史的プロトコルに差 | **未コピー**。既存の名前ホワイトリストは引き続き**本人確認ではない** |
| Bedrock, data/assets, WASM plugin | その他 | Mod/Forge保留かつ法的/実装上の依存関係が大きい | **未コピー** |

## GPL-3.0 とソースの扱い

Pumpkin本体のソースはGPL-3.0。今回利用したGPL由来コードと統合した実行プログラムの配布には、GPLv3のソース提供・ライセンス通知などが必要。リポジトリの `LICENSE` はPumpkinと同じGNU GPLv3本文。コピー元、元SHA、改変事項は `crates/pumpkin-nbt/UPSTREAM.md` に記録。

第三者のゲーム本体/アセットやModのJAR等はコピーしていない。PumpkinのプラグインAPIの二重ライセンスは、今回選択したNBT crateには適用されない。

## 保存の互換性と移行

- 新しいプレイヤー保存先: `world/players/<name>.dat`。形式は**gzip圧縮Java NBT**。`Pos`, `Motion`, `Rotation`等の基本タグを出力。
- 旧形式: `world/players/<name>.json`。新NBTファイルがない時のみ読み込み、保存時に`.dat`を作る。元JSONは破壊しない。
- NBTというコンテナ形式の採用は、**Vanilla 1.7.10の`player.dat`との完全互換、Anvilとの互換やサバイバル完成を保証しない**。
- 自動テストは、gzip NBTの読み戻し、旧JSON移行、ワールド境界検査、実TCPの接続/再接続を含む。

## 今後のマージ方針

移植元の行数を増やすこと自体は完成指標にしない。Protocol 5へ書き換えた機能単位でビルド、単体、パケット、ワールド破損耐性、実機1.7.10を確認して段階マージする。未確認コードの一括上書きは禁止。
