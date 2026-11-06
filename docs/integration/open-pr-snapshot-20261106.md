# 17 Open PRs — integration snapshot

**This branch is a disposable validation candidate, not a production merge or a release.**

main snapshot: `26cc4ec35530cc742a0f9ba3930cca8ef15eba42`

## Included GitHub PR heads
- #729: `eebf33db24524342d1281af29a6a290893cf30a2`
- #731: `44953244df25f0b81d4d1f0964101262b9541f79`
- #733: `afd0bb22a62971fa6cb1e170bfc08455f33f6a47`
- #734: `ba16ff52da0f11b0f4ebe4a72f2f830689f19e9b`
- #736: `a5c81db9f80dca94adc24fc869e4e8503d44ef6c`
- #837: `02d6c9ffac75dc5a6a6693fbaa66d554b7baee3b`
- #840: `d58ce8a5335fca1e317ceefc68c4a51aa6b15b22`
- #841: `91dbf74da149f5ce88f9b965d0c3aeb8cfc48d7d`
- #844: `d4ddcff17b2dc7da4347a70e956d346e17101dec`
- #846: `e44acd9f2fb0c8442263857c8614efeaf6a6f461`
- #849: `f86e7ccb8bc78ee88a53b079f677179cf0e94187`
- #851: `c6e409be7339980368746e2a2a4ec65c326aba07`
- #852: `c5eb6fa77338a29ca4c0f71be1a3a453da1e12d5`
- #853: `0ae2d0446596133ce86feb8fcfd25a9462bf0896`
- #855: `6b91086140f0fdf6e8be0256afdd8698c68c0cf2`
- #857: `de0b288ea118c733a6e10fb9cff90e9076257f5b`
- #858: `16dc898579f5c8a57207c831195d3d7a9f80559c`

## Overlapping files resolved for combined verification
- `apps/vanilla-server/src/main.rs`: #846, #852, #857, #729
- `apps/vanilla-server/src/server_config.rs`: #736, #729
- `apps/vanilla-server/tests/protocol5_e2e.rs`: #841, #857

Every other modified file was staged with the exact Git blob SHA from the corresponding PR head.
Full-context unified patch application was used for all hunks except a single context line omitted in each of two adjacent insertion hunks. The result is a candidate that still needs Rust compilation, format verification and integration tests.

## Local quality gates
Run on a disposable checkout, never a real saved world:
```powershell
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace --release
.\verify-windows.ps1
```

Real Windows/Linux Minecraft Java 1.7.10 compatibility, save recovery and fuzzing remain to be validated separately.
**NOT_RUN:** Rustfmt, cargo tests, Clippy, release build, actual Minecraft client and fuzz campaign.
GitHub Actions have not been invoked. Do not merge this candidate into main until validation evidence exists.
