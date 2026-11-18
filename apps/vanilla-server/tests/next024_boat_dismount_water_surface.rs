//! NEXT-024 / #1007: switching from a boat back to foot at the water
//! surface must leave the session on solid footing: the first on-foot
//! position after dismount is accepted without a rollback, no drowning
//! or damage follows the water-surface stand, and the mount registry
//! never carries a double mount (a mount while seated is refused, and a
//! fresh mount only works again after the clean detach). Uses the native
//! Rust server in Survival, a real boat placed on placed water, and
//! disposable data. Unmodified Minecraft 1.7.10 client acceptance
//! remains untested here.

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
            "industrialrust-next024-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("whitelist.json"), r#"[{"name":"BoatMover"}]"#).unwrap();
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

/// C0C Steer Vehicle: sideways, forward, jump, dismount.
fn steer(forward: f32, dismount: bool) -> Vec<u8> {
    let mut body = 0.0f32.to_be_bytes().to_vec();
    body.extend_from_slice(&forward.to_be_bytes());
    body.push(0);
    body.push(u8::from(dismount));
    packet(12, &body)
}

/// C02 Use Entity: target Int, action Byte (0 = interact / mount).
fn use_entity(target: i32) -> Vec<u8> {
    let mut body = target.to_be_bytes().to_vec();
    body.push(0);
    packet(2, &body)
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
/// No S08 rollback, no attach change and no health loss may appear
/// between here and the echo.
fn barrier(client: &mut BufferedClient, marker: &str) {
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
        match id {
            0x00 if body.len() == 4 => client.write_all(&packet(0, &body)).unwrap(),
            0x08 => panic!("spurious S08 rollback during {marker:?}: {body:?}"),
            0x1b => panic!("attach changed during {marker:?}: {body:?}"),
            0x06 if body.len() >= 4 && f32::from_be_bytes(body[..4].try_into().unwrap()) < 20.0 => {
                panic!("damage during {marker:?}: {body:?}");
            }
            0x40 => panic!("disconnected during {marker:?}: {body:?}"),
            0x02 if body.windows(marker.len()).any(|w| w == marker.as_bytes()) => return,
            _ => {}
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

#[test]
fn next024_water_surface_dismount_resumes_foot_without_damage_or_double_mount() {
    let mut server = Server::start();
    let (mut mover, entity, origin) = server.join("BoatMover");
    assert_eq!(origin, [0.5, 64.0, 0.5]);

    // Water strip with a boat on it, built in Creative, then back to Survival.
    server.console("/gamemode creative BoatMover");
    wait_for(
        &mut mover,
        0x39,
        |body| body.first() == Some(&0x0d),
        "creative abilities",
    );
    place_water(&mut mover, 6);
    place_water(&mut mover, 7);
    give(&mut mover, 333);
    place_block(&mut mover, 6, 333, 64);
    let spawned = wait_for(
        &mut mover,
        0x0e,
        |body| {
            let mut remaining: &[u8] = body;
            legacy_protocol::read_varint(&mut remaining).is_ok() && remaining.first() == Some(&1)
        },
        "boat spawn",
    );
    let mut spawn_body = spawned.as_slice();
    let boat_id = i32::try_from(legacy_protocol::read_varint(&mut spawn_body).unwrap()).unwrap();
    server.console("/gamemode survival BoatMover");
    wait_for(
        &mut mover,
        0x39,
        |body| body.first() == Some(&0),
        "survival abilities",
    );

    // Walk to boarding range (mount radius is 4 blocks).
    let mut burst = Vec::new();
    let mut x = origin[0];
    while x < 5.0 {
        x += 0.5;
        burst.extend_from_slice(&position([x, 64.0, 0.5]));
    }
    mover.write_all(&burst).unwrap();
    barrier(&mut mover, "next024 walk to boat");

    // Mount once; a second mount while seated must not re-attach.
    mover.write_all(&use_entity(boat_id)).unwrap();
    wait_attach(&mut mover, entity, boat_id, "initial mount");
    mover.write_all(&use_entity(boat_id)).unwrap();
    barrier(&mut mover, "next024 double mount refused");

    // Steer briefly, then Shift-dismount through the steer packet.
    let mut steering = Vec::new();
    for _ in 0..6 {
        steering.extend_from_slice(&steer(0.5, false));
    }
    steering.extend_from_slice(&steer(0.0, true));
    mover.write_all(&steering).unwrap();
    wait_attach(&mut mover, entity, -1, "dismount detach");

    // First on-foot position at the water surface right after the detach:
    // the session already tracked the boat, so this is a small step that
    // must be accepted with no rollback, no damage and no re-attach.
    mover.write_all(&position([6.5, 64.0, 0.5])).unwrap();
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(400) {
        if let Some((id, body)) = pump(&mut mover) {
            if id == 0x00 && body.len() == 4 {
                mover.write_all(&packet(0, &body)).unwrap();
            }
            assert_ne!(id, 0x08, "rollback right after water-surface dismount");
            assert_ne!(id, 0x1b, "re-attach after dismount: {body:?}");
            if id == 0x06 && body.len() >= 4 {
                assert!(
                    f32::from_be_bytes(body[..4].try_into().unwrap()) >= 20.0,
                    "drowning/damage right after dismount: {body:?}"
                );
            }
        }
    }

    // Walk out of the water onto land: movement is fully resumed.
    let mut walk = Vec::new();
    for x in [5.5, 4.5] {
        walk.extend_from_slice(&position([x, 64.0, 0.5]));
    }
    mover.write_all(&walk).unwrap();
    barrier(&mut mover, "next024 on foot after dismount");

    // The registry holds no phantom mount: boarding again succeeds from
    // land, and the fresh mount is exclusive exactly like the first one.
    mover.write_all(&use_entity(boat_id)).unwrap();
    wait_attach(&mut mover, entity, boat_id, "remount after dismount");
    mover.write_all(&steer(0.0, true)).unwrap();
    wait_attach(&mut mover, entity, -1, "final detach");
    mover.write_all(&position([3.5, 64.0, 0.5])).unwrap();
    barrier(&mut mover, "next024 final");

    drop(mover);
    server.stop();
}
