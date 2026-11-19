//! NEXT-034 / #1017: a Mob riding relation must survive view (chunk)
//! boundary crossings. The Hub now tracks each peer's
//! mount EntityID; every Spawn delivery replays the Attach notification
//! (0x1B) right after the rider's Spawn: the join snapshot (tether
//! snapshots) and both directions of the Hub::move_player view-entry
//! transition. Live mount/dismount transitions publish through
//! Hub::set_mount immediately, and a 10-tick re-derivation across the
//! Mob/boat/minecart registries self-heals remote changes (broken boat,
//! killed mount). This test pins the observable wire contract:
//!   1. an observer that joins AFTER the ride began receives the Attach
//!      after both parent Spawns (rider 0x0C and mount 0x0F) in its join
//!      stream;
//!   2. an observer that leaves view and returns receives the Attach again
//!      right after the rider's fresh Spawn;
//!   3. after a real dismount the stream sends the -1 detach, and later
//!      re-entries replay no ghost Attach.
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

const PIG: u8 = 90;

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
            "industrialrust-next034-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join("whitelist.json"),
            r#"[{"name":"Trampler"},{"name":"Watcher"}]"#,
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

fn mob_spawn(body: &[u8]) -> Option<(i32, u8)> {
    let mut rest = body;
    let mut id: i32 = 0;
    let mut shift = 0;
    loop {
        let byte = *rest.first()?;
        rest = &rest[1..];
        id |= ((byte & 0x7f) as i32) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift > 28 {
            return None;
        }
    }
    Some((id, *rest.first()?))
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

/// Attach Entity (0x1B): i32 entity, i32 vehicle (-1 = detach), u8 leash.
fn attach_event(body: &[u8]) -> Option<(i32, i32)> {
    (body.len() == 9 && body[8] == 0).then(|| {
        (
            i32::from_be_bytes(body[..4].try_into().unwrap()),
            i32::from_be_bytes(body[4..8].try_into().unwrap()),
        )
    })
}

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

fn named_spawn(client: &mut BufferedClient, name: &str, context: &str) -> i32 {
    let body = wait_for(
        client,
        0x0c,
        |body| body.windows(name.len()).any(|w| w == name.as_bytes()),
        context,
    );
    entity_varint(&body).expect("spawn entity varint")
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

fn interact(client: &mut BufferedClient, target: i32) {
    let mut body = target.to_be_bytes().to_vec();
    body.push(0); // C02 Interact
    client.write_all(&packet(0x02, &body)).unwrap();
}

#[test]
fn next034_mount_attach_replays_across_view_boundaries() {
    let mut server = Server::start();
    let mut owner = server.join("Trampler");

    // Saddling and riding are Survival interactions (the Creative branch
    // skips item-debiting animal use), so switch the rider first. The
    // survival abilities mask is 0 (no fly), creative sends 0x0d, so the
    // predicate cannot match a login-stream 0x39.
    server.console("/gamemode survival Trampler");
    wait_for(
        &mut owner,
        0x39,
        |body| body.first() == Some(&0),
        "gamemode switch",
    );
    server.console("/give Trampler 329 1"); // saddle into the empty hotbar
    let mut got_saddle = false;
    let deadline = Instant::now() + Duration::from_secs(20);
    while !got_saddle {
        assert!(Instant::now() < deadline, "saddle give never echoed");
        let Some((packet_id, body)) = pump(&mut owner) else {
            continue;
        };
        match packet_id {
            0x00 if body.len() == 4 => owner.write_all(&packet(0, &body)).unwrap(),
            0x2f if body.len() >= 5 && body[3..5] == 329i16.to_be_bytes() => got_saddle = true,
            0x30 if body.windows(2).any(|w| w == 329i16.to_be_bytes()) => got_saddle = true,
            0x40 => panic!("owner disconnected: {body:?}"),
            _ => {}
        }
    }
    server.console("/summon Pig 2.5 64 0.5");
    let spawn = wait_for(
        &mut owner,
        0x0f,
        |body| matches!(mob_spawn(body), Some((_, PIG))),
        "pig spawn",
    );
    let pig = mob_spawn(&spawn).unwrap().0;

    // First Interact saddles (consuming the held saddle), second mounts.
    interact(&mut owner, pig);
    wait_for(
        &mut owner,
        0x2f,
        |body| body.len() == 5 && body[3..5] == (-1i16).to_be_bytes(),
        "saddle consumed from the held slot",
    );
    interact(&mut owner, pig);
    // The mount Attach is broadcast to every session (the owner included),
    // so the owner's own EntityID is read back from that packet: the first
    // Attach whose vehicle is the Pig.
    let mounted = wait_for(
        &mut owner,
        0x1b,
        |body| attach_event(body).is_some_and(|(_, vehicle)| vehicle == pig),
        "owner mount attach",
    );
    let rider = attach_event(&mounted).unwrap().0;

    // Criterion 1: an observer joining AFTER the ride began receives the
    // Attach after both parent Spawns in its join stream.
    let mut watcher = server.join("Watcher");
    let (rider_seen, pig_seen, attach_after) = scan_join_attach(&mut watcher, rider, pig);
    assert!(rider_seen, "join stream lost the rider Spawn");
    assert!(pig_seen, "join stream lost the mount Spawn");
    assert!(
        attach_after,
        "join stream carried no Attach after the parent Spawns"
    );

    // Criterion 2: leaving and returning across the view boundary re-sends
    // the Attach right after the rider's fresh Spawn (the riding relation
    // crosses chunk boundaries intact).
    teleport(&mut watcher, &mut server, "Watcher", [400.5, 64.0, 0.5]);
    wait_for(
        &mut watcher,
        0x13,
        |body| destroy_id(body) == Some(rider),
        "rider left view",
    );
    teleport(&mut watcher, &mut server, "Watcher", [1.5, 64.0, 0.5]);
    let respawned = named_spawn(&mut watcher, "Trampler", "rider re-entered view");
    assert_eq!(respawned, rider, "re-entry must reuse the entity ID");
    wait_for(
        &mut watcher,
        0x1b,
        |body| attach_event(body) == Some((rider, pig)),
        "re-entry Attach replay",
    );

    // Criterion 3: a real dismount detaches globally, and later re-entries
    // replay no ghost Attach.
    let mut steer = vec![0u8; 10];
    steer[9] = 1; // Steer Vehicle: dismount
    owner.write_all(&packet(0x0c, &steer)).unwrap();
    wait_for(
        &mut watcher,
        0x1b,
        |body| attach_event(body) == Some((rider, -1)),
        "dismount detach reaches the watcher",
    );
    teleport(&mut watcher, &mut server, "Watcher", [400.5, 64.0, 0.5]);
    wait_for(
        &mut watcher,
        0x13,
        |body| destroy_id(body) == Some(rider),
        "rider left view again",
    );
    teleport(&mut watcher, &mut server, "Watcher", [1.5, 64.0, 0.5]);
    named_spawn(&mut watcher, "Trampler", "rider re-entered after dismount");
    let settled = Instant::now() + Duration::from_millis(900);
    while Instant::now() < settled {
        let Some((packet_id, body)) = pump(&mut watcher) else {
            continue;
        };
        match packet_id {
            0x00 if body.len() == 4 => watcher.write_all(&packet(0, &body)).unwrap(),
            0x1b => {
                panic!("ghost Attach replayed for a finished ride: {body:?}");
            }
            0x40 => panic!("watcher disconnected: {body:?}"),
            _ => {}
        }
    }

    drop(watcher);
    server.stop();
}

/// Scan the joiner's post-0x08 replay window: the rider's 0x0C and the
/// mount's 0x0F must both appear, and the Attach must come after them.
fn scan_join_attach(client: &mut BufferedClient, rider: i32, pig: i32) -> (bool, bool, bool) {
    let mut rider_seen = false;
    let mut pig_seen = false;
    let mut attach_after = false;
    let settled = Instant::now() + Duration::from_millis(2200);
    while Instant::now() < settled {
        let Some((packet_id, body)) = pump(client) else {
            continue;
        };
        match packet_id {
            0x00 if body.len() == 4 => client.write_all(&packet(0, &body)).unwrap(),
            0x0c if body.windows(8).any(|w| w == b"Trampler") => {
                if entity_varint(&body) == Some(rider) {
                    rider_seen = true;
                }
            }
            0x0f if matches!(mob_spawn(&body), Some((id, PIG)) if id == pig) => {
                pig_seen = true;
            }
            0x1b if attach_event(&body) == Some((rider, pig)) && rider_seen && pig_seen => {
                attach_after = true;
                break;
            }
            0x40 => panic!("watcher disconnected during join scan: {body:?}"),
            _ => {}
        }
    }
    (rider_seen, pig_seen, attach_after)
}
