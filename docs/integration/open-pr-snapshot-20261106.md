# 19 Open PRs — integration snapshot

**This branch is a disposable validation candidate, not a production merge or a release.**

main snapshot: `ef28c275bcda09b945b66aaff0e318a838c6cfb3`

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
- #855: `354cfd3318032f410f19e43cbef77d5cd5355197`
- #857: `0b95f3dde3b10f3fb5e2b32caeb70c3951a41607`
- #858: `bc1f42707d50799b7d02160f9210cfb9d8e08a92`

- #861 (stacked on #857): `51f827a537f5b565da3520a538381a84512dec65`
- #862: `917500cdca123188e82c0ea33c6cb70550cc103b`
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

## Reconciliation update — merged PR #859

Latest main at update: `ef28c275bcda09b945b66aaff0e318a838c6cfb3`.
PR #859 had three rustfmt hunks in `apps/vanilla-server/tests/protocol5_e2e.rs`. They have been applied to this integration branch while preserving both new Protocol 5 regression tests from PRs #841 and #857. GitHub comparison confirms 0 commits behind main, and no deletions in that test file versus main.
PRs #855, #857, and #858 incorporated the same new main formatting in their branch ancestry; the source-specific blobs in this integration candidate are unchanged.
**Rust validation on this integration branch is still NOT_RUN**. PR #859 describes Windows fmt/clippy success on its own branch, but its 2 failing TCP integration tests are not a success claim for this candidate.
Snapshot branch and PR heads may advance independently. Compare PR HEAD SHAs before taking a release or merge decision.

## 19-PR expanded staging (still NOT_RUN)

- #861 gravity/collision is stacked on #857, and **cannot be merged before its parent**. Added the 20TPS worker in `main.rs`, its `ground_items.rs` source and `docs/23_ground_item_gravity_collision_20261106.md`.
- #862 note-block support material mapping was merged into `decorations.rs` without dropping #857's `contains_position` and `staged_sidecar_removal_detects_all_decorations_without_publishing` tests.
- #861 `protocol5_e2e.rs` contains its new gravity E2E test and #857's mining tests; #841's REG-123 Status Ping test was reinserted exactly. The earlier rustfmt #859 changes remain present.
- New changes have only been checked for scoped code overlap and marker preservation. **Cargo tests, rustfmt, Clippy, crash injection, 1.7.10 client, and physics load tests remain NOT_RUN.**
- `cargo test -p vanilla-server falling_item_ -- --nocapture` and `cargo test -p vanilla-server --bin vanilla-server add059_ -- --nocapture` are targeted additional gates.
- Do not merge the stacked #861 to main, or close #856/#483, until both parent tests and these new tests pass.
