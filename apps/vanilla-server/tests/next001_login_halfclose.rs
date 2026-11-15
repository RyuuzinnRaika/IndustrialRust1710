//! NEXT-001 / #984: a client that stops writing immediately after LoginStart
//! must never strand a name/UUID admission or a per-IP pending login permit.
//! Drives the real Rust server on loopback with a disposable world; no Java
//! client, Minecraft distribution, or operator world data is accessed.
use legacy_protocol::{read_frame, write_frame, write_string, write_varint, Cursor};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ACCOUNT: &str = "HalfClose01";

struct TestServer {
    child: Child,
    root: PathBuf,
    log: PathBuf,
    port: u16,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        // Preserve the temporary log when the test fails for diagnosis.
        if !thread::panicking() {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

impl TestServer {
    fn start() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "industrialrust-next001-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create disposable test directory");
        fs::write(root.join("whitelist.json"), r#"[{"name":"HalfClose01"}]"#)
            .expect("write isolated whitelist");
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("choose loopback port");
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let log = root.join("server.log");
        let output = File::create(&log).expect("create server log");
        let child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .args(["--bind", &format!("127.0.0.1:{port}"), "--world"])
            .arg(root.join("halfclose.irw"))
            .arg("--whitelist")
            .arg(root.join("whitelist.json"))
            .stdin(Stdio::piped())
            .stdout(Stdio::from(output.try_clone().unwrap()))
            .stderr(Stdio::from(output))
            .spawn()
            .expect("launch native Rust server");
        let mut server = Self {
            child,
            root,
            log,
            port,
        };
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            if let Some(exit) = server.child.try_wait().expect("poll server") {
                panic!(
                    "server exited before listening: {exit}\n{}",
                    fs::read_to_string(&server.log).unwrap_or_default()
                );
            }
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return server;
            }
            assert!(Instant::now() < deadline, "server startup timed out");
            thread::sleep(Duration::from_millis(40));
        }
    }

    fn stop(&mut self) {
        self.child
            .stdin
            .as_mut()
            .expect("server stdin")
            .write_all(b"/stop\n")
            .expect("send graceful stop");
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = self.child.try_wait().expect("poll shutdown") {
                assert!(
                    status.success(),
                    "graceful shutdown failed: {status}\n{}",
                    fs::read_to_string(&self.log).unwrap_or_default()
                );
                return;
            }
            assert!(Instant::now() < deadline, "server /stop timed out");
            thread::sleep(Duration::from_millis(50));
        }
    }
}

fn packet(id: u32, body: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();
    write_varint(id, &mut payload);
    payload.extend_from_slice(body);
    let mut frame = Vec::new();
    write_frame(&mut frame, &payload).unwrap();
    frame
}

fn login(port: u16) -> TcpStream {
    let mut socket = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    socket
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut handshake = Vec::new();
    write_varint(5, &mut handshake);
    write_string("localhost", &mut handshake).unwrap();
    handshake.extend_from_slice(&port.to_be_bytes());
    write_varint(2, &mut handshake);
    socket.write_all(&packet(0, &handshake)).unwrap();
    let mut start = Vec::new();
    write_string(ACCOUNT, &mut start).unwrap();
    socket.write_all(&packet(0, &start)).unwrap();
    socket
}

fn drain_until_server_closes(mut socket: TcpStream) {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut bytes = [0u8; 8192];
    loop {
        match socket.read(&mut bytes) {
            Ok(0) => return,
            Ok(_) => {}
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) => {}
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
                ) =>
            {
                return;
            }
            Err(err) => panic!("socket read failed while draining: {err}"),
        }
        assert!(
            Instant::now() < deadline,
            "half-closed login stayed connected for more than 15 seconds"
        );
    }
}

#[test]
fn next001_half_closed_login_releases_identity_for_same_name_rejoin() {
    let mut server = TestServer::start();

    // Repeat instead of relying on a single scheduling arrangement: an
    // unauthenticated abort may occur before or after the Hub admission.
    for attempt in 0..3 {
        let disconnected = login(server.port);
        disconnected
            .shutdown(Shutdown::Write)
            .expect("half close client write side");
        drain_until_server_closes(disconnected);

        let mut rejoined = login(server.port);
        // Login Success must be the first login response. A stuck Hub identity
        // would instead cause Login Disconnect (packet 0x00).
        rejoined
            .set_read_timeout(Some(Duration::from_secs(8)))
            .unwrap();
        let reply = read_frame(&mut rejoined).unwrap_or_else(|err| {
            panic!("rejoin {attempt} failed to receive Login Success: {err}")
        });
        let mut response = Cursor::new(&reply);
        assert_eq!(
            response.read_varint().unwrap(),
            2,
            "rejoin {attempt} was rejected, possibly by a stale admission"
        );
        assert_eq!(response.read_string(40).unwrap().len(), 36);
        assert_eq!(response.read_string(16).unwrap(), ACCOUNT);
        response.finish().unwrap();
        rejoined
            .shutdown(Shutdown::Write)
            .expect("close rejoin client write side");
        drain_until_server_closes(rejoined);
    }

    server.stop();
}
