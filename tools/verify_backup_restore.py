#!/usr/bin/env python3
"""J92: Verify an offline backup by restoring it into an isolated temporary tree.

This is a restore *drill*, not an in-place restore command or an online
consistency snapshot. Configure temporary locations outside all live worlds
and under operator control. Scratch candidates inside the backup are refused.
Concurrent replacement of temporary-directory path components is not guarded.
If no safe system temporary directory is usable, the drill fails rather than
falling back to the current working directory.
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


def _reject_reparse_point(metadata):
    # Windows junctions can look like ordinary directories to lstat and os.walk.
    # Reject all reparse points, including types other than junctions and links.
    if getattr(metadata, "st_file_attributes", 0) & 0x400:
        raise ValueError("backup contains a symlink or Windows reparse point")


def _marker_matches(marker: Path) -> bool:
    """Read no more than the expected marker and one trailing byte."""
    data = bytearray()
    with marker.open("rb") as stream:
        while len(data) < len(MARKER) + 1:
            chunk = stream.read(len(MARKER) + 1 - len(data))
            if chunk is None:
                raise BlockingIOError("marker read would block")
            if not chunk:
                break
            data.extend(chunk)
    return data == MARKER


def snapshot_hashes(root: Path) -> dict[str, str]:
    """Return a bounded content manifest; reject links and special files."""
    root_metadata = root.lstat()
    _reject_reparse_point(root_metadata)
    if not stat.S_ISDIR(root_metadata.st_mode):
        raise ValueError("backup root must be a real directory")
    files: dict[str, str] = {}
    size = 0
    for current, directories, names in os.walk(root, followlinks=False):
        for directory in directories:
            metadata = (Path(current) / directory).lstat()
            _reject_reparse_point(metadata)
            if not stat.S_ISDIR(metadata.st_mode):
                raise ValueError("backup contains a symlink or special directory")
        for name in names:
            path = Path(current) / name
            metadata = path.lstat()
            _reject_reparse_point(metadata)
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


def _safe_scratch_directory(backup: Path):
    """Create scratch outside the backup without probing unsafe temp paths."""
    backup_root = backup.resolve(strict=True)
    if tempfile.tempdir is not None:
        candidates = [os.fsdecode(tempfile.tempdir)]
    else:
        candidates = [os.environ.get(name) for name in ("TMPDIR", "TEMP", "TMP")]
        if os.name == "nt":
            candidates.append(os.path.expanduser(r"~\AppData\Local\Temp"))
            if system_root := os.environ.get("SYSTEMROOT"):
                candidates.append(os.path.join(system_root, "Temp"))
            candidates.extend((r"C:\temp", r"C:\tmp", r"\temp", r"\tmp"))
        else:
            candidates.extend(("/tmp", "/var/tmp", "/usr/tmp"))

    last_error = None
    for candidate in candidates:
        if not candidate:
            continue
        try:
            directory = Path(candidate).resolve(strict=True)
        except (OSError, RuntimeError) as error:
            last_error = error
            continue
        if not directory.is_dir():
            last_error = NotADirectoryError("temporary directory candidate is not a directory")
            continue
        if directory.is_relative_to(backup_root):
            raise ValueError("temporary directory must be outside the backup")
        try:
            # Supplying dir avoids gettempdir(), which probes by writing files.
            return tempfile.TemporaryDirectory(prefix="industrial1710-restore-drill-", dir=directory)
        except OSError as error:
            last_error = error
    raise OSError("no usable temporary directory outside the backup") from last_error


def verify_restore_point(server: Path, backup: Path, timeout: int = 120, *, native_stdout=None) -> int:
    """Verify a create-only backup on scratch storage without publishing a world."""
    if not stat.S_ISREG(server.lstat().st_mode):
        raise ValueError("server executable must be a regular file")
    backup_metadata = backup.lstat()
    _reject_reparse_point(backup_metadata)
    if not stat.S_ISDIR(backup_metadata.st_mode):
        raise ValueError("backup must be a real directory")
    marker = backup / ".industrial1710-backup"
    marker_metadata = marker.lstat()
    _reject_reparse_point(marker_metadata)
    if not stat.S_ISREG(marker_metadata.st_mode) or not _marker_matches(marker):
        raise ValueError("backup ownership marker is missing or invalid")
    # Scan before format detection, which otherwise follows level.dat links.
    before = snapshot_hashes(backup)
    anvil = (backup / "level.dat").is_file()
    irw_files = sorted(backup.glob("*.irw"))
    if (anvil and irw_files) or (not anvil and len(irw_files) != 1):
        raise ValueError("expected one IRW save or one Anvil level.dat, not mixed/ambiguous data")
    with _safe_scratch_directory(backup) as scratch:
        copy = Path(scratch) / "world"
        # Preserve links, *never dereference* them; the next scan rejects any.
        shutil.copytree(backup, copy, symlinks=True)
        after = snapshot_hashes(copy)
        if before != after or snapshot_hashes(backup) != before:
            raise ValueError("backup changed during restore drill or files differ")
        target = copy if anvil else copy / irw_files[0].name
        # Run the native verifier from disposable scratch, never from the
        # operator's current directory (which may contain a live world).
        # Make the server path absolute before changing its working directory.
        subprocess.run([str(server.resolve(strict=True)), "--verify-world", str(target)],
                       check=True, timeout=timeout, cwd=str(copy.parent), stdout=native_stdout)
        # The verifier is required to be read-only. A successful exit code
        # does not excuse accidental changes to the isolated restored copy.
        if snapshot_hashes(copy) != before:
            raise ValueError("native verifier modified the isolated restored world")
        # Also detect source mutation even if it occurred during verification.
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
