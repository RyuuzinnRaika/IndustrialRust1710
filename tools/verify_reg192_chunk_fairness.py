#!/usr/bin/env python3
"""REG-192 / #828: bounded 1-vs-8 Protocol 5 chunk-stream fairness acceptance.

Linux loopback only: distinct 127/8 source IPs keep eight legitimate players
within the server's per-IP connection limits. No production world is touched.
This does not represent vanilla 1.7.10 GUI or network-WAN performance.
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
import threading
import time

from e48_load_benchmark import (
    frame, mc_string, read_frame, reserve_loopback_port, shutdown,
    stable_summary, varint, wait_until_listening,
)

DIRECTIONS = ((1, 0), (-1, 0), (0, 1), (0, -1),
              (1, 1), (1, -1), (-1, 1), (-1, -1))


def chunk_coord(position: float) -> int:
    return math.floor(position / 16)


def parse_chunk(packet: bytes) -> tuple[int, int] | None:
    """Count only loaded S21 columns, not bitmap-zero chunk unloads."""
    if len(packet) < 17 or packet[8] != 1:
        raise ValueError("malformed Protocol 5 S21 chunk header")
    mask = int.from_bytes(packet[9:11], "big")
    compressed = int.from_bytes(packet[13:17], "big", signed=True)
    if compressed < 0 or 17 + compressed != len(packet):
        raise ValueError("invalid S21 compressed length")
    if mask == 0 and compressed == 0:
        return None
    if mask == 0 or compressed == 0:
        raise ValueError("invalid loaded chunk bitmap/length")
    return struct.unpack(">ii", packet[:8])


class Explorer:
    def __init__(self, port: int, name: str, source_ip: str):
        self.name = name
        self.socket = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.socket.settimeout(5)
        try:
            self.socket.bind((source_ip, 0))
            self.socket.connect(("127.0.0.1", port))
        except BaseException:
            self.socket.close()
            raise
        self.lock = threading.Lock()
        self.stop = threading.Event()
        self.ready = threading.Event()
        self.failure: str | None = None
        self.position: tuple[float, float, float] | None = None
        self.chunks: list[tuple[float, tuple[int, int]]] = []
        self.keepalives = 0
        self.packets = 0
        self.thread = threading.Thread(target=self._read, daemon=True)
        handshake = (
            varint(5) + mc_string("localhost") +
            struct.pack(">H", port) + varint(2)
        )
        self.send(0, handshake)
        self.send(0, mc_string(name))
        self.thread.start()

    def send(self, packet_id: int, payload: bytes) -> None:
        with self.lock:
            self.socket.sendall(frame(packet_id, payload))

    def _read(self) -> None:
        logged_in = False
        try:
            while not self.stop.is_set():
                try:
                    packet_id, payload = read_frame(self.socket)
                except socket.timeout:
                    continue
                self.packets += 1
                if not logged_in:
                    if packet_id == 0:
                        raise ValueError("server refused Protocol 5 Login")
                    if packet_id == 2:
                        logged_in = True
                    continue
                if packet_id == 0 and len(payload) == 4:
                    self.send(0, payload)
                    self.keepalives += 1
                elif packet_id == 0x21:
                    chunk = parse_chunk(payload)
                    if chunk is not None:
                        with self.lock:
                            self.chunks.append((time.monotonic(), chunk))
                elif packet_id == 0x08 and len(payload) >= 24:
                    coords = struct.unpack(">ddd", payload[:24])
                    if not all(math.isfinite(value) for value in coords):
                        raise ValueError("server sent nonfinite player position")
                    if self.position is None:
                        self.position = coords
                        self.ready.set()
                elif packet_id == 0x40:
                    raise ValueError("server disconnected active player")
        except (OSError, EOFError, ValueError) as error:
            if not self.stop.is_set():
                self.failure = str(error)
        finally:
            if not self.stop.is_set() and self.failure is None:
                self.failure = "stream closed unexpectedly"

    def chunk_events(self) -> list[tuple[float, tuple[int, int]]]:
        with self.lock:
            return self.chunks.copy()

    def move(self, x: float, y: float, z: float) -> None:
        self.send(4, struct.pack(">ddddB", x, y, y + 1.62, z, 1))

    def close(self) -> None:
        self.stop.set()
        try:
            self.socket.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self.socket.close()
        self.thread.join(timeout=3)


def summarize(records: list[tuple[float, tuple[int, int]]],
              baseline: set[tuple[int, int]], crossing: float) -> dict:
    fresh = [(when, chunk) for when, chunk in records
             if when >= crossing and chunk not in baseline]
    unique = {chunk for _, chunk in fresh}
    return {
        "first_new_chunk_ms": round((min(x[0] for x in fresh) - crossing) * 1000, 2)
        if fresh else None,
        "new_chunks": len(unique),
        "new_packets": len(fresh),
    }


def quantile95(values: list[float]) -> float:
    values = sorted(values)
    if not values:
        raise ValueError("P95 requires observed samples")
    return values[math.ceil(0.95 * len(values)) - 1]


def phase(server: Path, directory: Path, count: int,
          steps: int, step_interval: float, settle: float) -> dict:
    directory.mkdir(exist_ok=False)
    names = [f"Explore{i}" for i in range(count)]
    (directory / "whitelist.json").write_text(
        json.dumps([{"name": name} for name in names]), encoding="utf-8"
    )
    world = directory / "exploration.irw"
    port = reserve_loopback_port()
    command = [
        str(server), "--bind", f"127.0.0.1:{port}",
        "--world", str(world), "--whitelist",
        str(directory / "whitelist.json"),
    ]
    clients: list[Explorer] = []
    child = None
    logfile = directory / "server.log"
    summary: dict = {"players": count, "verified": False}
    with logfile.open("wb") as log:
        try:
            child = subprocess.Popen(
                command, cwd=directory, stdin=subprocess.PIPE,
                stdout=log, stderr=subprocess.STDOUT,
            )
            wait_until_listening(child, port)
            # Two players per distinct loopback IP, below the fixed
            # MAX_ACTIVE_PER_IP=4 gate. This requires Linux 127/8 support.
            for i, name in enumerate(names):
                client = Explorer(port, name, f"127.0.0.{2 + i // 2}")
                clients.append(client)
                if not client.ready.wait(30) or client.failure:
                    raise RuntimeError(f"{name}: Login/initial chunks failed: {client.failure}")
            initial = [client.chunk_events() for client in clients]
            for i, events in enumerate(initial):
                if not events:
                    raise RuntimeError(f"{names[i]} received no initial S21 columns")
            coords = [list(client.position) for client in clients]
            original_centers = [
                (chunk_coord(x), chunk_coord(z)) for x, _, z in coords
            ]
            crossed_at: list[float | None] = [None] * count
            started = time.monotonic()
            for index in range(steps):
                if child.poll() is not None:
                    raise RuntimeError(f"server exited during exploration: {child.returncode}")
                for i, client in enumerate(clients):
                    if client.failure or not client.thread.is_alive():
                        raise RuntimeError(f"{client.name} reader failed: {client.failure}")
                    dx, dz = DIRECTIONS[i]
                    coords[i][0] += dx * 1.0
                    coords[i][2] += dz * 1.0
                    x, y, z = coords[i]
                    center = (chunk_coord(x), chunk_coord(z))
                    if crossed_at[i] is None and center != original_centers[i]:
                        crossed_at[i] = time.monotonic()
                    client.move(x, y, z)
                time.sleep(step_interval)
            time.sleep(settle)
            outcomes = []
            for i, client in enumerate(clients):
                crossed = crossed_at[i]
                if crossed is None:
                    raise RuntimeError(f"{names[i]} never crossed a chunk boundary")
                baseline = {chunk for _, chunk in initial[i]}
                outcome = summarize(client.chunk_events(), baseline, crossed)
                if outcome["new_chunks"] == 0:
                    raise RuntimeError(f"{names[i]} starved: no new chunk streamed")
                outcomes.append({
                    "player": client.name,
                    "direction": DIRECTIONS[i],
                    "initial_chunks": len(baseline),
                    "packets_seen": client.packets,
                    "keepalives": client.keepalives,
                    **outcome,
                })
            summary["elapsed_seconds"] = time.monotonic() - started
            summary["clients"] = outcomes
            summary["first_chunk_p95_ms"] = quantile95(
                [entry["first_new_chunk_ms"] for entry in outcomes]
            )
            summary["first_chunk_max_ms"] = max(
                entry["first_new_chunk_ms"] for entry in outcomes
            )
            summary["min_new_chunks_per_player"] = min(
                entry["new_chunks"] for entry in outcomes
            )
            for client in clients:
                client.close()
            clients.clear()
            shutdown(child)
            child = None
            if world.with_suffix(".pending-txn.json").exists():
                raise RuntimeError("redo journal remains after orderly /stop")
            verification = subprocess.run(
                [str(server), "--verify-world", str(directory)],
                cwd=directory, capture_output=True, text=True, timeout=60,
            )
            if verification.returncode:
                raise RuntimeError(f"read-only world validation failed: {verification.stderr}")
            metrics = world.with_suffix(".metrics.jsonl")
            if not metrics.is_file():
                raise RuntimeError("native tick telemetry missing")
            rows = [json.loads(line) for line in metrics.read_text().splitlines()
                    if line.strip()]
            summary["native_telemetry"] = stable_summary(rows, count)
            summary["verified"] = True
        finally:
            for client in clients:
                client.close()
            if child is not None:
                if child.poll() is None:
                    child.kill()
                child.wait(timeout=10)
            (directory / "summary.json").write_text(
                json.dumps(summary, indent=2) + "\n", encoding="utf-8"
            )
    return summary


def run(args: argparse.Namespace) -> None:
    root = args.output
    root.mkdir(exist_ok=False)
    report = {
        "issue": 828,
        "regression": "REG-192",
        "started_utc": datetime.now(timezone.utc).isoformat(),
        "platform": platform.platform(),
        "server_binary": str(args.server),
        "client": "synthetic Protocol 5 TCP, Java 1.7.10 GUI NOT_RUN",
        "baseline": None, "eight_players": None, "verified": False,
    }
    try:
        report["baseline"] = phase(args.server, root / "single", 1,
                                   args.steps, args.step_interval, args.settle_seconds)
        report["eight_players"] = phase(args.server, root / "eight", 8,
                                        args.steps, args.step_interval, args.settle_seconds)
        baseline = report["baseline"]["first_chunk_p95_ms"]
        concurrent = report["eight_players"]["first_chunk_p95_ms"]
        report["p95_first_chunk_slowdown"] = concurrent / max(1, baseline)
        if args.max_p95_slowdown is not None and (
                report["p95_first_chunk_slowdown"] > args.max_p95_slowdown):
            raise RuntimeError("8-player first-chunk P95 slowdown exceeds configured limit")
        report["verified"] = True
    finally:
        report["finished_utc"] = datetime.now(timezone.utc).isoformat()
        (root / "manifest.json").write_text(
            json.dumps(report, indent=2) + "\n", encoding="utf-8"
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True,
                        help="fresh nonexistent directory, not a player world")
    parser.add_argument("--steps", type=int, default=24)
    parser.add_argument("--step-interval", type=float, default=0.1)
    parser.add_argument("--settle-seconds", type=float, default=18)
    parser.add_argument("--max-p95-slowdown", type=float, default=None)
    args = parser.parse_args()
    if not sys.platform.startswith("linux"):
        parser.error("8 distinct 127/8 client source IPs require Linux loopback")
    if not 18 <= args.steps <= 96:
        parser.error("--steps must be within 18..96 to cross a chunk boundary")
    if not math.isfinite(args.step_interval) or not 0.05 <= args.step_interval <= 1.0:
        parser.error("step interval must be 0.05..1.0s")
    if not math.isfinite(args.settle_seconds) or not 5 <= args.settle_seconds <= 120:
        parser.error("settle time must be 5..120s")
    if args.max_p95_slowdown is not None and (
            not math.isfinite(args.max_p95_slowdown) or args.max_p95_slowdown < 1):
        parser.error("P95 slowdown limit must be finite and >= 1")
    try:
        args.server = args.server.resolve(strict=True)
        if not args.server.is_file():
            raise ValueError("server executable not found")
        args.output = args.output.resolve()
        run(args)
    except (OSError, TimeoutError, ValueError, RuntimeError,
            subprocess.TimeoutExpired) as error:
        print(f"REG-192 FAIL/INCOMPLETE: {error}", file=sys.stderr)
        return 1
    print(f"REG-192 measured data: {args.output / 'manifest.json'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
