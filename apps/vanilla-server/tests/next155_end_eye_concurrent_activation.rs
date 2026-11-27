//! NEXT-155 / #1138: acceptance regression — concurrent End-eye insertion.
//!
//! Two Protocol 5 clients race the FINAL eye into one shared End Portal
//! frame cell (block 120; legacy metadata 0=S, 1=W, 2=N, 3=E plus the
//! bit-2 "Eye" flag). The server serialises the use under the world mutex
//! (C08 special-item branch -> use_special_item stages the change set,
//! journal-commits world + inventory together, then swaps live state),
//! and `end_eye_changes` returns an EMPTY change set for a cell that is
//! not frame 120 or already holds an eye. An empty change set never
//! debits and never publishes, so exactly one client can light the cell,
//! consume one eye and generate the nine 119 portal tiles:
//! 「同時操作でアイ消費・ポータル生成が一回ずつ発生する」.
//!
//! Wire contract pinned per observer:
//!   * exactly one 0x23 Block Change lights the contested cell
//!     (block 120 with the eye bit); no other update for that cell;
//!   * exactly nine 0x23 Block Changes for the 3x3 centre — each of the
//!     nine tiles announced exactly once, no duplicate generation, and no
//!     other block change inside the ring;
//!   * exactly one 0x30 Window Items dump across both clients shows slot
//!     36 emptied (one eye consumed); the loser's 0x30 update still shows
//!     its one eye (the empty change set debits and journals nothing);
//!   * sequential replay on the now-lit cell (already-lit guard): neither
//!     the winner (after a refill) nor the loser consumes anything or
//!     changes any ring cell;
//!   * durable files: twelve lit frames and nine 119 tiles exactly once
//!     in the IRW world, and exactly one consumed eye across both
//!     survival inventory saves.
//!
//! Which client wins is deliberately never asserted — only the
//! exactly-once invariants survive. Unmodified 1.7.10 client acceptance
//! remains NOT_RUN on this headless host; disposable worlds only.

#[path = "support/packet_client.rs"]
mod packet_client;

use legacy_protocol::{write_frame, write_string, write_varint};
use packet_client::BufferedClient;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Frame ring and portal tiles sit at walkable height: the flat world
/// ground plane is solid at y=63, air above.
const FRAME_Y: i32 = 64;
/// Contested cell: centre-relative (2, 0) — the middle frame on the east
/// side, legacy facing 1. The fixture lights the other eleven cells.
const CONTESTED: (i32, i32) = (2, 0);
const CONTESTED_FACING: u8 = 1;
const EYE_ITEM: i16 = 381;
const FRAME_BLOCK: u16 = 120;
const PORTAL_BLOCK: u16 = 119;

/// The twelve ring cells with the facings `end_eye_changes` demands: every
/// frame must face the portal centre (rz=-2 -> 0, rz=2 -> 2, rx=-2 -> 3,
/// rx=2 -> 1). Placing the frames while standing in the centre reproduces
/// exactly these values through `end_frame_placement_facing`.
fn ring_cells() -> Vec<(i32, i32, u8)> {
    let mut cells = Vec::new();
    for offset in -1..=1 {
        cells.push((offset, -2, 0u8));
        cells.push((offset, 2, 2));
        cells.push((-2, offset, 3));
        cells.push((2, offset, 1));
    }
    assert_eq!(cells.len(), 12);
    cells
}

fn centre_cells() -> Vec<(i32, i32)> {
    let mut cells = Vec::new();
    for dx in -1..=1 {
        for dz in -1..=1 {
            cells.push((dx, dz));
        }
    }
    assert_eq!(cells.len(), 9);
    cells
}

fn in_ring_area(x: i32, z: i32) -> bool {
    x.abs() <= 2 && z.abs() <= 2
}

