# IndustrialRust1710: GitHub Actionsを使わない自宅サーバRust検証

**方針:** GitHub Actions、GitHub self-hosted runner、ワークフロートークンは使わず、Ubuntu PCの `git` / `cargo` / `python3` だけでソースを検査する。既存のPR #653のActions方式は使用しない。

- 実装: [scripts/local-rust-ci.sh](../scripts/local-rust-ci.sh)
- 定期実行: [systemd service](systemd/industrialrust-local-ci.service) と [timer](systemd/industrialrust-local-ci.timer)
- `main` だけを **HTTPS匿名読み取り** で取得。PRブランチは実行しない。自動承認・自動マージ・GitHubへの書込みは一切行わない。
- 検証: `cargo fmt --all -- --check`, `cargo test --workspace --all-targets`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo build --release -p vanilla-server -p legacy-status-server`, Python検証用unittest。
- 毎回新しく `main` を浅くcloneし、対象コミットSHAを固定して検査する。Cargoビルドキャッシュのみ専用領域に保持。
- 結果: `reports/latest.txt` に `PASS` / `FAIL`、コミットSHA、ログパスを保存。成功したSHAだけ `last-success-sha` に記録。
- `--once`: 必ず検証。`--if-changed`: 前回成功した `main` SHAと同じならスキップ。失敗時は次回再試行。重複ジョブは `flock` で防止。
- 既存のGitHub Actions定義は変更しない。**この方式の実行・判断ではActionsの状態を使用しない**。リポジトリの既存ワークフローが他のイベントで動くこと自体を停止する手順ではない。

## 0. セキュリティ

本リポジトリは公開されている。自宅ネットワークの管理者権限・SSH鍵・本番Minecraftワールドがある環境で、外部から取得したコードをそのままテスト実行するのは危険。

推奨構成は、**自宅Ubuntuサーバ上の独立した非特権Linux VM**。VMの外部とは別のビルド用仮想HDDを用意し、VM内で `/mnt/nas_storage` にマウントする。ホストの本物のNAS・秘密鍵・Dockerソケット・サーバ管理ポートをVMに共有しない。インターネットへの読み取りは必要だが、家庭内LANへの到達はネットワークで制限する。

以下のサンプルはVM内部でのみ使用する想定。`/mnt/nas_storage` が単なる共用NAS上の本番ディレクトリなら、この配置を使わず独立ディスクを準備する。CIユーザーはsudo権限・GitHub書込みトークンを持たない。検査結果の `PASS` は実Minecraft Java 1.7.10クライアントの動作保証を意味しない。

## 1. 準備（Ubuntu Linux x64）

Windows PCからホームサーバに接続する場合:

```powershell
ssh ryuuzinnserver
```

検証に必要なコマンドを準備:

```bash
sudo apt-get update
sudo apt-get install -y git curl ca-certificates python3 build-essential pkg-config libssl-dev util-linux coreutils
```

VM内で専用アカウントを作る（すでにある場合は再作成しない）:

```bash
sudo useradd --home-dir /mnt/nas_storage/build-server/industrialrust1710-local-ci --shell /bin/bash --no-create-home ir-ci
sudo install -d -o ir-ci -g ir-ci -m 0700 /mnt/nas_storage/build-server/industrialrust1710-local-ci
sudo install -d -o ir-ci -g ir-ci -m 0700 /mnt/nas_storage/build-server/industrialrust1710-local-ci/bin
sudo install -d -o ir-ci -g ir-ci -m 0700 /mnt/nas_storage/build-server/industrialrust1710-local-ci/state
```

Rust `rust-toolchain.toml` が指定する **1.90.0** に合わせて、専用ユーザーのHOMEにRustupを導入する:

```bash
sudo -u ir-ci -H bash -c 'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.90.0'
sudo -u ir-ci -H bash -c '"$HOME/.cargo/bin/rustup" component add rustfmt clippy --toolchain 1.90.0'
```

実行スクリプトは、このブランチを展開したGit checkoutから **安定した専用パスへコピー**する:

```bash
git clone --depth 1 --branch ci/local-rust-verification-no-actions-20261027 https://github.com/RyuuzinnRaika/IndustrialRust1710.git /tmp/industrialrust-ci-setup
sudo install -o ir-ci -g ir-ci -m 0750 /tmp/industrialrust-ci-setup/scripts/local-rust-ci.sh /mnt/nas_storage/build-server/industrialrust1710-local-ci/bin/local-rust-ci.sh
```

## 2. Actionsなしで今すぐ1回検査

```bash
sudo -u ir-ci -H env IR_CI_ROOT=/mnt/nas_storage/build-server/industrialrust1710-local-ci/state \
  bash /mnt/nas_storage/build-server/industrialrust1710-local-ci/bin/local-rust-ci.sh --once
