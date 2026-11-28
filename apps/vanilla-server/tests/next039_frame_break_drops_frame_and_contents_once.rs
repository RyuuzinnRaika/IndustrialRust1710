//! NEXT-039 / #1022: breaking an item frame must separate its drops - the
//! frame item (389) and the stored item - while each appears exactly once and
//! stays synchronized for every observer. The server owns the whole
//! lifecycle: placement consumes the held frame, one interact inserts the
//! held stack (debit) or rotates the display, and one attack removes the
//! entity, journals the hanging file plus the ground-item pool together,
//! broadcasts a single Destroy and one Spawn Object (type 2) + metadata pair
//! per drop. This test pins the observable wire contract with two clients:
//!   1. the survival placement echo (0x2F) plus the frame Spawn Object
//!      (0x0E, type 71, vanilla direction data) reach the observer;
//!   2. one interact inserts the held item and the observer sees the frame
//!      metadata (0x1C) carrying the displayed stack;
//!   3. one attack destroys the frame: exactly one 0x13 for the frame id,
//!      exactly two distinct ground-item spawns - one carrying the frame
//!      item 389, one carrying the stored item - each announced once, and a
//!      quiet window afterwards shows no ghost re-Spawn or second Destroy.
//!
//! Unmodified Minecraft 1.7.10 client acceptance remains untested here;
//! disposable worlds only.

#[path = "support/packet_client.rs"]
mod packet_client;

use legacy_protocol::{write_frame, write_string, write_varint};
use packet_client::BufferedClient;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Hanging entities start at this EntityID and only ascend.
const FIRST_HANGING_ID: i32 = 1_600_000;
/// Ground items live above 1_000_000_000 and also only ascend.
const FIRST_GROUND_ID: i32 = 1_000_000_000;
const FRAME: i32 = 389;

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
            "industrialrust-next039-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join("whitelist.json"),
            r#"[{"name":"Breaker"},{"name":"Watcher"}]"#,
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

fn pump(socket: &mut BufferedClient) -> Option<(u32, Vec<u8>)> {
    socket.poll_packet().expect("read Protocol 5 packet")
}

fn entity_varint(body: &[u8]) -> Option<i32> {
    let mut rest = body;
    let mut id: i32 = 0;
    let mut shift = 0;
    loop {
        let byte = *rest.first()?;
        rest = &rest[1..];
        id |= ((byte & 0x7f) as i32) << shift;
        if byte & 0x80 == 0 {
            return Some(id);
        }
        shift += 7;
        if shift > 28 {
            return None;
        }
    }
}

fn varint_len(body: &[u8]) -> usize {
    body.iter()
        .take(5)
        .position(|byte| byte & 0x80 == 0)
        .map_or(5, |index| index + 1)
}

/// Spawn Object (0x0E) type 71 (item frame): [entity VarInt][71][x i32]
/// [y i32][z i32][pitch u8][yaw u8][direction i32].
fn frame_spawn(body: &[u8]) -> Option<(i32, i32)> {
    let id = entity_varint(body)?;
    let head = varint_len(body);
    if body.len() != head + 19 || body[head] != 71 {
        return None;
    }
    let direction = i32::from_be_bytes(body[head + 15..head + 19].try_into().unwrap());
    Some((id, direction))
}

/// Spawn Object (0x0E) type 2 (dropped item).
fn item_spawn_id(body: &[u8]) -> Option<i32> {
    let id = entity_varint(body)?;
    let head = varint_len(body);
    (body.len() == head + 19 && body[head] == 2).then_some(id)
}

/// Destroy Entities (0x13) with a single entity.
fn destroy_id(body: &[u8]) -> Option<i32> {
    (body.len() == 5 && body[0] == 1).then(|| i32::from_be_bytes(body[1..5].try_into().unwrap()))
}

/// Entity Metadata (0x1C) for one entity, as (entity id, whole body).
fn metadata_target(body: &[u8]) -> Option<i32> {
    (body.len() >= 5).then(|| i32::from_be_bytes(body[..4].try_into().unwrap()))
}

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
        if id == 0x00 && body.len() == 4 {
            client.write_all(&packet(0, &body)).unwrap();
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

/// Wait until the client echoes a window-0 dump containing the flat entry.
fn wait_for_entry(client: &mut BufferedClient, entry: [u8; 6], context: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(Instant::now() < deadline, "{context}: timed out");
        let Some((id, body)) = pump(client) else {
            continue;
        };
        if id == 0x00 && body.len() == 4 {
            client.write_all(&packet(0, &body)).unwrap();
        }
        if id == 0x40 {
            panic!("{context}: disconnected: {body:?}");
        }
        if id == 0x30 && body.windows(6).any(|w| w == entry) {
            return;
        }
    }
}

