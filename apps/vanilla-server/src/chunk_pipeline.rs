//! Bounded, two-worker 1.7.10 chunk compression pipeline.
//! Raw world snapshots are produced under the world lock by the caller;
//! CPU-intensive zlib compression never holds that lock.
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::io::{self, Write};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

const QUEUE_CAPACITY: usize = 32;
/// Per-session bounded batch to prevent one login from filling the shared queue.
pub const SESSION_BATCH_LIMIT: usize = 8;
const SUBMIT_TIMEOUT: Duration = Duration::from_millis(100);
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
    /// Poll completion without blocking a session or world tick thread.
    ///
    /// A caller may periodically poll while doing other work. After Ready,
    /// this handle is consumed and must not be polled or finished again.
    /// Unlike finish(), Pending never blocks on CPU-heavy zlib work.
    pub fn poll(&self) -> io::Result<Option<Vec<u8>>> {
        match self.0.try_recv() {
            Ok(result) => result.map(Some),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(io::Error::other("chunk encoder stopped")),
        }
    }

    pub fn finish(self) -> io::Result<Vec<u8>> {
        if let Some(packet) = self.poll()? {
            return Ok(packet);
        }
        self.0
            .recv()
            .map_err(|_| io::Error::other("chunk encoder stopped"))?
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
    // Never indefinitely block an inbound session on an overloaded compression
    // pipeline. The calling thread must NOT hold the world lock at this point.
    // Keep ownership of the Job across retries; no duplicate jobs are queued.
    let mut job = Job {
        x,
        z,
        mask,
        raw,
        reply: tx,
    };
    let deadline = Instant::now() + SUBMIT_TIMEOUT;
    loop {
        match sender().try_send(job) {
            Ok(()) => break,
            Err(TrySendError::Full(pending)) => {
                job = pending;
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "chunk compression queue busy",
                    ));
                }
                thread::sleep(Duration::from_millis(1));
            }
            Err(TrySendError::Disconnected(_)) => {
                return Err(io::Error::other("chunk compression queue closed"));
            }
        }
    }
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
    fn chunk_completion_poll_is_nonblocking_and_yields_valid_data() {
        use std::time::{Duration, Instant};
        let raw = vec![3u8; 20_736];
        let pending = submit(0, 0, 0x18, raw.clone()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        let packet = loop {
            if let Some(packet) = pending.poll().unwrap() {
                break packet;
            }
            assert!(Instant::now() < deadline, "chunk worker did not finish");
            std::thread::yield_now();
        };
        let size = i32::from_be_bytes(packet[13..17].try_into().unwrap()) as usize;
        assert_eq!(packet.len(), 17 + size);
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
