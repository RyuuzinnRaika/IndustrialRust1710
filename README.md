# IndustrialRust1710

**細分化した実装仕様（全380条件）:** [詳細仕様 I：通信・地形・物理](docs/02_詳細要件_通信・地形・物理_20261028.md) / [詳細仕様 II：Mob・サバイバル・保存](docs/03_詳細要件_Mob・生存・保存_20261028.md) / [1.7.10仕様差分・95項目トレーサビリティ・24実機試験](docs/04_1.7.10仕様マトリクス・受入試験台帳_20261028.md)。本家1.7.10の履歴を基準に入力・正常処理・拒否時動作・試験を定義。Rust CIは起動せず、実機未試験はNOT_RUNと記録。

> **最新の詳細要件定義:** [Minecraft 1.7.10 要件95項目・実装状況台帳](docs/01_要件定義_詳細・実装状況_20261028.md)（2026-10-28）。各機能の実装内容・部分実装・未検証・Wiki版差分・受入手順を整理しています。コードの存在と実機互換完了は別判定です。

**Minecraft Java Edition 1.7.10 / Protocol 5向けの、Rustネイティブサーバー開発プロジェクト**です。

> **開発途中です。** 現在のRust実装と「無改造のMinecraft 1.7.10で完全に遊べること」は同義ではありません。機能ごとにコード・CI・実クライアント検証を分けて確認してください。Forge/FML Modの実行・互換機能は現時点の主対象ではありません。

## 開発の優先順位・やることリスト

