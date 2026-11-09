#!/usr/bin/env python3
"""REG-187 / #823: opt-in real Rust server stop/restart session-lock race.

Starts one or more Rust vanilla-server subprocesses against a NEW isolated
world. Verifies the second process cannot take session.lock before the first
has completed its worker drain, then checks a legitimate restart/save.
Never uses an existing world or manually deletes its files.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import socket
import subprocess
import sys
import time

from e48_load_benchmark import (
    new_output_directory, reserve_loopback_port, shutdown,
    wait_until_listening,
)


def log_has_lock_busy(log: str) -> bool:
    lower = log.lower()
    return "session lock" in lower and "busy" in lower


def server_command(server: Path, root: Path, port: int) -> list[str]:
    return [
        str(server), "--bind", f"127.0.0.1:{port}",
        "--world", str(root / "exclusive.irw"),
        "--whitelist", str(root / "whitelist.json"),
    ]


def spawn(server: Path, root: Path, label: str) -> tuple[
    subprocess.Popen, Path, object, int
]:
    port = reserve_loopback_port()
    logfile = root / f"{label}.log"
    out = logfile.open("wb")
    try:
        child = subprocess.Popen(
            server_command(server, root, port),
            cwd=root, stdin=subprocess.PIPE,
            stdout=out, stderr=subprocess.STDOUT,
        )
    except BaseException:
        out.close()
        raise
    try:
        wait_until_listening(child, port)
    except BaseException:
        if child.poll() is None:
            child.kill()
        child.wait(timeout=10)
        out.close()
        raise
    return child, logfile, out, port


def wait_log(child: subprocess.Popen, log: Path, marker: str, offset: int = 0) -> None:
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if child.poll() is not None:
            raise RuntimeError(f"native server exited before {marker!r}: {child.returncode}")
        with log.open("rb") as data:
            data.seek(offset)
            if marker in data.read().decode("utf-8", "replace"):
                return
        time.sleep(0.05)
    raise TimeoutError(f"marker {marker!r} not observed in {log}")


def command(child: subprocess.Popen, text: str) -> None:
    if child.poll() is not None or child.stdin is None:
        raise RuntimeError("native server console unavailable")
    child.stdin.write((text + "\n").encode("utf-8"))
    child.stdin.flush()


def checked_command(child: subprocess.Popen, log: Path,
                    text: str, marker: str) -> None:
    offset = log.stat().st_size
    command(child, text)
    wait_log(child, log, marker, offset)


def attempt_competitor(server: Path, root: Path, label: str) -> dict:
    """An unjoined old process must fail lock acquisition, not accept clients."""
    result = subprocess.run(
        server_command(server, root, reserve_loopback_port()),
        cwd=root, stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        timeout=15, check=False,
    )
    log = result.stdout.decode("utf-8", "replace")
    (root / f"{label}.log").write_text(log, encoding="utf-8")
    if result.returncode == 0:
        raise RuntimeError(f"{label}: competitor exited successfully while old lock held")
    if not log_has_lock_busy(log):
        raise RuntimeError(
            f"{label}: second process failed for an unrelated reason (exit {result.returncode})"
        )
    return {
        "exit_code": result.returncode,
        "session_lock_busy": True,
        "log": f"{label}.log",
    }


def file_hash(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as inp:
        while part := inp.read(65536):
            digest.update(part)
    return digest.hexdigest()


def kill_if_running(child: subprocess.Popen | None) -> None:
    if child is None:
        return
    if child.poll() is None:
        child.kill()
    child.wait(timeout=10)


def run(server: Path, root: Path, commit: str) -> dict:
    new_output_directory(root)
    (root / "whitelist.json").write_text(
        json.dumps([{"name": "RustTester"}]), encoding="utf-8"
    )
    world = root / "exclusive.irw"
    report = {
        "issue": 823,
        "regression": "REG-187",
        "server_binary": str(server),
        "server_commit": commit,
        "started_utc": datetime.now(timezone.utc).isoformat(),
        "mode": "real Rust subprocess, loopback-only, NEW world",
        "cases": [], "verified": False,
    }
    first = None
    first_log_handle = None
    idle = None
    successor = None
    successor_log_handle = None
    try:
        first, log, first_log_handle, port = spawn(server, root, "original")
        checked_command(first, log, "/setblock 4 72 4 stone", "Block set at 4 72 4")
        if not world.is_file():
            raise RuntimeError("original server did not persist edited world")
        before = file_hash(world)
        attempt = attempt_competitor(server, root, "while-running")
        if file_hash(world) != before:
            raise RuntimeError("world changed during refused competing startup")
        report["cases"].append({"phase": "running", **attempt})

        # Keep an incomplete handshake socket alive while /stop joins the
        # 5-second socket-read worker. During that drain, the old session lock
        # must remain held; do not rely on a 1-ms scheduler race.
        idle = socket.create_connection(("127.0.0.1", port), timeout=5)
        time.sleep(0.2)
        checked_command(
            first, log, "/stop", "Server stopping; waiting for players and world workers"
        )
        if first.poll() is not None:
            raise RuntimeError("drain window ended before lock competitor probe")
        attempt = attempt_competitor(server, root, "during-stop")
        report["cases"].append({"phase": "shutdown_drain", **attempt})
        idle.close()
        idle = None
        if first.wait(timeout=30) != 0:
            raise RuntimeError("original server graceful /stop failed")
        if "Server safely stopped;" not in log.read_text(
                encoding="utf-8", errors="replace"):
            raise RuntimeError("original did not report final persisted checkpoint")
        first_log_handle.close()
        first_log_handle = None
        first = None

        # A new process may begin only after the original has *fully exited*.
        successor, successor_log, successor_log_handle, _ = spawn(
            server, root, "successor"
        )
        checked_command(
            successor, successor_log, "/testforblock 4 72 4 stone",
            "testforblock: matched stone at 4 72 4",
        )
        checked_command(
            successor, successor_log, "/setblock 4 72 4 dirt",
            "Block set at 4 72 4",
        )
        shutdown(successor)
        successor = None
        successor_log_handle.close()
        successor_log_handle = None
        after = file_hash(world)
        if after == before:
            raise RuntimeError("successor block update was not persisted")
        # A fully exited old process cannot write another generation's world.
        time.sleep(1)
        if file_hash(world) != after:
            raise RuntimeError("world changed after successful successor shutdown")
        report["cases"].append({
            "phase": "successor",
            "restarted": True,
            "prior_world_sha256": before,
            "new_world_sha256": after,
            "stable_after_seconds": 1,
        })

        check = subprocess.run(
            [str(server), "--verify-world", str(root)],
            cwd=root, capture_output=True, text=True, timeout=60,
        )
        if check.returncode:
            raise RuntimeError(f"read-only world verification failed: {check.stderr}")
        report["verified"] = True
        return report
    finally:
        if idle is not None:
            idle.close()
        kill_if_running(first)
        kill_if_running(successor)
        if first_log_handle is not None:
            first_log_handle.close()
        if successor_log_handle is not None:
            successor_log_handle.close()
        report["finished_utc"] = datetime.now(timezone.utc).isoformat()
        (root / "manifest.json").write_text(
            json.dumps(report, indent=2) + "\n", encoding="utf-8"
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--server-commit", default="UNKNOWN")
    args = parser.parse_args()
    try:
        server = args.server.resolve(strict=True)
        if not server.is_file():
            raise ValueError("native server binary missing")
        result = run(server, args.output.resolve(), args.server_commit)
        if not result["verified"]:
            raise RuntimeError("lock-shutdown test incomplete")
        print("PASS: lock refused during active run and drain; successor saved")
        return 0
    except (OSError, TimeoutError, ValueError, RuntimeError,
            subprocess.TimeoutExpired) as error:
        print(f"REG-187 FAIL/INCOMPLETE: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
