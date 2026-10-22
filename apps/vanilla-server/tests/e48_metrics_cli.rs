//! E48 CLI acceptance: validate a real telemetry v2-shaped JSONL row.
use std::fs;
use std::process::Command;

fn fixture(contents: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "ir1710-e48-metrics-{}-{:?}.jsonl",
        std::process::id(),
        std::thread::current().id()
    ));
    fs::write(&path, contents).unwrap();
    path
}

#[test]
fn summary_accepts_current_producer_schema_and_enforces_real_thresholds() {
    let path = fixture(concat!(
        "{\"schema_version\":2,\"window_seconds\":10.0,\"processed_ticks\":198,",
        "\"missed_ticks\":2,\"target_tps\":20,\"mspt_mean\":6.5,",
        "\"mspt_p95\":9.0,\"mspt_p99\":12.0,\"mspt_samples\":198,",
        "\"players\":1,\"players_min\":1,\"players_peak\":4,",
        "\"players_mean\":2.5,\"process_rss_bytes\":1048576}\n"
    ));
    let exe = env!("CARGO_BIN_EXE_metrics-summary");
    let success = Command::new(exe)
        .arg(&path)
        .args([
            "--min-tps",
            "19",
            "--min-tick-ratio",
            "0.98",
            "--max-p99-mspt",
            "20",
            "--min-seconds",
            "10",
            "--min-windows",
            "1",
        ])
        .output()
        .unwrap();
    assert!(
        success.status.success(),
        "{}",
        String::from_utf8_lossy(&success.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&success.stdout).unwrap();
    assert_eq!(report["players_peak"], 4);
    assert_eq!(report["by_player_count"][0]["players"], 4);
    assert_eq!(report["tps_processed"], 19.8);

    let regression = Command::new(exe)
        .arg(&path)
        .args(["--min-tps", "20"])
        .output()
        .unwrap();
    assert!(!regression.status.success());
    fs::remove_file(path).unwrap();
}

#[test]
fn summary_rejects_invalid_metrics_instead_of_reporting_success() {
    let path = fixture(concat!(
        "{\"schema_version\":2,\"window_seconds\":10.0,",
        "\"processed_ticks\":200,\"missed_ticks\":0,\"target_tps\":20,",
        "\"mspt_p95\":4.0,\"mspt_p99\":5.0,\"players\":1}\n"
    ));
    let output = Command::new(env!("CARGO_BIN_EXE_metrics-summary"))
        .arg(&path)
        .output()
        .unwrap();
    assert!(!output.status.success(), "v2 must include sample count");
    fs::remove_file(path).unwrap();
}
