//! NEXT-037 / #1020: with two players watching, the fishing float must
//! converge - it spawns once for every observer, announces its flight, and
//! disappears exactly once (never ghosting back, never double-destroyed).
//! The float registry is keyed by owner and every removal path (dry reel,
//! tick expiry, owner disconnect) removes it under the single Fishing lock
//! and emits exactly one Destroy for its never-reused id. This test pins
//! the observable wire contract:
//!   1. both watchers receive the same Spawn Object (0x0E, type 90) whose
//!      data int is the Angler's live EntityID, plus flight teleports;
//!   2. after the Angler's second use-item (dry reel) BOTH watchers see
//!      exactly one Destroy (0x13) for that id, and a long silent window
//!      afterwards contains no re-announce of the id (no ghost);
//!   3. recasting yields a fresh id, and the Angler's disconnect cleanup
//!      delivers that second float's Destroy to both watchers once too.
//!
//! Unmodified Minecraft 1.7.10 client acceptance remains untested here;
//! disposable worlds only.

#[path = "support/packet_client.rs"]
mod packet_client;

use legacy_protocol::{write_frame, write_string, write_varint};
use packet_client::BufferedClient;
use std::fs::{self, File};
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const HOOK: u8 = 90;

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
            "industrialrust-next037-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join("whitelist.json"),
            r#"[{"name":"WatcherA"},{"name":"WatcherB"},{"name":"Angler"}]"#,
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

    fn join(&self, name: &str) -> (BufferedClient, i32) {
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.try_join(name) {
                Ok(session) => return session,
                Err(rejected) => {
                    assert!(attempt < 25, "{name}: join rejected {attempt}x: {rejected}");
                    thread::sleep(Duration::from_millis(300));
                }
            }
        }
    }

    fn try_join(&self, name: &str) -> Result<(BufferedClient, i32), String> {
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
                    let entity = i32::from_be_bytes(body[..4].try_into().unwrap());
                    let marker = Instant::now() + Duration::from_secs(25);
                    loop {
                        assert!(Instant::now() < marker, "{name}: S08 timed out");
                        let Some((id, body)) = pump(&mut socket) else {
                            continue;
                        };
                        match id {
                            0x00 if body.len() == 4 => socket.write_all(&packet(0, &body)).unwrap(),
                            0x08 if body.len() == 33 => return Ok((socket, entity)),
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

fn wait_for(
    client: &mut BufferedClient,
    id: u32,
    predicate: impl Fn(&[u8]) -> bool,
    context: &str,
) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(Instant::now() < deadline, "{context}: timed out");
        let Some((packet_id, body)) = pump(client) else {
            continue;
        };
        if packet_id == 0x00 && body.len() == 4 {
            client.write_all(&packet(0, &body)).unwrap();
        }
        if packet_id == 0x40 {
            panic!("{context}: disconnected: {body:?}");
        }
        if packet_id == id && predicate(&body) {
            return body;
        }
    }
}

/// Observe `client` for `window` and assert no packet re-references the
/// float: no second Destroy, no re-Spawn, no teleport of a gone entity.
fn expect_silent_about(client: &mut BufferedClient, float: i32, window: Duration, who: &str) {
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        let Some((packet_id, body)) = pump(client) else {
            continue;
        };
        if packet_id == 0x00 && body.len() == 4 {
            client.write_all(&packet(0, &body)).unwrap();
        }
        match packet_id {
            0x13 => {
                assert_ne!(
                    destroy_id(&body),
                    Some(float),
                    "{who}: second Destroy for float {float}"
                );
            }
            0x0e => {
                assert_ne!(
                    hook_spawn(&body).map(|(id, _)| id),
                    Some(float),
                    "{who}: ghost re-Spawn of float {float}"
                );
            }
            0x18 => {
                assert_ne!(
                    teleport_entity(&body),
                    Some(float),
                    "{who}: teleport of removed float {float}"
                );
            }
            _ => {}
        }
    }
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

/// Spawn Object (0x0E) type 90: [entity VarInt][90][x i32][y i32][z i32]
/// [pitch u8][yaw u8][owner i32][vx i16][vy i16][vz i16].
fn hook_spawn(body: &[u8]) -> Option<(i32, i32)> {
    let id = entity_varint(body)?;
    let head = varint_len(body);
    if body.len() < head + 19 || body[head] != HOOK {
        return None;
    }
    let owner = i32::from_be_bytes(body[head + 15..head + 19].try_into().unwrap());
    Some((id, owner))
}

/// Destroy Entities (0x13) with a single entity.
fn destroy_id(body: &[u8]) -> Option<i32> {
    (body.len() == 5 && body[0] == 1).then(|| i32::from_be_bytes(body[1..5].try_into().unwrap()))
}

/// Entity Teleport (0x18): leading entity i32.
fn teleport_entity(body: &[u8]) -> Option<i32> {
    (body.len() == 18).then(|| i32::from_be_bytes(body[..4].try_into().unwrap()))
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
    let s08 = wait_for(
        client,
        0x08,
        |body| {
            body.len() == 33
                && (feet(body)[0] - target[0]).abs() <= 1.0 / 16.0
                && (feet(body)[1] - target[1]).abs() <= 1.0 / 16.0
        },
        &format!("{name}: teleport S08"),
    );
    assert_eq!(&s08[16..24], &target[2].to_be_bytes(), "tp S08 z");
    client.write_all(&position(target, 0.0, 0.0)).unwrap();
}

/// C08 air use (-1, 255, -1) face -1 claiming the held fishing rod: the
/// first packet casts the float, the second reels it in (dry = no loot).
/// The claim must match the held stack exactly - including the durability
/// damage a reel just applied - exactly like a real client's snapshot.
fn use_rod(client: &mut BufferedClient, damage: i16) {
    let mut body = Vec::new();
    body.extend_from_slice(&(-1i32).to_be_bytes());
    body.push(255);
    body.extend_from_slice(&(-1i32).to_be_bytes());
    body.push((-1i8) as u8);
    body.extend_from_slice(&346i16.to_be_bytes());
    body.push(1);
    body.extend_from_slice(&damage.to_be_bytes());
    body.extend_from_slice(&(-1i16).to_be_bytes());
    body.extend_from_slice(&[0, 0, 0]);
    client.write_all(&packet(8, &body)).unwrap();
}

/// Read the held rod's current durability from the owner's 0x30 echo:
/// entry [0x01,0x5A][count][damage i16][nbt -1] (window 0 dumps are flat).
fn observe_rod_damage(client: &mut BufferedClient) -> i16 {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(Instant::now() < deadline, "rod echo timed out");
        let Some((packet_id, body)) = pump(client) else {
            continue;
        };
        if packet_id == 0x00 && body.len() == 4 {
            client.write_all(&packet(0, &body)).unwrap();
        }
        if packet_id == 0x40 {
            panic!("rod echo: disconnected: {body:?}");
        }
        if packet_id == 0x30 {
            for w in body.windows(7) {
                if w[0..2] == [1, 90] && w[2] == 1 && w[5..7] == [255, 255] {
                    return i16::from_be_bytes([w[3], w[4]]);
                }
            }
        }
    }
}

