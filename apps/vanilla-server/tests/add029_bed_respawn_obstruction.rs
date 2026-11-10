//! ADD-029 / #453: respawn at a claimed bed whose standing cell is
//! obstructed must fall back to a safe position next to the bed —
//! not all the way to world spawn. Fully obstructing the bed's
//! neighborhood restores the world-spawn fallback. The scenario
//! runs on a survival server (bed claims require survival):
//! /time set night, /give a bed, sleep, obstruct, die by falling.
//!
//! Real-client verification with an unmodified 1.7.10 client: NOT_RUN.

use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn varint(value: i32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut v = value as u32;
    loop {
        let mut byte = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if v == 0 {
            return out;
        }
    }
}

fn mc_string(text: &str) -> Vec<u8> {
    let mut out = varint(text.len() as i32);
    out.extend_from_slice(text.as_bytes());
    out
}

fn frame(id: i32, payload: &[u8]) -> Vec<u8> {
    let mut body = varint(id);
    body.extend_from_slice(payload);
    let mut out = varint(body.len() as i32);
    out.extend_from_slice(&body);
    out
}

fn read_varint(data: &[u8]) -> Option<(i32, usize)> {
    let mut value: i64 = 0;
    let mut shift = 0;
    for (i, &byte) in data.iter().enumerate() {
        value |= ((byte & 0x7f) as i64) << shift;
        if byte & 0x80 == 0 {
            return Some((value as i32, i + 1));
        }
        shift += 7;
        if shift > 35 {
            return None;
        }
    }
    None
}

fn try_parse_frame(data: &[u8]) -> Option<(i32, Vec<u8>, usize)> {
    let (len, n1) = read_varint(data)?;
    if len < 0 || data.len() < n1 + len as usize {
        return None;
    }
    let body = &data[n1..n1 + len as usize];
    let (id, n2) = read_varint(body)?;
    Some((id, body[n2..].to_vec(), n1 + len as usize))
}

struct Client {
    stream: TcpStream,
    buffer: Vec<u8>,
    backlog: Vec<(i32, Vec<u8>)>,
}

impl Client {
    fn connect(port: u16) -> Client {
        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        Client {
            stream,
            buffer: Vec::new(),
            backlog: Vec::new(),
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).unwrap();
    }

    fn poll(&mut self) {
        let mut chunk = [0u8; 65536];
        match self.stream.read(&mut chunk) {
            Ok(n) if n > 0 => self.buffer.extend_from_slice(&chunk[..n]),
            _ => {}
        }
        loop {
            match try_parse_frame(&self.buffer) {
                Some((id, payload, used)) => {
                    self.buffer.drain(..used);
                    if id == 0x00 && payload.len() == 4 {
                        // KeepAlive echo.
                        let echo = frame(0x00, &payload);
                        let _ = self.stream.write_all(&echo);
                    }
                    self.backlog.push((id, payload));
                }
                None => return,
            }
        }
    }

