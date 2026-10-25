#!/usr/bin/env python3
"""Opt-in J92 maintenance supervisor: stop, verify, snapshot, restart.

Only manages the server process it launches. It never attempts a hot copy.
Requires Python 3.9+ and an existing vanilla-server executable.
"""
from __future__ import annotations

import argparse
import signal
import stat
import subprocess
import sys
import threading
import time
from pathlib import Path


def backup_names(root: Path) -> set[str]:
    """Snapshot direct entries without following links or trusting markers."""
    try:
        children = list(root.iterdir())
    except FileNotFoundError:
        return set()
    if not stat.S_ISDIR(root.lstat().st_mode):
        raise RuntimeError("backup root must be a real directory")
    return {entry.name for entry in children if entry.name.startswith("backup-")}


def maintenance(server: Path, world: Path, backups: Path, keep: int) -> None:
    """Fail closed: stop, verify, rotate, restore-drill, then allow restart."""
    subprocess.run([str(server), "--verify-world", str(world)], check=True)
    before = backup_names(backups)
    subprocess.run(
        [str(server), "--world", str(world), "--backup-rotate", str(backups), str(keep)],
        check=True,
    )
    created = backup_names(backups) - before
    if len(created) != 1:
        raise RuntimeError("cannot uniquely identify the new backup for restore drill")
    restored = backups / created.pop()
    subprocess.run([
        sys.executable, str(Path(__file__).with_name("verify_backup_restore.py")),
        "--server", str(server), "--backup", str(restored),
    ], check=True)


class Supervisor:
    def __init__(self, command: list[str], server: Path, world: Path,
                 backups: Path, keep: int, interval: float, stop_timeout: float):
        self.command = command
        self.server = server
        self.world = world
        self.backups = backups
        self.keep = keep
        self.interval = interval
        self.stop_timeout = stop_timeout
        self.halt = threading.Event()
        self.guard = threading.Lock()
        self.child: subprocess.Popen | None = None

    def _launch(self) -> subprocess.Popen:
        child = subprocess.Popen(self.command, stdin=subprocess.PIPE)
        with self.guard:
            self.child = child
        print(f"[maintenance] started server PID {child.pid}", flush=True)
        return child

    def _console(self) -> None:
        # Terminal commands still reach the child, including a manual /stop.
        # No command is interpreted as privileged by this Python wrapper.
        for line in sys.stdin:
            with self.guard:
                child = self.child
                if child is None or child.poll() is not None or child.stdin is None:
                    continue
                try:
                    child.stdin.write(line.encode("utf-8"))
                    child.stdin.flush()
                except (BrokenPipeError, OSError):
                    pass

    def _stop(self, child: subprocess.Popen) -> None:
        if child.poll() is not None:
            raise RuntimeError("server stopped unexpectedly; backup aborted")
        with self.guard:
            if child.stdin is None:
                raise RuntimeError("server console is unavailable")
            child.stdin.write(b"/stop\n")
            child.stdin.flush()
        try:
            code = child.wait(timeout=self.stop_timeout)
        except subprocess.TimeoutExpired as exc:
            # Do not force-kill or snapshot a possibly dirty world.
            raise RuntimeError("graceful /stop timed out; backup aborted") from exc
        if code != 0:
            raise RuntimeError(f"server /stop exited with status {code}")
        with self.guard:
            self.child = None
        print("[maintenance] server stopped cleanly", flush=True)

    def run(self) -> int:
        threading.Thread(target=self._console, name="console-forward", daemon=True).start()
        child = self._launch()
        next_backup = time.monotonic() + self.interval
        try:
            while not self.halt.is_set():
                if child.poll() is not None:
                    # Intentional console /stop is a terminal operation, not a restart.
                    print("[maintenance] server exited; supervisor terminating", flush=True)
                    return child.returncode or 0
                remaining = next_backup - time.monotonic()
                if remaining > 0:
                    self.halt.wait(min(remaining, 1.0))
                    continue
                self._stop(child)
                print("[maintenance] validating, rotating and restore-drilling offline world", flush=True)
                maintenance(self.server, self.world, self.backups, self.keep)
                child = self._launch()
                next_backup = time.monotonic() + self.interval
            # External SIGINT/SIGTERM requests graceful shutdown without backup.
            if child.poll() is None:
                self._stop(child)
            return 0
        except (OSError, subprocess.CalledProcessError, RuntimeError) as exc:
            print(f"[maintenance] FAILED CLOSED: {exc}", file=sys.stderr, flush=True)
            # Never restart after a failed verification, stop, or backup.
            return 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", required=True, type=Path)
    parser.add_argument("--world", required=True, type=Path)
    parser.add_argument("--backups", required=True, type=Path)
    parser.add_argument("--keep", type=int, default=7)
    parser.add_argument("--every-hours", type=float, default=24)
    parser.add_argument("--stop-timeout", type=float, default=60)
    parser.add_argument("server_args", nargs=argparse.REMAINDER)
    args = parser.parse_args()

    if not 1 <= args.keep <= 100:
        parser.error("--keep must be 1..100")
    if not 1 <= args.every_hours <= 24 * 30:
        parser.error("--every-hours must be 1..720")
    if not 5 <= args.stop_timeout <= 600:
        parser.error("--stop-timeout must be 5..600")
    server = args.server.resolve(strict=True)
    if not server.is_file():
        parser.error("--server is not a regular file")
    world = args.world.resolve(strict=False)
    backups = args.backups.resolve(strict=False)
    if backups == world.parent or backups.is_relative_to(world.parent) or world.parent.is_relative_to(backups):
        parser.error("backup tree must not overlap the world tree")
    extra = args.server_args
    if extra and extra[0] == "--":
        extra = extra[1:]
    forbidden = {"--world", "--backup-rotate", "--backup-world",
                 "--verify-world", "--prepare-anvil-copy"}
    if forbidden.intersection(extra):
        parser.error("duplicate world or offline operation in server arguments")
    command = [str(server), "--world", str(world), *extra]
    runner = Supervisor(command, server, world, backups, args.keep,
                        args.every_hours * 3600, args.stop_timeout)
    for signum in (signal.SIGINT, signal.SIGTERM):
        signal.signal(signum, lambda _sig, _frame: runner.halt.set())
    return runner.run()


if __name__ == "__main__":
    sys.exit(main())
