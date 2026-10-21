# D31 段階実装: Vanilla 1.7.10 アイテムIDとメタデータの厳格化

作成日: 2026-10-21。比較対象はアップロードされた Java 1.7.10 サーバー JAR（SHA-256 `c70870f00c4024d829e154f7e5f4e885b02dd87991726a3308d81f513972f3fc`）。

## 今回実装した範囲

- `items_1710.rs` に1.7.10の通常ブロック・通常アイテム・馬アイテム・レコードの識別範囲を設置。
- 1.8以降に追加されたIDや 4095 等、従来の `1..=4095` 判定で誤認していたものを拒否。
- 看板/エンダーパール/雪玉などの16個制限、装備・道具・ポーションなどの1個制限を共通化。
- アイテムの耐久値と metadata（染料、魚、ポーション、スポーンエッグ、地図等）を区別して検査。
- 既存の `Stack::max_count()` と `Stack::is_valid()` から一元参照。保存/受信・移動に既存の検証を適用。
- ID範囲・スタック量・メタデータ・耐久の境界テストを追加。

## 注意・未完了

**D31全体は未完了。特にNBT関連は今回変更していない。**

- Vanilla 1.7.10には内部用ブロックIDや異なるitem registrationがある。現段階の `1..=175` は互換性を保つための範囲判定であり、全番号がプレイヤー用アイテムとして取得可能な意味ではない。
- `Stack { id,count,damage }` はまだ `tag`、`display`、`ench`、`RepairCost`、`AttributeModifiers` を持たない。
- Protocol 5 Slotは、受信時NBTを拒否し、送信時は `-1` を使用する旧動作のまま。
- フルNBT対応には `Stack` の所有権設計とシリアライズ形式、Slotの双方向Codec、Player/Chest/Hopper/Furnace/Brewer/GroundItemの後方互換移行が必要。
- 既存の保存ファイルに範囲外IDが含まれている場合、fail-closedで読み込みを拒否する。勝手に削除することはない。Modded world非対応。

## 回帰試験

```bash
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace --release
```

手動受入: 新規Vanillaワールド、古い保存データ、木・石・鉄・ダイヤ道具の耐久、染料/魚/ポーションのmetadata、クリエイティブ入力とチェスト/かまど/醸造/ホッパーを検証すること。

## 続く実装順

D31 NBTの安全なモデル変更 → Protocol 5双方向Codec → すべての保存/移送 → D32全クリック操作 → D33全レシピ。
