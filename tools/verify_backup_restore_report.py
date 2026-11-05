#!/usr/bin/env python3
"""Issue #523: report a verified, isolated backup restore drill as JSON.

Read-only for the original backup and world. The existing J92 verifier owns
copying to a disposable temporary directory and invoking --verify-world.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import stat
import subprocess
import sys

import verify_backup_restore as verifier


def report_restore_point(server: Path, backup: Path, timeout: int = 120) -> dict:
    """Return a machine-readable report only after the native restore drill passes."""
    # A pre/post snapshot prevents a report about bytes other than those
    # supplied to the existing verifier, without writing into the backup.
    initial = verifier.snapshot_hashes(backup)
    file_count = verifier.verify_restore_point(server, backup, timeout)
    if len(initial) != file_count:
        raise ValueError("backup file count changed during restore drill")
    if verifier.snapshot_hashes(backup) != initial:
        raise ValueError("backup content changed during restore drill")

    contents = []
    total_bytes = 0
    for relative, sha256 in sorted(initial.items()):
        metadata = (backup / relative).lstat()
        if not stat.S_ISREG(metadata.st_mode):
            raise ValueError("backup contains a symlink or special file")
        total_bytes += metadata.st_size
        contents.append({"path": relative, "bytes": metadata.st_size, "sha256": sha256})
    # Reject any size/content change during report generation as well.
    if verifier.snapshot_hashes(backup) != initial:
        raise ValueError("backup content changed while assembling report")
    if total_bytes > verifier.MAX_BYTES or len(contents) > verifier.MAX_FILES:
        raise ValueError("backup exceeds reporting limits")

    manifest = json.dumps(contents, sort_keys=True, ensure_ascii=True, separators=(",", ":"))
    return {
        "schema": "industrialrust1710.restore-report.v1",
        "status": "PASS",
        "backup_generation_directory": backup.name,
        "world_format": "Anvil" if "level.dat" in initial else "IRW",
        "file_count": file_count,
        "total_bytes": total_bytes,
        "files": contents,
        "manifest_sha256": hashlib.sha256(manifest.encode("utf-8")).hexdigest(),
        "verified_with": "native --verify-world on an isolated temporary copy",
        "source_bytes_unchanged": True,
    }


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, required=True)
    parser.add_argument("--backup", type=Path, required=True)
    parser.add_argument("--timeout", type=int, default=120)
    args = parser.parse_args(argv)
    if not 5 <= args.timeout <= 600:
        parser.error("--timeout must be 5..600 seconds")
    try:
        report = report_restore_point(args.server, args.backup, args.timeout)
    except (OSError, ValueError, subprocess.SubprocessError) as exc:
        print(f"RESTORE_REPORT_FAILED: {exc}", file=sys.stderr)
        return 1
    print(json.dumps(report, sort_keys=True, ensure_ascii=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
