//! TCP-level acceptance tests for ADD-001..010 (Protocol 5).
//! Unit tests cover byte decoding / bounded NBT; these drive a real server.
use legacy_protocol::{read_frame, read_varint, write_string, write_varint, Cursor};
use serde_json::{json, Value};
use std::fs;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Server {
    child: Child,
    dir: PathBuf,
    port: u16,
}

impl Server {
    fn launch() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "industrialrust1710-p5-security-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("whitelist.json"),
            serde_json::to_vec(&json!([
                {"name": "RustTester"},
                {"name": "LongName12345678"}
            ]))
            .unwrap(),
        )
        .unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .args(["--bind", &format!("127.0.0.1:{port}")])
            .arg("--whitelist")
            .arg(dir.join("whitelist.json"))
            .arg("--world")
            .arg(dir.join("security.irw"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let server = Self { child, dir, port };
        let deadline = Instant::now() + Duration::from_secs(15);
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(Instant::now() < deadline, "test server failed to bind");
            thread::sleep(Duration::from_millis(40));
        }
        server
    }

    fn connect(&self) -> TcpStream {
        // An earlier negative-protocol connection may still be releasing its
        // per-IP permit. A refused TCP connection is not a protocol assertion;
        // retry briefly without accepting any invalid login or packet.
        let deadline = Instant::now() + Duration::from_secs(2);
        let s = loop {
            match TcpStream::connect(("127.0.0.1", self.port)) {
                Ok(socket) => break socket,
                Err(error)
                    if error.kind() == io::ErrorKind::ConnectionRefused
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(50));
                }
                Err(error) => panic!("test TCP server unavailable: {error}"),
            }
        };
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        s.set_write_timeout(Some(Duration::from_secs(3))).unwrap();
        s
    }

    fn status_online(&self) -> u64 {
        let mut s = self.connect();
        s.write_all(&handshake(self.port, "localhost", 1)).unwrap();
        s.write_all(&frame(0, &[])).unwrap();
        let (id, payload) = receive(&mut s);
        assert_eq!(id, 0);
        let mut c = Cursor::new(&payload);
        let value: Value = serde_json::from_str(c.read_string(65536).unwrap()).unwrap();
        c.finish().unwrap();
        assert_eq!(value["version"]["protocol"], 5);
        value["players"]["online"].as_u64().unwrap()
    }

    fn login(&self, name: &str) -> TcpStream {
        let mut s = self.connect();
        s.write_all(&handshake(self.port, "localhost", 2)).unwrap();
        s.write_all(&frame(0, &mc_string(name))).unwrap();
        let (id, payload) = receive(&mut s);
        assert_eq!(id, 2, "login {name} rejected: {payload:?}");
        s
    }

    fn wait_online(&self, expected: u64) {
        let until = Instant::now() + Duration::from_secs(8);
        while self.status_online() != expected {
            assert!(
                Instant::now() < until,
                "online count never became {expected}"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn varint(v: u32) -> Vec<u8> {
    let mut b = Vec::new();
    write_varint(v, &mut b);
    b
}

fn mc_string(value: &str) -> Vec<u8> {
    let mut b = Vec::new();
    write_string(value, &mut b).unwrap();
    b
}

fn frame(id: u32, payload: &[u8]) -> Vec<u8> {
    let mut data = varint(id);
    data.extend_from_slice(payload);
    let mut output = varint(data.len() as u32);
    output.extend_from_slice(&data);
    output
}

fn handshake_version(port: u16, host: &str, next: u32, version: u32) -> Vec<u8> {
    let mut body = varint(version);
    body.extend_from_slice(&mc_string(host));
    body.extend_from_slice(&port.to_be_bytes());
    body.extend_from_slice(&varint(next));
    frame(0, &body)
}

fn handshake(port: u16, host: &str, next: u32) -> Vec<u8> {
    handshake_version(port, host, next, 5)
}

fn receive(stream: &mut TcpStream) -> (u32, Vec<u8>) {
    let packet = read_frame(stream).expect("TCP packet missing");
    let mut input = packet.as_slice();
    let id = read_varint(&mut input).unwrap();
    (id, input.to_vec())
}

fn must_close(mut socket: TcpStream) {
    // Read through any pending Login Disconnect / Join chunks; the remote
    // endpoint must close because the packet is malformed, not because this
    // test closed its own write half.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut bytes = [0; 65536];
    loop {
        match socket.read(&mut bytes) {
            Ok(0) => {
                // The child closes its socket before the worker's IP permit
                // drops. Give it time to finish before the next adversarial
                // handshake; security assertions remain unchanged.
                thread::sleep(Duration::from_millis(100));
                return;
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
                ) =>
            {
                thread::sleep(Duration::from_millis(100));
                return;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) => {}
            Err(error) => panic!("unexpected socket error: {error}"),
        }
        assert!(
            Instant::now() < deadline,
            "server retained malformed TCP session"
        );
    }
}

fn bad_handshake(server: &Server, data: &[u8]) {
    let mut socket = server.connect();
    socket.write_all(data).unwrap();
    must_close(socket);
    assert_eq!(server.status_online(), 0);
}

fn bad_login(server: &Server, payload: &[u8]) {
    let mut socket = server.connect();
    socket
        .write_all(&handshake(server.port, "localhost", 2))
        .unwrap();
    socket.write_all(payload).unwrap();
    must_close(socket);
    assert_eq!(server.status_online(), 0);
}

#[test]
fn add005_state_machine_never_admits_out_of_order_packets() {
    let server = Server::launch();
    assert_eq!(server.status_online(), 0);

    // No packets (including valid Play IDs) are permitted before Handshake.
    bad_handshake(&server, &frame(4, &[0; 33]));
    bad_handshake(&server, &frame(1, &0i64.to_be_bytes()));
    // Only state 1 (Status) and state 2 (Login) exist after Handshake.
    for next in [0, 3, 99] {
        bad_handshake(&server, &handshake(server.port, "localhost", next));
    }

    // A Login Start is not a Status Request (which must have no body).
    let mut status = server.connect();
    status
        .write_all(&handshake(server.port, "localhost", 1))
        .unwrap();
    status
        .write_all(&frame(0, &mc_string("RustTester")))
        .unwrap();
    must_close(status);
    assert_eq!(server.status_online(), 0);

    // Once the Status Response was sent, this socket can only Ping/close.
    // It must not be upgraded to Login or Play by a subsequent packet.
    let mut status = server.connect();
    status
        .write_all(&handshake(server.port, "localhost", 1))
        .unwrap();
    status.write_all(&frame(0, &[])).unwrap();
    assert_eq!(receive(&mut status).0, 0);
    status
        .write_all(&frame(0, &mc_string("RustTester")))
        .unwrap();
    must_close(status);
    assert_eq!(server.status_online(), 0);

    // Valid Handshake->Login disallows Status Ping and any Play packet.
    bad_login(&server, &frame(1, &0i64.to_be_bytes()));
    bad_login(&server, &frame(4, &[0; 33]));

    // Pipelining a whitelisted-looking Play mutation behind an unapproved
    // identity must not create a session or enter the game dispatcher.
    let mut unauthenticated = server.connect();
    let mut pipelined = handshake(server.port, "localhost", 2);
    pipelined.extend_from_slice(&frame(0, &mc_string("NotAllowed")));
    pipelined.extend_from_slice(&frame(8, &[0; 15]));
    unauthenticated.write_all(&pipelined).unwrap();
    let (id, _) = receive(&mut unauthenticated);
    assert_eq!(id, 0, "unapproved identity must receive Login Disconnect");
    must_close(unauthenticated);
    assert_eq!(server.status_online(), 0);

    // The invalid sessions must not poison subsequent successful admission.
    let valid = server.login("RustTester");
    server.wait_online(1);
    drop(valid);
    server.wait_online(0);
}

#[test]
fn add005_duplicate_login_does_not_replace_active_play_session() {
    let server = Server::launch();
    let mut first = server.login("RustTester");
    server.wait_online(1);

    // One identity may have only one admitted Play session. A simultaneous
    // second Login receives a Login Disconnect, never Login Success.
    let mut duplicate = server.connect();
    duplicate
        .write_all(&handshake(server.port, "localhost", 2))
        .unwrap();
    duplicate
        .write_all(&frame(0, &mc_string("RustTester")))
        .unwrap();
    let (id, _) = receive(&mut duplicate);
    assert_eq!(id, 0, "duplicate login must not enter Play");
    must_close(duplicate);
    server.wait_online(1);

    // The original player must remain authenticated and connected.
    first.write_all(&frame(0, &0i32.to_be_bytes())).unwrap();
    server.wait_online(1);
    drop(first);
    server.wait_online(0);
}

#[test]
fn add011_two_simultaneous_tcp_logins_have_exactly_one_play_owner() {
    use std::sync::{Arc, Barrier};

    let server = Server::launch();
    let mut sockets = Vec::new();
    for _ in 0..2 {
        let mut stream = server.connect();
        stream
            .write_all(&handshake(server.port, "localhost", 2))
            .unwrap();
        sockets.push(stream);
    }
    let barrier = Arc::new(Barrier::new(2));
    let threads: Vec<_> = sockets
        .into_iter()
        .map(|mut socket| {
            let start = Arc::clone(&barrier);
            thread::spawn(move || {
                start.wait();
                socket
                    .write_all(&frame(0, &mc_string("RustTester")))
                    .unwrap();
                let (packet_id, _) = receive(&mut socket);
                (packet_id, socket)
            })
        })
        .collect();
    let mut results: Vec<_> = threads
        .into_iter()
        .map(|join| join.join().unwrap())
        .collect();
    results.sort_by_key(|(packet_id, _)| *packet_id);
    assert_eq!(
        results
            .iter()
            .map(|(packet_id, _)| *packet_id)
            .collect::<Vec<_>>(),
        vec![0, 2],
        "one Login Disconnect and one Login Success are required",
    );

    let rejected = results.remove(0).1;
    must_close(rejected);
    let mut owner = results.remove(0).1;
    server.wait_online(1);
    // The losing connection cannot replace or kick the admitted session.
    owner.write_all(&frame(0, &0i32.to_be_bytes())).unwrap();
    server.wait_online(1);
    drop(owner);
    server.wait_online(0);

    // Only after the owner exits may the identity be admitted again.
    let replacement = server.login("RustTester");
    server.wait_online(1);
    drop(replacement);
    server.wait_online(0);
}

#[test]
fn add012_reconnect_never_allows_old_socket_to_overwrite_new_inventory_save() {
    let server = Server::launch();
    let inventory_path = server.dir.join("players/rusttester.inventory.json");

    let join_play = |socket: &mut TcpStream| {
        socket
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            assert!(Instant::now() < deadline, "server did not complete Join");
            if receive(socket).0 == 0x08 {
                return;
            }
        }
    };

    let write_slot = |socket: &mut TcpStream, id: i16, count: u8| {
        let mut item = 36i16.to_be_bytes().to_vec();
        item.extend_from_slice(&id.to_be_bytes());
        item.push(count);
        item.extend_from_slice(&0i16.to_be_bytes());
        item.extend_from_slice(&(-1i16).to_be_bytes());
        socket.write_all(&frame(0x10, &item)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            assert!(
                Instant::now() < deadline,
                "Creative SetSlot not acknowledged"
            );
            let (packet_id, body) = receive(socket);
            if packet_id == 0x2f && body.starts_with(&[0, 0, 36]) {
                assert_eq!(&body[3..5], &id.to_be_bytes());
                assert_eq!(body[5], count);
                return;
            }
        }
    };

    let mut previous = server.login("RustTester");
    join_play(&mut previous);
    write_slot(&mut previous, 1, 3);
    let mut stale = previous.try_clone().unwrap();
    previous.shutdown(Shutdown::Both).unwrap();
    drop(previous);
    server.wait_online(0);

    let first: Value = serde_json::from_slice(&fs::read(&inventory_path).unwrap()).unwrap();
    assert_eq!(first["slots"][36]["id"], 1);
    assert_eq!(first["slots"][36]["count"], 3);

    let mut current = server.login("RustTester");
    join_play(&mut current);
    write_slot(&mut current, 4, 7);
    server.wait_online(1);

    // Even though the old TCP handle still exists locally, the socket has
    // been shut down and cannot enqueue a stale Creative inventory mutation.
    assert!(stale
        .write_all(&frame(0x10, &[0, 36, 0, 1, 64, 0, 0, 255, 255]))
        .is_err());
    current.shutdown(Shutdown::Both).unwrap();
    drop(current);
    server.wait_online(0);

    let saved: Value = serde_json::from_slice(&fs::read(&inventory_path).unwrap()).unwrap();
    assert_eq!(saved["slots"][36]["id"], 4);
    assert_eq!(saved["slots"][36]["count"], 7);
}

#[test]
fn add004_protocol_version_and_255_character_hostname_boundaries() {
    let server = Server::launch();
    // Both sides of Protocol 5 must fail closed as a Login Disconnect,
    // without admitting a player or preventing a following valid login.
    for version in [4, 6] {
        let mut socket = server.connect();
        socket
            .write_all(&handshake_version(server.port, "localhost", 2, version))
            .unwrap();
        let (id, data) = receive(&mut socket);
        assert_eq!(id, 0, "wrong protocol must yield Login Disconnect");
        let mut reason = Cursor::new(&data);
        assert!(reason.read_string(512).unwrap().contains("1.7.10"));
        reason.finish().unwrap();
        must_close(socket);
        assert_eq!(server.status_online(), 0);
    }

    // The vanilla 255-character Handshake hostname limit is inclusive.
    let mut status = server.connect();
    status
        .write_all(&handshake(server.port, &"h".repeat(255), 1))
        .unwrap();
    status.write_all(&frame(0, &[])).unwrap();
    assert_eq!(receive(&mut status).0, 0);
    drop(status);

    // A valid 16-character Login name must still be admitted after all
    // invalid connections. The 17-character rejection is checked below.
    let valid_login = server.login("LongName12345678");
    server.wait_online(1);
    drop(valid_login);
    server.wait_online(0);
}

#[test]
fn malformed_and_fragmented_protocol5_connections_are_isolated() {
    let server = Server::launch();
    assert_eq!(server.status_online(), 0);

    // #425 ADD-001: 6-byte VarInt must not allocate a payload.
    bad_handshake(&server, &[0x80, 0x80, 0x80, 0x80, 0x80, 0x00]);
    // #426 ADD-002: zero, negative and over-limit framed payloads.
    bad_handshake(&server, &[0]);
    bad_handshake(&server, &[0xff, 0xff, 0xff, 0xff, 0x0f]);
    bad_handshake(&server, &[0x81, 0x80, 0x40]); // 1 MiB + 1 bytes

    // #427 ADD-003: invalid multibyte Login name is a connection error.
    bad_login(&server, &frame(0, &[2, 0xc3, 0x28]));
    // Truncated UTF-8 on the declared Login username payload.
    bad_login(&server, &frame(0, &[3, 0xe3, 0x81]));
    // #428 ADD-004: too-long host and too-long Login username.
    bad_handshake(&server, &handshake(server.port, &"h".repeat(256), 2));
    bad_login(&server, &frame(0, &mc_string(&"X".repeat(17))));

    // #429 ADD-005: a Play packet cannot run in Status or Login state.
    let mut status = server.connect();
    status
        .write_all(&handshake(server.port, "localhost", 1))
        .unwrap();
    status.write_all(&frame(4, &[0; 33])).unwrap();
    must_close(status);
    bad_login(&server, &frame(4, &[0; 33]));
    // #430 ADD-006: a modern compressed-style packet is not Protocol 5.
    let mut status = server.connect();
    status
        .write_all(&handshake(server.port, "localhost", 1))
        .unwrap();
    status.write_all(&frame(0, &[0])).unwrap(); // extra data-length field
    must_close(status);

    // #434 ADD-010: one-byte TCP writes and back-to-back frames both login.
    let mut split = server.connect();
    for byte in handshake(server.port, "localhost", 2) {
        split.write_all(&[byte]).unwrap();
    }
    for byte in frame(0, &mc_string("LongName12345678")) {
        split.write_all(&[byte]).unwrap();
    }
    assert_eq!(receive(&mut split).0, 2);
    server.wait_online(1);
    drop(split);
    server.wait_online(0);

    let mut coalesced = server.connect();
    let mut bytes = handshake(server.port, "localhost", 2);
    bytes.extend_from_slice(&frame(0, &mc_string("RustTester")));
    coalesced.write_all(&bytes).unwrap();
    assert_eq!(receive(&mut coalesced).0, 2);
    server.wait_online(1);
    drop(coalesced);
    server.wait_online(0);

    // #432 ADD-008: non-finite world coordinates and rotations are fatal.
    let mut invalid_position = server.login("RustTester");
    let mut pos = Vec::new();
    for n in [f64::NAN, 65.0, 64.0, 0.5] {
        pos.extend_from_slice(&n.to_be_bytes());
    }
    pos.push(1); // onGround
    invalid_position.write_all(&frame(4, &pos)).unwrap();
    must_close(invalid_position);
    server.wait_online(0);

    let mut invalid_look = server.login("RustTester");
    let mut look = f32::INFINITY.to_be_bytes().to_vec();
    look.extend_from_slice(&0.0f32.to_be_bytes());
    look.push(1); // onGround
    invalid_look.write_all(&frame(5, &look)).unwrap();
    must_close(invalid_look);
    server.wait_online(0);

    // #433 ADD-009: unsolicited nonzero challenge responses are forbidden.
    let mut replay = server.login("RustTester");
    replay
        .write_all(&frame(0, &0x12345678i32.to_be_bytes()))
        .unwrap();
    must_close(replay);
    server.wait_online(0);

    // Valid clients continue to authenticate and remain unaffected.
    let final_login = server.login("RustTester");
    server.wait_online(1);
    // Graceful TCP half-close allows server to retire this one session.
    final_login.shutdown(Shutdown::Both).unwrap();
    server.wait_online(0);
}

#[test]
fn vanilla_1710_terrain_loading_keepalives_preserve_challenge_validation() {
    let server = Server::launch();
    let mut socket = server.login("RustTester");
    server.wait_online(1);

    // GuiDownloadTerrain in vanilla 1.7.10 sends new C00PacketKeepAlive()
    // (the default ID 0), even before receiving any S00 challenge.
    // These packets are not a heartbeat reply and must not disconnect.
    for _ in 0..3 {
        socket.write_all(&frame(0, &0i32.to_be_bytes())).unwrap();
        server.wait_online(1);
    }

    // Wait for a real server-issued challenge (about 10 seconds); drain
    // normal Play packets while reading from the same Protocol 5 TCP stream.
    socket
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let challenge = loop {
        assert!(Instant::now() < deadline, "no server KeepAlive challenge");
        let (id, payload) = receive(&mut socket);
        if id == 0 {
            break payload;
        }
    };
    assert_eq!(challenge.len(), 4);
    assert_ne!(challenge, 0i32.to_be_bytes());

    // The loading screen may continue sending ID 0 after a challenge.
    // Only an exact matching reply can acknowledge the challenge.
    socket.write_all(&frame(0, &0i32.to_be_bytes())).unwrap();
    socket.write_all(&frame(0, &challenge)).unwrap();
    server.wait_online(1);

    // A second answer to the same challenge is a replay and is rejected.
    socket.write_all(&frame(0, &challenge)).unwrap();
    must_close(socket);
    server.wait_online(0);
}

#[test]
fn keepalive_challenge_allows_full_response_timeout() {
    let server = Server::launch();
    let mut socket = server.login("RustTester");
    server.wait_online(1);
    socket
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let challenge = loop {
        assert!(Instant::now() < deadline, "no server KeepAlive challenge");
        let (id, payload) = receive(&mut socket);
        if id == 0 {
            break payload;
        }
    };

    // Keep ordinary player traffic flowing while delaying the answer. A
    // challenge sent ten seconds after the last pong must still have the full
    // 40-second reply window; the previous check expired after ~30 s.
    for _ in 0..32 {
        socket.write_all(&frame(3, &[1])).unwrap();
        thread::sleep(Duration::from_secs(1));
    }
    assert_eq!(server.status_online(), 1, "challenge expired too early");
    socket.write_all(&frame(0, &challenge)).unwrap();

    // A new challenge proves the late response was actually accepted, rather
    // than merely leaving the session open until the original timeout.
    let next_deadline = Instant::now() + Duration::from_secs(15);
    let next_challenge = loop {
        assert!(
            Instant::now() < next_deadline,
            "late reply was not accepted"
        );
        let (id, payload) = receive(&mut socket);
        if id == 0 {
            break payload;
        }
    };
    assert_ne!(next_challenge, challenge);
    socket.write_all(&frame(0, &next_challenge)).unwrap();
    assert_eq!(server.status_online(), 1);
}
