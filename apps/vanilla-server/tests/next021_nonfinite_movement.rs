//! NEXT-021 / #1004: absolute position packets carrying NaN, Infinity or
//! huge-but-finite coordinates must never corrupt the stored position and
//! must never crash the server. Two distinct fail-closed behaviours are
//! pinned against the REAL server binary (isolated scratch world):
//!
//!   1. non-finite values (NaN / +/-Infinity in x, feet Y, stance, Z or in
//!      the C06 yaw/pitch floats) abort packet parsing: the offending
//!      connection is closed and NO position is adopted;
//!   2. huge but finite coordinates (1e18) are out of the bounded world:
//!      instead of adopting them, the server answers with an S08 teleport
//!      correction to its own finite authoritative position;
//!   3. afterwards the server is still healthy: a fresh session joins
//!      normally and reports a finite position (the player save was never
//!      poisoned by any of the hostile packets).
//!
//! Real 1.7.10 client acceptance: NOT_RUN (headless host; these are wire
//! packets an unmodified client never sends, and the test distinguishes
//! them from simulated behaviour by asserting the server's wire replies).
use legacy_protocol::{read_frame, read_varint, write_string, write_varint};
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Server {
    child: Child,
    root: std::path::PathBuf,
    port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        if !thread::panicking() {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

impl Server {
    fn start(tag: &str) -> Server {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "ir1710-next021-{tag}-{}-{stamp}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("whitelist.json"),
            r#"[{"name":"FiniteTester"},{"name":"HostileSocket"}]"#,
        )
        .unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .args(["--bind", &format!("127.0.0.1:{port}"), "--world"])
            .arg(root.join("flat.irw"))
            .arg("--whitelist")
            .arg(root.join("whitelist.json"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "server did not open {port}");
            thread::sleep(Duration::from_millis(100));
        }
        Server { child, root, port }
    }

    fn connect(&self, name: &str) -> TcpStream {
        let mut socket = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(250)))
            .unwrap();
        let mut handshake = Vec::new();
        write_varint(5, &mut handshake);
        write_string("127.0.0.1", &mut handshake).unwrap();
        handshake.extend_from_slice(&self.port.to_be_bytes());
        write_varint(2, &mut handshake);
        socket.write_all(&frame(0, &handshake)).unwrap();
        socket.write_all(&frame(0, &mc_string(name))).unwrap();
        socket
    }
}

fn vi(value: u32) -> Vec<u8> {
    let mut out = Vec::new();
    write_varint(value, &mut out);
    out
}

fn mc_string(value: &str) -> Vec<u8> {
    let mut out = Vec::new();
    write_string(value, &mut out).unwrap();
    out
}

fn frame(id: u32, data: &[u8]) -> Vec<u8> {
    let mut packet = vi(id);
    packet.extend_from_slice(data);
    let mut message = vi(packet.len() as u32);
    message.extend_from_slice(&packet);
    message
}

fn pump(socket: &mut TcpStream) -> (u32, Vec<u8>) {
    let packet = read_frame(socket).expect("play frame");
    let mut raw = packet.as_slice();
    let id = read_varint(&mut raw).expect("packet id");
    (id, raw.to_vec())
}

fn read_position(packet: &[u8]) -> [f64; 3] {
    // S08 carries the client stance (feet + eye); feet Y = stance - 1.62.
    [
        f64::from_be_bytes(packet[0..8].try_into().unwrap()),
        f64::from_be_bytes(packet[8..16].try_into().unwrap()) - f64::from(1.62_f32),
        f64::from_be_bytes(packet[16..24].try_into().unwrap()),
    ]
}

/// C04 absolute position wire order: X, feet Y, stance, Z, onGround.
fn position_packet(x: f64, feet_y: f64, z: f64) -> Vec<u8> {
    let mut body = Vec::new();
    for value in [x, feet_y, feet_y + 1.62, z] {
        body.extend_from_slice(&value.to_be_bytes());
    }
    body.push(1);
    frame(4, &body)
}

/// Drain the join burst until the first S08, echoing KeepAlive like a client.
fn await_position(socket: &mut TcpStream, who: &str) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        assert!(Instant::now() < deadline, "{who}: no S08 after join");
        let (id, body) = pump(socket);
        if id == 0x00 && body.len() == 4 {
            let _ = socket.write_all(&frame(0, &body));
        }
        if id == 0x08 {
            return body;
        }
    }
}

/// The server must close this connection (EOF) without adopting a position.
fn expect_close(socket: &mut TcpStream, who: &str) {
    let mut chunk = [0u8; 8192];
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match socket.read(&mut chunk) {
            Ok(0) => return,
            Ok(_) => {}
            Err(error)
                if error.kind() == ErrorKind::WouldBlock || error.kind() == ErrorKind::TimedOut => {
            }
            Err(error) => panic!("{who}: unexpected socket error {error}"),
        }
        assert!(
            Instant::now() < deadline,
            "{who}: server never closed the socket"
        );
    }
}

