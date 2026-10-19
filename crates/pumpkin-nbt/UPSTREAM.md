# Pumpkin NBT source provenance

- Upstream: https://github.com/pumpkin-mc/pumpkin
- Revision: `c7a234ab8a3b3296d68a5f40411995920e11528a` (2026-10-18 UTC)
- Original path: `crates/pumpkin-nbt/`
- Original license: GNU General Public License version 3 (GPLv3). See repository `LICENSE`.
- Copied Rust source files: `lib.rs`, `tag.rs`, `compound.rs`, `deserializer.rs`, `serializer.rs`, `nbt_compress.rs`.
- Changes: remove `nbt_ops` module dependency on Pumpkin's dynamic codec crate; use independently specified Cargo dependencies. Other copied Rust source remains verbatim.
- Integration: NBT binary data is format-compatible in principle, but the server's complete 1.7.10 world/player.dat compatibility is **not** yet established. Do not confuse NBT parsing with game/world compatibility.
- Copyright of copied Pumpkin source remains with the Pumpkin contributors. Preserve this notice and `LICENSE` upon redistribution.
