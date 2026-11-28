//! NEXT-036 / #1019: an Arrow in flight keeps its owner attribution after
//! the shooting player logs out. The Arrow stores a fire-time snapshot
//! (`shooter` EntityID + optional Mob `shooter_kind`); the player's EntityID
//! never lives in the Mob registry and the disconnect path never touches
//! `mobs.arrows`, so a mid-flight logout must not cancel, disarm or
//! de-attribute the projectile. This test pins the observable wire
//! contract from a second client's stream:
//!   1. the arrow's Spawn Object (0x0E, type 60) advertises the shooter's
//!      live EntityID and full-charge velocity;
//!   2. the shooter's Destroy (0x13) lands while the arrow is still in
//!      flight (a 27.5-block gap gives ~10 flight ticks versus a ~5-tick
//!      disconnect window);
//!   3. AFTER that Destroy the Cow's health metadata (0x1C, DataWatcher
//!      index 6) drops from 10.0 to exactly 7.0 - the snapshot damage was
//!      applied with the shooter gone from the Hub;
//!   4. the impacted arrow itself despawns with its own Destroy.
//!
//! Unmodified Minecraft 1.7.10 client acceptance remains untested here;
//! disposable worlds and mobs only.

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

const COW: u8 = 92;
const ARROW: u8 = 60;

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
            "industrialrust-next036-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join("whitelist.json"),
            r#"[{"name":"Observer"},{"name":"Archer"}]"#,
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

    /// Handshake to the 33-byte S08 spawn marker; also captures the player's
    /// own EntityID from the S01 Join Game body.
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
                0x01 if body.len() >= 4 => return self.finish_join(socket, &body),
                0x40 => return Err(String::from_utf8_lossy(&body).into_owned()),
                _ => {}
            }
        }
    }

    fn finish_join(
        &self,
        mut socket: BufferedClient,
        s01: &[u8],
    ) -> Result<(BufferedClient, i32), String> {
        let entity = i32::from_be_bytes(s01[..4].try_into().unwrap());
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            assert!(Instant::now() < deadline, "join timed out waiting for S08");
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

/// Mob Spawn (0x0F): entity VarInt followed by the type byte.
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

/// Spawn Object (0x0E) carrying an Arrow with a known shooter: the body is
/// [entity VarInt][type 60][x i32][y i32][z i32][pitch u8][yaw u8]
/// [shooter i32][vx i16][vy i16][vz i16]; speed 3.0 encodes as 24000.
fn arrow_spawn(body: &[u8]) -> Option<(i32, i32, i16)> {
    let id = entity_varint(body)?;
    let head = varint_len(body);
    if body.len() < head + 21 || body[head] != ARROW {
        return None;
    }
    let shooter = i32::from_be_bytes(body[head + 15..head + 19].try_into().unwrap());
    let vx = i16::from_be_bytes(body[head + 19..head + 21].try_into().unwrap());
    Some((id, shooter, vx))
}

fn varint_len(body: &[u8]) -> usize {
    body.iter()
        .take(5)
        .position(|byte| byte & 0x80 == 0)
        .map_or(5, |index| index + 1)
}

/// Entity Metadata (0x1C) health float: [id i32][0x66][health f32][0x7F].
fn health_update(body: &[u8]) -> Option<(i32, f32)> {
    (body.len() == 10 && body[4] == ((3 << 5) | 6) && body[9] == 0x7f).then(|| {
        (
            i32::from_be_bytes(body[..4].try_into().unwrap()),
            f32::from_be_bytes(body[5..9].try_into().unwrap()),
        )
    })
}

/// Destroy Entities (0x13) with a single entity.
fn destroy_id(body: &[u8]) -> Option<i32> {
    (body.len() == 5 && body[0] == 1).then(|| i32::from_be_bytes(body[1..5].try_into().unwrap()))
}

/// C08 Player Block Placement on air (-1, 255, -1) face -1 is the 1.7.10
/// "use item" packet: it carries the claimed held stack and starts the bow
/// draw for a held bow 261 in Survival.
fn use_held_bow(client: &mut BufferedClient) {
    let mut body = Vec::new();
    body.extend_from_slice(&(-1i32).to_be_bytes());
    body.push(255);
    body.extend_from_slice(&(-1i32).to_be_bytes());
    body.push((-1i8) as u8);
    body.extend_from_slice(&261i16.to_be_bytes()); // claimed item must equal held
    body.push(1); // count
    body.extend_from_slice(&0i16.to_be_bytes()); // damage
    body.push(0); // no NBT: single TAG_End byte // no NBT
    body.extend_from_slice(&[0, 0, 0]); // cursor x/y/z
    client.write_all(&packet(8, &body)).unwrap();
}

/// C07 status 5 releases the drawn bow (charge comes from the hold time).
fn release_bow(client: &mut BufferedClient) {
    let mut body = vec![5u8];
    body.extend_from_slice(&10i32.to_be_bytes());
    body.push(64);
    body.extend_from_slice(&0i32.to_be_bytes());
    body.push(255);
    client.write_all(&packet(7, &body)).unwrap();
}

#[test]
fn next036_arrow_damages_and_keeps_owner_after_shooter_logout() {
    let mut server = Server::start();
    let (mut observer, _) = server.join("Observer");

    // A Cow 27.5 blocks down the +x axis: full-charge flight is ~10 ticks
    // (~500 ms), comfortably longer than any disconnect teardown, so the
    // hit provably lands after the shooter has left the Hub.
    server.console("/summon Cow 32.5 64 0.5");
    let spawn = wait_for(
        &mut observer,
        0x0f,
        |body| mob_spawn(body).is_some_and(|(_, kind)| kind == COW),
        "cow spawn",
    );
    let cow = mob_spawn(&spawn).unwrap().0;

    let (mut archer, archer_id) = server.join("Archer");
    // The observer must see the archer's live Spawn Player before the shot.
    let pspawn = wait_for(
        &mut observer,
        0x0c,
        |body| entity_varint(body) == Some(archer_id),
        "observer sees archer spawn",
    );
    assert_eq!(entity_varint(&pspawn), Some(archer_id));

    // Bow and arrows; drawing is a Survival interaction.
    server.console("/gamemode survival Archer");
    wait_for(
        &mut archer,
        0x39,
        |body| body.first() == Some(&0),
        "gamemode switch",
    );
    server.console("/give Archer 261 1");
    wait_for(
        &mut archer,
        0x30,
        |body| {
            // 261 encodes as i16 [0x01,0x05]: entry [item][count][dmg][nbt -1].
            body.windows(6).any(|w| w == [1, 5, 1, 0, 0, 0])
        },
        "bow delivered",
    );
    server.console("/give Archer 262 5");
    wait_for(
        &mut archer,
        0x30,
        |body| {
            body.windows(6).any(|w| w == [1, 6, 5, 0, 0, 0]) // 262 x5
        },
        "arrows delivered",
    );

    teleport(&mut archer, &mut server, "Archer", [4.5, 64.0, 0.5]);
    // Face +x (yaw -90) with a hair of upward pitch so the whole ~27-block
    // sweep stays inside the Cow's 1.4-high hitbox.
    archer
        .write_all(&position([4.5, 64.0, 0.5], -90.0, -2.0))
        .unwrap();
    use_held_bow(&mut archer);

    // Hold past full charge (>= 1.0 s) while pumping keep-alives.
    let held = Instant::now() + Duration::from_millis(1250);
    while Instant::now() < held {
        let Some((packet_id, body)) = pump(&mut archer) else {
            continue;
        };
        if packet_id == 0x00 && body.len() == 4 {
            archer.write_all(&packet(0, &body)).unwrap();
        }
    }
    release_bow(&mut archer);
    // The release handler echoes the debited window BEFORE broadcasting the
    // arrow Spawn, so a closed socket would abort it. Pump ~150 ms (3 ticks)
    // to let the release land, THEN log out: the hit only arrives at tick
    // ~10 (~500 ms), far beyond any disconnect teardown.
    let settle = Instant::now() + Duration::from_millis(150);
    while Instant::now() < settle {
        let Some((packet_id, body)) = pump(&mut archer) else {
            continue;
        };
        if packet_id == 0x00 && body.len() == 4 {
            archer.write_all(&packet(0, &body)).unwrap();
        }
    }
    drop(archer);

    // 1. The arrow spawns with the archer's still-live EntityID as shooter.
    let shot = wait_for(
        &mut observer,
        0x0e,
        |body| arrow_spawn(body).is_some(),
        "arrow spawn",
    );
    let (arrow_id, shooter, vx) = arrow_spawn(&shot).unwrap();
    assert_eq!(
        shooter, archer_id,
        "the Spawn Object must advertise the shooting player"
    );
    // Full charge (speed 3.0) with pitch -2 degrees: vx truncates to 23985.
    assert_eq!(
        vx,
        (3.0_f64 * 2.0_f64.to_radians().cos() * 8000.0) as i16,
        "full-charge horizontal speed on the wire"
    );

    // 2. The shooter's Destroy precedes the impact.
    wait_for(
        &mut observer,
        0x13,
        |body| destroy_id(body) == Some(archer_id),
        "archer destroy before impact",
    );

    // 3. After the shooter is gone the Cow takes the snapshot damage: the
    //    health metadata drops 10.0 -> 7.0 with nothing left to look up.
    let hurt = wait_for(
        &mut observer,
        0x1c,
        |body| health_update(body) == Some((cow, 7.0)),
        "cow health 7.0 after shooter logout",
    );
    assert_eq!(health_update(&hurt).unwrap(), (cow, 7.0));

    // 4. The impacted arrow despawns for every observer.
    wait_for(
        &mut observer,
        0x13,
        |body| destroy_id(body) == Some(arrow_id),
        "arrow destroyed on impact",
    );

    server.stop();
}
