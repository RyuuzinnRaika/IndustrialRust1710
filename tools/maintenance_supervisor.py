#!/usr/bin/env python3
"""Opt-in J92 maintenance supervisor: stop, verify, snapshot, restart.

Only manages the server process it launches. It never attempts a hot copy.
Requires Python 3.9+ and an existing vanilla-server executable.
"""
from __future__ import annotations

import argparse
import shutil
import signal
import stat
import subprocess
import sys
import threading
import time
from pathlib import Path


# Must match the Rust backup_rotation::MARKER exactly. Only the native
# offline rotator can create a marked generation; unrelated folders survive.
BACKUP_MARKER = b"IndustrialRust1710 verified offline backup v1\n"


def backup_names(root: Path) -> set[str]:
    """Snapshot direct entries without following links or trusting markers."""
    try:
        if not stat.S_ISDIR(root.lstat().st_mode):
            raise RuntimeError("backup root must be a real directory")
    except FileNotFoundError:
        return set()
    return {entry.name for entry in root.iterdir() if entry.name.startswith("backup-")}


def owned_generations(root: Path) -> list[Path]:
    """Only count owned, marked, direct-child backup directories."""
    try:
        if not stat.S_ISDIR(root.lstat().st_mode):
            raise RuntimeError("backup root must be a real directory")
    except FileNotFoundError:
        return []
    owned = []
    for path in root.iterdir():
        if not path.name.startswith("backup-") or not stat.S_ISDIR(path.lstat().st_mode):
            continue
        marker = path / ".industrial1710-backup"
        try:
            if stat.S_ISREG(marker.lstat().st_mode) and marker.read_bytes() == BACKUP_MARKER:
                owned.append(path)
        except FileNotFoundError:
            continue
    return sorted(owned)


def prune_verified_backups(root: Path, keep: int, new: Path) -> None:
    """Only prune after an isolated restore drill has completed successfully."""
    owned = owned_generations(root)
    if new not in owned:
        raise RuntimeError("verified new backup disappeared before retention")
    # Always keep the newly proven restorable generation, even if the system
    # clock moved backwards so its filename sorts before older snapshots.
    deletable = [path for path in owned if path != new]
    for old in deletable[:max(0, len(owned) - keep)]:
        # Recheck immediately before deleting. Never follow a replaced link.
        marker = old / ".industrial1710-backup"
        if not stat.S_ISDIR(old.lstat().st_mode) or not stat.S_ISREG(marker.lstat().st_mode):
            raise RuntimeError("backup changed during retention; refusing removal")
        if marker.read_bytes() != BACKUP_MARKER:
            raise RuntimeError("backup ownership changed; refusing removal")
        shutil.rmtree(old)


def maintenance(server: Path, world: Path, backups: Path, keep: int) -> None:
    """Fail closed: stop, verify, snapshot, restore-drill, then prune/restart."""
    subprocess.run([str(server), "--verify-world", str(world)], check=True)
    before = backup_names(backups)
    prior = owned_generations(backups)
    # The native rotator prunes *before* an external restore drill. Grant
    # enough temporary capacity to protect every older generation until the
    # new snapshot has passed the drill. The native CLI caps retention at 100.
    if len(prior) >= 100:
        raise RuntimeError("100 backup generations exist; manual archival is required")
    staging_keep = max(keep, len(prior) + 1)
    subprocess.run(
        [str(server), "--world", str(world),
         "--backup-rotate", str(backups), str(staging_keep)],
        check=True,
    )
    created = backup_names(backups) - before
    if len(created) != 1:
        raise RuntimeError("cannot uniquely identify the new backup for restore drill")
    restored = backups / created.pop()
    after = owned_generations(backups)
    if restored not in after or not set(prior).issubset(after):
        raise RuntimeError("new backup is unmarked or older generations changed")
    subprocess.run([
        sys.executable, str(Path(__file__).with_name("verify_backup_restore.py")),
        "--server", str(server), "--backup", str(restored),
    ], check=True)
    # No older generation is removed if the world, backup, or restore drill
    # fails. Preserve unrelated unmarked directories during retention.
    prune_verified_backups(backups, keep, restored)


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
