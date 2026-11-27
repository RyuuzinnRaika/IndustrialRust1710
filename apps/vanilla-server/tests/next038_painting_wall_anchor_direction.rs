//! NEXT-038 / #1021: Spawn Painting (0x10) must anchor on the solid wall
//! block, not the air cell in front of it. The 1.7.10 client feeds the
//! packet coordinates straight into EntityHanging.setDirection, which snaps
//! the painting plane 9/16 block *out of the packet cell* along the direction
//! byte; an air-cell anchor therefore renders every painting floating one
//! block off the wall. This test pins the observable wire contract with two
//! clients:
//!   1. each of the four horizontal click faces yields exactly one 0x10 whose
//!      Int anchor equals the clicked wall block and whose direction byte
//!      matches the vanilla mapping (north face 2 -> 2, south 3 -> 0,
//!      west 4 -> 1, east 5 -> 3), with a bounded art title and an ascending
//!      hanging EntityID;
//!   2. leaving the painting chunks sends exactly one Destroy (0x13) per
//!      painting id and no re-Spawn while away;
//!   3. rejoining the chunks replays byte-identical 0x10 bodies, so a late
//!      observer anchors the very same way as the placing client.
//!
//! Unmodified Minecraft 1.7.10 client acceptance remains untested here;
//! disposable worlds only.

#[path = "support/packet_client.rs"]
mod packet_client;

use legacy_protocol::{write_frame, write_string, write_varint};
use packet_client::BufferedClient;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, File};
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Hanging entities start at this EntityID and only ascend.
const FIRST_HANGING_ID: i32 = 1_600_000;

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
            "industrialrust-next038-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join("whitelist.json"),
            r#"[{"name":"Watcher"},{"name":"Painter"}]"#,
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

/// Spawn Painting (0x10): [entity VarInt][title string][x i32][y i32][z i32]
/// [direction i32]. Returns (id, title, anchor, direction).
fn painting_spawn(body: &[u8]) -> Option<(i32, Vec<u8>, [i32; 3], i32)> {
    let id = entity_varint(body)?;
    let head = varint_len(body);
    let title_len = usize::from(*body.get(head)?);
    if title_len == 0 || title_len > 13 {
        return None;
    }
    let coords = head + 1 + title_len;
    if body.len() != coords + 16 {
        return None;
    }
    let anchor = [
        i32::from_be_bytes(body[coords..coords + 4].try_into().unwrap()),
        i32::from_be_bytes(body[coords + 4..coords + 8].try_into().unwrap()),
        i32::from_be_bytes(body[coords + 8..coords + 12].try_into().unwrap()),
    ];
    let direction = i32::from_be_bytes(body[coords + 12..coords + 16].try_into().unwrap());
    Some((id, body[head + 1..coords].to_vec(), anchor, direction))
}

/// Destroy Entities (0x13) with a single entity.
fn destroy_id(body: &[u8]) -> Option<i32> {
    (body.len() == 5 && body[0] == 1).then(|| i32::from_be_bytes(body[1..5].try_into().unwrap()))
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

/// Handshake-safe pump: answers keep-alives, refuses disconnects, and hands
/// the caller every other packet.
fn next_packet(
    stash: &mut VecDeque<(u32, Vec<u8>)>,
    client: &mut BufferedClient,
    context: &str,
) -> (u32, Vec<u8>) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(Instant::now() < deadline, "{context}: timed out");
        if let Some(packet) = stash.pop_front() {
            return packet;
        }
        let Some((id, body)) = pump(client) else {
            continue;
        };
        if id == 0x00 && body.len() == 4 {
            client.write_all(&packet(0, &body)).unwrap();
            continue;
        }
        if id == 0x40 {
            panic!("{context}: disconnected: {body:?}");
        }
        return (id, body);
    }
}

/// Console teleport that keeps a caller-provided stash fed: hanging Spawn
/// and Destroy packets can arrive before the S08 confirmation, so they are
/// parked instead of discarded.
fn teleport(
    client: &mut BufferedClient,
    server: &mut Server,
    name: &str,
    target: [f64; 3],
    stash: &mut VecDeque<(u32, Vec<u8>)>,
) {
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
        if matches!(id, 0x10 | 0x13) {
            stash.push_back((id, body));
        }
    }
}