/// Keep the Angler's stream pumping (keep-alives) for a while.
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
fn next037_fishing_float_appears_once_and_disappears_once_for_both_watchers() {
    let mut server = Server::start();
    let (mut watcher_a, _) = server.join("WatcherA");
    let (mut watcher_b, _) = server.join("WatcherB");
    let (mut angler, angler_id) = server.join("Angler");

    server.console("/gamemode survival Angler");
    wait_for(
        &mut angler,
        0x39,
        |body| body.first() == Some(&0),
        "gamemode switch",
    );
    server.console("/give Angler 346 1");
    wait_for(
        &mut angler,
        0x30,
        // 346 = 0x015A: entry [item][count][dmg][nbt -1].
        |body| body.windows(7).any(|w| w == [1, 90, 1, 0, 0, 255, 255]),
        "rod delivered",
    );
    teleport(&mut angler, &mut server, "Angler", [4.5, 64.0, 0.5]);
    // Face +x over the flat plain so the float flies and announces motion.
    angler
        .write_all(&position([4.5, 64.0, 0.5], -90.0, 0.0))
        .unwrap();

    // --- Phase 1: cast, both watchers see one Spawn each with the live
    // Angler id as the data owner, plus flight teleports.
    use_rod(&mut angler, 0);
    let spawn_a = wait_for(
        &mut watcher_a,
        0x0e,
        |body| hook_spawn(body).is_some(),
        "watcher A: hook spawn",
    );
    let (float_1, owner_a) = hook_spawn(&spawn_a).unwrap();
    let spawn_b = wait_for(
        &mut watcher_b,
        0x0e,
        |body| hook_spawn(body) == Some((float_1, angler_id)),
        "watcher B: same hook spawn",
    );
    let (_, owner_b) = hook_spawn(&spawn_b).unwrap();
    assert_eq!(owner_a, angler_id, "spawn data carries the Angler");
    assert_eq!(owner_b, angler_id);

    wait_for(
        &mut watcher_a,
        0x18,
        |body| teleport_entity(body) == Some(float_1),
        "watcher A: float teleports while flying",
    );
    wait_for(
        &mut watcher_b,
        0x18,
        |body| teleport_entity(body) == Some(float_1),
        "watcher B: float teleports while flying",
    );

    // --- Phase 1 removal: a second use-item dries the reel. The float is
    // destroyed exactly once for each watcher.
    idle(&mut angler, Duration::from_millis(150));
    use_rod(&mut angler, 0);
    wait_for(
        &mut watcher_a,
        0x13,
        |body| destroy_id(body) == Some(float_1),
        "watcher A: float destroyed once",
    );
    wait_for(
        &mut watcher_b,
        0x13,
        |body| destroy_id(body) == Some(float_1),
        "watcher B: float destroyed once",
    );
    expect_silent_about(&mut watcher_a, float_1, Duration::from_millis(1_200), "A");
    expect_silent_about(&mut watcher_b, float_1, Duration::from_millis(1_200), "B");

    // --- Phase 2: a recast gets a fresh id; the Angler's disconnect must
    // clean the float up for both watchers with exactly one Destroy.
    // The dry reel wore the rod: claim with the current durability.
    let rod_damage = observe_rod_damage(&mut angler);
    assert!(rod_damage >= 1, "the reel must have worn the rod");
    use_rod(&mut angler, rod_damage);
    let spawn2 = wait_for(
        &mut watcher_a,
        0x0e,
        |body| hook_spawn(body).is_some_and(|(id, _)| id != float_1),
        "watcher A: recast spawns a fresh float",
    );
    let (float_2, _) = hook_spawn(&spawn2).unwrap();
    assert_ne!(float_2, float_1, "float ids are never reused");
    wait_for(
        &mut watcher_b,
        0x0e,
        |body| hook_spawn(body) == Some((float_2, angler_id)),
        "watcher B: same fresh spawn",
    );

    drop(angler);
    wait_for(
        &mut watcher_a,
        0x13,
        |body| destroy_id(body) == Some(float_2),
        "watcher A: disconnect destroys the float",
    );
    wait_for(
        &mut watcher_b,
        0x13,
        |body| destroy_id(body) == Some(float_2),
        "watcher B: disconnect destroys the float",
    );
    expect_silent_about(&mut watcher_a, float_2, Duration::from_millis(900), "A");
    expect_silent_about(&mut watcher_b, float_2, Duration::from_millis(900), "B");

    server.stop();
}
