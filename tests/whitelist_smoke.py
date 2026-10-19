#!/usr/bin/env python3
"""Login admission tests: deny unknown names and fail closed on bad/revoked whitelist."""
import json
import socket
import sys
import time
from pathlib import Path

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1"
PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 25568
FILE = Path(sys.argv[3]) if len(sys.argv) > 3 else Path("whitelist.json")
ORIGINAL = FILE.read_bytes()

def varint(n):
    out = bytearray()
    while True:
        b = n & 127
        n >>= 7
        out.append(b | (128 if n else 0))
        if not n:
            return bytes(out)

def text(value):
    payload = value.encode()
    return varint(len(payload)) + payload

def frame(pid, data=b""):
    payload = varint(pid) + data
    return varint(len(payload)) + payload

def exact(connection, length):
    data = bytearray()
    while len(data) < length:
        item = connection.recv(length-len(data))
        if not item:
            raise EOFError("unexpected connection close")
        data.extend(item)
    return bytes(data)

def read_varint_stream(conn):
    n = 0
    for i in range(5):
        v = exact(conn, 1)[0]
        n |= (v & 127) << (i*7)
        if not v & 128:
            return n
    raise ValueError("invalid length")

def receive(conn):
    length = read_varint_stream(conn)
    payload = exact(conn, length)
    shift = 0
    packet_id = 0
    for i,b in enumerate(payload[:5]):
        packet_id |= (b & 127) << shift
        shift += 7
        if not b & 128:
            return packet_id, payload[i+1:]
    raise ValueError("invalid packet ID")

def login(name):
    conn = socket.create_connection((HOST, PORT), timeout=4)
    conn.settimeout(4)
    handshake = frame(0,varint(5)+text("localhost")+PORT.to_bytes(2,"big")+varint(2))
    conn.sendall(handshake+frame(0,text(name)))
    return conn

def denial(name):
    with login(name) as sock:
        pid, payload = receive(sock)
        assert pid == 0, (name,pid)
        assert b"whitelist" in payload.lower(), (name,payload)

def success(name):
    conn = login(name)
    pid,_ = receive(conn)
    assert pid == 2, (name,pid)
    return conn

try:
    # Always reject a player whose name is absent.
    denial("NotListed")
    # Malformed config must deny instead of allowing all.
    FILE.write_text('INVALID JSON')
    denial("RustTester")
    FILE.write_text('[]')
    denial("RustTester")
    # Live reloading at login.
    FILE.write_bytes(ORIGINAL)
    with success("RustTester") as conn:
        FILE.write_text('[]')
        deadline = time.monotonic() + 8
        kicked = False
        while time.monotonic() < deadline:
            pid, data = receive(conn)
            if pid == 0x40:
                assert b"whitelist" in data.lower()
                kicked = True
                break
        assert kicked, "live whitelist revocation not enforced"
finally:
    FILE.write_bytes(ORIGINAL)

# The server may still be saving a kicked player's NBT data when its
# disconnect packet is received. Gate subsequent integration tests on
# actual session release instead of relying on a timing sleep.
def online_players():
    with socket.create_connection((HOST,PORT),timeout=3) as conn:
        conn.settimeout(3)
        packet_handshake=frame(0,varint(5)+text("localhost")+PORT.to_bytes(2,"big")+varint(1))
        conn.sendall(packet_handshake+frame(0))
        pid,payload=receive(conn)
        assert pid==0
        size=0
        index=0
        for shift in range(0,35,7):
            byte=payload[index]
            index+=1
            size |= (byte & 127) << shift
            if not byte & 128: break
        return json.loads(payload[index:index+size])["players"]["online"]

deadline=time.monotonic()+6
while online_players() != 0:
    if time.monotonic() > deadline:
        raise AssertionError("revoked player session was not released")
    time.sleep(0.05)

print("Whitelist admission/invalid JSON/live revocation smoke test passed")