struct Server {
    child: Child,
    root: PathBuf,
    log: PathBuf,
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
    fn start() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "industrialrust-next155-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join("whitelist.json"),
            r#"[{"name":"SeekerA"},{"name":"SeekerB"}]"#,
        )
        .unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let log = root.join("server.log");
        let output = File::create(&log).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .current_dir(&root)
            .args(["--bind", &format!("127.0.0.1:{port}")])
            .arg("--world")
            .arg(root.join("flat.irw"))
            .arg("--whitelist")
            .arg(root.join("whitelist.json"))
            .stdin(Stdio::piped())
            .stdout(Stdio::from(output.try_clone().unwrap()))
            .stderr(Stdio::from(output))
            .spawn()
            .unwrap();
        let mut server = Self {
            child,
            root,
            log,
            port,
        };
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            if let Some(status) = server.child.try_wait().unwrap() {
                panic!(
                    "server exited before listening: {status}\n{}",
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

    fn console(&mut self, command: &str) {
        self.child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(format!("{command}\n").as_bytes())
            .unwrap();
    }

    fn join(&self, name: &str) -> BufferedClient {
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.try_join(name) {
                Ok(socket) => return socket,
                Err(rejected) => {
                    assert!(attempt < 25, "{name}: join rejected {attempt}x: {rejected}");
                    thread::sleep(Duration::from_millis(300));
                }
            }
        }
    }

    fn try_join(&self, name: &str) -> Result<BufferedClient, String> {
        let mut socket = BufferedClient::new(TcpStream::connect(("127.0.0.1", self.port)).unwrap());
        socket.set_nodelay(true).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(250)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut handshake = Vec::new();
        write_varint(5, &mut handshake);
        write_string("localhost", &mut handshake).unwrap();
        handshake.extend_from_slice(&self.port.to_be_bytes());
        write_varint(2, &mut handshake);
        socket.write_all(&packet(0, &handshake)).unwrap();
        let mut start = Vec::new();
        write_string(name, &mut start).unwrap();
        socket.write_all(&packet(0, &start)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            assert!(Instant::now() < deadline, "{name}: join timed out");
            let Some((id, body)) = pump(&mut socket) else {
                continue;
            };
            match id {
                0x00 if body.len() == 4 => socket.write_all(&packet(0, &body)).unwrap(),
                0x01 if body.len() >= 4 => {
                    let marker = Instant::now() + Duration::from_secs(25);
                    loop {
                        assert!(Instant::now() < marker, "{name}: S08 timed out");
                        let Some((id, body)) = pump(&mut socket) else {
                            continue;
                        };
                        match id {
                            0x00 if body.len() == 4 => socket.write_all(&packet(0, &body)).unwrap(),
                            0x08 if body.len() == 33 => return Ok(socket),
                            0x40 => return Err(String::from_utf8_lossy(&body).into_owned()),
                            _ => {}
                        }
                    }
                }
                0x40 => return Err(String::from_utf8_lossy(&body).into_owned()),
                _ => {}
            }
        }
    }

    fn stop(&mut self) {
        self.child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"/stop\n")
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "shutdown failed: {status}\n{}",
                    fs::read_to_string(&self.log).unwrap_or_default()
                );
                return;
            }
            assert!(Instant::now() < deadline, "server shutdown timed out");
            thread::sleep(Duration::from_millis(40));
        }
    }
}

fn packet(id: u32, body: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();
    write_varint(id, &mut payload);
    payload.extend_from_slice(body);
    let mut framed = Vec::new();
    write_frame(&mut framed, &payload).unwrap();
    framed
}

/// Every pump loop acknowledges 0x00 keep-alives and never pushes any
/// other packet back into the stream it is collecting.
fn pump(socket: &mut BufferedClient) -> Option<(u32, Vec<u8>)> {
    socket.poll_packet().expect("read Protocol 5 packet")
}

fn ack_keepalive(socket: &mut BufferedClient, id: u32, body: &[u8]) -> bool {
    if id == 0x00 && body.len() == 4 {
        socket.write_all(&packet(0, body)).unwrap();
        return true;
    }
    false
}

/// S08 Position & Look: [x][head y][z] as big-endian f64.
fn feet(s08_body: &[u8]) -> [f64; 2] {
    [
        f64::from_be_bytes(s08_body[..8].try_into().unwrap()),
        f64::from_be_bytes(s08_body[8..16].try_into().unwrap()) - f64::from(1.62_f32),
    ]
}

