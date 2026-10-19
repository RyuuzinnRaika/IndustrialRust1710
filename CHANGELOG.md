## 2026-10-19 — Anvilスナップショットの非破壊書き出し

- `--export-anvil NEW_DIR`でIRW1変更済みチャンクとスポーン周辺を1.7.10 Anvil地域ファイルに出力する。
- `level.dat`（gzip NBT）と`.mca`（4KiB sector + zlib NBT）を作成。バイナリ構造と高高度の編集をテスト。
- 既存出力先を上書きせず、既存ゲーム処理はIRW1のまま維持。
- Fastanvil/fastnbtの緩いOSSライセンスを使い、Pumpkin GPLv3ソースは不使用。

## 2026-10-19 — 動的チャンク配信と高高度セクション

- 初期25列固定配信から、位置に応じた差分チャンク配信/解放へ変更。境界は±16384ブロック。
- sparse IRW1世界を固定80×80から32,768×32,768の座標範囲へ拡張し、保存形式は維持。
- 既存ワールドの再読み込み、高高度と地下の追加セクション、負座標、ソケット移動試験を追加。
- 1.7.10 Anvil保存への移行、非同期チャンク生成、サバイバル機能は引き続き未対応。

## 2026-10-19 — Protocol 5の独立マルチプレイヤー実装

- Pumpkin非依存のRust独自実装で複数プレイヤー（最大8人）を登録・退出管理。
- 1.7.10独自のPlayer Info/Spawn Player/Entity Teleport/Destroyを実装。
- 接続中の他プレイヤーへ移動、ブロックの変更、チャットを配信。タブ一覧と/ list対応。
- 実TCPの2人同時ログイン、重複名拒否、移動、編集、退出テストをCIに追加。

## 2026-10-19 — GPLv3コードの現行ツリーからの除去

- Pumpkinからコピーした`crates/pumpkin-nbt`と、コピーしたGPLv3のトップレベル`LICENSE`を現行ツリーから削除。
- NBT入出力をMIT OR Apache-2.0のfastnbtへ置き換え、プレイヤー保存・移行の互換動作を維持。
- 全体のソースコードライセンスは未確定。元のGPLv3コミット/PRはGit履歴に残るため、履歴まで削除したとの主張はしない。
- `docs/15_Pumpkin導入監査.md`と`THIRD_PARTY_NOTICES.md`に、過去の取込みと現行コードの区別を記録。

## 2026-10-19 — Pumpkin由来NBTの段階導入（PR作業中）

- Pumpkin upstream c7a234ab8a3b3296d68a5f40411995920e11528a のNBTモジュールをGPLv3の表示と共にコピー（依存codec部分は省略）。
- プレイヤー座標保存を独自JSONからgzip NBTへ切替。旧JSONは移行読込に対応。
- Python実TCPスモークテストでプレイヤーNBTのPos復元を検査。
- 新しいMinecraft向けのプロトコルやワールド生成を1.7.10に混入させない。

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