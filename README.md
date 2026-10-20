## 2026-10-20: かまどのGUI・精錬・燃料・20TPS駆動（限定実装）

`vanilla-server` で、バニラ Java Edition 1.7.10 / Protocol 5向けの3スロットかまどを追加。ブロックID61を設置して右クリックすると `minecraft:furnace` のウィンドウ（入力0、燃料1、出力2、プレイヤー3〜38）を開きます。製錬結果の取り出し・素材・燃料の投入は**サーバー管理**のインベントリ操作を介して行います。出力スロットへアイテムを投入する操作や不正なウィンドウIDは拒否します。

- Clock（20Hz）を共有する独立ワールド処理スレッドで、プレイヤーがいない間も製錬を進めます。**1アイテム200tick=約10秒**です。終了したサーバーの経過時間を起動時に遡及計算する機能はありません。
- 例：鉄鉱石→鉄インゴット、金鉱石→金インゴット、砂→ガラス、丸石→石、原木→木炭、生肉→焼肉、粘土→レンガなど。**すべてのバニラ1.7.10レシピではなく限定セット**です。
- 燃料：石炭・木炭1600tick、石炭ブロック16000tick、木材300tick、棒100tick、溶岩バケツ20000tick（使用後に空バケツを残す）など、1.7.10相当の限定セット。燃料不足・出力満杯時は次の燃料を消費しません。
- Window Properties（クライアント受信`0x31`）に製錬進捗（0）、燃焼残量（1）、燃料の基準燃焼時間（2）を通知。稼働中はID61→62、停止で62→61の点灯状態をワールド編集・共有ブロック更新へ反映します。
- ワールド単位の`*.furnaces.json`に材料・燃料・完成品・燃焼残量・製錬進捗を保存（原子的な単一ファイル置換）。燃料消費・完成品生成は**同一ファイル**で即時確定し、それ以外の進捗は最大約1秒ごとにチェックポイント保存します。再起動時に状態を復元します。最大512かまど / 256KiB。
- 複数人が同じかまどを開いた場合、共通の状態を共有します。プレイヤーインベントリは別のファイルなので、**異常終了の境界ではアイテム消失リスク**が残ります。入力/燃料の追加ではプレイヤー側を先に、出力/残材の取得ではかまど側を先に保存することで単純な複製を避けます。
- **未対応**: バニラ全レシピ・全燃料の網羅、経験値払い出し、ホッパー/自動搬入、かまどの破壊ドロップ、一般的なAnvil TileEntity保存、実Minecraft 1.7.10 GUIによる受入検証。内容や燃焼時間が残るかまどは安全のため破壊できません。通常Survival全体は未完成です。

## 2026-10-20: 単体チェスト・共有保管（限定実装）

- チェストID54の設置・右クリックで1.7.10の`minecraft:chest`ウィンドウを開き、チェスト27 + プレイヤー36 = 63スロットを同期。
- サーバー側で左・右クリック、Shift移動、プレイヤー領域内の数字キー入れ替えを判定。クライアントのアイテム申告は信用しません。
- チェストの中身はワールドに付随する`<world名>.chests.json`（例`world/flat.chests.json`）に原子的な単一ファイル置換で保存し、再起動時に再読込。512チェスト・400KiBの制限を設定。
- ロックを共有して複数人の同時更新を直列化し、変更した内容を開いている他プレイヤーに再送。
- **安全上の制限**: チェストとカーソルの異なる種類のアイテム交換、チェストとホットバーの数字キー交換、2連チェスト、アイテム入りチェストの破壊は禁止。チェストとプレイヤーの保存先は別ファイルなので、異常終了時の**アイテム消失リスクは残る**（複製防止のため出所を先に保存）。チェストのAnvil TileEntityへの書き出し・インポートは未対応です。
- チェストはGUIと保存が対象。かまど/ホッパー/チェストの物理・赤石・ブロックエンティティ更新は未対応。これだけでは通常のバニラSurvival完成とはみなしません。

## 2026-10-20: クラフト機能（限定実装）

