//! NEXT-026 / #1009: a placed fence must behave like unmodified 1.7.10
//! at jump height: its collision volume spans 1.5 blocks, so a player
//! walking into it is rolled back, a normal jump (apex ~1.249 blocks)
//! can neither enter nor clear it, the exact 1.5 top supports standing
//! with no gravity correction, and stepping off is an ordinary fall.
//! Uses the native Rust server in Survival, a real fence placed through
//! the Protocol 5 place packet (fences, Nether fences and walls are now
//! placeable ordinary blocks), and disposable data. Unmodified
//! Minecraft 1.7.10 client acceptance remains untested here.

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
            "industrialrust-next026-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("whitelist.json"), r#"[{"name":"FenceTop"}]"#).unwrap();
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

/// A chat echo follows everything processed earlier on the same stream:
/// no correction may appear between here and the echo.
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
    give_packet.push(0); // no NBT: single TAG_End byte
    client.write_all(&packet(0x10, &give_packet)).unwrap();
    wait_for(
        client,
        0x2f,
        |body| body.len() == 9 && body[3..5] == id.to_be_bytes(),
        "slot echo",
    );
}

/// Place a block by clicking the top face of (x, y, 0); the new block
/// lands at y + 1. Waits for the matching 0x23 block update.
fn place(client: &mut BufferedClient, x: i32, item: i16, y: u8, landed: u8, block: u8) {
    let mut place_packet = x.to_be_bytes().to_vec();
    place_packet.push(y);
    place_packet.extend_from_slice(&0i32.to_be_bytes());
    place_packet.push(1); // top face
    place_packet.extend_from_slice(&item.to_be_bytes());
    place_packet.push(1);
    place_packet.extend_from_slice(&0i16.to_be_bytes());
    place_packet.push(0); // no NBT: single TAG_End byte
    place_packet.extend_from_slice(&[8, 8, 8]);
    client.write_all(&packet(8, &place_packet)).unwrap();
    wait_for(
        client,
        0x23,
        |body| {
            body.len() == 11
                && body[0..4] == x.to_be_bytes()
                && body[4] == landed
                && body[9] == block
        },
        "block update",
    );
}

/// Send a movement the fence must reject: exactly one S08 restoring the
/// given authority, then acknowledge it so the gate clears.
fn rejected(client: &mut BufferedClient, attempt: [f64; 3], authority: [f64; 3], label: &str) {
    client.write_all(&position(attempt)).unwrap();
    let correction = wait_for(
        client,
        0x08,
        |body| body.len() == 33,
        &format!("{label}: rollback"),
    );
    let restored = feet(&correction);
    for (actual, expected) in restored.into_iter().zip(authority) {
        assert!(
            (actual - expected).abs() < 1e-6,
            "{label}: must restore authority {authority:?}, got {restored:?}"
        );
    }
    client.write_all(&position(authority)).unwrap();
}

#[test]
fn next026_fence_top_is_one_and_a_half_blocks_over_ground() {
    let mut server = Server::start();
    let (mut mover, _, origin) = server.join("FenceTop");
    assert_eq!(origin, [0.5, 64.0, 0.5]);

    // Build: a real fence on the flat ground through the place packet
    // (possible because fences are ordinary placeable blocks), then back
    // to Survival where movement validation runs.
    server.console("/gamemode creative FenceTop");
    wait_for(
        &mut mover,
        0x39,
        |body| body.first() == Some(&0x0d),
        "creative abilities",
    );
    give(&mut mover, 85);
    place(&mut mover, 5, 85, 63, 64, 85);
    server.console("/gamemode survival FenceTop");
    wait_for(
        &mut mover,
        0x39,
        |body| body.first() == Some(&0),
        "survival abilities",
    );

    // Walk up to the fence: every stroke short of its AABB is accepted.
    let mut walk = Vec::new();
    let mut x = origin[0];
    while x < 4.5 {
        x += 0.5;
        walk.extend_from_slice(&position([x, 64.0, 0.5]));
    }
    mover.write_all(&walk).unwrap();
    barrier(&mut mover, "next026 walk to fence");

    // Walking into the fence cell is rejected; the 1.5-tall volume leaves
    // no free corridor at ground level.
    rejected(
        &mut mover,
        [5.5, 64.0, 0.5],
        [4.5, 64.0, 0.5],
        "walk into fence",
    );

    // A normal jump cannot mount or clear the fence: the measured apex
    // pose (1.249 blocks up) still sits inside the 1.5 collision box.
    rejected(
        &mut mover,
        [5.5, 65.249187078745, 0.5],
        [4.5, 64.0, 0.5],
        "jump apex inside fence",
    );
    // And the exact 1.5 top is out of reach of the jump itself (dy 1.5
    // exceeds the 1.25 jump limit).
    rejected(
        &mut mover,
        [5.5, 65.5, 0.5],
        [4.5, 64.0, 0.5],
        "jump onto fence top",
    );

    // Teleport onto the fence top: the exact 1.5 top supports standing —
    // repeated reports there earn no gravity correction for 700 ms.
    server.console("/tp FenceTop 5.5 65.5 0.5");
    let landed = wait_for(
        &mut mover,
        0x08,
        |body| body.len() == 33 && (feet(body)[0] - 5.5).abs() < 0.01,
        "teleport onto fence top",
    );
    let top = feet(&landed);
    assert!(
        (top[1] - 65.5).abs() < 1.0 / 16.0,
        "teleport must land on the 1.5 top: {top:?}"
    );
    mover.write_all(&position(top)).unwrap();
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(700) {
        mover.write_all(&position(top)).unwrap();
        thread::sleep(Duration::from_millis(90));
        while let Some((id, body)) = pump(&mut mover) {
            if id == 0x00 && body.len() == 4 {
                mover.write_all(&packet(0, &body)).unwrap();
            }
            assert_ne!(id, 0x08, "fence top failed to support standing: {body:?}");
            assert_ne!(id, 0x40, "disconnected while standing: {body:?}");
        }
    }

    // Stepping off the top and dropping 1.5 blocks to the ground is an
    // ordinary accepted movement.
    mover.write_all(&position([4.5, top[1], 0.5])).unwrap();
    mover.write_all(&position([4.5, 64.0, 0.5])).unwrap();
    barrier(&mut mover, "next026 step off the fence");

    // And walking back into the fence from the other side is again the
    // same vanilla rejection.
    rejected(
        &mut mover,
        [5.5, 64.0, 0.5],
        [4.5, 64.0, 0.5],
        "re-enter fence",
    );

    drop(mover);
    server.stop();
}
