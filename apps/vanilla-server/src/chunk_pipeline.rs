//! Bounded, two-worker 1.7.10 chunk compression pipeline.
//! Raw world snapshots are produced under the world lock by the caller;
//! CPU-intensive zlib compression never holds that lock.
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::io::{self, Write};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

const QUEUE_CAPACITY: usize = 32;
const WORKERS: usize = 2;
const MAX_RAW_CHUNK_BYTES: usize = 16 * 10_240 + 256;
static WORK: OnceLock<SyncSender<Job>> = OnceLock::new();

struct Job {
    x: i32,
    z: i32,
    mask: u16,
    raw: Vec<u8>,
    reply: SyncSender<io::Result<Vec<u8>>>,
}

/// One bounded outstanding request. Cancelling a session drops the receiver;
/// its worker finishes safely without keeping the session alive.
pub struct PendingChunk(Receiver<io::Result<Vec<u8>>>);

impl PendingChunk {
    pub fn finish(self) -> io::Result<Vec<u8>> {
        self.0.recv().map_err(|_| io::Error::other("chunk encoder stopped"))?
    }
}

fn encode(job: &Job) -> io::Result<Vec<u8>> {
    let mut compressor = ZlibEncoder::new(Vec::new(), Compression::fast());
    compressor.write_all(&job.raw)?;
    let compressed = compressor.finish()?;
    // Protocol 5 S21: X, Z, full column, primary/add bitmap, byte count, zlib.
    let mut packet = Vec::with_capacity(17 + compressed.len());
    packet.extend_from_slice(&job.x.to_be_bytes());
    packet.extend_from_slice(&job.z.to_be_bytes());
    packet.push(1);
    packet.extend_from_slice(&job.mask.to_be_bytes());
    packet.extend_from_slice(&0u16.to_be_bytes());
    let len = i32::try_from(compressed.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "chunk too large"))?;
    packet.extend_from_slice(&len.to_be_bytes());
    packet.extend_from_slice(&compressed);
    Ok(packet)
}

fn sender() -> &'static SyncSender<Job> {
    WORK.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<Job>(QUEUE_CAPACITY);
        let shared = Arc::new(Mutex::new(rx));
        for index in 0..WORKERS {
            let queue = Arc::clone(&shared);
            thread::Builder::new()
                .name(format!("ir-chunk-encoder-{index}"))
                .spawn(move || loop {
                    let item = match queue.lock() {
                        Ok(receiver) => receiver.recv(),
                        Err(_) => break,
                    };
                    let Ok(job) = item else { break };
                    let encoded = encode(&job);
                    let _ = job.reply.send(encoded);
                })
                .expect("cannot launch chunk encoder");
        }
        tx
    })
}

pub fn submit(x: i32, z: i32, mask: u16, raw: Vec<u8>) -> io::Result<PendingChunk> {
    if raw.len() > MAX_RAW_CHUNK_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "chunk snapshot exceeds raw memory bound",
        ));
    }
    let (tx, rx) = mpsc::sync_channel(1);
    // Bounded backpressure: never allocate an unbounded work backlog.
    sender()
        .send(Job {
            x,
            z,
            mask,
            raw,
            reply: tx,
        })
        .map_err(|_| io::Error::other("chunk compression queue closed"))?;
    Ok(PendingChunk(rx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::ZlibDecoder;
    use std::io::Read;

    #[test]
    fn golden_protocol5_chunk_header_and_zlib_roundtrip() {
        let raw = vec![3u8; 20_736];
        let packet = submit(-1, 2, 0x18, raw.clone()).unwrap().finish().unwrap();
        assert_eq!(&packet[..4], &(-1i32).to_be_bytes());
        assert_eq!(&packet[4..8], &2i32.to_be_bytes());
        assert_eq!(&packet[8..13], &[1, 0, 24, 0, 0]);
        let size = i32::from_be_bytes(packet[13..17].try_into().unwrap()) as usize;
        assert_eq!(size, packet.len() - 17);
        let mut decoded = Vec::new();
        ZlibDecoder::new(&packet[17..])
            .read_to_end(&mut decoded)
            .unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn rejects_unbounded_raw_chunk_jobs() {
        assert!(submit(0, 0, 0, vec![0; MAX_RAW_CHUNK_BYTES + 1]).is_err());
    }
}
