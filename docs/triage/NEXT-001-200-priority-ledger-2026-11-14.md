# NEXT-001–200: IndustrialRust1710 Issue processing ledger

Updated: 2026-11-14 (JST). This is a triage/coordination ledger, **not** proof that 200 bugs exist or that 200 issues are fixed.
Scope: [NEXT Issue search](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues?q=is%3Aissue+NEXT-+in%3Atitle) (200 individual proposed acceptance/regression tasks).

## Safety and acceptance rules

- **P0**: preserve worlds, player inventories, security boundaries, admission and crash resistance; P1: primary vanilla 1.7.10 behavior; P2: instrumentation/documentation.
- **Never close an issue merely because code exists.** Completion requires an exact commit SHA, targeted Rust test, fmt/clippy where applicable and separate unmodified Java 1.7.10 client evidence when a wire/GUI behavior is claimed.
- **NOT_RUN ≠ PASS.** The connected-workflow environment has no Rust toolchain, so new PRs are deliberately Draft and unmerged until tested on a trusted machine.
- Avoid triggering GitHub Actions Rust CI; do not modify a production world; respect Rust-only runtime and `docs/PROJECT_POLICIES.md`.
- If another merged issue/PR provides the same coverage, inspect it and attach evidence, then close the duplicate with `duplicate`/`not_planned` and a reference. Do not delete work or duplicate implementations.

## Execution waves

| Wave | Definition | Start condition |
|---|---|---|
| W0 | Cross-file data integrity / rollback / corruption and item economy | Review first; test on disposable world copies |
| W1 | Authentication, packet parser, session/permission protection | Security-aware code review + local Rust tests |
| W2 | Critical gameplay / chunks / entity correctness | Run bounded Rust E2E and real-client checks |
| W3 | Major interoperability for networking, inventory and survival | P0 gates proven and merged |
| W4 | Extended game features, physics, dimensions, load and AI | M1-min gate proven |
| W5 | Quality, observability, documentation | Can parallelize where independent |

## Known active changes