/// C08 place against a wall face claiming one held Painting (321).
fn place_painting(client: &mut BufferedClient, wall: (i32, i32, i32), face: i8) {
    let mut body = Vec::new();
    body.extend_from_slice(&wall.0.to_be_bytes());
    body.push(wall.1 as u8);
    body.extend_from_slice(&wall.2.to_be_bytes());
    body.push(face as u8);
    body.extend_from_slice(&321i16.to_be_bytes());
    body.push(1);
    body.extend_from_slice(&0i16.to_be_bytes());
    body.push(0); // no NBT: single TAG_End byte
    body.extend_from_slice(&[0, 0, 0]);
    client.write_all(&packet(8, &body)).unwrap();
}

/// Keep the stream pumping (keep-alives) for a while.
fn idle(client: &mut BufferedClient, duration: Duration) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        if let Some((packet_id, body)) = pump(client) {
            if packet_id == 0x00 && body.len() == 4 {
                client.write_all(&packet(0, &body)).unwrap();
            }
        }
    }
}

#[test]
fn next038_painting_packets_anchor_the_wall_for_both_placing_and_joining_clients() {
    let mut server = Server::start();
    let mut watcher = server.join("Watcher");
    let mut painter = server.join("Painter");

    // Four isolated stone walls, one per horizontal direction, around the
    // painter's perch. Isolated single blocks force 1x1 art motifs, so the
    // packet anchor is unambiguous.
    for wall in [(0, 64, -2), (0, 64, 2), (-2, 64, 0), (2, 64, 0)] {
        server.console(&format!("/setblock {} {} {} stone", wall.0, wall.1, wall.2));
    }
    server.console("/give Painter 321 1");
    let mut stash = VecDeque::new();
    // 321 = 0x0141: window-0 entry [item][count][dmg][nbt -1]. Everything
    // else (chunk maps, welcome chat, keep-alive replies) is discarded.
    {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            assert!(Instant::now() < deadline, "painting delivery timed out");
            let Some((id, body)) = pump(&mut painter) else {
                continue;
            };
            if id == 0x00 && body.len() == 4 {
                painter.write_all(&packet(0, &body)).unwrap();
            }
            if id == 0x30 && body.windows(6).any(|w| w == [1, 65, 1, 0, 0, 0]) {
                break;
            }
        }
    }
    teleport(
        &mut painter,
        &mut server,
        "Painter",
        [0.5, 64.0, 0.5],
        &mut stash,
    );
    teleport(
        &mut watcher,
        &mut server,
        "Watcher",
        [8.5, 64.0, 8.5],
        &mut stash,
    );

    // (clicked wall, clicked face, vanilla direction byte) per the 1.7.10
    // EntityHanging mapping. Each wall is clicked from inside the ring.
    let cases = [
        ((0, 64, -2), 3i8, 0i32),
        ((0, 64, 2), 2, 2),
        ((-2, 64, 0), 5, 3),
        ((2, 64, 0), 4, 1),
    ];
    let mut originals: BTreeMap<i32, Vec<u8>> = BTreeMap::new();
    for &(wall, face, direction) in &cases {
        place_painting(&mut painter, wall, face);
        let (id, body) = loop {
            let (id, body) = next_packet(&mut stash, &mut watcher, "painting spawn");
            if id == 0x10 {
                if let Some((seen_id, title, anchor, seen_direction)) = painting_spawn(&body) {
                    assert_eq!(
                        anchor,
                        [wall.0, wall.1, wall.2],
                        "wall {:?} face {}: 0x10 must anchor on the clicked wall block, \
                         not the air cell",
                        wall,
                        face
                    );
                    assert_eq!(
                        direction, seen_direction,
                        "wall {:?} face {} direction byte",
                        wall, face
                    );
                    assert!(
                        String::from_utf8(title).is_ok_and(|t| !t.is_empty() && t.len() <= 13),
                        "vanilla art title bound"
                    );
                    assert!(
                        seen_id >= FIRST_HANGING_ID
                            && seen_id > *originals.keys().next_back().unwrap_or(&0),
                        "hanging ids start at {FIRST_HANGING_ID} and ascend"
                    );
                    break (seen_id, body);
                }
            }
        };
        // A duplicate immediate re-Spawn for the same id is a bug; the stream
        // must stay quiet about this painting for a short window.
        let quiet = Instant::now() + Duration::from_millis(150);
        while Instant::now() < quiet {
            if let Some((id2, body2)) = pump(&mut watcher) {
                if id2 == 0x00 && body2.len() == 4 {
                    watcher.write_all(&packet(0, &body2)).unwrap();
                }
                if id2 == 0x10 {
                    if let Some((dup, _, _, _)) = painting_spawn(&body2) {
                        assert_ne!(dup, id, "painting {id} announced twice");
                    }
                }
            }
        }
        originals.insert(id, body);
        idle(&mut painter, Duration::from_millis(60));
    }
    assert_eq!(originals.len(), 4, "four paintings announced once each");

    // --- Phase 2: leaving the painting chunks destroys every painting for
    // the watcher, exactly once each, with no ghost re-Spawn while away.
    teleport(
        &mut watcher,
        &mut server,
        "Watcher",
        [200.5, 64.0, 200.5],
        &mut stash,
    );
    let mut destroyed: BTreeSet<i32> = BTreeSet::new();
    while destroyed.len() < 4 {
        let (id, body) = next_packet(&mut stash, &mut watcher, "chunk-leave destroy");
        if id == 0x13 {
            if let Some(victim) = destroy_id(&body) {
                if originals.contains_key(&victim) {
                    assert!(
                        destroyed.insert(victim),
                        "second Destroy for painting {victim}"
                    );
                }
            }
        }
    }
    let away = Instant::now() + Duration::from_millis(900);
    while Instant::now() < away {
        let Some((id, body)) = pump(&mut watcher) else {
            continue;
        };
        if id == 0x00 && body.len() == 4 {
            watcher.write_all(&packet(0, &body)).unwrap();
            continue;
        }
        if id == 0x13 {
            if let Some(victim) = destroy_id(&body) {
                assert!(
                    !originals.contains_key(&victim),
                    "second Destroy for painting {victim}"
                );
            }
        }
        if id == 0x10 {
            if let Some((ghost, _, _, _)) = painting_spawn(&body) {
                assert!(
                    !originals.contains_key(&ghost),
                    "ghost re-Spawn of painting {ghost} while far away"
                );
            }
        }
    }

    // --- Phase 3: rejoining the chunks replays the very same 0x10 bodies,
    // so a late or returning observer anchors identically.
    teleport(
        &mut watcher,
        &mut server,
        "Watcher",
        [8.5, 64.0, 8.5],
        &mut stash,
    );
    let mut replayed: BTreeMap<i32, Vec<u8>> = BTreeMap::new();
    while replayed.len() < 4 {
        let (id, body) = next_packet(&mut stash, &mut watcher, "chunk-rejoin replay");
        if id == 0x10 {
            if let Some((seen_id, _, anchor, direction)) = painting_spawn(&body) {
                let Some((_, expected_body)) = originals.get_key_value(&seen_id) else {
                    panic!("rejoin spawned unknown painting {seen_id}");
                };
                let (_, _, want_anchor, want_direction) =
                    painting_spawn(expected_body).expect("stored body parses");
                assert_eq!(
                    (anchor, direction),
                    (want_anchor, want_direction),
                    "rejoin replay must keep the wall anchor for painting {seen_id}"
                );
                assert!(
                    replayed.insert(seen_id, body).is_none(),
                    "painting {seen_id} replayed twice"
                );
            }
        }
    }
    for (id, body) in &replayed {
        assert_eq!(
            body, &originals[id],
            "rejoin replay of painting {id} is not byte-identical"
        );
    }

    server.stop();
}
