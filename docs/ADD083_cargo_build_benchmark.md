# ADD-083 / Issue #508 — isolated Cargo release-build benchmark

This is a **local verification-only** Python utility, not a Rust server feature.
It never launches GitHub Actions, runs cargo clean, removes worlds, or modifies
an existing Cargo build directory. Rust product runtime remains Rust-only.

## Windows PowerShell

From the repository root, with Python 3.10+ and Rust stable installed:

```powershell
python tools/benchmark_cargo_builds.py --output-dir .\target\bench-20261110-first

# Repeat on the same hardware/toolchain using a fresh output directory:
python tools/benchmark_cargo_builds.py --output-dir .\target\bench-20261110-second --baseline .\target\bench-20261110-first\report.json
```

On Linux, replace the paths with \`./target/bench-first\` and
\`./target/bench-second\`; otherwise the command is identical.

Each run creates a **new** Cargo target directory within the requested output
folder. The first \`cargo build --release -p vanilla-server -p legacy-status-server\`
is a cold build in that directory. The second identical command uses the same
directory as a warm/incremental build. It records:
- commit SHA (if Git is available), cargo/rustc version, OS/CPU architecture
- cold/warm elapsed milliseconds, exit status
- SHA-256 and byte size of both release executables at each phase
- a baseline comparison where an individually configurable maximum ratio
  (default 1.25) flags a performance regression in either build

The report is \`report.json\`; the corresponding detailed local execution
logs are \`cold.log\` and \`warm.log\`. **Review these logs for sensitive
paths/content before sharing them.** The JSON contains structured metadata,
not raw log text or local absolute paths.

The baseline must be produced on the same system, architecture and Rust
toolchain; incompatible metadata causes \`INCOMPARABLE\` (exit code 2)
instead of a misleading pass. There is no absolute time threshold, because
a CPU-specific measured baseline is required. Results are timing samples,
**not** an assertion that an implementation is fast or Minecraft-compatible.

Exit codes: 0 = successful measurement (and optional comparison PASS),
1 = build failure, missing binary, changed warm binary or a regression above
the configured ratio, 2 = invalid output/baseline/setup. An absent baseline
is explicitly \`NOT_RUN\`, not \`PASS\`.

Before closing #508, collect at least three measured runs on both Windows
and Linux with consistent power, machine load, Rust version and commit SHA;
choose a recorded initial baseline and explain expected variance. Also run
the separate local quality gates:

```bash
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p vanilla-server -p legacy-status-server
```

These commands have **not** been executed by this benchmark addition.
No GitHub Actions Rust CI is to be started under the repository's policy.

Regression unit tests for the benchmark itself:

```bash
python -m unittest discover -s tools -p 'test_benchmark_cargo_builds.py' -v
```

The fixture runs a fake Cargo executable on Unix; this tests isolation and
reporting without pretending to have compiled the Rust project.
