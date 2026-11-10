#!/usr/bin/env python3
"""Offline cold/warm Rust build benchmark in an isolated Cargo target directory.

Verification-only program: never runs GitHub Actions, calls cargo clean, or
modifies an existing Cargo target directory. Logs are local and not shareable
without manual secrets review.
"""
import argparse
import hashlib
import json
import os
import platform
import re
import subprocess
import time
from pathlib import Path

BINARIES = ("vanilla-server", "legacy-status-server")
TARGET_ARGS = ("build", "--release", "-p", BINARIES[0], "-p", BINARIES[1])
SCHEMA = 1


def executable_path(target_dir: Path, name: str, system: str) -> Path:
    return target_dir / "release" / (name + (".exe" if system == "Windows" else ""))


def fingerprints(target_dir: Path, system: str) -> dict:
    result = {}
    for name in BINARIES:
        path = executable_path(target_dir, name, system)
        if not path.is_file():
            raise ValueError(f"Missing build output: {name}")
        h = hashlib.sha256()
        with path.open("rb") as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b""):
                h.update(chunk)
        result[name] = {"bytes": path.stat().st_size, "sha256": h.hexdigest()}
    return result


def version(program: str, root: Path) -> str:
    try:
        proc = subprocess.run([program, "--version"], cwd=root, capture_output=True,
                              text=True, timeout=15, check=False)
    except (OSError, subprocess.TimeoutExpired):
        return "unknown"
    if proc.returncode != 0:
        return "unknown"
    value = proc.stdout.strip()
    # Do not copy arbitrary output, paths, or environment into the report.
    if len(value) > 120 or not re.fullmatch(r"[A-Za-z0-9()._+\- ]+", value):
        return "unknown"
    return value


def git_head(root: Path) -> str:
    try:
        result = subprocess.run(["git", "rev-parse", "HEAD"], cwd=root,
                                capture_output=True, text=True, timeout=10, check=False)
    except (OSError, subprocess.TimeoutExpired):
        return "unknown"
    value = result.stdout.strip()
    return value if result.returncode == 0 and re.fullmatch(r"[0-9a-fA-F]{40}", value) else "unknown"


def compare_baseline(current: dict, baseline: dict, max_ratio: float) -> dict:
    if not 1.0 <= max_ratio <= 10.0:
        raise ValueError("max_ratio must be between 1 and 10")
    required = ("system", "machine", "cargo_version", "rustc_version", "profile", "binaries")
    if baseline.get("schema_version") != SCHEMA or baseline.get("status") != "PASS":
        return {"status": "INCOMPARABLE", "reason": "Invalid or failed baseline"}
    if any(current.get(k) != baseline.get(k) for k in required):
        return {"status": "INCOMPARABLE", "reason": "Platform, toolchain or targets differ"}
    ratios = {}
    for phase in ("cold", "warm"):
        now = current.get(f"{phase}_elapsed_ms")
        before = baseline.get(f"{phase}_elapsed_ms")
        if type(now) not in (int, float) or type(before) not in (int, float) or now <= 0 or before <= 0:
            return {"status": "INCOMPARABLE", "reason": "Missing positive timing"}
        ratios[phase] = round(now / before, 4)
    return {"status": "FAIL" if any(x > max_ratio for x in ratios.values()) else "PASS",
            "cold_ratio": ratios["cold"], "warm_ratio": ratios["warm"],
            "max_allowed_ratio": max_ratio}


def run(root: Path, output: Path, cargo: str = "cargo", rustc: str = "rustc",
        baseline: dict | None = None, max_ratio: float = 1.25) -> dict:
    # The output directory MUST NOT exist; no reuse of historical targets.
    output.mkdir(parents=True, exist_ok=False)
    target = output / "cargo-target"
    system = platform.system()
    report = {
        "schema_version": SCHEMA, "status": "NOT_RUN", "commit": git_head(root),
        "system": system, "machine": platform.machine(), "profile": "release",
        "binaries": list(BINARIES),
        "cargo_version": version(cargo, root), "rustc_version": version(rustc, root),
        "commands": ["cargo build --release -p vanilla-server -p legacy-status-server --target-dir <new isolated dir>"],
        "cold_elapsed_ms": None, "warm_elapsed_ms": None,
        "artifacts": {}, "baseline_comparison": {"status": "NOT_RUN", "reason": "No baseline supplied"},
        "note": "Local benchmark only. Raw logs remain private in output directory; not a CI or gameplay PASS.",
    }
    try:
        for phase in ("cold", "warm"):
            command = [cargo, *TARGET_ARGS, "--target-dir", str(target)]
            start = time.perf_counter_ns()
            with (output / f"{phase}.log").open("xb") as log:
                try:
                    completed = subprocess.run(command, cwd=root, stdout=log, stderr=subprocess.STDOUT,
                                               check=False, timeout=3600)
                    code = completed.returncode
                except (OSError, subprocess.TimeoutExpired):
                    code = None
            report[f"{phase}_elapsed_ms"] = round((time.perf_counter_ns() - start) / 1_000_000, 3)
            report[f"{phase}_exit_code"] = code
            if code != 0:
                report["status"] = "FAIL" if code is not None else "NOT_RUN"
                break
            try:
                report["artifacts"][phase] = fingerprints(target, system)
            except (OSError, ValueError):
                report["status"] = "FAIL"
                break
        else:
            report["status"] = "PASS"
            report["binary_hashes_unchanged"] = (
                report["artifacts"]["cold"] == report["artifacts"]["warm"]
            )
            if not report["binary_hashes_unchanged"]:
                report["status"] = "FAIL"
            if baseline is not None and report["status"] == "PASS":
                report["baseline_comparison"] = compare_baseline(report, baseline, max_ratio)
    finally:
        with (output / "report.json").open("x", encoding="utf-8") as dest:
            json.dump(report, dest, ensure_ascii=False, indent=2)
            dest.write("\n")
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path, required=True,
                        help="New directory for build artifacts and logs; must not exist")
    parser.add_argument("--baseline", type=Path, help="Earlier report.json from same host/toolchain")
    parser.add_argument("--max-regression-ratio", type=float, default=1.25)
    args = parser.parse_args()
    if not 1.0 <= args.max_regression_ratio <= 10.0:
        parser.error("--max-regression-ratio must be between 1 and 10")
    base = None
    if args.baseline:
        if args.baseline.stat().st_size > 100_000:
            parser.error("Baseline report too large")
        base = json.loads(args.baseline.read_text(encoding="utf-8"))
        if not isinstance(base, dict):
            parser.error("Baseline must be a JSON object")
    try:
        report = run(Path.cwd(), args.output_dir, baseline=base,
                     max_ratio=args.max_regression_ratio)
    except (OSError, ValueError) as exc:
        parser.exit(2, f"Cannot benchmark: {exc}\n")
    print(f"Build benchmark: {report['status']}; baseline: {report['baseline_comparison']['status']}")
    print("Full logs remain local. Check output-dir/report.json for timings and hashes.")
    if report["status"] != "PASS":
        return 1
    if report["baseline_comparison"]["status"] == "FAIL":
        return 1
    if report["baseline_comparison"]["status"] == "INCOMPARABLE":
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
