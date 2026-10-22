//! Summarize measured world-worker JSONL telemetry and enforce CI thresholds.
//! A summary is only as representative as the supplied real server workload.
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

#[derive(Debug, Default)]
struct Load {
    windows: usize,
    seconds: f64,
    processed: u64,
    missed: u64,
    worst_p99: f64,
}

#[derive(Debug, Default)]
struct Summary {
    windows: usize,
    seconds: f64,
    processed: u64,
    missed: u64,
    peak_players: u64,
    max_p95: f64,
    max_p99: f64,
    peak_rss: u64,
    weighted_mspt_total: f64,
    mspt_samples: u64,
    by_players: BTreeMap<u64, Load>,
}

impl Summary {
    fn add(&mut self, value: &Value) -> io::Result<()> {
        let number = |key: &str| -> io::Result<f64> {
            let n = value[key]
                .as_f64()
                .ok_or_else(|| invalid("missing numeric metric"))?;
            if n.is_finite() && n >= 0.0 {
                Ok(n)
            } else {
                Err(invalid("nonfinite or negative metric"))
            }
        };
        let integer = |key: &str| -> io::Result<u64> {
            value[key]
                .as_u64()
                .ok_or_else(|| invalid("missing integer metric"))
        };
        if !matches!(value["schema_version"].as_u64(), Some(1 | 2)) || value["target_tps"] != 20 {
            return Err(invalid("unsupported telemetry schema"));
        }
        let seconds = number("window_seconds")?;
        if seconds <= 0.0 {
            return Err(invalid("zero telemetry window"));
        }
        let players = integer("players")?;
        // v2 reports the maximum concurrently connected peers in each window.
        // Using only the final instantaneous count underreports transient load.
        let load_players = value["players_peak"].as_u64().unwrap_or(players);
        // The server currently admits at most eight peers. Reject impossible
        // metrics rather than silently accepting an unrelated JSONL schema.
        if players > 8 || load_players > 8 {
            return Err(invalid("impossible player count"));
        }
        let p95 = number("mspt_p95")?;
        let p99 = number("mspt_p99")?;
        if p95 > p99 {
            return Err(invalid("p95 greater than p99"));
        }
        // Legacy JSONL rows may omit samples, so keep that field optional.
        // A new row with the actual samples must supply its matching mean.
        if value["schema_version"] == 2 && !value["mspt_samples"].is_u64() {
            return Err(invalid("v2 telemetry requires mspt_samples"));
        }
        if let Some(samples) = value["mspt_samples"].as_u64() {
            if samples > 4096 {
                return Err(invalid("mspt sample count exceeds producer bound"));
            }
            let mean = number("mspt_mean")?;
            self.weighted_mspt_total += mean * samples as f64;
            self.mspt_samples = self
                .mspt_samples
                .checked_add(samples)
                .ok_or_else(|| invalid("mspt sample count overflow"))?;
        }
        self.seconds += seconds;
        self.processed = self
            .processed
            .checked_add(integer("processed_ticks")?)
            .ok_or_else(|| invalid("processed tick counter overflow"))?;
        self.missed = self
            .missed
            .checked_add(integer("missed_ticks")?)
            .ok_or_else(|| invalid("missed tick counter overflow"))?;
        self.peak_players = self.peak_players.max(load_players);
        self.max_p95 = self.max_p95.max(p95);
        self.max_p99 = self.max_p99.max(p99);
        let group = self.by_players.entry(load_players).or_default();
        group.windows += 1;
        group.seconds += seconds;
        group.processed = group
            .processed
            .checked_add(integer("processed_ticks")?)
            .ok_or_else(|| invalid("load tick counter overflow"))?;
        group.missed = group
            .missed
            .checked_add(integer("missed_ticks")?)
            .ok_or_else(|| invalid("load missed tick counter overflow"))?;
        group.worst_p99 = group.worst_p99.max(p99);
        if let Some(rss) = value["process_rss_bytes"].as_u64() {
            self.peak_rss = self.peak_rss.max(rss);
        }
        self.windows += 1;
        Ok(())
    }

