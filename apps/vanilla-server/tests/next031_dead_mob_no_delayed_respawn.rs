//! NEXT-031 / #1014: a dead Entity must never reappear through a delayed
//! Spawn. Both replay sites (session join and post-death respawn) snapshot
//! the shared Mob map under the same mutex that removes a killed Mob, and a
//! joining player is registered in the hub BEFORE its snapshot is written,
//! so every Destroy that follows the snapshot still reaches that socket.
//! This test pins the observable contract on the wire:
//!   1. a killed Mob's socket order is status(3) then exactly one Destroy,
//!      and no later Spawn names that entity ID;
//!   2. a second player joining after the kill never sees a Spawn for the
//!      dead ID (join snapshot suppression);
//!   3. a viewer online at kill time receives Spawn-before-Destroy, never a
//!      Spawn after the Destroy;
//!   4. a rejoin after the kill sees no delayed Spawn, and the sidecar file
//!      no longer names the dead ID (a restart cannot resurrect it);
//!   5. after the observer itself dies and respawns, the respawn snapshot
//!      still excludes every dead ID while a living Mob is replayed (the
//!      snapshot is functional, not vacuously empty).
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

const CHICKEN: u8 = 93;

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
            "industrialrust-next031-{}-{stamp}",
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

    /// Join tolerating the transient Duplicate-name rejection while a
    /// previous session for the same name is still being released.
    fn join(&self, name: &str) -> (BufferedClient, JoinScan) {
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.try_join(name) {
                Ok(joined) => return joined,
                Err(rejected) => {
                    assert!(attempt < 25, "{name}: join rejected {attempt}x: {rejected}");
                    thread::sleep(Duration::from_millis(300));
                }
            }
        }
    }

    fn try_join(&self, name: &str) -> Result<(BufferedClient, JoinScan), String> {
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
        let mut scan = JoinScan::default();
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            assert!(Instant::now() < deadline, "{name}: join timed out");
            let Some((id, body)) = pump(&mut socket) else {
                continue;
            };
            match id {
                0x00 if body.len() == 4 => socket.write_all(&packet(0, &body)).unwrap(),
                0x08 if body.len() == 33 => break,
                0x0f => {
                    if let Some((spawn_id, _)) = mob_spawn(&body) {
                        scan.spawns.insert(spawn_id);
                    }
                }
                0x0c if body.windows(8).any(|window| window == b"Trampler") => {
                    scan.saw_trampler = true;
                }
                0x40 => return Err(String::from_utf8_lossy(&body).into_owned()),
                _ => {}
            }
        }
        // Keep draining briefly: join replays all world-owned entities
        // immediately after the position update.
        let settled = Instant::now() + Duration::from_millis(1400);
        while Instant::now() < settled {
            let Some((id, body)) = pump(&mut socket) else {
                continue;
            };
            match id {
                0x00 if body.len() == 4 => socket.write_all(&packet(0, &body)).unwrap(),
                0x0f => {
                    if let Some((spawn_id, _)) = mob_spawn(&body) {
                        scan.spawns.insert(spawn_id);
                    }
                }
                0x0c if body.windows(8).any(|window| window == b"Trampler") => {
                    scan.saw_trampler = true;
                }
                0x40 => return Err(format!("disconnected during scan: {body:?}")),
                _ => {}
            }
        }
        Ok((socket, scan))
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

