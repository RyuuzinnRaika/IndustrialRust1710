//! Fixed-rate 20Hz monotonic tick clock. This is the scheduler foundation;
//! not yet the full vanilla entity/block simulation.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_millis(50);

#[derive(Default)]
pub struct Clock {
    ticks: AtomicU64,
    running: AtomicBool,
    time_offset: AtomicU64,
}
impl Clock {
    pub fn start() -> (Arc<Self>, JoinHandle<()>) {
        let clock = Arc::new(Self {
            ticks: AtomicU64::new(0),
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
    #[cfg(test)]
    pub fn stop(&self) {
        self.running.store(false, Ordering::Release);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clock_advances_and_stops() {
        let (clock, handle) = Clock::start();
        thread::sleep(Duration::from_millis(125));
        let count = clock.ticks();
        assert!((1..=6).contains(&count), "ticks={count}");
        assert_eq!(clock.day_time(), 6000 + count as i64);
        clock.stop();
        handle.join().unwrap();
    }
}
