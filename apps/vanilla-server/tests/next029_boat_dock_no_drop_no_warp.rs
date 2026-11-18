//! NEXT-029 / #1012: a boat ramming the shore — at low or full speed —
//! must never drop items, never break, and never teleport the hull or the
//! rider. Uses the native Rust server in Survival: a real water channel
//! walled by a stone block, a real boat placed with item 333, and real
//! Steer Vehicle packets. The session then proves the wire contract — the
//! hull's teleports stay one bounded hop apart and inside the water cells,
//! the rider is never corrected, detached, damaged or rolled back while
//! the boat parks, and breaking the parked boat drops exactly one item,
//! once. Unmodified Minecraft 1.7.10 client acceptance remains untested
//! here; disposable data only.

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
            "industrialrust-next029-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("whitelist.json"), r#"[{"name":"DockRam"}]"#).unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let log = root.join("server.log");
        let output = File::create(&log).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .current_dir(&root)
            .args(["--bind", &format!("127.0.0.1:{port}")])
            .arg("--survival")
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

    fn join(&self, name: &str) -> (BufferedClient, i32, [f64; 3]) {
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
        let mut entity = None;
        loop {
            assert!(Instant::now() < deadline, "{name}: join timed out");
            let Some((id, body)) = pump(&mut socket) else {
                continue;
            };
            match id {
                0x00 if body.len() == 4 => socket.write_all(&packet(0, &body)).unwrap(),
                0x01 if body.len() >= 6 => {
                    entity = Some(i32::from_be_bytes(body[..4].try_into().unwrap()));
                    assert_eq!(body[4] & 7, 0, "test session must be Survival");
                }
                0x08 if body.len() == 33 => {
                    let position = [
                        f64::from_be_bytes(body[..8].try_into().unwrap()),
                        f64::from_be_bytes(body[8..16].try_into().unwrap()) - f64::from(1.62_f32),
                        f64::from_be_bytes(body[16..24].try_into().unwrap()),
                    ];
                    return (
                        socket,
                        entity.expect("Join Game before initial S08"),
                        position,
                    );
                }
                0x40 => panic!("{name}: disconnected during join: {body:?}"),
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

/// C06 Position and Look with onGround=true.
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

/// C05 Look with onGround=true.
fn look(yaw: f32) -> Vec<u8> {
    let mut body = yaw.to_be_bytes().to_vec();
    body.extend_from_slice(&0.0f32.to_be_bytes());
    body.push(1);
    packet(5, &body)
}

/// C0C Steer Vehicle: sideways, forward, jump, dismount.
fn steer(sideways: f32, forward: f32, dismount: bool) -> Vec<u8> {
    let mut body = sideways.to_be_bytes().to_vec();
    body.extend_from_slice(&forward.to_be_bytes());
    body.push(0);
    body.push(u8::from(dismount));
    packet(12, &body)
}

/// C02 Use Entity: target Int, action Byte (0 = mount, 1 = attack/break).
fn use_entity(target: i32, action: u8) -> Vec<u8> {
    let mut body = target.to_be_bytes().to_vec();
    body.push(action);
    packet(2, &body)
}

fn feet(s08_body: &[u8]) -> [f64; 3] {
    [
        f64::from_be_bytes(s08_body[..8].try_into().unwrap()),
        f64::from_be_bytes(s08_body[8..16].try_into().unwrap()) - f64::from(1.62_f32),
        f64::from_be_bytes(s08_body[16..24].try_into().unwrap()),
    ]
}

fn wait_for(
    client: &mut BufferedClient,
    id: u32,
    predicate: impl Fn(&[u8]) -> bool,
    context: &str,
) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(10);
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

/// S1B Attach Entity for this session: rider Int, vehicle Int (-1 = leave).
fn wait_attach(client: &mut BufferedClient, entity: i32, vehicle: i32, context: &str) {
    wait_for(
        client,
        0x1b,
        |body| {
            body.len() == 9
                && body[..4] == entity.to_be_bytes()
                && body[4..8] == vehicle.to_be_bytes()
        },
        context,
    );
}

/// A chat echo follows everything processed earlier on the same stream.
/// `forbidden` describes what must not appear before the echo.
fn barrier(
    client: &mut BufferedClient,
    marker: &str,
    reject: impl Fn(u32, &[u8]) -> Option<&'static str>,
) {
    let mut body = Vec::new();
    write_string(marker, &mut body).unwrap();
    client.write_all(&packet(1, &body)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            Instant::now() < deadline,
            "chat barrier {marker:?} timed out"
        );
        let Some((id, body)) = pump(client) else {
            continue;
        };
        if id == 0x00 && body.len() == 4 {
            client.write_all(&packet(0, &body)).unwrap();
        }
        if let Some(reason) = reject(id, &body) {
            panic!("{reason} during {marker:?}: {body:?}");
        }
        if id == 0x40 {
            panic!("disconnected during {marker:?}");
        }
        if id == 0x02 && body.windows(marker.len()).any(|w| w == marker.as_bytes()) {
            return;
        }
    }
}

/// Creative-give item and wait for the 0x2f echo.
fn give(client: &mut BufferedClient, id: i16) {
    let mut give_packet = 36i16.to_be_bytes().to_vec();
    give_packet.extend_from_slice(&id.to_be_bytes());
    give_packet.push(1);
    give_packet.extend_from_slice(&0i16.to_be_bytes());
    give_packet.extend_from_slice(&(-1i16).to_be_bytes());
    client.write_all(&packet(0x10, &give_packet)).unwrap();
    wait_for(
        client,
        0x2f,
        |body| body.len() == 10 && body[3..5] == id.to_be_bytes(),
        "slot echo",
    );
}

fn place_block(client: &mut BufferedClient, x: i32, item: i16, y: u8) {
    let mut place = x.to_be_bytes().to_vec();
    place.push(y);
    place.extend_from_slice(&0i32.to_be_bytes());
    place.push(1); // top face
    place.extend_from_slice(&item.to_be_bytes());
    place.push(1);
    place.extend_from_slice(&0i16.to_be_bytes());
    place.extend_from_slice(&(-1i16).to_be_bytes());
    place.extend_from_slice(&[8, 8, 8]);
    client.write_all(&packet(8, &place)).unwrap();
}

/// Place a water source at (x, 64, 0) and wait for its block update.
fn place_water(client: &mut BufferedClient, x: i32) {
    give(client, 8);
    place_block(client, x, 8, 63);
    wait_for(
        client,
        0x23,
        |body| body.len() == 11 && body[0..4] == x.to_be_bytes() && body[4] == 64 && body[9] == 8,
        "water block update",
    );
}

/// Protocol 5 fixed-point coordinate (/32) as carried by S18 teleports.
fn fixed(body: &[u8]) -> f64 {
    f64::from(i32::from_be_bytes(body[..4].try_into().unwrap())) / 32.0
}

/// Spawn Object type byte after the VarInt entity id.
fn object_type(body: &[u8]) -> Option<u8> {
    let mut remaining: &[u8] = body;
    legacy_protocol::read_varint(&mut remaining)
        .ok()
        .and_then(|_| remaining.first().copied())
}

#[test]
fn next029_full_speed_dock_drops_nothing_and_breaks_nothing() {
    let mut server = Server::start();
    let (mut mover, entity, origin) = server.join("DockRam");
    assert_eq!(origin, [0.5, 64.0, 0.5]);

    // Build a five-cell water channel (x = 2..=6 at y 64) capped by one
    // stone wall block at x = 7, then a boat on the x = 3 water cell.
    server.console("/gamemode creative DockRam");
    wait_for(
        &mut mover,
        0x39,
        |body| body.first() == Some(&0x0d),
        "creative abilities",
    );
    for x in 2..=6 {
        place_water(&mut mover, x);
    }
    give(&mut mover, 1);
    place_block(&mut mover, 7, 1, 63);
    wait_for(
        &mut mover,
        0x23,
        |body| body.len() == 11 && body[0..4] == 7i32.to_be_bytes() && body[9] == 1,
        "wall block update",
    );
    give(&mut mover, 333);
    place_block(&mut mover, 3, 333, 64);
    let spawned = wait_for(
        &mut mover,
        0x0e,
        |body| object_type(body) == Some(1),
        "boat spawn",
    );
    let mut spawn_body = spawned.as_slice();
    let boat_id = i32::try_from(legacy_protocol::read_varint(&mut spawn_body).unwrap()).unwrap();
    server.console("/gamemode survival DockRam");
    wait_for(
        &mut mover,
        0x39,
        |body| body.first() == Some(&0),
        "survival abilities",
    );

    // Board from the shore side, aim the bow at the wall (+x = yaw 270).
    mover.write_all(&position([2.5, 64.0, 0.5])).unwrap();
    barrier(&mut mover, "next029 board approach", |id, _| {
        if id == 0x08 {
            Some("walk rollback")
        } else {
            None
        }
    });
    mover.write_all(&use_entity(boat_id, 0)).unwrap();
    wait_attach(&mut mover, entity, boat_id, "boat mount");
    mover.write_all(&look(270.0)).unwrap();

    // Full throttle into the shore for 2.5 s: the hull must stay over the
    // water cells, publish only one-bounded-hop teleports, and nothing may
    // drop, break, detach or correct the rider on the way.
    let mut hull = [3.5, 64.45, 0.5];
    let mut parked = 0usize;
    let started = Instant::now();
    let mut next_input = Instant::now();
    while started.elapsed() < Duration::from_millis(2500) {
        if Instant::now() >= next_input {
            mover.write_all(&steer(0.0, 1.0, false)).unwrap();
            mover.write_all(&look(270.0)).unwrap();
            next_input = Instant::now() + Duration::from_millis(90);
        }
        let Some((id, body)) = pump(&mut mover) else {
            continue;
        };
        match id {
            0x00 if body.len() == 4 => mover.write_all(&packet(0, &body)).unwrap(),
            0x18 if body.len() == 18 && body[..4] == boat_id.to_be_bytes() => {
                let next = [
                    fixed(&body[4..8]),
                    fixed(&body[8..12]),
                    fixed(&body[12..16]),
                ];
                assert!(next[0] < 7.0, "hull rammed into the wall cell: {next:?}");
                let hop = (next[0] - hull[0]).hypot(next[2] - hull[2]);
                assert!(
                    hop <= 0.36,
                    "hull teleported by {hop}: {:?} -> {next:?}",
                    hull
                );
                parked = if next[0] == hull[0] { parked + 1 } else { 0 };
                hull = next;
            }
            0x08 => panic!(
                "rider corrected while riding the docked boat: {:?}",
                feet(&body)
            ),
            0x1b => panic!("rider detached while docking: {body:?}"),
            0x13 => panic!("boat destroyed while docking: {body:?}"),
            0x06 if body.len() >= 4 && f32::from_be_bytes(body[..4].try_into().unwrap()) < 20.0 => {
                panic!("rider damaged while docking: {body:?}");
            }
            0x0e if object_type(&body) == Some(2) => {
                panic!("docking dropped an item: {body:?}");
            }
            0x40 => panic!("disconnected while docking"),
            _ => {}
        }
    }
    // It actually rammed the shore: parked at the wall side of the channel
    // with dozens of identical parked teleports and zero corrections.
    assert!(hull[0] > 6.0, "boat never reached the wall: {hull:?}");
    assert!(hull[0] < 7.0, "hull entered the wall cell: {hull:?}");
    assert!(parked >= 10, "boat never parked cleanly: {hull:?}");

    // Break the parked boat once: exactly one detach, one destroy and one
    // dropped boat item. The rider then reports its on-foot pose.
    mover.write_all(&use_entity(boat_id, 1)).unwrap();
    let mut detach = 0usize;
    let mut destroy = 0usize;
    let mut drops = 0usize;
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(700) {
        let Some((id, body)) = pump(&mut mover) else {
            continue;
        };
        match id {
            0x00 if body.len() == 4 => mover.write_all(&packet(0, &body)).unwrap(),
            0x1b if body.len() == 9
                && body[..4] == entity.to_be_bytes()
                && body[4..8] == (-1i32).to_be_bytes() =>
            {
                detach += 1;
                if detach == 1 {
                    mover.write_all(&position([hull[0], 64.0, 0.5])).unwrap();
                }
            }
            0x13 if body.len() == 5 && body[1..5] == boat_id.to_be_bytes() => destroy += 1,
            0x0e if object_type(&body) == Some(2) => drops += 1,
            0x40 => panic!("disconnected while breaking the boat"),
            _ => {}
        }
    }
    assert_eq!(detach, 1, "detach must fire exactly once");
    assert_eq!(destroy, 1, "destroy must fire exactly once");
    assert_eq!(drops, 1, "the boat must drop exactly one item");

    // A second attack on the gone boat is silent: no second detach, no
    // second destroy, no duplicate drop before the chat echo. (A 0x13 for
    // the dropped ITEM — pickup or expiry — is legitimate and not denied.)
    mover.write_all(&use_entity(boat_id, 1)).unwrap();
    barrier(&mut mover, "next029 second break silent", |id, body| {
        if id == 0x1b && body.len() == 9 && body[..4] == entity.to_be_bytes() {
            Some("duplicate detach")
        } else if id == 0x13 && body.len() == 5 && body[1..5] == boat_id.to_be_bytes() {
            Some("duplicate destroy")
        } else if id == 0x0e && object_type(body) == Some(2) {
            Some("duplicate boat drop")
        } else {
            None
        }
    });

    // The session is a healthy on-foot player at the docked spot.
    mover.write_all(&position([hull[0], 64.0, 0.5])).unwrap();
    barrier(&mut mover, "next029 on foot at the dock", |id, _| {
        if id == 0x08 {
            Some("on-foot rollback")
        } else {
            None
        }
    });
    mover.write_all(&position([5.5, 64.0, 0.5])).unwrap();
    barrier(&mut mover, "next029 walk out", |id, _| {
        if id == 0x08 {
            Some("walk rollback")
        } else {
            None
        }
    });

    drop(mover);
    server.stop();
}
