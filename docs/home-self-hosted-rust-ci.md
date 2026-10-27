# 自宅サーバPCで IndustrialRust1710 の Rust CI を実行する

## 現在の段階

追加ワークフロー: [Rust CI (home Linux, trusted manual)](../.github/workflows/home-rust-ci.yml)

- これは**手動実行専用**。従来の `ci.yml`（GitHub-hosted）には手を加えない。
- GitHub Actions → **Rust CI (home Linux, trusted manual)** → **Run workflow** → `main` から開始する。
- 実行者 `RyuuzinnRaika` / `main` のみ許可。外部のPRコードはこの常駐Runnerで実行しない。
- 実行先ラベル: `[self-hosted, Linux, X64, industrialrust-ci]`。
- `cargo fmt`、workspace tests、`cargo clippy -D warnings`、release build、SHA256記録を実行。
- Cargoの **ビルド成果物は毎回削除**。Rustキャッシュアクションは使用しない。
- Workディレクトリ・一時ファイル・Rustツールチェーン用HOMEを、NAS/HDD (`/mnt/nas_storage`) に配置する。SSDやRAMディスクは使わない。
- 必要なHDD容量とCPU時間はその分増加する。初回の依存ダウンロードも行う。

**重要**: 新しいRunnerが `Online` になるまでは、このワークフローの実行は `Queued` のままです。追加しただけで自宅PC上での実行が始まるわけではありません。

## セキュリティ・運用上の必須条件

公開リポジトリに家庭内の常駐self-hosted runnerを無条件で接続することは危険。外部PRから実行可能な `pull_request` / `pull_request_target` をこのワークフローに追加しない。

**推奨: 自宅サーバ上の独立したLinux VM内に専用Runnerを作る。** ホストのSSH鍵・NAS資格情報・ソースコード用トークン・個人データ・Minecraft本番ワールドをVMに共有しない。VMから家庭内LANへの通信を制限し、必要な外向きHTTPSのみ許可する。ホスト本体に直接Runnerを設置するのは推奨しない。

Runnerにはroot権限、sudo権限、Dockerソケット、ホストファイルシステムの広範な書込み権限、リポジトリへの書込みトークンを付与しない。ビルドはVM内の非特権アカウントで実施する。GitHub側の `permissions: contents: read` と `persist-credentials: false` は防御の一部にすぎない。

## 1. Linux x64専用Runnerの登録

*以下の例はVM内で `/mnt/nas_storage` が独立した作業用HDDとしてマウントされている場合。メインサーバのNASデータをVMへそのまま共有してはいけない。*

Ubuntu VM内で、専用ユーザーと作業先を作成:

```bash
sudo useradd -m -d /mnt/nas_storage/build-server/industrialrust1710-home -s /bin/bash ir-ci
sudo install -d -o ir-ci -g ir-ci -m 0700 /mnt/nas_storage/build-server/industrialrust1710-home/runner
sudo -iu ir-ci
cd /mnt/nas_storage/build-server/industrialrust1710-home/runner
```

このユーザーでRust `stable` をインストールする（導入済みなら省略可）。Rustと依存キャッシュも `HOME` のHDDに置く:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
. "$HOME/.cargo/env"
rustup component add rustfmt clippy
rustc --version
cargo --version
```

GitHubの [IndustrialRust1710 → Settings → Actions → Runners](https://github.com/RyuuzinnRaika/IndustrialRust1710/settings/actions/runners) → **New self-hosted runner** → **Linux / x64** を開く。表示された公式ダウンロードコマンドとSHA256検証コマンドを **`ir-ci` ユーザー** で実行する（RunnerのバージョンはGitHubが示す最新版を使う）。

公式の `./config.sh --url ... --token ...` コマンドには、次の設定も付ける:

```bash
./config.sh --url https://github.com/RyuuzinnRaika/IndustrialRust1710 \
  --token '<GitHubの画面が発行する短命の登録トークン>' \
  --name industrialrust-home-01 \
  --labels industrialrust-ci \
  --work _work
```

> 上記トークンは説明用のプレースホルダーです。実際のトークンはここに投稿したり、Gitに保存したりしない。実行する `config.sh` は公式手順に従って展開したRunnerのものを使う。

ランナー登録後、`ir-ci` から抜け、root権限のある管理者として **そのRunnerディレクトリから** サービスを登録:

```bash
exit
cd /mnt/nas_storage/build-server/industrialrust1710-home/runner
sudo ./svc.sh install ir-ci
sudo ./svc.sh start
sudo ./svc.sh status
```

Linux Runnerのユーザーサービスは自宅サーバの他プロジェクト（InkBridge、DeskBridgeXR）のRunnerと**別インストール・別サービス**にする。既存Runnerの設定を上書きしない。

## 2. 自宅マシンでの実行確認

1. GitHubのRunner一覧で `industrialrust-home-01` が **Online**、ラベル `industrialrust-ci`、`Linux`、`X64` を持つことを確認。
2. ワークフローを `main` で手動実行。
3. Job詳細の **Set up job → Runner name** が `industrialrust-home-01` であることを確認。
4. Rustの書式・テスト・lint・ビルドが成功したことを確認。
5. GitHub ActionsのArtifactからSHA256/BUILD-MANIFESTを取得して、ビルド場所・コミット・コンパイラ版を記録。

`Queued` のままなら、RunnerのOnline状態、専用ラベル、所属リポジトリを確認。`Verify dedicated NAS/HDD environment` で失敗した場合、`RUNNER_TEMP` または `HOME` が `/mnt/nas_storage` ではない。ジョブを別ディスクへ逃がすのではなく、Runner側の配置を修正する。

## 3. 定常運用へ移行する条件

上記の専用Runnerが実際にCI全件を通過した後、`ci.yml` の `push: main` など**信頼されたイベントに限って**self-hostedへ段階移行する。公開PRからの任意コードを自宅サーバ上で自動実行しない。一般の `pull_request` 検証はGitHub-hostedか専用の破棄型VM等、隔離された環境を使用する。

標準のWindows GUI/Windows向けバイナリCIはLinuxではそのまま実行できない。必要ならWindows専用Runnerを別途構成し、再現性・リリース・ブランチ削除の各workflowも用途別に検証してから切り替える。特にブランチ削除のような `contents: write` のジョブは自宅Runnerへの安易な移行対象にしない。