- プレイヤーの2×2クラフト入力（Window 0、1〜4）と作業台の3×3（専用ウィンドウ1〜9）をサーバーが保持します。
- 初期サバイバル向けレシピ：原木→木材、木材→棒、作業台、チェスト、かまど、木/石の剣・ツルハシ・シャベル・斧・クワ。
- 完成品スロット（0）はレシピから毎回計算し、取得時に**入力を1個ずつ消費**します。クライアントの「完成品を持っている」という申告は採用しません。
- 作業台（ブロックID 58）の設置と右クリックでProtocol 5のGUIが開きます。Crafting Tableウィンドウ（0x2D）、Window Items（0x30）、Confirm Transaction（0x32）に対応。
- クラフト中の素材は別セーブではなく、サーバーが管理する従来のプレイヤーインベントリJSONに保存します。作業台を閉じても素材はこの試験版では**保持**され、再入場後に再利用できます（バニラと挙動が異なります）。
- **未対応**: すべてのレシピ/道具耐久・NBT・防具・クラフト結果の連続Shift大量生産（現状は1回ずつ）・ドラッグ操作・材料が容器を返すレシピ、チェスト/かまどの実機能とGUI、重力/Mob/戦闘/地形生成/Anvil完全対応。
- バニラ1.7.10クライアントの実機GUI確認は別途必要です。Rust製ソケット試験と実機は区別します。

## 2026-10-20: インベントリの通常クリック操作

Protocol 5 のプレイヤー用Window 0で通常/右/Shiftクリックとホットバー数字キーによる移動を限定的に追加しました。カーソル上のスタックもサーバー側で所有・保存し、偽装したクリックパケットからアイテムを作らない設計です。クラフト、防具、チェスト、アイテム投棄、ドラッグ配置、Mob/物理等は依然未完成で、**通常のMinecraft 1.7.10 Survivalが完成したわけではありません**。実機クライアント検証も必要です。

## SurvivalのアイテムEntity（試験版）

`--survival`モードの採掘物は、従来の「掘ると直接インベントリへ追加」から変更し、**地面のアイテムEntity**として生成するようになりました。Minecraft Java 1.7.10 / Protocol 5のSpawn Object (`0x0E`)、Entity Metadata (`0x1C`)、Collect Item (`0x0D`)、Destroy Entities (`0x13`)を使用します。

- 近づいてから約750ms以上経ったアイテムを拾得し、インベントリに入れます。
- 最大512個まで生成。地面アイテムをワールド本体とは別の`*.drops.json`ファイルに保存し、再起動後に復元します。
- 複数プレイヤーが同時に拾得しても同一IDを2回取得しないようサーバーが一元管理します。
- **未対応**: アイテム物理/重力/合体/消滅時間/投げ捨て/チェスト/全アイテム種類、アイテムごとの真の拾得当たり判定。ワールドとアイテムとインベントリの3ファイル更新は完全な原子トランザクションではないため、クラッシュ時のごく一部のアイテム損失があり得ます（複製リスクより安全を優先）。

## Rust Survival試験モード（限定）

通常のCreative PoCを残しつつ、**`--survival`を明示したときだけ**生存モード（Protocol 5 gamemode 0）でログインします。

```bash
cargo run --release -p vanilla-server -- --survival --whitelist whitelist.json --world ./world/flat.irw
```

草・土・砂・砂利・石・丸石・木材の一部だけについて、手で掘る開始/終了時間の検査と簡易採掘物の直接インベントリ加算を行います。ブロック設置では選択中のアイテムを必ず消費します。Creativeで取得したアイテムとSurvival取得アイテムは**別の保存ファイル**に分離し、クリエイティブ用パケットをSurvival中に送ってもアイテムは作られません。

**限定実装**：採掘速度は未完成の暫定値、アイテムはドロップEntityとして出現せず直接インベントリへ加算。クラフトGUI・アイテムクリック/移動・道具耐久・空腹・Mob・重力/ダメージ・ワールド生成は未実装です。「普通のSurvivalが完成した」という意味ではありません。

# IndustrialRust1710 — Rustのみで実装するMinecraft Java 1.7.10サーバー

このプロジェクトは **RustネイティブのMinecraft Java Edition 1.7.10 / Protocol 5サーバー**を開発します。**公式Javaサーバーを起動するラッパーやPython製の運用・テストプログラムは、現行ソースから廃止しました。** ゲーム本体と自動通信テストはRustです。Mod/Forge互換機能は後回しにしています。

### 現時点で動くRustバイナリ

