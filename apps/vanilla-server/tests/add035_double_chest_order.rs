//! ADD-035 / #459: a double chest's merged window order must not
//! depend on which half was placed first or which half is clicked:
//! halves are normalized by coordinate. Reversed build order,
//! deposits into both halves, and a disconnect/reconnect must
//! leave both halves' contents and counts exactly as deposited,
//! in the window and in the persisted per-half store.
//!
//! The break-one-half leg is not covered here: packet-level mining
//! of a chest is currently refused by the server (tile-entity
//! contents cannot become drops yet), so it stays NOT_RUN.
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
    let dir = std::env::temp_dir().join(format!("ir1710-add035-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("whitelist.json"), r#"[{"name":"Keeper035"}]"#).unwrap();
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

fn parse_window_slots(payload: &[u8]) -> Vec<(i16, u8)> {
    let mut slots = Vec::new();
    if payload.len() < 3 {
        return slots;
    }
    let count = i16::from_be_bytes([payload[1], payload[2]]) as usize;
    let mut at = 3;
    for _ in 0..count {
        if at + 2 > payload.len() {
            break;
        }
        let id = i16::from_be_bytes([payload[at], payload[at + 1]]);
        at += 2;
        if id == -1 {
            slots.push((-1, 0));
            continue;
        }
        if at + 3 > payload.len() {
            break;
        }
        let n = payload[at];
        at += 3; // count + damage
        if at + 2 > payload.len() {
            break;
        }
        let nbt = i16::from_be_bytes([payload[at], payload[at + 1]]);
        at += 2;
        if nbt > 0 {
            at += nbt as usize;
        }
        slots.push((id, n));
    }
    slots
}

fn close_window(client: &mut Client, window: u8) {
    client.send(&frame(0x0d, &[window]));
}

#[test]
fn add035_double_chest_order_survives_reconnect_and_half_loss() {
    let (mut server, dir, port) = spawn_server("double-chest");
    wait_port(port);
    let (mut client, spawn) = join(port, "Keeper035");
    let gx = spawn.0.floor() as i32;
    let gz = spawn.2.floor() as i32;

    // Reversed build order: EAST half first, then WEST half.
    creative_set(&mut client, 36, 54, 2);
    use_block(&mut client, gx + 1, 63, gz, Some((54, 1)));
    client
        .until(Duration::from_secs(5), |id, payload| {
            id == 0x23 && payload.len() >= 11 && payload[9] == 54
        })
        .expect("east chest half must be placed");
    use_block(&mut client, gx, 63, gz, Some((54, 1)));
    client
        .until(Duration::from_secs(5), |id, payload| {
            id == 0x23 && payload.len() >= 11 && payload[9] == 54
        })
        .expect("west chest half must be placed");

    // Open via the WEST half: the merged window must have 54 chest
    // slots (90 with the player section).
    use_block(&mut client, gx, 64, gz, None);
    client
        .until(Duration::from_secs(5), |id, payload| {
            id == 0x2d && payload.first() == Some(&1)
        })
        .expect("double chest must open window 1");
    let items = client
        .until(Duration::from_secs(5), |id, payload| {
            id == 0x30 && payload.first() == Some(&1)
        })
        .expect("double chest window items");
    assert_eq!(parse_window_slots(&items).len(), 90);

    // Deposit cobble x10 into window slot 0 (first normalized half)
    // and stone x7 into window slot 27 (second half).
    creative_set(&mut client, 37, 4, 10);
    let _ = click(&mut client, 1, 82, 1);
    let _ = click(&mut client, 1, 0, 2);
    creative_set(&mut client, 38, 1, 7);
    let _ = click(&mut client, 1, 83, 3);
    client.backlog.clear();
    let items = click(&mut client, 1, 27, 4);
    let slots = parse_window_slots(&items);
    assert_eq!(slots[0], (4, 10), "first half holds the cobble");
    assert_eq!(slots[27], (1, 7), "second half holds the stone");
    close_window(&mut client, 1);
    drop(client);

    // Reconnect and open via the EAST half: same order, same counts.
    let (mut client2, _) = join(port, "Keeper035");
    use_block(&mut client2, gx + 1, 64, gz, None);
    client2
        .until(Duration::from_secs(5), |id, payload| {
            id == 0x2d && payload.first() == Some(&1)
        })
        .expect("double chest must reopen after reconnect");
    let items = client2
        .until(Duration::from_secs(5), |id, payload| {
            id == 0x30 && payload.first() == Some(&1)
        })
        .expect("reopened window items");
    let slots = parse_window_slots(&items);
    assert_eq!(slots.len(), 90);
    assert_eq!(slots[0], (4, 10), "order is stable across reconnect");
    assert_eq!(slots[27], (1, 7));
    close_window(&mut client2, 1);

    close_window(&mut client2, 1);
    drop(client2);

    // Each half persists its normalized share independently: after a
    // clean stop, the chest store must key the cobble under the west
    // half's coordinates and the stone under the east half's.
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
    let store: Value =
        serde_json::from_slice(&std::fs::read(dir.join("flat.chests.json")).unwrap()).unwrap();
    let west = &store["entries"][&format!("{gx},64,{gz}")];
    let east = &store["entries"][&format!("{},64,{gz}", gx + 1)];
    assert_eq!(west[0]["id"].as_i64(), Some(4));
    assert_eq!(west[0]["count"].as_i64(), Some(10));
    assert_eq!(east[0]["id"].as_i64(), Some(1));
    assert_eq!(east[0]["count"].as_i64(), Some(7));
    let _ = std::fs::remove_dir_all(&dir);
}
