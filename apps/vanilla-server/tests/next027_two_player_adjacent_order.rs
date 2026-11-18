//! NEXT-027 / #1010: two players standing inside each other's hitboxes
//! (1.7.10 has no player-vs-player push-out) must keep authoritative
//! positions that never depend on which session's packets the server
//! processed first. Both sessions swap places through one another with
//! interleaved packets, then with opposite-order bursts, and every
//! rejoin must read back exactly the last position each client reported.
//! Uses the native Rust server in Survival and disposable data.
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
            "industrialrust-next027-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join("whitelist.json"),
            r#"[{"name":"WalkerOne"},{"name":"WalkerTwo"}]"#,
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

    fn join(&self, name: &str) -> (BufferedClient, [f64; 3]) {
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
                0x08 if body.len() == 33 => {
                    let position = [
                        f64::from_be_bytes(body[..8].try_into().unwrap()),
                        f64::from_be_bytes(body[8..16].try_into().unwrap()) - f64::from(1.62_f32),
                        f64::from_be_bytes(body[16..24].try_into().unwrap()),
                    ];
                    return (socket, position);
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

/// A chat echo follows everything this session sent earlier: no S08
/// correction (no push-out, no rollback) may appear before the echo.
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
            0x08 => panic!("spurious S08 (push-out?) during {marker:?}: {body:?}"),
            0x06 if body.len() >= 4 && f32::from_be_bytes(body[..4].try_into().unwrap()) < 20.0 => {
                panic!("damage during {marker:?}: {body:?}");
            }
            0x40 => panic!("disconnected during {marker:?}: {body:?}"),
            0x02 if body.windows(marker.len()).any(|w| w == marker.as_bytes()) => return,
            _ => {}
        }
    }
}

fn assert_close(actual: [f64; 3], expected: [f64; 3], label: &str) {
    for (actual, expected) in actual.into_iter().zip(expected) {
        assert!(
            (actual - expected).abs() < 1e-6,
            "{label}: {actual} != {expected}"
        );
    }
}

#[test]
fn next027_adjacent_two_player_swaps_never_diverge_regardless_of_order() {
    let mut server = Server::start();
    let (mut one, origin_one) = server.join("WalkerOne");
    let (mut two, origin_two) = server.join("WalkerTwo");
    // Both sessions share the world spawn: their hitboxes fully overlap
    // from the first tick, exactly the densest "adjacent" case.
    assert_eq!(origin_one, [0.5, 64.0, 0.5]);
    assert_eq!(origin_two, [0.5, 64.0, 0.5]);

    // Separate along z so the swap runs head-on through each other.
    let mut opening = Vec::new();
    let mut z = 0.5;
    while z < 2.5 {
        z += 0.5;
        opening.extend_from_slice(&position([0.5, 64.0, z]));
    }
    two.write_all(&opening).unwrap();
    barrier(&mut two, "next027 separate");
    barrier(&mut one, "next027 separate observer");

    // Swap #1: strictly interleaved reports. The two sessions are served
    // by independent threads, so the server's processing order is
    // arbitrary already; the hitboxes overlap for every step of the swap.
    for step in 1..=8 {
        let offset = 0.25 * f64::from(step);
        one.write_all(&position([0.5, 64.0, 0.5 + offset])).unwrap();
        two.write_all(&position([0.5, 64.0, 2.5 - offset])).unwrap();
    }
    barrier(&mut one, "next027 interleaved swap one");
    barrier(&mut two, "next027 interleaved swap two");

    // Swap #2: opposite-order bursts — one session's whole path is
    // queued before the other's even begins.
    let mut burst_one = Vec::new();
    for step in 1..=8 {
        burst_one.extend_from_slice(&position([0.5, 64.0, 2.5 - 0.25 * f64::from(step)]));
    }
    one.write_all(&burst_one).unwrap();
    barrier(&mut one, "next027 burst one first");
    let mut burst_two = Vec::new();
    for step in 1..=8 {
        burst_two.extend_from_slice(&position([0.5, 64.0, 0.5 + 0.25 * f64::from(step)]));
    }
    two.write_all(&burst_two).unwrap();
    barrier(&mut two, "next027 burst two last");

    // Exactly the reported positions survived: no session was ever
    // nudged by the other's presence or processing order.
    let last_one = [0.5, 64.0, 0.5];
    let last_two = [0.5, 64.0, 2.5];

    // Disconnect both; each rejoin must read back the last reported
    // position, proving the authority never diverged mid-swap.
    drop(one);
    drop(two);
    thread::sleep(Duration::from_millis(250));
    let (mut one, saved_one) = server.join("WalkerOne");
    assert_close(saved_one, last_one, "WalkerOne rejoin position");
    barrier(&mut one, "next027 one rejoined");
    drop(one);
    thread::sleep(Duration::from_millis(250));
    let (mut two, saved_two) = server.join("WalkerTwo");
    assert_close(saved_two, last_two, "WalkerTwo rejoin position");
    barrier(&mut two, "next027 two rejoined");
    drop(two);
    server.stop();
}
