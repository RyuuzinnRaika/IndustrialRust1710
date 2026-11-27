//! NEXT-004 / #987: Handshake next-state values, abnormal advertised ports
//! and legacy pre-handshake bytes must never admit a Play session. Drives
//! the real Rust server on loopback with a disposable world; no Java client,
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

const ACCOUNT: &str = "Next004Tester";
const PORT_ZERO_ACCOUNT: &str = "Next004PortZero";
const PORT_MAX_ACCOUNT: &str = "Next004PortMax";
const POST_LEGACY_ACCOUNT: &str = "Next004PostPing";

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
            "industrialrust-next004-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create disposable test directory");
        fs::write(
            root.join("whitelist.json"),
            serde_json::to_vec(
                &[
                    ACCOUNT,
                    PORT_ZERO_ACCOUNT,
                    PORT_MAX_ACCOUNT,
                    POST_LEGACY_ACCOUNT,
                ]
                .map(|name| serde_json::json!({ "name": name })),
            )
            .expect("serialize isolated whitelist"),
        )
        .expect("write isolated whitelist");
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("choose loopback port");
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let log = root.join("server.log");
        let output = File::create(&log).expect("create server log");
        let child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .args(["--bind", &format!("127.0.0.1:{port}"), "--world"])
            .arg(root.join("next004.irw"))
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

/// One length-prefixed Protocol 5 packet with the given body.
fn transmit(socket: &mut TcpStream, id: u32, body: &[u8]) {
    let mut packet = Vec::new();
    write_varint(id, &mut packet);
    packet.extend_from_slice(body);
    write_frame(socket, &packet).expect("write packet");
}

/// Handshake body with a raw next-state VarInt so non-canonical and
/// five-byte encodings can be sent exactly as a hostile client would.
fn handshake_body(version: u32, host: &str, port: u16, next_state: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    write_varint(version, &mut body);
    write_string(host, &mut body).unwrap();
    body.extend_from_slice(&port.to_be_bytes());
    body.extend_from_slice(next_state);
    body
}

fn login_start(name: &str) -> Vec<u8> {
    let mut body = Vec::new();
    write_string(name, &mut body).unwrap();
    body
}

/// A hostile Handshake must produce zero response bytes and a prompt close.
/// Any byte at all could become a Login Success, so this is strict: no frame,
/// no partial frame, only EOF or a reset within the bound.
fn expect_silent_close(mut socket: TcpStream, label: &str) {
    let _ = socket.shutdown(Shutdown::Write);
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut one = [0u8; 1];
    loop {
        match socket.read(&mut one) {
            Ok(0) => return,
            Ok(n) => panic!("{label}: server replied with {n} byte(s) to a rejected Handshake"),
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

/// Complete a genuine whitelisted login and require Login Success (0x02).
fn expect_login_success(port: u16, advertised_port: u16, name: &str, label: &str) {
    let mut socket = connect(port, Duration::from_secs(8));
    transmit(
        &mut socket,
        0,
        &handshake_body(5, "localhost", advertised_port, &[0x02]),
    );
    transmit(&mut socket, 0, &login_start(name));
    let frame =
        read_frame(&mut socket).unwrap_or_else(|err| panic!("{label}: no login response: {err}"));
    let mut response = Cursor::new(&frame);
    let packet_id = response.read_varint().unwrap();
    let disconnect_reason = if packet_id == 0 {
        response.read_string(32_768).ok()
    } else {
        None
    };
    assert_eq!(
        packet_id, 2,
        "{label}: expected Login Success, never Disconnect or Play; disconnect: {disconnect_reason:?}"
    );
    assert_eq!(
        response.read_string(40).unwrap().len(),
        36,
        "{label}: Login Success must carry a UUID"
    );
    assert_eq!(response.read_string(16).unwrap(), name);
    response.finish().unwrap();
    let _ = socket.shutdown(Shutdown::Write);
}

#[test]
fn next004_unknown_next_state_never_reaches_play() {
    let mut server = TestServer::start();
    let cases: &[(&str, &[u8])] = &[
        ("state 0", &[0x00][..]),
        ("state 3", &[0x03][..]),
        (
            "state -1 (five-byte VarInt)",
            &[0xFF, 0xFF, 0xFF, 0xFF, 0x0F][..],
        ),
        ("state 0 non-canonical VarInt", &[0x80, 0x00][..]),
        ("state 255", &[0xFF, 0x01][..]),
    ];
    for (label, next_state) in cases {
        let mut socket = connect(server.port, Duration::from_secs(5));
        transmit(
            &mut socket,
            0,
            &handshake_body(5, "localhost", server.port, next_state),
        );
        // Push a well-formed LoginStart behind the bad Handshake: an
        // implementation that skipped the state check would proceed to Play.
        transmit(&mut socket, 0, &login_start(ACCOUNT));
        expect_silent_close(socket, label);
    }
    // A rejected state must not poison later admissions.
    expect_login_success(server.port, server.port, ACCOUNT, "login after bad states");
    server.stop();
}

#[test]
fn next004_abnormal_ports_and_legacy_ping_are_fail_closed() {
    let mut server = TestServer::start();

    // Advertised ports are informational: 0 and 65535 must neither block a
    // genuine login nor route anything into Play by themselves.
    // Login Success precedes session teardown. Use distinct whitelisted
    // identities so the duplicate-login guard cannot mask port handling.
    expect_login_success(
        server.port,
        0,
        PORT_ZERO_ACCOUNT,
        "login via advertised port 0",
    );
    expect_login_success(
        server.port,
        65535,
        PORT_MAX_ACCOUNT,
        "login via advertised port 65535",
    );

    // Status state works with an advertised port that never matches bind.
    let mut status_socket = connect(server.port, Duration::from_secs(5));
    transmit(
        &mut status_socket,
        0,
        &handshake_body(5, "play.example.test", 65535, &[0x01]),
    );
    transmit(&mut status_socket, 0, &[]);
    let frame = read_frame(&mut status_socket).expect("status response");
    let mut payload = Cursor::new(&frame);
    assert_eq!(payload.read_varint().unwrap(), 0);
    let response: Value =
        serde_json::from_str(payload.read_string(32768).unwrap()).expect("status JSON");
    assert_eq!(response["version"]["protocol"], 5);
    drop(status_socket);

    // Legacy pre-handshake ping byte 0xFE. Framed, it declares a 254-byte
    // body whose first VarInt is 127 — not Handshake. It must be dropped
    // with zero response bytes.
    let mut legacy_framed = connect(server.port, Duration::from_secs(8));
    let mut payload = vec![0xFE];
    payload.extend_from_slice(&[0x7F; 254]);
    legacy_framed
        .write_all(&payload)
        .expect("write legacy ping");
    expect_silent_close(legacy_framed, "framed legacy ping 0xFE");

    // Unframed single byte: the reader must stop at its bounded socket
    // timeout instead of hanging on the declared 254-byte body forever.
    let mut legacy_raw = connect(server.port, Duration::from_secs(12));
    legacy_raw
        .write_all(&[0xFE])
        .expect("write raw legacy ping");
    expect_silent_close(legacy_raw, "unframed legacy ping 0xFE");

    // The server itself stays fully functional for honest clients.
    expect_login_success(
        server.port,
        server.port,
        POST_LEGACY_ACCOUNT,
        "login after legacy ping traffic",
    );
    server.stop();
}
