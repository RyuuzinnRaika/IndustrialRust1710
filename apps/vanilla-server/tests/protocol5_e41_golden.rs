//! E41: immutable Protocol 5 wire fixtures plus real Rust-server packet checks.
//! These tests are NOT a substitute for an unmodified Java 1.7.10 GUI run.
use flate2::read::ZlibDecoder;
use legacy_protocol::{read_frame, read_varint, write_frame, write_string, write_varint, Cursor};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Only one live test may reserve a loopback port and launch the binary at
/// a time. Concurrent port-0 reservations followed by a released listener can
/// let a second test claim the same port before the first server binds it.
static LIVE_SERVER_LOCK: Mutex<()> = Mutex::new(());

fn packet(id: u32, payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    write_varint(id, &mut body);
    body.extend_from_slice(payload);
    let mut frame = Vec::new();
    write_frame(&mut frame, &body).unwrap();
    frame
}

fn handshake(port: u16, target: u32) -> Vec<u8> {
    let mut body = vec![5]; // Protocol 5 VarInt.
    write_string("localhost", &mut body).unwrap();
    body.extend_from_slice(&port.to_be_bytes());
    write_varint(target, &mut body);
    packet(0, &body)
}

fn received(socket: &mut TcpStream) -> (u32, Vec<u8>) {
    let data = read_frame(socket).expect("Protocol 5 framed packet");
    let mut bytes = data.as_slice();
    let id = read_varint(&mut bytes).expect("packet ID");
    (id, bytes.to_vec())
}

#[test]
fn frozen_handshake_and_keepalive_wire_images() {
    assert_eq!(
        handshake(25565, 2),
        [0x0f, 0, 5, 9, b'l', b'o', b'c', b'a', b'l', b'h', b'o', b's', b't', 0x63, 0xdd, 2]
    );
    assert_eq!(
        packet(1, &[1, 2, 3, 4, 5, 6, 7, 8]),
        [9, 1, 1, 2, 3, 4, 5, 6, 7, 8]
    );
    let mut stream = packet(0x38, &[4, b'B', b'e', b't', b'a', 1, 0, 0]);
    let mut src = stream.as_slice();
    let data = read_frame(&mut src).unwrap();
    assert_eq!(data, [0x38, 4, b'B', b'e', b't', b'a', 1, 0, 0]);
    assert!(src.is_empty());
    stream[0] = 0; // Zero-length frame is invalid.
    assert!(read_frame(&mut stream.as_slice()).is_err());
}

