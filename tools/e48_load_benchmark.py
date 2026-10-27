#!/usr/bin/env python3
"""Opt-in E48/E50 measured 1/2/4/8-player synthetic Protocol 5 load test.

Starts isolated Rust server subprocesses on LOOPBACK and fresh worlds only.
Measures actual Rust world-worker telemetry; this is NOT a vanilla Java GUI test.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import json
import math
import os
from pathlib import Path
import platform
import socket
import struct
import subprocess
import sys
import threading
import time

COUNTS = (1, 2, 4, 8)
MAX_FRAME = 2 * 1024 * 1024
PROTOCOL = 5


def varint(number: int) -> bytes:
    if not 0 <= number <= 0xFFFFFFFF:
        raise ValueError("VarInt outside 32-bit range")
    result = bytearray()
    while True:
        value = number & 0x7F
        number >>= 7
        result.append(value | (0x80 if number else 0))
        if not number:
            return bytes(result)


def frame(packet_id: int, payload: bytes) -> bytes:
    message = varint(packet_id) + payload
    if not message or len(message) > MAX_FRAME:
        raise ValueError("invalid Protocol 5 frame length")
    return varint(len(message)) + message


def mc_string(value: str) -> bytes:
    data = value.encode("utf-8")
    return varint(len(data)) + data


def read_exact(sock: socket.socket, count: int) -> bytes:
    pieces = bytearray()
    while len(pieces) < count:
        block = sock.recv(count - len(pieces))
        if not block:
            raise EOFError("Protocol 5 stream closed")
        pieces.extend(block)
    return bytes(pieces)


def read_varint(sock: socket.socket) -> int:
    result = 0
    for shift in range(0, 35, 7):
        byte = read_exact(sock, 1)[0]
        if shift == 28 and byte > 0x0F:
            raise ValueError("Protocol 5 VarInt exceeds 32 bits")
        result |= (byte & 0x7F) << shift
        if not byte & 0x80:
            return result
    raise ValueError("Protocol 5 VarInt exceeds five bytes")


def read_frame(sock: socket.socket) -> tuple[int, bytes]:
    size = read_varint(sock)
    if size == 0 or size > MAX_FRAME:
        raise ValueError("oversize or empty Protocol 5 packet")
    data = memoryview(read_exact(sock, size))
    # Parsing a packet ID from the already bounded frame avoids an unbounded
    # separate socket read, and rejects malformed/overlong VarInts.
    packet_id = 0
    for index, byte in enumerate(data[:5]):
        if index == 4 and byte > 0x0F:
            raise ValueError("invalid Protocol 5 packet ID")
        packet_id |= (byte & 0x7F) << (7 * index)
        if not byte & 0x80:
            return packet_id, bytes(data[index + 1:])
    raise ValueError("unterminated Protocol 5 packet ID")


class SyntheticClient:
    def __init__(self, port: int, name: str):
        self.name = name
        self.sock = socket.create_connection(("127.0.0.1", port), timeout=5)
        self.sock.settimeout(5)
        self.write_guard = threading.Lock()
        self.stop = threading.Event()
        self.ready = threading.Event()
        self.failure: str | None = None
        self.keepalives = 0
        self.packets = 0
        self.explosion_packets = 0
        self.entity_teleports = 0
        self.position: tuple[float, float, float] | None = None
        self.thread = threading.Thread(target=self._read, daemon=True, name=f"p5-{name}")
        handshake = (
            varint(PROTOCOL) + mc_string("localhost") +
            struct.pack(">H", port) + varint(2)
        )
        self.send(0, handshake)
        self.send(0, mc_string(name))
        self.thread.start()

    def send(self, packet_id: int, payload: bytes) -> None:
        with self.write_guard:
            self.sock.sendall(frame(packet_id, payload))

    def _read(self) -> None:
        logged_in = False
        try:
            while not self.stop.is_set():
                try:
                    packet_id, payload = read_frame(self.sock)
                except socket.timeout:
                    continue
                self.packets += 1
                if logged_in and packet_id == 0x27:
                    self.explosion_packets += 1
                if logged_in and packet_id == 0x18:
                    self.entity_teleports += 1
                if not logged_in:
                    if packet_id == 0:
                        raise ValueError("server rejected login")
                    if packet_id == 2:
                        logged_in = True
                    continue
                if packet_id == 0 and len(payload) == 4:
                    self.send(0, payload)  # KeepAlive must be echoed.
                    self.keepalives += 1
                elif packet_id == 8 and len(payload) >= 24:
                    self.position = struct.unpack(">ddd", payload[:24])
                    if all(math.isfinite(x) for x in self.position):
                        self.ready.set()
                elif packet_id == 0x40:
                    raise ValueError("server disconnected active player")
        except (OSError, ValueError, EOFError) as exc:
            if not self.stop.is_set():
                self.failure = f"{self.name}: {exc}"
        finally:
            if not self.stop.is_set() and self.failure is None:
                self.failure = f"{self.name}: stream ended without stop"

    def move(self, iteration: int) -> None:
        if self.position is None:
            raise RuntimeError(f"{self.name} did not receive the initial position")
        x, y, z = self.position
        # Remain within a tiny bounded square; we are testing protocol/TPS,
        # not world exploration or game-play AI. Vanilla 1.7.10 C04 sends feet Y before stance.
        dx = 0.02 if iteration % 2 else 0.0
        payload = struct.pack(">ddddB", x + dx, y, y + 1.62, z, 1)
        self.send(4, payload)

    def close(self) -> None:
        self.stop.set()
        try:
            self.sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self.sock.close()
        self.thread.join(timeout=2)


def stable_summary(rows: list[dict], count: int) -> dict:
    """Reject mixed-load windows; never substitute synthetic metrics for real data."""
    selected = []
    for row in rows:
        if row.get("schema_version") != 2:
            raise ValueError("telemetry is not schema v2")
        if (row.get("players_min"), row.get("players_peak")) != (count, count):
            continue
        duration = row.get("window_seconds")
        ticks = row.get("processed_ticks")
        missed = row.get("missed_ticks")
        samples = row.get("mspt_samples")
        p99 = row.get("mspt_p99")
        if (not isinstance(duration, (int, float)) or not math.isfinite(duration)
                or duration <= 0 or not isinstance(ticks, int) or ticks < 0
                or not isinstance(missed, int) or missed < 0
                or not isinstance(samples, int) or samples <= 0
                or not isinstance(p99, (int, float)) or not math.isfinite(p99)
                or p99 < 0):
            raise ValueError("invalid stable-load telemetry window")
        selected.append(row)
    if not selected:
        raise ValueError(f"no complete v2 telemetry window with exactly {count} peers")
    seconds = sum(r["window_seconds"] for r in selected)
    ticks = sum(r["processed_ticks"] for r in selected)
    missed = sum(r["missed_ticks"] for r in selected)
    if seconds <= 0:
        raise ValueError("zero stable-load duration")
    rss = [r["process_rss_bytes"] for r in selected
           if isinstance(r.get("process_rss_bytes"), int)
           and r["process_rss_bytes"] >= 0]
    # Long-run F60 trend indicators are computed only from stable peer windows.
    # A positive RSS delta suggests retained memory, but does not by itself
    # prove a leak. Short runs (< 6 windows) are inconclusive.
    enough_trend = len(selected) >= 6
    first = selected[:len(selected) // 2]
    second = selected[len(selected) // 2:]
    def half_tps(windows: list[dict]) -> float:
        return sum(r["processed_ticks"] for r in windows) / sum(
            r["window_seconds"] for r in windows)
    return {
        "players": count,
        "stable_windows": len(selected),
        "stable_seconds": seconds,
        "tps_processed": ticks / seconds,
        "tick_delivery_ratio": ticks / (ticks + missed) if ticks + missed else 0,
        "mspt_p99_worst_window": max(r["mspt_p99"] for r in selected),
        "process_rss_peak_bytes": max(rss) if rss else None,
        "process_rss_growth_bytes": (rss[-1] - rss[0])
        if enough_trend and len(rss) == len(selected) else None,
        "tps_first_half": half_tps(first) if enough_trend else None,
        "tps_second_half": half_tps(second) if enough_trend else None,
        "mspt_p99_first_half": max(r["mspt_p99"] for r in first) if enough_trend else None,
        "mspt_p99_second_half": max(r["mspt_p99"] for r in second) if enough_trend else None,
        "processed_ticks": ticks,
        "missed_ticks": missed,
    }


def new_output_directory(path: Path) -> None:
    # The entire benchmark is create-only: never delete/reuse an existing save.
    path.mkdir(parents=True, exist_ok=False)


def reserve_loopback_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def wait_until_listening(child: subprocess.Popen, port: int) -> None:
    deadline = time.monotonic() + 25
    while time.monotonic() < deadline:
        if child.poll() is not None:
            raise RuntimeError(f"server exited during boot: {child.returncode}")
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.3):
                return
        except OSError:
            time.sleep(0.05)
    raise TimeoutError("server did not bind its loopback port")


def shutdown(child: subprocess.Popen, timeout: float = 45) -> None:
    if child.poll() is not None:
        raise RuntimeError(f"server exited unexpectedly: {child.returncode}")
    if child.stdin is None:
        raise RuntimeError("server console missing")
    child.stdin.write(b"/stop\n")
    child.stdin.flush()
    try:
        result = child.wait(timeout=timeout)
    except subprocess.TimeoutExpired as exc:
        raise TimeoutError("graceful /stop timed out; cannot certify save") from exc
    if result:
        raise RuntimeError(f"server /stop failed: {result}")


def run_phase(server: Path, output: Path, peers: int,
              seconds: float, warmup: float, inject_crash: bool) -> dict:
    directory = output / f"load-{peers}"
    directory.mkdir(exist_ok=False)
    world = directory / "world.irw"
    whitelist = directory / "whitelist.json"
    whitelist.write_text(
        json.dumps([{"name": f"Load{i}"} for i in range(peers)]),
        encoding="utf-8",
    )
    port = reserve_loopback_port()
    command = [
        str(server), "--bind", f"127.0.0.1:{port}",
        "--whitelist", str(whitelist), "--world", str(world),
    ]
    clients: list[SyntheticClient] = []
    with (directory / "server.log").open("wb") as log:
        child = subprocess.Popen(
            command, stdin=subprocess.PIPE, stdout=log, stderr=subprocess.STDOUT,
            cwd=directory,
        )
        try:
            wait_until_listening(child, port)
            for i in range(peers):
                client = SyntheticClient(port, f"Load{i}")
                clients.append(client)
                if not client.ready.wait(20):
                    raise RuntimeError(client.failure or f"{client.name}: login timed out")
            started = time.monotonic()
            deadline = started + warmup + seconds
            iteration = 0
            while time.monotonic() < deadline:
                if child.poll() is not None:
                    raise RuntimeError(f"server crashed during load: {child.returncode}")
                for client in clients:
                    if client.failure or not client.thread.is_alive():
                        raise RuntimeError(client.failure or f"{client.name}: reader stopped")
                    client.move(iteration)
                iteration += 1
                time.sleep(min(0.2, max(0.0, deadline - time.monotonic())))
            # Stop clients only after measured load. No hidden reconnects.
            total_packets = sum(c.packets for c in clients)
            total_keepalives = sum(c.keepalives for c in clients)
            for client in clients:
                client.close()
            clients.clear()
            if inject_crash:
                # Process kill, not a power-loss simulation. Restart the same
                # world and verify the recovery path before passing this phase.
                child.kill()
                child.wait(timeout=10)
                child = subprocess.Popen(
                    command, stdin=subprocess.PIPE, stdout=log, stderr=subprocess.STDOUT,
                    cwd=directory,
                )
                wait_until_listening(child, port)
            shutdown(child)
            verify = subprocess.run(
                [str(server), "--verify-world", str(world.parent)],
                cwd=directory, capture_output=True, text=True, timeout=30,
            )
            if verify.returncode:
                raise RuntimeError(f"read-only world verification failed: {verify.stderr}")
        finally:
            for client in clients:
                client.close()
            if child.poll() is None:
                child.kill()
            child.wait(timeout=10)

    metrics_file = world.with_suffix(".metrics.jsonl")
    if not metrics_file.is_file():
        raise RuntimeError("server did not emit world-worker telemetry")
    rows = [json.loads(line) for line in metrics_file.read_text(encoding="utf-8").splitlines()
            if line.strip()]
    summary = stable_summary(rows, peers)
    summary.update({
        "synthetic_client": True,
        "load_seconds_requested": seconds,
        "warmup_seconds": warmup,
        "protocol_packets_received": total_packets,
        "keepalives_responded": total_keepalives,
        "process_kill_restart_verified": inject_crash,
        "telemetry_file": str(metrics_file.relative_to(output)),
    })
    (directory / "summary.json").write_text(
        json.dumps(summary, indent=2) + "\n", encoding="utf-8",
    )
    return summary


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", required=True, type=Path,
                        help="built vanilla-server executable")
    parser.add_argument("--output", required=True, type=Path,
                        help="NEW output directory, must not already exist")
    parser.add_argument("--seconds", type=float, default=60,
                        help="seconds of test load per profile (20..86400)")
    parser.add_argument("--warmup-seconds", type=float, default=12,
                        help="initial full-peer warmup per profile (10..120)")
    parser.add_argument("--inject-crash", action="store_true",
                        help="kill/restart only the 8-peer phase before read-only verification")
    parser.add_argument("--max-rss-growth-mib", type=float, default=None,
                        help="F60: fail if observed stable-window RSS growth exceeds this MiB; requires >= 6 windows")
    parser.add_argument("--min-tps", type=float, default=None,
                        help="F60: fail if stable processed TPS falls below this value")
    args = parser.parse_args()
    if (not math.isfinite(args.seconds) or not 20 <= args.seconds <= 86400
            or not math.isfinite(args.warmup_seconds)
            or not 10 <= args.warmup_seconds <= 120):
        parser.error("load duration must be 20..86400s; warmup must be 10..120s")
    if args.max_rss_growth_mib is not None and (
            not math.isfinite(args.max_rss_growth_mib) or args.max_rss_growth_mib < 0):
        parser.error("--max-rss-growth-mib must be a finite non-negative number")
    if args.min_tps is not None and (
            not math.isfinite(args.min_tps) or not 0 < args.min_tps <= 20):
        parser.error("--min-tps must be in (0, 20]")
    server = args.server.resolve(strict=True)
    if not server.is_file():
        parser.error("--server is not an executable file")
    output = args.output.resolve()
    new_output_directory(output)
    report = {
        "timestamp_utc": datetime.now(timezone.utc).isoformat(),
        "platform": platform.platform(),
        "python": platform.python_version(),
        "server_binary": str(server),
        "load_type": "synthetic Protocol 5 TCP clients; NOT unmodified Java 1.7.10 GUI",
        "telemetry_source": "real vanilla-server world-worker schema v2",
        "profiles": [],
    }
    try:
        for count in COUNTS:
            print(f"[E48] {count} peers: warmup={args.warmup_seconds}s load={args.seconds}s",
                  flush=True)
            summary = run_phase(server, output, count, args.seconds,
                                args.warmup_seconds, args.inject_crash and count == 8)
            report["profiles"].append(summary)
            if args.min_tps is not None and summary["tps_processed"] < args.min_tps:
                raise RuntimeError(f"{count} peers: stable TPS below threshold")
            if args.max_rss_growth_mib is not None:
                growth = summary["process_rss_growth_bytes"]
                if growth is None:
                    raise RuntimeError(f"{count} peers: insufficient stable RSS trend evidence")
                if growth > args.max_rss_growth_mib * 1024 * 1024:
                    raise RuntimeError(f"{count} peers: observed RSS growth above threshold")
            (output / "manifest.json").write_text(
                json.dumps(report, indent=2) + "\n", encoding="utf-8",
            )
            print(f"  measured stable TPS={summary['tps_processed']:.3f}, "
                  f"P99 MSPT worst={summary['mspt_p99_worst_window']:.3f}ms", flush=True)
    except (OSError, ValueError, RuntimeError, TimeoutError) as exc:
        report["error"] = str(exc)
        (output / "manifest.json").write_text(
            json.dumps(report, indent=2) + "\n", encoding="utf-8",
        )
        print(f"[E48] FAILED CLOSED: {exc}", file=sys.stderr)
        return 1
    print(f"Measured results: {output / 'manifest.json'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
