# E41 / E48 / E50: Protocol 5固定バイト列・計測集計の統合（2026-10-23）

対象: `fix/e41-e48-protocol5-metrics-v2-20261023`。既存PR #170にある独立テストと集計器を**現在のmainを起点に取り込み**、実サーバーが出力するtelemetry v2との互換性を補正する。

## 今回の差分
- E41: `crates/legacy-protocol/tests/protocol5_goldens.rs` でHandshake、Player List、Window/Slot、Frame分割、Malformed VarIntなどの独立したバイト列回帰試験を追加。
- E48: `metrics-summary` CLIでJSONLを集計。v1およびv2を区別し、v2の`mspt_samples`必須条件、`players_peak`（負荷区分）の確認、TPS・Tick配信率・MSPT P99・RSSの閾値比較を追加。
- E48/E50: `e48_metrics_cli.rs` に、実サーバーのv2と同形の入力で「閾値内は終了0、閾値超過・不正行は非0」のサブプロセス試験を追加。

## 利用例
サーバーを実際に稼働させた際のJSONLを別途収集し、次のように集計する。**CIの合成データでは性能を実測したことにならない**。

```bash
cargo test --workspace --all-targets
cargo run -p vanilla-server --bin metrics-summary -- path/to/metrics.jsonl --min-windows 6 --min-seconds 60 --min-tps 18 --max-p99-mspt 50
```

`--max-rss-mib` 等の閾値も指定できる。異なる負荷条件を評価する場合、**1/2/4/8人で実際に接続・プレイした時間帯を別々に記録し、測定窓の開始・終了やホスト構成も保管すること**。集計器の`by_player_count`はv2の窓内`players_peak`を優先して区分する（窓内で人数が変わった場合は厳密な固定人数測定ではない）。

## 明確に未完了の項目
- E41: Entity / Inventory / Window / Chunk / NBTの**全パケット網羅**ではない。
- E42: **無改造Minecraft Java 1.7.10 GUI**の実機操作は未実施。`docs/group-e/2026-10-21-E42-vanilla-client-e2e.md` の別途証跡が必要。
- E43/E44: 全Entity interest management、負荷高騰下の再現性評価は未完了。
- E45/E46/E47: ジャーナルの全操作横断、`.mca`主保存形式、ライブTileEntity双方向互換は未完了。既存IRW保存を置換しない。
- E48: 1/2/4/8人の長時間実測データは未収集。
- E49: 全管理権限・安全停止の受入確認は別途必要（PR #176）。
- E50: Chaos / Soak / 長時間・実クライアント試験は未実施。

## マージゲート
```bash
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p vanilla-server -p legacy-status-server
```

CI結果が緑になるまではマージ可と判断しない。ブランチが競合した場合は`main`から再度差分を検証する。
