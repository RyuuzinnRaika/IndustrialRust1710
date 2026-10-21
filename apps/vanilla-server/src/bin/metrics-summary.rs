//! Summarize measured world-worker JSONL telemetry and enforce CI thresholds.
//! A summary is only as representative as the supplied real server workload.
use serde_json::{json, Value};
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

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
        if value["schema_version"] != 1 || value["target_tps"] != 20 {
            return Err(invalid("unsupported telemetry schema"));
        }
        let seconds = number("window_seconds")?;
        if seconds <= 0.0 {
            return Err(invalid("zero telemetry window"));
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
        self.peak_players = self.peak_players.max(integer("players")?);
        self.max_p95 = self.max_p95.max(number("mspt_p95")?);
        self.max_p99 = self.max_p99.max(number("mspt_p99")?);
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
        json!({
            "windows": self.windows,
            "seconds": self.seconds,
            "processed_ticks": self.processed,
            "missed_ticks": self.missed,
            "tps_processed": self.tps(),
            "tick_delivery_ratio": self.tick_ratio(),
            "mspt_p95_worst_window": self.max_p95,
            "mspt_p99_worst_window": self.max_p99,
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
        return Err(invalid("usage: metrics-summary FILE [--min-tps N] [--min-tick-ratio N] [--max-p99-mspt N] [--max-rss-mib N]"));
    };
    let summary = read_metrics(Path::new(path))?;
    let mut min_tps = 0.0;
    let mut min_ratio = 0.0;
    let mut max_p99 = f64::INFINITY;
    let mut max_rss = f64::INFINITY;
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
            _ => return Err(invalid("unknown threshold flag")),
        }
        index += 2;
    }
    if min_ratio > 1.0 {
        return Err(invalid("tick delivery ratio must be between 0 and 1"));
    }
    println!("{}", summary.report());
    if summary.tps() < min_tps
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
    }
}
