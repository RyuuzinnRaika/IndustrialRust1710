#!/usr/bin/env python3
"""Protocol 5 socket smoke test for native Rust vanilla-only server."""
import json
import socket
import struct
import sys
import time
import zlib
from pathlib import Path

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1"
PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 25568
SAVE = Path(sys.argv[3]) if len(sys.argv) > 3 else None

def vi(n):
    b = bytearray()
    while True:
        part = n & 127
        n >>= 7
        b.append(part | (128 if n else 0))
        if not n:
            return bytes(b)

def string(s):
    data = s.encode()
    return vi(len(data)) + data

def frame(packet_id, content=b""):
    packet = vi(packet_id) + content
    return vi(len(packet)) + packet

def exact(sock, n):
    out = bytearray()
    while len(out) < n:
        part = sock.recv(n-len(out))
        if not part:
            raise EOFError("socket closed")
        out += part
    return bytes(out)

def read_varint(data, pos=0):
    n = 0
    for i in range(5):
        current = data[pos+i]
        n |= (current & 127) << (7*i)
        if not (current & 128):
            return n, pos+i+1
    raise ValueError("invalid varint")

def packet(sock):
    header = bytearray()
    for _ in range(5):
        a = exact(sock, 1)[0]
        header.append(a)
        if not (a & 128):
            break
    length, _ = read_varint(header)
    assert 0 < length <= 1048576
    data = exact(sock, length)
    pid, start = read_varint(data)
    return pid, data[start:]

def decode_string(data):
    n, pos = read_varint(data)
    return data[pos:pos+n].decode()

def handshake(state):
    data = vi(5)+string("localhost")+struct.pack(">H", PORT)+vi(state)
    return frame(0, data)

with socket.create_connection((HOST, PORT), timeout=5) as sock:
    sock.settimeout(5)
    sock.sendall(handshake(1) + frame(0))
    pid, data = packet(sock)
    status = json.loads(decode_string(data))
    assert pid == 0 and status["version"]["protocol"] == 5
    assert status["players"]["max"] == 1

with socket.create_connection((HOST, PORT), timeout=5) as sock:
    sock.settimeout(5)
    sock.sendall(handshake(2) + frame(0, string("RustTester")))
    pid, data = packet(sock)
    assert pid == 2
    uuid = decode_string(data)
    assert len(uuid) == 36
    name_start = len(vi(len(uuid))) + len(uuid)
    assert decode_string(data[name_start:]) == "RustTester"
    got = set()
    chunks = 0
    for _ in range(45):
        pid, data = packet(sock)
        got.add(pid)
        if pid == 0x01:
            assert struct.unpack_from(">i", data)[0] == 1
            assert data[4] == 1
        if pid == 0x21:
            chunks += 1
            assert len(data) >= 17
            primary, extra = struct.unpack_from(">HH", data, 9)
            assert (primary, extra) == (0x18, 0)
            compressed_len = struct.unpack_from(">i", data, 13)[0]
            blocks = zlib.decompress(data[17:17+compressed_len])
            assert len(blocks) == 20736
        if pid == 0x08:
            break
    assert chunks == 25, chunks
    assert {0x01, 0x05, 0x06, 0x21, 0x08}.issubset(got), got
    # Dig one grass block; server should confirm air.
    dig = b"\x00" + struct.pack(">iBi", 0,63,0) + b"\x01"
    sock.sendall(frame(0x07, dig))
    for _ in range(5):
        pid, data = packet(sock)
        if pid == 0x23:
            assert struct.unpack_from(">iBi", data) == (0,63,0)
            assert data[9] == 0
            break
    else:
        raise AssertionError("no dig update")
    # Place cobblestone above another grass block.
    slot = struct.pack(">hBh",4,1,0) + struct.pack(">h",-1)
    place = struct.pack(">iBi",1,63,0) + b"\x01" + slot + b"\x08\x08\x08"
    sock.sendall(frame(0x08, place))
    for _ in range(5):
        pid, data = packet(sock)
        if pid == 0x23:
            assert struct.unpack_from(">iBi", data) == (1,64,0)
            assert data[9] == 4
            break
    else:
        raise AssertionError("no place update")
if SAVE:
    for _ in range(20):
        if SAVE.exists() and SAVE.stat().st_size >= 30:
            break
        time.sleep(0.1)
    assert SAVE.exists()
    save = SAVE.read_bytes()
    assert save[:4] == b"IRW1"
    assert struct.unpack(">I", save[4:8])[0] == 2
print("Vanilla Protocol 5 Login/Play/chunks/edit/persistence smoke test passed")
