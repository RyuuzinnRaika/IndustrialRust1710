#!/usr/bin/env python3
"""E42: evidence gate for a *real*, unmodified Minecraft Java 1.7.10 client.

CI cannot run the licensed game's GUI. This tool does not assert that testing
took place: operators must record each observation and attach local evidence.
"""
from __future__ import annotations

import argparse
import copy
import hashlib
from collections import Counter
import datetime as dt
import json
import re
import sys
from pathlib import Path, PurePosixPath

CASES = tuple(f"42-{n:02d}" for n in range(1, 10))
STATUSES = {"PASS", "FAIL", "BLOCKED", "NOT_RUN"}
EVIDENCE_TYPES = {".png", ".jpg", ".jpeg", ".webp", ".mp4", ".webm", ".log", ".txt", ".jsonl"}
HASH = re.compile(r"[0-9a-fA-F]{40}\Z")
DIGEST = re.compile(r"[0-9a-f]{64}\Z")


def new_report(sha: str) -> dict:
    if not HASH.fullmatch(sha):
        raise ValueError("server_commit must be a 40-digit Git commit SHA")
    return {
        "schema_version": 2,
        "server_commit": sha.lower(),
        "client": {
            "edition": "Minecraft Java Edition",
            "version": "1.7.10",
            "unmodified": True,
            "launcher": "",
            "os_java_runtime": "",
        },
        "executed_at": "",
        "connected_clients_peak": 0,
        "server_launch_arguments_redacted": "",
        "cases": {
            key: {"status": "NOT_RUN", "observation": "", "evidence": []}
            for key in CASES
        },
    }


def _evidence_path(root: Path, value: str) -> Path:
    if not isinstance(value, str) or not value:
        raise ValueError("evidence path must be a non-empty string")
    relative = PurePosixPath(value)
    if (
        relative.is_absolute()
        or "\\" in value
        or any(part in ("..", ".") for part in value.split("/"))
        or relative.suffix.lower() not in EVIDENCE_TYPES
    ):
        raise ValueError(f"unsafe or unsupported evidence path: {value!r}")
    # Resolve both ends, reject symlinks leading outside the evidence root.
    resolved = (root / value).resolve(strict=True)
    if not resolved.is_relative_to(root.resolve(strict=True)):
        raise ValueError(f"evidence escapes root: {value!r}")
    if not resolved.is_file() or resolved.stat().st_size == 0:
        raise ValueError(f"empty or non-file evidence: {value!r}")
    return resolved


def _sha256(path: Path) -> str:
    """Stream evidence hashes to avoid reading large recordings into memory."""
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def seal_report(report: dict, evidence_root: Path) -> dict:
    """Hash a manually completed report. Hashing is not proof of real gameplay."""
    if not isinstance(report, dict) or report.get("schema_version") != 2:
        raise ValueError("sealing requires an E42 schema v2 report")
    cases = report.get("cases")
    if not isinstance(cases, dict) or set(cases) != set(CASES):
        raise ValueError("sealing requires exactly nine E42 cases")
    sealed = copy.deepcopy(report)
    for key, row in sealed["cases"].items():
        if not isinstance(row, dict):
            raise ValueError(f"{key}: invalid case")
        if row.get("status") != "PASS":
            continue
        entries = row.get("evidence")
        if not isinstance(entries, list) or not entries:
            raise ValueError(f"{key}: PASS requires evidence paths")
        result = []
        for entry in entries:
            name = entry.get("path") if isinstance(entry, dict) else entry
            resolved = _evidence_path(evidence_root, name)
            result.append({"path": name, "sha256": _sha256(resolved)})
        row["evidence"] = result
    return sealed


