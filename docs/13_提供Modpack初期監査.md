# 提供Modpack初期監査（2026-10-19）

## 対象と監査手法

ユーザーから提供された `1.7.10modServerSkyBlock.zip` を読み取り専用で静的検査した。
内部のMod JAR/ZIPは実行せず、ファイル一覧、ZIPエントリ、`mcmod.info` と
`META-INF/MANIFEST.MF` を調査した。MinecraftやForge/Modの成果物・セーブデータは
このリポジトリへ**一切コピーしていない**。

- Minecraft: 1.7.10
- Forge本体: `forge-1.7.10-10.13.4.1614-1.7.10-universal.jar`
- Modアーカイブ: **39個**（35 JAR + 4 ZIP）
- Java `.class` エントリ: **15,398個**（単純合計、互換実行できる数ではない）
- `mcmod.info` エントリ: 31ファイル（JSON解析可能30、解析不可1）
- `FMLCorePlugin` 指定: 10ファイル（マニフェスト表記に基づく）

## 対象の主な工業化Mod

| 目的 | 実ファイルの例 | Rust側の次の検証対象 |
| --- | --- | --- |
| 電力/機械 | Mekanism 9.1.1.1031 / MekanismGenerators / MekanismTools | エネルギー・機械tick・保存 |
| 発電 | BigReactors 0.4.3A | エネルギー生成と構造状態 |
| 輸送 | BuildCraft 7.1.26 / LogisticsPipes 0.9.3.132 | パイプ経路・アイテム移動 |
| 自動化 | MineFactoryReloaded 2.8.1-174 | 機械動作・インベントリ |
| 配線 | ProjectRed 4.7.0pre12.95 シリーズ | 隣接ネットワーク・イベント |
| 鉄道 | Railcraft 9.12.2.0 | エンティティと状態永続化 |
| SkyBlock | Ex-Nihilo / Excompressum | 資源生成規則 |
| 補助基盤 | CodeChickenCore / CoFHCore / ForgeMultipart | CoreModとレジストリの互換性 |

JARの存在はModが正常起動したことの証明ではない。依存関係は
`@Mod(dependencies=...)` などに埋め込まれている場合があり、
`mcmod.info` だけでは正確に復元できない。最新の静的解析器は
これらを**未確認**として出力する。

## ログから判明した制約

添付の `logs/latest.log` において
`Failed to load eula.txt` とEULA同意待ちで停止した記録がある。
これは**保存されていたログの時点**の状態であり、現時点で正常起動できるか
どうかの判定ではない。
他のFMLログにも警告があるが、単独の断片でMod動作不能と断定しない。

## 実装と次の受け入れ試験

1. 今回: ZIPまたは展開済みサーバーのMod一覧とメタデータをJSONで収集。
   Mod JARを実行せず、ZIPの読み取りサイズに制限を設ける。
2. 次段階: Java `@Mod` アノテーションと依存指定を抽出し、Mod IDと
   FML handshakeに必要なMod一覧を検査する。
3. その後: RustネイティブのLogin/Play、最小ワールドとNBT読み込みを
   Goldenテスト付きで実装。既存ワールドは読み取り専用とする。
4. Forge/FML handshakeとRegistry/ModIdDataの互換性を
   同じModpackの実機クライアント/サーバーのトレースで比較する。
5. 工業化Mod機能は対象を1個ずつ選び、電力・機械・アイテムなどの
   保存則、同一tick条件、データ保存互換性を確認する。

この監査結果は**静的解析**であり、Minecraftログイン、Forge実行、
Mod互換の達成を意味しない。
