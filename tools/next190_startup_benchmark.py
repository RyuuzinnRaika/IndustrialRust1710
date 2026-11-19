#!/usr/bin/env python3
"""Opt-in synthetic Protocol 5 startup benchmark; never used by runtime.

Cold means a new flat world; warm means a fresh process loading a copy of
that run's cleanly stopped world. OS/disk caches are NOT controlled. The
bound-port log is not an accept-loop or pure world-load measurement.
No existing world is read or removed. The output directory must be new.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import queue
import re
import shutil
import socket
import statistics
import struct
import subprocess
import sys
import threading
import time
import zlib

from e48_load_benchmark import frame, mc_string, read_frame, varint

LISTENING = re.compile(rb"^IndustrialRust1710: listening on 127\.0\.0\.1:(\d+) ")
TIMEOUT = 45.0
MAX_PACKETS = 4096


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def manifest_world(world: Path) -> dict[str, str]:
    """Hash only plain files in the harness-created tree; reject links."""
    result = {}
    def fail_walk(error: OSError) -> None:
        raise error

    for root, directories, files in os.walk(world, followlinks=False, onerror=fail_walk):
        for name in directories + files:
            item = Path(root) / name
            info = item.lstat()
            if item.is_symlink() or getattr(info, "st_file_attributes", 0) & 0x400:
                raise ValueError("fixture contains a symlink or reparse point")
            if item.is_file():
                result[item.relative_to(world).as_posix()] = sha256(item)
            elif not item.is_dir():
                raise ValueError("fixture contains a non-regular file")
    return result


def copy_fixture(source: Path, target: Path) -> dict[str, str]:
    before = manifest_world(source)
    shutil.copytree(source, target, symlinks=True)
    after = manifest_world(target)
    if before != after:
        raise ValueError("fixture copy content mismatch")
    return before


def parse_vi(data: bytes, offset: int = 0) -> tuple[int, int]:
    value = 0
    for index in range(5):
        if offset >= len(data):
            raise ValueError("truncated VarInt")
        byte = data[offset]
        offset += 1
        if index == 4 and byte > 15:
            raise ValueError("oversize VarInt")
        value |= (byte & 127) << (7 * index)
        if not byte & 128:
            return value, offset
    raise ValueError("overlong VarInt")


def parse_string(data: bytes, offset: int) -> tuple[str, int]:
    size, offset = parse_vi(data, offset)
    if size > len(data) - offset:
        raise ValueError("truncated string")
    return data[offset:offset + size].decode("utf-8"), offset + size


def check_chunk(body: bytes) -> tuple[int, int]:
    if len(body) < 17 or body[8] != 1:
        raise ValueError("invalid full chunk header")
    x, z = struct.unpack(">ii", body[:8])
    sections, extra, size = struct.unpack(">HHi", body[9:17])
    if sections & 0x18 != 0x18 or extra != 0 or size < 0 or len(body) != 17 + size:
        raise ValueError("invalid chunk sections or compressed size")
    expected = sections.bit_count() * 10240 + 256
    decoder = zlib.decompressobj()
    raw = decoder.decompress(body[17:], expected + 1)
    if len(raw) != expected or not decoder.eof or decoder.unused_data:
        raise ValueError("invalid chunk payload")
    return x, z


def check_inventory(body: bytes) -> None:
    if len(body) < 3 or body[:3] != b"\x00\x00\x2d":
        raise ValueError("missing initial player inventory")
    cursor = 3
    for _ in range(45):
        if len(body) < cursor + 2:
            raise ValueError("truncated inventory")
        item = int.from_bytes(body[cursor:cursor + 2], "big", signed=True)
        cursor += 2
        if item != -1:
            if len(body) < cursor + 5:
                raise ValueError("truncated item")
            nbt = int.from_bytes(body[cursor + 3:cursor + 5], "big", signed=True)
            cursor += 5
            if nbt < -1 or len(body) < cursor + max(nbt, 0):
                raise ValueError("invalid item NBT")
            cursor += max(nbt, 0)
    if cursor != len(body):
        raise ValueError("trailing inventory bytes")


class DeadlineReader:
    """Reapply the remaining deadline for every recv used by e48.read_frame."""

    def __init__(self, client: socket.socket, deadline: float):
        self.client = client
        self.deadline = deadline

    def recv(self, size: int) -> bytes:
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("Protocol 5 receive deadline exceeded")
        self.client.settimeout(remaining)
        return self.client.recv(size)


def join_measurement(port: int, name: str, result: dict) -> None:
    deadline = time.monotonic() + TIMEOUT
    with socket.create_connection(("127.0.0.1", port), timeout=TIMEOUT) as client:
        result["tcp_connected_ns"] = time.monotonic_ns()
        handshake = varint(5) + mc_string("localhost") + struct.pack(">H", port) + varint(2)
        client.sendall(frame(0, handshake) + frame(0, mc_string(name)))
        result["login_sent_ns"] = time.monotonic_ns()
        login = False
        join = False
        inventory = False
        hotbar = False
        chunks = set()
        for _ in range(MAX_PACKETS):
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("join deadline exceeded")
            packet, body = read_frame(DeadlineReader(client, deadline))
            now = time.monotonic_ns()
            if not login:
                if packet != 2:
                    raise ValueError("login was not successful")
                uuid, at = parse_string(body, 0)
                user, at = parse_string(body, at)
                if len(uuid) != 36 or user != name or at != len(body):
                    raise ValueError("invalid Login Success")
                login = True
                result["login_success_ns"] = now
            elif packet == 0 and len(body) == 4:
                client.sendall(frame(0, body))
            elif packet == 1:
                if len(body) < 10:
                    raise ValueError("invalid Join Game")
                if body[4] != 1 or body[5] != 0:
                    raise ValueError("unexpected gamemode or dimension")
                result["observed_join_gamemode"] = "creative"
                result["observed_join_dimension"] = 0
                result["observed_join_difficulty"] = body[6]
                join = True
                result["join_game_ns"] = now
            elif packet == 0x21:
                coord = check_chunk(body)
                if coord in chunks:
                    raise ValueError("duplicate initial chunk")
                chunks.add(coord)
            elif packet == 0x30:
                check_inventory(body)
                inventory = True
            elif packet == 9:
                if len(body) != 1 or body[0] > 8:
                    raise ValueError("invalid hotbar")
                hotbar = True
            elif packet == 8:
                if len(body) != 33:
                    raise ValueError("invalid initial position")
                x, y, z, yaw, pitch, _ = struct.unpack(">dddffB", body)
                if not all(math.isfinite(v) for v in (x, y, z, yaw, pitch)):
                    raise ValueError("non-finite position")
                cx, cz = math.floor(x / 16), math.floor(z / 16)
                expected = {(cx + dx, cz + dz) for dx in range(-2, 3) for dz in range(-2, 3)}
                if not (join and inventory and hotbar and chunks == expected):
                    raise ValueError("incomplete initial join burst")
                result["initial_join_ns"] = now
                client.sendall(frame(1, mc_string("/list")))
                break
            elif packet == 0x40:
                raise ValueError("server disconnected during join")
        else:
            raise TimeoutError("join packet limit reached")

        for _ in range(MAX_PACKETS):
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("Play response deadline exceeded")
            packet, body = read_frame(DeadlineReader(client, deadline))
            if packet == 0 and len(body) == 4:
                client.sendall(frame(0, body))
            elif packet == 2 and b"Online (1)" in body:
                result["play_list_ns"] = time.monotonic_ns()
                return
            elif packet == 0x40:
                raise ValueError("server disconnected before Play response")
        raise TimeoutError("Play packet limit reached")


def watch_stdout(stream, log, events: queue.Queue) -> None:
    try:
        for line in iter(stream.readline, b""):
            now = time.monotonic_ns()
            log.write(line)
            log.flush()
            match = LISTENING.match(line)
            if match:
                events.put(("listening", now, int(match.group(1))))
    except Exception as exc:
        events.put(("reader_error", f"{type(exc).__name__}: {exc}"))
    finally:
        events.put(("eof",))


def measure(server: Path, directory: Path, mode: str, name: str, *, existing: bool = False,
            copied_fixture_sha256: dict | None = None, pair: int = 0) -> dict:
    result = {"pair": pair, "mode": mode, "status": "failed", "errors": [],
              "forced_termination_attempted": False}
    if copied_fixture_sha256 is not None:
        result["copied_fixture_sha256"] = copied_fixture_sha256
    child = None
    reader = None
    stdout = None
    stderr = None
    owned_directory = existing
    completed = False
    events: queue.Queue = queue.Queue()
    world = directory / "world"

    def record_error(stage: str, exc: Exception) -> None:
        result["errors"].append(f"{stage}: {type(exc).__name__}: {exc}")

    try:
        if not existing:
            directory.mkdir(exist_ok=False)
            owned_directory = True
            world.mkdir(exist_ok=False)
            (world / "whitelist.json").write_text(
                json.dumps([{"name": "ColdProbe"}, {"name": "WarmProbe"}]), encoding="utf-8")
        elif not world.is_dir():
            raise ValueError("warm fixture missing")
        if (world / "server.properties").exists():
            raise ValueError("unexpected server.properties in isolated fixture")
        command = [str(server), "--bind", "127.0.0.1:0", "--world", str(world / "flat.irw"),
                   "--whitelist", str(world / "whitelist.json"), "--creative"]
        stdout = (directory / "stdout.log").open("xb")
        stderr = (directory / "stderr.log").open("xb")
        result["spawn_before_ns"] = time.monotonic_ns()
        child = subprocess.Popen(command, cwd=world, stdin=subprocess.PIPE,
                                 stdout=subprocess.PIPE, stderr=stderr)
        result["spawn_return_ns"] = time.monotonic_ns()
        reader = threading.Thread(target=watch_stdout, args=(child.stdout, stdout, events), daemon=True)
        reader.start()
        try:
            event = events.get(timeout=TIMEOUT)
        except queue.Empty as exc:
            raise TimeoutError("no listening log") from exc
        if event[0] == "reader_error":
            raise RuntimeError(f"stdout reader failed: {event[1]}")
        if event[0] != "listening":
            raise RuntimeError("server exited before listening log")
        _, result["listening_log_ns"], port = event
        if not 1 <= port <= 65535:
            raise ValueError("invalid bound port")
        join_measurement(port, name, result)
        if child.poll() is not None:
            raise RuntimeError("server exited before graceful stop")
        child.stdin.write(b"/stop\n")
        child.stdin.flush()
        result["stop_requested_ns"] = time.monotonic_ns()
        result["exit_code"] = child.wait(timeout=TIMEOUT)
        result["exit_ns"] = time.monotonic_ns()
        if result["exit_code"] != 0:
            raise RuntimeError("server failed graceful stop")
        completed = True
    except Exception as exc:
        record_error("run", exc)
    finally:
        if child is not None:
            try:
                if child.poll() is None:
                    result["forced_termination_attempted"] = True
                    try:
                        child.kill()
                    finally:
                        child.wait(timeout=10)
            except Exception as exc:
                record_error("child cleanup", exc)
        if reader is not None:
            try:
                reader.join(timeout=10)
                if reader.is_alive():
                    raise TimeoutError("stdout reader still running")
            except Exception as exc:
                record_error("reader join", exc)
        while True:
            try:
                event = events.get_nowait()
            except queue.Empty:
                break
            if event[0] == "reader_error":
                result["errors"].append(f"stdout reader: {event[1]}")
        for label, stream in (("stdin", getattr(child, "stdin", None)),
                              ("child stdout", getattr(child, "stdout", None)
                               if reader is None or not reader.is_alive() else None),
                              ("stdout log", stdout if reader is None or not reader.is_alive() else None),
                              ("stderr log", stderr)):
            if stream is not None:
                try:
                    stream.close()
                except Exception as exc:
                    record_error(f"close {label}", exc)
        if "spawn_before_ns" in result:
            start = result["spawn_before_ns"]
            result["elapsed_ms"] = {
                key.removesuffix("_ns"): (value - start) / 1_000_000
                for key, value in result.items()
                if key.endswith("_ns") and isinstance(value, int)
            }
        if completed and not result["errors"]:
            result["status"] = "passed"
        if owned_directory:
            try:
                (directory / "result.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
            except Exception as exc:
                result["status"] = "failed"
                record_error("write result", exc)
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, required=True, help="prebuilt vanilla-server executable")
    parser.add_argument("--output", type=Path, required=True, help="new isolated result directory")
    parser.add_argument("--revision", required=True, help="source revision of the executable")
    parser.add_argument("--pairs", type=int, default=3, help="cold/warm pairs (1..30)")
    args = parser.parse_args()
    if not 1 <= args.pairs <= 30:
        parser.error("--pairs must be 1..30")
    server = args.server.resolve(strict=True)
    if not server.is_file():
        parser.error("--server must be a file")
    output = args.output.absolute()
    output.mkdir(exist_ok=False)
    report = {"revision_user_supplied": args.revision, "server_sha256": None,
              "timestamp_utc": datetime.now(timezone.utc).isoformat(),
              "platform": None, "cpu": None, "python": platform.python_version(),
              "cold": "new flat world", "warm": "copy of corresponding stopped cold world",
              "configuration": {
                  "world_generation": "default IRW1 flat; no terrain/biome/expanded seed option",
                  "world_seed": None,
                  "gamemode": "creative via --creative",
                  "dimension": "default overworld",
                  "online_mode": False,
                  "difficulty": "default 2, unless warm world contains a persisted difficulty override; observed value recorded per Join Game",
                  "server_properties": "absent in isolated fixture; checked before startup",
                  "evidence": "apps/vanilla-server/src/main.rs defaults and world-load branch; apps/vanilla-server/src/world.rs Generation::Flat default",
              },
              "cache": "OS/disk cache uncontrolled", "client": "synthetic Protocol 5",
              "vanilla_1_7_10_client": "NOT_RUN", "pure_world_load": "NOT_MEASURED",
              "time_units": "monotonic nanoseconds; start before Popen; bound log observed by reader",
              "caveats": ["fixed order cold then warm; copying can warm OS cache",
                          "warm world also includes prior cold session and shutdown state",
                          "chunk decoding and client processing contribute to observed times",
                          "receive deadline is applied to each recv; other processing and cleanup have separate bounds",
                          "bound log is after bind, not a measurement of accept-loop readiness",
                          "no performance threshold or real-client compatibility claim"],
              "runs": [{"pair": index, "mode": mode, "status": "not_run",
                        "reason": "not started"}
                       for index in range(args.pairs) for mode in ("cold", "warm")]}
    try:
        report["server_sha256"] = sha256(server)
        report["platform"] = platform.platform()
        report["cpu"] = platform.processor()
        for index in range(args.pairs):
            cold_dir = output / f"pair-{index:02d}-cold"
            cold = measure(server, cold_dir, "cold", "ColdProbe", pair=index)
            report["runs"][index * 2] = cold
            if cold["status"] != "passed":
                report["runs"][index * 2 + 1]["reason"] = "cold failed"
                continue
            warm_dir = output / f"pair-{index:02d}-warm"
            try:
                warm_dir.mkdir(exist_ok=False)
                snapshot = copy_fixture(cold_dir / "world", warm_dir / "world")
                # WarmProbe is distinct so its first login has no saved player state.
                warm = measure(server, warm_dir, "warm", "WarmProbe", existing=True,
                               copied_fixture_sha256=snapshot, pair=index)
            except Exception as exc:
                warm = {"mode": "warm", "status": "failed",
                        "errors": [f"fixture setup: {type(exc).__name__}: {exc}"]}
            warm["pair"] = index
            report["runs"][index * 2 + 1] = warm
    except Exception as exc:
        report["harness_error"] = f"{type(exc).__name__}: {exc}"
    finally:
        report["successful_run_summary_ms"] = {}
        for mode in ("cold", "warm"):
            measured = [row["elapsed_ms"]["initial_join"] for row in report["runs"]
                        if row["mode"] == mode and row["status"] == "passed"]
            report["successful_run_summary_ms"][mode] = {
                "count": len(measured), "values": measured,
                "mean": statistics.fmean(measured) if measured else None,
                "median": statistics.median(measured) if measured else None,
                "min": min(measured) if measured else None,
                "max": max(measured) if measured else None,
                "note": "descriptive only; failed and missing runs remain in runs",
            }
        (output / "manifest.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    return 0 if len(report["runs"]) == args.pairs * 2 and all(
        row["status"] == "passed" for row in report["runs"]) else 1


if __name__ == "__main__":
    sys.exit(main())
