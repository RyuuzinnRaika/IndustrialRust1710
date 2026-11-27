//! NEXT-032 / #1015: an observer that enters view distance AFTER an
//! equipment change must still receive the correct armor and held item.
//! The server publishes the server-owned equipment snapshot to the Hub on
//! held selection, Creative grant, respawn and a 10-tick diff refresh; the
//! Hub replays it right after every Spawn delivery — the join snapshot
//! (main.rs previous loop) and both directions of the view-entry
//! transition (Hub::move_player). This test pins the observable wire
//! contract:
//!   1. an observer online when gear changes receives Entity Equipment
//!      (0x04) for the changed slots;
//!   2. a player that joins later receives, after the target's Spawn, a
//!      0x04 per non-empty equipment slot with the true item IDs;
//!   3. after leaving and re-entering view distance the re-entry Spawn is
//!      again followed by the full equipment replay.
//!
//! Unmodified Minecraft 1.7.10 client acceptance remains untested here;
//! disposable data only.

#[path = "support/packet_client.rs"]
mod packet_client;

use legacy_protocol::{write_frame, write_string, write_varint};
use packet_client::BufferedClient;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
            "industrialrust-next032-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join("whitelist.json"),
            r#"[{"name":"Trampler"},{"name":"Watcher"},{"name":"Latecomer"}]"#,
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
                Ok(client) => return client,
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
                0x08 if body.len() == 33 => return Ok(socket),
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

/// Entity Equipment (0x04): i32 entity, i16 equipment slot, Slot payload.
fn equipment_event(body: &[u8]) -> Option<(i32, i16, i16)> {
    (body.len() >= 8).then(|| {
        (
            i32::from_be_bytes(body[..4].try_into().unwrap()),
            i16::from_be_bytes(body[4..6].try_into().unwrap()),
            i16::from_be_bytes(body[6..8].try_into().unwrap()),
        )
    })
}

fn wait_for(
    client: &mut BufferedClient,
    id: u32,
    predicate: impl Fn(&[u8]) -> bool,
    context: &str,
) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(15);
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

fn give(client: &mut BufferedClient, slot: i16, id: i16, count: u8) {
    let mut give_packet = slot.to_be_bytes().to_vec();
    give_packet.extend_from_slice(&id.to_be_bytes());
    give_packet.push(count);
    give_packet.extend_from_slice(&0i16.to_be_bytes());
    give_packet.push(0); // no NBT: single TAG_End byte
    client.write_all(&packet(0x10, &give_packet)).unwrap();
    wait_for(
        client,
        0x2f,
        |body| body.len() >= 8 && body[3..5] == id.to_be_bytes(),
        "slot echo",
    );
}

fn feet(s08_body: &[u8]) -> [f64; 2] {
    [
        f64::from_be_bytes(s08_body[..8].try_into().unwrap()),
        f64::from_be_bytes(s08_body[8..16].try_into().unwrap()) - f64::from(1.62_f32),
    ]
}

fn position(xyz: [f64; 3]) -> Vec<u8> {
    let mut body = Vec::new();
    for value in [xyz[0], xyz[1], xyz[1] + 1.62, xyz[2]] {
        body.extend_from_slice(&value.to_be_bytes());
    }
    body.extend_from_slice(&0.0f32.to_be_bytes());
    body.extend_from_slice(&0.0f32.to_be_bytes());
    body.push(1);
    packet(6, &body)
}