fn position(xyz: [f64; 3], yaw: f32, pitch: f32) -> Vec<u8> {
    let mut body = Vec::new();
    for value in [xyz[0], xyz[1], xyz[1] + 1.62, xyz[2]] {
        body.extend_from_slice(&value.to_be_bytes());
    }
    body.extend_from_slice(&yaw.to_be_bytes());
    body.extend_from_slice(&pitch.to_be_bytes());
    body.push(1);
    packet(6, &body)
}

fn teleport(client: &mut BufferedClient, server: &mut Server, name: &str, target: [f64; 3]) {
    server.console(&format!(
        "/tp {name} {} {} {}",
        target[0], target[1], target[2]
    ));
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(Instant::now() < deadline, "{name}: teleport S08 timed out");
        let Some((id, body)) = pump(client) else {
            continue;
        };
        if ack_keepalive(client, id, &body) {
            continue;
        }
        if id == 0x40 {
            panic!("{name}: teleport: disconnected: {body:?}");
        }
        if id == 0x08
            && body.len() == 33
            && (feet(&body)[0] - target[0]).abs() <= 1.0 / 16.0
            && (feet(&body)[1] - target[1]).abs() <= 1.0 / 16.0
            && f64::from_be_bytes(body[16..24].try_into().unwrap()) == target[2]
        {
            client.write_all(&position(target, 0.0, 0.0)).unwrap();
            return;
        }
    }
}

/// C10 Creative Inventory Action + the Set Slot confirmation it must echo.
fn creative_set(client: &mut BufferedClient, slot: i16, item: i16, count: u8) {
    let mut body = Vec::new();
    body.extend_from_slice(&slot.to_be_bytes());
    body.extend_from_slice(&item.to_be_bytes());
    body.push(count);
    body.extend_from_slice(&0i16.to_be_bytes());
    body.extend_from_slice(&(-1i16).to_be_bytes());
    client.write_all(&packet(0x10, &body)).unwrap();
    let want = slot.to_be_bytes();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(
            Instant::now() < deadline,
            "creative set slot {slot} timed out"
        );
        let Some((id, body)) = pump(client) else {
            continue;
        };
        if ack_keepalive(client, id, &body) {
            continue;
        }
        if id == 0x40 {
            panic!("creative set: disconnected: {body:?}");
        }
        if id == 0x2f && body.len() >= 3 && body[0] == 0 && body[1..3] == want {
            return;
        }
    }
}

/// C08 right-click against one face, claiming one undamaged item.
fn right_click(client: &mut BufferedClient, target: (i32, u8, i32), face: u8, item: i16) {
    let mut body = Vec::new();
    body.extend_from_slice(&target.0.to_be_bytes());
    body.push(target.1);
    body.extend_from_slice(&target.2.to_be_bytes());
    body.push(face);
    body.extend_from_slice(&item.to_be_bytes());
    body.push(1);
    body.extend_from_slice(&0i16.to_be_bytes());
    body.extend_from_slice(&(-1i16).to_be_bytes());
    body.extend_from_slice(&[8, 8, 8]);
    client.write_all(&packet(8, &body)).unwrap();
}

/// Parse 0x23 Block Change `[x i32][y u8][z i32][id VarInt][meta u8]`.
/// Only single-byte varint ids (0..=127) are relevant here.
fn block_change(body: &[u8]) -> Option<(i32, u8, i32, u8, u8)> {
    (body.len() == 11 && body[9] < 128).then(|| {
        (
            i32::from_be_bytes(body[..4].try_into().unwrap()),
            body[4],
            i32::from_be_bytes(body[5..9].try_into().unwrap()),
            body[9],
            body[10],
        )
    })
}

