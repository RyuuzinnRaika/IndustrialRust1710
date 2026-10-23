//! Bounded, two-worker 1.7.10 chunk compression pipeline.
//! Raw world snapshots are produced under the world lock by the caller;
//! CPU-intensive zlib compression never holds that lock.
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::io::{self, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

const QUEUE_CAPACITY: usize = 32;
/// Per-session bounded batch to prevent one login from filling the shared queue.
pub const SESSION_BATCH_LIMIT: usize = 8;
const SUBMIT_TIMEOUT: Duration = Duration::from_millis(100);
/// Cap wall-clock time spent on a batch of compressed chunks for one client.
/// The session is disconnected on expiry, instead of hanging on a dead worker.
pub const BATCH_DEADLINE: Duration = Duration::from_secs(15);
// Chunk zlib jobs run outside the World lock. Bound parallel compression to
// avoid starving the 20Hz world thread or inflating in-flight memory on large hosts.
const MAX_WORKERS: usize = 4;
fn worker_count(available: usize) -> usize {
    available.clamp(1, MAX_WORKERS)
}
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

    /// Wait only until the caller's absolute deadline. All chunks in a
    /// session batch share the same deadline, so a stalled worker cannot
    /// multiply the timeout by the number of queued chunks.
    pub fn finish_before(self, deadline: Instant) -> io::Result<Vec<u8>> {
        if let Some(packet) = self.poll()? {
            return Ok(packet);
        }
        match self
            .0
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "chunk compression deadline exceeded",
            )),
            Err(RecvTimeoutError::Disconnected) => Err(io::Error::other("chunk encoder stopped")),
        }
    }

    pub fn finish(self) -> io::Result<Vec<u8>> {
        self.finish_before(Instant::now() + BATCH_DEADLINE)
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
        let workers = worker_count(
            thread::available_parallelism().map_or(2, |parallel| parallel.get()),
        );
        for index in 0..workers {
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

fn enqueue(tx: &SyncSender<Job>, mut job: Job, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        match tx.try_send(job) {
            Ok(()) => return Ok(()),
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
    let job = Job {
        x,
        z,
        mask,
        raw,
        reply: tx,
    };
    enqueue(sender(), job, SUBMIT_TIMEOUT)?;
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
    fn stalled_encoder_cannot_block_session_past_deadline() {
        // Retain the reply sender without sending: a crashed or wedged worker
        // must never leave the client thread blocked forever.
        let (reply, receiver) = mpsc::sync_channel(1);
        let started = Instant::now();
        let error = PendingChunk(receiver)
            .finish_before(started + Duration::from_millis(15))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(reply);
    }

    #[test]
    fn stopped_encoder_disconnects_without_waiting_for_deadline() {
        let (reply, receiver) = mpsc::sync_channel(1);
        drop(reply);
        let started = Instant::now();
        let error = PendingChunk(receiver)
            .finish_before(started + Duration::from_secs(10))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn full_queue_times_out_without_discarding_existing_work() {
        // A local queue without workers makes backpressure deterministic,
        // independent of CI runner CPU load and scheduling.
        let (tx, rx) = mpsc::sync_channel(1);
        let make_job = |x| {
            let (response, _reply) = mpsc::sync_channel(1);
            Job {
                x,
                z: 0,
                mask: 0,
                raw: vec![0; 256],
                reply: response,
            }
        };
        enqueue(&tx, make_job(1), Duration::from_millis(10)).unwrap();
        let start = Instant::now();
        let error = enqueue(&tx, make_job(2), Duration::from_millis(10)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert!(start.elapsed() < Duration::from_secs(1));
        assert_eq!(rx.try_recv().unwrap().x, 1); // No eviction/reordering.
        enqueue(&tx, make_job(3), Duration::ZERO).unwrap();
        assert_eq!(rx.try_recv().unwrap().x, 3);
    }

    #[test]
    fn adaptive_workers_remain_bounded_by_cpu_and_memory_budget() {
        assert_eq!(worker_count(0), 1);
        assert_eq!(worker_count(1), 1);
        assert_eq!(worker_count(2), 2);
        assert_eq!(worker_count(4), 4);
        assert_eq!(worker_count(128), MAX_WORKERS);
    }

    #[test]
    fn rejects_unbounded_raw_chunk_jobs() {
        assert!(submit(0, 0, 0, vec![0; MAX_RAW_CHUNK_BYTES + 1]).is_err());
    }
}
