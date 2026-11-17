//! NEXT-175 / #1158: once /stop has been accepted, the console worker sets
//! the shutdown flag before printing its marker, and the accept loop checks
//! that flag before spawning any session. A Login attempt that arrives after
//! the marker must therefore never complete, while the server still drains
//! and exits cleanly. Drives the real Rust server on loopback with a
//! disposable world; no Java client or operator world data is accessed.
use legacy_protocol::{read_frame, write_frame, write_string, write_varint};
use std::fs::{self, File};
use std::io::{self, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ACCOUNT: &str = "Next175Late";

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
            "industrialrust-next175-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create disposable test directory");
        fs::write(
            root.join("whitelist.json"),
            format!(r#"[{{"name":"{ACCOUNT}"}}]"#),
        )
        .expect("write isolated whitelist");
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("choose loopback port");
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let log = root.join("server.log");
        let output = File::create(&log).expect("create server log");
        let child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .args(["--bind", &format!("127.0.0.1:{port}"), "--world"])
            .arg(root.join("next175.irw"))
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

    /// Send /stop and wait until the console worker has set the shutdown flag
    /// (its stdout marker is written after the flag, so seeing the marker
    /// proves every later accept-loop pass observes shutdown).
    fn request_stop(&mut self) {
        self.child
            .stdin
            .as_mut()
            .expect("server stdin")
            .write_all(b"/stop\n")
            .expect("send /stop");
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let text = fs::read_to_string(&self.log).unwrap_or_default();
            if text.contains("Server stopping") {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "server never acknowledged /stop\n{text}"
            );
            thread::sleep(Duration::from_millis(25));
        }
    }
}

fn transmit(socket: &mut TcpStream, id: u32, body: &[u8]) -> io::Result<()> {
    let mut packet = Vec::new();
    write_varint(id, &mut packet);
    packet.extend_from_slice(body);
    write_frame(socket, &packet)
}

/// Handshake(next=2) body for the given advertised port.
fn handshake_body(port: u16) -> Vec<u8> {
    let mut handshake = Vec::new();
    write_varint(5, &mut handshake);
    write_string("localhost", &mut handshake).unwrap();
    handshake.extend_from_slice(&port.to_be_bytes());
    write_varint(2, &mut handshake);
    handshake
}

/// Complete a genuine whitelisted login and require Login Success (0x02).
fn expect_login_success(port: u16, label: &str) -> TcpStream {
    let socket = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut socket = socket;
    transmit(&mut socket, 0, &handshake_body(port)).expect("handshake");
    let mut start = Vec::new();
    write_string(ACCOUNT, &mut start).unwrap();
    transmit(&mut socket, 0, &start).expect("login start");
    let frame = read_frame(&mut socket).unwrap_or_else(|err| panic!("{label}: no login: {err}"));
    let mut response = legacy_protocol::Cursor::new(&frame);
    assert_eq!(
        response.read_varint().unwrap(),
        2,
        "{label}: expected Login Success"
    );
    response.read_string(40).unwrap();
    assert_eq!(
        response.read_string(16).unwrap(),
        ACCOUNT,
        "{label}: Login Success must echo the account name"
    );
    response.finish().unwrap();
    socket
}

/// After /stop, a fresh Login attempt must never complete: either the TCP
/// connect itself is refused (process already exited) or the server closes
/// the socket / never answers. A Login Success frame is the failure mode.
fn expect_login_never_completes(port: u16, label: &str) {
    let mut socket = match TcpStream::connect(("127.0.0.1", port)) {
        Ok(socket) => socket,
        Err(_) => return, // listener is gone: nothing was admitted
    };
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    // Best effort: even if the write fails the peer is already gone.
    if transmit(&mut socket, 0, &handshake_body(port)).is_err() {
        return;
    }
    let mut start = Vec::new();
    write_string(ACCOUNT, &mut start).unwrap();
    if transmit(&mut socket, 0, &start).is_err() {
        return;
    }
    match read_frame(&mut socket) {
        Err(_) => {} // timeout / EOF / reset: rejected without admission
        Ok(frame) => panic!(
            "{label}: server admitted a post-/stop Login with {} byte(s): {frame:?}",
            frame.len()
        ),
    }
}

#[test]
fn next175_login_after_stop_is_never_admitted_and_server_exits_cleanly() {
    let mut server = TestServer::start();

    // Control: the same client can genuinely log in while the server runs.
    let honest = expect_login_success(server.port, "login before /stop");
    honest.shutdown(Shutdown::Write).unwrap();
    drop(honest);

    server.request_stop();

    // The console marker implies the shutdown flag is already stored, so any
    // accept-loop pass after this point refuses session creation. Retry the
    // late attempt a few times across the drain window: while the listener
    // socket still exists the backlog is never served, and after exit every
    // attempt is refused. Both behaviours count as rejection.
    for round in 0..4 {
        expect_login_never_completes(server.port, &format!("late login {round}"));
        thread::sleep(Duration::from_millis(120));
    }

    // And the stop itself still completes gracefully.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = server.child.try_wait().expect("poll shutdown") {
            assert!(
                status.success(),
                "graceful shutdown failed: {status}\n{}",
                fs::read_to_string(&server.log).unwrap_or_default()
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "server /stop timed out\n{}",
            fs::read_to_string(&server.log).unwrap_or_default()
        );
        thread::sleep(Duration::from_millis(50));
    }
}
