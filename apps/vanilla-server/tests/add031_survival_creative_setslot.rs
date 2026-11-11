//! ADD-031 / #455: on a survival-mode server, the Creative
//! Inventory Action packet (0x10) must never mint items. The
//! session layer consumes and ignores it for non-creative players
//! and resynchronizes the real inventory; this test sends raw C10
//! packets for valuable items and proves they appear neither in
//! the resynchronized window nor in the saved inventory file.
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
    let dir = std::env::temp_dir().join(format!("ir1710-add031-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("whitelist.json"), r#"[{"name":"Miner031"}]"#).unwrap();
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

/// Send a Creative Inventory Action packet without expecting any
/// confirmation: on a survival server there must be none.
fn creative_send(client: &mut Client, slot: i16, id: i16, count: u8) {
    let mut p = slot.to_be_bytes().to_vec();
    p.extend_from_slice(&id.to_be_bytes());
    p.push(count);
    p.extend_from_slice(&0i16.to_be_bytes());
    p.extend_from_slice(&(-1i16).to_be_bytes());
    client.send(&frame(0x10, &p));
}

/// Item ids contained in a Window Items payload for window 0.
fn window_item_ids(payload: &[u8]) -> Vec<i16> {
    let mut ids = Vec::new();
    if payload.len() < 3 {
        return ids;
    }
    let count = i16::from_be_bytes([payload[1], payload[2]]) as usize;
    let mut at = 3;
    for _ in 0..count {
        if at + 2 > payload.len() {
            break;
        }
        let id = i16::from_be_bytes([payload[at], payload[at + 1]]);
        at += 2;
        ids.push(id);
        if id != -1 {
            at += 3; // count + damage
            if at + 2 > payload.len() {
                break;
            }
            let nbt = i16::from_be_bytes([payload[at], payload[at + 1]]);
            at += 2;
            if nbt > 0 {
                at += nbt as usize;
            }
        }
    }
    ids
}

fn json_contains_id(value: &Value, want: i64) -> bool {
    match value {
        Value::Object(map) => {
            map.get("id").and_then(Value::as_i64) == Some(want)
                || map.values().any(|v| json_contains_id(v, want))
        }
        Value::Array(items) => items.iter().any(|v| json_contains_id(v, want)),
        _ => false,
    }
}

#[test]
fn add031_survival_server_ignores_creative_inventory_action() {
    let (mut server, dir, port) = spawn_server("survival-c10");
    wait_port(port);
    let (mut client, _spawn) = join(port, "Miner031");

    // Diamond x64 into hotbar slot 36 via Creative Inventory Action.
    creative_send(&mut client, 36, 264, 64);
    let resync = client
        .until(Duration::from_secs(5), |id, payload| {
            id == 0x30 && payload.first() == Some(&0)
        })
        .expect("survival C10 must be answered with a window resync");
    let ids = window_item_ids(&resync);
    assert!(
        !ids.contains(&264),
        "diamond must not appear in the resynchronized inventory: {ids:?}"
    );

    // A second attempt with a different slot and item.
    creative_send(&mut client, 40, 279, 1);
    let resync = client
        .until(Duration::from_secs(5), |id, payload| {
            id == 0x30 && payload.first() == Some(&0)
        })
        .expect("second C10 must also resync");
    let ids = window_item_ids(&resync);
    assert!(
        !ids.contains(&279),
        "diamond pickaxe must not appear: {ids:?}"
    );
    assert!(!ids.contains(&264), "diamond must not appear: {ids:?}");

    // The persisted inventory must be clean as well.
    {
        let stdin = server.stdin.as_mut().unwrap();
        stdin.write_all(b"/stop\n").unwrap();
    }
    let _ = server.wait();
    let saved: Value = serde_json::from_slice(
        &std::fs::read(dir.join("players/miner031.survival.inventory.json")).unwrap(),
    )
    .unwrap();
    assert!(
        !json_contains_id(&saved, 264),
        "saved inventory holds a diamond"
    );
    assert!(
        !json_contains_id(&saved, 279),
        "saved inventory holds a diamond pickaxe"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
