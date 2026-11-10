# ADD-089 / Issue #514 — independent automated and client-acceptance gates

The existing Rust \`industrial-verify\` local runner records separate checks for
rustfmt, Rust unit/integration/TCP tests, Clippy, release binaries, smoke tests,
Python verification tests and the human-observed E42 real-client evidence.
When run with \`--automated-only\` it can exit 0 while E42 is **NOT_RUN**.
This is intentional for automation, but is **not** a complete acceptance pass.

This verification-only Python tool evaluates an existing local
\`industrial-verify\` report without rerunning Rust, starting GitHub Actions,
opening Minecraft, or modifying worlds:

```powershell
# First run a local verifier (no GitHub Rust CI).
.\verify-windows.ps1

# Replace <run> with the actual run directory created by the verifier.
python tools/check_verification_gates.py --report "target/verification/<run>/report.json" --require automated
python tools/check_verification_gates.py --report "target/verification/<run>/report.json" --require client_e42
python tools/check_verification_gates.py --report "target/verification/<run>/report.json" --require all
```

The \`--require all\` check is intentionally strict and **will not pass**
when the E42 evidence has not been supplied, even if all Rust/TCP tests PASS.
The \`--require automated\` check uses only the six automated checks;
the \`--require client_e42\` check uses the E42 result only. Every check is
traceable to a 40-character source commit SHA.

Exit codes: \`0=PASS\`, \`1=FAIL\`, \`2=NOT_RUN or invalid/missing evidence\`.
Missing, duplicate, unknown or malformed check rows cannot be silently
promoted into success. The report is locally generated and self-reported;
this checker does **not** independently attest the authenticity of manual
Minecraft 1.7.10 observations.

Test only the gate logic without Cargo:

```bash
python -m unittest discover -s tools -p 'test_check_verification_gates.py' -v
```

**This is a local gate evaluation utility, not a change to the disabled
GitHub Actions Rust CI.** The workflow continues to be manual-only under
AGENTS.md, and no Rust workflow should be started or rerun. Issue #514
requires future local Rust logs, actual E42 client evidence and a
review of any separately configured GitHub required checks before it can
be considered fully accepted.
