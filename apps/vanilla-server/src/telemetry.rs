//! Real world-worker timing (not an estimate from the wall clock's tick counter).
//! Emits JSON lines so CI and operators can compare server runs.
use serde_json::json;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const REPORT_INTERVAL: Duration = Duration::from_secs(10);
const MAX_SAMPLES: usize = 4096;

pub struct TickTelemetry {
    path: PathBuf,
    started: Instant,
    window_start_unix_ms: u64,
    samples_ms: Vec<f64>,
    processed_ticks: u64,
    missed_ticks: u64,
    players_min: Option<usize>,
    players_peak: usize,
    player_ticks: u128,
}

impl TickTelemetry {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            started: Instant::now(),
            window_start_unix_ms: unix_time_ms(),
            samples_ms: Vec::new(),
            processed_ticks: 0,
            missed_ticks: 0,
            players_min: None,
            players_peak: 0,
            player_ticks: 0,
        }
    }

    /// Record an actual batch of world ticks. Clock ticks skipped during a stall
    /// are *not* counted as processed TPS.
    pub fn record(&mut self, elapsed: Duration, ticks: u32, missed: u64, players: usize) {
        self.processed_ticks = self.processed_ticks.saturating_add(u64::from(ticks));
        self.missed_ticks = self.missed_ticks.saturating_add(missed);
        // Attribute connected-player levels to processed world ticks rather
        // than treating only the final instant as the whole ten-second window.
        if ticks > 0 {
            self.players_min = Some(self.players_min.map_or(players, |min| min.min(players)));
            self.players_peak = self.players_peak.max(players);
            self.player_ticks = self
                .player_ticks
                .saturating_add((players as u128) * u128::from(ticks));
        }
        if ticks > 0 {
            let per_tick = elapsed.as_secs_f64() * 1000.0 / f64::from(ticks);
            // The sample bound applies even if the process runs for days.
            if self.samples_ms.len() < MAX_SAMPLES {
                self.samples_ms.extend(
                    std::iter::repeat_n(per_tick, ticks as usize)
                        .take(MAX_SAMPLES - self.samples_ms.len()),
                );
            }
        }
        if self.started.elapsed() >= REPORT_INTERVAL {
            // Keep this window in memory when the filesystem is unavailable.
            // The next record() or explicit shutdown will retry the write.
            if let Err(error) = self.finish(players) {
                eprintln!("METRICS WRITE ERROR: {error}");
            }
        }
    }

    /// Persist the trailing (possibly shorter than ten seconds) window on
    /// orderly shutdown. Idempotent after a successful write; a failed write
    /// preserves the window so a later attempt can recover the measurements.
    pub fn finish(&mut self, players: usize) -> io::Result<()> {
        if self.processed_ticks == 0 && self.missed_ticks == 0 {
            return Ok(());
        }
        self.flush(players)?;
        self.started = Instant::now();
        self.window_start_unix_ms = unix_time_ms();
        self.processed_ticks = 0;
        self.missed_ticks = 0;
        self.players_min = None;
        self.players_peak = 0;
        self.player_ticks = 0;
        self.samples_ms.clear();
        Ok(())
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
            "schema_version": 2,
            "process_id": std::process::id(),
            "window_start_unix_ms": self.window_start_unix_ms,
            "window_end_unix_ms": unix_time_ms(),
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
            "players_min": self.players_min,
            "players_peak": self.players_min.map(|_| self.players_peak),
            "players_mean": (self.processed_ticks > 0).then(|| self.player_ticks as f64 / self.processed_ticks as f64),
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

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
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
    fn reports_player_weighted_load_and_tick_quality_without_time_sleep() {
        let path = std::env::temp_dir().join(format!(
            "ir-e48-telemetry-{}-{:?}.jsonl",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut samples = TickTelemetry::new(path.clone());
        for (players, ticks) in [(1, 2u32), (2, 2), (4, 2), (8, 2)] {
            samples.record(Duration::from_millis(10), ticks, 0, players);
        }
        assert_eq!(samples.processed_ticks, 8);
        // Force only the reporting timer without waiting ten seconds in CI.
        samples.started = Instant::now() - Duration::from_secs(11);
        samples.record(Duration::from_millis(10), 2, 1, 8);
        let line = std::fs::read_to_string(&path).unwrap();
        let report: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(report["schema_version"], 2);
        assert_eq!(report["process_id"], std::process::id());
        assert!(report["window_start_unix_ms"].as_u64().is_some());
        assert!(report["window_end_unix_ms"].as_u64().is_some());
        assert_eq!(report["processed_ticks"], 10);
        assert_eq!(report["missed_ticks"], 1);
        assert_eq!(report["mspt_samples"], 10);
        assert_eq!(report["players"], 8);
        assert_eq!(report["players_min"], 1);
        assert_eq!(report["players_peak"], 8);
        // (1*2 + 2*2 + 4*2 + 8*2 + 8*2) / 10 == 4.6
        assert_eq!(report["players_mean"].as_f64().unwrap(), 4.6);
        assert_eq!(samples.players_min, None);
        assert_eq!(samples.player_ticks, 0);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn real_work_samples_are_bounded() {
        let mut t = TickTelemetry::new(PathBuf::from("never-written.jsonl"));
        t.record(Duration::from_millis(200), 5000, 12, 2);
        assert_eq!(t.samples_ms.len(), MAX_SAMPLES);
        assert_eq!(t.processed_ticks, 5000);
        assert_eq!(t.missed_ticks, 12);
    }
    #[test]
    fn finish_persists_short_shutdown_window_exactly_once() {
        let path = std::env::temp_dir().join(format!(
            "ir-e48-final-{}-{:?}.jsonl",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut telemetry = TickTelemetry::new(path.clone());
        // A run shorter than the normal report interval must still be visible.
        telemetry.record(Duration::from_millis(12), 2, 0, 1);
        assert!(!path.exists());
        telemetry.finish(1).unwrap();
        telemetry.finish(1).unwrap();
        let rows = std::fs::read_to_string(&path).unwrap();
        assert_eq!(rows.lines().count(), 1);
        let row: serde_json::Value = serde_json::from_str(&rows).unwrap();
        assert_eq!(row["processed_ticks"], 2);
        assert_eq!(row["players_peak"], 1);
        assert_eq!(row["mspt_samples"], 2);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn failed_flush_keeps_window_for_a_retry() {
        let path = std::env::temp_dir().join(format!(
            "ir-e48-retry-{}-{:?}.jsonl",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&path);
        // A directory where a file is expected reliably causes a write error.
        std::fs::create_dir(&path).unwrap();
        let mut telemetry = TickTelemetry::new(path.clone());
        telemetry.record(Duration::from_millis(30), 3, 1, 2);
        assert!(telemetry.finish(2).is_err());
        assert_eq!(telemetry.processed_ticks, 3);
        assert_eq!(telemetry.missed_ticks, 1);
        assert_eq!(telemetry.samples_ms.len(), 3);
        std::fs::remove_dir(&path).unwrap();
        telemetry.finish(2).unwrap();
        let row: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(row["processed_ticks"], 3);
        assert_eq!(row["missed_ticks"], 1);
        assert_eq!(row["mspt_samples"], 3);
        assert_eq!(telemetry.processed_ticks, 0);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn finish_without_observed_ticks_does_not_create_a_row() {
        let path = std::env::temp_dir().join(format!(
            "ir-e48-empty-{}-{:?}.jsonl",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);
        TickTelemetry::new(path.clone()).finish(0).unwrap();
        assert!(!path.exists());
    }
}