#[derive(Default)]
struct JoinScan {
    spawns: HashSet<i32>,
    saw_trampler: bool,
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

fn destroy_id(body: &[u8]) -> Option<i32> {
    // S13 Destroy Entity: signed count (always 1 here) then big-endian IDs.
    (body.len() == 5 && body[0] == 1).then(|| i32::from_be_bytes(body[1..5].try_into().unwrap()))
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

fn feet(s08_body: &[u8]) -> [f64; 2] {
    [
        f64::from_be_bytes(s08_body[..8].try_into().unwrap()),
        f64::from_be_bytes(s08_body[8..16].try_into().unwrap()) - f64::from(1.62_f32),
    ]
}

fn wait_for(
    client: &mut BufferedClient,
    id: u32,
    predicate: impl Fn(&[u8]) -> bool,
    context: &str,
) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(25);
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

fn give(client: &mut BufferedClient, id: i16, count: u8) {
    let mut give_packet = 36i16.to_be_bytes().to_vec();
    give_packet.extend_from_slice(&id.to_be_bytes());
    give_packet.push(count);
    give_packet.extend_from_slice(&0i16.to_be_bytes());
    give_packet.push(0); // no NBT: single TAG_End byte
    client.write_all(&packet(0x10, &give_packet)).unwrap();
    wait_for(
        client,
        0x2f,
        |body| body.len() == 9 && body[3..5] == id.to_be_bytes(),
        "slot echo",
    );
}

fn summon_chicken(server: &mut Server, client: &mut BufferedClient, z: f64) -> i32 {
    server.console(&format!("/summon Chicken 0.5 65 {z}"));
    let spawn = wait_for(
        client,
        0x0f,
        |body| matches!(mob_spawn(body), Some((_, CHICKEN))),
        "chicken spawn",
    );
    mob_spawn(&spawn).unwrap().0
}

/// Punch until the chicken dies; the killed Mob's stream order must be
/// status(3) then exactly one Destroy.
fn kill_mob(client: &mut BufferedClient, id: i32, context: &str) {
    let attack = {
        let mut body = id.to_be_bytes().to_vec();
        body.push(1); // left-click attack
        packet(0x02, &body)
    };
    let deadline = Instant::now() + Duration::from_secs(25);
    let mut saw_death_status = false;
    loop {
        assert!(Instant::now() < deadline, "{context}: kill timed out");
        client.write_all(&attack).unwrap();
        let until = Instant::now() + Duration::from_millis(700);
        while Instant::now() < until {
            let Some((packet_id, body)) = pump(client) else {
                continue;
            };
            match packet_id {
                0x00 if body.len() == 4 => client.write_all(&packet(0, &body)).unwrap(),
                0x1a if body.len() == 5 && body[..4] == id.to_be_bytes() && body[4] == 3 => {
                    saw_death_status = true;
                }
                0x13 if destroy_id(&body) == Some(id) => {
                    assert!(
                        saw_death_status,
                        "{context}: Destroy arrived before the death status"
                    );
                    return;
                }
                0x40 => panic!("{context}: disconnected: {body:?}"),
                _ => {}
            }
        }
    }
}

/// Every later packet for this window must not re-Spawn the dead IDs and
/// must not repeat their Destroy.
fn assert_no_delayed_spawn(
    client: &mut BufferedClient,
    dead: &[i32],
    window: Duration,
    context: &str,
) {
    let settled = Instant::now() + window;
    while Instant::now() < settled {
        let Some((packet_id, body)) = pump(client) else {
            continue;
        };
        match packet_id {
            0x00 if body.len() == 4 => client.write_all(&packet(0, &body)).unwrap(),
            0x0f => {
                if let Some((spawn_id, _)) = mob_spawn(&body) {
                    assert!(
                        !dead.contains(&spawn_id),
                        "{context}: delayed Spawn for dead entity {spawn_id}"
                    );
                }
            }
            0x13 => {
                if let Some(gone) = destroy_id(&body) {
                    assert!(
                        !dead.contains(&gone),
                        "{context}: repeated Destroy for dead entity {gone}"
                    );
                }
            }
            0x40 => panic!("{context}: disconnected: {body:?}"),
            _ => {}
        }
    }
}

/// The viewer of a kill must observe Spawn before Destroy (or only the
/// Destroy if it joined in between), and never a Spawn afterwards.
fn assert_viewer_spawn_then_destroy(client: &mut BufferedClient, id: i32, context: &str) {
    let mut saw_spawn = false;
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        assert!(Instant::now() < deadline, "{context}: destroy timed out");
        let Some((packet_id, body)) = pump(client) else {
            continue;
        };
        match packet_id {
            0x00 if body.len() == 4 => client.write_all(&packet(0, &body)).unwrap(),
            0x0f => {
                if matches!(mob_spawn(&body), Some((spawn_id, CHICKEN)) if spawn_id == id) {
                    assert!(!saw_spawn, "{context}: duplicate Spawn for {id}");
                    saw_spawn = true;
                }
            }
            0x13 if destroy_id(&body) == Some(id) => break,
            0x40 => panic!("{context}: disconnected: {body:?}"),
            _ => {}
        }
    }
    assert!(saw_spawn, "{context}: viewer never saw the Spawn");
    assert_no_delayed_spawn(client, &[id], Duration::from_millis(800), context);
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

#[test]
fn next031_dead_mob_never_reappears_through_delayed_spawns() {
    let mut server = Server::start();
    let (mut attacker, opener) = server.join("Trampler");
    assert!(opener.spawns.is_empty(), "flat world starts mobless");

    // Phase 1 — a killed Mob emits status(3) then exactly one Destroy, and
    // nothing later re-Spawns its ID on the killer's own stream.
    give(&mut attacker, 267, 1); // iron sword, held slot
    let first = summon_chicken(&mut server, &mut attacker, 2.5);
    kill_mob(&mut attacker, first, "killer stream");
    assert_no_delayed_spawn(
        &mut attacker,
        &[first],
        Duration::from_millis(900),
        "killer post-kill",
    );

    // Phase 2 — a player joining after the kill gets no Spawn for the dead
    // ID from the join snapshot, yet the snapshot itself is functional.
    let (mut watcher, second_join) = server.join("Watcher");
    assert!(
        !second_join.spawns.contains(&first),
        "join snapshot re-Spawned the dead entity {first}"
    );
    assert!(
        second_join.saw_trampler,
        "join snapshot lost the online player (vacuous snapshot)"
    );

    // Phase 3 — a viewer online at kill time sees Spawn before Destroy and
    // never a Spawn afterwards.
    let second = summon_chicken(&mut server, &mut attacker, -2.5);
    kill_mob(&mut attacker, second, "killer stream 2");
    assert_viewer_spawn_then_destroy(&mut watcher, second, "watcher of kill 2");

    // Phase 4 — a rejoin is just another snapshot: still no dead IDs, and
    // the sidecar no longer persists them (a restart cannot resurrect).
    drop(watcher);
    thread::sleep(Duration::from_millis(700));
    let (rejoiner, rejoin_scan) = server.join("Watcher");
    assert!(
        !rejoin_scan.spawns.contains(&first) && !rejoin_scan.spawns.contains(&second),
        "rejoin snapshot re-Spawned dead entities: {:?}",
        rejoin_scan.spawns
    );
    assert!(
        rejoin_scan.saw_trampler,
        "rejoin snapshot lost the online player (vacuous snapshot)"
    );
    // The sidecar autosaves every 100 ticks; wait for the durable snapshot
    // to prove it no longer persists the dead ids (a restart cannot
    // resurrect them).
    let sidecar_path = server.root.join("flat.mobs.json");
    let cleared = Instant::now() + Duration::from_secs(8);
    loop {
        let sidecar = fs::read_to_string(&sidecar_path).unwrap_or_default();
        if !sidecar.contains(&first.to_string()) && !sidecar.contains(&second.to_string()) {
            break;
        }
        assert!(
            Instant::now() < cleared,
            "mobs sidecar still persists a dead entity id"
        );
        thread::sleep(Duration::from_millis(200));
    }

    // Phase 5 — the observer's own death and respawn: the respawn snapshot
    // (the second delayed-replay site) still excludes every dead ID while
    // replaying a living Mob.
    server.console("/gamemode survival Trampler");
    wait_for(
        &mut attacker,
        0x39,
        |body| body.first() == Some(&0),
        "survival abilities",
    );
    let survivor = summon_chicken(&mut server, &mut attacker, 0.5);
    teleport(&mut attacker, &mut server, [0.5, 150.0, 0.5]);
    let mut descents = 0usize;
    let dead = wait_until_dead(&mut attacker, &mut descents);
    assert!(dead, "idle gravity never fell the player to death");
    assert!(descents >= 5, "idle gravity steps observed: {descents}");
    attacker.write_all(&packet(0x16, &[0])).unwrap(); // C16 status: respawn
    wait_for(&mut attacker, 0x07, |_| true, "respawn");
    let mut survivor_spawns = 0usize;
    let settled = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < settled {
        let Some((packet_id, body)) = pump(&mut attacker) else {
            continue;
        };
        match packet_id {
            0x00 if body.len() == 4 => attacker.write_all(&packet(0, &body)).unwrap(),
            0x0f => {
                if let Some((spawn_id, _)) = mob_spawn(&body) {
                    assert!(
                        spawn_id != first && spawn_id != second,
                        "respawn snapshot re-Spawned dead entity {spawn_id}"
                    );
                    if spawn_id == survivor {
                        survivor_spawns += 1;
                    }
                }
            }
            0x40 => panic!("disconnected after respawn: {body:?}"),
            _ => {}
        }
    }
    assert_eq!(
        survivor_spawns, 1,
        "respawn snapshot must replay the living Mob exactly once"
    );

    drop(rejoiner);
    server.stop();
}

/// Stay silent after the teleport: server-side idle gravity integrates the
/// fall and the hazard loop delivers fatal fall damage (Update Health 0).
fn wait_until_dead(client: &mut BufferedClient, descents: &mut usize) -> bool {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if Instant::now() >= deadline {
            return false;
        }
        let Some((packet_id, body)) = pump(client) else {
            continue;
        };
        match packet_id {
            0x00 if body.len() == 4 => client.write_all(&packet(0, &body)).unwrap(),
            0x08 if body.len() == 33 => *descents += 1,
            0x06 if body.len() == 10
                && f32::from_be_bytes(body[..4].try_into().unwrap()) == 0.0 =>
            {
                return true;
            }
            0x40 => panic!("disconnected during the fatal fall: {body:?}"),
            _ => {}
        }
    }
}
