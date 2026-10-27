#!/usr/bin/env python3
"""Opt-in TNT stress test using an actual Rust server (not a simulated tick loop).

Creates ONLY fresh isolated IRW test worlds, starts a server bound to loopback,
connects a synthetic Protocol 5 client and measures emitted worker telemetry.
Tests /stop, optional process-kill recovery, packet counts, and world integrity.
No Java GUI client is started; real-world TPS must be measured on target hardware.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import json
import math
from pathlib import Path
import platform
import socket
import struct
import subprocess
import sys
import time

from e48_load_benchmark import (
    SyntheticClient, new_output_directory, reserve_loopback_port,
    shutdown, stable_summary, wait_until_listening,
)

PROFILES = (100, 500, 1000, 4096)
BASE_ENTITY_ID = 10_000_001


def seed_irw(path: Path, count: int, *, chain_queue: bool = False) -> None:
    """An IRW1 flat world with valid legacy TNT1/TNT2 entity trailer."""
    if path.exists():
        raise FileExistsError(path)
    if count not in PROFILES:
        raise ValueError("unsupported TNT count")
    data = bytearray(b"IRW1" + struct.pack(">I", 1 if chain_queue else 0))
    if chain_queue:
        # Extra chain TNT is intentionally a block, not an entity.
        data += struct.pack(">iBiH", -16, 200, 0, 46 << 4)
    data += b"TNT2" if chain_queue else b"TNT1"
    data += struct.pack(">H", count)
    for index in range(count):
        x = (index % 64) * 24
        z = (index // 64) * 24
        data += struct.pack(">iiiiI", BASE_ENTITY_ID + index, x, 200, z, 160)
    if chain_queue:
        data += struct.pack(">I", 1)
        data += struct.pack(">iBi", -16, 200, 0)
    path.write_bytes(data)


def tnt_snapshot_count(path: Path) -> tuple[str, int]:
    """Validate enough IRW structure to find primed TNT without guessing."""
    raw = path.read_bytes()
    if len(raw) < 8:
        raise ValueError("truncated IRW")
    if raw[:4] == b"IRW1":
        offset = 8
    elif raw[:4] in (b"IRW2", b"IRW3") and len(raw) >= 16:
        offset = 16
    elif raw[:4] == b"IRW4" and len(raw) >= 17:
        offset = 17
    else:
        raise ValueError("unknown IRW world format")
    count = struct.unpack(">I", raw[offset-4:offset])[0]
    if count > 65536:
        raise ValueError("IRW edit count exceeded")
    end = offset + 11 * count
    if end > len(raw):
        raise ValueError("truncated IRW block list")
    trailer = raw[end:]
    if not trailer:
        return "none", 0
    if len(trailer) < 6 or trailer[:4] not in (b"TNT1", b"TNT2", b"TNT3", b"TNT4"):
        raise ValueError("unknown entity trailer")
    primed = struct.unpack(">H", trailer[4:6])[0]
    entity_stride = 44 if trailer[:4] == b"TNT4" else 20
    if primed > 4096 or len(trailer) < 6 + entity_stride * primed:
        raise ValueError("truncated TNT entity trailer")
    return trailer[:4].decode("ascii"), primed


def read_rss(pid: int) -> int | None:
    """Linux procfs RSS, if available; do not invent readings on Windows."""
    try:
        for line in Path(f"/proc/{pid}/status").read_text().splitlines():
            if line.startswith("VmRSS:"):
                return int(line.split()[1]) * 1024
    except (OSError, ValueError, IndexError):
        pass
    return None


def run_profile(server: Path, output: Path, count: int, duration: float,
                inject_crash: bool, chain_queue: bool) -> dict:
    folder = output / f"tnt-{count}"
    folder.mkdir(exist_ok=False)
    world = folder / "world.irw"
    seed_irw(world, count, chain_queue=chain_queue)
    whitelist = folder / "whitelist.json"
    whitelist.write_text('[{"name":"TntLoad"}]', encoding="utf-8")
    port = reserve_loopback_port()
    command = [str(server), "--bind", f"127.0.0.1:{port}",
               "--whitelist", str(whitelist), "--world", str(world)]
    before = tnt_snapshot_count(world)
    clients = []
    crashed_remaining = None
    peak_rss = None
    crashed = False
    observed_explosions = 0
    observed_teleports = 0
    captured_packets = 0
    with (folder / "server.log").open("wb") as log:
        child = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=log,
                                 stderr=subprocess.STDOUT, cwd=folder)
        try:
            wait_until_listening(child, port)
            client = SyntheticClient(port, "TntLoad")
            clients.append(client)
            if not client.ready.wait(20):
                raise RuntimeError(client.failure or "Protocol 5 login timed out")
            started = time.monotonic()
            deadline = started + duration
            while time.monotonic() < deadline:
                if child.poll() is not None:
                    raise RuntimeError(f"server exited early: {child.returncode}")
                for item in clients:
                    if item.failure:
                        raise RuntimeError(item.failure)
                rss = read_rss(child.pid)
                if rss is not None:
                    peak_rss = max(peak_rss or 0, rss)
                # Only kill AFTER an actual explosion packet is seen, not
                # during startup or idle warmup (important for 100 TNT).
                if inject_crash and not crashed and any(
                    item.explosion_packets for item in clients
                ):
                    for item in clients:
                        captured_packets += item.packets
                        observed_explosions += item.explosion_packets
                        observed_teleports += item.entity_teleports
                        item.close()
                    clients.clear()
                    child.kill()  # Deliberately simulate process termination.
                    child.wait(timeout=10)
                    crashed_remaining = tnt_snapshot_count(world)[1]
                    child = subprocess.Popen(command, stdin=subprocess.PIPE,
                                             stdout=log, stderr=subprocess.STDOUT, cwd=folder)
                    wait_until_listening(child, port)
                    client = SyntheticClient(port, "TntLoad")
                    clients.append(client)
                    if not client.ready.wait(20):
                        raise RuntimeError(client.failure or "reconnect after crash timed out")
                    crashed = True
                time.sleep(min(0.2, max(0, deadline - time.monotonic())))
            if inject_crash and not crashed:
                raise RuntimeError("no explosion packet observed; crash test was not exercised")
            packet_count = captured_packets + sum(item.packets for item in clients)
            explosions = observed_explosions + sum(item.explosion_packets for item in clients)
            teleports = observed_teleports + sum(item.entity_teleports for item in clients)
            keepalives = sum(item.keepalives for item in clients)
            for item in clients:
                item.close()
            clients.clear()
            shutdown(child, timeout=120)
            after = tnt_snapshot_count(world)
            if after[1] > count or (crashed_remaining is not None and after[1] > crashed_remaining):
                raise RuntimeError("primed TNT count increased despite no TNT block ignition")
            verify = subprocess.run([str(server), "--verify-world", str(world)],
                                    cwd=folder, capture_output=True, text=True, timeout=90)
            if verify.returncode:
                raise RuntimeError(f"read-only --verify-world failed: {verify.stderr}")
        finally:
            for item in clients:
                item.close()
            if child.poll() is None:
                child.kill()
            child.wait(timeout=10)
    telemetry = world.with_suffix(".metrics.jsonl")
    rows = [json.loads(line) for line in telemetry.read_text(encoding="utf-8").splitlines()
            if line.strip()] if telemetry.exists() else []
    # Stable_summary is only meaningful for complete synthetic-client windows.
    try:
        measurements = stable_summary(rows, 1)
    except ValueError:
        measurements = {"telemetry_note": "insufficient complete one-peer windows"}
    measurements.update({
        "scenario_tnt": count, "before_format": before[0],
        "after_format": after[0], "remaining_tnt": after[1],
        "crash_remaining_tnt": crashed_remaining,
        "process_kill_restart_verified": crashed,
        "client_packets": packet_count, "explosion_packets": explosions,
        "entity_teleports": teleports, "keepalives": keepalives,
        "peak_rss_bytes_linux": peak_rss, "elapsed_seconds": duration,
        "world_file_bytes": world.stat().st_size,
    })
    (folder / "summary.json").write_text(json.dumps(measurements, indent=2) + "\n",
                                         encoding="utf-8")
    return measurements


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path,
                        help="brand-new output directory; existing paths are refused")
    parser.add_argument("--counts", default="100,500,1000,4096")
    parser.add_argument("--seconds", type=float, default=75.0)
    parser.add_argument("--inject-crash", action="store_true")
    parser.add_argument("--tnt2-chain-queue", action="store_true")
    args = parser.parse_args()
    try:
        counts = [int(v) for v in args.counts.split(",")]
    except ValueError:
        parser.error("--counts must contain comma-separated integers")
    if not counts or len(counts) != len(set(counts)) or any(v not in PROFILES for v in counts):
        parser.error(f"--counts must be unique entries from {PROFILES}")
    if not math.isfinite(args.seconds) or not 15 <= args.seconds <= 3600:
        parser.error("--seconds must be between 15 and 3600")
    server = args.server.resolve(strict=True)
    if not server.is_file():
        parser.error("--server must be a compiled Rust server")
    output = args.output.resolve()
    new_output_directory(output)
    report = {
        "timestamp_utc": datetime.now(timezone.utc).isoformat(),
        "platform": platform.platform(), "binary": str(server),
        "client_kind": "synthetic Protocol 5, not unmodified Minecraft Java GUI",
        "profiles": [],
    }
    try:
        for count in counts:
            print(f"[TNT] {count} primed entities / {args.seconds}s", flush=True)
            report["profiles"].append(run_profile(
                server, output, count, args.seconds, args.inject_crash,
                args.tnt2_chain_queue,
            ))
            (output / "manifest.json").write_text(json.dumps(report, indent=2) + "\n",
                                                  encoding="utf-8")
    except (OSError, RuntimeError, ValueError, TimeoutError) as error:
        report["error"] = str(error)
        (output / "manifest.json").write_text(json.dumps(report, indent=2) + "\n",
                                              encoding="utf-8")
        print(f"[TNT] FAILED: {error}", file=sys.stderr)
        return 1
    print(f"[TNT] report: {output / 'manifest.json'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
