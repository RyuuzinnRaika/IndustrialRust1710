# Third-party dependency notices

This project does not vendor Pumpkin's GPLv3 NBT source in the current source tree.

## fastnbt

- Project: https://github.com/owengage/fastnbt
- Dependency: `fastnbt = "=2.6.3"`
- License: **MIT OR Apache-2.0** (license choice by applicable terms).
- Purpose: Rust Java Edition NBT encoding/decoding using documented Serde APIs.
- Original code remains copyrighted by its authors. No fastnbt source files are copied into this repository; Cargo resolves the external package.
- Redistributors must preserve applicable copyright/license notices and check the resolved dependency tree for additional notice obligations.

## Other dependencies

Rust/Cargo transitive and direct dependencies (including serde, flate2 and md5) retain their own licenses. Before distribution, generate a locked dependency audit (`cargo metadata --format-version 1` or `cargo license`) and review all entries.

## Historic upstream

Pumpkin is **GPLv3**: https://github.com/pumpkin-mc/pumpkin. A previous commit imported its NBT source in PR #6. That code was removed from the *current source tree*; published Git history was not rewritten. See `docs/15_Pumpkin導入監査.md` for the historical distinction.
