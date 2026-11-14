"""NEXT-193 / #1176: verify that a failed Rust server boot leaves world files intact.

This is a separate Python verifier, never a production server component.
The original fixture is read-only; corruption and boot happen solely in a
disposable temporary copy, bound to 127.0.0.1 on an ephemeral port.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import tempfile
from pathlib import Path
from typing import Callable

MAX_FIXTURE_BYTES = 128 * 1024 * 1024


def snapshot(root: Path) -> dict[str, str]:
    """Hash every file in the scratch world, including sidecars."""
    result: dict[str, str] = {}
    for path in root.rglob("*"):
        if path.is_symlink():
            raise ValueError("symlinks are forbidden in the isolated fixture")
        if path.is_file():
            digest = hashlib.sha256()
            with path.open("rb") as handle:
                for block in iter(lambda: handle.read(1024 * 1024), b""):
                    digest.update(block)
            result[path.relative_to(root).as_posix()] = digest.hexdigest()
    return dict(sorted(result.items()))


def probe_failed_boot(
    server: Path,
    fixture: Path,
    *,
    timeout: int = 20,
    runner: Callable[..., subprocess.CompletedProcess[bytes]] = subprocess.run,
) -> dict[str, object]:
    if not 1 <= timeout <= 60:
        raise ValueError("timeout must be 1..60 seconds")
    if fixture.is_symlink() or not fixture.is_file() or fixture.suffix.lower() != ".irw":
        raise ValueError("world-fixture must be a regular .irw file, not a symlink")
    if fixture.stat().st_size > MAX_FIXTURE_BYTES:
        raise ValueError("world fixture is too large for a disposable corruption test")
    if not server.is_file():
        raise ValueError("server binary must exist")

    with tempfile.TemporaryDirectory(prefix="ir1710-next193-") as isolated:
        root = Path(isolated)
        world = root / "corrupted.irw"
        shutil.copyfile(fixture, world)
        # Only corrupt the COPY; the original must never be used as --world.
        with world.open("r+b") as handle:
            handle.seek(0)
            handle.write(b"NEXT193_BAD_IRW_HEADER")
            handle.truncate(max(22, world.stat().st_size))
        whitelist = root / "whitelist.json"
        whitelist.write_text('[{"name":"RustTester"}]', encoding="utf-8")
        before = snapshot(root)
        cmd = [
            str(server), "--bind", "127.0.0.1:0", "--world", str(world),
            "--whitelist", str(whitelist),
        ]
        try:
            run = runner(
                cmd, cwd=root, stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                timeout=timeout, check=False,
            )
        except subprocess.TimeoutExpired as err:
            raise AssertionError("corrupt world was not rejected before timeout") from err
        after = snapshot(root)
        if run.returncode == 0:
            raise AssertionError("server accepted corrupted world instead of failing closed")
        if before != after:
            changed = sorted(set(before) ^ set(after) | {
                key for key in before.keys() & after.keys()
                if before[key] != after[key]
            })
            raise AssertionError(f"failed startup modified sandbox world files: {changed}")
        return {
            "result": "PASS",
            "kind": "failed-boot-world-immutability",
            "exit_code": run.returncode,
            "files_sha256": after,
            "source_fixture_unchanged": True,
            "actual_rust_server_tested": True,
        }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, required=True)
    parser.add_argument("--world-fixture", type=Path, required=True)
    parser.add_argument("--timeout", type=int, default=20)
    args = parser.parse_args()
    evidence = probe_failed_boot(
        args.server.expanduser().resolve(),
        args.world_fixture.expanduser().absolute(),
        timeout=args.timeout,
    )
    print(json.dumps(evidence, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
