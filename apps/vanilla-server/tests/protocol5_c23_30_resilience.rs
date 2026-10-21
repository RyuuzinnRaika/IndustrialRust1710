//! Independent TCP acceptance tests for the C23-C30 workstream.
//! All traffic uses the original Minecraft 1.7.10 (Protocol 5) framing.
//! The test deliberately kills and restarts the real Rust server process.
use legacy_protocol::{read_frame, read_varint, write_string, write_varint, Cursor};
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Harness {
    process: Child,
    dir: PathBuf,
    port: u16,
}

impl Harness {
    fn launch() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "industrialrust1710-c30-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("whitelist.json"),
            serde_json::to_vec(&json!([
                {"name":"RustTester"},
                {"name":"TwinAlpha"},
                {"name":"TwinBeta"}
            ]))
            .unwrap(),
        )
        .unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let process = Self::spawn(&dir, port);
        let mut server = Self { process, dir, port };
        server.wait_ready();
        server
    }

    fn spawn(dir: &PathBuf, port: u16) -> Child {
        Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .args(["--bind", &format!("127.0.0.1:{port}")])
            .arg("--whitelist")
            .arg(dir.join("whitelist.json"))
            .arg("--world")
            .arg(dir.join("flat.irw"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    fn wait_ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            if TcpStream::connect(("127.0.0.1", self.port)).is_ok() {
                return;
            }
            assert!(self.process.try_wait().unwrap().is_none(), "server exited");
            assert!(Instant::now() < deadline, "server did not bind");
            thread::sleep(Duration::from_millis(40));
        }
    }

    fn restart_uncleanly(&mut self) {
        self.process.kill().unwrap();
        self.process.wait().unwrap();
        self.process = Self::spawn(&self.dir, self.port);
        self.wait_ready();
    }

    fn connect(&self) -> TcpStream {
        let socket = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(12)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(12)))
            .unwrap();
        socket
    }

    fn status(&self) -> Value {
        let mut socket = self.connect();
        socket.write_all(&handshake(self.port, 1)).unwrap();
        socket.write_all(&frame(0, &[])).unwrap();
        let (id, payload) = read_packet(&mut socket);
        assert_eq!(id, 0);
        let mut cursor = Cursor::new(&payload);
        let info = serde_json::from_str(cursor.read_string(65536).unwrap()).unwrap();
        cursor.finish().unwrap();
        info
    }

    fn login(&self, name: &str) -> TcpStream {
        let mut socket = self.connect();
        socket.write_all(&handshake(self.port, 2)).unwrap();
        socket.write_all(&frame(0, &mc_string(name))).unwrap();
        let (id, payload) = read_packet(&mut socket);
        assert_eq!(id, 2, "login failed for {name}: {payload:?}");
        socket
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn varint(number: u32) -> Vec<u8> {
    let mut output = Vec::new();
    write_varint(number, &mut output);
    output
}

fn mc_string(value: &str) -> Vec<u8> {
    let mut output = Vec::new();
    write_string(value, &mut output).unwrap();
    output
}

fn frame(id: u32, payload: &[u8]) -> Vec<u8> {
    let mut packet = varint(id);
    packet.extend_from_slice(payload);
    let mut output = varint(packet.len() as u32);
    output.extend_from_slice(&packet);
    output
}

fn handshake(port: u16, state: u32) -> Vec<u8> {
    let mut body = varint(5);
    body.extend_from_slice(&mc_string("localhost"));
    body.extend_from_slice(&port.to_be_bytes());
    body.extend_from_slice(&varint(state));
    frame(0, &body)
}

fn read_packet(socket: &mut TcpStream) -> (u32, Vec<u8>) {
    let packet = read_frame(socket).expect("read Protocol 5 packet");
    let mut bytes = packet.as_slice();
    let id = read_varint(&mut bytes).expect("read packet ID");
    (id, bytes.to_vec())
}

fn wait_for(socket: &mut TcpStream, expected: u32, matches: impl Fn(&[u8]) -> bool) -> Vec<u8> {
    for _ in 0..160 {
        let (id, body) = read_packet(socket);
        if id == 0 && body.len() == 4 {
            socket.write_all(&frame(0, &body)).unwrap();
        }
        if id == expected && matches(&body) {
            return body;
        }
    }
    panic!("missing packet 0x{expected:02x}");
}

fn inventory_item(window_items: &[u8], wanted: usize) -> Option<(i16, u8, i16)> {
    assert_eq!(window_items[0], 0);
    assert_eq!(i16::from_be_bytes([window_items[1], window_items[2]]), 45);
    let mut offset = 3;
    for slot in 0..45 {
        let id = i16::from_be_bytes([window_items[offset], window_items[offset + 1]]);
        offset += 2;
        let current = if id == -1 {
            None
        } else {
            let count = window_items[offset];
            let damage = i16::from_be_bytes([window_items[offset + 1], window_items[offset + 2]]);
            let nbt = i16::from_be_bytes([window_items[offset + 3], window_items[offset + 4]]);
            assert_eq!(nbt, -1, "unexpected NBT in bounded test item");
            offset += 5;
            Some((id, count, damage))
        };
        if slot == wanted {
            return current;
        }
    }
    panic!("inventory slot {wanted} missing");
}

fn creative_set(socket: &mut TcpStream, slot: i16, id: i16, count: u8) {
    let mut packet = slot.to_be_bytes().to_vec();
    packet.extend_from_slice(&id.to_be_bytes());
    packet.push(count);
    packet.extend_from_slice(&0i16.to_be_bytes());
    packet.extend_from_slice(&(-1i16).to_be_bytes());
    socket.write_all(&frame(0x10, &packet)).unwrap();
    wait_for(socket, 0x2f, |p| {
        p.len() >= 3 && p[0] == 0 && p[1..3] == slot.to_be_bytes()
    });
}

#[test]
fn protocol5_rejects_malformed_varints_and_keeps_serving_two_players() {
    let server = Harness::launch();
    assert_eq!(server.status()["version"]["protocol"], 5);

    // The five-byte VarInt limit must be enforced without a process crash.
    let mut malformed = server.connect();
    malformed
        .write_all(&[0xff, 0xff, 0xff, 0xff, 0x7f])
        .unwrap();
    drop(malformed);

    // A malformed login body cannot bypass the whitelist.
    let mut invalid_login = server.connect();
    invalid_login.write_all(&handshake(server.port, 2)).unwrap();
    invalid_login
        .write_all(&frame(0, &[0xff, 0xff, 0xff, 0xff, 0xff, 0x01]))
        .unwrap();
    drop(invalid_login);

    let alice = server.login("TwinAlpha");
    let bob = server.login("TwinBeta");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if server.status()["players"]["online"].as_u64() == Some(2) {
            break;
        }
        assert!(Instant::now() < deadline, "two sessions not visible");
        thread::sleep(Duration::from_millis(50));
    }
    drop(bob);
    drop(alice);
    assert_eq!(server.status()["version"]["protocol"], 5);
}

#[test]
fn protocol5_creative_inventory_survives_forced_restart_without_duplication() {
    let mut server = Harness::launch();
    let mut socket = server.login("RustTester");
    creative_set(&mut socket, 36, 264, 7);
    drop(socket);

    // Kill without a graceful shutdown. The Set Slot acknowledgement is sent
    // only after the authoritative inventory file has been persisted.
    server.restart_uncleanly();

    let mut reconnect = server.login("RustTester");
    let contents = wait_for(&mut reconnect, 0x30, |p| p.starts_with(&[0, 0, 45]));
    assert_eq!(inventory_item(&contents, 36), Some((264, 7, 0)));

    // An invalid Click Window must not change any saved stack.
    reconnect.write_all(&frame(0x0e, &[0, 0])).unwrap();
    drop(reconnect);
    server.restart_uncleanly();
    let mut again = server.login("RustTester");
    let contents = wait_for(&mut again, 0x30, |p| p.starts_with(&[0, 0, 45]));
    assert_eq!(inventory_item(&contents, 36), Some((264, 7, 0)));
}
