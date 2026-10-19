//! Protocol 5 end-to-end tests written entirely in Rust.
//! Launches the native IndustrialRust1710 binary, never Java or Python.
use flate2::read::ZlibDecoder;
use legacy_protocol::{read_frame, read_varint, write_string, write_varint, Cursor};
use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_PACKETS: usize = 160;
struct NativeServer {
    process: Child,
    directory: PathBuf,
    port: u16,
}
impl NativeServer {
    fn start() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "ir1710-rust-protocol5-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let whitelist = json!([
            {"name":"RustTester"}, {"name":"Positioner"},
            {"name":"TwinAlpha"}, {"name":"TwinBeta"}, {"name":"ChunkScout"}
        ]);
        fs::write(
            directory.join("whitelist.json"),
            serde_json::to_vec(&whitelist).unwrap(),
        )
        .unwrap();
        let reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);
        let process = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .args(["--bind", &format!("127.0.0.1:{port}")])
            .arg("--whitelist")
            .arg(directory.join("whitelist.json"))
            .arg("--world")
            .arg(directory.join("flat.irw"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut server = Self {
            process,
            directory,
            port,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "native server did not start");
            assert!(
                server.process.try_wait().unwrap().is_none(),
                "native server exited"
            );
            thread::sleep(Duration::from_millis(50));
        }
        server
    }
    fn client(&self) -> TcpStream {
        let addr = SocketAddr::from(([127, 0, 0, 1], self.port));
        let socket = TcpStream::connect_timeout(&addr, Duration::from_secs(3)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(8)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(8)))
            .unwrap();
        socket.set_nodelay(true).unwrap();
        socket
    }
    fn status(&self) -> Value {
        let mut socket = self.client();
        socket.write_all(&handshake(self.port, 1)).unwrap();
        socket.write_all(&frame(0, &[])).unwrap();
        let (packet_id, body) = receive(&mut socket);
        assert_eq!(packet_id, 0);
        let mut cursor = Cursor::new(&body);
        let status = serde_json::from_str::<Value>(cursor.read_string(65536).unwrap()).unwrap();
        cursor.finish().unwrap();
        status
    }
    fn wait_idle(&self) {
        let deadline = Instant::now() + Duration::from_secs(9);
        while self.status()["players"]["online"].as_u64() != Some(0) {
            assert!(Instant::now() < deadline, "server never released session");
            thread::sleep(Duration::from_millis(40));
        }
    }
    fn connect(&self, name: &str) -> TcpStream {
        let mut socket = self.client();
        socket.write_all(&handshake(self.port, 2)).unwrap();
        socket.write_all(&frame(0, &mc_string(name))).unwrap();
        socket
    }
    fn join(&self, name: &str) -> (TcpStream, i32, [f64; 3]) {
        let mut socket = self.connect(name);
        let (pid, body) = receive(&mut socket);
        assert_eq!(pid, 2, "login denied for {name}: {body:?}");
        let mut cursor = Cursor::new(&body);
        assert_eq!(cursor.read_string(40).unwrap().len(), 36);
        assert_eq!(cursor.read_string(16).unwrap(), name);
        cursor.finish().unwrap();
        let mut entity_id = None;
        let mut position = None;
        let mut chunks = 0;
        for _ in 0..MAX_PACKETS {
            let (pid, payload) = receive(&mut socket);
            if pid == 1 {
                entity_id = Some(i32::from_be_bytes(payload[..4].try_into().unwrap()));
                assert_eq!(payload[4], 1); // Creative mode, still a PoC.
            } else if pid == 0x21 {
                let sections = u16::from_be_bytes(payload[9..11].try_into().unwrap());
                assert_eq!(sections & 0x18, 0x18);
                let compressed = i32::from_be_bytes(payload[13..17].try_into().unwrap());
                assert!(compressed > 0);
                let end = 17 + compressed as usize;
                assert_eq!(end, payload.len());
                let mut decoded = Vec::new();
                ZlibDecoder::new(&payload[17..end])
                    .read_to_end(&mut decoded)
                    .unwrap();
                assert_eq!(decoded.len(), sections.count_ones() as usize * 10240 + 256);
                chunks += 1;
            } else if pid == 8 {
                position = Some(read_position(&payload));
                break;
            }
        }
        assert_eq!(chunks, 25, "initial viewport should contain 25 chunks");
        (
            socket,
            entity_id.expect("missing Join Game"),
            position.expect("missing position"),
        )
    }
}
impl Drop for NativeServer {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
        let _ = fs::remove_dir_all(&self.directory);
    }
}
fn vi(value: u32) -> Vec<u8> {
    let mut v = Vec::new();
    write_varint(value, &mut v);
    v
}
fn mc_string(value: &str) -> Vec<u8> {
    let mut result = Vec::new();
    write_string(value, &mut result).unwrap();
    result
}
fn frame(pid: u32, data: &[u8]) -> Vec<u8> {
    let mut packet = vi(pid);
    packet.extend_from_slice(data);
    let mut message = vi(packet.len() as u32);
    message.extend_from_slice(&packet);
    message
}
fn handshake(port: u16, state: u32) -> Vec<u8> {
    let mut payload = vi(5);
    payload.extend_from_slice(&mc_string("localhost"));
    payload.extend_from_slice(&port.to_be_bytes());
    payload.extend_from_slice(&vi(state));
    frame(0, &payload)
}
fn receive(socket: &mut TcpStream) -> (u32, Vec<u8>) {
    let packet = read_frame(socket).expect("read Protocol 5 frame");
    let mut raw = packet.as_slice();
    let pid = read_varint(&mut raw).expect("packet id");
    (pid, raw.to_vec())
}
fn read_position(packet: &[u8]) -> [f64; 3] {
    [
        f64::from_be_bytes(packet[0..8].try_into().unwrap()),
        f64::from_be_bytes(packet[8..16].try_into().unwrap()),
        f64::from_be_bytes(packet[16..24].try_into().unwrap()),
    ]
}
fn until_packet(socket: &mut TcpStream, id: u32, matches: impl Fn(&[u8]) -> bool) -> Vec<u8> {
    for _ in 0..MAX_PACKETS {
        let (packet_id, body) = receive(socket);
        if packet_id == id && matches(&body) {
            return body;
        }
    }
    panic!("expected packet {id:#x} not received");
}
fn await_file(path: &PathBuf) {
    let deadline = Instant::now() + Duration::from_secs(6);
    while !path.is_file() {
        assert!(
            Instant::now() < deadline,
            "file not saved: {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(40));
    }
}
fn send_position(s: &mut TcpStream, x: f64, y: f64, z: f64) {
    let mut data = Vec::new();
    for v in [x, y + 1.62, y, z] {
        data.extend_from_slice(&v.to_be_bytes());
    }
    data.push(1);
    s.write_all(&frame(4, &data)).unwrap();
}
fn send_fragments(s: &mut TcpStream, packet: &[u8], pieces: &[usize]) {
    let mut index = 0;
    for &count in pieces {
        s.write_all(&packet[index..index + count]).unwrap();
        index += count;
        thread::sleep(Duration::from_millis(120));
    }
    s.write_all(&packet[index..]).unwrap();
}

#[test]
fn native_1710_protocol5_whitelist_movement_multiplayer_and_save_roundtrip() {
    let server = NativeServer::start();
    let status = server.status();
    assert_eq!(status["version"]["protocol"], 5);
    assert_eq!(status["players"]["max"], 8);
    assert_eq!(status["players"]["online"], 0);

    let mut denied = server.connect("NotAllowed");
    let (pid, body) = receive(&mut denied);
    assert_eq!(pid, 0);
    assert!(String::from_utf8_lossy(&body).contains("whitelist"));
    drop(denied);
    server.wait_idle();

    // A split VarInt / chat payload must survive multiple 50ms polls.
    let (mut client, _, pos) = server.join("RustTester");
    assert_eq!(pos, [0.5, 65.0, 0.5]);
    let list = frame(1, &mc_string("/list"));
    send_fragments(&mut client, &list, &[1, 1, 1, 2]);
    let response = until_packet(&mut client, 2, |body| {
        body.windows(10).any(|window| window == b"Online (1)")
    });
    assert!(String::from_utf8_lossy(&response).contains("RustTester"));
    send_fragments(&mut client, &frame(1, &mc_string("/help")), &[1, 2]);
    until_packet(&mut client, 2, |body| {
        body.windows(9).any(|w| w == b"Commands:")
    });

    // Dig a grass block and place a cobblestone block through Protocol 5.
    let mut dig = vec![0u8];
    dig.extend_from_slice(&0i32.to_be_bytes());
    dig.push(63);
    dig.extend_from_slice(&0i32.to_be_bytes());
    dig.push(1);
    client.write_all(&frame(7, &dig)).unwrap();
    until_packet(&mut client, 0x23, |b| {
        b.len() >= 11
            && i32::from_be_bytes(b[..4].try_into().unwrap()) == 0
            && b[4] == 63
            && b[9] == 0
    });
    let mut place = Vec::new();
    place.extend_from_slice(&1i32.to_be_bytes());
    place.push(63);
    place.extend_from_slice(&0i32.to_be_bytes());
    place.push(1);
    place.extend_from_slice(&4i16.to_be_bytes());
    place.push(1);
    place.extend_from_slice(&0i16.to_be_bytes());
    place.extend_from_slice(&(-1i16).to_be_bytes());
    place.extend_from_slice(&[8, 8, 8]);
    client.write_all(&frame(8, &place)).unwrap();
    until_packet(&mut client, 0x23, |b| {
        b.len() >= 11
            && i32::from_be_bytes(b[..4].try_into().unwrap()) == 1
            && b[4] == 64
            && b[9] == 4
    });
    drop(client);
    server.wait_idle();
    let original_path = server.directory.join("flat.irw");
    await_file(&original_path);
    assert_eq!(&fs::read(&original_path).unwrap()[..4], b"IRW1");

    let (mut mover, _, _) = server.join("Positioner");
    send_position(&mut mover, 3.5, 65.0, 2.5);
    drop(mover);
    server.wait_idle();
    await_file(&server.directory.join("players/positioner.dat"));
    let (mut mover, _, current) = server.join("Positioner");
    assert_eq!(current, [3.5, 65.0, 2.5]);
    send_position(&mut mover, 16384.5, 65.0, 2.5);
    until_packet(&mut mover, 8, |b| read_position(b) == current);
    drop(mover);
    server.wait_idle();

    // New peers are announced with 1.7.10 named-entity packets, not modern ones.
    let (mut alice, alice_id, _) = server.join("TwinAlpha");
    let (mut bob, bob_id, _) = server.join("TwinBeta");
    assert_ne!(alice_id, bob_id);
    assert_eq!(server.status()["players"]["online"], 2);
    until_packet(&mut alice, 0x0c, |_| true);
    until_packet(&mut bob, 0x0c, |_| true);
    send_position(&mut bob, 4.5, 65.0, 4.5);
    until_packet(&mut alice, 0x18, |b| {
        b.len() > 4 && i32::from_be_bytes(b[..4].try_into().unwrap()) == bob_id
    });
    alice
        .write_all(&frame(1, &mc_string("hello from Rust")))
        .unwrap();
    until_packet(&mut bob, 2, |b| {
        b.windows(15).any(|w| w == b"hello from Rust")
    });
    drop(alice);
    until_packet(&mut bob, 0x13, |b| {
        b.len() >= 5 && i32::from_be_bytes(b[1..5].try_into().unwrap()) == alice_id
    });
    drop(bob);
    server.wait_idle();

    let (mut scout, _, _) = server.join("ChunkScout");
    send_position(&mut scout, 55.5, 65.0, 0.5);
    let (mut loaded, mut unloaded) = (0, 0);
    for _ in 0..50 {
        let (id, data) = receive(&mut scout);
        if id == 0x21 {
            let mask = u16::from_be_bytes(data[9..11].try_into().unwrap());
            if mask == 0 {
                unloaded += 1;
            } else {
                loaded += 1;
            }
        }
        if loaded == 15 && unloaded == 15 {
            break;
        }
    }
    assert_eq!((loaded, unloaded), (15, 15));
    drop(scout);
    server.wait_idle();

    // The native Anvil export and import must preserve every IRW1 byte.
    let before = fs::read(&original_path).unwrap();
    let source_dir = server.directory.join("snapshot");
    let restored = server.directory.join("restored.irw");
    let export = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
        .arg("--world")
        .arg(&original_path)
        .arg("--export-anvil")
        .arg(&source_dir)
        .output()
        .unwrap();
    assert!(
        export.status.success(),
        "Anvil export: {}",
        String::from_utf8_lossy(&export.stderr)
    );
    let import = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
        .arg("--world")
        .arg(&restored)
        .arg("--import-anvil")
        .arg(&source_dir)
        .output()
        .unwrap();
    assert!(
        import.status.success(),
        "Anvil import: {}",
        String::from_utf8_lossy(&import.stderr)
    );
    assert_eq!(fs::read(&restored).unwrap(), before);
    let retry = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
        .arg("--world")
        .arg(&restored)
        .arg("--import-anvil")
        .arg(&source_dir)
        .status()
        .unwrap();
    assert!(!retry.success(), "import overwrote an existing world");
}
