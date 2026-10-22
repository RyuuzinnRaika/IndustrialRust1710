#!/usr/bin/env python3
"""J92: Verify an offline backup by restoring it into an isolated temporary tree.

Never writes to the original world or the backup. This is a restore *drill*,
not an in-place restore command or an online consistency snapshot.
"""
from __future__ import annotations

import argparse
import hashlib
import os
from pathlib import Path
import shutil
import stat
import subprocess
import tempfile

MARKER = b"IndustrialRust1710 verified offline backup v1\n"
MAX_FILES = 100_000
MAX_BYTES = 32 * 1024 * 1024 * 1024


def snapshot_hashes(root: Path) -> dict[str, str]:
    """Return a bounded content manifest; reject links and special files."""
    if not stat.S_ISDIR(root.lstat().st_mode):
        raise ValueError("backup root must be a real directory")
    files: dict[str, str] = {}
    size = 0
    for current, directories, names in os.walk(root, followlinks=False):
        for directory in directories:
            if not stat.S_ISDIR((Path(current) / directory).lstat().st_mode):
                raise ValueError("backup contains a symlink or special directory")
        for name in names:
            path = Path(current) / name
            metadata = path.lstat()
            if not stat.S_ISREG(metadata.st_mode):
                raise ValueError("backup contains a symlink or special file")
            size += metadata.st_size
            if size > MAX_BYTES or len(files) >= MAX_FILES:
                raise ValueError("backup exceeds bounded restore verification limits")
            digest = hashlib.sha256()
            with path.open("rb") as stream:
                for block in iter(lambda: stream.read(1024 * 1024), b""):
                    digest.update(block)
            files[path.relative_to(root).as_posix()] = digest.hexdigest()
    return files


def verify_restore_point(server: Path, backup: Path, timeout: int = 120) -> int:
    """Verify a create-only backup on scratch storage without publishing a world."""
    if not stat.S_ISREG(server.lstat().st_mode):
        raise ValueError("server executable must be a regular file")
    if not stat.S_ISDIR(backup.lstat().st_mode):
        raise ValueError("backup must be a real directory")
    marker = backup / ".industrial1710-backup"
    if not stat.S_ISREG(marker.lstat().st_mode) or marker.read_bytes() != MARKER:
        raise ValueError("backup ownership marker is missing or invalid")
    if not ((backup / "flat.irw").is_file() or (backup / "level.dat").is_file()):
        raise ValueError("no supported IRW or Anvil world in backup")
    before = snapshot_hashes(backup)
    with tempfile.TemporaryDirectory(prefix="industrial1710-restore-drill-") as scratch:
        copy = Path(scratch) / "world"
        # Preserve links, *never dereference* them; the next scan rejects any.
        shutil.copytree(backup, copy, symlinks=True)
        after = snapshot_hashes(copy)
        if before != after or snapshot_hashes(backup) != before:
            raise ValueError("backup changed during restore drill or files differ")
        subprocess.run([str(server), "--verify-world", str(copy)],
                       check=True, timeout=timeout)
        # Detect source mutation even if it occurred while the native verifier ran.
        if snapshot_hashes(backup) != before:
            raise ValueError("backup changed during offline world inspection")
    return len(before)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, required=True)
    parser.add_argument("--backup", type=Path, required=True)
    parser.add_argument("--timeout", type=int, default=120)
    args = parser.parse_args()
    if not 5 <= args.timeout <= 600:
        parser.error("--timeout must be 5..600 seconds")
    try:
        count = verify_restore_point(args.server, args.backup, args.timeout)
    except (OSError, ValueError, subprocess.SubprocessError) as exc:
        parser.exit(1, f"RESTORE DRILL FAILED (original untouched): {exc}\n")
    print(f"Restore drill PASS: {count} byte-identical files; read-only world verification passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
