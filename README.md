# IndustrialRust1710

Rustネイティブ Minecraft Java Edition 1.7.10 (Protocol 5) サーバー研究開発。

**未完成**：現在はStatus/Ping応答、Loginの明示拒否、独立した工業ネットワーク計算、合成ベンチマークのみ。Minecraftワールド、ログイン/Play、Forge/FMLハンドシェイク、JAR互換実行、Java→Rust変換はまだありません。

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo run -p legacy-status-server -- --bind 127.0.0.1:25565
cargo run --release -p industrial-bench -- 10000 200
```

Forge Modをそのまま実行できると誤解しないよう、未実装機能はこの文書に記載します。ライセンスを確認せずMinecraft/Modのバイナリを配布しないでください。
