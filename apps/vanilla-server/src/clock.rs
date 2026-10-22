//! Fixed-rate 20Hz monotonic tick clock. This is the scheduler foundation;
//! not yet the full vanilla entity/block simulation.
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_millis(50);
const SNAPSHOT_MAGIC_V1: &[u8; 4] = b"IRT1";
const SNAPSHOT_MAGIC_V2: &[u8; 4] = b"IRT2";

/// Clock metadata is deliberately separate from the IRW save and compatible
/// with existing 20-byte IRT1 tick snapshots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockSnapshot {
    ticks: u64,
    time_offset: u64,
    weather_override: u8,
}

#[derive(Default)]
pub struct Clock {
    ticks: AtomicU64,
    running: AtomicBool,
    time_offset: AtomicU64,
    weather_override: AtomicU8, // 0=automatic, 1=clear, 2=rain, 3=thunder
}
impl Clock {
    /// Start the monotonic server ticker from an already validated snapshot.
    /// Server downtime deliberately does not advance in-game time.
    #[cfg(test)]
    pub fn start_at(initial_ticks: u64) -> (Arc<Self>, JoinHandle<()>) {
        Self::start_from(ClockSnapshot {
            ticks: initial_ticks,
            time_offset: 0,
            weather_override: 0,
        })
    }

    pub fn start_from(snapshot: ClockSnapshot) -> (Arc<Self>, JoinHandle<()>) {
        let clock = Arc::new(Self {
            ticks: AtomicU64::new(snapshot.ticks),
            running: AtomicBool::new(true),
            time_offset: AtomicU64::new(snapshot.time_offset),
            weather_override: AtomicU8::new(snapshot.weather_override),
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
        let offset = self.time_offset.load(Ordering::Acquire) % 24_000;
        let weather = self.weather_override.load(Ordering::Acquire);
        file.write_all(SNAPSHOT_MAGIC_V2)?;
        file.write_all(&ticks.to_be_bytes())?;
        file.write_all(&(!ticks).to_be_bytes())?;
        file.write_all(&offset.to_be_bytes())?;
        file.write_all(&(!offset).to_be_bytes())?;
        file.write_all(&[weather])?;
        file.sync_all()?;
        drop(file);
        fs::rename(temp, path)?;
        #[cfg(unix)]
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn load_ticks(path: &Path) -> io::Result<u64> {
        Ok(Self::load_state(path)?.ticks)
    }

    pub fn load_state(path: &Path) -> io::Result<ClockSnapshot> {
        let empty = ClockSnapshot {
            ticks: 0,
            time_offset: 0,
            weather_override: 0,
        };
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(empty),
            Err(e) => return Err(e),
        };
        if bytes.len() != 20 && bytes.len() != 37 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bad clock length",
            ));
        }
        let v1 = bytes.len() == 20 && &bytes[..4] == SNAPSHOT_MAGIC_V1;
        let v2 = bytes.len() == 37 && &bytes[..4] == SNAPSHOT_MAGIC_V2;
        if !v1 && !v2 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bad clock header",
            ));
        }
        let ticks = u64::from_be_bytes(bytes[4..12].try_into().unwrap());
        let inverse = u64::from_be_bytes(bytes[12..20].try_into().unwrap());
        if inverse != !ticks {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "corrupt clock ticks",
            ));
        }
        if v1 {
            return Ok(ClockSnapshot { ticks, ..empty });
        }
        let time_offset = u64::from_be_bytes(bytes[20..28].try_into().unwrap());
        let inverse = u64::from_be_bytes(bytes[28..36].try_into().unwrap());
        let weather_override = bytes[36];
        if inverse != !time_offset || time_offset >= 24_000 || weather_override > 3 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "corrupt clock state",
            ));
        }
        Ok(ClockSnapshot {
            ticks,
            time_offset,
            weather_override,
        })
    }

    pub fn ticks(&self) -> u64 {
        self.ticks.load(Ordering::Acquire)
    }
    pub fn day_time(&self) -> i64 {
        ((6000 + self.ticks() % 24_000 + self.time_offset.load(Ordering::Acquire) % 24_000)
            % 24_000) as i64
    }
    /// Set 1.7.10 daylight time. World age is unaffected.
    pub fn set_day_time(&self, requested: u64) -> io::Result<()> {
        if requested >= 24_000 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "daytime must be 0..23999",
            ));
        }
        let base = (6000 + self.ticks() % 24_000) % 24_000;
        self.time_offset
            .store((requested + 24_000 - base) % 24_000, Ordering::Release);
        Ok(())
    }
    pub fn set_weather(&self, requested: &str) -> io::Result<()> {
        let value = match requested {
            "auto" => 0,
            "clear" => 1,
            "rain" => 2,
            "thunder" => 3,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "weather must be auto|clear|rain|thunder",
                ))
            }
        };
        self.weather_override.store(value, Ordering::Release);
        Ok(())
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
        match self.weather_override.load(Ordering::Acquire) {
            1 => (false, false),
            2 => (true, false),
            3 => (true, true),
            _ => {
                let day = self.ticks() / 24_000;
                (matches!(day % 7, 2 | 3), day % 7 == 3)
            }
        }
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
            weather_override: AtomicU8::new(0),
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
    fn legacy_snapshot_migrates_and_console_settings_survive_restart() {
        let path = std::env::temp_dir().join(format!(
            "ir1710-clock-v2-{}-{:?}.bin",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_file(&path);
        let mut legacy = b"IRT1".to_vec();
        legacy.extend_from_slice(&1234u64.to_be_bytes());
        legacy.extend_from_slice(&(!1234u64).to_be_bytes());
        fs::write(&path, &legacy).unwrap();
        assert_eq!(Clock::load_state(&path).unwrap().ticks, 1234);
        let (clock, runner) = Clock::start_from(Clock::load_state(&path).unwrap());
        clock.set_day_time(18_000).unwrap();
        clock.set_weather("thunder").unwrap();
        assert_eq!(clock.day_time(), 18_000);
        clock.save_ticks(&path).unwrap();
        clock.stop();
        runner.join().unwrap();
        let snapshot = Clock::load_state(&path).unwrap();
        let (again, handle) = Clock::start_from(snapshot);
        assert_eq!(
            again.day_time(),
            (18_000 + (again.ticks() - snapshot.ticks) % 24_000) as i64 % 24_000
        );
        assert_eq!(again.weather(), (true, true));
        assert!(again.set_day_time(24_000).is_err());
        assert!(again.set_weather("storm").is_err());
        again.stop();
        handle.join().unwrap();
        let mut bytes = fs::read(&path).unwrap();
        bytes[25] ^= 1;
        fs::write(&path, bytes).unwrap();
        assert_eq!(
            Clock::load_state(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        fs::remove_file(&path).unwrap();
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
