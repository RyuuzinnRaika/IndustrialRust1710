#!/usr/bin/env python3
"""REG-195 / #831: opt-in 1,000 full Protocol 5 login/logout resource audit.

Only creates a fresh test world and binds 127.0.0.1. Not a DOS benchmark:
the default 3.2-second cadence stays below the 24-logins/minute per-IP gate.
Shortened --sessions runs are smoke tests, never evidence for 1,000 sessions.
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
    values = [sample[key] for sample in samples
              if isinstance(sample.get(key), int)]
    return {
        "sample_count": len(values),
        "initial": values[0] if values else None,
        "final": values[-1] if values else None,
        "delta": values[-1] - values[0] if len(values) >= 2 else None,
        "peak": max(values) if values else None,
    }


def save_report(output: Path, report: dict) -> None:
    (output / "manifest.json").write_text(
        json.dumps(report, indent=2) + "\n", encoding="utf-8")


def run(server: Path, output: Path, sessions: int, interval: float,
        max_rss_growth_mib: float | None) -> dict:
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
        "issue": 831, "regression": "REG-195",
        "started_utc": datetime.now(timezone.utc).isoformat(),
        "requested_sessions": sessions, "full_1000_requested": sessions >= 1000,
        "interval_seconds": interval, "server_binary": str(server),
        "platform": platform.platform(),
        "client_type": "synthetic native Protocol 5, not Java GUI",
        "completed_sessions": 0, "final_fresh_login": False,
        "samples": [], "verified": False,
    }
    child = None
    client = None
    with log_path.open("wb") as log:
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
                    raise RuntimeError("RSS measurement unavailable; threshold cannot be checked")
                if growth > max_rss_growth_mib * 1024 * 1024:
                    raise RuntimeError(f"RSS growth exceeded configured threshold: {growth}")
            shutdown(child)
            child = None
            if world.with_suffix(".pending-txn.json").exists():
                raise RuntimeError("pending transaction after orderly shutdown")
            verify = subprocess.run([str(server), "--verify-world", str(output)],
                                    cwd=output, capture_output=True, text=True, timeout=60)
            if verify.returncode:
                raise RuntimeError(f"read-only world verification failed: {verify.stderr[-1000:]}")
            report["verified"] = True
        finally:
            if client is not None:
                client.close()
            if child is not None:
                if child.poll() is None:
                    child.kill()
                child.wait(timeout=10)
            report["finished_utc"] = datetime.now(timezone.utc).isoformat()
            save_report(output, report)
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--sessions", type=int, default=1000)
    parser.add_argument("--interval-seconds", type=float, default=3.2)
    parser.add_argument("--max-rss-growth-mib", type=float, default=None)
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
                     args.interval_seconds, args.max_rss_growth_mib)
        print(f"PASS: {report['completed_sessions']} full logins + recovery probe")
        if not report["full_1000_requested"]:
            print("SMOKE ONLY: 1000-session acceptance NOT_RUN")
        return 0
    except (OSError, RuntimeError, ValueError, TimeoutError, subprocess.TimeoutExpired) as exc:
        print(f"REG-195 FAIL/INCOMPLETE: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
