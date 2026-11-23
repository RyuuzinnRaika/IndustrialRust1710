#!/usr/bin/env python3
"""REG-195 / #831: opt-in 1,000 full Protocol 5 login/logout resource audit.

Only creates a fresh test world and binds 127.0.0.1. Not a DOS benchmark:
the default 3.2-second cadence stays below the 24-logins/minute per-IP gate.
Shortened --sessions runs are smoke tests, never evidence for 1,000 sessions.
``lifecycle_verified`` covers the executed session lifecycle and recovery probe.
``metrics_complete`` records whether all scheduled resource samples exist.
Neither proves that disconnected sessions were released or memory leaks absent;
``verified`` remains false until the issue's acceptance criteria can be tested.
Schema version 2 changes ``verified`` from the version 1 lifecycle-only meaning;
external report consumers must use the explicit lifecycle and coverage fields.
Smoke runs retain exit 0 when their lifecycle finishes. Full runs return 2 while
acceptance is NOT_RUN. Explicit --measure-only permits exit 0 for a full run
with complete samples, but does not change verified or the acceptance status.
Missing samples needed by a requested RSS limit also return 2, even for smoke;
full runs with missing samples return 2 even with --measure-only. Failures use 1.
Manifest writes are not atomic; interruption or write failure can leave partial data.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import json
import math
from pathlib import Path
import platform
import re
import subprocess
import sys
import time

from e48_load_benchmark import (
    SyntheticClient, new_output_directory, reserve_loopback_port,
    wait_until_listening, shutdown,
)

ONLINE = re.compile(r"^Online \((\d+)\):", re.MULTILINE)


class ResourceMeasurementUnavailable(RuntimeError):
    """A required resource value could not be measured; not a threshold failure."""


def proc_snapshot(pid: int) -> dict:
    """Linux-only /proc counters; missing fields remain null, not zero."""
    state = {"rss_bytes": None, "rss_peak_bytes": None,
             "threads": None, "fd_count": None}
    try:
        for line in Path(f"/proc/{pid}/status").read_text().splitlines():
            if line.startswith("VmRSS:"):
                state["rss_bytes"] = int(line.split()[1]) * 1024
            elif line.startswith("VmHWM:"):
                state["rss_peak_bytes"] = int(line.split()[1]) * 1024
            elif line.startswith("Threads:"):
                state["threads"] = int(line.split()[1])
        state["fd_count"] = len(list(Path(f"/proc/{pid}/fd").iterdir()))
    except (OSError, ValueError, IndexError):
        pass
    return state


def query_online_count(child: subprocess.Popen, log: Path, timeout: float = 5) -> int:
    """Require a NEW /list response, never reuse an earlier Online (0)."""
    if child.poll() is not None or child.stdin is None:
        raise RuntimeError("native server exited before /list")
    offset = log.stat().st_size
    child.stdin.write(b"/list\n")
    child.stdin.flush()
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if child.poll() is not None:
            raise RuntimeError(f"server exited during /list: {child.returncode}")
        with log.open("rb") as handle:
            handle.seek(offset)
            new_log = handle.read().decode("utf-8", "replace")
        counts = ONLINE.findall(new_log)
        if counts:
            return int(counts[-1])
        time.sleep(0.05)
    raise TimeoutError("fresh /list response not observed")


def await_no_players(child: subprocess.Popen, log: Path) -> None:
    deadline = time.monotonic() + 8
    while time.monotonic() < deadline:
        if query_online_count(child, log) == 0:
            return
        time.sleep(0.1)
    raise RuntimeError("a disconnected session remained registered online")


def full_login(port: int, name: str) -> SyntheticClient:
    """One counted attempt must reach Play and receive its initial position."""
    client = SyntheticClient(port, name)
    if not client.ready.wait(15) or client.failure:
        reason = client.failure or "login timed out"
        client.close()
        raise RuntimeError(f"{name}: full Protocol 5 login failed: {reason}")
    return client


def trend(samples: list[dict], key: str) -> dict:
    def observed(value: object) -> bool:
        return isinstance(value, int) and not isinstance(value, bool) and value >= 0

    values = [sample[key] for sample in samples if observed(sample.get(key))]
    # Do not silently replace a missing boundary with a later/earlier sample.
    initial = samples[0].get(key) if samples and observed(samples[0].get(key)) else None
    final = samples[-1].get(key) if samples and observed(samples[-1].get(key)) else None
    return {
        "sample_count": len(values),
        "initial": initial,
        "final": final,
        "delta": (final - initial if len(samples) >= 2
                  and initial is not None and final is not None else None),
        "peak": max(values) if values else None,
    }


def resource_samples_complete(samples: list[dict], sessions: int) -> bool:
    """All scheduled samples, including both boundaries, need real counters."""
    expected = [0, *range(25, sessions + 1, 25)]
    if sessions % 25:
        expected.append(sessions)
    if [sample.get("completed") for sample in samples] != expected:
        return False
    for sample in samples:
        for key in ("rss_bytes", "fd_count", "threads"):
            value = sample.get(key)
            if not isinstance(value, int) or isinstance(value, bool) or value < 0:
                return False
    return True


def save_report(output: Path, report: dict) -> None:
    (output / "manifest.json").write_text(
        json.dumps(report, indent=2) + "\n", encoding="utf-8")


def print_stderr(message: str) -> None:
    """Preserve the intended exit status if the diagnostic stream is broken."""
    try:
        print(message, file=sys.stderr)
    except Exception:
        pass


def finish_report(output: Path, report: dict, client, child, primary_error, log=None) -> None:
    """Finish independent cleanup, then report; defer interrupts until then."""
    cleanup_errors = []
    interruption = None

    def failed(label, error):
        nonlocal interruption
        cleanup_errors.append(f"{label}: {error}")
        if not isinstance(error, Exception) and interruption is None:
            interruption = error

    def report_stderr(message):
        try:
            print(message, file=sys.stderr)
        except BaseException as error:
            failed("stderr.write", error)

    if client is not None:
        try:
            client.close()
        except BaseException as error:
            failed("client.close", error)
    if child is not None:
        try:
            if child.poll() is None:
                child.kill()
        except BaseException as error:
            failed("child.poll/kill", error)
        try:
            child.wait(timeout=10)
        except BaseException as error:
            failed("child.wait", error)
    if log is not None:
        try:
            log.close()
        except BaseException as error:
            failed("server.log.close", error)
    report["cleanup_errors"] = cleanup_errors
    if cleanup_errors:
        report["lifecycle_verified"] = False
        report["verified"] = False
        report_stderr("REG-195 cleanup failed: " + "; ".join(cleanup_errors))
    report["finished_utc"] = datetime.now(timezone.utc).isoformat()
    report_error = None
    try:
        save_report(output, report)
    except BaseException as error:
        report_error = error
        if not isinstance(error, Exception) and interruption is None:
            interruption = error
        report_stderr(f"REG-195 final manifest write failed: {error}")
    # Control-flow interruptions are deferred, never silently discarded.
    # If work already failed, keep that exception as the interruption's cause.
    if interruption is not None:
        raise interruption from primary_error
    if report_error is not None and primary_error is None:
        raise report_error
    if cleanup_errors and primary_error is None:
        raise RuntimeError("cleanup failed: " + "; ".join(cleanup_errors))


def run(server: Path, output: Path, sessions: int, interval: float,
        max_rss_growth_mib: float | None, measure_only: bool = False) -> dict:
    new_output_directory(output)
    world = output / "reconnect.irw"
    whitelist = output / "whitelist.json"
    whitelist.write_text(json.dumps([{"name": "Load0"}, {"name": "Load1"}]),
                         encoding="utf-8")
    port = reserve_loopback_port()
    command = [str(server), "--bind", f"127.0.0.1:{port}",
               "--world", str(world), "--whitelist", str(whitelist)]
    log_path = output / "server.log"
    report = {
        "schema_version": 2,
        "issue": 831, "regression": "REG-195",
        "started_utc": datetime.now(timezone.utc).isoformat(),
        "requested_sessions": sessions, "full_1000_requested": sessions >= 1000,
        "measurement_only_requested": measure_only,
        "max_rss_growth_mib": max_rss_growth_mib,
        "interval_seconds": interval, "server_binary": str(server),
        "platform": platform.platform(),
        "client_type": "synthetic native Protocol 5, not Java GUI",
        "completed_sessions": 0, "final_fresh_login": False,
        "samples": [], "lifecycle_verified": False,
        "metrics_complete": False,
        "rss_limit_status": "NOT_REQUESTED" if max_rss_growth_mib is None else "NOT_RUN",
        "retained_sessions_status": "NOT_MEASURED",
        "acceptance_status": "NOT_RUN", "verified": False,
        "cleanup_errors": [],
    }
    child = None
    client = None
    log = log_path.open("wb")
    primary_error = None
    try:
        child = subprocess.Popen(command, cwd=output, stdin=subprocess.PIPE,
                                 stdout=log, stderr=subprocess.STDOUT)
        wait_until_listening(child, port)
        start = time.monotonic()
        report["samples"].append({"completed": 0, **proc_snapshot(child.pid)})
        for i in range(sessions):
            if child.poll() is not None:
                raise RuntimeError(f"native server crashed: {child.returncode}")
            # The launch rate is deliberately limited, so the test
            # measures session retention, not the login flood limiter.
            client = full_login(port, "Load0")
            if query_online_count(child, log_path) != 1:
                raise RuntimeError(f"iteration {i}: expected exactly one online player")
            client.close()
            client = None
            await_no_players(child, log_path)
            report["completed_sessions"] = i + 1
            if (i + 1) % 25 == 0 or i + 1 == sessions:
                report["samples"].append({
                    "completed": i + 1, "elapsed_seconds": time.monotonic() - start,
                    **proc_snapshot(child.pid),
                })
                save_report(output, report)
            time.sleep(max(0, start + (i + 1) * interval - time.monotonic()))
        # A distinct player after all repeats tests actual admission
        # recovery rather than just successful TCP listener connect.
        client = full_login(port, "Load1")
        if query_online_count(child, log_path) != 1:
            raise RuntimeError("fresh player was not registered after stress")
        client.close()
        client = None
        await_no_players(child, log_path)
        report["final_fresh_login"] = True
        report["elapsed_seconds"] = time.monotonic() - start
        report["rss"] = trend(report["samples"], "rss_bytes")
        report["fd"] = trend(report["samples"], "fd_count")
        report["threads"] = trend(report["samples"], "threads")
        if max_rss_growth_mib is not None:
            growth = report["rss"]["delta"]
            if growth is None:
                report["rss_limit_status"] = "UNAVAILABLE"
                raise ResourceMeasurementUnavailable(
                    "RSS measurement unavailable; threshold cannot be checked")
            if growth > max_rss_growth_mib * 1024 * 1024:
                report["rss_limit_status"] = "FAIL"
                raise RuntimeError(f"RSS growth exceeded configured threshold: {growth}")
            report["rss_limit_status"] = "PASS"
        shutdown(child)
        child = None
        if world.with_suffix(".pending-txn.json").exists():
            raise RuntimeError("pending transaction after orderly shutdown")
        verify = subprocess.run([str(server), "--verify-world", str(output)],
                                cwd=output, capture_output=True, text=True, timeout=60)
        if verify.returncode:
            raise RuntimeError(f"read-only world verification failed: {verify.stderr[-1000:]}")
        report["lifecycle_verified"] = True
        report["metrics_complete"] = resource_samples_complete(report["samples"], sessions)
        # /list measures online sessions, not objects retained after removal.
        # Resource samples alone do not prove non-growing retained sessions.
        # Keep acceptance_status NOT_RUN and verified false until measured.
    except BaseException as error:
        # Track this run's failure, not an unrelated caller's active except.
        primary_error = error
        raise
    finally:
        finish_report(output, report, client, child, primary_error, log)
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--server", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--sessions", type=int, default=1000)
    parser.add_argument("--interval-seconds", type=float, default=3.2)
    parser.add_argument("--max-rss-growth-mib", type=float, default=None)
    parser.add_argument("--measure-only", action="store_true",
                        help="allow exit 0 for complete measurements; acceptance remains NOT_RUN")
    args = parser.parse_args()
    if not 1 <= args.sessions <= 10000:
        parser.error("--sessions must be 1..10000")
    if not (math.isfinite(args.interval_seconds) and
            3.0 <= args.interval_seconds <= 3600):
        parser.error("interval must be 3..3600s to avoid admission limiter")
    if args.max_rss_growth_mib is not None and not (
            math.isfinite(args.max_rss_growth_mib) and args.max_rss_growth_mib >= 0):
        parser.error("RSS growth limit must be nonnegative and finite")
    try:
        server = args.server.resolve(strict=True)
        report = run(server, args.output.resolve(), args.sessions,
                     args.interval_seconds, args.max_rss_growth_mib, args.measure_only)
        if not report["full_1000_requested"]:
            print(f"SMOKE: {report['completed_sessions']} full logins + recovery probe; "
                  "1000-session acceptance NOT_RUN")
            return 0
        if not report["metrics_complete"]:
            print_stderr("INCOMPLETE: lifecycle completed but required resource measurements are missing")
            return 2
        print(f"MEASURED: {report['completed_sessions']} full logins + recovery probe; "
              "acceptance NOT_RUN (retained sessions not measured; no leak verdict)")
        return 0 if args.measure_only else 2
    except ResourceMeasurementUnavailable as exc:
        print_stderr(f"REG-195 INCOMPLETE: {exc}")
        return 2
    except (OSError, RuntimeError, ValueError, TimeoutError, subprocess.TimeoutExpired) as exc:
        print_stderr(f"REG-195 FAIL/INCOMPLETE: {exc}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
