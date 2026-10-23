//! A10 subprocess acceptance: all dimensions share one root and survive restarts.
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn boot_and_stop(exe: &str, root: &Path, whitelist: &Path, dimension: &str) {
    let mut child = Command::new(exe)
        .args([
            "--bind",
            "127.0.0.1:0",
            "--survival",
            "--expanded-seed",
            "42",
            "--dimension-root",
        ])
        .arg(root)
        .args(["--dimension", dimension, "--whitelist"])
        .arg(whitelist)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("multi-world server should spawn");
    let worlds = [
        root.join("overworld.irw"),
        root.join("DIM-1/nether.irw"),
        root.join("DIM1/end.irw"),
    ];
    let deadline = Instant::now() + Duration::from_secs(25);
    while !worlds.iter().all(|path| path.is_file()) && Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("multi-world startup failed: {status}");
        }
        thread::sleep(Duration::from_millis(50));
    }
    if !worlds.iter().all(|path| path.is_file()) {
        child.kill().unwrap();
        let _ = child.wait();
        panic!("multi-world startup failed to generate all dimension files");
    }
    thread::sleep(Duration::from_millis(250));
    child.stdin.take().unwrap().write_all(b"/stop\n").unwrap();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("multi-world server did not stop");
        }
        thread::sleep(Duration::from_millis(50));
    };
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(status.success(), "multi-world shutdown failed: {stderr}");
    assert!(worlds.iter().all(|path| path.is_file()));
}

#[test]
fn three_dimensions_boot_restart_and_reject_seed_mismatch() {
    let exe = env!("CARGO_BIN_EXE_vanilla-server");
    let scratch = std::env::temp_dir().join(format!(
        "ir1710-a10-bootstrap-{}-{:?}",
        std::process::id(),
        thread::current().id()
    ));
    let _ = fs::remove_dir_all(&scratch);
    fs::create_dir_all(&scratch).unwrap();
    let root = scratch.join("three-worlds");
    let whitelist = scratch.join("whitelist.json");
    fs::write(&whitelist, r#"[{"name":"RustTester"}]"#).unwrap();

    boot_and_stop(exe, &root, &whitelist, "overworld");
    boot_and_stop(exe, &root, &whitelist, "nether");
    let files = [
        root.join("overworld.irw"),
        root.join("DIM-1/nether.irw"),
        root.join("DIM1/end.irw"),
    ];
    let snapshots = files.map(|path| fs::read(path).unwrap());

    let status = Command::new(exe)
        .args(["--bind", "127.0.0.1:0", "--survival", "--expanded-seed", "43", "--dimension-root"])
        .arg(&root)
        .args(["--whitelist"])
        .arg(&whitelist)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success(), "incorrect seed must refuse startup");
    for (path, original) in files.iter().zip(snapshots.iter()) {
        assert_eq!(&fs::read(path).unwrap(), original);
    }
    fs::remove_dir_all(&scratch).unwrap();
}