/// Slot 36 of a window-0 0x30 dump. Outer `None` means "not a window-0
/// dump"; `Some(None)` is an empty slot; `Some(Some(bytes))` the raw
/// seven-byte stack entry. Player windows in this scenario hold only
/// untagged stacks, so empty entries take two bytes and items seven.
fn slot36(body: &[u8]) -> Option<Option<[u8; 7]>> {
    if body.len() < 3 || body[0] != 0 {
        return None;
    }
    if i16::from_be_bytes(body[1..3].try_into().unwrap()) != 45 {
        return None;
    }
    let mut cursor = 3usize;
    for index in 0..45i16 {
        if cursor + 2 > body.len() {
            return None;
        }
        let head = [body[cursor], body[cursor + 1]];
        if head == [255, 255] {
            if index == 36 {
                return Some(None);
            }
            cursor += 2;
        } else {
            if cursor + 7 > body.len() {
                return None;
            }
            let entry: [u8; 7] = body[cursor..cursor + 7].try_into().unwrap();
            if index == 36 {
                return Some(Some(entry));
            }
            cursor += 7;
        }
    }
    None
}

/// Window entry for `count` ender eyes (381 = 0x017D, no NBT).
fn eye_entry(count: u8) -> [u8; 7] {
    [1, 125, count, 0, 0, 255, 255]
}

fn wait_slot36(client: &mut BufferedClient, entry: [u8; 7], context: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(
            Instant::now() < deadline,
            "{context}: slot-36 eye update timed out"
        );
        let Some((id, body)) = pump(client) else {
            continue;
        };
        if ack_keepalive(client, id, &body) {
            continue;
        }
        if id == 0x40 {
            panic!("{context}: disconnected: {body:?}");
        }
        if id == 0x30 && slot36(&body) == Some(Some(entry)) {
            return;
        }
    }
}

fn wait_frame_block(client: &mut BufferedClient, x: i32, z: i32, meta: u8, context: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(
            Instant::now() < deadline,
            "{context}: frame update at ({x},{z}) timed out"
        );
        let Some((id, body)) = pump(client) else {
            continue;
        };
        if ack_keepalive(client, id, &body) {
            continue;
        }
        if id == 0x40 {
            panic!("{context}: disconnected: {body:?}");
        }
        if id == 0x23 {
            if let Some((bx, by, bz, block, bm)) = block_change(&body) {
                if bx == x && by as i32 == FRAME_Y && bz == z {
                    assert_eq!(
                        block, FRAME_BLOCK as u8,
                        "{context}: ({x},{z}) must hold 120"
                    );
                    assert_eq!(bm, meta, "{context}: metadata at ({x},{z})");
                    return;
                }
            }
        }
    }
}

fn wait_survival(client: &mut BufferedClient, who: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(
            Instant::now() < deadline,
            "{who}: survival switch timed out"
        );
        let Some((id, body)) = pump(client) else {
            continue;
        };
        if ack_keepalive(client, id, &body) {
            continue;
        }
        if id == 0x40 {
            panic!("{who}: disconnected: {body:?}");
        }
        if id == 0x39 && body.first() == Some(&0) {
            return;
        }
    }
}

fn drain(client: &mut BufferedClient, window: Duration) {
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        if let Some((id, body)) = pump(client) {
            ack_keepalive(client, id, &body);
        }
    }
}

/// Everything one racer observed while firing at the contested cell.
#[derive(Default)]
struct Tally {
    /// 0x23 lighting the contested cell (120 with the eye bit).
    contested_lit: usize,
    /// Any other 0x23 touching the contested cell.
    contested_other: usize,
    /// 0x23 announcements per block-119 cell inside the ring area.
    tiles: BTreeMap<(i32, i32), usize>,
    /// Other 0x23 changes inside the ring area.
    stray_changes: usize,
    /// Window-0 dumps whose slot 36 is empty: each one is a debit.
    empty_dumps: usize,
    /// Window-0 dumps whose slot 36 still carries at least one eye: the
    /// unchanged-inventory echo of a no-op (loser / already-lit replay).
    eye_dumps: usize,
    /// 0x2F updates clearing hotbar slot 36.
    slot_clears: usize,
}

