#!/usr/bin/env python3
"""Offline, fail-closed gate check for a local industrial-verify report.json.

Does not run tests, modify worlds, start CI, or consider Rust TCP tests a
substitute for unmodified Minecraft Java 1.7.10 client evidence.
"""
import argparse
import json
import re
from pathlib import Path

AUTO = (
    "rustfmt",
    "Rust unit/integration/Protocol 5 TCP",
    "Clippy strict",
    "Release server binaries",
    "Server --help smoke",
    "Python verifier unit tests",
)
CLIENT = "E42 client evidence validation"
ALLOWED = set(AUTO) | {CLIENT}
VALID = {"PASS", "FAIL", "NOT_RUN"}


def evaluate(report: object) -> dict:
    if not isinstance(report, dict) or report.get("schema_version") != 1:
        raise ValueError("Expected industrial-verify report schema version 1")
    commit = report.get("commit")
    if not isinstance(commit, str) or re.fullmatch(r"[0-9a-fA-F]{40}", commit) is None:
        raise ValueError("Report needs a 40-character commit SHA for traceability")
    rows = report.get("checks")
    if not isinstance(rows, list) or len(rows) > 64:
        raise ValueError("Invalid check list")
    found = {}
    for row in rows:
        if not isinstance(row, dict):
            raise ValueError("Invalid check row")
        name, state = row.get("name"), row.get("status")
        if not isinstance(name, str) or name not in ALLOWED or name in found or state not in VALID:
            raise ValueError("Unknown, duplicate, or invalid check state")
        code = row.get("exit_code")
        if code is not None and (type(code) is not int or not -255 <= code <= 255):
            raise ValueError("Invalid exit code")
        if state == "PASS" and code != 0:
            raise ValueError("PASS requires a successful exit code")
        if state == "NOT_RUN" and code is not None:
            raise ValueError("NOT_RUN must not have an exit code")
        found[name] = state

    def fold(names):
        states = [found.get(name, "NOT_RUN") for name in names]
        if "FAIL" in states:
            return "FAIL"
        if "NOT_RUN" in states:
            return "NOT_RUN"
        return "PASS"

    automated = fold(AUTO)
    client = fold((CLIENT,))
    combined = fold(AUTO + (CLIENT,))
    return {
        "commit": commit,
        "automated": automated,
        "client_e42": client,
        "all": combined,
        "missing_checks": [name for name in AUTO + (CLIENT,) if name not in found],
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--require", choices=("automated", "client_e42", "all"), default="all")
    args = parser.parse_args()
    try:
        if args.report.stat().st_size > 2_000_000:
            raise ValueError("Report too large")
        raw = json.loads(args.report.read_text(encoding="utf-8"))
        result = evaluate(raw)
    except (OSError, ValueError, UnicodeError, json.JSONDecodeError) as exc:
        # Do not echo exception text containing potentially sensitive paths.
        parser.exit(2, f"Invalid local verification report ({type(exc).__name__})\n")
    print(f"commit={result['commit']}")
    print(f"automated={result['automated']} client_e42={result['client_e42']} all={result['all']}")
    print("Automated Rust/TCP tests do not attest real-client acceptance.")
    selected = result[args.require]
    return 0 if selected == "PASS" else (1 if selected == "FAIL" else 2)


if __name__ == "__main__":
    raise SystemExit(main())
