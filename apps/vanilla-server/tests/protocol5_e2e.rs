//! Protocol 5 end-to-end tests written entirely in Rust.
//! Launches the native IndustrialRust1710 binary, never Java or Python.
use flate2::read::ZlibDecoder;
use legacy_protocol::{read_frame, read_varint, write_string, write_varint, Cursor};
use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_PACKETS: usize = 160;
// These end-to-end tests reserve ephemeral ports before spawning binaries.
// Serialize reservations and process lifetime to prevent cross-test port races.
static SERVER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
struct NativeServer {
    _exclusive_test_run: std::sync::MutexGuard<'static, ()>,
    process: Child,
    directory: PathBuf,
    port: u16,
    survival: bool,
}
impl NativeServer {
    fn start() -> Self {
        Self::start_with_mode(false)
    }
    fn start_with_mode(survival: bool) -> Self {
        let exclusive = SERVER_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let mut command = Command::new(env!("CARGO_BIN_EXE_vanilla-server"));
        command
            .args(["--bind", &format!("127.0.0.1:{port}")])
            .arg("--whitelist")
            .arg(directory.join("whitelist.json"))
            .arg("--world")
            .arg(directory.join("flat.irw"));
        if survival {
            command.arg("--survival");
        }
        let process = command
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut server = Self {
            _exclusive_test_run: exclusive,
            process,
            directory,
            port,
            survival,
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
        let mut inventory_seen = false;
        let mut hotbar_seen = false;
        for _ in 0..MAX_PACKETS {
            let (pid, payload) = receive(&mut socket);
            if pid == 1 {
                entity_id = Some(i32::from_be_bytes(payload[..4].try_into().unwrap()));
                assert_eq!(payload[4], u8::from(!self.survival));
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
            } else if pid == 0x30 {
                assert_eq!(&payload[..3], &[0, 0, 45]);
                // Empty stacks take 2 bytes; populated stacks take 7 bytes.
                // After reconnect the saved Creative hotbar must be restored.
                let mut offset = 3usize;
                for _ in 0..45 {
                    assert!(offset + 2 <= payload.len());
                    let item = i16::from_be_bytes(payload[offset..offset + 2].try_into().unwrap());
                    offset += if item == -1 { 2 } else { 7 };
                }
                assert_eq!(offset, payload.len());
                inventory_seen = true;
            } else if pid == 0x09 {
                assert_eq!(payload.len(), 1);
                assert!(payload[0] <= 8, "invalid selected hotbar index");
                hotbar_seen = true;
            } else if pid == 8 {
                position = Some(read_position(&payload));
                break;
            }
        }
        assert_eq!(chunks, 25, "initial viewport should contain 25 chunks");
        assert!(inventory_seen, "missing 1.7.10 Window Items");
        assert!(hotbar_seen, "missing selected hotbar packet");
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
/// Read a Play packet while acting like a live Protocol 5 client:
/// server KeepAlive requests must be acknowledged even during long E2E tests.
#[track_caller]
fn until_packet(socket: &mut TcpStream, id: u32, matches: impl Fn(&[u8]) -> bool) -> Vec<u8> {
    let caller = std::panic::Location::caller();
    let deadline = Instant::now() + Duration::from_secs(25);
    let mut seen = 0usize;
    for _ in 0..MAX_PACKETS {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for packet {id:#x} at {caller}; received {seen} packets"
        );
        let packet = read_frame(&mut *socket).unwrap_or_else(|error| {
            panic!("waiting for packet {id:#x} at {caller}, after {seen} packets: {error}")
        });
        let mut raw = packet.as_slice();
        let packet_id = read_varint(&mut raw).expect("valid Play packet ID");
        let body = raw.to_vec();
        seen += 1;
        if packet_id == 0 && body.len() == 4 {
            // S00 KeepAlive: the same signed 32-bit identifier is returned in C00.
            socket
                .write_all(&frame(0, &body))
                .expect("reply to KeepAlive");
        }
        if packet_id == id && matches(&body) {
            return body;
        }
    }
    panic!("expected packet {id:#x} at {caller} not received after {seen} packets");
}
fn await_file(path: &Path) {
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

    // Dig a grass block. Placement is denied until the Creative action has
    // populated a server-owned hotbar slot.
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
    until_packet(&mut client, 0x2f, |b| b == [0, 0, 36, 255, 255]);
    until_packet(&mut client, 0x23, |b| {
        b.len() >= 11
            && i32::from_be_bytes(b[..4].try_into().unwrap()) == 1
            && b[4] == 64
            && b[9] == 0
    });
    // Protocol5 C10 Creative Inventory Action: window 0, slot 36, item 4 x64.
    let mut creative = 36i16.to_be_bytes().to_vec();
    creative.extend_from_slice(&[0, 4, 64, 0, 0, 255, 255]);
    client.write_all(&frame(0x10, &creative)).unwrap();
    until_packet(&mut client, 0x2f, |b| {
        b == [0, 0, 36, 0, 4, 64, 0, 0, 255, 255]
    });
    // Now the same client-held value is backed by the server's selected slot.
    client.write_all(&frame(8, &place)).unwrap();
    until_packet(&mut client, 0x23, |b| {
        b.len() >= 11
            && i32::from_be_bytes(b[..4].try_into().unwrap()) == 1
            && b[4] == 64
            && b[9] == 4
    });
    // Invalid hotbar indexes are not accepted. Valid selection of an empty
    // slot must not allow forged placement from the previous item.
    client.write_all(&frame(9, &1i16.to_be_bytes())).unwrap();
    client.write_all(&frame(8, &place)).unwrap();
    until_packet(&mut client, 0x2f, |b| b == [0, 0, 37, 255, 255]);
    drop(client);
    server.wait_idle();
    let persisted_inv = server.directory.join("players/rusttester.inventory.json");
    await_file(&persisted_inv);
    let saved_inv: Value = serde_json::from_slice(&fs::read(&persisted_inv).unwrap()).unwrap();
    assert_eq!(saved_inv["selected"], 1);
    assert_eq!(saved_inv["slots"][36]["id"], 4);
    // Rejoin: selected hotbar must remain unchanged.
    let (mut rejoined, _, _) = server.join("RustTester");
    rejoined.write_all(&frame(9, &0i16.to_be_bytes())).unwrap();
    drop(rejoined);
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
        let mut raw = b;
        read_varint(&mut raw).ok() == Some(bob_id as u32) && raw.len() == 15 // three i32 positions, yaw, pitch, onGround
    });
    alice
        .write_all(&frame(1, &mc_string("hello from Rust")))
        .unwrap();
    until_packet(&mut bob, 2, |b| {
        b.windows(15).any(|w| w == b"hello from Rust")
    });
    drop(alice);
    until_packet(&mut bob, 0x13, |b| {
        let mut raw = b;
        read_varint(&mut raw).ok() == Some(1)
            && read_varint(&mut raw).ok() == Some(alice_id as u32)
            && raw.is_empty()
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

#[test]
fn survival_mining_requires_delay_and_inventory_pays_for_placements() {
    let server = NativeServer::start_with_mode(true);
    let (mut client, _, _) = server.join("RustTester");

    // No prior inventory and empty hotbar: Creative action must not mint items.
    let mut creative = 36i16.to_be_bytes().to_vec();
    creative.extend_from_slice(&[0, 4, 64, 0, 0, 255, 255]);
    client.write_all(&frame(0x10, &creative)).unwrap();
    until_packet(&mut client, 0x30, |data| data.len() == 93);

    let mut dig = vec![0];
    dig.extend_from_slice(&0i32.to_be_bytes());
    dig.push(63);
    dig.extend_from_slice(&0i32.to_be_bytes());
    dig.push(1);
    let mut finish = dig.clone();
    finish[0] = 2;
    // A forged immediate completion without starting must be rejected.
    client.write_all(&frame(7, &finish)).unwrap();
    until_packet(&mut client, 0x23, |b| {
        b.len() >= 11 && b[4] == 63 && b[9] == 2
    });
    client.write_all(&frame(7, &dig)).unwrap();
    client.write_all(&frame(7, &finish)).unwrap();
    until_packet(&mut client, 0x23, |b| {
        b.len() >= 11 && b[4] == 63 && b[9] == 2
    });

    // Mine from within reach but outside the pickup radius. The drop must
    // exist on the ground before a player actually moves within pickup range.
    send_position(&mut client, 4.5, 65.0, 0.5);
    client.write_all(&frame(7, &dig)).unwrap();
    thread::sleep(Duration::from_millis(900));
    client.write_all(&frame(7, &finish)).unwrap();
    let spawn = until_packet(&mut client, 0x0e, |b| b.len() == 24 && b[5] == 2);
    let mut entity_bytes = spawn.as_slice();
    let drop_id = read_varint(&mut entity_bytes).unwrap() as i32;
    assert!(drop_id > 1_000_000_000);
    let metadata = until_packet(&mut client, 0x1c, |b| {
        b.len() >= 13 && b[..4] == drop_id.to_be_bytes()
    });
    assert_eq!(&metadata[4..], &[0xaa, 0, 3, 1, 0, 0, 255, 255, 127]);
    until_packet(&mut client, 0x23, |b| {
        b.len() >= 11 && b[4] == 63 && b[9] == 0
    });
    let drop_path = server.directory.join("flat.drops.json");
    await_file(&drop_path);
    let drop_file: Value = serde_json::from_slice(&fs::read(&drop_path).unwrap()).unwrap();
    assert_eq!(drop_file["entries"].as_object().unwrap().len(), 1);

    // A late-joining player must also see this persistent item in the world.
    let (late, _, _) = server.join("TwinAlpha");
    drop(late);

    thread::sleep(Duration::from_millis(900));
    send_position(&mut client, 0.5, 65.0, 0.5);
    until_packet(&mut client, 0x2f, |b| {
        b == [0, 0, 36, 0, 3, 1, 0, 0, 255, 255]
    });
    until_packet(&mut client, 0x0d, |b| {
        let mut raw = b;
        read_varint(&mut raw).ok() == Some(drop_id as u32)
            && read_varint(&mut raw).is_ok()
            && raw.is_empty()
    });
    until_packet(&mut client, 0x13, |b| {
        let mut raw = b;
        read_varint(&mut raw).ok() == Some(1)
            && read_varint(&mut raw).ok() == Some(drop_id as u32)
            && raw.is_empty()
    });
    let pickup_file: Value = serde_json::from_slice(&fs::read(&drop_path).unwrap()).unwrap();
    assert!(pickup_file["entries"].as_object().unwrap().is_empty());

    // Client claims more than it owns; the server permits one placed dirt but
    // consumes its own authoritative inventory count.
    let mut placed = vec![];
    placed.extend_from_slice(&1i32.to_be_bytes());
    placed.push(63);
    placed.extend_from_slice(&0i32.to_be_bytes());
    placed.push(1);
    placed.extend_from_slice(&3i16.to_be_bytes());
    placed.push(64);
    placed.extend_from_slice(&0i16.to_be_bytes());
    placed.extend_from_slice(&(-1i16).to_be_bytes());
    placed.extend_from_slice(&[8, 8, 8]);
    client.write_all(&frame(8, &placed)).unwrap();
    // Successful placement sends the full server-owned inventory (S30),
    // because consuming a bucket can also return an empty bucket elsewhere.
    // The selected hotbar slot 36 must now be empty.
    until_packet(&mut client, 0x30, |b| {
        b.len() >= 77 && b[..3] == [0, 0, 45] && b[75..77] == [255, 255]
    });
    until_packet(&mut client, 0x23, |b| {
        b.len() >= 11 && b[4] == 64 && b[9] == 3
    });
    // Repeating the placement with an empty hotbar must be refused.
    placed[3] = 2; // still a valid, unedited target x=2
    client.write_all(&frame(8, &placed)).unwrap();
    until_packet(&mut client, 0x2f, |b| b == [0, 0, 36, 255, 255]);
    until_packet(&mut client, 0x23, |b| {
        b.len() >= 11 && b[4] == 64 && b[9] == 0
    });

    drop(client);
    server.wait_idle();
    let path = server
        .directory
        .join("players/rusttester.survival.inventory.json");
    await_file(&path);
    let value: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert!(value["slots"][36].is_null());
    // Old Creative inventory never becomes Survival inventory.
    assert!(!server
        .directory
        .join("players/rusttester.inventory.json")
        .exists());
    let (rejoined, _, _) = server.join("RustTester");
    drop(rejoined);
    server.wait_idle();
}

#[test]
fn native_click_window_is_authoritative_and_persists_carried_stack() {
    let server = NativeServer::start();
    let (mut client, _, _) = server.join("RustTester");

    // Create one Creative stack through the existing authoritative action.
    let mut creative = 36i16.to_be_bytes().to_vec();
    creative.extend_from_slice(&[0, 3, 5, 0, 0, 255, 255]);
    client.write_all(&frame(0x10, &creative)).unwrap();
    until_packet(&mut client, 0x2f, |p| {
        p == [0, 0, 36, 0, 3, 5, 0, 0, 255, 255]
    });

    let click = |slot: i16, action: i16, button: i8, mode: i8, claimed: &[u8]| {
        let mut body = vec![0]; // window 0
        body.extend_from_slice(&slot.to_be_bytes());
        body.push(button as u8);
        body.extend_from_slice(&action.to_be_bytes());
        body.push(mode as u8);
        body.extend_from_slice(claimed);
        frame(0x0e, &body)
    };

    // Left click takes all five items; the clicked-item claim is ignored.
    client
        .write_all(&click(36, 7, 0, 0, &(-1i16).to_be_bytes()))
        .unwrap();
    until_packet(&mut client, 0x32, |p| p == [0, 0, 7, 1]);
    let items = until_packet(&mut client, 0x30, |_| true);
    assert_eq!(&items[3 + 36 * 2..3 + 36 * 2 + 2], &(-1i16).to_be_bytes());
    until_packet(&mut client, 0x2f, |p| {
        p == [255, 255, 255, 0, 3, 5, 0, 0, 255, 255]
    });

    // Unsupported drag must fail even when the client claims a valuable item.
    let forged = [0, 57, 64, 0, 0, 255, 255];
    client.write_all(&click(10, 8, 0, 5, &forged)).unwrap();
    until_packet(&mut client, 0x32, |p| p == [0, 0, 8, 0]);
    until_packet(&mut client, 0x30, |_| true);
    until_packet(&mut client, 0x2f, |p| {
        p == [255, 255, 255, 0, 3, 5, 0, 0, 255, 255]
    });
    // Clientbound rejection acknowledgement (serverbound 0x0F).
    client.write_all(&frame(0x0f, &[0, 0, 8, 0])).unwrap();

    // Right click places exactly one item, preserving four in the cursor.
    client
        .write_all(&click(10, 9, 1, 0, &(-1i16).to_be_bytes()))
        .unwrap();
    until_packet(&mut client, 0x32, |p| p == [0, 0, 9, 1]);
    let items = until_packet(&mut client, 0x30, |_| true);
    assert_eq!(
        &items[3 + 10 * 2..3 + 10 * 2 + 7],
        &[0, 3, 1, 0, 0, 255, 255]
    );
    until_packet(&mut client, 0x2f, |p| {
        p == [255, 255, 255, 0, 3, 4, 0, 0, 255, 255]
    });
    drop(client);
    server.wait_idle();

    let saved = server.directory.join("players/rusttester.inventory.json");
    await_file(&saved);
    let value: Value = serde_json::from_slice(&fs::read(saved).unwrap()).unwrap();
    assert_eq!(value["slots"][10]["count"], 1);
    assert_eq!(value["cursor"]["count"], 4);
}

#[test]
fn native_protocol5_crafting_inventory_and_workbench_gui_roundtrip() {
    let server = NativeServer::start();
    let (mut client, _, _) = server.join("RustTester");
    let creative = |slot: i16, id: i16, amount: u8| {
        let mut payload = slot.to_be_bytes().to_vec();
        payload.extend_from_slice(&id.to_be_bytes());
        payload.push(amount);
        payload.extend_from_slice(&0i16.to_be_bytes());
        payload.extend_from_slice(&(-1i16).to_be_bytes());
        frame(0x10, &payload)
    };
    let click = |client: &mut TcpStream, window: u8, slot: i16, action: i16, button: i8| {
        let mut payload = vec![window];
        payload.extend_from_slice(&slot.to_be_bytes());
        payload.push(button as u8);
        payload.extend_from_slice(&action.to_be_bytes());
        payload.push(0); // normal click
        payload.extend_from_slice(&(-1i16).to_be_bytes()); // empty client claim
        client.write_all(&frame(0x0e, &payload)).unwrap();
        until_packet(client, 0x32, |p| {
            p == [window, action.to_be_bytes()[0], action.to_be_bytes()[1], 1]
        });
        let items = until_packet(client, 0x30, |p| p[0] == window);
        until_packet(client, 0x2f, |p| p.starts_with(&[255, 255, 255]));
        items
    };
    client.write_all(&creative(36, 17, 1)).unwrap();
    until_packet(&mut client, 0x2f, |p| p.starts_with(&[0, 0, 36]));
    click(&mut client, 0, 36, 1, 0);
    let grid = click(&mut client, 0, 1, 2, 0);
    assert_eq!(&grid[3..10], &[0, 5, 4, 0, 0, 255, 255]); // four planks
    click(&mut client, 0, 0, 3, 0); // consume the log
    for (slot, action) in (1..=4).zip(4..=7) {
        click(&mut client, 0, slot, action, 1);
    }
    let table = click(&mut client, 0, 0, 8, 0);
    assert_eq!(&table[3..5], &(-1i16).to_be_bytes()); // ingredients spent
    click(&mut client, 0, 36, 9, 0); // hold crafted table in hotbar

    // Place the crafting table atop the default grass and open its 3x3 GUI.
    let mut place = 0i32.to_be_bytes().to_vec();
    place.push(63);
    place.extend_from_slice(&0i32.to_be_bytes());
    place.push(1); // top face
    place.extend_from_slice(&58i16.to_be_bytes());
    place.push(1);
    place.extend_from_slice(&0i16.to_be_bytes());
    place.extend_from_slice(&(-1i16).to_be_bytes());
    place.extend_from_slice(&[8, 8, 8]);
    client.write_all(&frame(8, &place)).unwrap();
    until_packet(&mut client, 0x23, |p| {
        p.len() >= 11 && p[4] == 64 && p[9] == 58
    });
    let mut use_table = 0i32.to_be_bytes().to_vec();
    use_table.push(64);
    use_table.extend_from_slice(&0i32.to_be_bytes());
    use_table.push(1);
    use_table.extend_from_slice(&(-1i16).to_be_bytes());
    use_table.extend_from_slice(&[8, 8, 8]);
    client.write_all(&frame(8, &use_table)).unwrap();
    let opened = until_packet(&mut client, 0x2d, |p| p.first() == Some(&1));
    assert!(opened
        .windows("minecraft:crafting_table".len())
        .any(|w| w == b"minecraft:crafting_table"));
    until_packet(&mut client, 0x30, |p| p.starts_with(&[1, 0, 46]));
    until_packet(&mut client, 0x2f, |p| p.starts_with(&[255, 255, 255]));

    // Server-owned 3x3 recipe for a wooden pickaxe: three planks and two sticks.
    client.write_all(&creative(36, 5, 3)).unwrap();
    until_packet(&mut client, 0x2f, |p| p.starts_with(&[0, 0, 36]));
    client.write_all(&creative(37, 280, 2)).unwrap();
    until_packet(&mut client, 0x2f, |p| p.starts_with(&[0, 0, 37]));
    let mut action = 10i16;
    let mut do_click = |slot: i16, button: i8| {
        let result = click(&mut client, 1, slot, action, button);
        action += 1;
        result
    };
    do_click(37, 0); // planks: 3
    do_click(1, 1);
    do_click(2, 1);
    do_click(3, 1);
    do_click(38, 0); // sticks: 2
    do_click(5, 1);
    let items = do_click(8, 1);
    assert_eq!(&items[3..10], &[1, 14, 1, 0, 0, 255, 255]); // wooden pickaxe ID 270
    let crafted = do_click(0, 0);
    assert_eq!(&crafted[3..5], &(-1i16).to_be_bytes()); // recipe consumed
    client.write_all(&frame(13, &[1])).unwrap();
    drop(client);
    server.wait_idle();
    let save = server.directory.join("players/rusttester.inventory.json");
    await_file(&save);
    let saved: Value = serde_json::from_slice(&fs::read(save).unwrap()).unwrap();
    assert_eq!(saved["cursor"]["id"], 270);
    assert!(saved["workbench"]
        .as_array()
        .unwrap()
        .iter()
        .all(Value::is_null));
}

#[test]
fn native_single_chest_shared_storage_and_protocol5_gui() {
    let server = NativeServer::start();
    let (mut alice, _, _) = server.join("RustTester");

    // Authoritative Creative chest placement on the default flat grass.
    let creative = |slot: i16, id: i16, count: u8| {
        let mut p = slot.to_be_bytes().to_vec();
        p.extend_from_slice(&id.to_be_bytes());
        p.push(count);
        p.extend_from_slice(&0i16.to_be_bytes());
        p.extend_from_slice(&(-1i16).to_be_bytes());
        frame(0x10, &p)
    };
    alice.write_all(&creative(36, 54, 1)).unwrap();
    until_packet(&mut alice, 0x2f, |p| p.starts_with(&[0, 0, 36]));
    let mut place = 0i32.to_be_bytes().to_vec();
    place.push(63);
    place.extend_from_slice(&0i32.to_be_bytes());
    place.push(1);
    place.extend_from_slice(&54i16.to_be_bytes());
    place.push(1);
    place.extend_from_slice(&0i16.to_be_bytes());
    place.extend_from_slice(&(-1i16).to_be_bytes());
    place.extend_from_slice(&[8, 8, 8]);
    alice.write_all(&frame(8, &place)).unwrap();
    until_packet(&mut alice, 0x23, |p| {
        p.len() >= 11 && p[4] == 64 && p[9] == 54
    });

    let mut open = 0i32.to_be_bytes().to_vec();
    open.push(64);
    open.extend_from_slice(&0i32.to_be_bytes());
    open.push(1);
    open.extend_from_slice(&(-1i16).to_be_bytes());
    open.extend_from_slice(&[8, 8, 8]);
    alice.write_all(&frame(8, &open)).unwrap();
    let opened = until_packet(&mut alice, 0x2d, |p| p.first() == Some(&1));
    assert_eq!(*opened.last().unwrap(), 27);
    assert!(opened.windows(15).any(|part| part == b"minecraft:chest"));
    until_packet(&mut alice, 0x30, |p| p.starts_with(&[1, 0, 63]));
    until_packet(&mut alice, 0x2f, |p| p.starts_with(&[255, 255, 255]));

    let click = |client: &mut TcpStream, window: u8, slot: i16, action: i16, mode: i8| {
        let mut p = vec![window];
        p.extend_from_slice(&slot.to_be_bytes());
        p.push(0); // left click
        p.extend_from_slice(&action.to_be_bytes());
        p.push(mode as u8);
        p.extend_from_slice(&(-1i16).to_be_bytes()); // untrusted claim
        client.write_all(&frame(0x0e, &p)).unwrap();
        until_packet(client, 0x32, |b| {
            b == [window, action.to_be_bytes()[0], action.to_be_bytes()[1], 1]
        });
        let items = until_packet(client, 0x30, |b| b.first() == Some(&window));
        until_packet(client, 0x2f, |b| b.starts_with(&[255, 255, 255]));
        items
    };
    // Player hotbar slot 37 maps to chest window slot 55.
    alice.write_all(&creative(37, 4, 5)).unwrap();
    until_packet(&mut alice, 0x2f, |p| p.starts_with(&[0, 0, 37]));
    click(&mut alice, 1, 55, 1, 0);
    let contents = click(&mut alice, 1, 0, 2, 0);
    assert_eq!(&contents[3..10], &[0, 4, 5, 0, 0, 255, 255]);

    let stored = server.directory.join("flat.chests.json");
    await_file(&stored);
    let snapshot: Value = serde_json::from_slice(&fs::read(&stored).unwrap()).unwrap();
    assert_eq!(snapshot["entries"]["0,64,0"][0]["count"], 5);

    let (mut bob, _, _) = server.join("TwinAlpha");
    bob.write_all(&frame(8, &open)).unwrap();
    let second = until_packet(&mut bob, 0x2d, |p| p.first() == Some(&1));
    assert_eq!(*second.last().unwrap(), 27);
    let second_items = until_packet(&mut bob, 0x30, |p| p.starts_with(&[1, 0, 63]));
    assert_eq!(&second_items[3..10], &[0, 4, 5, 0, 0, 255, 255]);
    until_packet(&mut bob, 0x2f, |p| p.starts_with(&[255, 255, 255]));

    // A shift click transfers one stack, no duplication.
    let moved = click(&mut bob, 1, 0, 1, 1);
    assert_eq!(&moved[3..5], &(-1i16).to_be_bytes());
    let alice_updated = until_packet(&mut alice, 0x30, |p| p.starts_with(&[1, 0, 63]));
    assert_eq!(&alice_updated[3..5], &(-1i16).to_be_bytes());

    bob.write_all(&frame(13, &[1])).unwrap();
    alice.write_all(&frame(13, &[1])).unwrap();
    drop(bob);
    drop(alice);
    server.wait_idle();
    let empty: Value = serde_json::from_slice(&fs::read(stored).unwrap()).unwrap();
    assert!(empty["entries"].as_object().unwrap().is_empty());
}

#[test]
fn native_furnace_protocol5_gui_fuel_and_20tps_smelting() {
    let server = NativeServer::start();
    let (mut alice, _, _) = server.join("RustTester");
    let creative = |slot: i16, id: i16, count: u8| {
        let mut bytes = slot.to_be_bytes().to_vec();
        bytes.extend_from_slice(&id.to_be_bytes());
        bytes.push(count);
        bytes.extend_from_slice(&0i16.to_be_bytes());
        bytes.extend_from_slice(&(-1i16).to_be_bytes());
        frame(0x10, &bytes)
    };
    // Place an unlit furnace above the grass at the origin.
    alice.write_all(&creative(36, 61, 1)).unwrap();
    until_packet(&mut alice, 0x2f, |p| p.starts_with(&[0, 0, 36]));
    let mut place = 0i32.to_be_bytes().to_vec();
    place.push(63);
    place.extend_from_slice(&0i32.to_be_bytes());
    place.push(1);
    place.extend_from_slice(&61i16.to_be_bytes());
    place.push(1);
    place.extend_from_slice(&0i16.to_be_bytes());
    place.extend_from_slice(&(-1i16).to_be_bytes());
    place.extend_from_slice(&[8, 8, 8]);
    alice.write_all(&frame(8, &place)).unwrap();
    until_packet(&mut alice, 0x23, |p| {
        p.len() >= 11 && p[4] == 64 && p[9] == 61
    });
    let mut open = 0i32.to_be_bytes().to_vec();
    open.push(64);
    open.extend_from_slice(&0i32.to_be_bytes());
    open.push(1);
    open.extend_from_slice(&(-1i16).to_be_bytes());
    open.extend_from_slice(&[8, 8, 8]);
    alice.write_all(&frame(8, &open)).unwrap();
    let opened = until_packet(&mut alice, 0x2d, |p| p.first() == Some(&1));
    assert_eq!(opened.last(), Some(&3));
    assert!(opened
        .windows(b"minecraft:furnace".len())
        .any(|s| s == b"minecraft:furnace"));
    until_packet(&mut alice, 0x30, |p| p.starts_with(&[1, 0, 39]));
    for property in 0..3u8 {
        until_packet(&mut alice, 0x31, |p| {
            p.len() == 5 && p[0] == 1 && p[2] == property
        });
    }
    until_packet(&mut alice, 0x2f, |p| p.starts_with(&[255, 255, 255]));
    alice.write_all(&creative(37, 15, 1)).unwrap();
    until_packet(&mut alice, 0x2f, |p| p.starts_with(&[0, 0, 37]));
    alice.write_all(&creative(38, 263, 1)).unwrap();
    until_packet(&mut alice, 0x2f, |p| p.starts_with(&[0, 0, 38]));

    let shift = |client: &mut TcpStream, slot: i16, action: i16| {
        let mut data = vec![1];
        data.extend_from_slice(&slot.to_be_bytes());
        data.push(0);
        data.extend_from_slice(&action.to_be_bytes());
        data.push(1); // shift-click
        data.extend_from_slice(&(-1i16).to_be_bytes());
        client.write_all(&frame(0x0e, &data)).unwrap();
        until_packet(client, 0x32, |p| {
            p == [1, action.to_be_bytes()[0], action.to_be_bytes()[1], 1]
        });
    };
    shift(&mut alice, 31, 1); // input from hotbar 37
    until_packet(&mut alice, 0x30, |p| p.starts_with(&[1, 0, 39]));
    // Some properties may be queued before the cursor Set Slot packet.
    until_packet(&mut alice, 0x2f, |p| p.starts_with(&[255, 255, 255]));
    shift(&mut alice, 32, 2); // coal from hotbar 38
    until_packet(&mut alice, 0x30, |p| p.starts_with(&[1, 0, 39]));
    until_packet(&mut alice, 0x2f, |p| p.starts_with(&[255, 255, 255]));

    let saved_path = server.directory.join("flat.furnaces.json");
    await_file(&saved_path);
    let deadline = Instant::now() + Duration::from_secs(16);
    let completed = loop {
        let document: Value = serde_json::from_slice(&fs::read(&saved_path).unwrap()).unwrap();
        let furnace = &document["entries"]["0,64,0"];
        if furnace["slots"][2]["id"] == 265 {
            break furnace.clone();
        }
        assert!(
            Instant::now() < deadline,
            "200-tick iron smelting did not finish"
        );
        thread::sleep(Duration::from_millis(70));
    };
    assert!(
        completed["slots"][0].is_null(),
        "one ore should be consumed"
    );
    assert!(
        completed["slots"][1].is_null(),
        "one coal should be consumed"
    );
    assert_eq!(completed["slots"][2]["count"], 1);
    assert_eq!(completed["burn_total"], 1600);
    assert!(completed["burn_left"].as_u64().unwrap() > 0);
    // A second viewer gets the existing result and current progress properties.
    let (mut bob, _, _) = server.join("TwinAlpha");
    bob.write_all(&frame(8, &open)).unwrap();
    until_packet(&mut bob, 0x2d, |p| p.first() == Some(&1));
    let items = until_packet(&mut bob, 0x30, |p| p.starts_with(&[1, 0, 39]));
    assert!(items.windows(7).any(|s| s == [1, 9, 1, 0, 0, 255, 255]));
    for property in 0..3u8 {
        until_packet(&mut bob, 0x31, |p| {
            p.len() == 5 && p[0] == 1 && p[2] == property
        });
    }
    until_packet(&mut bob, 0x2f, |p| p.starts_with(&[255, 255, 255]));
    shift(&mut bob, 2, 3); // extract output through authoritative transaction
    until_packet(&mut bob, 0x30, |p| p.starts_with(&[1, 0, 39]));
    until_packet(&mut bob, 0x2f, |p| p.starts_with(&[255, 255, 255]));
    drop(bob);
    drop(alice);
    server.wait_idle();
    let updated: Value = serde_json::from_slice(&fs::read(saved_path).unwrap()).unwrap();
    assert!(updated["entries"]["0,64,0"]["slots"][2].is_null());
    let inv = server.directory.join("players/twinalpha.inventory.json");
    await_file(&inv);
    let value: Value = serde_json::from_slice(&fs::read(inv).unwrap()).unwrap();
    assert!(value["slots"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["id"] == 265));
}

#[test]
fn survival_shovel_mining_wears_out_last_use_and_persists_dirt_drop() {
    let server = NativeServer::start_with_mode(true);
    let player_inventory = server
        .directory
        .join("players/rusttester.survival.inventory.json");
    fs::create_dir_all(player_inventory.parent().unwrap()).unwrap();
    let mut slots = vec![Value::Null; 45];
    slots[36] = json!({"id":273,"count":1,"damage":130}); // stone shovel, last use
    fs::write(
        &player_inventory,
        serde_json::to_vec(&json!({
            "version":1,
            "selected":0,
            "slots":slots,
            "cursor":null,
            "workbench":vec![Value::Null;9]
        }))
        .unwrap(),
    )
    .unwrap();
    let (mut client, _, _) = server.join("RustTester");
    send_position(&mut client, 4.5, 65.0, 0.5);
    let mut start = vec![0]; // begin digging
    start.extend_from_slice(&0i32.to_be_bytes());
    start.push(63);
    start.extend_from_slice(&0i32.to_be_bytes());
    start.push(1);
    let mut finish = start.clone();
    finish[0] = 2;
    // Abnormally fast client must not mine even with a tool.
    client.write_all(&frame(7, &start)).unwrap();
    client.write_all(&frame(7, &finish)).unwrap();
    until_packet(&mut client, 0x23, |p| {
        p.len() >= 11 && p[4] == 63 && p[9] == 2
    });
    // The stone shovel saves time vs bare-hand 750ms, and breaks on its last use.
    client.write_all(&frame(7, &start)).unwrap();
    thread::sleep(Duration::from_millis(350));
    client.write_all(&frame(7, &finish)).unwrap();
    until_packet(&mut client, 0x2f, |p| p == [0, 0, 36, 255, 255]);
    let metadata = until_packet(&mut client, 0x1c, |p| {
        p.len() >= 13 && p[4] == 0xaa && p[5..7] == [0, 3]
    });
    assert_eq!(&metadata[4..], &[0xaa, 0, 3, 1, 0, 0, 255, 255, 127]);
    until_packet(&mut client, 0x23, |p| {
        p.len() >= 11 && p[4] == 63 && p[9] == 0
    });
    drop(client);
    server.wait_idle();
    let data: Value = serde_json::from_slice(&fs::read(player_inventory).unwrap()).unwrap();
    assert!(
        data["slots"][36].is_null(),
        "the shovel must break at durability 131"
    );
    let drops: Value =
        serde_json::from_slice(&fs::read(server.directory.join("flat.drops.json")).unwrap())
            .unwrap();
    assert_eq!(drops["entries"].as_object().unwrap().len(), 1);
}

#[test]
fn native_protocol5_survival_q_and_ctrl_q_drop_authoritative_stacks() {
    let server = NativeServer::start_with_mode(true);
    let path = server
        .directory
        .join("players/rusttester.survival.inventory.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut slots = vec![Value::Null; 45];
    slots[36] = json!({"id":3,"count":12,"damage":0});
    fs::write(
        &path,
        serde_json::to_vec(&json!({
            "version": 1, "selected": 0, "slots": slots,
            "cursor": null, "workbench": vec![Value::Null; 9]
        }))
        .unwrap(),
    )
    .unwrap();
    let (mut client, _, _) = server.join("RustTester");
    let mut packet = vec![4u8]; // Q: drop one
    packet.extend_from_slice(&0i32.to_be_bytes());
    packet.push(0);
    packet.extend_from_slice(&0i32.to_be_bytes());
    packet.push(255);
    client.write_all(&frame(7, &packet)).unwrap();
    until_packet(&mut client, 0x2f, |p| {
        p.len() == 10 && p[0..3] == [0, 0, 36] && p[5] == 11
    });
    packet[0] = 3; // Ctrl+Q: drop remaining stack
    client.write_all(&frame(7, &packet)).unwrap();
    until_packet(&mut client, 0x2f, |p| p == [0, 0, 36, 255, 255]);
    let saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert!(saved["slots"][36].is_null());
    let drops: Value =
        serde_json::from_slice(&fs::read(server.directory.join("flat.drops.json")).unwrap())
            .unwrap();
    let mut counts: Vec<u64> = drops["entries"]
        .as_object()
        .unwrap()
        .values()
        .map(|value| value["stack"]["count"].as_u64().unwrap())
        .collect();
    counts.sort_unstable();
    assert_eq!(counts, [1, 11]);
}

#[test]
fn native_survival_collision_damage_death_respawn_and_health_persistence() {
    let server = NativeServer::start_with_mode(true);
    let (mut client, _, _) = server.join("RustTester");

    // Position inside bedrock at y=60 must be rejected by the server.
    send_position(&mut client, 0.5, 60.0, 0.5);
    until_packet(&mut client, 0x08, |packet| {
        read_position(packet) == [0.5, 65.0, 0.5]
    });

    // Falling eight blocks to the y=64 grass surface costs five health.
    send_position(&mut client, 0.5, 72.0, 0.5);
    send_position(&mut client, 0.5, 64.0, 0.5);
    let damage = until_packet(&mut client, 0x06, |packet| {
        packet.len() == 10 && f32::from_be_bytes(packet[0..4].try_into().unwrap()) == 15.0
    });
    assert_eq!(&damage[4..6], &20i16.to_be_bytes());

    // The same surface is still supported; repeated packets cannot damage twice.
    send_position(&mut client, 0.5, 64.0, 0.5);
    drop(client);
    server.wait_idle();
    let (mut rejoined, _, position) = server.join("RustTester");
    assert_eq!(position, [0.5, 64.0, 0.5]);

    // Further fall damage is lethal and causes a 0-health update.
    send_position(&mut rejoined, 0.5, 90.0, 0.5);
    send_position(&mut rejoined, 0.5, 64.0, 0.5);
    until_packet(&mut rejoined, 0x06, |packet| {
        packet.len() == 10 && packet[0..4] == 0f32.to_be_bytes()
    });
    // Protocol 5 C16: Perform Respawn, one-byte command.
    rejoined.write_all(&frame(22, &[0])).unwrap();
    until_packet(&mut rejoined, 0x07, |packet| {
        packet.len() >= 6 && packet[0..4] == 0i32.to_be_bytes()
    });
    until_packet(&mut rejoined, 0x06, |packet| {
        packet.len() == 10 && packet[0..4] == 20f32.to_be_bytes()
    });
    until_packet(&mut rejoined, 0x08, |packet| {
        read_position(packet) == [0.5, 65.0, 0.5]
    });
    drop(rejoined);
    server.wait_idle();
    let (final_session, _, position) = server.join("RustTester");
    assert_eq!(position, [0.5, 65.0, 0.5]);
    drop(final_session);
}

#[test]
fn native_protocol5_gravity_sand_falls_after_support_is_mined() {
    let server = NativeServer::start();
    let (mut client, _, _) = server.join("RustTester");
    // Creative C10 grants one sand stack to selected hotbar slot 36.
    let mut creative = 36i16.to_be_bytes().to_vec();
    creative.extend_from_slice(&[0, 12, 64, 0, 0, 255, 255]);
    client.write_all(&frame(0x10, &creative)).unwrap();
    until_packet(&mut client, 0x2f, |b| {
        b == [0, 0, 36, 0, 12, 64, 0, 0, 255, 255]
    });

    // Place sand above grass (y64). Supported sand should remain stationary.
    let mut place = Vec::new();
    place.extend_from_slice(&0i32.to_be_bytes());
    place.push(63);
    place.extend_from_slice(&0i32.to_be_bytes());
    place.push(1);
    place.extend_from_slice(&12i16.to_be_bytes());
    place.push(64);
    place.extend_from_slice(&0i16.to_be_bytes());
    place.extend_from_slice(&(-1i16).to_be_bytes());
    place.extend_from_slice(&[8, 8, 8]);
    client.write_all(&frame(8, &place)).unwrap();
    until_packet(&mut client, 0x23, |b| {
        b.len() == 11 && b[4] == 64 && b[9] == 12
    });

    // Removing the grass wakes sand one block above it. The world tick,
    // not the client, moves it downward and sends both block updates.
    let mut dig = vec![0u8];
    dig.extend_from_slice(&0i32.to_be_bytes());
    dig.push(63);
    dig.extend_from_slice(&0i32.to_be_bytes());
    dig.push(1);
    client.write_all(&frame(7, &dig)).unwrap();
    until_packet(&mut client, 0x23, |b| {
        b.len() == 11 && b[4] == 64 && b[9] == 0
    });
    until_packet(&mut client, 0x23, |b| {
        b.len() == 11 && b[4] == 63 && b[9] == 12
    });
    let snapshot = fs::read(server.directory.join("flat.irw")).unwrap();
    assert_eq!(&snapshot[..4], b"IRW1");
    assert_eq!(snapshot.len() - 8, 11); // the fallen sand replaces the dug grass
    drop(client);
    server.wait_idle();
}

#[test]
fn native_protocol5_airborne_player_gets_20tps_gravity_after_client_silence() {
    let server = NativeServer::start_with_mode(true);
    let (mut client, _, _) = server.join("RustTester");

    // Report a valid airborne position then stop transmitting movement.
    // Server owns the fall from the sixth silent tick onwards.
    send_position(&mut client, 0.5, 70.0, 0.5);
    let dropped = until_packet(&mut client, 0x08, |packet| {
        let position = read_position(packet);
        position[0] == 0.5 && position[1] < 70.0 && position[1] >= 64.0
    });
    assert!(read_position(&dropped)[1] < 70.0);
    let hurt = until_packet(&mut client, 0x06, |packet| {
        packet.len() == 10 && packet[0..4] == 17f32.to_be_bytes()
    });
    assert_eq!(hurt.len(), 10);
    drop(client);
    server.wait_idle();

    let (rejoined, _, position) = server.join("RustTester");
    assert_eq!(position, [0.5, 64.0, 0.5]);
    drop(rejoined);
    server.wait_idle();
}

#[test]
fn native_survival_food_hunger_health_and_death_drops_roundtrip() {
    let server = NativeServer::start_with_mode(true);
    // Seed a native 1.7.10-compatible NBT player with partial hunger.
    // The server must load these values without trusting client-held food.
    #[derive(serde::Serialize)]
    struct SeedPlayer {
        #[serde(rename = "Pos")]
        pos: Vec<f64>,
        #[serde(rename = "Health")]
        health: f32,
        #[serde(rename = "foodLevel")]
        food: i32,
        #[serde(rename = "foodSaturationLevel")]
        saturation: f32,
    }
    let player_file = server.directory.join("players/rusttester.dat");
    fs::create_dir_all(player_file.parent().unwrap()).unwrap();
    let encoded = fastnbt::to_bytes(&SeedPlayer {
        pos: vec![0.5, 64.0, 0.5],
        health: 10.0,
        food: 10,
        saturation: 0.0,
    })
    .unwrap();
    let mut compressed = flate2::write::GzEncoder::new(
        fs::File::create(&player_file).unwrap(),
        flate2::Compression::default(),
    );
    compressed.write_all(&encoded).unwrap();
    compressed.finish().unwrap();

    let inventory_file = server
        .directory
        .join("players/rusttester.survival.inventory.json");
    let mut slots = vec![Value::Null; 45];
    slots[36] = json!({"id":260,"count":2,"damage":0});
    slots[37] = json!({"id":1,"count":9,"damage":0});
    fs::write(
        &inventory_file,
        serde_json::to_vec(&json!({
            "version":1, "selected":0, "slots":slots, "cursor":null,
            "workbench":vec![Value::Null;9]
        }))
        .unwrap(),
    )
    .unwrap();

    let (mut client, _, start) = server.join("RustTester");
    assert_eq!(start, [0.5, 64.0, 0.5]);
    // Invalid forged item use is ignored without any inventory credit.
    let mut use_item = (-1i32).to_be_bytes().to_vec();
    use_item.push(255);
    use_item.extend_from_slice(&(-1i32).to_be_bytes());
    use_item.push(255);
    use_item.extend_from_slice(&1i16.to_be_bytes());
    use_item.push(2);
    use_item.extend_from_slice(&0i16.to_be_bytes());
    use_item.extend_from_slice(&(-1i16).to_be_bytes());
    use_item.extend_from_slice(&[8, 8, 8]);
    client.write_all(&frame(8, &use_item)).unwrap();

    // Authentic apple (260) with 2 items starts 32 tick food use.
    use_item[10] = 1; // first byte of 260i16 = 1, second is 4
    use_item[11] = 4;
    client.write_all(&frame(8, &use_item)).unwrap();
    until_packet(&mut client, 0x2f, |packet| {
        packet.len() == 10
            && packet[..3] == [0, 0, 36]
            && packet[3..5] == 260i16.to_be_bytes()
            && packet[5] == 1
    });
    until_packet(&mut client, 0x06, |packet| {
        packet.len() == 10
            && packet[..4] == 10f32.to_be_bytes()
            && packet[4..6] == 14i16.to_be_bytes()
    });

    // Death must drop the remaining apple and stone and clear source inventory.
    send_position(&mut client, 0.5, 90.0, 0.5);
    send_position(&mut client, 0.5, 64.0, 0.5);
    until_packet(&mut client, 0x06, |packet| {
        packet.len() == 10 && packet[..4] == 0f32.to_be_bytes()
    });
    let drops_file = server.directory.join("flat.drops.json");
    await_file(&drops_file);
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        let value: Value = serde_json::from_slice(&fs::read(&drops_file).unwrap()).unwrap();
        if value["entries"].as_object().unwrap().len() == 2 {
            let counts: Vec<u64> = value["entries"]
                .as_object()
                .unwrap()
                .values()
                .map(|v| v["stack"]["count"].as_u64().unwrap())
                .collect();
            assert!(counts.contains(&1));
            assert!(counts.contains(&9));
            break;
        }
        assert!(Instant::now() < deadline, "death drops never persisted");
        thread::sleep(Duration::from_millis(50));
    }
    let inv: Value = serde_json::from_slice(&fs::read(&inventory_file).unwrap()).unwrap();
    assert!(inv["slots"][36].is_null());
    assert!(inv["slots"][37].is_null());

    client.write_all(&frame(22, &[0])).unwrap();
    until_packet(&mut client, 0x07, |_| true);
    until_packet(&mut client, 0x06, |packet| {
        packet.len() == 10
            && packet[..4] == 20f32.to_be_bytes()
            && packet[4..6] == 20i16.to_be_bytes()
    });
    drop(client);
    server.wait_idle();
    let (rejoined, _, _) = server.join("RustTester");
    drop(rejoined);
    server.wait_idle();
}

#[test]
fn native_protocol5_water_spreads_and_lever_powers_redstone_wire() {
    let server = NativeServer::start();
    let (mut client, _, _) = server.join("RustTester");
    let mut place = |id: i16, x: i32| {
        let mut creative = 36i16.to_be_bytes().to_vec();
        creative.extend_from_slice(&id.to_be_bytes());
        creative.push(1);
        creative.extend_from_slice(&0i16.to_be_bytes());
        creative.extend_from_slice(&(-1i16).to_be_bytes());
        client.write_all(&frame(0x10, &creative)).unwrap();
        until_packet(&mut client, 0x2f, |body| {
            body.len() == 10 && body[3..5] == id.to_be_bytes()
        });
        let mut packet = x.to_be_bytes().to_vec();
        packet.push(63);
        packet.extend_from_slice(&0i32.to_be_bytes());
        packet.push(1);
        packet.extend_from_slice(&id.to_be_bytes());
        packet.push(1);
        packet.extend_from_slice(&0i16.to_be_bytes());
        packet.extend_from_slice(&(-1i16).to_be_bytes());
        packet.extend_from_slice(&[8, 8, 8]);
        client.write_all(&frame(8, &packet)).unwrap();
        until_packet(&mut client, 0x23, |body| {
            body.len() == 11
                && body[0..4] == x.to_be_bytes()
                && body[4] == 64
                && body[9] == if id == 331 { 55 } else { id as u8 }
        });
    };
    place(69, -4); // lever
    place(331, -3); // redstone dust item -> wire block55
    place(8, 5); // source water supported by grass
    drop(place); // release closure's mutable TcpStream borrow
    until_packet(&mut client, 0x23, |body| {
        body.len() == 11
            && body[0..4] == 6i32.to_be_bytes()
            && body[4] == 64
            && body[9] == 8
            && body[10] >= 1
    });
    // Right click the lever to turn on the 15-strength redstone source.
    let mut click = (-4i32).to_be_bytes().to_vec();
    click.push(64);
    click.extend_from_slice(&0i32.to_be_bytes());
    click.push(1);
    click.extend_from_slice(&(-1i16).to_be_bytes()); // empty held snapshot
    click.extend_from_slice(&[8, 8, 8]);
    client.write_all(&frame(8, &click)).unwrap();
    until_packet(&mut client, 0x23, |body| {
        body.len() == 11
            && body[0..4] == (-4i32).to_be_bytes()
            && body[4] == 64
            && body[9] == 69
            && body[10] & 8 != 0
    });
    until_packet(&mut client, 0x23, |body| {
        body.len() == 11
            && body[0..4] == (-3i32).to_be_bytes()
            && body[4] == 64
            && body[9] == 55
            && body[10] == 15
    });
    drop(client);
    server.wait_idle();
}
