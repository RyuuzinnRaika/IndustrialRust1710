//! NEXT-023 / #1006: the moment a player leaves flowing water for land
//! must never trigger a server speed/rollback correction, and a genuinely
//! rejected movement must produce exactly one S08 that is never repeated
//! while unacknowledged. Uses the native Rust server in survival (movement
//! validation is the survival path), a real water source placed through the
//! Protocol 5 place packet, and disposable data. Unmodified Minecraft 1.7.10
//! client acceptance remains untested here.

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
            "industrialrust-next023-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("whitelist.json"), r#"[{"name":"ShoreMover"}]"#).unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let log = root.join("server.log");
        let output = File::create(&log).unwrap();
        // Survival: valid_movement collision validation and the teleport
        // gate only run on the survival movement path.
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

/// C06 Position with view and onGround=true; S08 carries feet+1.62 eye Y.
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

/// A chat echo follows every movement processed earlier on the same
/// session stream: no S08 may appear between here and the echo.
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

/// Creative-give the water block item and place a source at (x, 64, 0)
/// on top of the flat ground, exactly as the 1.7.10 place packet does.
fn place_water(client: &mut BufferedClient, x: i32) {
    let mut give = 36i16.to_be_bytes().to_vec();
    give.extend_from_slice(&8i16.to_be_bytes());
    give.push(1);
    give.extend_from_slice(&0i16.to_be_bytes());
    give.extend_from_slice(&(-1i16).to_be_bytes());
    client.write_all(&packet(0x10, &give)).unwrap();
    wait_for(
        client,
        0x2f,
        |body| body.len() == 10 && body[3..5] == 8i16.to_be_bytes(),
        "water slot echo",
    );
    let mut place = x.to_be_bytes().to_vec();
    place.push(63); // bottom block top face -> the new block lands at y=64
    place.extend_from_slice(&0i32.to_be_bytes());
    place.push(1); // face: top
    place.extend_from_slice(&8i16.to_be_bytes());
    place.push(1);
    place.extend_from_slice(&0i16.to_be_bytes());
    place.extend_from_slice(&(-1i16).to_be_bytes());
    place.extend_from_slice(&[8, 8, 8]);
    client.write_all(&packet(8, &place)).unwrap();
    wait_for(
        client,
        0x23,
        |body| body.len() == 11 && body[0..4] == x.to_be_bytes() && body[4] == 64 && body[9] == 8,
        "water block update",
    );
}

#[test]
fn next023_water_land_crossing_never_rolls_back_and_corrections_never_repeat() {
    let mut server = Server::start();
    let (mut mover, _, origin) = server.join("ShoreMover");
    assert_eq!(origin, [0.5, 64.0, 0.5]);

    // Two water cells at ground level: the crossing passes through both.
    server.console("/gamemode creative ShoreMover");
    wait_for(
        &mut mover,
        0x39,
        |body| body.first() == Some(&0x0d),
        "creative abilities",
    );
    place_water(&mut mover, 6);
    place_water(&mut mover, 7);
    server.console("/gamemode survival ShoreMover");
    wait_for(
        &mut mover,
        0x39,
        |body| body.first() == Some(&0),
        "survival abilities",
    );

    // Swim-scale crossing out (0.25/packet) through both water cells past
    // the water's edge, then fast 1.2-block strokes back onto land,
    // ending on solid ground where the rollback phase starts.
    let mut burst = Vec::new();
    let mut x = origin[0];
    while x < 9.0 {
        x += 0.25;
        burst.extend_from_slice(&position([x, 64.0, 0.5]));
    }
    for x in [7.8, 6.6, 5.4, 4.5] {
        burst.extend_from_slice(&position([x, 64.0, 0.5]));
    }
    mover.write_all(&burst).unwrap();
    barrier(&mut mover, "next023 water-land crossing");

    // A genuine collision rollback happens exactly once: diving into the
    // solid ground below the shore restores the authoritative position.
    mover.write_all(&position([4.5, 62.0, 0.5])).unwrap();
    let correction = wait_for(
        &mut mover,
        0x08,
        |body| body.len() == 33,
        "rollback to authority",
    );
    let restored = feet(&correction);
    for (actual, expected) in restored.into_iter().zip([4.5, 64.0, 0.5]) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "correction must restore the authority: {restored:?}"
        );
    }

    // The vanilla client keeps reporting its stale pose meanwhile: eight
    // more unacknowledged copies of the rejected position must NOT repeat
    // the correction (the gate resends at most once per second, never per
    // packet).
    let mut stale = Vec::new();
    for _ in 0..8 {
        stale.extend_from_slice(&position([4.5, 62.0, 0.5]));
    }
    mover.write_all(&stale).unwrap();
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(700) {
        if let Some((id, body)) = pump(&mut mover) {
            if id == 0x00 && body.len() == 4 {
                mover.write_all(&packet(0, &body)).unwrap();
            }
            assert_ne!(id, 0x08, "pending rollback repeated before its ack");
            assert_ne!(id, 0x40, "disconnected: {body:?}");
        }
    }

    // The exact acknowledgement clears the gate and legal movement resumes
    // without any further correction.
    mover.write_all(&position([4.5, 64.0, 0.5])).unwrap();
    mover.write_all(&position([5.5, 64.0, 0.5])).unwrap();
    barrier(&mut mover, "next023 movement resumed after ack");

    drop(mover);
    server.stop();
}
