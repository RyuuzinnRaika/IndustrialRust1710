//! J91 subprocess acceptance: terminate via real OS console and verify saved world.
use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn stop_closes_server_and_persists_readable_world() {
    let exe = env!("CARGO_BIN_EXE_vanilla-server");
    let root = std::env::temp_dir().join(format!("j91-stop-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let whitelist = root.join("whitelist.json");
    fs::write(&whitelist, r#"[{"name":"RustTester"}]"#).unwrap();
    let world = root.join("world/flat.irw");
    let mut child = Command::new(exe)
        .args(["--bind", "127.0.0.1:0", "--world"])
        .arg(&world)
        .arg("--whitelist")
        .arg(&whitelist)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("server subprocess should spawn");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !world.exists() && Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("server exited before world startup: {status}");
        }
        thread::sleep(Duration::from_millis(50));
    }
    if !world.exists() {
        child.kill().unwrap();
        let _ = child.wait();
        panic!("server did not create its world");
    }
    // A successful world write precedes the console thread spawn.
    // Give the server a short opportunity to initialize all workers.
    thread::sleep(Duration::from_millis(200));
    child.stdin.take().unwrap().write_all(b"/stop\n").unwrap();
    let finished = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("server did not terminate after /stop");
        }
        thread::sleep(Duration::from_millis(50));
    };
    assert!(
        finished.success(),
        "server exit status after /stop: {finished}"
    );
    let check = Command::new(exe)
        .arg("--verify-world")
        .arg(world.parent().unwrap())
        .status()
        .unwrap();
    assert!(check.success(), "saved world failed read-only validation");
    fs::remove_dir_all(&root).unwrap();
}