def validate(report: dict, evidence_root: Path, require_pass: bool) -> list[str]:
    problems = []
    if not isinstance(report, dict) or report.get("schema_version") not in (1, 2):
        return ["unsupported E42 report schema"]
    version = report["schema_version"]
    if require_pass and version != 2:
        problems.append("PASS certification requires schema v2 with SHA-256 evidence")
    if not isinstance(report.get("server_commit"), str) or not HASH.fullmatch(report["server_commit"]):
        problems.append("server_commit must identify an exact 40-character commit")
    client = report.get("client")
    if not isinstance(client, dict) or (
        client.get("edition") != "Minecraft Java Edition"
        or client.get("version") != "1.7.10"
        or client.get("unmodified") is not True
    ):
        problems.append("client must be unmodified Minecraft Java Edition 1.7.10")
    if not isinstance(client, dict) or not all(
        isinstance(client.get(key), str) and client[key].strip()
        for key in ("launcher", "os_java_runtime")
    ):
        problems.append("launcher and os_java_runtime are required")
    when = report.get("executed_at")
    try:
        parsed = dt.datetime.fromisoformat(when)
        if parsed.tzinfo is None or parsed.utcoffset() is None:
            raise ValueError("timezone required")
    except (TypeError, ValueError):
        problems.append("executed_at requires a timezone-aware ISO-8601 timestamp")
    clients = report.get("connected_clients_peak")
    if type(clients) is not int or not 1 <= clients <= 8:
        problems.append("connected_clients_peak must be an integer in 1..8")
    args = report.get("server_launch_arguments_redacted")
    if not isinstance(args, str) or not args.strip():
        problems.append("redacted server launch arguments are required")
    cases = report.get("cases")
    if not isinstance(cases, dict) or set(cases) != set(CASES):
        problems.append("exactly nine E42 scenarios are required")
        return problems
    checksummed_by_case = {}
    for key in CASES:
        row = cases[key]
        if not isinstance(row, dict) or row.get("status") not in STATUSES:
            problems.append(f"{key}: invalid status")
            continue
        status = row["status"]
        if require_pass and status != "PASS":
            problems.append(f"{key}: {status} is not PASS")
        if status == "PASS":
            if key in ("42-05", "42-09") and (type(clients) is not int or clients < 2):
                problems.append(f"{key}: multi-client scenario needs >=2 clients")
            if not isinstance(row.get("observation"), str) or not row["observation"].strip():
                problems.append(f"{key}: PASS needs a written observation")
            attachments = row.get("evidence")
            if not isinstance(attachments, list) or not attachments:
                problems.append(f"{key}: PASS needs at least one artifact")
            else:
                valid_digests = set()
                for entry in attachments:
                    if version == 2:
                        if not isinstance(entry, dict) or set(entry) != {"path", "sha256"}:
                            problems.append(f"{key}: evidence needs path and sha256")
                            continue
                        name, expected = entry["path"], entry["sha256"]
                        if not isinstance(expected, str) or not DIGEST.fullmatch(expected):
                            problems.append(f"{key}: invalid SHA-256 digest")
                            continue
                    else:
                        name, expected = entry, None
                    try:
                        path = _evidence_path(evidence_root, name)
                        if version == 2:
                            actual = _sha256(path)
                            if actual != expected:
                                problems.append(f"{key}: evidence SHA-256 mismatch: {name}")
                            else:
                                valid_digests.add(actual)
                    except (OSError, ValueError) as exc:
                        problems.append(f"{key}: {exc}")
                if version == 2:
                    checksummed_by_case[key] = valid_digests
    if version == 2:
        counts = Counter(value for values in checksummed_by_case.values()
                         for value in values)
        for key, values in checksummed_by_case.items():
            if not any(counts[value] == 1 for value in values):
                problems.append(f"{key}: PASS needs at least one case-specific artifact")
    return problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    initialize = sub.add_parser("init", help="create an unverified NOT_RUN report")
    initialize.add_argument("--commit", required=True)
    initialize.add_argument("--output", type=Path, required=True)
    sealer = sub.add_parser("seal", help="hash PASS artifacts into a separate report")
    sealer.add_argument("--report", type=Path, required=True)
    sealer.add_argument("--evidence-root", type=Path, required=True)
    sealer.add_argument("--output", type=Path, required=True)
    check = sub.add_parser("verify", help="check human-recorded real-client evidence")
    check.add_argument("--report", type=Path, required=True)
    check.add_argument("--evidence-root", type=Path, required=True)
    check.add_argument("--require-pass", action="store_true")
    args = parser.parse_args(argv)
    if args.command == "init":
        report = new_report(args.commit)
        args.output.parent.mkdir(parents=True, exist_ok=True)
        with args.output.open("x", encoding="utf-8") as handle:
            json.dump(report, handle, ensure_ascii=False, indent=2)
            handle.write("\n")
        print(f"Created NOT_RUN report: {args.output}")
        return 0
    report = json.loads(args.report.read_text(encoding="utf-8"))
    if args.command == "seal":
        sealed = seal_report(report, args.evidence_root)
        problems = validate(sealed, args.evidence_root, require_pass=False)
        if problems:
            raise ValueError("; ".join(problems))
        args.output.parent.mkdir(parents=True, exist_ok=True)
        with args.output.open("x", encoding="utf-8") as handle:
            json.dump(sealed, handle, ensure_ascii=False, indent=2)
            handle.write("\n")
        print(f"Sealed SHA-256 E42 evidence: {args.output}")
        return 0
    problems = validate(report, args.evidence_root, args.require_pass)
    cases = report.get("cases", {}) if isinstance(report, dict) else {}
    summary = {status: sum(
        1 for row in cases.values()
        if isinstance(row, dict) and row.get("status") == status
    ) for status in sorted(STATUSES)} if isinstance(cases, dict) else {}
    print(json.dumps({"case_counts": summary, "verified": not problems}, sort_keys=True))
    for problem in problems:
        print("E42: " + problem, file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, json.JSONDecodeError) as exc:
        print(f"E42: {exc}", file=sys.stderr)
        sys.exit(2)