    fn tps(&self) -> f64 {
        self.processed as f64 / self.seconds
    }

    fn tick_ratio(&self) -> f64 {
        let ticks = self.processed.saturating_add(self.missed);
        if ticks == 0 {
            0.0
        } else {
            self.processed as f64 / ticks as f64
        }
    }

    fn report(&self) -> Value {
        let by_player_count: Vec<_> = self
            .by_players
            .iter()
            .map(|(players, load)| {
                let total = load.processed.saturating_add(load.missed);
                json!({
                    "players": players,
                    "windows": load.windows,
                    "seconds": load.seconds,
                    "tps_processed": load.processed as f64 / load.seconds,
                    "tick_delivery_ratio": if total == 0 { 0.0 } else {
                        load.processed as f64 / total as f64
                    },
                    "mspt_p99_worst_window": load.worst_p99,
                })
            })
            .collect();
        json!({
            "windows": self.windows,
            "seconds": self.seconds,
            "processed_ticks": self.processed,
            "missed_ticks": self.missed,
            "tps_processed": self.tps(),
            "tick_delivery_ratio": self.tick_ratio(),
            "mspt_p95_worst_window": self.max_p95,
            "mspt_p99_worst_window": self.max_p99,
            "mspt_mean_weighted": if self.mspt_samples == 0 {
                None
            } else {
                Some(self.weighted_mspt_total / self.mspt_samples as f64)
            },
            "mspt_sample_count": self.mspt_samples,
            "by_player_count": by_player_count,
            "players_peak": self.peak_players,
            "rss_peak_bytes": self.peak_rss,
        })
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn read_metrics(path: &Path) -> io::Result<Summary> {
    let mut summary = Summary::default();
    for (line_no, line) in BufReader::new(File::open(path)?).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&line).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("line {}: {error}", line_no + 1),
            )
        })?;
        summary.add(&value)?;
    }
    if summary.windows == 0 {
        return Err(invalid("metrics file contains no samples"));
    }
    Ok(summary)
}

fn run(args: &[String]) -> io::Result<()> {
    let Some(path) = args.first() else {
        return Err(invalid("usage: metrics-summary FILE [--min-tps N] [--min-tick-ratio N] [--max-p99-mspt N] [--max-rss-mib N] [--min-seconds N] [--min-windows N]"));
    };
    let summary = read_metrics(Path::new(path))?;
    let mut min_tps = 0.0;
    let mut min_ratio = 0.0;
    let mut max_p99 = f64::INFINITY;
    let mut max_rss = f64::INFINITY;
    let mut min_seconds = 0.0;
    let mut min_windows = 0usize;
    let mut index = 1;
    while index < args.len() {
        let name = &args[index];
        let input = args
            .get(index + 1)
            .ok_or_else(|| invalid("missing threshold value"))?;
        let value = input
            .parse::<f64>()
            .map_err(|_| invalid("invalid threshold value"))?;
        if !value.is_finite() || value < 0.0 {
            return Err(invalid("invalid threshold range"));
        }
        match name.as_str() {
            "--min-tps" => min_tps = value,
            "--min-tick-ratio" => min_ratio = value,
            "--max-p99-mspt" => max_p99 = value,
            "--max-rss-mib" => max_rss = value,
            "--min-seconds" => min_seconds = value,
            "--min-windows" => {
                if value.fract() != 0.0 || value > u32::MAX as f64 {
                    return Err(invalid("invalid minimum window count"));
                }
                min_windows = value as usize;
            }
            _ => return Err(invalid("unknown threshold flag")),
        }
        index += 2;
    }
    if min_ratio > 1.0 {
        return Err(invalid("tick delivery ratio must be between 0 and 1"));
    }
    println!("{}", summary.report());
    if summary.windows < min_windows
        || summary.seconds < min_seconds
        || summary.tps() < min_tps
        || summary.tick_ratio() < min_ratio
        || summary.max_p99 > max_p99
        || summary.peak_rss as f64 > max_rss * 1024.0 * 1024.0
    {
        return Err(io::Error::other("telemetry regression threshold exceeded"));
    }
    Ok(())
}