fn teleport(client: &mut BufferedClient, server: &mut Server, target: [f64; 3]) {
    server.console(&format!(
        "/tp Trampler {} {} {}",
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
        "teleport S08",
    );
    assert_eq!(&s08[16..24], &target[2].to_be_bytes(), "tp S08 z");
    client.write_all(&position(target)).unwrap();
}

/// Locate the entity ID of the named player's 0x0c Spawn on this stream.
fn named_spawn(client: &mut BufferedClient, name: &str, context: &str) -> i32 {
    let body = wait_for(
        client,
        0x0c,
        |body| body.windows(name.len()).any(|w| w == name.as_bytes()),
        context,
    );
    entity_varint(&body).expect("spawn entity varint")
}

/// The gear set (slot, item) the observer currently holds for `target`,
/// collected over the given window; also reports whether any 0x04 for the
/// target arrived (used to prove the replay happened after the Spawn).
fn collect_gear(
    client: &mut BufferedClient,
    target: i32,
    window: Duration,
    context: &str,
) -> HashSet<(i16, i16)> {
    let mut gear = HashSet::new();
    let settled = Instant::now() + window;
    while Instant::now() < settled {
        let Some((packet_id, body)) = pump(client) else {
            continue;
        };
        match packet_id {
            0x00 if body.len() == 4 => client.write_all(&packet(0, &body)).unwrap(),
            0x04 => {
                if let Some((entity, slot, item)) = equipment_event(&body) {
                    if entity == target && item >= 0 {
                        gear.insert((slot, item));
                    }
                }
            }
            0x40 => panic!("{context}: disconnected: {body:?}"),
            _ => {}
        }
    }
    gear
}

#[test]
fn next032_late_observers_replay_true_armor_and_held_item() {
    let mut server = Server::start();
    let mut owner = server.join("Trampler");

    // Equip: helmet (window slot 5), chestplate (6), boots (8), plus two
    // hotbar weapons; the held item is slot 36 while selection is 0.
    give(&mut owner, 5, 298, 1);
    give(&mut owner, 6, 299, 1);
    give(&mut owner, 8, 301, 1);
    give(&mut owner, 36, 267, 1);
    give(&mut owner, 37, 268, 1);
    // Wait past at least one 10-tick refresh so the Hub cache is primed.
    thread::sleep(Duration::from_millis(800));

    // An online observer receives the live held-item change immediately.
    let mut watcher = server.join("Watcher");
    let owner_id = named_spawn(&mut watcher, "Trampler", "watcher sees owner");
    let mut gear = collect_gear(
        &mut watcher,
        owner_id,
        Duration::from_millis(1500),
        "watcher join gear",
    );
    for expected in [(0i16, 267i16), (1, 301), (3, 299), (4, 298)] {
        assert!(
            gear.contains(&expected),
            "watcher join replay missing slot/item {expected:?}: {gear:?}"
        );
    }
    owner.write_all(&packet(0x09, &1i16.to_be_bytes())).unwrap();
    wait_for(
        &mut watcher,
        0x04,
        |body| equipment_event(body) == Some((owner_id, 0, 268)),
        "live held swap reaches watcher",
    );

    // The core criterion: a player that joins NOW (after every change)
    // receives the true gear right after the owner's Spawn.
    let mut latecomer = server.join("Latecomer");
    let late_owner = named_spawn(&mut latecomer, "Trampler", "latecomer sees owner");
    gear = collect_gear(
        &mut latecomer,
        late_owner,
        Duration::from_millis(1500),
        "latecomer join gear",
    );
    for expected in [(0i16, 268i16), (1, 301), (3, 299), (4, 298)] {
        assert!(
            gear.contains(&expected),
            "latecomer join replay missing slot/item {expected:?}: {gear:?}"
        );
    }

    // Re-entry through the view boundary replays the full gear again for
    // BOTH observers, each time after a fresh Spawn of the owner.
    teleport(&mut owner, &mut server, [400.5, 64.0, 0.5]);
    for observer in [&mut watcher, &mut latecomer] {
        wait_for(
            observer,
            0x13,
            |body| body.len() == 5 && body[0] == 1 && body[1..5] == owner_id.to_be_bytes(),
            "owner left view",
        );
    }
    teleport(&mut owner, &mut server, [1.5, 64.0, 0.5]);
    for observer in [&mut watcher, &mut latecomer] {
        let respawned = named_spawn(observer, "Trampler", "owner re-entered view");
        assert_eq!(respawned, owner_id, "re-entry must reuse the entity ID");
        let replayed = collect_gear(
            observer,
            owner_id,
            Duration::from_millis(1500),
            "re-entry gear replay",
        );
        for expected in [(0i16, 268i16), (1, 301), (3, 299), (4, 298)] {
            assert!(
                replayed.contains(&expected),
                "re-entry replay missing slot/item {expected:?}: {replayed:?}"
            );
        }
    }

    drop(watcher);
    drop(latecomer);
    server.stop();
}
