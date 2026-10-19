# Mod静的解析 PoC

`cargo run -p mod-analyzer -- path/to/mod.jar`

JARを実行せず、Java classのバージョン・定数プールのForge/FML参照・CoreModマニフェスト・mcmod.info有無を検査します。これでMod互換や変換可能性を保証するものではありません。サイズ制限と不正Class検出を実装しています。Minecraft本体・Modの資産はリポジトリに含めません。
