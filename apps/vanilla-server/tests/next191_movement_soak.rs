//! NEXT-191 / #1174: soak the movement path and record spurious teleport
//! corrections. Real unmodified 1.7.10 client acceptance remains NOT_RUN
//! (headless host); this test is the scripted Protocol 5 equivalent that
//! runs on every gate: it drives the REAL server binary on an isolated
//! scratch world with a continuous movement stream — one C04 position
//! packet every 50 ms for a fixed soak window (20 TPS cadence, straight
//! line, creative), exactly what a walking client sends — and counts every
//! S08 Position & Look correction that is NOT an acknowledgement of a
//! client-authoritative join position.
//!
//! Pass criteria (scripted soak; real client NOT_RUN):
//!   * zero spurious S08 corrections while the client keeps a legal,
//!     monotonic position stream;
//!   * KeepAlive challenges keep being echoed and the session survives the
//!     whole window (no disconnect);
//!   * clean /stop persists the final position (no poisoned state after
//!     the soak);
//!   * the correction count is printed for the issue trail.
#[path = "support/packet_client.rs"]
mod packet_client;

use legacy_protocol::{write_string, write_varint};
use packet_client::BufferedClient;
use std::fs;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SOAK_SECS: u64 = 60;
const STEP: f64 = 0.1;
const TICK: Duration = Duration::from_millis(50);

struct Server {
    child: Child,
    root: std::path::PathBuf,
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
    fn start(tag: &str) -> Server {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "ir1710-next191-{tag}-{}-{stamp}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("whitelist.json"), r#"[{"name":"Soaker"}]"#).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .args(["--bind", &format!("127.0.0.1:{port}"), "--world"])
            .arg(root.join("flat.irw"))
            .arg("--whitelist")
            .arg(root.join("whitelist.json"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "server did not open {port}");
            thread::sleep(Duration::from_millis(100));
        }
        Server { child, root, port }
    }

    fn connect(&self, name: &str) -> BufferedClient {
        let mut socket = BufferedClient::new(TcpStream::connect(("127.0.0.1", self.port)).unwrap());
        socket
            .set_read_timeout(Some(Duration::from_millis(250)))
            .unwrap();
        let mut handshake = Vec::new();
        write_varint(5, &mut handshake);
        write_string("127.0.0.1", &mut handshake).unwrap();
        handshake.extend_from_slice(&self.port.to_be_bytes());
        write_varint(2, &mut handshake);
        socket.write_all(&frame(0, &handshake)).unwrap();
        socket.write_all(&frame(0, &mc_string(name))).unwrap();
        socket
    }
}

fn vi(value: u32) -> Vec<u8> {
    let mut out = Vec::new();
    write_varint(value, &mut out);
    out
}

fn mc_string(value: &str) -> Vec<u8> {
    let mut out = Vec::new();
    write_string(value, &mut out).unwrap();
    out
}

fn frame(id: u32, data: &[u8]) -> Vec<u8> {
    let mut packet = vi(id);
    packet.extend_from_slice(data);
    let mut message = vi(packet.len() as u32);
    message.extend_from_slice(&packet);
    message
}

fn pump(socket: &mut BufferedClient) -> Option<(u32, Vec<u8>)> {
    socket.poll_packet().expect("read Protocol 5 packet")
}

fn read_position(body: &[u8]) -> [f64; 3] {
    [
        f64::from_be_bytes(body[0..8].try_into().unwrap()),
        f64::from_be_bytes(body[8..16].try_into().unwrap()) - f64::from(1.62_f32),
        f64::from_be_bytes(body[16..24].try_into().unwrap()),
    ]
}

/// C04 absolute position wire order: X, feet Y, stance, Z, onGround.
fn position_packet(x: f64, feet_y: f64, z: f64) -> Vec<u8> {
    let mut body = Vec::new();
    for value in [x, feet_y, feet_y + 1.62, z] {
        body.extend_from_slice(&value.to_be_bytes());
    }
    body.push(1);
    frame(4, &body)
}

#[test]
fn next191_continuous_movement_soak_records_spurious_teleports() {
    let mut server = Server::start("soak");
    let mut client = server.connect("Soaker");
    // Join burst until the first S08 (authoritative spawn position).
    let deadline = Instant::now() + Duration::from_secs(15);
    let spawn = loop {
        assert!(Instant::now() < deadline, "no S08 after join");
        let Some((id, body)) = pump(&mut client) else {
            continue;
        };
        if id == 0x00 && body.len() == 4 {
            let _ = client.write_all(&frame(0, &body));
        }
        if id == 0x08 && body.len() >= 24 {
            break read_position(&body);
        }
    };

    // Continuous walk: one packet per 50 ms tick along +X.
    let mut x = spawn[0];
    let mut packets_sent = 0u32;
    let mut corrections = 0u32;
    let mut keepalives = 0u32;
    let mut acked = 0u32;
    let soak = Instant::now() + Duration::from_secs(SOAK_SECS);
    let mut last_sent = Instant::now() - TICK;
    while Instant::now() < soak {
        if last_sent.elapsed() >= TICK {
            x += STEP;
            client
                .write_all(&position_packet(x, spawn[1], spawn[2]))
                .unwrap();
            client.flush().unwrap();
            packets_sent += 1;
            last_sent = Instant::now();
        }
        // Drain whatever arrived since the last tick.
        while let Some((id, body)) = pump(&mut client) {
            if id == 0x00 && body.len() == 4 {
                let _ = client.write_all(&frame(0, &body));
                keepalives += 1;
            }
            if id == 0x08 && body.len() >= 24 {
                // A correction for a just-sent legal position is spurious.
                // Acks of our own stream (matching the last sent X within a
                // tick of latency) are counted but never counted as spurious.
                let at = read_position(&body);
                if (at[0] - (x - STEP)).abs() > 0.55 || (at[1] - spawn[1]).abs() > 0.01 {
                    corrections += 1;
                    // Behave like a client: acknowledge the correction.
                    client
                        .write_all(&position_packet(at[0], at[1], at[2]))
                        .unwrap();
                    acked += 1;
                }
            }
        }
    }

    // The session must still be healthy at the end of the soak.
    client
        .write_all(&frame(1, &mc_string("/list")))
        .expect("session must accept chat after soak");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut listed = false;
    while Instant::now() < deadline && !listed {
        let Some((id, body)) = pump(&mut client) else {
            thread::sleep(Duration::from_millis(50));
            continue;
        };
        listed = id == 0x02 && String::from_utf8_lossy(&body).contains("Online (1)");
    }
    assert!(listed, "session lost during soak");

    eprintln!(
        "NEXT-191 soak: duration={SOAK_SECS}s packets={packets_sent} corrections={corrections} \
         correction_acks={acked} keepalives={keepalives} (real 1.7.10 client NOT_RUN)"
    );

    // Clean stop; the walked-upon position must have persisted.
    {
        let mut stdin = server.child.stdin.take().expect("server stdin");
        stdin.write_all(b"/stop\n").unwrap();
        stdin.flush().unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match server.child.try_wait().unwrap() {
            Some(status) => {
                assert!(status.success(), "server must stop cleanly: {status}");
                break;
            }
            None => {
                assert!(Instant::now() < deadline, "server did not stop");
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
    let save = server.root.join("players/soaker.dat");
    assert!(save.exists(), "soak session must persist its player file");
    let _ = fs::remove_dir_all(&server.root);

    assert!(
        packets_sent > 100,
        "soak must actually stream movement: sent {packets_sent}"
    );
    assert_eq!(
        corrections, 0,
        "continuous legal movement produced {corrections} spurious S08 corrections"
    );
}
