#!/usr/bin/env python3
"""REG-188 / #824: verify native Protocol 5 player save/rejoin at negative X/Z.

Fresh loopback-only creative test world; no existing save is reused. Performs
a genuine server restart after each negative coordinate. Synthetic client
results are not unmodified Minecraft 1.7.10 GUI acceptance evidence.
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
    shutdown, wait_until_listening,
)

NEGATIVE = (-16.0, -1.0, -1000.0)
ITEM_ID = 1
ITEM_COUNT = 8
PLAYER = "Positioner"
# The native server's S08 spawn packet carries the *client* position, whose
# Y is the player's eye height: server feet Y + 1.62 (see send_player_position
# and read_position in the Rust suite). Synthetic clients must undo that
# offset before comparing S08 against the feet coordinates they reported.
CLIENT_EYE_OFFSET = 1.62


def server_feet(position):
    """Translate a raw S08 position into the server's authoritative feet Y."""
    if position is None:
        return None
    return (position[0], position[1] - CLIENT_EYE_OFFSET, position[2])


def inventory_count(path: Path, item_id: int) -> int:
    """Only count *actual persistent* player slots, not a GUI preview."""
    data = json.loads(path.read_text(encoding="utf-8"))
    slots = data.get("slots")
    if not isinstance(slots, list) or len(slots) != 45:
        raise ValueError("invalid player inventory slot count")
    total = 0
    for entry in slots:
        if entry is None:
            continue
        if not isinstance(entry, dict) or not isinstance(entry.get("count"), int):
            raise ValueError("invalid player ItemStack")
        if entry.get("id") == item_id:
            total += entry["count"]
    return total


def wait_inventory(path: Path, count: int) -> None:
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        try:
            if inventory_count(path, ITEM_ID) == count:
                return
        except (OSError, ValueError, KeyError, json.JSONDecodeError):
            pass
        time.sleep(0.05)
    raise RuntimeError(f"inventory sidecar did not save {count} input items: {path}")


def position_matches(actual: tuple[float, float, float] | None,
                     x: float, y: float, z: float) -> bool:
    return actual is not None and all(
        math.isfinite(value) and abs(value - wanted) <= 1e-6
        for value, wanted in zip(actual, (x, y, z))
    )


def start(server: Path, root: Path, port: int, log: Path) -> tuple[subprocess.Popen, object]:
    logfile = log.open("wb")
    try:
        child = subprocess.Popen(
            [str(server), "--bind", f"127.0.0.1:{port}",
             "--world", str(root / "isolated.irw"),
             "--whitelist", str(root / "whitelist.json")],
            cwd=root, stdin=subprocess.PIPE, stdout=logfile,
            stderr=subprocess.STDOUT,
        )
    except BaseException:
        logfile.close()
        raise
    try:
        wait_until_listening(child, port)
    except BaseException:
        if child.poll() is None:
            child.kill()
        child.wait(timeout=10)
        logfile.close()
        raise
    return child, logfile


def connect(port: int) -> SyntheticClient:
    client = SyntheticClient(port, PLAYER)
    if not client.ready.wait(30) or client.failure:
        reason = client.failure or "login timed out"
        client.close()
        raise RuntimeError(f"Protocol 5 Play admission failed: {reason}")
    return client


def send_console(child: subprocess.Popen, text: str) -> None:
    if child.poll() is not None or child.stdin is None:
        raise RuntimeError("native server not running")
    child.stdin.write((text + "\n").encode("utf-8"))
    child.stdin.flush()


