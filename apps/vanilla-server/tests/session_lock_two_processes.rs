//! REG-184 / #820: two Rust server processes must not own one world.
//! This test uses a create-only disposable world. Never point it at a user save.
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn command(exe: &str, world: &Path, whitelist: &Path, bind: &str) -> Command {
    let mut cmd = Command::new(exe);
    cmd.args(["--bind", bind, "--world"])
        .arg(world)
        .arg("--whitelist")
        .arg(whitelist)
        .stdout(Stdio::null());
    cmd
}

fn temporary_loopback_port() -> u16 {
    // Use a distinct listener port for each owner and :0 for the contender.
    // The probed port must be bound *after* world session.lock is acquired.
    let probe = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    probe.local_addr().unwrap().port()
}

fn launch(exe: &str, world: &Path, whitelist: &Path, port: u16) -> Server {
    let bind = format!("127.0.0.1:{port}");
    let child = command(exe, world, whitelist, &bind)
        .stdin(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn isolated vanilla-server");
    Server(child)
}

fn wait_for_world(server: &mut Server, world: &Path, lock_path: &Path, port: u16) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let files_ready = world.is_file()
            && fs::metadata(lock_path)
                .is_ok_and(|metadata| metadata.is_file() && metadata.len() == 8);
        if files_ready && TcpStream::connect(("127.0.0.1", port)).is_ok() {
            // The old world/lock files survive a restart, so file existence
            // alone cannot prove that the new process acquired the lock.
            // The server binds its TCP listener only after world startup.
            assert!(server.0.try_wait().unwrap().is_none());
            return;
        }
        assert!(
            server.0.try_wait().unwrap().is_none(),
            "server exited before binding its isolated listener and world"
        );
        assert!(
            Instant::now() < deadline,
            "server never initialized its world and TCP listener"
        );
        thread::sleep(Duration::from_millis(40));
    }
}

fn denied_second_start(exe: &str, world: &Path, whitelist: &Path) -> (ExitStatus, String) {
    let mut child = command(exe, world, whitelist, "127.0.0.1:0")
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn competing server");
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            // A wrongly admitted second server could stay alive indefinitely.
            // Kill only our test child and fail, not the first server.
            let _ = child.kill();
            let _ = child.wait();
            panic!("second process was not rejected within ten seconds");
        }
        thread::sleep(Duration::from_millis(20));
    };
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    (status, stderr)
}

fn stop(server: &mut Server) {
    server
        .0
        .stdin
        .take()
        .expect("server stdin")
        .write_all(b"/stop\n")
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = server.0.try_wait().unwrap() {
            assert!(
                status.success(),
                "server failed graceful shutdown: {status}"
            );
            return;
        }
        assert!(Instant::now() < deadline, "server did not stop gracefully");
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn reg184_second_process_cannot_rewrite_a_live_world_and_lock_is_released() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let scratch =
        std::env::temp_dir().join(format!("ir1710-reg184-{}-{stamp}", std::process::id()));
    // Create-only prevents accidental reuse/removal of a real save directory.
    fs::create_dir(&scratch).unwrap();
    let world = scratch.join("isolated/flat.irw");
    let lock_path = scratch.join("isolated/session.lock");
    let whitelist = scratch.join("whitelist.json");
    fs::write(&whitelist, r#"[{"name":"RustTester"}]"#).unwrap();

    let exe = env!("CARGO_BIN_EXE_vanilla-server");
    let initial_port = temporary_loopback_port();
    let mut first = launch(exe, &world, &whitelist, initial_port);
    wait_for_world(&mut first, &world, &lock_path, initial_port);
    let world_before = fs::read(&world).unwrap();

    #[cfg(unix)]
    let marker_before = fs::read(&lock_path).unwrap();

    let (status, stderr) = denied_second_start(exe, &world, &whitelist);
    assert!(!status.success(), "competing process unexpectedly started");
    assert!(
        stderr.contains("world session lock is busy"),
        "expected exclusive world-lock failure, got: {stderr}"
    );
    assert!(
        first.0.try_wait().unwrap().is_none(),
        "rejecting the second process must not kill the first"
    );
    assert_eq!(
        fs::read(&world).unwrap(),
        world_before,
        "rejected process must not replace the active world snapshot"
    );
    assert_eq!(fs::metadata(&lock_path).unwrap().len(), 8);
    // Windows denies a second handle's byte-range reads while this lock is
    // held; on Unix, compare the actual marker bytes as additional evidence.
    #[cfg(unix)]
    assert_eq!(fs::read(&lock_path).unwrap(), marker_before);

    stop(&mut first);
    drop(first);
    let restarted_port = temporary_loopback_port();
    let mut restarted = launch(exe, &world, &whitelist, restarted_port);
    wait_for_world(&mut restarted, &world, &lock_path, restarted_port);
    stop(&mut restarted);
    drop(restarted);

    fs::remove_dir_all(&scratch).unwrap();
}