```powershell
git clone https://github.com/RyuuzinnRaika/IndustrialRust1710.git
cd IndustrialRust1710
Copy-Item config/whitelist.example.json whitelist.json
notepad whitelist.json
cargo run --release -p vanilla-server -- --whitelist whitelist.json
```

現在はlocalhost (`127.0.0.1:25565`) のみで待ち受けます。接続には **Minecraft Java Edition 1.7.10のModなしクライアント**を使用します。ユーザー名ホワイトリストに登録した名前で接続してください。ホワイトリストは名前照合のみで、**Mojang/Microsoftアカウントの本人認証ではありません**。公開インターネットへの接続を許可しないでください。

**完成状況を誤認しないでください。** Rust版は最大8人・フラットワールドのCreative操作・45スロットのサーバー管理インベントリとホットバー選択/保存・チャンク配信・チャット・座標保存・限定Anvilエクスポート/インポートまでを自動検証しています。**サバイバル、フルインベントリ機能（防具/チェスト/カーソル操作/エンチャントNBT）、クラフト、Mob、AI、物理、レッドストーン、完全なAnvil常時保存は未実装**であり、現時点では通常のバニラMinecraftを完全に遊べません。実際の1.7.10クライアントによる総合操作検証も未実施です。

### Rustのみのビルド・通信テスト

```bash
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p vanilla-server
```

`apps/vanilla-server/tests/protocol5_e2e.rs` と `apps/legacy-status-server/tests/protocol5.rs` は、**Rustで実際のローカルTCP接続・二人同時ログイン・パケット分割・ワールド保存・Anvil往復変換**を検証します。Java・Pythonのランタイム、MinecraftサーバーJAR、EULAの自動同意は不要です。

**Minecraft Java Edition 1.7.10（Protocol 5）向けRustネイティブサーバーの研究開発プロジェクト。**

> 現行のソースツリーにはPumpkin由来のGPLv3実装を含みません。現状は開発用PoCです。バニラ1.7.10向けLogin/Playの基礎は追加済みですが、完成したMinecraftサーバーではありません。Mod JAR実行・Java→Rust変換は今は対象外です。

## 新規: Rustネイティブのバニラ最小サーバー

ModとConfig処理は後回しにし、Java Edition 1.7.10 **バニラクライアント専用**でLogin/Play・移動に応じた5×5チャンク配信・Creative設置/破壊・専用形式への保存を実装中です。`vanilla-server` はStatus専用の `legacy-status-server` とは別バイナリです。

~~~bash
cp config/whitelist.example.json whitelist.json
# whitelist.json の name を参加するプレイヤー名に変更
cargo run --release -p vanilla-server -- --bind 127.0.0.1:25565 --world ./world/flat.irw --whitelist ./whitelist.json
~~~

ユーザー名ホワイトリスト方式・最大8人・既存ワールド非互換の**試作**です（本人確認なし）。詳細と未実装範囲: [バニラ最小サーバー](docs/14_バニラ最小サーバー.md)。

## Anvil形式への安全な書き出し（試験版）

ゲーム本体の保存形式は引き続き独自`IRW1`です。Anvil出力は**既存ワールドを上書きしない別のスナップショット**です。

```bash
cargo run --release -p vanilla-server -- --world ./world/flat.irw --export-anvil ./exported-vanilla-world
```

出力先のフォルダーがすでに存在する場合は安全のため失敗します。生成範囲は初期5×5列＋変更のあるチャンク列（上限256）です。これだけでバニラ1.7.10の完全なワールド互換やサバイバル動作が保証されるわけではありません。

### 書き出したスナップショットの読み戻し

独立した変換処理で、**このサーバーが生成したフラットワールドのAnvilスナップショットのみ**を新規IRW1ファイルへ読み戻せます。元の世界や既存の保存先は上書きしません。

```bash
cargo run --release -p vanilla-server -- --import-anvil ./exported-vanilla-world --world ./world/restored.irw
```

他のMinecraftワールドやForgeワールドのAnvilファイルを汎用的に読み込む機能ではありません。エンティティ/タイルエンティティ・未知のブロック/不正なチャンクは安全のため取り込みを拒否します。

## 現時点で動作する機能