fn main() -> io::Result<()> {
    run(&std::env::args().skip(1).collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculates_real_windows_and_rejects_invalid_records() {
        let mut metrics = Summary::default();
        for row in [
            json!({"schema_version":1,"target_tps":20,"window_seconds":10.0,"processed_ticks":200,
                "missed_ticks":0,"players":1,"mspt_p95":7.0,"mspt_p99":10.0,"process_rss_bytes":1024}),
            json!({"schema_version":1,"target_tps":20,"window_seconds":10.0,"processed_ticks":150,
                "missed_ticks":50,"players":8,"mspt_p95":32.0,"mspt_p99":47.0,"process_rss_bytes":4096}),
        ] {
            metrics.add(&row).unwrap();
        }
        assert_eq!(metrics.windows, 2);
        assert_eq!(metrics.tps(), 17.5);
        assert_eq!(metrics.tick_ratio(), 0.875);
        assert_eq!(metrics.max_p99, 47.0);
        assert_eq!(metrics.peak_players, 8);
        assert_eq!(metrics.peak_rss, 4096);
        assert!(metrics.add(&json!({"schema_version":2})).is_err());
        let report = metrics.report();
        assert_eq!(report["by_player_count"].as_array().unwrap().len(), 2);
        assert_eq!(report["by_player_count"][0]["players"], 1);
        assert_eq!(report["by_player_count"][1]["players"], 8);
    }

    #[test]
    fn accepts_current_v2_world_worker_schema_and_buckets_peak_load() {
        let mut summary = Summary::default();
        summary
            .add(&json!({
                "schema_version": 2, "target_tps": 20, "window_seconds": 10.0,
                "processed_ticks": 198, "missed_ticks": 2, "players": 1,
                "players_min": 1, "players_peak": 4, "players_mean": 2.5,
                "mspt_mean": 7.0, "mspt_p95": 10.0, "mspt_p99": 12.0,
                "mspt_samples": 198, "process_rss_bytes": 1_048_576
            }))
            .unwrap();
        assert_eq!(summary.peak_players, 4);
        assert_eq!(summary.tps(), 19.8);
        assert_eq!(summary.report()["by_player_count"][0]["players"], 4);
        assert_eq!(summary.report()["mspt_mean_weighted"], 7.0);
        assert!(summary
            .add(&json!({
                "schema_version": 2, "target_tps": 20, "window_seconds": 10.0,
                "processed_ticks": 0, "missed_ticks": 0, "players": 0,
                "mspt_p95": 0.0, "mspt_p99": 0.0
            }))
            .is_err()); // samples required in v2
    }

    #[test]
    fn weights_mspt_by_actual_sample_count_and_guards_bad_telemetry() {
        let mut summary = Summary::default();
        let sample = |players, mean, count, p95, p99| {
            json!({
                "schema_version":1,"target_tps":20,
                "window_seconds":10.0,"processed_ticks":200,"missed_ticks":0,
                "players":players,"mspt_p95":p95,"mspt_p99":p99,
                "mspt_mean":mean,"mspt_samples":count
            })
        };
        summary.add(&sample(1, 5.0, 10, 10.0, 12.0)).unwrap();
        summary.add(&sample(2, 15.0, 30, 17.0, 20.0)).unwrap();
        assert_eq!(summary.mspt_samples, 40);
        assert_eq!(summary.report()["mspt_mean_weighted"], 12.5);
        assert!(summary.add(&sample(1, 1.0, 5, 25.0, 10.0)).is_err());
        assert!(summary.add(&sample(99, 1.0, 5, 2.0, 3.0)).is_err());
        assert!(summary.add(&sample(1, 1.0, 5000, 2.0, 3.0)).is_err());
    }
}
