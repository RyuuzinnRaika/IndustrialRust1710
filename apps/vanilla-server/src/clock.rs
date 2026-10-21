//! Fixed-rate 20Hz monotonic tick clock. This is the scheduler foundation;
//! not yet the full vanilla entity/block simulation.
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_millis(50);
const SNAPSHOT_MAGIC: &[u8; 4] = b"IRT1";

#[derive(Default)]
pub struct Clock {
    ticks: AtomicU64,
    running: AtomicBool,
    time_offset: AtomicU64,
}
impl Clock {
    /// Start the monotonic server ticker from an already validated snapshot.
    /// Server downtime deliberately does not advance in-game time.
    pub fn start_at(initial_ticks: u64) -> (Arc<Self>, JoinHandle<()>) {
        let clock = Arc::new(Self {
            ticks: AtomicU64::new(initial_ticks),
            running: AtomicBool::new(true),
            time_offset: AtomicU64::new(0),
        });
        let worker = Arc::clone(&clock);
        let handle = thread::spawn(move || {
            let mut next = Instant::now() + TICK;
            while worker.running.load(Ordering::Acquire) {
                let now = Instant::now();
                if now < next {
                    thread::sleep(next - now);
                } else {
                    // Avoid unbounded "catch-up" work after a stall.
                    next = now;
                }
                if !worker.running.load(Ordering::Acquire) {
                    break;
                }
                worker.ticks.fetch_add(1, Ordering::AcqRel);
                next += TICK;
            }
        });
        (clock, handle)
    }
    /// Persist exact server ticks, including the day and deterministic weather
    /// phase, without changing any existing IRW1/2/3 world data.
    /// Tick complement detects truncation or elementary file corruption.
    pub fn save_ticks(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        // This suffix differs from the IRW world's own .tmp save path.
        let temp = path.with_extension("clock.tmp");
        let mut file = File::create(&temp)?;
        let ticks = self.ticks();
        file.write_all(SNAPSHOT_MAGIC)?;
        file.write_all(&ticks.to_be_bytes())?;
        file.write_all(&(!ticks).to_be_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(temp, path)
    }

    pub fn load_ticks(path: &Path) -> io::Result<u64> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e),
        };
        if bytes.len() != 20 || &bytes[..4] != SNAPSHOT_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bad clock header",
            ));
        }
        let ticks = u64::from_be_bytes(bytes[4..12].try_into().unwrap());
        let inverted = u64::from_be_bytes(bytes[12..20].try_into().unwrap());
        if inverted != !ticks {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "corrupt clock ticks",
            ));
        }
        Ok(ticks)
    }

    pub fn ticks(&self) -> u64 {
        self.ticks.load(Ordering::Acquire)
    }
    pub fn day_time(&self) -> i64 {
        ((6000 + (self.ticks() + self.time_offset.load(Ordering::Acquire)) % 24000) % 24000) as i64
    }
    /// Move the shared daylight clock to morning after a completed sleep.
    pub fn skip_to_morning(&self) {
        let day = self.day_time() as u64;
        self.time_offset
            .fetch_add((24000 - day) % 24000, Ordering::AcqRel);
    }
    /// Deterministic 1.7.10-style weather episodes on the 20TPS world clock.
    /// No real-time catch-up takes place during a server shutdown.
    pub fn weather(&self) -> (bool, bool) {
        let day = self.ticks() / 24_000;
        (matches!(day % 7, 2 | 3), day % 7 == 3)
    }
    pub fn weather_packets(&self) -> Vec<Vec<u8>> {
        let (rain, thunder) = self.weather();
        let mut packets = vec![vec![if rain { 1 } else { 2 }, 0, 0, 0, 0]];
        for (reason, amount) in [
            (7u8, if rain { 1.0f32 } else { 0.0 }),
            (8u8, if thunder { 1.0f32 } else { 0.0 }),
        ] {
            let mut body = vec![reason];
            body.extend_from_slice(&amount.to_be_bytes());
            packets.push(body);
        }
        packets
    }
    #[cfg(test)]
    pub fn stop(&self) {
        self.running.store(false, Ordering::Release);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clock_roundtrip_preserves_day_and_thunder_phase_and_rejects_corruption() {
        let path =
            std::env::temp_dir().join(format!("ir1710-clock-snapshot-{}.bin", std::process::id()));
        let _ = fs::remove_file(&path);
        assert_eq!(Clock::load_ticks(&path).unwrap(), 0);
        let initial = 24_000 * 3 + 37;
        let clock = Clock {
            ticks: AtomicU64::new(initial),
            running: AtomicBool::new(false),
            time_offset: AtomicU64::new(0),
        };
        assert_eq!(clock.weather(), (true, true));
        clock.save_ticks(&path).unwrap();
        assert_eq!(Clock::load_ticks(&path).unwrap(), initial);
        let (restarted, worker) = Clock::start_at(Clock::load_ticks(&path).unwrap());
        assert!(restarted.ticks() >= initial);
        assert_eq!(restarted.weather(), (true, true));
        restarted.stop();
        worker.join().unwrap();
        let mut damaged = fs::read(&path).unwrap();
        damaged[8] ^= 1;
        fs::write(&path, damaged).unwrap();
        assert_eq!(
            Clock::load_ticks(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn clock_advances_and_stops() {
        let (clock, handle) = Clock::start_at(0);
        thread::sleep(Duration::from_millis(125));
        let count = clock.ticks();
        assert!((1..=6).contains(&count), "ticks={count}");
        assert_eq!(clock.day_time(), 6000 + count as i64);
        clock.stop();
        handle.join().unwrap();
    }
}
