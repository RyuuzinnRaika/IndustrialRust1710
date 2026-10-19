"""Small synchronous Protocol 5 status probe for serialized integration tests.

An ACK for a disconnect packet is not the same as the server having fully
released the associated admission slot. Poll the status endpoint to avoid
mistaking a concurrent-session rejection for a login regression.
"""
import json
import socket
import time

def varint(value):
    out = bytearray()
    while True:
        part = value & 0x7f
        value >>= 7
        out.append(part | (0x80 if value else 0))
        if not value:
            return bytes(out)

def take(sock, amount):
    data = bytearray()
    while len(data) < amount:
        piece = sock.recv(amount - len(data))
        if not piece:
            raise EOFError("incomplete protocol5 status frame")
        data += piece
    return bytes(data)

def read_varint(sock):
    val = 0
    for i in range(5):
        b = take(sock, 1)[0]
        val |= (b & 127) << (7*i)
        if not b & 128:
            return val
    raise ValueError("invalid protocol5 VarInt")

def online_count(host, port):
    host_bytes = b"localhost"
    handshake = varint(0) + varint(5) + varint(len(host_bytes)) + host_bytes + port.to_bytes(2,"big") + varint(1)
    request = varint(len(handshake)) + handshake + b"\x01\x00"
    with socket.create_connection((host, port), timeout=3) as conn:
        conn.settimeout(3)
        conn.sendall(request)
        frame_length = read_varint(conn)
        if frame_length < 3 or frame_length > 65536:
            raise ValueError("invalid status response length")
        payload = take(conn, frame_length)
        # A single Status Response packet id 0, followed by VarInt string.
        if payload[0] != 0:
            raise ValueError("unexpected status packet ID")
        position = 1
        size = 0
        for i in range(5):
            byte = payload[position]
            position += 1
            size |= (byte & 127) << (7 * i)
            if not byte & 128:
                break
        status = json.loads(payload[position:position+size])
        return status["players"]["online"]

def wait_for_idle(host, port, timeout=8):
    deadline = time.monotonic() + timeout
    while True:
        if online_count(host, port) == 0:
            return
        if time.monotonic() >= deadline:
            raise AssertionError("previous Protocol 5 player session was not released")
        time.sleep(0.05)
