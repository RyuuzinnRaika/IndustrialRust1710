//! NEXT-035 / #1018: when resting ItemEntities
//! merge, every observer must converge: the absorbed entity's destroy
//! (0x13) and the survivor's re-announced count (0x1C item-slot metadata)
//! are produced as one atomic batch by `GroundItems::advance()` (sidecar
//! committed first) and fan out through the GLOBAL registry broadcast,
//! matching the global ground-item spawn/destroy tracking. This test pins
//! the observable wire contract:
//!   1. two live observers (one standing next to the merge, one far away)
//!      receive the identical sequence spawn(A) → count 1 → spawn(B) →
//!      count 11 → destroy(B) → count 12 on both streams;
//!   2. the survivor is never destroyed and its count never regresses;
//!   3. an observer joining AFTER the merge replays the survivor's Spawn
//!      with the merged count 12 and never sees the absorbed ID at all
//!      (no ghost Spawn, no retrospective destroy).
//!
//! Unmodified Minecraft 1.7.10 client acceptance remains untested here;
//! disposable data only.

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

const DIRT: i16 = 3;

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
            "industrialrust-next035-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join("whitelist.json"),
            r#"[{"name":"Sweeper"},{"name":"Watcher"},{"name":"Latecomer"}]"#,
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

/// Spawn Object (0x0E) for a dropped item: leading entity VarInt, type 2.
fn item_spawn_id(body: &[u8]) -> Option<i32> {
    let id = entity_varint(body)?;
    // Ground item ids are allocated above 1e9: five VarInt bytes.
    (body.len() > 5 && body[5] == 2).then_some(id)
}

/// Item-slot metadata (0x1C): [id i32][0xAA][item i16][count u8]
/// [damage i16][TAG_End 0x00][0x7F] for an NBT-less stack.
fn item_count_update(body: &[u8]) -> Option<(i32, i16, u8)> {
    (body.len() == 12 && body[4] == 0xaa && body[10] == 0 && body[11] == 0x7f).then(|| {
        (
            i32::from_be_bytes(body[..4].try_into().unwrap()),
            i16::from_be_bytes(body[5..7].try_into().unwrap()),
            body[7],
        )
    })
}

/// Destroy Entities (0x13) with a single entity.
fn destroy_id(body: &[u8]) -> Option<i32> {
    (body.len() == 5 && body[0] == 1).then(|| i32::from_be_bytes(body[1..5].try_into().unwrap()))
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
    client.write_all(&position(target)).unwrap();
}

/// C07 status 3 drops the whole held stack, status 4 exactly one item;
/// the packet carries no real mining target (the server drops the
/// server-owned held slot at its own player cell).
fn drop_held(client: &mut BufferedClient, whole: bool) {
    let mut body = vec![if whole { 3u8 } else { 4u8 }];
    body.extend_from_slice(&10i32.to_be_bytes());
    body.push(64);
    body.extend_from_slice(&0i32.to_be_bytes());
    body.push(255);
    client.write_all(&packet(7, &body)).unwrap();
}