def run(server: Path, output: Path) -> dict:
    new_output_directory(output)
    (output / "whitelist.json").write_text(
        json.dumps([{"name": PLAYER}]), encoding="utf-8"
    )
    inventory = output / "players" / "positioner.inventory.json"
    player_data = output / "players" / "positioner.dat"
    report = {
        "issue": 824, "regression": "REG-188",
        "started_utc": datetime.now(timezone.utc).isoformat(),
        "platform": platform.platform(), "server_binary": str(server),
        "mode": "isolated creative Protocol 5 synthetic client",
        "java_1710_gui": "NOT_RUN", "cases": [],
        "verified": False,
    }
    child = None
    logfile = None
    client = None
    y = None  # server feet height, captured on the first admission
    try:
        for i, coordinate in enumerate(NEGATIVE):
            port = reserve_loopback_port()
            child, logfile = start(server, output, port, output / f"write-{i}.log")
            try:
                client = connect(port)
                initial = server_feet(client.position)
                if i > 0:
                    expected_prior = NEGATIVE[i - 1]
                    if not position_matches(initial, expected_prior, y, expected_prior):
                        raise RuntimeError(
                            f"player prior position unexpectedly reset: {initial}"
                        )
                if i == 0:
                    send_console(child, f"/give {PLAYER} {ITEM_ID} {ITEM_COUNT}")
                    wait_inventory(inventory, ITEM_COUNT)
                else:
                    wait_inventory(inventory, ITEM_COUNT)
                y = initial[1]
                client.send(
                    4,
                    struct.pack(
                        ">ddddB", coordinate, y, y + CLIENT_EYE_OFFSET,
                        coordinate, 1,
                    ),
                )
                # Let the queued player packet finish chunk streaming; TCP
                # preserves packet order, and /stop joins the saved session.
                time.sleep(1.0)
                client.close()
                client = None
                shutdown(child)
                child = None
                if not player_data.is_file():
                    raise RuntimeError("player .dat snapshot missing after shutdown")
                if inventory_count(inventory, ITEM_ID) != ITEM_COUNT:
                    raise RuntimeError("inventory lost items during logout")
            finally:
                if client is not None:
                    client.close()
                    client = None
                if child is not None:
                    if child.poll() is None:
                        child.kill()
                    child.wait(timeout=10)
                    child = None
                logfile.close()
                logfile = None

            # Reboot the exact same *test* world and check that the position
            # issued by native S08 equals the saved negative location.
            port = reserve_loopback_port()
            child, logfile = start(server, output, port, output / f"reload-{i}.log")
            try:
                client = connect(port)
                reloaded = server_feet(client.position)
                if not position_matches(reloaded, coordinate, y, coordinate):
                    raise RuntimeError(
                        f"coordinate {coordinate}: restart returned {reloaded} instead"
                    )
                if inventory_count(inventory, ITEM_ID) != ITEM_COUNT:
                    raise RuntimeError("reload changed player ItemStack count")
                report["cases"].append({
                    "coordinate_x_z": coordinate,
                    "saved_y": y,
                    "client_position_before": initial,
                    "client_position_after": reloaded,
                    "persistent_item_id": ITEM_ID,
                    "persistent_item_count": ITEM_COUNT,
                    "restarted": True,
                })
                client.close()
                client = None
                shutdown(child)
                child = None
            finally:
                if client is not None:
                    client.close()
                    client = None
                if child is not None:
                    if child.poll() is None:
                        child.kill()
                    child.wait(timeout=10)
                    child = None
                logfile.close()
                logfile = None
        verify = subprocess.run(
            [str(server), "--verify-world", str(output / "isolated.irw")],
            cwd=output, capture_output=True, text=True, timeout=60,
        )
        if verify.returncode:
            raise RuntimeError(f"read-only world verification failed: {verify.stderr}")
        report["verified"] = len(report["cases"]) == len(NEGATIVE)
    finally:
        report["finished_utc"] = datetime.now(timezone.utc).isoformat()
        (output / "manifest.json").write_text(
            json.dumps(report, indent=2) + "\n", encoding="utf-8",
        )
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path,
                        help="brand new test directory, not any player world")
    args = parser.parse_args()
    try:
        server = args.server.resolve(strict=True)
        if not server.is_file():
            raise ValueError("native Rust executable missing")
        report = run(server, args.output.resolve())
        if not report["verified"]:
            raise RuntimeError("incomplete negative-position acceptance")
        print("PASS: X/Z=-16,-1,-1000 survived player logout, restart and item checks")
        return 0
    except (OSError, RuntimeError, ValueError, TimeoutError,
            subprocess.TimeoutExpired) as error:
        print(f"REG-188 FAIL/INCOMPLETE: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
