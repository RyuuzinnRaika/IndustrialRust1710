#!/usr/bin/env python3
"""Offline, allowlisted summary of industrial-verify's local report.json.

Never reads raw logs and never starts GitHub Actions or the server. Review the
result manually before uploading it: identifier-like strings may still be private.
"""
import argparse
import json
import re
from pathlib import Path

MAX_REPORT_BYTES = 2_000_000
SHA = re.compile(r"[0-9a-fA-F]{40}\Z")
IDENTIFIER = re.compile(r"[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*\Z")
SEED = re.compile(r"(?:[0-9]{1,20}|0x[0-9a-fA-F]{1,16}|[0-9a-fA-F]{6,32}(?:-[0-9a-fA-F]{1,16})?)\Z")
COMMANDS = {
    "rustfmt": "cargo fmt --all -- --check",
    "Rust unit/integration/Protocol 5 TCP": "cargo test --workspace --all-targets",
    "Clippy strict": "cargo clippy --workspace --all-targets -- -D warnings",
    "Release server binaries": "cargo build --release -p vanilla-server -p legacy-status-server",
    "Server --help smoke": "cargo run --release -p vanilla-server --bin vanilla-server -- --help",
    "Python verifier unit tests": "python -m unittest discover -s tools -p test_*.py -v",
    "E42 client evidence validation": "python tools/e42_acceptance.py verify --report <report> --evidence-root <dir> --require-pass",
}
VALID_STATUSES = {"PASS", "FAIL", "NOT_RUN"}


def export_report(data: object) -> dict:
    """Accept only structured, bounded fields; do not forward log paths/notes."""
    if not isinstance(data, dict) or data.get("schema_version") != 1:
        raise ValueError("Expected industrial-verify schema_version=1")
    raw_checks = data.get("checks")
    if not isinstance(raw_checks, list) or len(raw_checks) > 32:
        raise ValueError("Expected at most 32 verification checks")
    commit = data.get("commit")
    if not isinstance(commit, str) or not SHA.fullmatch(commit):
        commit = "unknown"
    checks = []
    seen = set()
    for raw in raw_checks:
        if not isinstance(raw, dict):
            raise ValueError("Invalid check")
        name, status = raw.get("name"), raw.get("status")
        if (not isinstance(name, str) or not isinstance(status, str)
                or name not in COMMANDS or name in seen or status not in VALID_STATUSES):
            raise ValueError("Unknown, duplicate, or invalid verification check")
        seen.add(name)
        code = raw.get("exit_code")
        if code is not None and (type(code) is not int or not 0 <= code <= 255):
            raise ValueError("Invalid exit code")
        if status == "PASS" and code != 0:
            raise ValueError("PASS requires exit code 0")
        tests = raw.get("failed_tests", [])
        if not isinstance(tests, list):
            raise ValueError("Invalid failed_tests")
        test_names = [s for s in tests[:32]
                      if isinstance(s, str) and len(s) <= 160 and IDENTIFIER.fullmatch(s)]
        seed = raw.get("seed")
        if not isinstance(seed, str) or not SEED.fullmatch(seed):
            seed = None
        checks.append({
            "name": name, "status": status, "exit_code": code,
            "reproduce": COMMANDS[name], "failed_tests": test_names,
            "test_seed": seed,
        })
    os_name = data.get("host_os")
    arch = data.get("host_arch")
    if not isinstance(os_name, str) or os_name not in {"linux", "windows", "macos", "freebsd", "unknown"}:
        os_name = "unknown"
    if not isinstance(arch, str) or arch not in {"x86", "x86_64", "aarch64", "arm", "unknown"}:
        arch = "unknown"
    return {
        "schema_version": 1, "source": "local industrial-verify report.json",
        "self_reported_not_independently_verified": True,
        "commit": commit, "host_os": os_name, "host_arch": arch,
        "checks": checks,
        "warning": "Raw stdout/stderr and filesystem paths are excluded. Review test names and seeds before sharing. NOT_RUN is not PASS.",
    }


def write_export(source: Path, destination: Path) -> None:
    if source.resolve() == destination.resolve():
        raise ValueError("Never overwrite the original report")
    if source.stat().st_size > MAX_REPORT_BYTES:
        raise ValueError("Source report exceeds size limit")
    data = json.loads(source.read_text(encoding="utf-8"))
    sanitized = export_report(data)
    # Exclusive creation avoids deleting or overwriting existing evidence.
    with destination.open("x", encoding="utf-8") as out:
        json.dump(sanitized, out, ensure_ascii=False, indent=2)
        out.write("\n")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path, help="industrial-verify report.json")
    parser.add_argument("--output", type=Path, required=True, help="new summary JSON (never overwritten)")
    args = parser.parse_args()
    try:
        write_export(args.source, args.output)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        parser.exit(2, f"Cannot export verification summary: {error}\n")


if __name__ == "__main__":
    main()
