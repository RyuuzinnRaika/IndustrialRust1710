#!/usr/bin/env python3
"""Persisted player position and out-of-range correction against real Protocol 5 TCP."""
import json
import socket
import struct
import sys
import time
from pathlib import Path

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1"
PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 25568
SAVE = Path(sys.argv[3]) if len(sys.argv) > 3 else Path("world/players/positioner.json")

def vi(n):
    b = bytearray()
    while True:
        x = n & 127
        n >>= 7
        b.append(x | (128 if n else 0))
        if not n: return bytes(b)

def string(s):
    data = s.encode()
    return vi(len(data)) + data

def frame(pid, body=b""):
    payload = vi(pid)+body
    return vi(len(payload)) + payload

def exact(s,n):
    b=bytearray()
    while len(b)<n:
        part=s.recv(n-len(b))
        if not part: raise EOFError()
        b+=part
    return bytes(b)

def packet(s):
    count=0
    for i in range(5):
        v=exact(s,1)[0]
        count|=(v & 127) << (7*i)
        if not v & 128:break
    data=exact(s,count)
    pid=0
    for i,v in enumerate(data[:5]):
        pid|=(v & 127) << (7*i)
        if not v & 128: return pid,data[i+1:]
    raise ValueError()

def connect():
    s=socket.create_connection((HOST, PORT),timeout=5)
    s.settimeout(5)
    s.sendall(frame(0,vi(5)+string("localhost")+struct.pack(">H",PORT)+vi(2))+frame(0,string("Positioner")))
    pid,_=packet(s)
    assert pid==2, pid
    for _ in range(40):
        pid,content=packet(s)
        if pid==0x08:
            return s,struct.unpack(">ddd",content[:24])
    raise AssertionError("no initial position")

if SAVE.exists():
    SAVE.unlink()
client,pos=connect()
assert pos == (0.5,65.0,0.5), pos
# 1.7.10 position: x, stance, feet y, z, onGround
client.sendall(frame(0x04,struct.pack(">ddddB",3.5,66.62,65.0,2.5,1)))
client.close()
for _ in range(40):
    if SAVE.is_file():
        try:
            data=json.loads(SAVE.read_text())
            if (data["x"],data["y"],data["z"]) == (3.5,65.0,2.5):break
        except (ValueError,KeyError):pass
    time.sleep(0.1)
else:
    raise AssertionError("did not persist player position")
client,pos=connect()
assert pos == (3.5,65.0,2.5), pos
# Movement beyond loaded chunk range must be corrected to authoritative position.
client.sendall(frame(0x04,struct.pack(">ddddB",55.5,66.62,65.0,2.5,1)))
for _ in range(10):
    pid,content=packet(client)
    if pid==0x08:
        assert struct.unpack(">ddd",content[:24]) == pos
        break
else:
    raise AssertionError("out-of-world position was not corrected")
client.close()
print("Player save/reconnect and out-of-bounds correction smoke test passed")
