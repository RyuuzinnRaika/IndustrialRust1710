#!/usr/bin/env python3
"""Read only an exported 1.7.10 Anvil snapshot using the public region container layout."""
import gzip
import struct
import sys
import zlib
from pathlib import Path

root = Path(sys.argv[1])
assert root.is_dir()
level = gzip.decompress((root/"level.dat").read_bytes())
assert level[:1] == b"\x0a" and b"IndustrialRust1710 Export" in level
region = root/"region"/"r.0.0.mca"
raw = region.read_bytes()
assert len(raw) >= 8192 and len(raw)%4096 == 0
location = raw[:4]
sector = int.from_bytes(location[:3],"big")
count = location[3]
assert sector >= 2 and 1 <= count <= 255
assert sector+count <= len(raw)//4096
at = sector*4096
length = struct.unpack_from(">I",raw,at)[0]
assert length>=2 and length<=count*4096-4
assert raw[at+4] == 2  # zlib
chunk = zlib.decompress(raw[at+5:at+4+length])
assert chunk[0] == 10
for tag in [b"xPos",b"zPos",b"Blocks",b"Data",b"SkyLight",b"BlockLight",b"HeightMap",b"Sections"]:
    assert tag in chunk, tag
assert (root/"region"/"r.-1.-1.mca").exists()
assert (root/"region"/"r.-1.0.mca").exists()
assert (root/"region"/"r.0.-1.mca").exists()
print("Anvil 1.7.10 export: gzip level.dat, 4KiB region sectors, zlib chunk NBT passed")
