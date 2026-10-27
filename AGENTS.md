# IndustrialRust1710 — repository-wide development instructions

## Language policy (mandatory)

**Product implementation and runtime behavior must be written in Rust only (`.rs`).**
This includes the Minecraft 1.7.10 server, protocols, chunk loading/sending, world generation and persistence, gameplay, Mob AI, commands, player sessions, administrator GUI, background maintenance/process supervision, and production-facing utilities.

**Python is permitted only for separate verification programs.** Examples: unit/integration/regression tests, client acceptance-test harnesses, load benchmarks, reproducibility checks, audit/check tools, test fixtures, and offline restore *verification*. Python must not implement new live server/production features, run as a required part of the shipping server, or become the implementation language of a runtime supervisor or administrator GUI. Prefer Rust unit and integration tests for Rust behavior; Python may be used as an external test driver.

Shell/PowerShell wrappers may perform minimal platform setup, environment checks, build, and launch steps. Do not put Minecraft rules, world mutations, security-sensitive business logic, or persistent runtime supervisors in such wrappers. Existing Java oracle files are for development verification only; no Java-based server implementation.

## Required behavior for developers and agents

1. Before changing a feature, implement the feature in the appropriate Rust crate under `apps/` or `crates/` (or in a new Rust crate). Do not create a Python substitute for missing runtime functionality.
2. Place any new Python source in `tools/`, and name it clearly as a test, verifier, acceptance tool, audit/check, or benchmark; no new Python files under `apps/`, `crates/`, or `scripts/`.
3. Keep optional Python checks out of runtime startup paths. CI may invoke Python to verify the Rust program.
4. Check `cargo fmt --all -- --check`, `cargo test --workspace --all-targets`, and `cargo clippy --workspace --all-targets -- -D warnings` where possible; explicitly report unrun tests and client/Windows-only verification.
5. When reviewing a pull request, reject new Python/C#/Java-based product implementations, even if they appear in `tools/` or are called from a launcher.

## Existing migration debt — do not silently expand

As of 2026-10-27, `tools/maintenance_supervisor.py` is an existing Python **runtime/process supervisor**, `scripts/prune_merged_pr_branches.py` is an existing Python **repository operations tool**, and `tools/ServerGui.cs` is an existing C# **Windows administrator GUI**. They predate this rule and are **not** examples of permitted new implementations. Preserve their behavior until safely replaced by tested Rust implementations; do not add new product functionality to them. Track migration in [issue #635](https://github.com/RyuuzinnRaika/IndustrialRust1710/issues/635).

`tools/GenLayer*Oracle.java` files are reference/test oracles, not runtime implementations. Existing Python files for acceptance tests, benchmarks, and restore verification remain allowed.

The `tools/test_language_policy.py` check is executed by the existing Python-test GitHub Actions workflow. It prevents newly tracked noncompliant source paths while migration debt remains explicitly enumerated.
