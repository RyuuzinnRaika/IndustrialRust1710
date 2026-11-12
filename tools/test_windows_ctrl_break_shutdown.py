#!/usr/bin/env python3
"""REG-198 / #834: opt-in Windows console-control shutdown acceptance.

Run from an interactive Windows console (not a detached CI session):
    python tools/test_windows_ctrl_break_shutdown.py target\\debug\\vanilla-server.exe

Uses CTRL_BREAK_EVENT for deterministic process-group delivery; manual Ctrl+C
acceptance is still required separately. This is TEST ONLY, never a supervisor.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time


def wait_log(process: subprocess.Popen[str], log: Path, expected: str) -> None:
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        contents = log.read_text(encoding="utf-8", errors="replace")
        if expected in contents:
            return
        if process.poll() is not None:
            raise AssertionError(
                f"server exited before {expected!r}: {process.returncode}; "
                f"log: {contents[-2000:]}"
            )
        time.sleep(0.05)
    raise TimeoutError(f"server never printed {expected!r}; see {log}")


def start_server(executable: Path, root: Path, attempt: str) -> tuple[
    subprocess.Popen[str], Path, object, object
]:
    stdout_path = root / f"{attempt}.stdout.log"
    stderr_path = root / f"{attempt}.stderr.log"
    stdout = stdout_path.open("w", encoding="utf-8")
    stderr = stderr_path.open("w", encoding="utf-8")
    try:
        process = subprocess.Popen(
            [
                str(executable), "--bind", "127.0.0.1:0",
                "--world", str(root / "flat.irw"),
                "--whitelist", str(root / "whitelist.json"),
            ],
            stdin=subprocess.PIPE,
            stdout=stdout,
            stderr=stderr,
            text=True,
            encoding="utf-8",
            creationflags=subprocess.CREATE_NEW_PROCESS_GROUP,
        )
    except BaseException:
        stdout.close()
        stderr.close()
        raise
    return process, stdout_path, stdout, stderr


def command(process: subprocess.Popen[str], text: str) -> None:
    assert process.stdin is not None
    process.stdin.write(text + "\n")
    process.stdin.flush()


def stop_if_running(process: subprocess.Popen[str]) -> None:
    if process.poll() is None:
        process.kill()
        process.wait(timeout=10)
    if process.stdin is not None:
        process.stdin.close()


def run(executable: Path) -> None:
    if os.name != "nt":
        raise RuntimeError("This verifier must run on Windows")
    if not executable.is_file():
        raise FileNotFoundError(executable)
    executable = executable.resolve()
    with tempfile.TemporaryDirectory(prefix="ir1710-reg198-") as scratch:
        root = Path(scratch)
        (root / "whitelist.json").write_text(
            json.dumps([{"name": "RustTester"}]), encoding="utf-8"
        )
        first, first_log, first_out, first_err = start_server(
            executable, root, "ctrl-break"
        )
        try:
            wait_log(first, first_log, "listening on")
            command(first, "/setblock 2 70 2 stone")
            wait_log(first, first_log, "Block set at 2 70 2")
            # Unlike a hard kill, this delivers a Windows console event to
            # the dedicated process group. The server must drain and exit 0.
            first.send_signal(signal.CTRL_BREAK_EVENT)
            code = first.wait(timeout=30)
            assert code == 0, f"console-control exit code was {code}"
            assert "Server safely stopped;" in first_log.read_text(
                encoding="utf-8", errors="replace"
            ), "final world/journal flush log missing"
            assert (root / "flat.irw").is_file()
            assert not (root / "flat.pending-txn.json").exists()
        finally:
            stop_if_running(first)
            first_out.close()
            first_err.close()
        second, second_log, second_out, second_err = start_server(
            executable, root, "restarted"
        )
        try:
            wait_log(second, second_log, "listening on")
            command(second, "/testforblock 2 70 2 stone")
            wait_log(second, second_log, "testforblock: matched stone at 2 70 2")
            command(second, "/stop")
            assert second.wait(timeout=30) == 0, "normal /stop on reload failed"
        finally:
            stop_if_running(second)
            second_out.close()
            second_err.close()
    print("PASS: CTRL_BREAK_EVENT, clean exit, persisted edit, restart, /stop")
    print("NOT_RUN: physical keyboard Ctrl+C, power-loss/crash injection, Minecraft client")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("server_binary", type=Path)
    args = parser.parse_args()
    try:
        run(args.server_binary)
    except (OSError, AssertionError, RuntimeError, TimeoutError) as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
