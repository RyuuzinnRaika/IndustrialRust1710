//! A resumable packet reader for the external Protocol 5 test clients.
use legacy_protocol::{read_varint, BufferedFrameReader};
use std::io::{self, Read};
use std::net::TcpStream;
use std::ops::{Deref, DerefMut};

/// Keep one reader for the lifetime of a decoded TCP stream. Raw reads bypass
/// this framing state and must not be interleaved with `poll_packet` calls.
pub struct BufferedClient {
    stream: TcpStream,
    reader: BufferedFrameReader,
}

impl BufferedClient {
    pub fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            reader: BufferedFrameReader::new(),
        }
    }

    /// Read one complete frame or wait for the socket's existing timeout.
    /// Partial prefix/body bytes survive a timeout for the next invocation.
    pub fn poll_packet(&mut self) -> io::Result<Option<(u32, Vec<u8>)>> {
        loop {
            let mut attempt = ReadAttempt {
                stream: &mut self.stream,
                progressed: false,
            };
            match self.reader.poll(&mut attempt)? {
                Some(packet) => {
                    let mut body = packet.as_slice();
                    let id = read_varint(&mut body)?;
                    return Ok(Some((id, body.to_vec())));
                }
                // BufferedFrameReader also returns None after a successful
                // partial read. Keep draining those reads, matching the old
                // helper's complete-frame-or-timeout behavior without loss.
                None if attempt.progressed => {}
                None => return Ok(None),
            }
        }
    }
}

struct ReadAttempt<'a> {
    stream: &'a mut TcpStream,
    progressed: bool,
}

impl Read for ReadAttempt<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let result = self.stream.read(bytes);
        self.progressed = matches!(&result, Ok(count) if *count > 0);
        result
    }
}

impl Deref for BufferedClient {
    type Target = TcpStream;

    fn deref(&self) -> &Self::Target {
        &self.stream
    }
}

impl DerefMut for BufferedClient {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.stream
    }
}
