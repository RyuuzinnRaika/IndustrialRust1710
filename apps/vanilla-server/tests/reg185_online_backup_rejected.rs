//! REG-185 / #821: backup must fail closed while live players own the world.
//! No operator save path is used. The test creates a fresh, disposable world.
use legacy_protocol::{read_frame, read_varint, write_frame, write_string, write_varint, Cursor};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Server {
    child: Child,
    log: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn start(exe: &str, world: &Path, whitelist: &Path, root: &Path) -> (Server, u16) {
    let port = port();
    let log = root.join("live-server.log");
    let output = File::create(&log).unwrap();
    let child = Command::new(exe)
        .args(["--bind", &format!("127.0.0.1:{port}"), "--world"])
        .arg(world)
        .arg("--whitelist")
        .arg(whitelist)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(output.try_clone().unwrap()))
        .stderr(Stdio::from(output))
        .spawn()
        .unwrap();
    let mut server = Server { child, log };
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            assert!(server.child.try_wait().unwrap().is_none());
            return (server, port);
        }
        if let Some(status) = server.child.try_wait().unwrap() {
            panic!(
                "server exited before listen: {status}; {}",
                fs::read_to_string(&server.log).unwrap_or_default()
            );
        }
        assert!(Instant::now() < deadline, "native server startup timed out");
        thread::sleep(Duration::from_millis(40));
    }
}

fn packet(id: u32, body: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_varint(id, &mut bytes);
    bytes.extend_from_slice(body);
    let mut framed = Vec::new();
    write_frame(&mut framed, &bytes).unwrap();
    framed
}

fn join(port: u16, name: &str) -> TcpStream {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(8))).unwrap();
    stream.set_write_timeout(Some(Duration::from_secs(8))).unwrap();
    let mut handshake = Vec::new();
    write_varint(5, &mut handshake);
    write_string("localhost", &mut handshake).unwrap();
    handshake.extend_from_slice(&port.to_be_bytes());
    write_varint(2, &mut handshake);
    stream.write_all(&packet(0, &handshake)).unwrap();
    let mut username = Vec::new();
    write_string(name, &mut username).unwrap();
    stream.write_all(&packet(0, &username)).unwrap();
    let mut authenticated = false;
    for _ in 0..160 {
        let bytes = read_frame(&mut stream).unwrap();
        let mut remaining = bytes.as_slice();
        let id = read_varint(&mut remaining).unwrap();
        if id == 2 && !authenticated {
            let mut fields = Cursor::new(remaining);
            assert_eq!(fields.read_string(40).unwrap().len(), 36);
            assert_eq!(fields.read_string(16).unwrap(), name);
            fields.finish().unwrap();
            authenticated = true;
        } else if id == 0 && remaining.len() == 4 {
            stream.write_all(&packet(0, remaining)).unwrap();
        } else if id == 8 {
            assert!(authenticated, "position preceded Login Success");
            return stream;
        }
    }
    panic!("no complete Protocol 5 join for {name}");
}

fn wait_process(mut child: Child, timeout: Duration) -> (ExitStatus, String) {
    let until = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            let mut output = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                stderr.read_to_string(&mut output).unwrap();
            }
            return (status, output);
        }
        if Instant::now() >= until {
            let _ = child.kill();
            let _ = child.wait();
            panic!("offline backup did not finish within {timeout:?}");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn backup(exe: &str, world: &Path, destination: &Path) -> (ExitStatus, String) {
    let child = Command::new(exe)
        .arg("--world")
        .arg(world)
        .arg("--backup-world")
        .arg(destination)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_process(child, Duration::from_secs(25))
}

fn stop(server: &mut Server) {
    server
        .child
        .stdin
        .take()
        .unwrap()
        .write_all(b"/stop\n")
        .unwrap();
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = server.child.try_wait().unwrap() {
            assert!(
                status.success(),
                "server /stop failed ({status}): {}",
                fs::read_to_string(&server.log).unwrap_or_default()
            );
            break;
        }
        assert!(Instant::now() < until, "live server failed to drain on /stop");
        thread::sleep(Duration::from_millis(30));
    }
}

#[test]
fn reg185_backup_refuses_connected_players_then_copies_offline_snapshot() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "ir1710-reg185-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir(&root).unwrap(); // never reuse another world's directory
    let dir = root.join("world");
    let world = dir.join("flat.irw");
    let whitelist = root.join("whitelist.json");
    fs::write(&whitelist, r#"[{"name":"TwinAlpha"},{"name":"TwinBeta"}]"#).unwrap();
    let exe = env!("CARGO_BIN_EXE_vanilla-server");
    let (mut server, port) = start(exe, &world, &whitelist, &root);

    // Two real logged-in Play sessions, not just TCP socket probes.
    let alpha = join(port, "TwinAlpha");
    let beta = join(port, "TwinBeta");
    assert!(server.child.try_wait().unwrap().is_none());
    let original = fs::read(&world).unwrap();
    let destination = root.join("refused-online");
    let (status, errors) = backup(exe, &world, &destination);
    assert!(!status.success(), "online backup unexpectedly succeeded");
    assert!(
        errors.contains("world session lock is busy"),
        "offline backup rejected for an unrelated reason: {errors}"
    );
    assert!(!destination.exists(), "partial live snapshot was published");
    assert!(
        !root.join("refused-online.incomplete").exists(),
        "live backup left a staging directory"
    );
    assert_eq!(
        fs::read(&world).unwrap(),
        original,
        "rejected backup changed the live world"
    );
    assert!(server.child.try_wait().unwrap().is_none());

    drop(alpha);
    drop(beta);
    stop(&mut server);
    drop(server);

    let complete = root.join("offline-snapshot");
    let (status, errors) = backup(exe, &world, &complete);
    assert!(status.success(), "offline backup failed: {errors}");
    assert!(complete.is_dir());
    assert_eq!(
        fs::read(complete.join("flat.irw")).unwrap(),
        fs::read(&world).unwrap(),
        "backup world differs from source at the stopped-world boundary"
    );
    assert_eq!(
        fs::read(complete.join("session.lock")).unwrap().len(),
        8,
        "backup must carry a valid 1.7.10 world session marker"
    );
    // Both logged-in users must be present at the same offline boundary.
    // Comparing all saved bytes also detects copying a partial player NBT
    // while the shutdown worker was still mutating it.
    for player in ["twinalpha", "twinbeta"] {
        let relative = Path::new("players").join(format!("{player}.dat"));
        let original_player = fs::read(dir.join(&relative))
            .unwrap_or_else(|error| panic!("missing saved {player}: {error}"));
        let backed_up_player = fs::read(complete.join(&relative))
            .unwrap_or_else(|error| panic!("missing backup {player}: {error}"));
        assert_eq!(
            backed_up_player, original_player,
            "player {player} changed across the offline snapshot"
        );
    }
    let check = Command::new(exe)
        .arg("--verify-world")
        .arg(complete.join("flat.irw"))
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "offline snapshot failed read-only verification: {}",
        String::from_utf8_lossy(&check.stderr)
    );

    fs::remove_dir_all(&root).unwrap();
}
