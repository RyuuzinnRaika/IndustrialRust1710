# IndustrialRust1710 — PR統合・ローカル検証チェックリスト（2026-10-29）

> 状態スナップショット。今後の担当者は**着手時にmainとPR HEADを再取得**してください。Issue [#347](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/347) の毎時統合履歴と [#660](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/660) の優先作業キューを参照してください。

## 統合前の最重要制約

1. **GitHub ActionsのRust CIは起動・再実行しない**。 `.github/workflows/ci.yml` の `workflow_dispatch` も実行しない。Rustの検証は開発PCなどのローカル環境で行い、実行できなければ `NOT_RUN` と報告する。
2. `docs/POLICY_RULES.md` の **GIT-004/GIT-005** に従う。Draftを直接マージしない。必須の非Rustチェック、レビュー、ローカルRustの関連試験・実機受け入れ、rollbackの根拠を確認する。**H-risk（保存/復旧/認証等）は管理者の変更内容に対する明示承認が必須**。
3. 既存ワールド・インベントリ・死亡ログ・プレイヤーデータの原本をテストに用いない。独立したテストワールド、または検証済みバックアップの複製を使う。
4. 非Rustの `J91 J92 operational helper tests` の成功は、Rust fmt/clippy/test/build またはMinecraft実機テストの成功ではない。

## 現在のPR一覧と受け入れの不足

2026-10-29 JST時点、`main` は `d21a568badda11a1f0aeb6c78302f3dd99326040`（PR #711が別担当によってマージ）。下記のHEADは調査時点の値。新コミットを積んだ場合は**新HEADで再検証**する。

| PR | 変更範囲 / リスク | スナップショットHEAD | マージ前の必須作業 |
|---|---|---|---|
| [#704](https://github.com/RyuuzinnRaika/IndustrialRust1710/pull/704) | Protocol 5採掘E2Eとテスト専用ヘルパー / L | `07c115db2ebfdefe251bbb32bfb419df7fe9011e` | 過去のWindows E2E 41/41 PASSはあるが全体品質ゲートはFAIL。現mainに対するHEAD単独試験、関連#711との整合を確認 |
| [#705](https://github.com/RyuuzinnRaika/IndustrialRust1710/pull/705) | Windows死亡ログ末尾破損の復旧と追記 / **H** | `bd0fe79857bd41d7c8a0002a730c2ef77846661e` | ローカル復旧テストに加え、最終HEADでのWindows実機・異常終了・ログ破損/重複回帰、**管理者の明示承認** |
| [#707](https://github.com/RyuuzinnRaika/IndustrialRust1710/pull/707) | ゾンビ攻撃 Protocol 5 TCP E2E / L | `78cd8b2442cba99a4586c4a53105cd74fc54801b` | 過去のPASSは未コミットの#704/#705変更を含んだ作業ツリー。HEADを独立して試験。実クライアントのクリック不具合 #706は別途追跡 |
| [#709](https://github.com/RyuuzinnRaika/IndustrialRust1710/pull/709) | #703の単体テスト期待値・fixture 4件 / L | `66f7b601b93246a07ccb6eabafe0b3c1aded8813` | 4個の個別単体テストとworkspace全体。死亡ログの残る1件は#705 |
| [#710](https://github.com/RyuuzinnRaika/IndustrialRust1710/pull/710) | #702のClippy 4分類 / M | `5442e1a3cf2eea20733a071283e45d3a193d6b6e` | Clippyとfmtを対象HEADで検証。**既存の大量のRustfmt差分は未修正** |
| [#711](https://github.com/RyuuzinnRaika/IndustrialRust1710/pull/711) | 採掘の20TPSタイミング猶予を変更 / M | merge `d21a568badda11a1f0aeb6c78302f3dd99326040` | **既にmainへマージ済み**だが、Rust/実クライアントの最終HEAD検証は未記録。採掘の復元/早掘り回帰を実機で優先確認 |

## 担当PCで実施する検証（GitHub Actionsでは実行しない）

PowerShellで対象PRの検証用worktreeを作り、**実行環境・ブランチ・HEAD SHA・テスト出力を同じ記録に残す**。`git switch` や `git worktree add` の前にローカルの未コミット変更を確認し、汚れた既存worktreeで異なるPRの結果を混ぜない。ワールド/データはこの確認だけでは操作しない。

```powershell
# リポジトリのルートで実行
git fetch origin
git status --short
git rev-parse HEAD
rustc --version
cargo --version

# 所有PC上の独立した検証作業ツリー（例：#709）
git worktree add --detach ..\IndustrialRust1710-verify-709 66f7b601b93246a07ccb6eabafe0b3c1aded8813
cd ..\IndustrialRust1710-verify-709
git status --short
git rev-parse HEAD

cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p vanilla-server -p legacy-status-server
```

※ 上の例は**未実行の手順**です。検証実施者は各コマンドの終了コード、失敗テスト名、ログ保存先と日時を記録します。PowerShellで複数のコマンドを続けるときは、終了コード `$LASTEXITCODE` をそれぞれ記録してください。結果は古いbase SHAの実行と混同しないでください。

### 対象絞り込みテスト例

```powershell
# #709 HEADで個別実行
cargo test -p vanilla-server --bin vanilla-server command_completion::tests::completes_player_names_and_legacy_arguments -- --exact
cargo test -p vanilla-server --bin vanilla-server survival::tests::wild_plants_are_breakable_and_return_legacy_survival_items -- --exact
cargo test -p vanilla-server --bin vanilla-server survival_damage::tests::simultaneous_heat_and_drowning_never_claim_single_source -- --exact
cargo test -p vanilla-server --bin vanilla-server world::tests::piston_destruction_drops_rail_and_device_item_ids -- --exact

# #704や#707の別HEADではそのPRだけの別worktreeで実行
cargo test -p vanilla-server --test protocol5_e2e
```

### 実機・異常終了の確認

- **採掘 #711/#704**：無改造 Java Edition 1.7.10 (Protocol 5) のSurvivalで草・土・石・木を同条件で繰り返し採掘し、掘れない/再出現/不自然な高速採掘とサーバー位置を記録。必要なら `IR_TRACE_MINING` と `IR_TRACE_MOVEMENT` を使用。実際の検証ワールドを事前にバックアップ。
- **死亡ログ #705**：テスト用JSONLの完全行と切れた最終行、Windowsのファイル共有条件を作り、追記後の完全行保持・重複なし・旧行を破損しないことを確認。実ワールド原本は禁止。
- **戦闘 #707/#706**：攻撃クリックで実際にUse Entityパケットが送られるか、ゾンビのHP減少・ノックバックとクライアント表示が一致するかを確認。

## マージ前チェック（PRごと）

- [ ] 現在の `main` と対象PRの `base_sha` / `head_sha` / `mergeable` を再取得
- [ ] コード差分と関連PRの変更が競合せず、別PRの未コミット差分に依存しない
- [ ] PRがDraftでない。H変更では**管理者の明示承認**が記録されている
- [ ] 適用されるrequired非Rustチェックが新HEADで成功し、重大な未解決レビュー指摘がない
- [ ] 必要なローカルRust試験および実機受入の結果をHEAD・環境・証跡付きで記録した（未実施なら未実施のまま保留）
- [ ] ワールド/セッション/ログなどの影響とrevert/復旧手順を確認
- [ ] マージ直前に別担当のmain更新を確認し、SHA照合付きでマージ
- [ ] マージ後にmain SHAと対象PRのmerged状態を読み戻し、必要な再検証結果をIssueへ反映

**結果の表記例**：`cargo test --workspace --all-targets: FAIL (4 tests; log ..., HEAD ...)`、`Protocol 5 client: NOT_RUN`。実施していない試験を `PASS` と書かない。
