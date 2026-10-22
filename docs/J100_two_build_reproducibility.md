# J100 — independent clean-build comparison

Use GitHub Actions → **J100 two-build artifact reproducibility check** → **Run workflow**.
On both Linux and Windows runners the workflow generates one `Cargo.lock`, builds two server binaries, runs `cargo clean`, rebuilds with `--locked` and fails if their SHA256 digests differ. An audit artifact contains the two checksum lists, source SHA, Rust/Cargo version and locked dependency graph.

This is a **single-run, same-host reproducibility smoke check**, not proof that every other runner can reproduce the binaries. The current repository does not commit `Cargo.lock` and does not pin stable Rust or the GitHub runner image. The generated lockfile and compiler version are recorded for audit, but cross-run bit-for-bit reproducibility is not guaranteed. Independently repeat on a separate controlled host, pin action digests and exact toolchain, and commit/review the lockfile before claiming complete J100 compliance. Source commit checks and binary hashes cannot replace release signing or malware inspection.

Workflows are **manual** to limit GitHub Actions runtime and quota; the ordinary PR CI remains unchanged.