/// C08 place against a wall face claiming one held item.
fn place(client: &mut BufferedClient, wall: (i32, i32, i32), face: i8, item: i16) {
    let mut body = Vec::new();
    body.extend_from_slice(&wall.0.to_be_bytes());
    body.push(wall.1 as u8);
    body.extend_from_slice(&wall.2.to_be_bytes());
    body.push(face as u8);
    body.extend_from_slice(&item.to_be_bytes());
    body.push(1);
    body.extend_from_slice(&0i16.to_be_bytes());
    body.push(0); // no NBT: single TAG_End byte
    body.extend_from_slice(&[0, 0, 0]);
    client.write_all(&packet(8, &body)).unwrap();
}

/// C02 Use Entity: [target i32][action i8].
fn use_entity(client: &mut BufferedClient, target: i32, action: i8) {
    let mut body = Vec::new();
    body.extend_from_slice(&target.to_be_bytes());
    body.push(action as u8);
    client.write_all(&packet(2, &body)).unwrap();
}

#[test]
fn next039_frame_break_drops_the_frame_and_its_contents_exactly_once() {
    let mut server = Server::start();
    let mut breaker = server.join("Breaker");
    let mut watcher = server.join("Watcher");

    server.console("/gamemode survival Breaker");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(Instant::now() < deadline, "gamemode switch timed out");
        let Some((id, body)) = pump(&mut breaker) else {
            continue;
        };
        if id == 0x00 && body.len() == 4 {
            breaker.write_all(&packet(0, &body)).unwrap();
        }
        if id == 0x39 && body.first() == Some(&0) {
            break;
        }
    }
    server.console("/setblock 0 64 -2 stone");
    // Give order matters: /give fills the selected hotbar slot first, so the
    // frame is granted, placed, and only then is the display item granted
    // into the now-empty held slot.
    server.console("/give Breaker 389 1");
    // 389 = 0x0185: window-0 entry [item][count][dmg][nbt -1].
    wait_for_entry(&mut breaker, [1, 133, 1, 0, 0, 0], "frame delivered");
    teleport(&mut breaker, &mut server, "Breaker", [0.5, 64.0, 0.5]);
    teleport(&mut watcher, &mut server, "Watcher", [8.5, 64.0, 8.5]);

    // --- Phase 1: survival placement. The held frame is consumed and the
    // observer sees the item-frame Spawn Object with the vanilla direction.
    place(&mut breaker, (0, 64, -2), 3, FRAME as i16);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(Instant::now() < deadline, "placement consumption timed out");
        let Some((id, body)) = pump(&mut breaker) else {
            continue;
        };
        if id == 0x00 && body.len() == 4 {
            breaker.write_all(&packet(0, &body)).unwrap();
        }
        // 0x2F Set Slot (window 0, hotbar slot 36) now empty.
        if id == 0x2f
            && body.len() >= 5
            && body[0] == 0
            && body[1..3] == [0, 36]
            && body[3..5] == [255, 255]
        {
            break;
        }
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    let frame_id = loop {
        assert!(Instant::now() < deadline, "frame spawn timed out");
        let Some((id, body)) = pump(&mut watcher) else {
            continue;
        };
        if id == 0x00 && body.len() == 4 {
            watcher.write_all(&packet(0, &body)).unwrap();
        }
        if id == 0x40 {
            panic!("frame spawn: disconnected: {body:?}");
        }
        if id == 0x0e {
            if let Some((seen, direction)) = frame_spawn(&body) {
                assert!(seen >= FIRST_HANGING_ID, "hanging ids start at 1600000");
                // Clicked north face 3 maps to vanilla hanging direction 0.
                assert_eq!(direction, 0, "frame direction byte for clicked face 3");
                break seen;
            }
        }
    };

    // --- Phase 2: insert the held stack. The breaker's copy is debited and
    // the observer sees the frame metadata carry the displayed item.
    server.console("/give Breaker 351 3");
    // 351 = 0x015F.
    wait_for_entry(&mut breaker, [1, 95, 3, 0, 0, 0], "ink delivered");
    use_entity(&mut breaker, frame_id, 0);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(Instant::now() < deadline, "insert debit timed out");
        let Some((id, body)) = pump(&mut breaker) else {
            continue;
        };
        if id == 0x00 && body.len() == 4 {
            breaker.write_all(&packet(0, &body)).unwrap();
        }
        // 0x2F Set Slot: slot 36 now holds [351 x2] after the debit.
        if id == 0x2f
            && body.len() >= 6
            && body[0] == 0
            && body[1..3] == [0, 36]
            && body[3..5] == [1, 95]
            && body[5] == 2
        {
            break;
        }
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(
            Instant::now() < deadline,
            "displayed item metadata timed out"
        );
        let Some((id, body)) = pump(&mut watcher) else {
            continue;
        };
        if id == 0x00 && body.len() == 4 {
            watcher.write_all(&packet(0, &body)).unwrap();
        }
        if id == 0x40 {
            panic!("insert: disconnected: {body:?}");
        }
        if id == 0x1c && metadata_target(&body) == Some(frame_id) {
            // The spawn-time metadata may still be queued; only the insert
            // update carries the displayed stack.
            if body.windows(2).any(|w| w == [1, 95]) {
                break;
            }
        }
    }

    // --- Phase 3: one attack. Exactly one Destroy for the frame, and
    // exactly two ground-item spawns (frame 389 + stored 351), each once.
    use_entity(&mut breaker, frame_id, 1);
    let mut dropped: BTreeMap<i32, Option<Vec<u8>>> = BTreeMap::new();
    let mut destroyed = 0;
    let deadline = Instant::now() + Duration::from_secs(20);
    while destroyed != 1 || dropped.len() != 2 {
        assert!(
            Instant::now() < deadline,
            "break drops timed out (destroyed {destroyed}, drops {:?})",
            dropped.keys().collect::<Vec<_>>()
        );
        let Some((id, body)) = pump(&mut watcher) else {
            continue;
        };
        if id == 0x00 && body.len() == 4 {
            watcher.write_all(&packet(0, &body)).unwrap();
            continue;
        }
        if id == 0x40 {
            panic!("break: disconnected: {body:?}");
        }
        if id == 0x13 && destroy_id(&body) == Some(frame_id) {
            assert_eq!(destroyed, 0, "frame destroyed twice for the watcher");
            destroyed += 1;
        }
        if id == 0x0e {
            if let Some(ground) = item_spawn_id(&body) {
                assert!(ground > FIRST_GROUND_ID, "ground ids start above 1e9");
                assert_ne!(ground, frame_id, "a drop reused the frame id");
                assert!(
                    dropped.insert(ground, None).is_none(),
                    "ground item {ground} announced twice"
                );
            }
        }
        if id == 0x1c {
            if let Some(ground) = metadata_target(&body) {
                if let Some(slot @ None) = dropped.get_mut(&ground) {
                    *slot = Some(body);
                }
            }
        }
        if destroyed == 1 && dropped.len() == 2 {
            // Both metadata payloads must arrive before the quiet window.
            let settled = Instant::now() + Duration::from_millis(1_500);
            while Instant::now() < settled && dropped.values().any(Option::is_none) {
                let Some((id, body)) = pump(&mut watcher) else {
                    continue;
                };
                if id == 0x00 && body.len() == 4 {
                    watcher.write_all(&packet(0, &body)).unwrap();
                }
                if id == 0x1c {
                    if let Some(ground) = metadata_target(&body) {
                        if let Some(slot @ None) = dropped.get_mut(&ground) {
                            *slot = Some(body);
                        }
                    }
                }
            }
            assert_eq!(
                dropped.values().filter(|body| body.is_some()).count(),
                2,
                "both drops must announce their item metadata"
            );
            let mut payloads: Vec<&[u8]> = Vec::new();
            for payload in dropped.values() {
                payloads.push(payload.as_deref().expect("both payloads present"));
            }
            let carries = |bytes: [u8; 2]| {
                payloads
                    .iter()
                    .any(|payload| payload.windows(2).any(|w| w == bytes))
            };
            assert!(carries([1, 133]), "one drop must carry the frame item 389");
            assert!(
                carries([1, 95]),
                "the other drop must carry the stored item 351"
            );

            // Quiet window: no ghost re-Spawn, no second Destroy, and no
            // third drop of any kind.
            let quiet = Instant::now() + Duration::from_millis(900);
            while Instant::now() < quiet {
                let Some((id, body)) = pump(&mut watcher) else {
                    continue;
                };
                if id == 0x00 && body.len() == 4 {
                    watcher.write_all(&packet(0, &body)).unwrap();
                    continue;
                }
                if id == 0x13 {
                    assert_ne!(
                        destroy_id(&body),
                        Some(frame_id),
                        "second Destroy for frame {frame_id}"
                    );
                }
                if id == 0x0e {
                    if let Some(ghost) = item_spawn_id(&body) {
                        assert!(
                            !dropped.contains_key(&ghost),
                            "ghost re-Spawn of ground item {ghost}"
                        );
                        panic!("a third drop appeared: {ghost}");
                    }
                }
            }
            break;
        }
    }

    server.stop();
}
