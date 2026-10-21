//! Real world-worker timing (not an estimate from the wall clock's tick counter).
//! Emits JSON lines so CI and operators can compare server runs.
use serde_json::json;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const REPORT_INTERVAL: Duration = Duration::from_secs(10);
const MAX_SAMPLES: usize = 4096;

pub struct TickTelemetry {
    path: PathBuf,
    started: Instant,
    samples_ms: Vec<f64>,
    processed_ticks: u64,
    missed_ticks: u64,
}

impl TickTelemetry {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            started: Instant::now(),
            samples_ms: Vec::new(),
            processed_ticks: 0,
            missed_ticks: 0,
        }
    }

    /// Record an actual batch of world ticks. Clock ticks skipped during a stall
    /// are *not* counted as processed TPS.
    pub fn record(&mut self, elapsed: Duration, ticks: u32, missed: u64, players: usize) {
        self.processed_ticks += u64::from(ticks);
        self.missed_ticks += missed;
        if ticks > 0 {
            let per_tick = elapsed.as_secs_f64() * 1000.0 / f64::from(ticks);
            // The sample bound applies even if the process runs for days.
            if self.samples_ms.len() < MAX_SAMPLES {
                self.samples_ms.extend(
                    std::iter::repeat(per_tick)
                        .take(ticks as usize)
                        .take(MAX_SAMPLES - self.samples_ms.len()),
                );
            }
        }
        if self.started.elapsed() >= REPORT_INTERVAL {
            if let Err(error) = self.flush(players) {
                eprintln!("METRICS WRITE ERROR: {error}");
            }
            self.started = Instant::now();
            self.processed_ticks = 0;
            self.missed_ticks = 0;
            self.samples_ms.clear();
        }
    }

    fn flush(&self, players: usize) -> io::Result<()> {
        let elapsed = self.started.elapsed().as_secs_f64();
        let samples = &self.samples_ms;
        let mean = if samples.is_empty() {
            0.0
        } else {
            samples.iter().sum::<f64>() / samples.len() as f64
        };
        let mut sorted = samples.clone();
        sorted.sort_by(f64::total_cmp);
        let measured_tps = self.processed_ticks as f64 / elapsed.max(f64::EPSILON);
        let row = json!({
            "schema_version": 1,
            "window_seconds": elapsed,
            "processed_ticks": self.processed_ticks,
            "missed_ticks": self.missed_ticks,
            "tps_processed": measured_tps,
            "target_tps": 20,
            "mspt_mean": mean,
            "mspt_p95": percentile(&sorted, 0.95),
            "mspt_p99": percentile(&sorted, 0.99),
            "mspt_samples": samples.len(),
            "players": players,
            "process_rss_bytes": resident_bytes()
        });
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(file, "{row}")
    }
}

fn percentile(sorted: &[f64], probability: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() as f64 * probability).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    sorted[index]
}

#[cfg(target_os = "linux")]
fn resident_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|line| {
        let value = line.strip_prefix("VmRSS:")?;
        value
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()?
            .checked_mul(1024)
    })
}

#[cfg(not(target_os = "linux"))]
fn resident_bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_percentiles_handle_empty_and_outliers() {
        assert_eq!(percentile(&[], 0.99), 0.0);
        let samples = [1.0, 2.0, 3.0, 4.0, 100.0];
        assert_eq!(percentile(&samples, 0.50), 3.0);
        assert_eq!(percentile(&samples, 0.95), 100.0);
        assert_eq!(percentile(&samples, 0.99), 100.0);
    }

    #[test]
    fn real_work_samples_are_bounded() {
        let mut t = TickTelemetry::new(PathBuf::from("never-written.jsonl"));
        t.record(Duration::from_millis(200), 5000, 12, 2);
        assert_eq!(t.samples_ms.len(), MAX_SAMPLES);
        assert_eq!(t.processed_ticks, 5000);
        assert_eq!(t.missed_ticks, 12);
    }
}
