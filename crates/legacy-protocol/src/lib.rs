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

/// Resumable Minecraft VarInt frame reader for socket connections with
/// short timeouts. Unlike read_exact, timed-out *partial* reads retain bytes
/// for the next poll rather than silently discarding the packet prefix.
///
/// This is appropriate for the Play state, where outbound events must be
/// serviced even when no incoming packet arrives.
#[derive(Default, Debug)]
pub struct BufferedFrameReader {
    received: Vec<u8>,
}
impl BufferedFrameReader {
    pub fn new() -> Self {
        Self::default()
    }
    fn take_complete(&mut self) -> io::Result<Option<Vec<u8>>> {
        let mut length = 0u32;
        let mut prefix = None;
        for (i, &byte) in self.received.iter().take(5).enumerate() {
            if i == 4 && byte > 15 {
                return Err(invalid("invalid fifth VarInt byte"));
            }
            length |= ((byte & 127) as u32) << (7 * i);
            if byte & 128 == 0 {
                prefix = Some(i + 1);
                break;
            }
        }
        let Some(header) = prefix else {
            if self.received.len() >= 5 {
                return Err(invalid("VarInt too long"));
            }
            return Ok(None);
        };
        let count = length as usize;
        if count == 0 || count > MAX_FRAME_BYTES {
            return Err(invalid("invalid frame size"));
        }
        if self.received.len() < header + count {
            return Ok(None);
        }
        let body = self.received[header..header + count].to_vec();
        self.received.drain(..header + count);
        Ok(Some(body))
    }
    /// Returns None only when the stream timed out or would block, preserving
    /// all bytes already received. EOF is an error, including a partial frame.
    pub fn poll<R: Read>(&mut self, reader: &mut R) -> io::Result<Option<Vec<u8>>> {
        if let Some(frame) = self.take_complete()? {
            return Ok(Some(frame));
        }
        // A valid max-size frame can be immediately followed by another
        // frame in the TCP stream. Bound each socket read to the remaining
        // capacity rather than over-reading and rejecting both packets.
        let capacity = (MAX_FRAME_BYTES + 5).saturating_sub(self.received.len());
        if capacity == 0 {
            return Err(invalid("receive buffer exceeds frame cap"));
        }
        let mut buf = [0u8; 8192];
        let count = match reader.read(&mut buf[..capacity.min(8192)]) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(count) => count,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                ) =>
            {
                return Ok(None)
            }
            Err(error) => return Err(error),
        };
        self.received.extend_from_slice(&buf[..count]);
        if self.received.len() > MAX_FRAME_BYTES + 5 {
            return Err(invalid("receive buffer exceeds frame cap"));
        }
        self.take_complete()
    }
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
    fn partial_frames_across_timeouts_are_lossless() {
        struct Fragments {
            pieces: std::collections::VecDeque<io::Result<Vec<u8>>>,
        }
        impl Read for Fragments {
            fn read(&mut self, data: &mut [u8]) -> io::Result<usize> {
                let part = self.pieces.pop_front().expect("unexpected read");
                let buf = part?;
                data[..buf.len()].copy_from_slice(&buf);
                Ok(buf.len())
            }
        }
        let timeout = || Err(io::Error::from(io::ErrorKind::TimedOut));
        let mut src = Fragments {
            pieces: std::collections::VecDeque::from([
                Ok(vec![0x83]),
                timeout(),
                Ok(vec![0x01, 0x01]),
                timeout(),
                Ok(vec![7; 130]),
                timeout(),
                Ok(vec![5, 42, 43, 44, 45, 46]),
            ]),
        };
        let mut reader = BufferedFrameReader::new();
        assert!(reader.poll(&mut src).unwrap().is_none());
        assert!(reader.poll(&mut src).unwrap().is_none());
        assert!(reader.poll(&mut src).unwrap().is_none());
        assert!(reader.poll(&mut src).unwrap().is_none());
        let first = reader.poll(&mut src).unwrap().unwrap();
        assert_eq!(first.len(), 131);
        assert_eq!(first[0], 1);
        assert_eq!(first[1..], [7; 130]);
        assert!(reader.poll(&mut src).unwrap().is_none());
        let second = reader.poll(&mut src).unwrap().unwrap();
        assert_eq!(second, vec![42, 43, 44, 45, 46]);
    }
    #[test]
    fn buffered_reader_rejects_bad_lengths_and_incomplete_eof() {
        let mut reader = BufferedFrameReader::new();
        assert!(reader.poll(&mut &[0u8][..]).is_err());
        let mut reader = BufferedFrameReader::new();
        assert!(reader.poll(&mut &[0x80u8][..]).unwrap().is_none());
        assert!(reader.poll(&mut &[][..]).is_err());
        let mut reader = BufferedFrameReader::new();
        assert!(reader
            .poll(&mut &[0xff, 0xff, 0xff, 0xff, 0x7f][..])
            .is_err());
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


    #[test]
    fn max_size_frame_followed_by_small_frame_does_not_false_overflow() {
        let first = vec![0x5a; MAX_FRAME_BYTES];
        let mut stream = Vec::new();
        write_frame(&mut stream, &first).unwrap();
        write_frame(&mut stream, &[0x01, 0x02]).unwrap();
        let mut stream = stream.as_slice();
        let mut reader = BufferedFrameReader::new();

        let received = loop {
            if let Some(packet) = reader.poll(&mut stream).unwrap() {
                break packet;
            }
        };
        assert_eq!(received, first);
        let second = reader.poll(&mut stream).unwrap().unwrap();
        assert_eq!(second, [0x01, 0x02]);
        assert!(reader.poll(&mut stream).is_err()); // Clean EOF.
    }
}
