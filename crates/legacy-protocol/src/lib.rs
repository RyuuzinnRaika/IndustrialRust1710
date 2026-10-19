//! Bounded framing and primitive packet parsing for Minecraft Java 1.7.10 (protocol 5).
use std::io::{self, Read, Write};
pub const PROTOCOL_VERSION: i32 = 5;
pub const MAX_FRAME_BYTES: usize = 1_048_576;
fn invalid(s: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, s)
}
pub fn write_varint(mut n: u32, out: &mut Vec<u8>) {
    loop {
        if n & !127 == 0 {
            out.push(n as u8);
            break;
        }
        out.push((n as u8 & 127) | 128);
        n >>= 7;
    }
}
pub fn read_varint<R: Read>(r: &mut R) -> io::Result<u32> {
    let mut n = 0u32;
    for i in 0..5 {
        let mut b = [0];
        r.read_exact(&mut b)?;
        if i == 4 && b[0] > 15 {
            return Err(invalid("invalid fifth VarInt byte"));
        }
        n |= ((b[0] & 127) as u32) << (7 * i);
        if b[0] & 128 == 0 {
            return Ok(n);
        }
    }
    Err(invalid("VarInt too long"))
}
pub fn read_frame<R: Read>(r: &mut R) -> io::Result<Vec<u8>> {
    let len = read_varint(r)? as usize;
    if len == 0 || len > MAX_FRAME_BYTES {
        return Err(invalid("invalid frame size"));
    }
    let mut b = vec![0; len];
    r.read_exact(&mut b)?;
    Ok(b)
}
pub fn write_frame<W: Write>(w: &mut W, p: &[u8]) -> io::Result<()> {
    if p.is_empty() || p.len() > MAX_FRAME_BYTES {
        return Err(invalid("invalid outgoing frame"));
    }
    let mut b = Vec::new();
    write_varint(p.len() as u32, &mut b);
    w.write_all(&b)?;
    w.write_all(p)
}
pub fn write_string(s: &str, out: &mut Vec<u8>) -> io::Result<()> {
    if s.len() > MAX_FRAME_BYTES - 5 {
        return Err(invalid("string too long"));
    }
    write_varint(s.len() as u32, out);
    out.extend_from_slice(s.as_bytes());
    Ok(())
}
pub struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl<'a> Cursor<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or_else(|| invalid("overflow"))?;
        let bytes = self
            .bytes
            .get(self.pos..end)
            .ok_or(io::ErrorKind::UnexpectedEof)?;
        self.pos = end;
        Ok(bytes)
    }
    pub fn read_varint(&mut self) -> io::Result<i32> {
        let mut r = &self.bytes[self.pos..];
        let before = r.len();
        let v = read_varint(&mut r)?;
        self.pos += before - r.len();
        Ok(v as i32)
    }
    pub fn read_u16(&mut self) -> io::Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
    pub fn read_i64(&mut self) -> io::Result<i64> {
        let b = self.take(8)?;
        Ok(i64::from_be_bytes(
            b.try_into().map_err(|_| invalid("short i64"))?,
        ))
    }
    pub fn read_string(&mut self, max_chars: usize) -> io::Result<&'a str> {
        let n = self.read_varint()?;
        if n < 0 || (n as usize) > max_chars.saturating_mul(4) {
            return Err(invalid("string length"));
        }
        let b = self.take(n as usize)?;
        let s = std::str::from_utf8(b).map_err(|_| invalid("utf8"))?;
        if s.chars().count() > max_chars {
            return Err(invalid("string chars"));
        }
        Ok(s)
    }
    pub fn finish(&self) -> io::Result<()> {
        if self.pos == self.bytes.len() {
            Ok(())
        } else {
            Err(invalid("trailing bytes"))
        }
    }
}
pub fn packet_id_and_body(data: &[u8]) -> io::Result<(i32, Cursor<'_>)> {
    let mut c = Cursor::new(data);
    let id = c.read_varint()?;
    Ok((id, c))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn varints() {
        for n in [0, 1, 127, 128, 255, u32::MAX] {
            let mut b = Vec::new();
            write_varint(n, &mut b);
            assert_eq!(read_varint(&mut b.as_slice()).unwrap(), n);
        }
    }
    #[test]
    fn malformed() {
        assert!(read_varint(&mut &[128, 128, 128, 128, 16][..]).is_err());
        assert!(read_frame(&mut &[0][..]).is_err());
    }
    #[test]
    fn frames() {
        let mut b = Vec::new();
        write_frame(&mut b, &[1, 2, 3]).unwrap();
        assert_eq!(read_frame(&mut b.as_slice()).unwrap(), [1, 2, 3]);
    }
    #[test]
    fn cursor_fields() {
        let mut b = Vec::new();
        write_varint(5, &mut b);
        write_string("localhost", &mut b).unwrap();
        b.extend_from_slice(&25565u16.to_be_bytes());
        let mut c = Cursor::new(&b);
        assert_eq!(c.read_varint().unwrap(), 5);
        assert_eq!(c.read_string(255).unwrap(), "localhost");
        assert_eq!(c.read_u16().unwrap(), 25565);
        c.finish().unwrap();
    }
}