```

結果確認:

```bash
sudo cat /mnt/nas_storage/build-server/industrialrust1710-local-ci/state/reports/latest.txt
```

ログは `state/reports/YYYYMMDDTHHMMSSZ-PID.log` に残る。途中で失敗すれば非ゼロ終了して `FAIL`。成功時のみ `last-success-sha` 更新。外部リポジトリに検証結果を送信しない。

## 3. GitHub Actionsを使わず毎時実行

上の配置を確認した後、systemdのoneshot+timerを入れる（常駐する独自デーモンは不要）:

```bash
sudo install -m 0644 /tmp/industrialrust-ci-setup/docs/systemd/industrialrust-local-ci.service /etc/systemd/system/industrialrust-local-ci.service
sudo install -m 0644 /tmp/industrialrust-ci-setup/docs/systemd/industrialrust-local-ci.timer /etc/systemd/system/industrialrust-local-ci.timer
sudo systemctl daemon-reload
sudo systemctl enable --now industrialrust-local-ci.timer
sudo systemctl start industrialrust-local-ci.service
sudo systemctl status industrialrust-local-ci.service --no-pager
sudo systemctl list-timers industrialrust-local-ci.timer
```

`OnCalendar=hourly`、ランダム遅延最大5分、前回失敗したコミットも再検証する。システムサービスの `User=ir-ci` で実行するので、sudoやrootでCargoをビルドしない。

ログ/操作:

```bash
sudo journalctl -u industrialrust-local-ci.service -n 80 --no-pager
sudo systemctl start industrialrust-local-ci.service
sudo systemctl stop industrialrust-local-ci.timer
sudo systemctl disable --now industrialrust-local-ci.timer
```

## 4. PRのマージ運用（Actionsに依存しない）

このジョブが証明するのは **検証された `main` の1つのSHA** だけ。PR変更を `main` にマージする**前**に必要な検証は別途、人間がレビューし、隔離した一時VMで該当のPRのソース/HEAD SHAを固定して手動実行する。常駐サーバ上の本ジョブはPRコードを自動実行しない。PRの最新SHAが変わった場合は再検証する。

`main` へ統合した後は本スクリプトの `--once` で改めて結果を記録する。テスト失敗、ワールド保存変更、1.7.10 Protocol 5互換性が不明のものはマージ保留とする。GitHub ActionsのCIチェックマークはこのローカル検証の代わりに使わない。

## 5. 停止・削除

```bash
sudo systemctl disable --now industrialrust-local-ci.timer
sudo systemctl stop industrialrust-local-ci.service
sudo rm -f /etc/systemd/system/industrialrust-local-ci.service /etc/systemd/system/industrialrust-local-ci.timer
sudo systemctl daemon-reload
```

`state` に作られるログ・Cargoターゲットは自動削除しない。ログ確認や容量見積もりの後に、専用アカウントの所有する `state/cargo-target` と古い `state/reports` だけを手動整理する。本番ワールド・他プロジェクトの成果物には触れない。
