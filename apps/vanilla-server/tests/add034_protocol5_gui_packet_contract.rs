//! ADD-034 / #458: contract test — a Protocol 5 (1.7.10) client
//! must never receive a packet that does not exist in the 1.7.10
//! play-state table, and the window/GUI family it does receive
//! must be exactly the 1.7.10 set. The test drives one session
//! through the GUI surface (chest, furnace, workbench, sign
//! editor, clicks) and records every clientbound packet id.
//!
//! Real-client verification with an unmodified 1.7.10 client: NOT_RUN.

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
    seen: std::collections::BTreeSet<i32>,
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
            seen: std::collections::BTreeSet::new(),
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
                    self.seen.insert(id);
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
    let dir = std::env::temp_dir().join(format!("ir1710-add034-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("whitelist.json"), r#"[{"name":"Keeper034"}]"#).unwrap();
    let stderr = std::fs::File::create(dir.join("server-stderr.log")).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
        .args(["--bind", &format!("127.0.0.1:{port}"), "--world"])
        .arg(dir.join("flat.irw"))
        .arg("--whitelist")
        .arg(dir.join("whitelist.json"))
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

fn creative_set(client: &mut Client, slot: i16, id: i16, count: u8) {
    let mut p = slot.to_be_bytes().to_vec();
    p.extend_from_slice(&id.to_be_bytes());
    p.push(count);
    p.extend_from_slice(&0i16.to_be_bytes());
    p.extend_from_slice(&(-1i16).to_be_bytes());
    client.send(&frame(0x10, &p));
    let want = [0u8, (slot >> 8) as u8, slot as u8];
    client
        .until(Duration::from_secs(5), |id, payload| {
            id == 0x2f && payload.starts_with(&want)
        })
        .expect("creative set must be confirmed by Set Slot");
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

/// Window click; returns the Window Items payload that follows.
fn click(client: &mut Client, window: u8, slot: i16, action: i16) -> Vec<u8> {
    let mut p = vec![window];
    p.extend_from_slice(&slot.to_be_bytes());
    p.push(0); // left button
    p.extend_from_slice(&action.to_be_bytes());
    p.push(0); // mode: normal click
    p.extend_from_slice(&(-1i16).to_be_bytes());
    client.send(&frame(0x0e, &p));
    let want = [window, action.to_be_bytes()[0], action.to_be_bytes()[1], 1];
    client
        .until(Duration::from_secs(5), |id, payload| {
            id == 0x32 && payload == want
        })
        .expect("click must be confirmed as accepted");
    client
        .until(Duration::from_secs(5), |id, payload| {
            id == 0x30 && payload.first() == Some(&window)
        })
        .expect("click must be followed by Window Items")
}

/// The complete 1.7.10 (protocol 5) clientbound play-state id range.
fn is_protocol5_play_id(id: i32) -> bool {
    (0x00..=0x40).contains(&id)
}

/// Open a container window and wait for its Open Window packet;
/// returns the window id.
fn open_window(client: &mut Client, x: i32, y: u8, z: i32) -> u8 {
    use_block(client, x, y, z, None);
    let opened = client
        .until(Duration::from_secs(5), |id, _| id == 0x2d)
        .expect("a window must open");
    let window = opened[0];
    client
        .until(Duration::from_secs(5), |id, payload| {
            id == 0x30 && payload.first() == Some(&window)
        })
        .expect("window items must follow the open");
    window
}

#[test]
fn add034_only_protocol5_packets_reach_the_client() {
    let (mut server, dir, port) = spawn_server("contract");
    wait_port(port);
    let (mut client, spawn) = join(port, "Keeper034");
    let gx = spawn.0.floor() as i32;
    let gz = spawn.2.floor() as i32;

    // Chest: place, open, click a slot, close.
    creative_set(&mut client, 36, 54, 1);
    use_block(&mut client, gx, 63, gz, Some((54, 1)));
    client
        .until(Duration::from_secs(5), |id, payload| {
            id == 0x23 && payload.len() >= 11 && payload[9] == 54
        })
        .expect("chest placement");
    let chest_window = open_window(&mut client, gx, 64, gz);
    let _ = click(&mut client, chest_window, 0, 1);
    client.send(&frame(0x0d, &[chest_window]));

    // Furnace one block over: its window adds Window Property (0x31).
    creative_set(&mut client, 36, 61, 1);
    use_block(&mut client, gx + 2, 63, gz, Some((61, 1)));
    client
        .until(Duration::from_secs(5), |id, payload| {
            id == 0x23 && payload.len() >= 11 && payload[9] == 61
        })
        .expect("furnace placement");
    let furnace_window = open_window(&mut client, gx + 2, 64, gz);
    client
        .until(Duration::from_secs(5), |id, _| id == 0x31)
        .expect("furnace window must send Window Property packets");
    client.send(&frame(0x0d, &[furnace_window]));

    // Workbench: another window kind through the same family.
    creative_set(&mut client, 36, 58, 1);
    use_block(&mut client, gx + 4, 63, gz, Some((58, 1)));
    client
        .until(Duration::from_secs(5), |id, payload| {
            id == 0x23 && payload.len() >= 11 && payload[9] == 58
        })
        .expect("workbench placement");
    let bench_window = open_window(&mut client, gx + 4, 64, gz);
    client.send(&frame(0x0d, &[bench_window]));

    // Placing a sign opens the 1.7.10 sign editor (0x36).
    creative_set(&mut client, 36, 63, 1);
    use_block(&mut client, gx + 6, 63, gz, Some((63, 1)));
    client
        .until(Duration::from_secs(5), |id, _| id == 0x36)
        .expect("sign placement must open the sign editor");

    client.collect(Duration::from_secs(1));
    let seen = client.seen.clone();

    // Non-vacuous: the GUI family really was exercised.
    for expected in [0x2d, 0x2f, 0x30, 0x31, 0x32, 0x36] {
        assert!(
            seen.contains(&expected),
            "GUI packet {expected:#04x} was never exercised; seen: {seen:?}"
        );
    }
    // Contract 1: nothing outside the protocol-5 play table at all.
    let foreign: Vec<i32> = seen
        .iter()
        .copied()
        .filter(|id| !is_protocol5_play_id(*id))
        .collect();
    assert!(
        foreign.is_empty(),
        "non-1.7.10 packet ids received: {foreign:?}"
    );
    // Contract 2: the window/GUI family is exactly the 1.7.10 set.
    const GUI_FAMILY: [i32; 7] = [0x2d, 0x2e, 0x2f, 0x30, 0x31, 0x32, 0x36];
    let gui_seen: Vec<i32> = seen
        .iter()
        .copied()
        .filter(|id| (0x2d..=0x36).contains(id) && *id != 0x34)
        .collect();
    for id in &gui_seen {
        assert!(
            GUI_FAMILY.contains(id),
            "packet {id:#04x} is in the window range but not a 1.7.10 GUI packet"
        );
    }

    drop(client);
    {
        let mut stdin = server.stdin.take().unwrap();
        stdin.write_all(b"/stop\n").unwrap();
    }
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
