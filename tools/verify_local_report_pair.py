"""Compare two local industrial-verify reports without reading logs or world data.

Usage: python tools/verify_local_report_pair.py --windows win/report.json --ubuntu linux/report.json
Exit: 0 = both verifiers succeeded with matching statuses; 1 = mismatch or failure; 2 = invalid evidence.
"""
import argparse
import json
from pathlib import Path
import re
import stat
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
        metadata = path.lstat()  # Do not follow a symlink into mutable or private evidence.
        if not stat.S_ISREG(metadata.st_mode):
            raise EvidenceError("report must be a regular file")
        if metadata.st_size > MAX_REPORT_BYTES:
            raise EvidenceError("report exceeds 1 MiB")
        # The file might grow after stat(): never read more than the cap + 1.
        with path.open("rb") as source:
            raw = source.read(MAX_REPORT_BYTES + 1)
        if len(raw) > MAX_REPORT_BYTES:
            raise EvidenceError("report exceeds 1 MiB")
        report = json.loads(raw.decode("utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise EvidenceError(f"could not read {expected_os} report: {type(error).__name__}") from error
    if (
        not isinstance(report, dict)
        or type(report.get("schema_version")) is not int
        or report["schema_version"] != 1
    ):
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
        status = check.get("status")
        if not isinstance(status, str) or status not in STATUSES:
            raise EvidenceError(f"{expected_os}: invalid check status")
        code = check.get("exit_code")
        if code is not None and type(code) is not int:
            raise EvidenceError(f"{expected_os}: malformed check exit_code")
        if status == "PASS" and code != 0:
            raise EvidenceError(f"{expected_os}: PASS without zero exit_code")
        if status == "FAIL" and code == 0:
            raise EvidenceError(f"{expected_os}: FAIL with zero exit_code")
        if status == "NOT_RUN" and code is not None:
            raise EvidenceError(f"{expected_os}: NOT_RUN with an exit_code")
    if tuple(names) != CHECK_NAMES:
        raise EvidenceError(f"{expected_os}: unexpected check names or order")

    # industrial-verify reports exit 1 for any FAIL, 2 for missing required
    # checks, and 0 only when every requested check passed. The optional E42
    # check may be NOT_RUN when --automated-only was used (the flag is not
    # included in schema v1). An inconsistent report must never pass a gate.
    statuses = [check["status"] for check in checks]
    failed = "FAIL" in statuses
    incomplete = "NOT_RUN" in statuses
    required_incomplete = "NOT_RUN" in statuses[:-1]
    exit_code = report["exit_code"]
    if (exit_code == 0 and (failed or required_incomplete)) or (
        exit_code == 1 and not failed
    ) or (exit_code == 2 and (failed or not incomplete)):
        raise EvidenceError(f"{expected_os}: summary exit_code conflicts with check statuses")
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
        "both_verifiers_succeeded": windows["exit_code"] == ubuntu["exit_code"] == 0,
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
        # Matching failures or incomplete verifications are not a PASS gate.
        return 0 if summary["identical_statuses"] and summary["both_verifiers_succeeded"] else 1
    except EvidenceError as error:
        print(f"invalid comparison evidence: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
