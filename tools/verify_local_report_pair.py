"""Compare two local industrial-verify reports without reading logs or world data.

Usage: python tools/verify_local_report_pair.py --windows win/report.json --ubuntu linux/report.json
Exit: 0 = identical statuses, 1 = different statuses, 2 = invalid/incomplete evidence.
"""
import argparse
import json
from pathlib import Path
import re
import sys

MAX_REPORT_BYTES = 1024 * 1024
CHECK_NAMES = (
    "rustfmt",
    "Rust unit/integration/Protocol 5 TCP",
    "Clippy strict",
    "Release server binaries",
    "Server --help smoke",
    "Python verifier unit tests",
    "E42 client evidence validation",
)
STATUSES = frozenset({"PASS", "FAIL", "NOT_RUN"})


class EvidenceError(ValueError):
    """Report cannot support a trustworthy cross-platform comparison."""


def load_report(path: Path, expected_os: str) -> dict:
    try:
        if path.stat().st_size > MAX_REPORT_BYTES:
            raise EvidenceError("report exceeds 1 MiB")
        report = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise EvidenceError(f"could not read {expected_os} report: {type(error).__name__}") from error
    if not isinstance(report, dict) or report.get("schema_version") != 1:
        raise EvidenceError(f"{expected_os}: unsupported report schema")
    if report.get("host_os") != expected_os:
        raise EvidenceError(f"{expected_os}: host_os mismatch")
    if not isinstance(report.get("host_arch"), str) or not report["host_arch"]:
        raise EvidenceError(f"{expected_os}: missing host_arch")
    commit = report.get("commit")
    if not isinstance(commit, str) or re.fullmatch(r"[0-9a-fA-F]{40}", commit) is None:
        raise EvidenceError(f"{expected_os}: commit must be a full Git SHA")
    if type(report.get("exit_code")) is not int or report["exit_code"] not in (0, 1, 2):
        raise EvidenceError(f"{expected_os}: invalid verifier exit_code")
    checks = report.get("checks")
    if not isinstance(checks, list) or len(checks) != len(CHECK_NAMES):
        raise EvidenceError(f"{expected_os}: missing or extra verification checks")
    names = []
    for check in checks:
        if not isinstance(check, dict):
            raise EvidenceError(f"{expected_os}: malformed check entry")
        names.append(check.get("name"))
        if check.get("status") not in STATUSES:
            raise EvidenceError(f"{expected_os}: invalid check status")
    if tuple(names) != CHECK_NAMES:
        raise EvidenceError(f"{expected_os}: unexpected check names or order")
    return report


def compare_reports(windows: dict, ubuntu: dict) -> dict:
    if windows["commit"].lower() != ubuntu["commit"].lower():
        raise EvidenceError("reports refer to different Git commits")
    entries = []
    for win, lin in zip(windows["checks"], ubuntu["checks"], strict=True):
        entries.append({
            "name": win["name"],
            "windows": win["status"],
            "linux": lin["status"],
            "match": win["status"] == lin["status"],
        })
    return {
        "schema_version": 1,
        "commit": windows["commit"].lower(),
        "windows_arch": windows["host_arch"],
        "linux_arch": ubuntu["host_arch"],
        "identical_statuses": all(entry["match"] for entry in entries),
        "checks": entries,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--windows", type=Path, required=True)
    parser.add_argument("--ubuntu", type=Path, required=True)
    parser.add_argument("--output", type=Path, help="new JSON file; never overwrite evidence")
    args = parser.parse_args(argv)
    try:
        windows = load_report(args.windows, "windows")
        linux = load_report(args.ubuntu, "linux")
        summary = compare_reports(windows, linux)
        encoded = json.dumps(summary, ensure_ascii=False, indent=2) + "\n"
        if args.output is not None:
            if args.output.resolve() in (args.windows.resolve(), args.ubuntu.resolve()):
                raise EvidenceError("output must not replace a source report")
            try:
                with args.output.open("x", encoding="utf-8") as output:
                    output.write(encoded)
            except OSError as error:
                raise EvidenceError(f"could not create output: {type(error).__name__}") from error
        print(encoded, end="")
        return 0 if summary["identical_statuses"] else 1
    except EvidenceError as error:
        print(f"invalid comparison evidence: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
