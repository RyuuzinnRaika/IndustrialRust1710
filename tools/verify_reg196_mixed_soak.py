#!/usr/bin/env python3
"""REG-196 / #832: opt-in 24-hour synthetic Protocol 5 mixed soak.

Creates a fresh test world, never touches player-owned worlds. Shortened runs
are smoke tests; they do not constitute 24-hour acceptance evidence.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import json
import math
from pathlib import Path
import platform
import struct
import subprocess
import sys
import time

from e48_load_benchmark import (
    SyntheticClient, new_output_directory, reserve_loopback_port,
    wait_until_listening, shutdown,
)


def wait_log(child: subprocess.Popen, log: Path, needle: str, offset: int = 0) -> None:
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if child.poll() is not None:
            raise RuntimeError(f"server exited before {needle!r}: {child.returncode}")
        with log.open("rb") as handle:
            handle.seek(offset)
            if needle in handle.read().decode("utf-8", "replace"):
                return
        time.sleep(0.05)
    raise TimeoutError(f"missing log marker {needle!r}; inspect {log}")


def command(child: subprocess.Popen, text: str) -> None:
    if child.poll() is not None or child.stdin is None:
        raise RuntimeError("server console unavailable")
    child.stdin.write((text + "\n").encode("utf-8"))
    child.stdin.flush()


def checked_command(child: subprocess.Popen, log: Path, text: str, success: str) -> None:
    offset = log.stat().st_size
    command(child, text)
    wait_log(child, log, success, offset)


def login(port: int) -> SyntheticClient:
    for attempt in range(5):
        client = SyntheticClient(port, "SoakTester")
        if client.ready.wait(15) and not client.failure:
            return client
        reason = client.failure or "login timed out"
        client.close()
        if attempt == 4:
            raise RuntimeError(f"login failed: {reason}")
        time.sleep(1)
    raise AssertionError("unreachable")


def mine(client: SyntheticClient) -> None:
    # C07 Start + Finish on the dirt block at (1,63,0), same wire shape as
    # protocol5_e2e. This target is within reach but not under the spawn point.
    request = bytes([0]) + struct.pack(">iBiB", 1, 63, 0, 1)
    client.send(7, request)
    time.sleep(1.0)
    client.send(7, bytes([2]) + request[1:])


def proc_metrics(pid: int) -> dict:
    data = {"rss_bytes": None, "threads": None, "fd_count": None}
    try:
        for line in Path(f"/proc/{pid}/status").read_text().splitlines():
            if line.startswith("VmRSS:"):
                data["rss_bytes"] = int(line.split()[1]) * 1024
            if line.startswith("Threads:"):
                data["threads"] = int(line.split()[1])
        data["fd_count"] = len(list(Path(f"/proc/{pid}/fd").iterdir()))
    except (OSError, ValueError, IndexError):
        pass
    return data


def summarize_telemetry(path: Path) -> dict:
    if not path.is_file():
        raise RuntimeError("world-worker telemetry missing")
    rows = [json.loads(line) for line in path.read_text().splitlines() if line.strip()]
    windows = [r for r in rows if r.get("schema_version") == 2 and r.get("window_seconds", 0) > 0]
    if not windows:
        raise RuntimeError("no valid schema v2 telemetry windows")
    duration = sum(r["window_seconds"] for r in windows)
    rss = [r["process_rss_bytes"] for r in windows if isinstance(r.get("process_rss_bytes"), int)]
    return {
        "windows": len(windows),
        "window_seconds": duration,
        "tps_processed": sum(r["processed_ticks"] for r in windows) / duration,
        "missed_ticks": sum(r["missed_ticks"] for r in windows),
        "mspt_p99_worst": max(r["mspt_p99"] for r in windows),
        "rss_peak": max(rss) if rss else None,
        "rss_delta_first_last": rss[-1] - rss[0] if len(rss) > 1 else None,
    }


def run(server: Path, output: Path, seconds: float, cycle: float) -> dict:
    new_output_directory(output)  # create only; fail if path already exists
    world = output / "soak.irw"
    whitelist = output / "whitelist.json"
    whitelist.write_text(json.dumps([{"name": "SoakTester"}]), encoding="utf-8")
    port = reserve_loopback_port()
    cmd = [str(server), "--bind", f"127.0.0.1:{port}", "--survival",
           "--world", str(world), "--whitelist", str(whitelist)]
    log_path = output / "server.log"
    report = {
        "issue": 832, "regression": "REG-196",
        "started_utc": datetime.now(timezone.utc).isoformat(),
        "requested_seconds": seconds, "full_24h_requested": seconds >= 86400,
        "platform": platform.platform(), "server_binary": str(server),
        "client": "synthetic Protocol 5; unmodified Java 1.7.10 NOT_RUN",
        "actions": {"building": 0, "mining_verified": 0, "reconnects": 0,
                    "save_verified": 0, "movement": 0},
        "samples": [], "verified": False,
    }
    child = None
    client = None
    with log_path.open("wb") as log:
        try:
            child = subprocess.Popen(cmd, cwd=output, stdin=subprocess.PIPE,
                                     stdout=log, stderr=subprocess.STDOUT)
            wait_until_listening(child, port)
            client = login(port)
            start = time.monotonic()
            next_sample = start
            index = 0
            while time.monotonic() < start + seconds:
                if child.poll() is not None:
                    raise RuntimeError(f"server exited under load: {child.returncode}")
                if client.failure or not client.thread.is_alive():
                    raise RuntimeError(client.failure or "Protocol 5 reader stopped")
                block = "stone" if index % 2 == 0 else "dirt"
                checked_command(child, log_path,
                                f"/setblock 4 72 4 {block}", "Block set at 4 72 4")
                report["actions"]["building"] += 1
                if index % 5 == 0:
                    checked_command(child, log_path,
                                    "/setblock 1 63 0 dirt", "Block set at 1 63 0")
                    mine(client)
                    checked_command(child, log_path, "/testforblock 1 63 0 air",
                                    "testforblock: matched air at 1 63 0")
                    report["actions"]["mining_verified"] += 1
                    checked_command(child, log_path, "/save", "World saved")
                    report["actions"]["save_verified"] += 1
                client.move(index)
                report["actions"]["movement"] += 1
                now = time.monotonic()
                if now >= next_sample:
                    report["samples"].append({
                        "elapsed_seconds": round(now - start, 2), **proc_metrics(child.pid),
                        "packets": client.packets, "keepalives": client.keepalives,
                    })
                    next_sample = now + 60
                # Each cycle exercises a real authenticated login state and
                # disconnect; no raw TCP connect is counted as reconnect.
                client.close()
                client = None
                time.sleep(0.15)
                client = login(port)
                report["actions"]["reconnects"] += 1
                index += 1
                time.sleep(max(0, min(start + index * cycle, start + seconds)
                               - time.monotonic()))
            report["elapsed_seconds"] = time.monotonic() - start
            client.close()
            client = None
            shutdown(child)
            child = None
            if world.with_suffix(".pending-txn.json").exists():
                raise RuntimeError("pending journal remained after clean shutdown")
            restart_path = output / "restart.log"
            with restart_path.open("wb") as restart_log:
                child = subprocess.Popen(cmd, cwd=output, stdin=subprocess.PIPE,
                                         stdout=restart_log, stderr=subprocess.STDOUT)
                wait_until_listening(child, port)
                expected = "stone" if (index - 1) % 2 == 0 else "dirt"
                checked_command(child, restart_path,
                                f"/testforblock 4 72 4 {expected}",
                                f"testforblock: matched {expected} at 4 72 4")
                shutdown(child)
                child = None
            verify = subprocess.run([str(server), "--verify-world", str(output)],
                                    cwd=output, capture_output=True, text=True, timeout=60)
            if verify.returncode != 0:
                raise RuntimeError(f"read-only world validation failed: {verify.stderr[-1000:]}")
            report["telemetry"] = summarize_telemetry(world.with_suffix(".metrics.jsonl"))
            diagnostics = [line[:300] for line in log_path.read_text(
                encoding="utf-8", errors="replace").splitlines()
                if any(tag in line.upper() for tag in ("ERROR", "PANIC", "FAILED"))]
            report["diagnostics"] = {
                "error_like_lines": len(diagnostics), "first_20": diagnostics[:20]}
            report["verified"] = True
        finally:
            if client is not None:
                client.close()
            if child is not None:
                if child.poll() is None:
                    child.kill()
                child.wait(timeout=10)
            report["finished_utc"] = datetime.now(timezone.utc).isoformat()
            (output / "manifest.json").write_text(
                json.dumps(report, indent=2) + "\n", encoding="utf-8")
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path,
                        help="new, nonexistent directory for test data")
    parser.add_argument("--seconds", type=float, default=86400)
    parser.add_argument("--cycle-seconds", type=float, default=60)
    args = parser.parse_args()
    if not (math.isfinite(args.seconds) and 15 <= args.seconds <= 172800):
        parser.error("--seconds must be within 15..172800")
    if not (math.isfinite(args.cycle_seconds) and 5 <= args.cycle_seconds <= 3600):
        parser.error("--cycle-seconds must be within 5..3600")
    try:
        server = args.server.resolve(strict=True)
        if not server.is_file():
            raise ValueError("server binary is not a file")
        report = run(server, args.output.resolve(), args.seconds, args.cycle_seconds)
        print(f"PASS: {report['elapsed_seconds']:.1f}s synthetic soak")
        if not report["full_24h_requested"]:
            print("SMOKE ONLY: 24-hour run NOT_RUN")
        return 0
    except (OSError, RuntimeError, ValueError, TimeoutError, subprocess.TimeoutExpired) as error:
        print(f"REG-196 FAIL/INCOMPLETE: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
