# E48/E50: 1・2・4・8人のProtocol 5通信負荷／プロセス障害の試験基盤

## 目的と受入境界

本ツールは **Rustサーバーの実計測** を行う。ただしクライアントはPython製Protocol 5 TCPシミュレーターであり、**Minecraft Java Edition 1.7.10の無改造GUI実機試験（E42）でも、実際のプレイを再現したベンチマークでもない**。Synthetic workloadを実ユーザー負荷実測と取り違えない。

- `tools/e48_load_benchmark.py` は標準Python3.9+のみを必要とする、明示的実行のベンチマーク。
- `--output` は**存在しない新規ディレクトリ限定**。各人数で別の新規ワールド、whitelist、ループバックポート、ログを用意する。既存ワールドは利用・削除しない。
- 1、2、4、8人それぞれのTCPクライアントをLoginしてC04 Player Positionを約5Hzで送信。サーバーKeepAliveへの応答とClientbound通信受信を継続する。
- ログイン人数が変化した窓は集計から除外し、Rustの`world.metrics.jsonl`（telemetry schema v2）の `players_min == players_peak == 測定対象人数` の実測窓のみを報告。
- TPS処理数／Tick配送率／MSPT P99 worst window／RSS peakと、完全人数の実測秒数を記録する。不正データ・時間不足・検証失敗は失敗として扱う。
- `--inject-crash` は最後の8人フェーズで、**計測後**プロセスを強制終了し、同じワールドを再起動して通常停止・読み取り専用`--verify-world`を実行する。この方式は**OSプロセス異常終了試験であり電源断試験ではない**。

## 実行例

サーバーバイナリをあらかじめビルドする。新規の試験用出力パスを指定する。

```bash
cargo build --release -p vanilla-server
python3 tools/e48_load_benchmark.py \
  --server ./target/release/vanilla-server \
  --output ./e48-load-20261023-run01 \
  --warmup-seconds 12 \
  --seconds 120 \
  --inject-crash
```

Windowsは`--server .\\target\\release\\vanilla-server.exe`、コマンド改行はPowerShellのバッククォートを使う。起動するサーバーは必ずloopback`127.0.0.1`へbindする。性能比較時はOS・CPU・メモリ・Cargoビルド条件・他プロセス負荷・Git SHAを記録すること。

## 証跡

各`load-N`フォルダー:
- `world.irw` および関連保存データ（**本家MinecraftワールドではなくIRW**）
- `world.metrics.jsonl`（Rust TickTelemetryの実行記録）
- `server.log`（クラッシュ／保存診断用）
- `summary.json`（人数固定窓だけの集計）

トップ`manifest.json`: 全人数の結果、試験日時、Python/OS、計測元、および失敗理由（失敗した場合）。実測ウィンドウがない場合は成功扱いしない。既存の`metrics-summary` CLIで元JSONLを追加解析してもよい。

## 限界と残課題

- このツールは主に**初回ログイン・通信・アイドル位置送信の負荷**を計測する。採掘・クラフト・GUI同時編集・大量Mob・ワールド探索の動的チャンク生成は再現しない。E48の**本格1〜8人プレイ負荷**は残る。
- 高負荷時の圧縮キュー深度やTPS悪化の原因分離には別のprofilingと負荷設計が必要（E44）。
- 電源断、リージョン書込み中の故障注入、長期Soak（数時間〜数日）、一貫性試験、メモリリークの評価はまだ実施していない（E45/E50）。
- 無改造Minecraft 1.7.10クライアント操作（E42）は`docs/group-e/2026-10-21-E42-vanilla-client-e2e.md`に従って別途実施する。
- GitHub CIでは標準ライブラリのドライバー単体テストのみを実施する。**実サーバーを指定した本番負荷試験の実測データはまだ取得していない**。

## CI / 単体テスト

```bash
python3 -m unittest discover -s tools -p 'test_*.py' -v
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p vanilla-server -p legacy-status-server
```