/// Fires one C08 at the shared empty cell the instant both racers clear
/// the barrier, then tallies the wire for a fixed window. No artificial
/// ordering is applied: TCP delivery and the server's per-dimension world
/// mutex decide which session wins.
fn race(mut socket: BufferedClient, gate: Arc<Barrier>) -> (BufferedClient, Tally) {
    gate.wait();
    right_click(
        &mut socket,
        (CONTESTED.0, FRAME_Y as u8, CONTESTED.1),
        1,
        EYE_ITEM,
    );
    let mut tally = Tally::default();
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        let Some((id, body)) = pump(&mut socket) else {
            continue;
        };
        if ack_keepalive(&mut socket, id, &body) {
            continue;
        }
        if id == 0x40 {
            panic!("racer disconnected during the race: {body:?}");
        }
        if id == 0x23 {
            if let Some((x, y, z, block, meta)) = block_change(&body) {
                if y as i32 == FRAME_Y && in_ring_area(x, z) {
                    if (x, z) == CONTESTED {
                        if block == FRAME_BLOCK as u8 && meta == (CONTESTED_FACING | 4) {
                            tally.contested_lit += 1;
                        } else {
                            tally.contested_other += 1;
                        }
                    } else if block == PORTAL_BLOCK as u8 {
                        *tally.tiles.entry((x, z)).or_insert(0) += 1;
                    } else {
                        tally.stray_changes += 1;
                    }
                }
            }
        }
        if id == 0x30 {
            if let Some(entry) = slot36(&body) {
                match entry {
                    None => tally.empty_dumps += 1,
                    Some(stack) => {
                        if stack[..2] == [1, 125] && stack[2] >= 1 {
                            tally.eye_dumps += 1;
                        }
                    }
                }
            }
        }
        if id == 0x2f
            && body.len() >= 5
            && body[0] == 0
            && body[1..3] == [0, 36]
            && body[3..5] == [255, 255]
        {
            tally.slot_clears += 1;
        }
    }
    (socket, tally)
}

/// Neither socket may see any ring block change, window dump or slot-36
/// update during the window: the already-lit guard must be silent.
fn assert_quiet(
    first: &mut BufferedClient,
    second: &mut BufferedClient,
    window: Duration,
    phase: &str,
) {
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        for client in [&mut *first, &mut *second] {
            let Some((id, body)) = pump(client) else {
                continue;
            };
            if ack_keepalive(client, id, &body) {
                continue;
            }
            if id == 0x40 {
                panic!("{phase}: disconnected: {body:?}");
            }
            if id == 0x23 {
                if let Some((x, y, z, block, meta)) = block_change(&body) {
                    if y as i32 == FRAME_Y && in_ring_area(x, z) {
                        panic!("{phase}: ring cell ({x},{z}) changed again to {block}/{meta}");
                    }
                }
            }
            if id == 0x30 && slot36(&body) == Some(None) {
                panic!("{phase}: a window dump emptied slot 36 (an eye was consumed)");
            }
            if id == 0x2f && body.len() >= 3 && body[0] == 0 && body[1..3] == [0, 36] {
                panic!("{phase}: unexpected set-slot for slot 36");
            }
        }
    }
}

