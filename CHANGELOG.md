## 2026-10-19 — プレイヤー座標の永続化（PR #5）

- プレイヤー座標（名前別）を上限・範囲チェック付きJSONに保存。
- ログアウト/再ログインで位置を復元し、ワールド境界外移動を補正。
- 自動テストでブロック保存と別にプレイヤー座標の永続性を検証。

## 2026-10-19 — ホワイトリストと20Hz Tick基盤（PR #4）

- バニラサーバーに fail-closed ユーザー名ホワイトリストと起動前バリデーションを導入。
- 接続中のアクセス取り消し、LAN明示許可、20Hz Tick時計と定期時刻通知を追加。
- 外部公開不可/UUID未検証などの限界と、バニラ完成までの残作業を文書化。

# 変更履歴

## 2026-10-19 — 1.7.10 Vanilla Rustサーバー基礎（PR #3）

- Mod/Forge/Config機能は当面保留。バニラ基本機能を先に実装する方針へ変更。
- 独立した vanilla-server：Protocol 5 Login/Play、固定平坦チャンク、Creativeブロック更新、独自IRW1保存を追加。
- 認証なし・1人のみ・既存世界と非互換という制限を明記。
- 単体テストと実TCPソケットのgoldenスモークテストをCIへ追加。

## 2026-10-19 — v0.3 Rustネイティブ優先に変更

- メインラインをForge/JVMオフロードから、Rust製Minecraft 1.7.10サーバーの新規実装へ変更。
- `crates/legacy-protocol` を追加。VarInt、フレーム、文字列、パケットカーソルの境界検証付き実装と単体テスト6件を記述。
- `apps/legacy-status-server` を追加。Protocol 5のHandshake→Status Request/Response→Ping/Pong、Login Startに対する明示的な切断をソース実装。
- `tests/protocol5_smoke.py` を追加。独立したプロトコルクライアントからの試験手順。
- `docs/10_Rustネイティブ優先方針.md`、`docs/11_Protocol5初期PoCの実行と検証.md` と要件定義のNAT-001〜006を追加。
- 旧資料のロードマップ等に優先順位変更を追記。

**検証制約：** 環境にRust toolchainが存在しないため、Cargo実行・バイナリ生成・1.7.10実機検証は未実施。Python smoke testの構文と固定Status JSONのJSON文法のみ確認。リポジトリのGitHub公開は未実施。