// Frozen Java 1.7-era NBT: anonymous TAG_Compound root, a Chest tile,
// and one SlotItem compound. Unlike constructing both sides with fastnbt,
// the expected bytes are independent of the serializer under test.
const CHEST_NBT: &[u8] = &[
    10, 0, 0, 8, 0, 2, b'i', b'd', 0, 5, b'C', b'h', b'e', b's', b't', 3, 0, 1, b'x', 0, 0, 0, 1,
    3, 0, 1, b'y', 0, 0, 0, 64, 3, 0, 1, b'z', 0, 0, 0, 2, 9, 0, 5, b'I', b't', b'e', b'm', b's',
    10, 0, 0, 0, 1, 1, 0, 4, b'S', b'l', b'o', b't', 0, 2, 0, 2, b'i', b'd', 0, 54, 1, 0, 5, b'C',
    b'o', b'u', b'n', b't', 1, 2, 0, 6, b'D', b'a', b'm', b'a', b'g', b'e', 0, 0, 0, 0,
];

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct ChestFixture {
    id: String,
    x: i32,
    y: i32,
    z: i32,
    #[serde(rename = "Items")]
    items: Vec<ItemFixture>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct ItemFixture {
    #[serde(rename = "Slot")]
    slot: i8,
    id: i16,
    #[serde(rename = "Count")]
    count: i8,
    #[serde(rename = "Damage")]
    damage: i16,
}

#[test]
fn frozen_chest_tileentity_nbt_roundtrip() {
    let chest: ChestFixture = fastnbt::from_bytes(CHEST_NBT).unwrap();
    assert_eq!(chest.id, "Chest");
    assert_eq!((chest.x, chest.y, chest.z), (1, 64, 2));
    assert_eq!(
        chest.items,
        vec![ItemFixture {
            slot: 0,
            id: 54,
            count: 1,
            damage: 0
        }]
    );
    assert_eq!(fastnbt::to_bytes(&chest).unwrap(), CHEST_NBT);
}

struct Server {
    process: Child,
    directory: PathBuf,
    port: u16,
}

impl Server {
    fn start() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "industrialrust1710-e41-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("whitelist.json"),
            r#"[{"name":"Alpha"},{"name":"Beta"}]"#,
        )
        .unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let process = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .args(["--bind", &format!("127.0.0.1:{port}")])
            .arg("--whitelist")
            .arg(directory.join("whitelist.json"))
            .arg("--world")
            .arg(directory.join("fixture.irw"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut server = Self {
            process,
            directory,
            port,
        };
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return server;
            }
            assert!(
                server.process.try_wait().unwrap().is_none(),
                "server exited"
            );
            assert!(Instant::now() < deadline, "server startup timeout");
            thread::sleep(Duration::from_millis(40));
        }
    }

    fn login(&self, name: &str) -> TcpStream {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        stream.write_all(&handshake(self.port, 2)).unwrap();
        let mut user = Vec::new();
        write_string(name, &mut user).unwrap();
        stream.write_all(&packet(0, &user)).unwrap();
        let (id, payload) = received(&mut stream);
        assert_eq!(id, 2, "login rejection: {payload:?}");
        let mut cursor = Cursor::new(&payload);
        assert_eq!(cursor.read_string(36).unwrap().len(), 36);
        assert_eq!(cursor.read_string(16).unwrap(), name);
        cursor.finish().unwrap();
        stream
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn check_chunk(data: &[u8]) {
    assert!(data.len() > 17);
    assert_eq!(data[8], 1, "must be full chunk");
    let mask = u16::from_be_bytes([data[9], data[10]]);
    assert_eq!(mask & 0x18, 0x18, "flat-world sections 3 and 4");
    assert_eq!(&data[11..13], &[0, 0], "no Add bitmap");
    let compressed = i32::from_be_bytes(data[13..17].try_into().unwrap());
    assert!(compressed > 0);
    assert_eq!(17 + compressed as usize, data.len());
    let mut uncompressed = Vec::new();
    ZlibDecoder::new(&data[17..])
        .take(200_000)
        .read_to_end(&mut uncompressed)
        .unwrap();
    assert_eq!(
        uncompressed.len(),
        mask.count_ones() as usize * 10_240 + 256
    );
}

fn consume_initial(socket: &mut TcpStream, expected_entity_id: i32) {
    let mut join = false;
    let mut chunks = 0;
    let mut window_items = false;
    let mut cursor_slot = false;
    let mut position = false;
    for _ in 0..110 {
        let (id, body) = received(socket);
        match id {
            0x01 => {
                let mut expected = expected_entity_id.to_be_bytes().to_vec();
                expected.extend_from_slice(&[1, 0, 2, 8, 7]);
                expected.extend_from_slice(b"default");
                assert_eq!(body, expected, "Join Game Protocol 5 golden bytes");
                join = true;
            }
            0x21 => {
                check_chunk(&body);
                chunks += 1;
            }
            0x30 => {
                let mut expected = vec![0, 0, 45]; // Window 0, 45 signed-short slots.
                for _ in 0..45 {
                    expected.extend_from_slice(&(-1i16).to_be_bytes());
                }
                assert_eq!(body, expected, "empty 1.7.10 Window Items golden bytes");
                window_items = true;
            }
            0x2f => {
                assert_eq!(
                    body,
                    [255, 255, 255, 255, 255],
                    "empty cursor S2F golden bytes"
                );
                cursor_slot = true;
            }
            0x08 => {
                assert_eq!(body.len(), 33, "Player Position + Look length");
                position = true;
                break;
            }
            0x00 if body.len() == 4 => socket.write_all(&packet(0, &body)).unwrap(),
            _ => {}
        }
    }
    assert!(
        join && window_items && cursor_slot && position,
        "initial login packets missing"
    );
    assert_eq!(chunks, 25, "initial 5x5 chunk view");
}

#[test]
fn live_protocol5_entity_chunk_inventory_and_window_golden() {
    let _live_guard = LIVE_SERVER_LOCK.lock().unwrap();
    let server = Server::start();
    let mut alpha = server.login("Alpha");
    consume_initial(&mut alpha, 1);
    let mut beta = server.login("Beta");
    consume_initial(&mut beta, 2);

    // The first client's bounded event queue must see the second user.
    let mut saw_list = false;
    let mut saw_entity = false;
    for _ in 0..80 {
        let (id, body) = received(&mut alpha);
        match id {
            0x38 if body == [4, b'B', b'e', b't', b'a', 1, 0, 0] => saw_list = true,
            0x0c => {
                // Golden Spawn Player 1.7.10: entity VarInt, UUID/name strings,
                // empty properties, XYZ fixed *32, yaw/pitch, item and metadata.
                // The Y field carries feet height: the flat fixture's top solid
                // block is the grass layer at y=63, so a joining player stands
                // at 64.0 (64.0 * 32 = 2048). This became the wire value when
                // #669 routed joining through the world-spawn surface search;
                // before that the server sent 65.0, one block above the floor.
                let mut expected = vec![2, 36];
                expected.extend_from_slice(b"1110dc55-a244-318c-9843-101d405cbf34");
                expected.extend_from_slice(&[4, b'B', b'e', b't', b'a', 0]);
                expected.extend_from_slice(&[
                    0, 0, 0, 16, // X = 0.5 blocks * 32
                    0, 0, 8, 0, // Y = 64.0 feet * 32
                    0, 0, 0, 16, // Z = 0.5 blocks * 32
                    0, 0, // yaw, pitch
                    0, 0,    // item (short -1 equivalent in this encoder)
                    0x7f, // trailing metadata terminator
                ]);
                assert_eq!(body, expected, "Spawn Player Protocol 5 golden bytes");
                saw_entity = true;
            }
            0x00 if body.len() == 4 => alpha.write_all(&packet(0, &body)).unwrap(),
            _ => {}
        }
        if saw_list && saw_entity {
            break;
        }
    }
    assert!(
        saw_list && saw_entity,
        "second player not visible to first player"
    );
}

/// Round-trip a compressed NBT-tagged Creative item through the running
/// Protocol 5 server. This checks packet boundaries beyond the unit parser.
#[test]
fn live_protocol5_creative_nbt_roundtrip_and_invalid_tag_recovery() {
    use fastnbt::Value;
    use flate2::{write::GzEncoder, Compression};
    use std::collections::HashMap;

    let _live_guard = LIVE_SERVER_LOCK.lock().unwrap();
    let server = Server::start();
    let mut alpha = server.login("Alpha");
    consume_initial(&mut alpha, 1);

    let root = Value::Compound(HashMap::from([
        (
            "UnknownField".to_string(),
            Value::String("keep me".to_string()),
        ),
        (
            "ench".to_string(),
            Value::List(vec![Value::Compound(HashMap::from([
                ("id".to_string(), Value::Short(16)),
                ("lvl".to_string(), Value::Short(3)),
            ]))]),
        ),
    ]));
    let raw = fastnbt::to_bytes(&root).unwrap();
    let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
    gzip.write_all(&raw).unwrap();
    let compressed = gzip.finish().unwrap();

    let mut slot = Vec::from([0, 36, 1, 20, 1, 0, 0]); // diamond sword 276
    slot.extend_from_slice(&(compressed.len() as i16).to_be_bytes());
    slot.extend_from_slice(&compressed);
    alpha.write_all(&packet(0x10, &slot)).unwrap();

    let mut expected = vec![0]; // S2F window ID
    expected.extend_from_slice(&slot);
    let mut found = false;
    for _ in 0..40 {
        let (id, body) = received(&mut alpha);
        match id {
            0x2f if body == expected => {
                found = true;
                break;
            }
            0x00 if body.len() == 4 => alpha.write_all(&packet(0, &body)).unwrap(),
            _ => {}
        }
    }
    assert!(found, "server failed to preserve creative NBT on S2F");

    // A bad gzip Compound is an application rejection, not a disconnect.
    alpha
        .write_all(&packet(0x10, &[0, 36, 1, 20, 1, 0, 0, 0, 3, 10, 0, 0]))
        .unwrap();
    let mut resynced = false;
    for _ in 0..40 {
        let (id, body) = received(&mut alpha);
        match id {
            0x30 if body.starts_with(&[0, 0, 45]) => {
                resynced = true;
                break;
            }
            0x00 if body.len() == 4 => alpha.write_all(&packet(0, &body)).unwrap(),
            _ => {}
        }
    }
    assert!(resynced, "bad creative NBT must resynchronize inventory");

    // Confirm connection remains usable after rejected NBT.
    alpha
        .write_all(&packet(0x10, &[0, 36, 0, 4, 64, 0, 0, 255, 255]))
        .unwrap();
    let mut usable = false;
    for _ in 0..40 {
        let (id, body) = received(&mut alpha);
        match id {
            0x2f if body == [0, 0, 36, 0, 4, 64, 0, 0, 255, 255] => {
                usable = true;
                break;
            }
            0x00 if body.len() == 4 => alpha.write_all(&packet(0, &body)).unwrap(),
            _ => {}
        }
    }
    assert!(usable, "valid creative actions still work after bad NBT");
}

/// C10 (Creative Inventory Action) -> S2F (Set Slot) wire contract, exercised
/// against the actual server binary. Fixed byte expectations are deliberately
/// independent from Inventory::encode_slot and the server packet writer.
#[test]
fn live_protocol5_creative_slot_set_and_clear_golden() {
    let _live_guard = LIVE_SERVER_LOCK.lock().unwrap();
    let server = Server::start();
    let mut alpha = server.login("Alpha");
    consume_initial(&mut alpha, 1);

    // Slot 36 = first hotbar slot, item 4 = cobblestone, 64, damage 0,
    // NBT length -1. This is Protocol 5, not modern Slot/NBT encoding.
    let create = packet(0x10, &[0, 36, 0, 4, 64, 0, 0, 255, 255]);
    // A TCP split across the frame-length and packet-ID boundary must not
    // silently lose the write when the server's read poll times out.
    alpha.write_all(&create[..1]).unwrap();
    thread::sleep(Duration::from_millis(75));
    alpha.write_all(&create[1..]).unwrap();

    let mut created = false;
    for _ in 0..30 {
        let (id, body) = received(&mut alpha);
        match id {
            0x2f => {
                assert_eq!(
                    body,
                    [0, 0, 36, 0, 4, 64, 0, 0, 255, 255],
                    "server-authoritative S2F filled slot bytes"
                );
                created = true;
                break;
            }
            0x00 if body.len() == 4 => alpha.write_all(&packet(0, &body)).unwrap(),
            _ => {}
        }
    }
    assert!(created, "no S2F Set Slot after C10 inventory action");

    // A Slot with item ID -1 occupies exactly two bytes: there is no
    // count/damage/NBT trailer on an empty slot.
    alpha.write_all(&packet(0x10, &[0, 36, 255, 255])).unwrap();
    let mut cleared = false;
    for _ in 0..30 {
        let (id, body) = received(&mut alpha);
        match id {
            0x2f => {
                assert_eq!(body, [0, 0, 36, 255, 255], "empty slot S2F golden bytes");
                cleared = true;
                break;
            }
            0x00 if body.len() == 4 => alpha.write_all(&packet(0, &body)).unwrap(),
            _ => {}
        }
    }
    assert!(cleared, "no S2F Set Slot after clearing hotbar");
}
