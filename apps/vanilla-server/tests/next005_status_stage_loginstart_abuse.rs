//! NEXT-005 / #988: a LoginStart sent while a connection is in Status state
//! must never be admitted to Play, and the rejected connection must be
//! released instead of piling up pending-connection resources. Drives the
//! real Rust server on loopback with a disposable world; no Java client,
//! Minecraft distribution, or operator world data is accessed.
use legacy_protocol::{read_frame, write_frame, write_string, write_varint, Cursor};
use serde_json::Value;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ACCOUNT: &str = "Next005Tester";

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
            "industrialrust-next005-{}-{stamp}",
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
            .arg(root.join("next005.irw"))
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

fn connect(port: u16, read_timeout: Duration) -> TcpStream {
    let socket = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    socket.set_read_timeout(Some(read_timeout)).unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    socket
}

fn transmit(socket: &mut TcpStream, id: u32, body: &[u8]) {
    let mut packet = Vec::new();
    write_varint(id, &mut packet);
    packet.extend_from_slice(body);
    write_frame(socket, &packet).expect("write packet");
}

fn status_handshake(port: u16) -> Vec<u8> {
    let mut body = Vec::new();
    write_varint(5, &mut body);
    write_string("localhost", &mut body).unwrap();
    body.extend_from_slice(&port.to_be_bytes());
    write_varint(1, &mut body);
    body
}

fn login_start(name: &str) -> Vec<u8> {
    let mut body = Vec::new();
    write_string(name, &mut body).unwrap();
    body
}

/// The Status-state abuser must receive zero bytes and a prompt close.
fn expect_silent_close(mut socket: TcpStream, label: &str) {
    let _ = socket.shutdown(Shutdown::Write);
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut one = [0u8; 1];
    loop {
        match socket.read(&mut one) {
            Ok(0) => return,
            Ok(n) => {
                panic!("{label}: server replied with {n} byte(s) to a Status-state LoginStart")
            }
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
                ) =>
            {
                return;
            }
            Err(err) if matches!(err.kind(), io::ErrorKind::TimedOut) => {
                assert!(
                    Instant::now() < deadline,
                    "{label}: rejected connection stayed open without closing"
                );
            }
            Err(err) => panic!("{label}: unexpected socket error: {err}"),
        }
    }
}

fn expect_status_json(port: u16) {
    let mut socket = connect(port, Duration::from_secs(5));
    transmit(&mut socket, 0, &status_handshake(port));
    transmit(&mut socket, 0, &[]);
    let frame = read_frame(&mut socket).expect("status response");
    let mut payload = Cursor::new(&frame);
    assert_eq!(payload.read_varint().unwrap(), 0);
    let response: Value =
        serde_json::from_str(payload.read_string(32768).unwrap()).expect("status JSON");
    assert_eq!(response["version"]["protocol"], 5);
    drop(socket);
}

fn expect_login_success(port: u16, label: &str) {
    let mut socket = connect(port, Duration::from_secs(8));
    let mut handshake = Vec::new();
    write_varint(5, &mut handshake);
    write_string("localhost", &mut handshake).unwrap();
    handshake.extend_from_slice(&port.to_be_bytes());
    write_varint(2, &mut handshake);
    transmit(&mut socket, 0, &handshake);
    transmit(&mut socket, 0, &login_start(ACCOUNT));
    let frame = read_frame(&mut socket).unwrap_or_else(|err| panic!("{label}: no login: {err}"));
    let mut response = Cursor::new(&frame);
    assert_eq!(
        response.read_varint().unwrap(),
        2,
        "{label}: expected Login Success after Status-state abuse"
    );
    assert_eq!(
        response.read_string(40).unwrap().len(),
        36,
        "{label}: Login Success must carry a UUID"
    );
    assert_eq!(response.read_string(16).unwrap(), ACCOUNT);
    response.finish().unwrap();
    let _ = socket.shutdown(Shutdown::Write);
}

#[test]
fn next005_loginstart_in_status_state_is_rejected_without_play() {
    let mut server = TestServer::start();

    // First packet after a Status handshake must be an empty request.
    // A LoginStart (0x00) with a name body is rejected with zero bytes.
    let mut early = connect(server.port, Duration::from_secs(5));
    transmit(&mut early, 0, &status_handshake(server.port));
    transmit(&mut early, 0, &login_start(ACCOUNT));
    expect_silent_close(early, "LoginStart instead of Status request");

    // Same state, but with a trailing byte after the request id: still not
    // an empty request, still closed in silence.
    let mut dirty = connect(server.port, Duration::from_secs(5));
    transmit(&mut dirty, 0, &status_handshake(server.port));
    transmit(&mut dirty, 0, &[0x2A]);
    expect_silent_close(dirty, "Status request with trailing garbage");

    // After a genuine status response, a LoginStart instead of Ping must
    // close the connection and must never produce a second response.
    let mut late = connect(server.port, Duration::from_secs(5));
    transmit(&mut late, 0, &status_handshake(server.port));
    transmit(&mut late, 0, &[]);
    let frame = read_frame(&mut late).expect("status response JSON");
    assert!(!frame.is_empty());
    transmit(&mut late, 0, &login_start(ACCOUNT));
    expect_silent_close(late, "LoginStart instead of Status ping");

    // The abuse above must not have exhausted pending-connection slots:
    // honest Status and Login flows still work end to end.
    expect_status_json(server.port);
    expect_login_success(server.port, "login after Status-state abuse");
    server.stop();
}

#[test]
fn next005_repeated_status_abuse_does_not_exhaust_connections() {
    let mut server = TestServer::start();
    // Twelve sequential abusers: each rejected connection must be released
    // immediately, far below any pending-connection admission limit.
    for attempt in 0..12 {
        let mut abuser = connect(server.port, Duration::from_secs(5));
        transmit(&mut abuser, 0, &status_handshake(server.port));
        transmit(&mut abuser, 0, &login_start(ACCOUNT));
        expect_silent_close(abuser, &format!("abuse attempt {attempt}"));
    }
    expect_status_json(server.port);
    expect_login_success(server.port, "login after 12 Status-state abusers");
    server.stop();
}
