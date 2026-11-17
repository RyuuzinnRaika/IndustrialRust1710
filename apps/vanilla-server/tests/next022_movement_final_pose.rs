//! NEXT-022 / #1005: a burst of accepted C04/C05/C06 packets must publish
//! its final position and view to another player even when input goes quiet.
//! Uses the native Rust server, two Creative sessions, and disposable data.
//! Unmodified Minecraft 1.7.10 client acceptance remains untested here.

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
            "industrialrust-next022-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join("whitelist.json"),
            r#"[{"name":"PoseMover"},{"name":"PoseObserver"}]"#,
        )
        .unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let log = root.join("server.log");
        let output = File::create(&log).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .current_dir(&root)
            .args(["--bind", &format!("127.0.0.1:{port}"), "--creative"])
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
                    assert_eq!(body[4] & 7, 1, "test sessions must be Creative");
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

fn position(id: u32, xyz: [f64; 3], yaw: f32, pitch: f32) -> Vec<u8> {
    let mut body = Vec::new();
    for value in [xyz[0], xyz[1], xyz[1] + 1.62, xyz[2]] {
        body.extend_from_slice(&value.to_be_bytes());
    }
    if id == 6 {
        body.extend_from_slice(&yaw.to_be_bytes());
        body.extend_from_slice(&pitch.to_be_bytes());
    }
    body.push(1);
    packet(id, &body)
}

fn look(yaw: f32, pitch: f32) -> Vec<u8> {
    let mut body = yaw.to_be_bytes().to_vec();
    body.extend_from_slice(&pitch.to_be_bytes());
    body.push(1);
    packet(5, &body)
}

fn processing_barrier(mover: &mut BufferedClient, marker: &str, expect_correction: bool) {
    // Chat follows the burst on the same stream. Its echo proves that the
    // owning session processed every movement sample, without sending an
    // extra movement packet that could itself flush a stale fanout cache.
    let mut body = Vec::new();
    write_string(marker, &mut body).unwrap();
    mover.write_all(&packet(1, &body)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut corrected = false;
    loop {
        assert!(
            Instant::now() < deadline,
            "movement processing barrier timed out"
        );
        let Some((id, body)) = pump(mover) else {
            continue;
        };
        match id {
            0x00 if body.len() == 4 => mover.write_all(&packet(0, &body)).unwrap(),
            0x08 => corrected = true,
            0x40 => panic!("mover disconnected: {body:?}"),
            0x02 if body
                .windows(marker.len())
                .any(|part| part == marker.as_bytes()) =>
            {
                assert_eq!(
                    corrected, expect_correction,
                    "accepted bursts must have no S08; the rejected sample must cause one"
                );
                return;
            }
            _ => {}
        }
    }
}

fn await_pose(observer: &mut BufferedClient, entity: i32, xyz: [f64; 3], angles: [u8; 2]) {
    let expected = xyz.map(|value| (value * 32.0).floor() as i32);
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut last = None;
    let mut final_seen_at = None;
    loop {
        if final_seen_at.is_some_and(|seen: Instant| seen.elapsed() >= Duration::from_millis(250)) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "quiet mover's final pose was never delivered: expected {expected:?}/{angles:?}, last {last:?}"
        );
        let Some((id, body)) = pump(observer) else {
            continue;
        };
        if id == 0x00 && body.len() == 4 {
            observer.write_all(&packet(0, &body)).unwrap();
        }
        if id == 0x40 {
            panic!("observer disconnected: {body:?}");
        }
        if id == 0x18 && body.len() == 18 && body[..4] == entity.to_be_bytes() {
            let coords = std::array::from_fn::<_, 3, _>(|index| {
                i32::from_be_bytes(body[4 + index * 4..8 + index * 4].try_into().unwrap())
            });
            let actual_angles = [body[16], body[17]];
            last = Some((coords, actual_angles));
            if coords == expected && actual_angles == angles {
                final_seen_at.get_or_insert_with(Instant::now);
            } else {
                assert!(
                    final_seen_at.is_none(),
                    "a stale pose followed the final accepted pose: {last:?}"
                );
            }
        }
    }
}

#[test]
fn next022_quiet_mover_eventually_publishes_final_position_and_look() {
    let mut server = Server::start();
    let (mut mover, mover_id, origin) = server.join("PoseMover");
    let (mut observer, _, _) = server.join("PoseObserver");

    // All samples remain in one chunk and above solid ground, so neither
    // a chunk-interest transition nor idle gravity can flush the final pose.
    // One write makes adjacent samples arrive together; the distinct final
    // sample follows many earlier accepted samples within the fanout window.
    let final_position = [origin[0] + 1.0, origin[1], origin[2] + 0.5];
    let mut burst = position(6, origin, 0.0, 0.0);
    for index in 0..32 {
        let offset = f64::from(index % 4) / 32.0;
        burst.extend_from_slice(&position(
            4,
            [origin[0] + offset, origin[1], origin[2]],
            0.0,
            0.0,
        ));
        burst.extend_from_slice(&look(45.0, 0.0));
    }
    // Exact multiples of 360/256 avoid rounding ambiguity: yaw=96, pitch=32.
    burst.extend_from_slice(&position(6, final_position, 135.0, 45.0));
    mover.write_all(&burst).unwrap();
    processing_barrier(&mut mover, "next022 first accepted burst", false);
    await_pose(&mut observer, mover_id, final_position, [96, 32]);

    // C04 must retain the last C05 view; a later C05 must be published even
    // when it changes only the view and the client then becomes silent.
    let second_position = [origin[0] + 1.5, origin[1], origin[2] + 1.0];
    let mut burst = position(6, final_position, 180.0, 0.0);
    for _ in 0..32 {
        burst.extend_from_slice(&look(225.0, 45.0));
        burst.extend_from_slice(&position(4, second_position, 0.0, 0.0));
    }
    burst.extend_from_slice(&look(270.0, -45.0));
    mover.write_all(&burst).unwrap();
    processing_barrier(&mut mover, "next022 second accepted burst", false);
    await_pose(&mut observer, mover_id, second_position, [192, 224]);

    // A rejected C06 parses its angles before rejecting out-of-bounds XYZ.
    // Deferred fanout must retain the last accepted snapshot, rather than
    // accidentally attaching those rejected angles to the accepted position.
    let third_position = [origin[0] + 2.0, origin[1], origin[2] + 1.5];
    let mut burst = position(6, second_position, 0.0, 0.0);
    for _ in 0..32 {
        burst.extend_from_slice(&look(45.0, 0.0));
    }
    burst.extend_from_slice(&position(6, third_position, 270.0, 0.0));
    burst.extend_from_slice(&look(315.0, -90.0));
    burst.extend_from_slice(&position(6, [1e18, origin[1], origin[2]], 0.0, 45.0));
    mover.write_all(&burst).unwrap();
    processing_barrier(
        &mut mover,
        "next022 rejected sample after accepted burst",
        true,
    );
    await_pose(&mut observer, mover_id, third_position, [224, 192]);

    drop(mover);
    drop(observer);
    server.stop();
}
