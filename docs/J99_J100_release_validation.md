# J99/J100: Windows/Linux local release validation and provenance

Status: **local validation required, release acceptance NOT_RUN until a recorded run succeeds.**

## Rust CI policy (mandatory)

**Do not start or rerun GitHub Actions Rust CI or the manual Rust release workflows.** This includes the `Windows and Linux verified binaries` and `Release binaries (Windows and Linux)` workflows. Their YAML files currently expose `workflow_dispatch` only. Creating a `v*` tag does **not** initiate these workflows; it is not a substitute for a local release build.

The source of truth is `AGENTS.md`, `docs/POLICY_RULES.md`, `docs/PROJECT_POLICIES.md` and `CONTRIBUTING.md`. A local build result does not automatically mean that a distributable artifact has been approved or published.

## Windows: PowerShell, repository root

Run using a checked-out, reviewed commit on the development PC. Do not use a production world for smoke or recovery tests.

```powershell
git rev-parse HEAD
rustc --version
cargo --version
.\verify-windows.ps1
cargo build --release -p vanilla-server --bins -p legacy-status-server
if ($LASTEXITCODE -ne 0) { throw "Release build failed" }
& ".\target\release\vanilla-server.exe" --help
if ($LASTEXITCODE -ne 0) { throw "Server --help smoke test failed" }
Get-FileHash -Algorithm SHA256 -LiteralPath ".\target\release\vanilla-server.exe"
Get-FileHash -Algorithm SHA256 -LiteralPath ".\target\release\legacy-status-server.exe"
```

The Windows verification wrapper builds `industrial-verify` and invokes it with `--automated-only` when no arguments are supplied. Read the generated `target/verification/` report and logs. Record each check as PASS, FAIL or NOT_RUN. A failed verifier or missing binary blocks release acceptance; it must not be silently bypassed.

## Linux: shell, repository root

```bash
git rev-parse HEAD
rustc --version
cargo --version
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p vanilla-server --bins -p legacy-status-server
./target/release/vanilla-server --help
sha256sum target/release/vanilla-server target/release/legacy-status-server
```

Use a separate **new, isolated test world and whitelist** for any live server, login or save/restore acceptance. The `--help` smoke test itself must not be interpreted as login/Protocol 5 compatibility.

## Required local provenance evidence

Record the tested Git commit SHA, OS/architecture, `rustc` version, `cargo` version, exact commands and exit codes, `Cargo.lock` hash if generated, executable SHA-256 hashes, and the path to the local verification report. Share only redacted evidence: do not publish account tokens, private keys, world contents, player identities or sensitive absolute paths.

A project-level `Cargo.lock` is not currently guaranteed to be committed. A lockfile created by one local build pins dependencies **for that build only**; a separate build on another day may resolve different versions. For reproducibility, retain a reviewed lockfile and dependency metadata with the local evidence, and do not claim bit-for-bit reproducibility without cross-machine comparison.

When the Rust toolchain or a test machine is unavailable, explicitly record **NOT_RUN**, not PASS. `cargo fmt`, workspace tests, Clippy, native binary smoke checks, real-client acceptance and world recovery evidence are separate gates.

## Scope limits and safe release operation

- Do **not** upload release binaries or publish GitHub Releases just because the commands above succeed. Release publication requires a separate explicit authorization and reviewed evidence.
- A green Rust TCP integration test does not establish compatibility with an **unmodified Minecraft Java 1.7.10 client**. See the E42 acceptance checklist under `docs/group-e/2026-10-21-E42-vanilla-client-e2e.md`.
- Backup and recovery verification must use **disposable copies**, never the original player world. Confirm files remain unchanged and failed recovery exits nonzero.
- Do not expose an offline-mode server to the Internet. Default to loopback; any LAN-based acceptance requires explicit authorization and appropriate firewall restrictions.
- This document does not reactivate or bypass any GitHub Actions workflow, required status check, branch protection or review requirement.

## Historical note

Older versions of this page described launching GitHub Actions release workflows or pushing `v*` tags. Those instructions are **obsolete under the current no-GitHub-Actions-Rust-CI policy**. A manually triggered workflow remains present in the repository but must not be used without an explicit policy change from the user.
