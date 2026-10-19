#!/usr/bin/env python3
"""Regression for split TCP frames in a 50ms-polled Protocol 5 Play session."""
import socket
import struct
import sys
import time
from protocol5_test_utils import wait_for_idle

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1"
PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 25568

def vi(n):
    out = bytearray()
    while True:
        value = n & 127
        n >>= 7
        out.append(value | (128 if n else 0))
        if not n:
            return bytes(out)

def string(value):
    data = value.encode()
    return vi(len(data)) + data

def frame(pid, body=b""):
    payload = vi(pid) + body
    return vi(len(payload)) + payload

def take(s, count):
    result = bytearray()
    while len(result) < count:
        chunk = s.recv(count - len(result))
        if not chunk:
            raise EOFError("server closed during fragmented Protocol 5 test")
        result.extend(chunk)
    return bytes(result)

def varint_socket(s):
    value = 0
    for i in range(5):
        byte = take(s, 1)[0]
        value |= (byte & 127) << (7 * i)
        if not byte & 128:
            return value
    raise AssertionError("bad packet VarInt")

def packet(s):
    size = varint_socket(s)
    assert 1 <= size <= 1048576
    raw = take(s, size)
    shift = 0
    packet_id = 0
    for i, byte in enumerate(raw[:5]):
        packet_id |= (byte & 127) << shift
        if not byte & 128:
            return packet_id, raw[i+1:]
        shift += 7
    raise AssertionError("invalid packet id")

def send_fragmented(s, raw, groups):
    index = 0
    for count in groups:
        s.sendall(raw[index:index + count])
        index += count
        time.sleep(0.12)  # Greater than the server's 50ms TCP receive timeout.
    s.sendall(raw[index:])

wait_for_idle(HOST, PORT)
with socket.create_connection((HOST, PORT), timeout=8) as s:
    s.settimeout(8)
    handshake = vi(5) + string("localhost") + struct.pack(">H", PORT) + vi(2)
    s.sendall(frame(0, handshake) + frame(0, string("RustTester")))
    assert packet(s)[0] == 2
    for _ in range(70):
        pid, body = packet(s)
        if pid == 0x08:
            break
    else:
        raise AssertionError("server never sent the initial player position")

    # Fragment both the size and body, waiting more than one receive timeout.
    request = frame(1, string("/list"))
    send_fragmented(s, request, [1, 1, 1, 2])
    for _ in range(50):
        pid, body = packet(s)
        if pid == 0x02 and b"Online (1)" in body:
            break
    else:
        raise AssertionError("no /list response after split TCP frames")

    # A second packet proves no trailing bytes or VarInt prefix were lost.
    request = frame(1, string("/help"))
    send_fragmented(s, request, [1, 2])
    for _ in range(50):
        pid, body = packet(s)
        if pid == 0x02 and b"Commands:" in body:
            break
    else:
        raise AssertionError("framing desynchronized after the first fragmented request")

wait_for_idle(HOST, PORT)
print("Protocol 5 fragmented TCP frames preserve two play commands across timeouts")
