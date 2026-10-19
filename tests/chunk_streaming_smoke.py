#!/usr/bin/env python3
"""Protocol 5 integration: client receives 5x5 rolling chunk window, both X directions."""
import socket
import struct
import sys
import time
import zlib

from protocol5_test_utils import wait_for_idle

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1"
PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 25568

def vi(v):
    out=bytearray()
    while True:
        b=v & 127
        v >>= 7
        out.append(b | (128 if v else 0))
        if not v:return bytes(out)

def string(s):
    b=s.encode()
    return vi(len(b))+b

def frame(pid,payload=b""):
    body=vi(pid)+payload
    return vi(len(body))+body

def take(sock,n):
    buf=bytearray()
    while len(buf)<n:
        part=sock.recv(n-len(buf))
        if not part:raise EOFError("closed while reading chunk response")
        buf.extend(part)
    return bytes(buf)

def read_packet(sock):
    length=0
    for i in range(5):
        b=take(sock,1)[0]
        length |= (b & 127) << (7*i)
        if not b & 128:break
    assert 1<=length<=1048576,length
    raw=take(sock,length)
    pid=0
    for i,b in enumerate(raw[:5]):
        pid |= (b & 127) << (7*i)
        if not b & 128:return pid,raw[i+1:]
    raise AssertionError("bad packet ID")

def read_chunks(sock,load,unload=0):
    got_load=set()
    got_unload=set()
    deadline=time.monotonic()+10
    while time.monotonic()<deadline and (len(got_load)<load or len(got_unload)<unload):
        pid,buf=read_packet(sock)
        if pid!=0x21:continue
        x,z=struct.unpack_from(">ii",buf)
        assert buf[8]==1
        mask,extra=struct.unpack_from(">HH",buf,9)
        assert extra==0
        size=struct.unpack_from(">i",buf,13)[0]
        compressed=buf[17:]
        assert len(compressed)==size, (x,z,len(compressed),size)
        if mask==0:
            assert size==0
            got_unload.add((x,z))
        else:
            assert mask & 0x18==0x18,(x,z,mask)
            data=zlib.decompress(compressed)
            assert len(data)==mask.bit_count()*10240+256,(x,z,mask,len(data))
            got_load.add((x,z))
    assert len(got_load)==load,(len(got_load),got_load)
    assert len(got_unload)==unload,(len(got_unload),got_unload)
    return got_load,got_unload

wait_for_idle(HOST,PORT)
s=socket.create_connection((HOST,PORT),timeout=5)
s.settimeout(10)
try:
    s.sendall(frame(0,vi(5)+string("localhost")+struct.pack(">H",PORT)+vi(2))+frame(0,string("ChunkScout")))
    assert read_packet(s)[0]==2
    initial,_=read_chunks(s,25)
    assert initial=={(x,z) for x in range(-2,3) for z in range(-2,3)}
    s.sendall(frame(0x04,struct.pack(">ddddB",55.5,57.12,55.5,0.5,1)))
    east,retired=read_chunks(s,15,15)
    assert east=={(x,z) for x in range(3,6) for z in range(-2,3)}
    assert retired=={(x,z) for x in range(-2,1) for z in range(-2,3)}
    s.sendall(frame(0x04,struct.pack(">ddddB",-50.5,57.12,55.5,0.5,1)))
    west,old=read_chunks(s,25,25)
    assert west=={(x,z) for x in range(-6,-1) for z in range(-2,3)}
    assert old=={(x,z) for x in range(1,6) for z in range(-2,3)}
finally:
    s.close()
wait_for_idle(HOST,PORT)
print("Protocol 5 streaming: 25 initial chunks, 15+15 east update, 25+25 west update passed")
