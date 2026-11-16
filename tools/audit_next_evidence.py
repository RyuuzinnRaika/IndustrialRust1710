#!/usr/bin/env python3
"""Offline NEXT issue/PR evidence-gap audit and Rust/E42 acceptance comparison.

This is a verification-only tool. It never starts a server, calls GitHub,
changes source issues, or infers successful gameplay from a Rust test.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

from e42_acceptance import CASES, validate as validate_e42

SHA = re.compile(r"(?<![0-9a-fA-F])[0-9a-fA-F]{40}(?![0-9a-fA-F])")
TEST = re.compile(r"(?<![A-Za-z0-9_])[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)+\b")
REQUIREMENT = re.compile(r"\b(?:M[0-4]|[A-E]\d{2}|N\d{2})\b")
REQ_LINE = re.compile(r"(?im)^\s*(?:関連要件|対象要件|要件ID|requirements?)\s*[:：]\s*(.+)$")
TEST_LINE = re.compile(r"(?m)^test (.+?) \.\.\. (ok|FAILED|ignored)(?:\s|$)")
VALID_RUST = {"PASS", "FAIL", "NOT_RUN"}
MAX_BYTES = 8_000_000


def read_json(path: Path) -> object:
    if path.stat().st_size > MAX_BYTES:
        raise ValueError("input exceeds size limit")
    return json.loads(path.read_text(encoding="utf-8"))


def rows(value: object, field: str) -> list[dict]:
    """Accept both a REST JSON list and a saved connector result list."""
    if isinstance(value, dict):
        value = value.get(field, value.get(field.capitalize()))
    if not isinstance(value, list) or len(value) > 10000 or any(not isinstance(r, dict) for r in value):
        raise ValueError(f"expected a JSON array of {field}")
    return value


def _number(row: dict) -> int:
    n = row.get("number")
    if type(n) is not int or n < 1:
        url = row.get("html_url") or row.get("display_url") or ""
        match = re.search(r"/(?:issues|pull)/(\d+)$", url) if isinstance(url, str) else None
        if not match:
            raise ValueError("issue/PR number missing")
        n = int(match.group(1))
    return n


def audit_snapshot(issues: list[dict], prs: list[dict]) -> dict:
    """Report missing *verifiable* evidence, not inferred fixes or closures."""
    result = {"schema_version": 1, "scope": "offline input snapshot", "issues": [], "prs": []}
    for issue in issues:
        if issue.get("pull_request"):
            continue
        body = issue.get("body") or ""
        if not isinstance(body, str):
            raise ValueError("non-string issue body")
        shas = sorted(set(SHA.findall(body)))
        tests = sorted(set(TEST.findall(body)))[:30]
        client_claim = bool(re.search(r"(?i)(?:E42|42-0[1-9]|1\.7\.10.{0,25}(?:実機|client))", body))
        missing = []
        if not tests:
            missing.append("named_test")
        if not shas:
            missing.append("commit_sha")
        # Text alone is not a sealed, independently validated E42 artifact.
        missing.append("verified_real_client_evidence")
        result["issues"].append({
            "number": _number(issue), "state": issue.get("state", "unknown"),
            "title": issue.get("title", ""), "candidate_test_names": tests,
            "commit_shas": shas, "client_evidence_claim_present": client_claim,
            "client_status": "NOT_RUN", "missing": missing,
        })
    for pr in prs:
        body = pr.get("body") or ""
        if not isinstance(body, str):
            raise ValueError("non-string PR body")
        reqs = sorted({x for line in REQ_LINE.findall(body) for x in REQUIREMENT.findall(line)})
        shas = sorted(set(SHA.findall(body)))
        tests = sorted(set(TEST.findall(body)))[:30]
        missing = []
        if not reqs:
            missing.append("requirement_ids")
        if not tests:
            missing.append("named_test")
        if not shas:
            missing.append("commit_sha")
        # A PR body describing 'PASS' is not cryptographic evidence.
        missing.append("verified_real_client_evidence")
        result["prs"].append({
            "number": _number(pr), "state": pr.get("state", "unknown"),
            "requirements": reqs, "candidate_test_names": tests,
            "commit_shas": shas, "client_status": "NOT_RUN", "missing": missing,
        })
    result["issues"].sort(key=lambda row: row["number"])
    result["prs"].sort(key=lambda row: row["number"])
    result["counts"] = {
        "issues_scanned": len(result["issues"]),
        "closed_issues_with_gaps": sum(x["state"] == "closed" and bool(x["missing"]) for x in result["issues"]),
        "prs_scanned": len(result["prs"]),
        "prs_missing_requirements": sum("requirement_ids" in x["missing"] for x in result["prs"]),
    }
    result["warning"] = ("Issue/PR body-only scan does not include comments, linked checks, or artifacts, and is not proof of a fix. "
                         "Unverified real-client evidence is conservatively NOT_RUN.")
    return result


def compare_e2e(rust_log: str, mapping: object, commit: str, client: object | None = None,
                evidence_root: Path | None = None) -> dict:
    """Join exact Rust test names with sealed E42 cases; fail closed on ambiguity."""
    if not isinstance(commit, str) or not SHA.fullmatch(commit):
        raise ValueError("a complete 40-character commit SHA is required")
    if not isinstance(mapping, dict) or set(mapping) != set(CASES):
        raise ValueError("mapping must cover exactly 42-01..42-09")
    if any(not isinstance(name, str) or not name or len(name) > 250 for name in mapping.values()):
        raise ValueError("invalid Rust test name")
    if len(set(mapping.values())) != len(CASES):
        raise ValueError("duplicate Rust test name in mapping")
    observed = {}
    for name, state in TEST_LINE.findall(rust_log):
        if name in observed:
            raise ValueError("ambiguous duplicate Rust test result")
        observed[name] = {"ok": "PASS", "FAILED": "FAIL", "ignored": "NOT_RUN"}[state]
    if client is not None:
        if evidence_root is None:
            raise ValueError("--evidence-root required for E42 report")
        if not isinstance(client, dict):
            raise ValueError("invalid E42 report")
        if client.get("server_commit", "").lower() != commit.lower():
            raise ValueError("Rust and E42 reports refer to different commits")
        problems = validate_e42(client, evidence_root, require_pass=False)
        if problems:
            raise ValueError("invalid/unverified E42 evidence: " + "; ".join(problems[:3]))
    output = []
    for case in CASES:
        rust_status = observed.get(mapping[case], "NOT_RUN")
        human = client["cases"][case]["status"] if client is not None else "NOT_RUN"
        client_status = human if human in ("PASS", "FAIL") else "NOT_RUN"
        if rust_status == "FAIL" or client_status == "FAIL":
            combined = "FAIL"
        elif rust_status == "PASS" and client_status == "PASS":
            combined = "PASS"
        else:
            combined = "NOT_RUN"
        output.append({"id": case, "rust_test": mapping[case],
                       "rust_status": rust_status, "client_status": client_status,
                       "client_source_status": human, "combined": combined,
                       "commit_sha": commit.lower()})
    return {"schema_version": 1, "commit_sha": commit.lower(), "cases": output,
            "all_pass": all(row["combined"] == "PASS" for row in output),
            "warning": "Rust tests do not replace sealed E42 unmodified-client evidence."}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    audit = commands.add_parser("audit", help="scan saved GitHub issues/PR JSON including closed issues")
    audit.add_argument("--issues", type=Path, required=True)
    audit.add_argument("--prs", type=Path, required=True)
    audit.add_argument("--output", type=Path, required=True)
    ledger = commands.add_parser("ledger", help="compare Cargo test log with verified E42 client evidence")
    ledger.add_argument("--rust-log", type=Path, required=True)
    ledger.add_argument("--mapping", type=Path, required=True)
    ledger.add_argument("--commit", required=True)
    ledger.add_argument("--client", type=Path)
    ledger.add_argument("--evidence-root", type=Path)
    ledger.add_argument("--output", type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        if args.command == "audit":
            data = audit_snapshot(rows(read_json(args.issues), "issues"), rows(read_json(args.prs), "prs"))
        else:
            if args.rust_log.stat().st_size > MAX_BYTES:
                raise ValueError("Rust log exceeds size limit")
            data = compare_e2e(args.rust_log.read_text(encoding="utf-8"), read_json(args.mapping),
                               args.commit, read_json(args.client) if args.client else None,
                               args.evidence_root)
        args.output.parent.mkdir(parents=True, exist_ok=True)
        with args.output.open("x", encoding="utf-8") as stream:
            json.dump(data, stream, ensure_ascii=False, indent=2)
            stream.write("\n")
    except (OSError, ValueError, KeyError, TypeError, UnicodeError, json.JSONDecodeError):
        # Never echo raw paths, logs, or token-like data from untrusted snapshots.
        print("Evidence audit failed (invalid input or output; inspect locally)", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
