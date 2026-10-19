# 変更履歴

## 2026-10-19 — v0.3 Rustネイティブ優先に変更

- メインラインをForge/JVMオフロードから、Rust製Minecraft 1.7.10サーバーの新規実装へ変更。
- `crates/legacy-protocol` を追加。VarInt、フレーム、文字列、パケットカーソルの境界検証付き実装と単体テスト6件を記述。
- `apps/legacy-status-server` を追加。Protocol 5のHandshake→Status Request/Response→Ping/Pong、Login Startに対する明示的な切断をソース実装。
- `tests/protocol5_smoke.py` を追加。独立したプロトコルクライアントからの試験手順。
- `docs/10_Rustネイティブ優先方針.md`、`docs/11_Protocol5初期PoCの実行と検証.md` と要件定義のNAT-001〜006を追加。
- 旧資料のロードマップ等に優先順位変更を追記。

**検証制約：** 環境にRust toolchainが存在しないため、Cargo実行・バイナリ生成・1.7.10実機検証は未実施。Python smoke testの構文と固定Status JSONのJSON文法のみ確認。リポジトリのGitHub公開は未実施。