| コンポーネント | 機能 | 限界 |
|---|---|---|
| `legacy-protocol` | VarInt、フレーム・文字列の境界チェック | Play/FMLは未対応 |
| `legacy-status-server` | 1.7.10クライアントのサーバー一覧向けStatus/Ping | Loginは明示拒否 |
| `vanilla-server` | ホワイトリスト入場判定、20Hz Tick時計、Login/Play、平坦ワールドの動的チャンク配信、限定Creative操作、45スロットのCreativeインベントリ/選択ホットバー・独立保存、ブロック保存とgzip NBTプレイヤー位置保存、独立Anvil書き出し/限定読み戻し、8人までの同期・共有チャット | 本人確認なし、既存ワールド非互換、サバイバルなし |
| `industrial-core` | 決定的Tick、接続グラフキャッシュ、エネルギー配分 | Minecraftゲーム本体とは未接続 |
| `industrial-bench` | 仮想エネルギー網の合成ベンチマーク | 実ワールドのTPSではない |
| `mod-analyzer` | JAR・ClassFile解析、Modpack ZIP/フォルダのJSON監査 | Modの実行・変換・互換性判定ではない |

## 準備と検証

Rustのstableツールチェーンが必要です。

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings

# Status専用サーバー。localhost:25565に待ち受けます。
cargo run -p legacy-status-server -- --bind 127.0.0.1:25565

# 別のターミナルから実行
cargo test --workspace --all-targets

# 仮想工場ネットワークの合成ベンチマーク
cargo run --release -p industrial-bench -- 10000 200

# Modを実行せずにJAR内部を読み取る
cargo run -p mod-analyzer -- /path/to/mod.jar

# 1.7.10サーバーのZIPを展開せずに読み取り専用監査
cargo run -p mod-analyzer -- --modpack /path/to/1.7.10modServerSkyBlock.zip --json modpack-report.json

# 展開済みのサーバーディレクトリ (mods/ と mods/1.7.10/ を探す)
cargo run -p mod-analyzer -- --modpack /path/to/server --json modpack-report.json
```

GitHub ActionsではRustフォーマット・全単体/統合テスト・Clippy・releaseビルドを実行し、Rust製の実TCPクライアントでStatus/Login/Playを検証します。旧Forge 1.7.10の実機クライアントによる接続テストは未実施です。

解析レポートにはMod ID（mcmod.info記載分）、CoreModのmanifest設定、参考となるクラス数、Forge/FML参照と読み取りエラーを保存します。**@ModアノテーションからのID・依存解決、Mod実行、JavaクラスのRust変換、Forge接続の実動作は未対応**です。ユーザー所有のMod/ワールドのJAR等をGitに追加しないでください。

## 実装ロードマップ

1. 1.7.10 Vanillaのログイン・ホワイトリスト・Play通信を安定化（進行中）
2. 20TPSワールドロジック、チャンク生成・保存、NBT/Anvilを実装
3. 複数プレイヤー・インベントリ・クラフト・サバイバル・Mob・物理・レッドストーンを順次実装
4. 1.7.10バニラクライアントの実機総合テストと耐久テスト
5. **Mod/Forge/Config、Java→Rustの変換はユーザー指定により保留**

Mod互換を広く保証することはできません。CoreMod、リフレクション、Javaクラス変換、Forgeライフサイクル等の互換性は検証と実装が必要です。

## ドキュメント

- [要件定義](docs/01_要件定義.md)
- [Mod静的解析PoC](docs/12_Mod静的解析PoC.md)
- [提供Modpack初期監査](docs/13_提供Modpack初期監査.md)

## ライセンスと著作権

**現行のソースツリーでは、PumpkinからコピーしたGPLv3コードを除去済みです。**
NBTの入出力は [fastnbt](https://github.com/owengage/fastnbt)（**MIT OR Apache-2.0**、Cargoで取得する外部依存）と、プロジェクト固有の連携処理を利用します。
利用している第三者ライブラリのライセンスは [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) に記録します。リポジトリの新規オリジナルコード全体のライセンスはまだ確定していません。

重要: **過去のGitコミット/PRにはPumpkin由来GPLv3コードが残っています。** 今回の通常の削除コミットで過去の配布履歴やライセンス義務まで消えたわけではありません。現在のソースとGit履歴を混同しないこと。状況は [ライセンス・移行監査](docs/15_Pumpkin導入監査.md) を参照してください。

Minecraft/Forge/既存ModのJARおよびゲームアセットはコピーしません。
