# Pumpkin参照とライセンス移行監査（2026-10-19）

## 現行ソースの状態

- IndustrialRust1710はRustでMinecraft Java Edition 1.7.10（Protocol 5）のサーバーを独立実装するプロジェクトです。
- Pumpkin（https://github.com/pumpkin-mc/pumpkin）は実装調査に使った参考リポジトリです。Pumpkin本体はGPLv3で、最新Minecraft向けの機能が多いため、通信・ワールドロジックのソースを1.7.10に直接コピーしません。
- **現行ブランチの`crates/pumpkin-nbt`のコピーは削除**し、NBT入出力は[fastnbt](https://github.com/owengage/fastnbt) v2.6.3（MIT OR Apache-2.0）に置き換えています。
- gzip圧縮、プレイヤーの`Pos`、移動境界検査、旧JSON形式の読み込みはプロジェクト側で構成しています。Minecraft 1.7.10の完全な`player.dat`、Anvil、サバイバル機能を保証するものではありません。
- 現行オリジナルコードのライセンスは未確定。第三者ライブラリの利用条件は別途順守します。

## 残る履歴の扱い（重要）

- **PR #6**（https://github.com/RyuuzinnRaika/IndustrialRust1710/pull/6）でPumpkin由来のNBTソースがGPLv3で取り込まれ、**PR #7**で通信テストの競合が修正されました。
- そのGitコミットおよび公開済みPRには、現行ツリーから削除したGPLv3のコードが**履歴として引き続き残ります**。
- 現行ツリーから削除しただけで、過去の配布に適用されるGPLv3の義務が消えたり、公開履歴が消去されるわけではありません。過去の公開コードについては当時のライセンスと著作権表示を尊重してください。
- 履歴の完全な再作成や強制pushは、既存のクローン/参照/PRに重大な影響があるため、**今回実行していません**。履歴の取り扱いは別途確認と明示的な判断が必要です。

## 将来の移植ルール

1. 仕様・振る舞いを公開プロトコル資料や独立資料で把握し、元コードを逐語/構造ごとに移植しない。
2. 第三者ライブラリ採用前にSPDX識別子、著作者表示、配布時の義務、間接依存を確認する。
3. 元がGPLv3なら、そのコードの直接コピーや派生実装を採用しない。利用する場合はGPL互換での配布条件を別途整理する。
4. 最新版とProtocol 5の差分をユニット・通信・ワールド互換性テストで検証する。
5. Minecraft/Forge/Modの再配布や実装互換性の保証を混同しない。

## 参照元

- Pumpkin調査時のupstream SHA：`c7a234ab8a3b3296d68a5f40411995920e11528a` （2026-10-18）
- fastnbt： https://github.com/owengage/fastnbt