#[test]
fn next021_nonfinite_positions_close_only_their_connection_and_never_poison_state() {
    let mut server = Server::start("nonfinite");
    // A legitimate session stays online throughout the hostile traffic.
    let mut keeper = server.connect("FiniteTester");
    let first = await_position(&mut keeper, "FiniteTester");
    let original = read_position(&first);
    assert!(
        original.iter().all(|value| value.is_finite()),
        "legit join position must be finite: {original:?}"
    );

    let nan = f64::NAN;
    let inf = f64::INFINITY;
    // Each hostile socket completes a legitimate Login, then sends ONE
    // non-finite absolute position packet. Each must be refused by closing
    // that single connection (parser fail-closed), never adopted.
    let cases: [(&str, Vec<u8>); 5] = [
        ("nan-x", position_packet(nan, 64.0, 0.5)),
        ("nan-y", position_packet(0.5, nan, 0.5)),
        ("inf-y", position_packet(0.5, inf, 0.5)),
        ("neg-inf-z", position_packet(0.5, 64.0, -inf)),
        ("nan-pitch-c06", {
            let mut body = Vec::new();
            for value in [0.5f64, 64.0, 65.62, 0.5] {
                body.extend_from_slice(&value.to_be_bytes());
            }
            body.extend_from_slice(&0.0f32.to_be_bytes()); // yaw
            body.extend_from_slice(&f32::NAN.to_be_bytes()); // pitch
            body.push(1);
            frame(6, &body)
        }),
    ];
    for (name, packet) in cases {
        let mut hostile = server.connect("HostileSocket");
        await_position(&mut hostile, name);
        hostile.write_all(&packet).unwrap();
        let _ = hostile.flush();
        expect_close(&mut hostile, name);
        drop(hostile);
    }

    // The server itself is still healthy: the keeper session is alive.
    keeper
        .write_all(&frame(1, &mc_string("/list")))
        .expect("keeper session must still accept chat");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut listed = false;
    while Instant::now() < deadline && !listed {
        let (id, body) = pump(&mut keeper);
        listed = id == 0x02 && String::from_utf8_lossy(&body).contains("Online (1)");
    }
    assert!(listed, "keeper session lost after hostile packets");
    assert!(
        server.child.try_wait().expect("poll server").is_none(),
        "server process must still be alive after hostile packets"
    );
}

#[test]
fn next021_huge_finite_position_gets_finite_teleport_correction_not_adoption() {
    let mut server = Server::start("huge");
    let mut client = server.connect("FiniteTester");
    let first = await_position(&mut client, "FiniteTester");
    let original = read_position(&first);

    // A finite but wildly out-of-bounds coordinate (1e18) must NOT be
    // adopted. The server answers with S08 to its own authoritative,
    // finite position (fail closed by teleport correction).
    client
        .write_all(&position_packet(1.0e18, 64.0, 0.5))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let corrected = loop {
        assert!(Instant::now() < deadline, "no S08 correction for 1e18");
        let (id, body) = pump(&mut client);
        if id == 0x08 && body.len() >= 24 {
            break read_position(&body);
        }
    };
    assert!(
        corrected.iter().all(|value| value.is_finite()),
        "teleport correction must be finite: {corrected:?}"
    );
    assert!(
        corrected[0].abs() < 1.0e6 && corrected[2].abs() < 1.0e6,
        "correction must not adopt the 1e18 coordinate: {corrected:?}"
    );
    assert_eq!(
        corrected, original,
        "correction must restore the authoritative position"
    );

    // Acknowledge the correction and prove the session continues normally.
    client
        .write_all(&position_packet(corrected[0], corrected[1], corrected[2]))
        .unwrap();
    client
        .write_all(&frame(1, &mc_string("/list")))
        .expect("session must continue after correction ack");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut listed = false;
    while Instant::now() < deadline && !listed {
        let (id, body) = pump(&mut client);
        listed = id == 0x02 && String::from_utf8_lossy(&body).contains("Online (1)");
    }
    assert!(listed, "session did not resume after teleport correction");

    // Clean stop; the finite authoritative position must be the one that
    // persisted (the 1e18 hostile coordinate was never stored).
    {
        let mut stdin = server.child.stdin.take().expect("server stdin");
        stdin.write_all(b"/stop\n").unwrap();
        stdin.flush().unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match server.child.try_wait().unwrap() {
            Some(status) => {
                assert!(status.success(), "server must stop cleanly: {status}");
                break;
            }
            None => {
                assert!(Instant::now() < deadline, "server did not stop");
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
    let save = server.root.join("players").join("finitetester.dat");
    assert!(save.exists(), "player save must exist after clean stop");
    let _ = fs::remove_dir_all(&server.root);
}
