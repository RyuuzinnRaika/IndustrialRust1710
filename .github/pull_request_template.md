## 変更の目的・関連Issue

- 対応Issue: <!-- #番号。なければ理由を記載 -->
- 何が問題で、何をどう直したか:

## 変更した範囲と影響

- 対象: <!-- Rust crate / docs / 検証ツール -->
- Minecraft Java 1.7.10 / Protocol 5 の期待する動作:
- ワールド・プレイヤーデータ・保存形式への影響:
- ロールバック/既存データ保護の方法:

## 言語ポリシー

- [ ] 製品本体・運用・GUIの新規実装はRustのみ
- [ ] Pythonは独立した検証/テスト/監査/ベンチマークに限定
- [ ] 既存の非Rust実装（Issue #635）を拡大していない

## テストと検証（実施した項目のみチェック）

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo test --workspace --all-targets`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo build --release -p vanilla-server -p legacy-status-server`
- [ ] 対象機能のRust回帰/TCPテスト
- [ ] Windows実機テスト（該当する場合）
- [ ] 無改造Minecraft 1.7.10クライアント実機テスト（該当する場合）
- [ ] 保存/再起動/異常終了/復旧試験（該当する場合）

**テスト結果・ログ・環境・コミットSHA:**

<!-- 未実施/失敗項目がある場合、理由と残るリスクを必ず記載 -->

## セキュリティ・レビュー

- [ ] 未信頼のパケット/入力/権限チェックを検討（該当する場合）
- [ ] 秘密情報・実ユーザーデータ・Minecraft/Mod配布物を含めていない
- [ ] 他担当のopen/draft PRと競合・重複しないか確認した

**未完了事項・注意点・レビューしてほしい部分:**

<!-- 脆弱性の攻撃手順をここに載せない。非公開報告は SECURITY.md を参照 -->