/// Sparse IRW edit table: header + `[x i32][y u8][z i32][block u16]` per
/// edited cell. Terrain outside the edit table is implicit in this world.
fn read_irw_edits(path: &Path) -> BTreeMap<(i32, u8, i32), u16> {
    let bytes = fs::read(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    assert!(bytes.len() >= 8, "world file shorter than its header");
    let offset = match &bytes[..4] {
        b"IRW1" => 8,
        b"IRW2" | b"IRW3" => 16,
        b"IRW4" => 17,
        other => panic!("unexpected world magic {other:?} in {}", path.display()),
    };
    assert!(bytes.len() >= offset, "truncated world header");
    let count = u32::from_be_bytes(bytes[offset - 4..offset].try_into().unwrap()) as usize;
    assert!(
        bytes.len() >= offset + count * 11,
        "world edit count/length mismatch"
    );
    let mut edits = BTreeMap::new();
    for record in bytes[offset..offset + count * 11].chunks_exact(11) {
        let x = i32::from_be_bytes(record[0..4].try_into().unwrap());
        let y = record[4];
        let z = i32::from_be_bytes(record[5..9].try_into().unwrap());
        let block = u16::from_be_bytes(record[9..11].try_into().unwrap());
        assert!(
            edits.insert((x, y, z), block).is_none(),
            "duplicate cell in the persisted world"
        );
    }
    edits
}

/// Total ender eyes (id 381) across one persisted survival inventory.
fn eye_total(path: &Path) -> i64 {
    let bytes = fs::read(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    let doc: Value = serde_json::from_slice(&bytes).expect("survival inventory must be JSON");
    doc["slots"]
        .as_array()
        .expect("inventory slots array")
        .iter()
        .filter(|stack| stack["id"].as_i64() == Some(381))
        .map(|stack| stack["count"].as_i64().unwrap_or_default())
        .sum()
}

#[test]
fn next155_concurrent_last_eye_consumes_one_eye_and_generates_the_portal_once() {
    let mut server = Server::start();

    // ---- Fixture: eleven lit eyes plus one empty cell, built in creative.
    let mut a = server.join("SeekerA");
    teleport(&mut a, &mut server, "SeekerA", [0.5, 64.0, 0.5]);
    creative_set(&mut a, 36, 120, 64);
    for &(x, z, facing) in &ring_cells() {
        // From the ring centre the placer-driven facing turns every frame
        // inward — the only orientation the activation rule accepts.
        right_click(&mut a, (x, 63, z), 1, 120);
        wait_frame_block(&mut a, x, z, facing, "frame placement");
    }
    // Ender eyes stack to 16 in 1.7.10; eleven insertions plus one spare.
    creative_set(&mut a, 36, EYE_ITEM, 12);
    for &(x, z, facing) in &ring_cells() {
        if (x, z) == CONTESTED {
            continue;
        }
        right_click(&mut a, (x, FRAME_Y as u8, z), 1, EYE_ITEM);
        wait_frame_block(&mut a, x, z, facing | 4, "creative eye insertion");
    }

    // ---- Both racers: survival, exactly one eye each, outside the ring.
    server.console("/gamemode survival SeekerA");
    wait_survival(&mut a, "SeekerA");
    server.console("/give SeekerA 381 1");
    wait_slot36(&mut a, eye_entry(1), "SeekerA eye delivered");
    let mut b = server.join("SeekerB");
    server.console("/gamemode survival SeekerB");
    wait_survival(&mut b, "SeekerB");
    server.console("/give SeekerB 381 1");
    wait_slot36(&mut b, eye_entry(1), "SeekerB eye delivered");
    teleport(&mut a, &mut server, "SeekerA", [5.5, 64.0, 0.5]);
    teleport(&mut b, &mut server, "SeekerB", [5.5, 64.0, 2.5]);
    drain(&mut a, Duration::from_millis(400));
    drain(&mut b, Duration::from_millis(400));

    // ---- Race: two live sockets fire at the SAME empty cell at once.
    let gate = Arc::new(Barrier::new(2));
    let handle_a = {
        let gate = Arc::clone(&gate);
        thread::spawn(move || race(a, gate))
    };
    let handle_b = {
        let gate = Arc::clone(&gate);
        thread::spawn(move || race(b, gate))
    };
    let (a, tally_a) = handle_a.join().expect("SeekerA race thread");
    let (b, tally_b) = handle_b.join().expect("SeekerB race thread");

    // Every observer sees one activation chain: one eye-bit update on the
    // contested cell and each of the nine tiles exactly once.
    let expected_tiles: BTreeMap<(i32, i32), usize> =
        centre_cells().into_iter().map(|cell| (cell, 1)).collect();
    for (who, tally) in [("SeekerA", &tally_a), ("SeekerB", &tally_b)] {
        assert_eq!(
            tally.contested_lit, 1,
            "{who}: the contested cell must light exactly once"
        );
        assert_eq!(
            tally.contested_other, 0,
            "{who}: no other update may touch the contested cell"
        );
        assert_eq!(
            tally.tiles, expected_tiles,
            "{who}: the nine portal tiles must appear exactly once each"
        );
        assert_eq!(
            tally.stray_changes, 0,
            "{who}: no other block change may appear inside the ring"
        );
    }

    // Exactly one eye is consumed across both players. Which client won
    // is an outcome, never a precondition. The loser is echoed its
    // unchanged inventory: a 0x30 whose slot 36 still carries one eye.
    assert_eq!(
        tally_a.empty_dumps + tally_b.empty_dumps,
        1,
        "exactly one client may lose its eye (A emptied {}, B emptied {})",
        tally_a.empty_dumps,
        tally_b.empty_dumps
    );
    assert_eq!(
        tally_a.eye_dumps + tally_b.eye_dumps,
        1,
        "the loser must be echoed its untouched single eye exactly once"
    );
    assert_eq!(
        tally_a.slot_clears + tally_b.slot_clears,
        0,
        "the special-item path debits through 0x30, never 0x2F"
    );
    let a_won = tally_a.empty_dumps == 1;
    let loser_eye_dumps = if a_won {
        tally_b.eye_dumps
    } else {
        tally_a.eye_dumps
    };
    let winner_eye_dumps = if a_won {
        tally_a.eye_dumps
    } else {
        tally_b.eye_dumps
    };
    assert_eq!(
        loser_eye_dumps, 1,
        "the loser keeps its eye: its 0x30 must still show one eye"
    );
    assert_eq!(
        winner_eye_dumps, 0,
        "the winner's consumed eye must not appear in any of its dumps"
    );
    let (mut loser, mut winner, winner_name) = if a_won {
        (b, a, "SeekerA")
    } else {
        (a, b, "SeekerB")
    };

    // ---- Sequential replay on the now-lit cell: the guard is silent.
    // The loser still holds its eye; clicking again must debit nothing.
    right_click(
        &mut loser,
        (CONTESTED.0, FRAME_Y as u8, CONTESTED.1),
        1,
        EYE_ITEM,
    );
    assert_quiet(
        &mut loser,
        &mut winner,
        Duration::from_millis(1_500),
        "loser replay on the lit cell",
    );
    // The winner refills one eye and repeats C08 on the lit cell: no
    // additional consumption, portal unchanged.
    server.console(&format!("/give {winner_name} 381 1"));
    wait_slot36(&mut winner, eye_entry(1), "winner refill delivered");
    right_click(
        &mut winner,
        (CONTESTED.0, FRAME_Y as u8, CONTESTED.1),
        1,
        EYE_ITEM,
    );
    assert_quiet(
        &mut loser,
        &mut winner,
        Duration::from_millis(1_500),
        "winner replay on the lit cell",
    );

    // ---- Clean shutdown, then durable-file cross-checks.
    drop(loser);
    drop(winner);
    thread::sleep(Duration::from_millis(800));
    server.stop();

    let edits = read_irw_edits(&server.root.join("flat.irw"));
    for &(x, z, facing) in &ring_cells() {
        assert_eq!(
            edits.get(&(x, FRAME_Y as u8, z)).copied(),
            Some((FRAME_BLOCK << 4) | u16::from(facing) | 4),
            "ring cell ({x},{z}) must persist as exactly one lit frame"
        );
    }
    let portal: BTreeSet<(i32, i32)> = edits
        .iter()
        .filter(|(_, block)| **block >> 4 == PORTAL_BLOCK)
        .map(|((x, _, z), _)| (*x, *z))
        .collect();
    assert_eq!(
        portal,
        centre_cells().into_iter().collect::<BTreeSet<_>>(),
        "the nine portal tiles must be persisted exactly once"
    );
    assert_eq!(
        edits
            .values()
            .filter(|block| **block >> 4 == FRAME_BLOCK)
            .count(),
        12,
        "exactly twelve frame blocks may exist in the world file"
    );

    // One eye was consumed in total: each racer holds exactly the eye it
    // was granted after the race (winner: refill; loser: its survivor).
    let loser_name = if winner_name == "SeekerA" {
        "SeekerB"
    } else {
        "SeekerA"
    };
    let eyes = |name: &str| {
        eye_total(&server.root.join("players").join(format!(
            "{}.survival.inventory.json",
            name.to_ascii_lowercase()
        )))
    };
    assert_eq!(
        eyes(winner_name),
        1,
        "the winner must hold exactly its post-race refill eye"
    );
    assert_eq!(
        eyes(loser_name),
        1,
        "the loser must have kept exactly its one eye"
    );
}