| ID | PR | Change | Status |
|---|---|---|---|
| NEXT-001 | [#1186](https://github.com/RyuuzinnRaika/IndustrialRust1710/pull/1186) | half-closed LoginStart regression | Draft / NOT_RUN |
| NEXT-002 | [#1187](https://github.com/RyuuzinnRaika/IndustrialRust1710/pull/1187) | absolute EncryptionResponse deadline | Draft / NOT_RUN |
| NEXT-003 | [#1188](https://github.com/RyuuzinnRaika/IndustrialRust1710/pull/1188) | pre-admission identity lease | Draft / NOT_RUN |
| NEXT-017 | [#1189](https://github.com/RyuuzinnRaika/IndustrialRust1710/pull/1189) | WriteZero transport regression | Draft / NOT_RUN |
| NEXT-164 | [#1191](https://github.com/RyuuzinnRaika/IndustrialRust1710/pull/1191) | cleanup of failed prepared redo journal | Draft / NOT_RUN |
| NEXT-193 | [#1193](https://github.com/RyuuzinnRaika/IndustrialRust1710/pull/1193) | copy-only corrupt-world boot integrity verifier | Draft; Python verifier unit tests 5 PASS; actual Rust server NOT_RUN |

## Batch progress (2026-11-14)

- Six Draft PRs proposed: #1186–#1189, #1191 and #1193. None passed Rust/vanilla-client validation here and none was merged. Python-only verifier unit tests for #1193 passed 5/5 on the working copy.
- Closed as not_planned after source-based scope audit: [NEXT-163 #1146](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1146) (no transaction IDs in journal v1), [NEXT-172 #1155](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1155) (whitelist has no server-side save operation).
- Clarified redo-vs-rollback acceptance criteria in [NEXT-162 #1145](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1145). Static audits commented on NEXT-009, NEXT-011 and NEXT-021.
- Remaining unverified cases must remain open. Closing 200 issues by fiat would be misleading and unsafe.

## All 200 tracked issues in recommended processing order

| Order | Wave | Priority | Issue | Action status |
|---:|---|---|---|---|
| 1 | W0 Data safety | P0 | [#1145 NEXT-162 Chunk保存済・TileEntity保存失敗時のredo世代一貫性](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1145) | Open · reproduce/compare existing code, then fix or document PASS |
| 2 | W0 Data safety | P0 | [#1146 NEXT-163 Journal再生で同一トランザクションIDが2回現れた場合](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1146) | Closed · not_planned (journal v1 has no transaction ID; idempotent replay existing) |
| 3 | W0 Data safety | P0 | [#1147 NEXT-164 ディスク残容量不足時の保存保留と安全停止](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1147) | Draft PR [#1191](https://github.com/RyuuzinnRaika/IndustrialRust1710/pull/1191) / NOT_RUN; broader ENOSPC case open |
| 4 | W0 Data safety | P0 | [#1144 NEXT-161 プレイヤーデータ保存中に持ち物変更要求が来た場合のスナップショット境界](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1144) | Open · reproduce/compare existing code, then fix or document PASS |
| 5 | W0 Data safety | P0 | [#1074 NEXT-091 多数スロットを連続変更した直後のセーブ整合](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1074) | Open · reproduce/compare existing code, then fix or document PASS |
| 6 | W0 Data safety | P0 | [#1076 NEXT-093 装備スロット更新と死亡Dropが同Tickのときの一回性](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1076) | Open · reproduce/compare existing code, then fix or document PASS |
| 7 | W0 Data safety | P0 | [#1084 NEXT-101 作業台の結果スロットを保持したまま入力材料を変更](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1084) | Open · reproduce/compare existing code, then fix or document PASS |
| 8 | W0 Data safety | P0 | [#1075 NEXT-092 複数ItemEntityを連続拾得した際の容量上限処理](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1075) | Open · reproduce/compare existing code, then fix or document PASS |
| 9 | W0 Data safety | P0 | [#1094 NEXT-111 破壊確定と工具耐久ゼロ到達が同Tickで発生](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1094) | Open · reproduce/compare existing code, then fix or document PASS |
| 10 | W0 Data safety | P0 | [#1155 NEXT-172 Whitelist保存失敗時にメモリだけ許可状態が変わらないこと](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1155) | Closed · not_planned (whitelist is read-only; reload is fail-closed) |
| 11 | W0 Data safety | P0 | [#1176 NEXT-193 破損ワールドでの起動失敗後に元データが不変か確認](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1176) | Draft PR [#1193](https://github.com/RyuuzinnRaika/IndustrialRust1710/pull/1193); Python verifier tests 5 PASS / native Rust NOT_RUN |
| 12 | W1 Protocol & auth | P0 | [#992 NEXT-009 Authentication成功後Play初期化失敗時の登録巻戻し](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/992) | Open · reproduce/compare existing code, then fix or document PASS |
| 13 | W1 Protocol & auth | P0 | [#986 NEXT-003 認証待ち中の同一ユーザー名による二重ログイン競合](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/986) | Draft PR [#1188](https://github.com/RyuuzinnRaika/IndustrialRust1710/pull/1188) / NOT_RUN |
| 14 | W1 Protocol & auth | P0 | [#985 NEXT-002 EncryptionResponse到着前のログインタイムアウト境界](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/985) | Draft PR [#1187](https://github.com/RyuuzinnRaika/IndustrialRust1710/pull/1187) / NOT_RUN |
| 15 | W1 Protocol & auth | P0 | [#989 NEXT-006 暗号化有効化境界で連続受信したフレームの復号単位](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/989) | Open · reproduce/compare existing code, then fix or document PASS |
| 16 | W1 Protocol & auth | P0 | [#984 NEXT-001 LoginStart直後にTCP半切断された接続の認証状態破棄](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/984) | Draft PR [#1186](https://github.com/RyuuzinnRaika/IndustrialRust1710/pull/1186) / NOT_RUN |
| 17 | W1 Protocol & auth | P0 | [#997 NEXT-014 Play開始前に先行到着した移動パケットの拒否](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/997) | Open · reproduce/compare existing code, then fix or document PASS |
| 18 | W1 Protocol & auth | P0 | [#994 NEXT-011 VarIntがバッファ末尾で途切れた場合の接続単位リセット](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/994) | Open · reproduce/compare existing code, then fix or document PASS |
| 19 | W1 Protocol & auth | P0 | [#1000 NEXT-017 送信側TCP writeゼロバイト反復時のCPUスピン防止](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1000) | Draft PR [#1189](https://github.com/RyuuzinnRaika/IndustrialRust1710/pull/1189) / NOT_RUN |
| 20 | W1 Protocol & auth | P0 | [#1004 NEXT-021 ワールド境界近傍の巨大相対移動値を有限数として検証](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1004) | Open · reproduce/compare existing code, then fix or document PASS |
| 21 | W1 Protocol & auth | P0 | [#1154 NEXT-171 管理コマンドの実行中に権限剥奪された場合の確定条件](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1154) | Open · reproduce/compare existing code, then fix or document PASS |
| 22 | W2 Runtime correctness | P0 | [#1104 NEXT-121 死亡確定と次の近接攻撃が同Tickで競合する場合を拒否](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1104) | Open · reproduce/compare existing code, then fix or document PASS |
| 23 | W2 Runtime correctness | P0 | [#1134 NEXT-151 ポータル転送開始時に死亡したプレイヤーの遷移取消](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1134) | Open · reproduce/compare existing code, then fix or document PASS |
| 24 | W2 Runtime correctness | P0 | [#1014 NEXT-031 Entity可視範囲加入中に死亡した対象のSpawn抑止](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1014) | Open · reproduce/compare existing code, then fix or document PASS |
| 25 | W2 Runtime correctness | P0 | [#1029 NEXT-046 同一Chunkを複数Sessionが要求した場合の生成排他](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1029) | Open · reproduce/compare existing code, then fix or document PASS |
| 26 | W2 Runtime correctness | P0 | [#1024 NEXT-041 Join中にブロック更新された初期Chunkの世代一致](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1024) | Open · reproduce/compare existing code, then fix or document PASS |
| 27 | W2 Runtime correctness | P0 | [#1054 NEXT-071 複数レッドストーン粉末同時除去時の安定収束](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1054) | Open · reproduce/compare existing code, then fix or document PASS |
| 28 | W2 Runtime correctness | P0 | [#1175 NEXT-192 2人サバイバルの採掘→譲渡→死亡→再起動を連続検証](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1175) | Open · reproduce/compare existing code, then fix or document PASS |
| 29 | W2 Runtime correctness | P0 | [#1174 NEXT-191 実機1.7.10で10分連続移動し不要なTeleport補正を記録](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1174) | Open · reproduce/compare existing code, then fix or document PASS |
| 30 | W2 Runtime correctness | P0 | [#995 NEXT-012 大きなChatとMoveパケットを連続投入した際の公平性](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/995) | Open · reproduce/compare existing code, then fix or document PASS |
| 31 | W3 Main compatibility | P1 | [#987 NEXT-004 Handshakeの余剰バイトと後続Packetの境界解釈](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/987) | Open · reproduce/compare existing code, then fix or document PASS |
| 32 | W3 Main compatibility | P1 | [#988 NEXT-005 Status後の不正LoginStart再利用を接続単位で隔離](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/988) | Open · reproduce/compare existing code, then fix or document PASS |
| 33 | W3 Main compatibility | P1 | [#990 NEXT-007 空白・全角類似名などプレイヤー名正規化の一貫性](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/990) | Open · reproduce/compare existing code, then fix or document PASS |
| 34 | W3 Main compatibility | P1 | [#991 NEXT-008 正規UUIDでの大小文字表現差を同一アカウントとして扱う](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/991) | Open · reproduce/compare existing code, then fix or document PASS |
| 35 | W3 Main compatibility | P1 | [#993 NEXT-010 初期スポーン決定中に切断した接続の世代管理](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/993) | Open · reproduce/compare existing code, then fix or document PASS |
| 36 | W3 Main compatibility | P1 | [#996 NEXT-013 書込キュー中のDestroyEntityと再Spawnの順序保証](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/996) | Open · reproduce/compare existing code, then fix or document PASS |
| 37 | W3 Main compatibility | P1 | [#998 NEXT-015 Read timeoutとKeepAlive timeoutの優先順位統一](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/998) | Open · reproduce/compare existing code, then fix or document PASS |
| 38 | W3 Main compatibility | P1 | [#999 NEXT-016 送信バッチ境界での複数ChunkPacket圧縮状態検証](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/999) | Open · reproduce/compare existing code, then fix or document PASS |
| 39 | W3 Main compatibility | P1 | [#1001 NEXT-018 低速受信者へのChat通知と地形配信の優先度分離](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1001) | Open · reproduce/compare existing code, then fix or document PASS |
| 40 | W3 Main compatibility | P1 | [#1002 NEXT-019 Session終了後に非同期送信結果が戻る場合の破棄](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1002) | Open · reproduce/compare existing code, then fix or document PASS |
| 41 | W3 Main compatibility | P1 | [#1005 NEXT-022 同TickにPositionとLookが複数到着した際の最終姿勢](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1005) | Open · reproduce/compare existing code, then fix or document PASS |
| 42 | W3 Main compatibility | P1 | [#1006 NEXT-023 水流から陸地への遷移瞬間のサーバー速度補正](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1006) | Open · reproduce/compare existing code, then fix or document PASS |
| 43 | W3 Main compatibility | P1 | [#1007 NEXT-024 ボートから徒歩へ切替えた直後の水面当たり判定](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1007) | Open · reproduce/compare existing code, then fix or document PASS |
| 44 | W3 Main compatibility | P1 | [#1008 NEXT-025 馬のジャンプ充電中に降車した場合の移動権限](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1008) | Open · reproduce/compare existing code, then fix or document PASS |
| 45 | W3 Main compatibility | P1 | [#1009 NEXT-026 フェンス上ジャンプ時の衝突ボックス高さ差](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1009) | Open · reproduce/compare existing code, then fix or document PASS |
| 46 | W3 Main compatibility | P1 | [#1010 NEXT-027 プレイヤー同士の密着移動と押出し順序](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1010) | Open · reproduce/compare existing code, then fix or document PASS |
| 47 | W3 Main compatibility | P1 | [#1011 NEXT-028 ノックバック中のテレポートで残存速度を初期化](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1011) | Open · reproduce/compare existing code, then fix or document PASS |
| 48 | W3 Main compatibility | P1 | [#1012 NEXT-029 ボート着岸後の滑走・破損判定](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1012) | Open · reproduce/compare existing code, then fix or document PASS |
| 49 | W3 Main compatibility | P1 | [#1013 NEXT-030 踏み抜き可能な作物上での接地判定と踏み荒らし](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1013) | Open · reproduce/compare existing code, then fix or document PASS |
| 50 | W3 Main compatibility | P1 | [#1015 NEXT-032 装備変更中に視距離へ入った観測者への初期同期](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1015) | Open · reproduce/compare existing code, then fix or document PASS |
| 51 | W3 Main compatibility | P1 | [#1016 NEXT-033 燃焼状態変化中に観測者が離脱した場合の送信抑止](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1016) | Open · reproduce/compare existing code, then fix or document PASS |
| 52 | W3 Main compatibility | P1 | [#1017 NEXT-034 Mobの騎乗関係がChunkをまたいだ場合の復元](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1017) | Open · reproduce/compare existing code, then fix or document PASS |
| 53 | W3 Main compatibility | P1 | [#1018 NEXT-035 落下中ItemEntityが合体した場合の観測者更新](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1018) | Open · reproduce/compare existing code, then fix or document PASS |
| 54 | W3 Main compatibility | P1 | [#1019 NEXT-036 射撃者がログアウトした後の矢の所有者帰属](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1019) | Open · reproduce/compare existing code, then fix or document PASS |
| 55 | W3 Main compatibility | P1 | [#1020 NEXT-037 釣り浮きEntityのSpawn/移動/回収表示整合](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1020) | Open · reproduce/compare existing code, then fix or document PASS |
| 56 | W3 Main compatibility | P1 | [#1021 NEXT-038 絵画の設置面更新と観測者向きの初期化](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1021) | Open · reproduce/compare existing code, then fix or document PASS |
| 57 | W3 Main compatibility | P1 | [#1022 NEXT-039 アイテムフレーム破壊時の額縁と中身のDrop分離](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1022) | Open · reproduce/compare existing code, then fix or document PASS |
| 58 | W3 Main compatibility | P1 | [#1025 NEXT-042 圧縮済Chunk生成結果の期限切れ破棄](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1025) | Open · reproduce/compare existing code, then fix or document PASS |
| 59 | W3 Main compatibility | P1 | [#1026 NEXT-043 跨ぎTileEntityの撤去とChunk再読み込み同期](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1026) | Open · reproduce/compare existing code, then fix or document PASS |
| 60 | W3 Main compatibility | P1 | [#1027 NEXT-044 Chunk送信順とChunk内のMob Spawn順を統合検証](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1027) | Open · reproduce/compare existing code, then fix or document PASS |
| 61 | W3 Main compatibility | P1 | [#1028 NEXT-045 負座標のBiome境界でのMapChunkBitmap照合](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1028) | Open · reproduce/compare existing code, then fix or document PASS |
| 62 | W3 Main compatibility | P1 | [#1030 NEXT-047 Chunk配信途中で難易度変更されたMob情報を再評価](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1030) | Open · reproduce/compare existing code, then fix or document PASS |
| 63 | W3 Main compatibility | P1 | [#1031 NEXT-048 地下採掘で空になったSectionの送信表現](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1031) | Open · reproduce/compare existing code, then fix or document PASS |
| 64 | W3 Main compatibility | P1 | [#1032 NEXT-049 遠方Chunkアンロード後のブロック更新蓄積を抑制](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1032) | Open · reproduce/compare existing code, then fix or document PASS |
| 65 | W3 Main compatibility | P1 | [#1077 NEXT-094 ダメージ値付きアイテムをホッパーで移送した際のNBT保持](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1077) | Open · reproduce/compare existing code, then fix or document PASS |
| 66 | W3 Main compatibility | P1 | [#1078 NEXT-095 Quickbar選択直後の攻撃で正しい装備を参照](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1078) | Open · reproduce/compare existing code, then fix or document PASS |
| 67 | W3 Main compatibility | P1 | [#1079 NEXT-096 空のエンダーチェスト初回開封時の個人領域割当](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1079) | Open · reproduce/compare existing code, then fix or document PASS |
| 68 | W3 Main compatibility | P1 | [#1080 NEXT-097 アイテム名の斜体・色コード付き表示とNBT保存](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1080) | Open · reproduce/compare existing code, then fix or document PASS |
| 69 | W3 Main compatibility | P1 | [#1081 NEXT-098 Creative破棄スロットへの不正負数個数を拒否](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1081) | Open · reproduce/compare existing code, then fix or document PASS |
| 70 | W3 Main compatibility | P1 | [#1082 NEXT-099 Potion NBT差分を持つ瓶同士の不正合成防止](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1082) | Open · reproduce/compare existing code, then fix or document PASS |
| 71 | W3 Main compatibility | P1 | [#1085 NEXT-102 クラフト画面からInventoryへ戻る際の残余材料返却](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1085) | Open · reproduce/compare existing code, then fix or document PASS |
| 72 | W3 Main compatibility | P1 | [#1086 NEXT-103 Shiftクリックで複数レシピ候補がある場合の優先選択](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1086) | Open · reproduce/compare existing code, then fix or document PASS |
| 73 | W3 Main compatibility | P1 | [#1087 NEXT-104 かまど稼働中に燃料スロットを空にするGUI再計算](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1087) | Open · reproduce/compare existing code, then fix or document PASS |
| 74 | W3 Main compatibility | P1 | [#1088 NEXT-105 醸造完了直後に瓶を交換した際の効果適用対象](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1088) | Open · reproduce/compare existing code, then fix or document PASS |
| 75 | W3 Main compatibility | P1 | [#1089 NEXT-106 金床で素材修理後に残余スタックをカーソルへ戻す](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1089) | Open · reproduce/compare existing code, then fix or document PASS |
| 76 | W3 Main compatibility | P1 | [#1090 NEXT-107 エンチャント候補表示中に本棚を除去した場合の再抽選](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1090) | Open · reproduce/compare existing code, then fix or document PASS |
| 77 | W3 Main compatibility | P1 | [#1091 NEXT-108 村人取引GUIの成立と取引相手死亡が競合する境界](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1091) | Open · reproduce/compare existing code, then fix or document PASS |
| 78 | W3 Main compatibility | P1 | [#1092 NEXT-109 ビーコンGUIの主効果選択と素材消費の整合](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1092) | Open · reproduce/compare existing code, then fix or document PASS |
| 79 | W3 Main compatibility | P1 | [#1095 NEXT-112 採掘開始後にゲームモード変更された場合の進行破棄](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1095) | Open · reproduce/compare existing code, then fix or document PASS |
| 80 | W3 Main compatibility | P1 | [#1096 NEXT-113 途中で持ち替えた際の採掘進捗再計算と実機同期](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1096) | Open · reproduce/compare existing code, then fix or document PASS |
| 81 | W3 Main compatibility | P1 | [#1097 NEXT-114 採掘対象の後方が水になった直後の更新通知](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1097) | Open · reproduce/compare existing code, then fix or document PASS |
| 82 | W3 Main compatibility | P1 | [#1098 NEXT-115 ミルク飲用直前に毒が再付与された場合の効果除去](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1098) | Open · reproduce/compare existing code, then fix or document PASS |
| 83 | W3 Main compatibility | P1 | [#1099 NEXT-116 満腹状態での食料使用を食事開始前に拒否する](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1099) | Open · reproduce/compare existing code, then fix or document PASS |
| 84 | W3 Main compatibility | P1 | [#1100 NEXT-117 腐った肉の空腹効果と満腹度回復の同時適用](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1100) | Open · reproduce/compare existing code, then fix or document PASS |
| 85 | W3 Main compatibility | P1 | [#1101 NEXT-118 溺死カウントダウン中に呼吸エンチャント装備を交換](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1101) | Open · reproduce/compare existing code, then fix or document PASS |
| 86 | W3 Main compatibility | P1 | [#1102 NEXT-119 ベッド撤去後の死亡で安全な世界スポーンへ戻る](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1102) | Open · reproduce/compare existing code, then fix or document PASS |
| 87 | W3 Main compatibility | P1 | [#1105 NEXT-122 弓を最大まで引く前に切断した場合の矢消費](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1105) | Open · reproduce/compare existing code, then fix or document PASS |
| 88 | W3 Main compatibility | P1 | [#1106 NEXT-123 射撃中に弓を持ち替えた場合の威力と消費元](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1106) | Open · reproduce/compare existing code, then fix or document PASS |
| 89 | W3 Main compatibility | P1 | [#1107 NEXT-124 投げたポーションが水面で破損した時の効果範囲](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1107) | Open · reproduce/compare existing code, then fix or document PASS |
| 90 | W3 Main compatibility | P1 | [#1108 NEXT-125 火炎エンチャント付き矢の着弾と雨での消火](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1108) | Open · reproduce/compare existing code, then fix or document PASS |
| 91 | W3 Main compatibility | P1 | [#1109 NEXT-126 エンダーパール命中とプレイヤー死亡の競合](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1109) | Open · reproduce/compare existing code, then fix or document PASS |
| 92 | W3 Main compatibility | P1 | [#1110 NEXT-127 雪玉によるBlaze追加ダメージと一般Mob無害判定](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1110) | Open · reproduce/compare existing code, then fix or document PASS |
| 93 | W3 Main compatibility | P1 | [#1111 NEXT-128 釣り竿でMobを引き寄せた際の速度と釣り糸破棄](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1111) | Open · reproduce/compare existing code, then fix or document PASS |
| 94 | W3 Main compatibility | P1 | [#1112 NEXT-129 ノックバックエンチャントと通常ノックバックの合成順](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1112) | Open · reproduce/compare existing code, then fix or document PASS |
| 95 | W3 Main compatibility | P1 | [#1148 NEXT-165 旧IRWファイルの未知フィールドを再保存時に保全](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1148) | Open · reproduce/compare existing code, then fix or document PASS |
| 96 | W3 Main compatibility | P1 | [#1149 NEXT-166 Anvil取り込みで同じChunk座標が重複した場合の拒否](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1149) | Open · reproduce/compare existing code, then fix or document PASS |
| 97 | W3 Main compatibility | P1 | [#1150 NEXT-167 NBT再帰深度上限を超えたTileEntityの隔離処理](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1150) | Open · reproduce/compare existing code, then fix or document PASS |
| 98 | W3 Main compatibility | P1 | [#1151 NEXT-168 複数世代Backupの復元先に旧Journalがある場合の検証](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1151) | Open · reproduce/compare existing code, then fix or document PASS |
| 99 | W3 Main compatibility | P1 | [#1152 NEXT-169 ワールド生成器バージョン変更時の既存Chunk境界保護](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1152) | Open · reproduce/compare existing code, then fix or document PASS |
| 100 | W3 Main compatibility | P1 | [#1156 NEXT-173 ゲーム中の設定リロードで不変設定と動的設定を分離](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1156) | Open · reproduce/compare existing code, then fix or document PASS |
| 101 | W3 Main compatibility | P1 | [#1157 NEXT-174 /save-allと自動保存が同Tick発火した場合の単一実行](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1157) | Open · reproduce/compare existing code, then fix or document PASS |
| 102 | W3 Main compatibility | P1 | [#1158 NEXT-175 /stop要求後に受信した新規Loginを確実に拒否](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1158) | Open · reproduce/compare existing code, then fix or document PASS |
| 103 | W3 Main compatibility | P1 | [#1159 NEXT-176 OP権限変更後のTab補完候補を即時再評価](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1159) | Open · reproduce/compare existing code, then fix or document PASS |
| 104 | W3 Main compatibility | P1 | [#1160 NEXT-177 設定ファイル内の相対パス解釈を起動ディレクトリから独立](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1160) | Open · reproduce/compare existing code, then fix or document PASS |
| 105 | W3 Main compatibility | P1 | [#1161 NEXT-178 Rust管理GUIからサーバー異常終了を一意に表示](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1161) | Open · reproduce/compare existing code, then fix or document PASS |
| 106 | W4 Extended compatibility | P1 | [#1034 NEXT-051 異なる探索順で村の家屋向きが変化しないことを保証](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1034) | Open · reproduce/compare existing code, then fix or document PASS |
| 107 | W4 Extended compatibility | P1 | [#1035 NEXT-052 湿地境界でのウィッチ小屋床高さと湧き領域](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1035) | Open · reproduce/compare existing code, then fix or document PASS |
| 108 | W4 Extended compatibility | P1 | [#1036 NEXT-053 樹木生成と地形編集が競合した際の人工ブロック保護](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1036) | Open · reproduce/compare existing code, then fix or document PASS |
| 109 | W4 Extended compatibility | P1 | [#1037 NEXT-054 ダンジョンチェストLootの固定seed再現性](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1037) | Open · reproduce/compare existing code, then fix or document PASS |
| 110 | W4 Extended compatibility | P1 | [#1038 NEXT-055 砂漠井戸の生成位置と隣接砂の置換判定](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1038) | Open · reproduce/compare existing code, then fix or document PASS |
| 111 | W4 Extended compatibility | P1 | [#1039 NEXT-056 巨大キノコ生成時の葉/キノコブロック置換優先度](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1039) | Open · reproduce/compare existing code, then fix or document PASS |
| 112 | W4 Extended compatibility | P1 | [#1040 NEXT-057 廃坑SpiderSpawnerの配置と通路断面の整合](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1040) | Open · reproduce/compare existing code, then fix or document PASS |
| 113 | W4 Extended compatibility | P1 | [#1041 NEXT-058 村鍛冶屋チェスト生成の二重Loot防止](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1041) | Open · reproduce/compare existing code, then fix or document PASS |
| 114 | W4 Extended compatibility | P1 | [#1042 NEXT-059 氷原と雪原での雪層初期配置の固定seed比較](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1042) | Open · reproduce/compare existing code, then fix or document PASS |
| 115 | W4 Extended compatibility | P1 | [#1044 NEXT-061 火を消した直後の光減衰とMobスポーン順序](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1044) | Open · reproduce/compare existing code, then fix or document PASS |
| 116 | W4 Extended compatibility | P1 | [#1045 NEXT-062 発光ブロックを移動させるピストン処理中の光再計算](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1045) | Open · reproduce/compare existing code, then fix or document PASS |
| 117 | W4 Extended compatibility | P1 | [#1046 NEXT-063 夜明け瞬間に葉の陰のSkylightが遅延しないか検証](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1046) | Open · reproduce/compare existing code, then fix or document PASS |
| 118 | W4 Extended compatibility | P1 | [#1047 NEXT-064 雨が屋根下の炎と作物に与える影響を分離](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1047) | Open · reproduce/compare existing code, then fix or document PASS |
| 119 | W4 Extended compatibility | P1 | [#1048 NEXT-065 雷の着弾位置に応じた炎生成とEntity被害](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1048) | Open · reproduce/compare existing code, then fix or document PASS |
| 120 | W4 Extended compatibility | P1 | [#1049 NEXT-066 Frozen Riverの氷融解後に水の更新が伝播](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1049) | Open · reproduce/compare existing code, then fix or document PASS |
| 121 | W4 Extended compatibility | P1 | [#1050 NEXT-067 RandomTick対象をセクション移動後にも維持](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1050) | Open · reproduce/compare existing code, then fix or document PASS |
| 122 | W4 Extended compatibility | P1 | [#1051 NEXT-068 ワールド時刻を逆行させた際の作物進行時間の単調性](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1051) | Open · reproduce/compare existing code, then fix or document PASS |
| 123 | W4 Extended compatibility | P1 | [#1053 NEXT-070 低光量下でのキノコ増殖と明るさ判定境界](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1053) | Open · reproduce/compare existing code, then fix or document PASS |
| 124 | W4 Extended compatibility | P1 | [#1055 NEXT-072 リピーターのロック入力切替え中に予約Tickを更新](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1055) | Open · reproduce/compare existing code, then fix or document PASS |
| 125 | W4 Extended compatibility | P1 | [#1056 NEXT-073 コンパレーターの減算モード切替え時の予定更新整理](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1056) | Open · reproduce/compare existing code, then fix or document PASS |
| 126 | W4 Extended compatibility | P1 | [#1057 NEXT-074 ホッパー上のレールとコンテナ向き更新の連動](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1057) | Open · reproduce/compare existing code, then fix or document PASS |
| 127 | W4 Extended compatibility | P1 | [#1058 NEXT-075 ドロッパー連鎖回路での同Tick順序依存を再現](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1058) | Open · reproduce/compare existing code, then fix or document PASS |
| 128 | W4 Extended compatibility | P1 | [#1059 NEXT-076 トラップドアの設置面破壊と信号更新の同時処理](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1059) | Open · reproduce/compare existing code, then fix or document PASS |
| 129 | W4 Extended compatibility | P1 | [#1060 NEXT-077 感圧板の複数Entity重複接触と解除タイミング](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1060) | Open · reproduce/compare existing code, then fix or document PASS |
| 130 | W4 Extended compatibility | P1 | [#1061 NEXT-078 オブザーバー不在の1.7.10における更新経路の明示](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1061) | Open · reproduce/compare existing code, then fix or document PASS |
| 131 | W4 Extended compatibility | P1 | [#1062 NEXT-079 赤石灯への連続ON/OFF入力で更新待ちが消えないこと](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1062) | Open · reproduce/compare existing code, then fix or document PASS |
| 132 | W4 Extended compatibility | P1 | [#1064 NEXT-081 満載チェスト付きトロッコ破壊時の在庫Drop保証](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1064) | Open · reproduce/compare existing code, then fix or document PASS |
| 133 | W4 Extended compatibility | P1 | [#1065 NEXT-082 ホッパー付きトロッコが停止中にアイテム回収する周期](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1065) | Open · reproduce/compare existing code, then fix or document PASS |
| 134 | W4 Extended compatibility | P1 | [#1066 NEXT-083 アクティベーターレールがTNT車両を起爆する経路](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1066) | Open · reproduce/compare existing code, then fix or document PASS |
| 135 | W4 Extended compatibility | P1 | [#1067 NEXT-084 レール分岐切替中に車両が接続点を通過する衝突](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1067) | Open · reproduce/compare existing code, then fix or document PASS |
| 136 | W4 Extended compatibility | P1 | [#1068 NEXT-085 空のトロッコを高速で押したときの速度減衰](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1068) | Open · reproduce/compare existing code, then fix or document PASS |
| 137 | W4 Extended compatibility | P1 | [#1069 NEXT-086 ボート同士の衝突と破損後のDrop個数](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1069) | Open · reproduce/compare existing code, then fix or document PASS |
| 138 | W4 Extended compatibility | P1 | [#1070 NEXT-087 ボート乗船中にゲームモード変更した場合の状態同期](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1070) | Open · reproduce/compare existing code, then fix or document PASS |
| 139 | W4 Extended compatibility | P1 | [#1071 NEXT-088 車両が未読込Chunkへ到達する直前の移動停止](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1071) | Open · reproduce/compare existing code, then fix or document PASS |
| 140 | W4 Extended compatibility | P1 | [#1072 NEXT-089 乗員死亡中の馬およびトロッコのDetach通知](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1072) | Open · reproduce/compare existing code, then fix or document PASS |
| 141 | W4 Extended compatibility | P1 | [#1114 NEXT-131 敵Mobのターゲット対象がログアウトした瞬間の再探索](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1114) | Open · reproduce/compare existing code, then fix or document PASS |
| 142 | W4 Extended compatibility | P1 | [#1115 NEXT-132 追跡中Zombieがドア破壊を開始し難易度変更された場合](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1115) | Open · reproduce/compare existing code, then fix or document PASS |
| 143 | W4 Extended compatibility | P1 | [#1116 NEXT-133 Skeletonの矢が障害物で止まった場合の再経路探索](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1116) | Open · reproduce/compare existing code, then fix or document PASS |
| 144 | W4 Extended compatibility | P1 | [#1117 NEXT-134 Creeperの起爆寸前にPeacefulへ変更した場合の破棄](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1117) | Open · reproduce/compare existing code, then fix or document PASS |
| 145 | W4 Extended compatibility | P1 | [#1118 NEXT-135 子Mobと大人Mobを同時飼育した際の年齢遷移保存](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1118) | Open · reproduce/compare existing code, then fix or document PASS |
| 146 | W4 Extended compatibility | P1 | [#1119 NEXT-136 狼が飼い主の攻撃対象を変更した時の敵対ターゲット](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1119) | Open · reproduce/compare existing code, then fix or document PASS |
| 147 | W4 Extended compatibility | P1 | [#1120 NEXT-137 スパイダージョッキーの騎乗構成と同時デスポーン](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1120) | Open · reproduce/compare existing code, then fix or document PASS |
| 148 | W4 Extended compatibility | P1 | [#1121 NEXT-138 Ghast火球を打ち返した後の攻撃者帰属更新](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1121) | Open · reproduce/compare existing code, then fix or document PASS |
| 149 | W4 Extended compatibility | P1 | [#1122 NEXT-139 Endermanの運搬ブロックが破壊された場合の状態解除](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1122) | Open · reproduce/compare existing code, then fix or document PASS |
| 150 | W4 Extended compatibility | P1 | [#1124 NEXT-141 小麦の最終成長Tickと収穫操作が競合した場合](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1124) | Open · reproduce/compare existing code, then fix or document PASS |
| 151 | W4 Extended compatibility | P1 | [#1125 NEXT-142 カボチャ/スイカ茎の向き変更と隣接果実生成](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1125) | Open · reproduce/compare existing code, then fix or document PASS |
| 152 | W4 Extended compatibility | P1 | [#1126 NEXT-143 サトウキビ最下段を破壊した時の上部連鎖Drop](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1126) | Open · reproduce/compare existing code, then fix or document PASS |
| 153 | W4 Extended compatibility | P1 | [#1127 NEXT-144 カカオ豆の向き別成熟とジャングル原木撤去](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1127) | Open · reproduce/compare existing code, then fix or document PASS |
| 154 | W4 Extended compatibility | P1 | [#1128 NEXT-145 羊を染色した直後に毛刈りした場合の色と数量](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1128) | Open · reproduce/compare existing code, then fix or document PASS |
| 155 | W4 Extended compatibility | P1 | [#1129 NEXT-146 村人の交易補充時にChunkをUnload/Reloadする挙動](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1129) | Open · reproduce/compare existing code, then fix or document PASS |
| 156 | W4 Extended compatibility | P1 | [#1130 NEXT-147 村の扉数変更後のゴーレム自然生成条件再判定](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1130) | Open · reproduce/compare existing code, then fix or document PASS |
| 157 | W4 Extended compatibility | P1 | [#1131 NEXT-148 村人の職業と取引リストを旧NBTから保存復元](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1131) | Open · reproduce/compare existing code, then fix or document PASS |
| 158 | W4 Extended compatibility | P1 | [#1132 NEXT-149 動物の求愛状態中に餌が切れた場合の解除](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1132) | Open · reproduce/compare existing code, then fix or document PASS |
| 159 | W4 Extended compatibility | P1 | [#1135 NEXT-152 Nether出口の安全位置探索が溶岩海に達した場合](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1135) | Open · reproduce/compare existing code, then fix or document PASS |
| 160 | W4 Extended compatibility | P1 | [#1136 NEXT-153 複数ポータルのリンク先が等距離だった場合の決定性](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1136) | Open · reproduce/compare existing code, then fix or document PASS |
| 161 | W4 Extended compatibility | P1 | [#1137 NEXT-154 ポータル枠の一部破壊と転送待ち時間のリセット](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1137) | Open · reproduce/compare existing code, then fix or document PASS |
| 162 | W4 Extended compatibility | P1 | [#1138 NEXT-155 End入口でエンダーアイを挿入し最後の枠を完成させる](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1138) | Open · reproduce/compare existing code, then fix or document PASS |
| 163 | W4 Extended compatibility | P1 | [#1139 NEXT-156 Endから帰還中に切断した場合の帰還地点保持](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1139) | Open · reproduce/compare existing code, then fix or document PASS |
| 164 | W4 Extended compatibility | P1 | [#1140 NEXT-157 Nether要塞外のBlazeスポーン禁止範囲の検証](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1140) | Open · reproduce/compare existing code, then fix or document PASS |
| 165 | W4 Extended compatibility | P1 | [#1141 NEXT-158 黒曜石ポータルと水/溶岩流動の隣接更新](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1141) | Open · reproduce/compare existing code, then fix or document PASS |
| 166 | W4 Extended compatibility | P1 | [#1142 NEXT-159 EnderDragon討伐中に全員が退出した場合の状態永続化](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1142) | Open · reproduce/compare existing code, then fix or document PASS |
| 167 | W4 Extended compatibility | P1 | [#1164 NEXT-181 AI経路探索を多人数Mob集中時にTick予算内へ収める](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1164) | Open · reproduce/compare existing code, then fix or document PASS |
| 168 | W4 Extended compatibility | P1 | [#1165 NEXT-182 Chunk圧縮処理とネットワーク送信を分離して混雑検証](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1165) | Open · reproduce/compare existing code, then fix or document PASS |
| 169 | W4 Extended compatibility | P1 | [#1166 NEXT-183 多数チェストを開閉する負荷でロック競合を可視化](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1166) | Open · reproduce/compare existing code, then fix or document PASS |
| 170 | W4 Extended compatibility | P1 | [#1167 NEXT-184 同一ChunkのBlockUpdate多発時にパケットを集約](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1167) | Open · reproduce/compare existing code, then fix or document PASS |
| 171 | W4 Extended compatibility | P1 | [#1168 NEXT-185 高頻度TNT点火による落下Entity・光計算複合負荷試験](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1168) | Open · reproduce/compare existing code, then fix or document PASS |
| 172 | W4 Extended compatibility | P1 | [#1169 NEXT-186 プレイヤー退室後のChunkキャッシュ回収時間を測定](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1169) | Open · reproduce/compare existing code, then fix or document PASS |
| 173 | W4 Extended compatibility | P1 | [#1170 NEXT-187 Mob AI停止中の非活性ChunkにCPU時間を費やさない](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1170) | Open · reproduce/compare existing code, then fix or document PASS |
| 174 | W4 Extended compatibility | P1 | [#1171 NEXT-188 保存処理とChunk生成の同時ピーク時のI/O待ち隔離](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1171) | Open · reproduce/compare existing code, then fix or document PASS |
| 175 | W4 Extended compatibility | P1 | [#1177 NEXT-194 WindowsとLinuxのProtocol 5ログを同一fixtureで比較](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1177) | Open · reproduce/compare existing code, then fix or document PASS |
| 176 | W4 Extended compatibility | P1 | [#1178 NEXT-195 実機クライアントとRust E2Eの差分を管理する台帳を自動生成](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1178) | Open · reproduce/compare existing code, then fix or document PASS |
| 177 | W4 Extended compatibility | P1 | [#1179 NEXT-196 Minecraft 1.7.10の純正サーバーとブロック操作を比較](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1179) | Open · reproduce/compare existing code, then fix or document PASS |
| 178 | W4 Extended compatibility | P1 | [#1180 NEXT-197 複数人での24時間放置後に再参加・保存の健全性を評価](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1180) | Open · reproduce/compare existing code, then fix or document PASS |
| 179 | W4 Extended compatibility | P1 | [#1181 NEXT-198 fuzz入力をProtocol状態別に分類して既知失敗を縮小する](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1181) | Open · reproduce/compare existing code, then fix or document PASS |
| 180 | W5 Operations & quality | P2 | [#1003 NEXT-020 Packet種別ごとの送受信バイト数を匿名集計する](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1003) | Open · reproduce/compare existing code, then fix or document PASS |
| 181 | W5 Operations & quality | P2 | [#1023 NEXT-040 Entity Metadata差分配信とフル同期を比較する計測](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1023) | Open · reproduce/compare existing code, then fix or document PASS |
| 182 | W5 Operations & quality | P2 | [#1033 NEXT-050 視距離別Chunk送信完了率を5秒区間で測定](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1033) | Open · reproduce/compare existing code, then fix or document PASS |
| 183 | W5 Operations & quality | P2 | [#1043 NEXT-060 本家ワールドとの差分をChunk単位ヒートマップで記録](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1043) | Open · reproduce/compare existing code, then fix or document PASS |
| 184 | W5 Operations & quality | P2 | [#1052 NEXT-069 降雨の開始終了通知と気象持続時間の統計比較](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1052) | Open · reproduce/compare existing code, then fix or document PASS |
| 185 | W5 Operations & quality | P2 | [#1063 NEXT-080 Redstone更新キューの遅延と循環打切り理由を記録](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1063) | Open · reproduce/compare existing code, then fix or document PASS |
| 186 | W5 Operations & quality | P2 | [#1073 NEXT-090 乗り物別のTick計算コストと配信パケットを比較](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1073) | Open · reproduce/compare existing code, then fix or document PASS |
| 187 | W5 Operations & quality | P2 | [#1083 NEXT-100 GUI操作系列をreplay可能なItemTransactionログとして記録](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1083) | Open · reproduce/compare existing code, then fix or document PASS |
| 188 | W5 Operations & quality | P2 | [#1093 NEXT-110 GUI種別ごとに実機スクリーンショットの受入台帳を作る](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1093) | Open · reproduce/compare existing code, then fix or document PASS |
| 189 | W5 Operations & quality | P2 | [#1103 NEXT-120 難易度変更による自然回復と飢餓ダメージ差の表を生成](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1103) | Open · reproduce/compare existing code, then fix or document PASS |
| 190 | W5 Operations & quality | P2 | [#1113 NEXT-130 難易度別に被ダメージ計算のゴールデンケースを生成](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1113) | Open · reproduce/compare existing code, then fix or document PASS |
| 191 | W5 Operations & quality | P2 | [#1123 NEXT-140 Mob AIタスク別の平均探索時間と失敗理由を計測](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1123) | Open · reproduce/compare existing code, then fix or document PASS |
| 192 | W5 Operations & quality | P2 | [#1133 NEXT-150 農業全種の骨粉効果を固定シードで確率比較](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1133) | Open · reproduce/compare existing code, then fix or document PASS |
| 193 | W5 Operations & quality | P2 | [#1143 NEXT-160 Overworld/Nether 8対1座標の丸め誤差一覧を固定化](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1143) | Open · reproduce/compare existing code, then fix or document PASS |
| 194 | W5 Operations & quality | P2 | [#1153 NEXT-170 IRW/Anvil変換前後のブロック・NBT差分レポート](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1153) | Open · reproduce/compare existing code, then fix or document PASS |
| 195 | W5 Operations & quality | P2 | [#1162 NEXT-179 初回セットアップで公開範囲・online-modeの安全診断を出す](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1162) | Open · reproduce/compare existing code, then fix or document PASS |
| 196 | W5 Operations & quality | P2 | [#1163 NEXT-180 操作履歴に管理者コマンドの成否と対象だけを記録](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1163) | Open · reproduce/compare existing code, then fix or document PASS |
| 197 | W5 Operations & quality | P2 | [#1172 NEXT-189 Rust allocator別の長時間メモリ断片化を検証](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1172) | Open · reproduce/compare existing code, then fix or document PASS |
| 198 | W5 Operations & quality | P2 | [#1173 NEXT-190 サーバー起動から最初のJoin完了までを段階測定](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1173) | Open · reproduce/compare existing code, then fix or document PASS |
| 199 | W5 Operations & quality | P2 | [#1182 NEXT-199 PRごとの変更対象要件IDと未実施実機検証を集計](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1182) | Open · reproduce/compare existing code, then fix or document PASS |
| 200 | W5 Operations & quality | P2 | [#1183 NEXT-200 既存Issueの完了判定に必要な証拠不足を一括抽出](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/1183) | Open · reproduce/compare existing code, then fix or document PASS |

## Required per-Issue report

- Baseline `main` SHA and relevant merged/open PR.
- Reproduction input (packet/seed/world snapshot), expected vanilla 1.7.10 result, actual result.
- Change made or reason unchanged; `cargo fmt --all -- --check`; targeted/full test and clippy outcome, all PASS/FAIL/NOT_RUN.
- Disposable world or backup policy, post-restart and multi-client observations where relevant.
- Final verdict: `fixed`, `already covered`, `duplicate`, `blocked`, or `unresolved`; close only when acceptance evidence is linked.