**[統合TODO（Issue #660）](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/660)** に、直近の修正順、各グループの担当、次の1PR単位、完了条件を集約しています。毎時の割当・PR統合履歴は [Issue #347](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/347)、最終のバニラサバイバル完成条件は [Issue #20](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/20) を参照してください。古い監査の未完了項目は、必ず最新のmain・PR・実機テスト証跡と照合します。

## プロジェクト共通ポリシー

開発者とAIエージェントは、[プロジェクトポリシー](docs/PROJECT_POLICIES.md)に従ってください。参加・PR手順は[CONTRIBUTING.md](CONTRIBUTING.md)、脆弱性の非公開報告方法は[SECURITY.md](SECURITY.md)、実装言語と既存の移行例外は[AGENTS.md](AGENTS.md)で確認できます。CI通過と無改造Minecraft 1.7.10実機検証は区別して報告してください。

## 開発言語の方針（必須）

**サーバー本体・ゲームロジック・通信・保存・管理GUI・本番運用機能の実装は Rust のみ**です。Python は独立した検証・テスト・負荷試験・回帰確認用のプログラムに限ります。新しいPython製の本体機能や管理・監督処理は追加しません。シェル/PowerShellは必要最小限の環境準備・起動に限定します。

既存のPython製メンテナンス監督・GitHubブランチ整理ツール、C#製Windows管理GUIは、機能を壊さずRustへ移行する既存課題です（[追跡Issue #635](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/635)）。現時点では**すべての周辺機能がRust化済みという意味ではありません**。開発者・自動実装担当向けの詳細ルールは [AGENTS.md](AGENTS.md) を参照してください。

**図解で全体を把握する:** [アーキテクチャ・通信・Tick・保存・移行・CI（Mermaid 6テーマ）](docs/00_プロジェクト図解.md)

## まず全体を知る

```mermaid
flowchart LR
    Client["Minecraft Java 1.7.10<br/>バニラクライアント"]
    Server["vanilla-server<br/>Rustゲーム本体"]
    Proto["legacy-protocol<br/>Protocol 5"]
    Game["World / Mob / Inventory"]
    Save[("独自IRW・関連データ")]
    Status["legacy-status-server<br/>Status / Ping専用"]
    Analyzer["mod-analyzer<br/>Mod JAR静的解析"]
    Bench["industrial-bench<br/>合成計測"]
    Core["industrial-core<br/>計算核"]
    Client -->|"Login / Play"| Server
    Client -.->|"一覧表示"| Status
    Server --> Proto
    Status --> Proto
    Server --> Game --> Save
    Bench --> Core
    Analyzer -.->|"実行しない"| Jar["Mod JAR"]
```

**大切な区別:** `legacy-status-server` はゲーム参加用のサーバーではなく、`mod-analyzer` はMod実行環境ではありません。また、Anvil/NBTの読み書きが部分的に存在しても、既存バニラワールドの完全な移行が保証されたわけではありません。

## 使い方

Rustのstableツールチェーンを用意します。次の例は開発・検証向けで、公開運用の設定ではありません。

**Windows (PowerShell)**

```powershell
git clone https://github.com/RyuuzinnRaika/IndustrialRust1710.git
cd IndustrialRust1710
Copy-Item config/whitelist.example.json whitelist.json
notepad whitelist.json
cargo run --release -p vanilla-server -- --bind 127.0.0.1:25565 --whitelist ./whitelist.json
```

**Linux / macOS**

```bash
git clone https://github.com/RyuuzinnRaika/IndustrialRust1710.git
cd IndustrialRust1710
cp config/whitelist.example.json whitelist.json
# whitelist.json に参加するユーザー名を設定
cargo run --release -p vanilla-server -- --bind 127.0.0.1:25565 --whitelist ./whitelist.json
```

対応クライアントは **Minecraft Java Edition 1.7.10**。既定はローカルループバックを前提とした開発構成です。ユーザー名ホワイトリスト単独では本人確認になりません。`online-mode` のコードが存在しても、設定・受け入れ試験を確認するまではインターネットへ公開しないでください。

## 主なコンポーネント

| パッケージ | 役割 | 留意点 |
| --- | --- | --- |
| `apps/vanilla-server` | Protocol 5のLogin/Play、ゲームロジック、セッション、保存 | Rustネイティブの開発中サーバー |
| `crates/legacy-protocol` | Protocol 5のフレーム・エンコード基盤 | 全Minecraft機能をこのcrateだけで実装するわけではない |
| `apps/legacy-status-server` | Status / Ping | Login/Playは`vanilla-server` |
| `crates/mod-analyzer` | Mod JAR・Modpackの静的解析 | Forge/Modは実行しない |
| `crates/industrial-core` | Tick・工業ネットワーク計算の核 | ゲーム本体とは別の研究基盤 |
| `apps/industrial-bench` | 合成負荷のベンチマーク | 実サーバーのTPS測定ではない |

## バイオーム付き地形をWindowsで起動（A01）

`run-windows.ps1` を**引数なし**で実行すると、Minecraft Java 1.7.10向けのIRW4地形生成器（固定シード42）が有効になり、平原・森林・海洋・川・起伏・洞窟・鉱石・村などを生成する**別のワールド** `world/biomes-42.irw` を作成／再読込します。最初はサバイバルモードです。IRW4の地形生成は独自実装であり、本家1.7.10と完全一致するわけではありません。

```powershell
.\run-windows.ps1
```

### IRW4の樹木補完・サバイバル採掘（2026-10-27）

草原（Plains）には低密度のオーク、サバンナ（Savanna）にはアカシアを追加します。旧来の森林・タイガ・ジャングル等の木の生成ロジックを優先し、スポーン中心の安全な空き地は維持します。**既存IRW4ワールドを削除・再生成する処理はありません。**

注意：IRW4は未編集ブロックを座標・シードから都度生成する方式です。従ってアップデート後は**過去に訪問済みでも未編集の草原/サバンナに木が現れ得ます**。保存済みのブロック編集は優先されますが、建築物の近くに新しい葉が現れる可能性があるため、**アップデート前にサーバーを停止し、ワールド保存先と関連するプレイヤー/アイテムファイルをまとめてバックアップ**してください。自然生成との完全な後方互換は保証しません。建築物を完全に固定したい場合は、バックアップした既存ワールドを元に戻すか、別名の新規ワールドで確認してください。

サバイバル採掘は、即時破壊可能な草/花などで同一ティックの開始・終了を許容し、サーバー処理タイミングの差として最大1ティックの猶予を設けています。未対応だった一部の自然ブロックを補完しました。掘ったはずのブロックが戻る場合は、PowerShellで診断モードを付けて起動し、`mining trace:` の出力を確認してください。

```powershell
$env:IR_TRACE_MINING = "1"
$env:IR_TRACE_MOVEMENT = "1"
.\run-windows.ps1
```

実行テスト・Rust CI・GitHub Actionsはこの修正時点では起動していません。ローカル環境で `cargo fmt --all -- --check`、`cargo test --workspace --all-targets`、`cargo clippy --workspace --all-targets -- -D warnings` と Minecraft Java 1.7.10 実機検証を実施してください。

### Windows版のサーバー管理GUI（Minecraft 1.7.10風）

Windows の通常の対話型ターミナルから `.\run-windows.ps1` を起動すると、ビルド確認後に本家サーバーの管理ウィンドウに似た画面を表示します。**Stats** はサーバープロセスのメモリ使用量（ワーキングセット）の推移、CPU使用率、スレッド数を、**Players** はログイン・ログアウトの通知から作ったオンライン一覧を、**Log and chat** はサーバー出力とコマンド入力欄を表示します。平均tick時間はサーバー側に測定値を公開するAPIがまだないため、`not available` と表示します。グラフは JVM heap ではなく OS のプロセスメモリです。

コマンド欄には `list`、`save`、`stop` などを入力できます。GUIから送信すると Rust サーバー側で必要な `/` を自動付与します。「Stop」ボタンまたはウィンドウを閉じる操作は `/stop` を送信し、ワールド保存とプレイヤー切断を待ちます。画面はローカルに表示されるだけで、外部公開する管理ポートは追加しません。

従来どおり**コンソールだけで起動**したい場合、または自動化・ログ収集・SSH等の場合は次を使用してください（`--nogui` も指定可能です）。

```powershell
.\run-windows.ps1 -NoGui
```

出力のリダイレクト時や `--verify-world` などのメンテナンスモードは自動的にGUIを起動しません。Windows PowerShell 5.1 のSTA環境を想定しています。実機のGUI動作はWindowsで確認してください。

従来の `world/flat.irw` は**自動変換・削除されません**。既存データを続けて利用する場合は、明示的に従来の保存先を指定してください。

```powershell
.\run-windows.ps1 --world world/flat.irw
```

シードや保存先を指定した別ワールドを作る場合は明示的に指定できます（既存保存先を上書きしません）。

```powershell
.\run-windows.ps1 --survival --expanded-seed 12345 --world world/biomes-12345.irw
```

`--expanded-seed` の初回起動は新しい保存先を使ってください。既存IRW1の平坦ワールドは、起動フラグを変えるだけで地形が切り替わることはありません。既存ワールドを必ずバックアップしてから試してください。起動・CIのテスト結果と無改造クライアントでの目視検証は別々に確認してください。

### 移動中の切断・MINECART CHECKPOINT ERROR（Windows）

`MINECART CHECKPOINT ERROR: expected 2 to 4 transaction participants` は、**旧版のバイナリ／ソース**でトロッコの定期保存（1ファイル）を古いジャーナル制限（2〜4ファイル）に通した時のエラーです。最新版の `journal.rs` は **1〜8ファイル**に対応しています。サバイバルモードやC04/C06移動の問題と混同せず、まず実行ソースを確認してください。

**サーバーを正常に停止**し、リポジトリのルートの PowerShell で実行します。

```powershell
git switch main
git pull --ff-only origin main
git rev-parse --short HEAD
Select-String -Path .\apps\vanilla-server\src\journal.rs -Pattern 'expected 1 to 8 transaction participants'
cargo clean -p vanilla-server
.\run-windows.ps1 -NoGui
```

`Select-String` に `expected 1 to 8 transaction participants` が出なければ、実行しているディレクトリが違うか、Git の更新に失敗しています。エラーが残る場合は `git rev-parse HEAD` と最初のエラーメッセージ全文を記録してください。**ワールド、`.minecarts.json`、`.pending-txn.json` を削除しないでください**。未完了のジャーナルはサーバー起動時にリプレイする設計です。

最新版には、実サーバーを起動し、20ティック後のトロッコ保存ファイル作成と、その後のサバイバル移動・再接続を確認する TCP 回帰テストが含まれます（`native_1710_minecart_checkpoint_keeps_survival_session_and_movement_alive`）。それでも切断される場合は、無改造1.7.10実機のサーバーログとクライアントログの両方で原因を切り分けます。

## 複数ディメンションの起動（A10・試験機能）

IRW4の3ディメンションを1つのワールドルートにまとめて管理する場合は、`--expanded-seed` と `--dimension-root` を併用します。

```bash
cargo run -p vanilla-server -- --survival --expanded-seed 42 --dimension-root ./worlds/demo --dimension overworld
```

- `./worlds/demo/overworld.irw`、`DIM-1/nether.irw`、`DIM1/end.irw` を同じシードで事前検査し、未作成の次元のみ生成します。
- 3つのワールドは独立した共有ハンドルで維持され、ルートの `session.lock` が各次元の書き込みを保護します。停止時には全次元のIRW4スナップショットをredo journal経由でまとめて保存します。
- `--dimension` で指定した1つの次元だけが、現段階のネットワークゲームセッションとTickの対象です。**プレイヤーがポータルに入って次元を切り替える処理は、まだ動作しません。** 同じワールドルートで複数プロセスを同時起動することもできません。
- `--dimension-root` は従来の `--world` とは併用不可です。既存のIRW1～IRW3やバニラのAnvilワールドには使用せず、開発用の新しいディレクトリに設定してください。

## 保存形式と開発上の安全性

- ライブゲームには独自の`IRW1〜IRW4`系ワールド保存と、プレイヤー・各装置等の個別データファイルを使います。
- 対応する複数ファイル更新にredo journalがありますが、**全保存経路の完全な原子性を意味しません**。
- Anvil / NBTとの相互運用・本家形式への切替は段階的な作業です。**元のバニラ/Forgeワールドを直接上書きしない**でください。
- 機能の実装と、無改造1.7.10クライアントによる実証試験は、別の合格条件です。

## ビルドと検証

```bash
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p vanilla-server -p legacy-status-server
```

**2026-10-27以降、GitHub ActionsのRust CIはpush/PRで自動実行しません。** 今後AI担当からRust CIを起動・再実行することもありません。上記cargoコマンドは開発PC等のローカル検証用です。未実施の検証を成功扱いせず、バニラ1.7.10実機検証とも区別します。停止済Rust CIがGitHub保護ルールの必須チェックに残っている場合、管理者が設定変更するまではマージ保留です。詳細は[ポリシー](docs/POLICY_RULES.md)。

## ドキュメント案内

| 調べたいこと | 読む資料 |
| --- | --- |
| 全体の構成・処理のつながり | **[Mermaid図解ガイド](docs/00_プロジェクト図解.md)** |
| **開発・AI・PR・データ保護の詳細ポリシー（38項目）** | **[詳細ポリシー規範](docs/POLICY_RULES.md)** |
| 目標とスコープ変更 | [要件定義](docs/01_要件定義.md) |
| バニラサーバーの詳細・段階実装 | [バニラ最小サーバー](docs/14_バニラ最小サーバー.md) |
| 保存形式を本家構造へ近づける順序 | [1.7.10セーブ互換と移行実装順](docs/17_1.7.10セーブ互換と移行実装順.md) |
| A〜Eの開発分担と未完了項目 | [50項目監査](docs/24_50項目実装監査と5人分担_20261021.md)、[100件バックログ](docs/25_修正すべき点100件_5人分担_20261021.md) |
| 停止・バックアップ・品質 | [J91〜J100](docs/J91-J100_運用品質実装状況.md) |
| **実行時の並列化・性能最適化** | **[実施した3件と追加8候補の監査](docs/29_並列最適化監査_20261023.md)** |
| **AI並列開発向け200項目の実装計画** | **[10グループ×20項目・担当ファイル・完了条件](docs/28_AI並列実装200項目_20261023.md)** |
| 詳細な変更履歴 | [CHANGELOG](CHANGELOG.md)、[従来READMEの記録（アーカイブ）](README_HISTORY_2026-10-20.md) |

> 古い設計書・監査表には、作成当時の「未実装」「PR待ち」の判定がそのまま残っています。**現在の状況はmain上の実コード、GitHub PR、CIおよび実機検証を優先**してください。

## ライセンスと注意

第三者ライブラリは [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) を参照してください。現在のソースツリーと過去のGit履歴のライセンス上の扱いは同一ではありません。歴史的なPumpkin由来コードの取込みについては [監査資料](docs/15_Pumpkin導入監査.md) に記録されています。Minecraft本体・Mod配布物・アセットはリポジトリへ追加しないでください。
