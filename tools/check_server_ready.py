#!/usr/bin/env python3
"""Read-only Minecraft Java 1.7.10 (Protocol 5) server readiness probe.

Requires only the Python standard library. A TCP listener alone does not count
as ready: the service must respond with IndustrialRust1710 status Protocol 5.
"""

import argparse
import json
import socket
import struct
import sys
import time


MAX_STATUS_FRAME = 32 * 1024


def _varint(value):
    if not 0 <= value < 2**32:
        raise ValueError("VarInt out of range")
    data = bytearray()
    while True:
        byte = value & 0x7f
        value >>= 7
        data.append(byte | (0x80 if value else 0))
        if not value:
            return bytes(data)


def _read_exact(connection, count):
    data = bytearray()
    while len(data) < count:
        part = connection.recv(count - len(data))
        if not part:
            raise ConnectionError("status connection closed early")
        data.extend(part)
    return bytes(data)


def _read_varint(connection):
    value = 0
    for shift in range(0, 35, 7):
        byte = _read_exact(connection, 1)[0]
        value |= (byte & 0x7f) << shift
        if not byte & 0x80:
            if value >= 2**32:
                raise ValueError("status VarInt overflow")
            return value
    raise ValueError("status VarInt is too long")


def _read_frame(connection):
    length = _read_varint(connection)
    if length == 0 or length > MAX_STATUS_FRAME:
        raise ValueError("invalid status response frame length")
    return _read_exact(connection, length)


def _frame(payload):
    return _varint(len(payload)) + payload


def probe(host, port, timeout=2.0):
    """Probe status once. Return a verified status dict; raise on failure."""
    if not 1 <= port <= 65535 or timeout <= 0:
        raise ValueError("invalid port or timeout")
    host_bytes = host.encode("utf-8")
    if not host_bytes or len(host_bytes) > 255:
        raise ValueError("invalid hostname")
    handshake = (
        _varint(0) + _varint(5) + _varint(len(host_bytes)) + host_bytes
        + struct.pack(">H", port) + _varint(1)
    )
    with socket.create_connection((host, port), timeout=timeout) as connection:
        connection.settimeout(timeout)
        connection.sendall(_frame(handshake) + _frame(_varint(0)))
        payload = _read_frame(connection)
        # A status response contains VarInt packet-id 0, then VarInt string size.
        packet_id, idx = _decode_varint(payload, 0)
        if packet_id != 0:
            raise ValueError("server returned a non-status packet")
        text_length, idx = _decode_varint(payload, idx)
        if text_length > MAX_STATUS_FRAME or len(payload) - idx != text_length:
            raise ValueError("malformed status JSON length")
        status = json.loads(payload[idx:].decode("utf-8"))
    if (
        not isinstance(status, dict)
        or not isinstance(status.get("version"), dict)
        or not isinstance(status.get("description"), dict)
    ):
        raise ValueError("malformed status object")
    if status.get("version", {}).get("protocol") != 5:
        raise ValueError("not a Minecraft Java 1.7.10 Protocol 5 server")
    if "IndustrialRust1710" not in status.get("description", {}).get("text", ""):
        raise ValueError("status responder is not IndustrialRust1710")
    return status


def _decode_varint(data, offset):
    value = 0
    for i in range(5):
        if offset + i >= len(data):
            raise ValueError("truncated packet VarInt")
        byte = data[offset + i]
        value |= (byte & 0x7f) << (7 * i)
        if not byte & 0x80:
            if value >= 2**32:
                raise ValueError("packet VarInt overflow")
            return value, offset + i + 1
    raise ValueError("packet VarInt is too long")


def wait_for_ready(host, port, timeout=30.0, interval=0.25):
    if timeout <= 0 or interval <= 0:
        raise ValueError("timeout and interval must be positive")
    deadline = time.monotonic() + timeout
    last_error = "no status response"
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError(last_error)
        try:
            return probe(host, port, timeout=min(2.0, remaining))
        except (OSError, ValueError, ConnectionError, json.JSONDecodeError) as error:
            last_error = str(error)
        time.sleep(min(interval, max(0.0, deadline - time.monotonic())))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=25565)
    parser.add_argument("--timeout", type=float, default=30.0)
    parser.add_argument("--interval", type=float, default=0.25)
    args = parser.parse_args(argv)
    try:
        status = wait_for_ready(args.host, args.port, args.timeout, args.interval)
    except (ValueError, OSError, TimeoutError) as error:
        print(f"NOT_READY: {error}", file=sys.stderr)
        return 1
    print(f"READY: IndustrialRust1710 Protocol {status['version']['protocol']} on {args.host}:{args.port}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
