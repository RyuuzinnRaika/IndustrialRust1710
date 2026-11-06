#!/usr/bin/env python3
"""Read-only IRW save fixture classification and SHA-256 report (Issue #512).

A structurally plausible header is NOT proof of a valid or repairable world.
An optional native --verify-world check runs only on a disposable copy.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import stat
import subprocess
import sys
import tempfile
from pathlib import Path

MAX_BYTES = 64 * 1024 * 1024
MAX_EDITS = 65_536
RECORD_BYTES = 11  # x:i32, y:u8, z:i32, block:u16
HEADERS = {b"IRW1": 8, b"IRW2": 16, b"IRW3": 16, b"IRW4": 17}


class InspectionFailure(ValueError):
    pass


def _read_source(path: Path) -> tuple[str, int, bytes]:
    """Hash all data, but hold only its bounded header in memory."""
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode):
        raise InspectionFailure("source must be a regular file, not a symlink")
    if info.st_size > MAX_BYTES:
        raise InspectionFailure("source exceeds bounded inspection limit")
    digest = hashlib.sha256()
    size = 0
    prefix = bytearray()
    with path.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            size += len(chunk)
            if size > MAX_BYTES:
                raise InspectionFailure("source grew beyond bounded inspection limit")
            digest.update(chunk)
            if len(prefix) < 17:
                prefix.extend(chunk[:17 - len(prefix)])
    return digest.hexdigest(), size, bytes(prefix)


def classify(prefix: bytes, size: int) -> str:
    if len(prefix) < 4:
        return "truncated_header"
    magic = prefix[:4]
    if magic not in HEADERS:
        if re.fullmatch(rb"IRW[0-9]", magic):
            return "unsupported_version"
        return "invalid_magic"
    base = HEADERS[magic]
    if size < base:
        return "truncated_header"
    if magic == b"IRW4" and prefix[12] not in (0, 1, 255):
        return "invalid_dimension"
    count_offset = base - 4
    count = int.from_bytes(prefix[count_offset:count_offset + 4], "big")
    if count > MAX_EDITS:
        return "invalid_edit_count"
    if size < base + count * RECORD_BYTES:
        return "truncated_payload"
    # Additional bytes may be a valid TNT/extended trailer. Only the Rust
    # verifier can check these or semantic constraints in individual edits.
    return "requires_native_verification"


def _repair_action(classification: str) -> str:
    if classification == "native_verified":
        return "none_required"
    if classification in ("requires_native_verification",):
        return "run_native_verifier"
    if classification == "unsupported_version":
        return "manual_review_unknown_version"
    return "restore_from_verified_backup_only"


def _verify_on_copy(server: Path, original: Path, timeout: int,
                    expected_sha256: str, expected_size: int) -> bool:
    if not server.is_file():
        raise InspectionFailure("native server binary not found")
    with tempfile.TemporaryDirectory(prefix="ir1710-save-fixture-") as scratch:
        target = Path(scratch) / "fixture.irw"
        # The fixture can change between initial inspection and this copy.
        # Never report that a different set of bytes passed native validation.
        copied_sha256 = hashlib.sha256()
        total = 0
        with original.open("rb") as source, target.open("xb") as destination:
            while chunk := source.read(1024 * 1024):
                total += len(chunk)
                if total > MAX_BYTES:
                    raise InspectionFailure("source grew during verification copy")
                copied_sha256.update(chunk)
                destination.write(chunk)
        if total != expected_size or copied_sha256.hexdigest() != expected_sha256:
            raise InspectionFailure("source changed before native verification copy")
        try:
            result = subprocess.run(
                [str(server.resolve()), "--verify-world", str(target)],
                cwd=scratch, capture_output=True, timeout=timeout, check=False,
            )
        except subprocess.TimeoutExpired:
            return False
        # Even an accidental write by the verifier must not be mistaken for
        # successful validation of the original, pre-verified fixture.
        verified_sha256, verified_size, _ = _read_source(target)
        if (verified_sha256, verified_size) != (expected_sha256, expected_size):
            raise InspectionFailure("native verification altered copied fixture")
        # Do not print arbitrary subprocess stdout/stderr: it can contain
        # paths, personal data or externally supplied payloads.
        return result.returncode == 0


def inspect(path: Path, expected_sha256: str | None = None,
            server: Path | None = None, timeout: int = 30) -> dict:
    if expected_sha256 is not None and not re.fullmatch(r"[a-fA-F0-9]{64}", expected_sha256):
        raise InspectionFailure("expected SHA-256 must be exactly 64 hex digits")
    if not 1 <= timeout <= 600:
        raise InspectionFailure("timeout must be 1..600 seconds")
    original_hash, size, prefix = _read_source(path)
    classification = classify(prefix, size)
    native = "NOT_RUN"
    if expected_sha256 is not None and original_hash.lower() != expected_sha256.lower():
        classification = "checksum_mismatch"
    elif classification == "requires_native_verification" and server is not None:
        native_ok = _verify_on_copy(server, path, timeout, original_hash, size)
        native = "PASS" if native_ok else "FAIL"
        classification = "native_verified" if native_ok else "native_failed"
    after_hash, after_size, _ = _read_source(path)
    if (original_hash, size) != (after_hash, after_size):
        raise InspectionFailure("source changed during read-only inspection")
    return {
        "schema": "industrialrust1710.save-fixture.v1",
        "format_hint": prefix[:4].decode("ascii", "backslashreplace"),
        "size_bytes": size,
        "sha256": original_hash,
        "expected_sha256": expected_sha256.lower() if expected_sha256 else None,
        "classification": classification,
        "repair_action": _repair_action(classification),
        "native_verifier": native,
        "original_unchanged": True,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--file", type=Path, required=True)
    parser.add_argument("--expected-sha256")
    parser.add_argument("--server", type=Path)
    parser.add_argument("--timeout", type=int, default=30)
    args = parser.parse_args(argv)
    try:
        result = inspect(args.file, args.expected_sha256, args.server, args.timeout)
    except (OSError, InspectionFailure) as error:
        print("SAVE_FIXTURE_FAILED: " + str(error), file=sys.stderr)
        return 1
    print(json.dumps(result, sort_keys=True, ensure_ascii=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
