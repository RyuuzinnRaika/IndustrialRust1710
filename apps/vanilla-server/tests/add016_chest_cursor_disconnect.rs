//! ADD-016 / #440: a player who disconnects with a chest window still
//! open and items on the cursor must neither lose nor duplicate items.
//! The cursor lives in the saved inventory and chest contents are
//! committed at click time; this test pins the totals across an abrupt
//! disconnect (no Close Window packet) and a reconnect.
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
    let dir = std::env::temp_dir().join(format!("ir1710-add016-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("whitelist.json"), r#"[{"name":"Keeper016"}]"#).unwrap();
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

/// Sum of `count` over every stack with the given id in a JSON slot array.
fn count_in_slots(slots: &Value, id: i64) -> i64 {
    slots
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter(|s| s["id"].as_i64() == Some(id))
                .map(|s| s["count"].as_i64().unwrap_or(0))
                .sum()
        })
        .unwrap_or(0)
}

fn read_json(path: &std::path::Path) -> Option<Value> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

#[test]
fn add016_cursor_items_survive_disconnect_with_chest_open() {
    let (mut server, dir, port) = spawn_server("cursor");
    wait_port(port);
    let (mut client, spawn) = join(port, "Keeper016");
    let gx = spawn.0.floor() as i32;
    let gz = spawn.2.floor() as i32;

    // Place a chest on the ground next to the spawn point.
    creative_set(&mut client, 36, 54, 1);
    use_block(&mut client, gx, 63, gz, Some((54, 1)));
    client
        .until(Duration::from_secs(5), |id, payload| {
            id == 0x23 && payload.len() >= 11 && payload[4] == 64 && payload[9] == 54
        })
        .expect("chest placement must produce a Block Change");

    // Give the player 10 cobblestone in hotbar slot 37.
    creative_set(&mut client, 37, 4, 10);

    // Open the chest (window 1, 27 chest slots + 36 player slots).
    use_block(&mut client, gx, 64, gz, None);
    client
        .until(Duration::from_secs(5), |id, payload| {
            id == 0x2d && payload.first() == Some(&1)
        })
        .expect("chest must open window 1");
    client
        .until(Duration::from_secs(5), |id, payload| {
            id == 0x30 && payload.first() == Some(&1)
        })
        .expect("chest window items must arrive");

    // Window slot 55 is player hotbar slot 37 in the chest window
    // layout. A normal click picks the stack up onto the cursor first;
    // a second click on chest slot 0 deposits it there.
    click(&mut client, 1, 55, 1);
    let items = click(&mut client, 1, 0, 2);
    // Window Items layout: window u8, count i16, then slots; slot 0 is
    // the first chest slot and must now hold id 4 x10.
    assert_eq!(
        &items[3..8],
        &[0, 4, 10, 0, 0],
        "chest slot 0 must hold the deposited stack"
    );

    // Pick the stack back up onto the cursor and leave it there.
    let items = click(&mut client, 1, 0, 3);
    assert_eq!(
        &items[3..5],
        &[255, 255],
        "chest slot 0 must be empty after the pickup"
    );
    let cursor = client
        .until(Duration::from_secs(5), |id, payload| {
            id == 0x2f
                && payload.len() >= 8
                && payload.starts_with(&[255, 255, 255])
                && payload[3..5] == [0, 4]
        })
        .expect("cursor update with the picked-up stack must arrive");
    assert_eq!(
        &cursor[3..8],
        &[0, 4, 10, 0, 0],
        "cursor must hold the picked-up stack of 10"
    );

    // Abrupt disconnect: drop the socket with the window still open.
    drop(client);

    // The server must persist the inventory (incl. cursor) on disconnect.
    let inventory_path = dir.join("players/keeper016.inventory.json");
    let deadline = Instant::now() + Duration::from_secs(15);
    let saved = loop {
        if let Some(value) = read_json(&inventory_path) {
            if value["cursor"]["id"].as_i64() == Some(4) {
                break value;
            }
        }
        assert!(
            Instant::now() < deadline,
            "inventory with the cursor stack was not saved after disconnect"
        );
        std::thread::sleep(Duration::from_millis(200));
    };
    assert_eq!(saved["cursor"]["count"].as_i64(), Some(10));
    // Totals at the disconnect checkpoint: cursor 10, player slots 0,
    // chest 0 — nothing lost, nothing duplicated.
    assert_eq!(count_in_slots(&saved["slots"], 4), 0);
    let chest_path = dir.join("flat.chests.json");
    let chest_total = read_json(&chest_path)
        .map(|value| {
            value["entries"]
                .as_object()
                .map(|entries| {
                    entries
                        .values()
                        .map(|slots| count_in_slots(slots, 4))
                        .sum::<i64>()
                })
                .unwrap_or(0)
        })
        .unwrap_or(0);
    assert_eq!(chest_total, 0, "chest must not retain a copy");

    // Reconnect: the same totals must still hold on disk and in game.
    let (mut client2, _) = join(port, "Keeper016");
    let after = read_json(&inventory_path).expect("inventory must exist after rejoin");
    assert_eq!(after["cursor"]["id"].as_i64(), Some(4));
    assert_eq!(after["cursor"]["count"].as_i64(), Some(10));
    assert_eq!(count_in_slots(&after["slots"], 4), 0);
    // The chest really is empty for the reconnected player too.
    use_block(&mut client2, gx, 64, gz, None);
    let items = client2
        .until(Duration::from_secs(5), |id, payload| {
            id == 0x30 && payload.first() == Some(&1)
        })
        .expect("reopened chest must send Window Items");
    assert_eq!(
        &items[3..5],
        &[255, 255],
        "reopened chest slot 0 must still be empty"
    );
    drop(client2);

    // Clean shutdown.
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
