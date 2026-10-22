//! J91 subprocess integration: OS console /stop must exit cleanly and leave a reloadable world.
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn start_and_stop(root: &Path, world: &Path, whitelist: &Path, first_boot: bool) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
        .args(["--bind", "127.0.0.1:0", "--world"])
        .arg(world)
        .arg("--whitelist")
        .arg(whitelist)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("server process must start");
    let deadline = Instant::now() + Duration::from_secs(20);
    if first_boot {
        while !world.exists() && Instant::now() < deadline {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("server exited before world creation: {status}");
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert!(world.is_file(), "server failed to create its world");
    }
    // Existing world appears before workers and console finish initializing.
    thread::sleep(Duration::from_millis(300));
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"/stop\n")
        .expect("OS console must accept /stop");
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("server failed to terminate after /stop");
        }
        thread::sleep(Duration::from_millis(50));
    };
    let mut diagnostic = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut diagnostic)
        .unwrap();
    assert!(
        status.success(),
        "server shutdown failed: {status}; stderr={diagnostic}"
    );
    assert!(fs::metadata(world).unwrap().len() > 32);
}

#[test]
fn stop_flushes_and_allows_clean_reopen() {
    let root = std::env::temp_dir().join(format!("j91-stop-reopen-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let whitelist = root.join("whitelist.json");
    fs::write(&whitelist, r#"[{"name":"RustTester"}]"#).unwrap();
    let world = root.join("world/flat.irw");
    start_and_stop(&root, &world, &whitelist, true);
    // A second process must be able to acquire the lock and read the saved world.
    start_and_stop(&root, &world, &whitelist, false);
    fs::remove_dir_all(&root).unwrap();
}
