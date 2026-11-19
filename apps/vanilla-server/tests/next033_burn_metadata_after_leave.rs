//! NEXT-033 / #1016: an observer that leaves DURING a burning state-change
//! stream must never receive that departed session's stale metadata. A
//! sunlit Zombie emits status(2) + health Entity Metadata (0x1C) once per
//! second. Those updates fan out through per-session mailboxes owned by the
//! live session; when the socket dies the queued copies die with it, Hub::
//! remove drops the peer so later burns cannot even be queued for it, and
//! the replacement session for the same name gets a fresh channel (pinned
//! generally by next019). This test pins the observable burning-specific
//! contract on the wire:
//!   1. the burn stream is a strictly decreasing health sequence for every
//!      live observer;
//!   2. after the observer's abrupt leave the burn keeps advancing on the
//!      remaining session (the simulation is not frozen by the departure);
//!   3. the rejoining observer's join snapshot carries the CURRENT health,
//!      never one from the disconnected period, and its later stream shows
//!      no 0x1C above that snapshot (no stale metadata) while still
//!      advancing below it (live updates keep flowing).
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

const ZOMBIE: u8 = 54;

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
            "industrialrust-next033-{}-{stamp}",
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

/// The Mob spawn embeds DataWatcher index 6 (float HP) as its final entry:
/// [0x66][f32][0x7f].
fn spawn_health(body: &[u8]) -> f32 {
    assert_eq!(body.last(), Some(&0x7f), "spawn metadata terminator");
    assert_eq!(body[body.len() - 6], 0x66, "spawn health index");
    f32::from_be_bytes(body[body.len() - 5..body.len() - 1].try_into().unwrap())
}

/// Entity Metadata (0x1C) health update: [id i32][0x66][f32][0x7f].
fn health_update(body: &[u8], entity: i32) -> Option<f32> {
    (body.len() == 10 && body[..4] == entity.to_be_bytes() && body[4] == 0x66 && body[9] == 0x7f)
        .then(|| f32::from_be_bytes(body[5..9].try_into().unwrap()))
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

/// Wait for the next strictly-decreasing burn health for `entity`.
fn wait_burn(client: &mut BufferedClient, entity: i32, previous: f32, context: &str) -> f32 {
    let body = wait_for(
        client,
        0x1c,
        |body| health_update(body, entity).is_some_and(|h| h < previous),
        context,
    );
    health_update(&body, entity).unwrap()
}

#[test]
fn next033_burn_metadata_never_reaches_a_departed_session() {
    let mut server = Server::start();
    let mut owner = server.join("Trampler");

    // A sunlit sky-exposed Zombie in the flat world burns 1 HP per second,
    // emitting status(2) + a health 0x1C on every burn tick.
    server.console("/summon Zombie 2.5 65 0.5");
    let spawn = wait_for(
        &mut owner,
        0x0f,
        |body| matches!(mob_spawn(body), Some((_, ZOMBIE))),
        "zombie spawn",
    );
    let zombie = mob_spawn(&spawn).unwrap().0;
    let mut health = spawn_health(&spawn);
    assert!(health > 15.0, "fresh Zombie starts at 20 HP: {health}");
    for i in 0..3 {
        health = wait_burn(&mut owner, zombie, health, &format!("owner burn {i}"));
    }

    // The observer joins mid-stream: its snapshot is at most the health the
    // already-connected session last saw (HP only decreases), and it keeps
    // decreasing on the observer's own stream.
    let mut watcher = server.join("Watcher");
    let seen = wait_for(
        &mut watcher,
        0x0f,
        |body| matches!(mob_spawn(body), Some((id, ZOMBIE)) if id == zombie),
        "watcher sees the burning Zombie",
    );
    let watched = spawn_health(&seen);
    assert!(
        watched <= health,
        "observer snapshot regressed: {watched} after {health}"
    );
    let watched = wait_burn(&mut watcher, zombie, watched, "watcher burn");

    // Abrupt leave with the state change still in flight. The remaining
    // session must keep advancing the burn: the departure freezes nothing
    // server-side, and nothing is delivered to the departed socket.
    drop(watcher);
    thread::sleep(Duration::from_millis(600));
    for i in 0..2 {
        health = wait_burn(
            &mut owner,
            zombie,
            health,
            &format!("burn continues while observer is gone {i}"),
        );
    }

    // Rejoin: the fresh stream's snapshot is the CURRENT health, never a
    // value from the disconnected period.
    let mut rejoiner = server.join("Watcher");
    let replay = wait_for(
        &mut rejoiner,
        0x0f,
        |body| matches!(mob_spawn(body), Some((id, ZOMBIE)) if id == zombie),
        "rejoin snapshot of the burning Zombie",
    );
    let rejoined = spawn_health(&replay);
    assert!(
        rejoined <= health && rejoined < watched,
        "rejoin replayed a stale snapshot: {rejoined} vs current {health} / last-seen {watched}"
    );

    // Afterwards the rejoiner sees only live updates: nothing ABOVE the
    // snapshot (that is stale metadata from the disconnected period) and at
    // least one BELOW it (the stream keeps advancing, not frozen).
    let settled = Instant::now() + Duration::from_millis(2600);
    let mut advanced = false;
    let mut last = rejoined;
    while Instant::now() < settled {
        let Some((packet_id, body)) = pump(&mut rejoiner) else {
            continue;
        };
        match packet_id {
            0x00 if body.len() == 4 => rejoiner.write_all(&packet(0, &body)).unwrap(),
            0x1c => {
                if let Some(next) = health_update(&body, zombie) {
                    assert!(
                        next <= last,
                        "stale burn metadata for the departed session: {next} > {last}"
                    );
                    if next < last {
                        last = next;
                        advanced = true;
                    }
                }
            }
            0x40 => panic!("rejoiner disconnected: {body:?}"),
            _ => {}
        }
    }
    assert!(advanced, "burn stream froze after the rejoin ({rejoined})");

    drop(rejoiner);
    server.stop();
}
