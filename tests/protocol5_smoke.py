#!/usr/bin/env python3
"""Real socket smoke test for Rust 1.7.10 Status/Ping and honest Login rejection."""
import json,socket,sys
def vi(n):
 out=bytearray()
 while True:
  if n<128:out.append(n);return bytes(out)
  out.append((n&127)|128);n>>=7
def st(s):
 b=s.encode();return vi(len(b))+b
def frame(b):return vi(len(b))+b
def exact(c,n):
 b=bytearray()
 while len(b)<n:
  part=c.recv(n-len(b))
  if not part:raise EOFError()
  b.extend(part)
 return bytes(b)
def rv(c):
 n=0
 for i in range(5):
  b=exact(c,1)[0];n|=(b&127)<<(7*i)
  if not b&128:return n
 raise ValueError()
def packet(c):
 b=exact(c,rv(c))
 for i,ch in enumerate(b[:5]):
  if not ch&128:return ch,b[i+1:]
 raise ValueError()
def string(b):
 n=b[0];assert n==len(b)-1;return b[1:].decode()
host=sys.argv[1] if len(sys.argv)>1 else "127.0.0.1"
port=int(sys.argv[2]) if len(sys.argv)>2 else 25565
def handshake(state):return frame(vi(0)+vi(5)+st("localhost")+port.to_bytes(2,"big")+vi(state))
with socket.create_connection((host,port),timeout=4) as c:
 c.settimeout(4);c.sendall(handshake(1)+frame(vi(0)))
 pid,data=packet(c);assert pid==0
 status=json.loads(string(data));assert status["version"]["protocol"]==5
 payload=(123456789).to_bytes(8,"big",signed=True);c.sendall(frame(vi(1)+payload))
 pid,content=packet(c);assert pid==1 and content==payload
 print("Protocol 5 Status/Ping passed")
with socket.create_connection((host,port),timeout=4) as c:
 c.settimeout(4);c.sendall(handshake(2)+frame(vi(0)+st("TestPlayer")))
 pid,data=packet(c);assert pid==0 and "not yet implemented" in string(data)
 print("Login rejection passed")
