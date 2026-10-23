#!/usr/bin/env python3
"""E42: evidence gate for a *real*, unmodified Minecraft Java 1.7.10 client.

CI cannot run the licensed game's GUI. This tool does not assert that testing
took place: operators must record each observation and attach local evidence.
"""
from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import sys
from pathlib import Path, PurePosixPath

CASES = tuple(f"42-{n:02d}" for n in range(1, 10))
STATUSES = {"PASS", "FAIL", "BLOCKED", "NOT_RUN"}
EVIDENCE_TYPES = {".png", ".jpg", ".jpeg", ".webp", ".mp4", ".webm", ".log", ".txt", ".jsonl"}
HASH = re.compile(r"[0-9a-fA-F]{40}\Z")


def new_report(sha: str) -> dict:
    if not HASH.fullmatch(sha):
        raise ValueError("server_commit must be a 40-digit Git commit SHA")
    return {
        "schema_version": 1,
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


def validate(report: dict, evidence_root: Path, require_pass: bool) -> list[str]:
    problems = []
    if not isinstance(report, dict) or report.get("schema_version") != 1:
        return ["unsupported E42 report schema"]
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
                for name in attachments:
                    try:
                        _evidence_path(evidence_root, name)
                    except (OSError, ValueError) as exc:
                        problems.append(f"{key}: {exc}")
    return problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    initialize = sub.add_parser("init", help="create an unverified NOT_RUN report")
    initialize.add_argument("--commit", required=True)
    initialize.add_argument("--output", type=Path, required=True)
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
