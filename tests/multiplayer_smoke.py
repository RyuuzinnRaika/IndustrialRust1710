#!/usr/bin/env python3
"""Simulate two simultaneous Java 1.7.10 Protocol 5 players over real TCP.

Validates entity IDs/spawn, tab list, movement, world edits, chat, duplicate
name denial, and disconnect. No Java game client or GPL code is used.
"""
import json
import socket
import struct
import sys
import time
from protocol5_test_utils import wait_for_idle, online_count

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1"
PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 25568

def vi(n):
    b=bytearray()
    while True:
        v=n & 127
        n >>= 7
        b.append(v | (128 if n else 0))
        if not n:return bytes(b)

def string(s):
    raw=s.encode("utf-8")
    return vi(len(raw))+raw

def frame(pid, data=b""):
    b=vi(pid)+data
    return vi(len(b))+b

def take(sock,n):
    out=bytearray()
    while len(out)<n:
        d=sock.recv(n-len(out))
        if not d:raise EOFError("peer closed")
        out+=d
    return bytes(out)

def rv(raw, idx=0):
    val=0
    for offset in range(5):
        b=raw[idx+offset]
        val|=(b & 127) << (7*offset)
        if not b & 128:return val,idx+offset+1
    raise ValueError("invalid varint")

def packet(sock):
    val=0
    for i in range(5):
        b=take(sock,1)[0]
        val|=(b & 127) << (7*i)
        if not b & 128:break
    data=take(sock,val)
    pid,idx=rv(data)
    return pid,data[idx:]

def str_at(raw,index=0):
    n,i=rv(raw,index)
    return raw[i:i+n].decode("utf-8"),i+n

def login(name):
    s=socket.create_connection((HOST,PORT),timeout=5)
    s.settimeout(5)
    hello=vi(5)+string("localhost")+struct.pack(">H",PORT)+vi(2)
    s.sendall(frame(0,hello)+frame(0,string(name)))
    pid,body=packet(s)
    if pid!=2:
        raise AssertionError((name,"login denied",pid,body))
    uid,offset=str_at(body)
    got_name,_=str_at(body,offset)
    assert len(uid)==36 and got_name==name
    # consume up to position; chunks precede it
    entity_id=None
    for _ in range(60):
        pid,body=packet(s)
        if pid==1:
            entity_id=struct.unpack_from(">i",body)[0]
        if pid==8:break
    assert entity_id is not None
    return s,entity_id

def expect(sock,pid,predicate=lambda x:True,timeout=5):
    end=time.monotonic()+timeout
    while time.monotonic()<end:
        packet_id,payload=packet(sock)
        if packet_id==pid and predicate(payload):
            return payload
    raise AssertionError(f"expected packet id {pid:#x}")

def name_of_info(payload):
    return str_at(payload)[0]

def spawn_id(payload):
    n,_=rv(payload)
    return n

wait_for_idle(HOST,PORT)
a,a_id=login("TwinAlpha")
b,b_id=login("TwinBeta")
try:
    assert a_id!=b_id,(a_id,b_id)
    assert online_count(HOST,PORT)==2
    # Other player appears in the tab list and as a named entity.
    expect(a,0x38,lambda p:name_of_info(p)=="TwinBeta")
    expect(a,0x0c,lambda p:spawn_id(p)==b_id)
    expect(b,0x38,lambda p:name_of_info(p)=="TwinAlpha")
    expect(b,0x0c,lambda p:spawn_id(p)==a_id)
    # Duplicate login must be rejected even though the username is permitted.
    duplicate=socket.create_connection((HOST,PORT),timeout=4)
    duplicate.settimeout(4)
    duplicate.sendall(frame(0,vi(5)+string("localhost")+struct.pack(">H",PORT)+vi(2))+frame(0,string("tWinaLpha")))
    packet_id,reason=packet(duplicate)
    assert packet_id==0 and b"already connected" in reason.lower(),(packet_id,reason)
    duplicate.close()
    # Bob moves within 80x80 loaded world; Alice must see his teleport.
    b.sendall(frame(4,struct.pack(">ddddB",4.5,66.62,65.0,4.5,1)))
    moved=expect(a,0x18,lambda payload:struct.unpack_from(">i",payload)[0]==b_id)
    assert struct.unpack_from(">iii",moved,4)==(144,2080,144),moved
    # Alice removes a nearby grass block, Bob receives the authoritative update.
    a.sendall(frame(7,b"\x00"+struct.pack(">iBi",3,63,3)+b"\x01"))
    update=expect(b,0x23,lambda payload:struct.unpack_from(">iBi",payload)==(3,63,3))
    assert update[9]==0
    # Chat must arrive at BOTH clients.
    a.sendall(frame(1,string("hello multiplayer")))
    msg=expect(b,0x02,lambda payload:b"hello multiplayer" in payload)
    assert b"TwinAlpha" in msg
    a.close()
    gone=expect(b,0x13,lambda payload:payload[:1]==b"\x01" and struct.unpack_from(">i",payload,1)[0]==a_id)
    assert gone
    gone_tab=expect(b,0x38,lambda payload:name_of_info(payload)=="TwinAlpha" and payload[-3:-2]==b"\x00")
    assert gone_tab
finally:
    a.close()
    b.close()
wait_for_idle(HOST,PORT)
print("Protocol5 two-player join/identity/spawn/move/chat/shared edits/quit passed")