    /// Wait for a packet matching `pred`, consuming the backlog.
    fn until(
        &mut self,
        timeout: Duration,
        mut pred: impl FnMut(i32, &[u8]) -> bool,
    ) -> Option<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(pos) = self
                .backlog
                .iter()
                .position(|(id, payload)| pred(*id, payload))
            {
                return Some(self.backlog.remove(pos).1);
            }
            if Instant::now() >= deadline {
                return None;
            }
            self.poll();
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn collect(&mut self, dur: Duration) {
        let deadline = Instant::now() + dur;
        while Instant::now() < deadline {
            self.poll();
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

fn be_f64(data: &[u8], at: usize) -> f64 {
    f64::from_be_bytes(data[at..at + 8].try_into().unwrap())
}

fn spawn_server(tag: &str) -> (Child, std::path::PathBuf, u16) {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let dir = std::env::temp_dir().join(format!("ir1710-add029-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("whitelist.json"), r#"[{"name":"Keeper029"}]"#).unwrap();
    let stderr = std::fs::File::create(dir.join("server-stderr.log")).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
        .args(["--bind", &format!("127.0.0.1:{port}"), "--world"])
        .arg(dir.join("flat.irw"))
        .arg("--whitelist")
        .arg(dir.join("whitelist.json"))
        .arg("--survival")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr))
        .spawn()
        .unwrap();
    (child, dir, port)
}

fn wait_port(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("server did not open port {port}");
}

fn login(port: u16, name: &str) -> Client {
    let mut client = Client::connect(port);
    let mut handshake = varint(5);
    handshake.extend_from_slice(&mc_string("127.0.0.1"));
    handshake.extend_from_slice(&port.to_be_bytes());
    handshake.extend_from_slice(&varint(2));
    client.send(&frame(0, &handshake));
    client.send(&frame(0, &mc_string(name)));
    client
}

/// Join and return (client, spawn position).
fn join(port: u16, name: &str) -> (Client, (f64, f64, f64)) {
    let mut client = login(port, name);
    let pos = client
        .until(Duration::from_secs(10), |id, _| id == 0x08)
        .expect("join must deliver Position & Look");
    client.collect(Duration::from_millis(500));
    (client, (be_f64(&pos, 0), be_f64(&pos, 8), be_f64(&pos, 16)))
}

fn console(server: &mut Child, cmd: &str) {
    let stdin = server.stdin.as_mut().unwrap();
    stdin.write_all(cmd.as_bytes()).unwrap();
    stdin.write_all(b"\n").unwrap();
}

fn use_block(client: &mut Client, x: i32, y: u8, z: i32, item: Option<(i16, u8)>) {
    let mut p = x.to_be_bytes().to_vec();
    p.push(y);
    p.extend_from_slice(&z.to_be_bytes());
    p.push(1); // top face
    match item {
        Some((id, count)) => {
            p.extend_from_slice(&id.to_be_bytes());
            p.push(count);
            p.extend_from_slice(&0i16.to_be_bytes());
            p.extend_from_slice(&(-1i16).to_be_bytes());
        }
        None => p.extend_from_slice(&(-1i16).to_be_bytes()),
    }
    p.extend_from_slice(&[8, 8, 8]);
    client.send(&frame(0x08, &p));
}

/// Protocol 5 C04 Player Position: x, feet y, stance, z, onGround.
fn send_position(client: &mut Client, x: f64, feet_y: f64, z: f64) {
    let mut p = x.to_be_bytes().to_vec();
    p.extend_from_slice(&feet_y.to_be_bytes());
    p.extend_from_slice(&(feet_y + 1.62).to_be_bytes());
    p.extend_from_slice(&z.to_be_bytes());
    p.push(1);
    client.send(&frame(0x04, &p));
}

fn held_change(client: &mut Client, hotbar_index: i16) {
    client.send(&frame(0x09, &hotbar_index.to_be_bytes()));
}

/// Kill the player with a long fall at (x, z): two position
/// claims — one high, one at the surface — make the server's fall
/// tracker apply the full distance as damage (the same pattern as
/// the native survival e2e in protocol5_e2e.rs).
fn die_by_falling(client: &mut Client, _server: &mut Child, x: f64, z: f64) {
    send_position(client, x, 100.0, z);
    send_position(client, x, 64.0, z);
    let dead = client.until(Duration::from_secs(15), |id, payload| {
        id == 0x06
            && payload.len() >= 4
            && f32::from_be_bytes(payload[0..4].try_into().unwrap()) <= 0.0
    });
    assert!(dead.is_some(), "the fall must kill the player");
}

/// Send Perform Respawn and return the respawn position.
fn respawn(client: &mut Client) -> (f64, f64, f64) {
    client.backlog.clear();
    client.send(&frame(0x16, &[0]));
    let pos = client
        .until(Duration::from_secs(10), |id, _| id == 0x08)
        .expect("respawn must deliver Position & Look");
    let (x, y, z) = (be_f64(&pos, 0), be_f64(&pos, 8), be_f64(&pos, 16));
    // Acknowledge the respawn teleport (the packet carries
    // feet + 1.62); further movement is ignored until the ACK.
    send_position(client, x, y - 1.62, z);
    (x, y, z)
}

#[test]
fn add029_obstructed_bed_respawns_adjacent_then_world_spawn() {
    let (mut server, dir, port) = spawn_server("bed-respawn");
    wait_port(port);
    let (mut client, world_spawn) = join(port, "Keeper029");

    console(&mut server, "/time set night");
    console(&mut server, "/give Keeper029 bed 1");
    // The given bed lands in a hotbar slot; the saved inventory is
    // the reliable witness (the give writes it immediately).
    let inventory_path = dir.join("players/keeper029.survival.inventory.json");
    let deadline = Instant::now() + Duration::from_secs(10);
    let slot = loop {
        if let Ok(bytes) = std::fs::read(&inventory_path) {
            if let Ok(saved) = serde_json::from_slice::<Value>(&bytes) {
                if let Some(found) = saved["slots"]
                    .as_array()
                    .and_then(|slots| slots.iter().position(|s| s["id"].as_i64() == Some(355)))
                {
                    break found as i16;
                }
            }
        }
        assert!(Instant::now() < deadline, "/give must deliver the bed");
        std::thread::sleep(Duration::from_millis(200));
    };
    assert!(
        (36..=44).contains(&slot),
        "the bed should land in the hotbar, got slot {slot}"
    );
    held_change(&mut client, slot - 36);
    client.collect(Duration::from_millis(500));

    // Place the bed at the spawn clearing (foot at (0, 64, 0), head
    // toward -z) and sleep in it to claim it.
    use_block(&mut client, 0, 63, 0, Some((355, 1)));
    let placed = client.until(Duration::from_secs(5), |id, payload| {
        id == 0x23 && payload.len() >= 11 && payload[9] == 26
    });
    assert!(placed.is_some(), "bed must be placed");
    use_block(&mut client, 0, 64, 0, None);
    client
        .until(Duration::from_secs(5), |id, _| id == 0x0a)
        .expect("sleeping must send Use Bed and claim the bed");
    client.collect(Duration::from_secs(2));

    // Obstruct the standing cell above the bed foot: respawn must
    // move to an adjacent ground-level cell, not to world spawn.
    console(&mut server, "/setblock 0 65 0 stone");
    client.collect(Duration::from_millis(500));
    die_by_falling(&mut client, &mut server, 2.5, 2.5);
    let (rx, ry, rz) = respawn(&mut client);
    // Position & Look carries feet + 1.62 in this protocol: the
    // world-spawn join value (65.62) is ground level, while standing
    // on the bed itself would read exactly 1.0 higher.
    assert_eq!(
        ry, world_spawn.1,
        "adjacent respawn stands at ground level, not on the bed: {ry}"
    );
    let (cx, cz) = (rx.floor() as i32, rz.floor() as i32);
    assert!(
        (-1..=1).contains(&cx) && (-1..=1).contains(&cz) && (cx, cz) != (0, 0),
        "respawn must be beside the bed, got cell ({cx}, {cz})"
    );

    // Step out of the bed's neighborhood first: the obstruction
    // ring is about to fill the cell the player is standing in.
    send_position(&mut client, 2.5, 64.0, 2.5);
    client.collect(Duration::from_millis(500));

    // Obstruct every adjacent feet cell as well: only then may the
    // world-spawn fallback engage.
    for dx in -1..=1 {
        for dz in -1..=1 {
            if (dx, dz) != (0, 0) {
                console(&mut server, &format!("/setblock {dx} 64 {dz} stone"));
            }
        }
    }
    client.collect(Duration::from_millis(500));
    die_by_falling(&mut client, &mut server, 2.5, 2.5);
    let (rx, ry, rz) = respawn(&mut client);
    assert!(
        (rx - world_spawn.0).abs() < 1.5 && (rz - world_spawn.2).abs() < 1.5 && ry > 64.5,
        "fully obstructed bed must fall back to world spawn, got ({rx}, {ry}, {rz}) vs {world_spawn:?}"
    );

    drop(client);
    console(&mut server, "/stop");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = server.try_wait().unwrap() {
            assert!(status.success(), "server must stop cleanly: {status}");
            break;
        }
        assert!(Instant::now() < deadline, "server did not stop in time");
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