#[test]
fn next035_merge_destroy_and_count_converge_on_every_observer() {
    let mut server = Server::start();
    let mut owner = server.join("Sweeper");
    let mut watcher = server.join("Watcher");

    // Dropping by C07 is a Survival interaction; switch first.
    server.console("/gamemode survival Sweeper");
    wait_for(
        &mut owner,
        0x39,
        |body| body.first() == Some(&0),
        "gamemode switch",
    );
    // Drop at a cell 10 blocks from world spawn: the watcher and the
    // latecomer stay at spawn and can never re-collect the survivor.
    teleport(&mut owner, &mut server, "Sweeper", [10.5, 64.0, 0.5]);
    server.console("/give Sweeper 3 12");
    let mut got_stack = false;
    let deadline = Instant::now() + Duration::from_secs(20);
    while !got_stack {
        assert!(Instant::now() < deadline, "give never echoed 12 dirt");
        let Some((packet_id, body)) = pump(&mut owner) else {
            continue;
        };
        match packet_id {
            0x00 if body.len() == 4 => owner.write_all(&packet(0, &body)).unwrap(),
            // 0x30 dumps slots without indices: an occupied stack is
            // [item i16][count u8][damage i16][nbt -1 i16].
            0x30 if body.windows(6).any(|w| w == [0, DIRT as u8, 12, 0, 0, 0]) => got_stack = true,
            0x2f if body.len() == 9 && body[3..5] == DIRT.to_be_bytes() && body[5] == 12 => {
                got_stack = true
            }
            0x40 => panic!("owner disconnected: {body:?}"),
            _ => {}
        }
    }

    // Two identical identity stacks land in one supported cell: the first
    // (lower entity id) survives, the second is absorbed into it. The
    // observers watch the identical spawn sequence: A count 1, then B
    // count 11 (spawn 0x0E + slot metadata 0x1C per entity). The owner's
    // inventory echoes (0x2F) are irrelevant and simply skipped.
    let first = |client: &mut BufferedClient, who: &str| -> i32 {
        let spawn = wait_for(
            client,
            0x0e,
            |body| item_spawn_id(body).is_some(),
            &format!("{who}: first item spawn"),
        );
        let id = item_spawn_id(&spawn).unwrap();
        wait_for(
            client,
            0x1c,
            |body| item_count_update(body) == Some((id, DIRT, 1)),
            &format!("{who}: first item count 1"),
        );
        id
    };
    let second = |client: &mut BufferedClient, who: &str, a: i32| -> i32 {
        let spawn = wait_for(
            client,
            0x0e,
            |body| item_spawn_id(body).is_some_and(|id| id != a),
            &format!("{who}: second item spawn"),
        );
        let b = item_spawn_id(&spawn).unwrap();
        wait_for(
            client,
            0x1c,
            |body| item_count_update(body) == Some((b, DIRT, 11)),
            &format!("{who}: second item count 11"),
        );
        b
    };
    drop_held(&mut owner, false); // one dirt
    let owner_a = first(&mut owner, "owner");
    drop_held(&mut owner, true); // remaining 11 as a second entity
    let owner_b = second(&mut owner, "owner", owner_a);
    let watcher_a = first(&mut watcher, "watcher");
    let watcher_b = second(&mut watcher, "watcher", watcher_a);
    assert_eq!(
        (owner_a, owner_b),
        (watcher_a, watcher_b),
        "observers must track the same entity ids"
    );

    // The merge itself on the owner's stream: destroy of the absorbed id
    // and the survivor's merged count, in that (produced) order.
    wait_for(
        &mut owner,
        0x13,
        |body| destroy_id(body) == Some(owner_b),
        "owner: absorbed id destroyed",
    );
    wait_for(
        &mut owner,
        0x1c,
        |body| item_count_update(body) == Some((owner_a, DIRT, 12)),
        "owner: survivor count 12",
    );
    // And on the far observer's stream: identical bytes, same order.
    wait_for(
        &mut watcher,
        0x13,
        |body| destroy_id(body) == Some(watcher_b),
        "watcher: absorbed id destroyed",
    );
    wait_for(
        &mut watcher,
        0x1c,
        |body| item_count_update(body) == Some((watcher_a, DIRT, 12)),
        "watcher: survivor count 12",
    );

    // Remove the thrower from the merge cell: with every player out of
    // pickup range the survivor must persist untouched.
    teleport(&mut owner, &mut server, "Sweeper", [300.5, 64.0, 0.5]);

    // An observer that joins AFTER the merge converges through the join
    // snapshot: the survivor's Spawn with count 12, and the absorbed id
    // never appears (no ghost Spawn, no destroy needed or sent).
    let mut latecomer = server.join("Latecomer");
    let mut survivor_seen = false;
    let mut count_seen = None;
    let settled = Instant::now() + Duration::from_millis(2600);
    while Instant::now() < settled {
        let Some((packet_id, body)) = pump(&mut latecomer) else {
            continue;
        };
        match packet_id {
            0x00 if body.len() == 4 => latecomer.write_all(&packet(0, &body)).unwrap(),
            0x0e => {
                let id = item_spawn_id(&body);
                assert_ne!(
                    id,
                    Some(owner_b),
                    "absorbed id ghosted into the join snapshot"
                );
                survivor_seen |= id == Some(owner_a);
            }
            0x1c => {
                if let Some((id, item, count)) = item_count_update(&body) {
                    if id == owner_a {
                        assert_eq!(item, DIRT);
                        assert_eq!(count, 12, "late join must replay the merged count");
                        count_seen = Some(count);
                    }
                    assert_ne!(id, owner_b, "absorbed id re-announced after its destroy");
                }
            }
            0x13 => {
                assert_ne!(
                    destroy_id(&body),
                    Some(owner_a),
                    "survivor destroyed while nobody was near"
                );
            }
            0x40 => panic!("latecomer disconnected: {body:?}"),
            _ => {}
        }
        if survivor_seen && count_seen.is_some() {
            break;
        }
    }
    assert!(survivor_seen, "late join never saw the survivor");
    assert_eq!(count_seen, Some(12), "late join lacked count 12");

    drop(latecomer);
    server.stop();
}
