//! ADD-017 / #441: when a Kick races inventory operations, the saved
//! state must equal exactly the operations confirmed before the kick
//! took effect — the in-flight click is either fully applied or fully
//! absent, never half-saved, duplicated, or lost. Operations are
//! persisted atomically per click on the session's single event loop;
//! this test pins that ordering end to end.
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
    let dir = std::env::temp_dir().join(format!("ir1710-add017-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("whitelist.json"), r#"[{"name":"Keeper017"}]"#).unwrap();
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
fn add017_kick_racing_inventory_clicks_saves_a_consistent_state() {
    let (mut server, dir, port) = spawn_server("kick");
    wait_port(port);
    let (mut client, _spawn) = join(port, "Keeper017");

    // 10 cobblestone in hotbar slot 37 (window 0 numbering).
    creative_set(&mut client, 37, 4, 10);

    // Confirmed sequence: pick the stack up (action 1), place it into
    // main-inventory slot 10 (action 2), pick it up again (action 3).
    // After action 3 the cursor holds the stack and slot 10 is empty.
    click(&mut client, 0, 37, 1);
    let items = click(&mut client, 0, 10, 2);
    // Window 0 Items: window u8, count i16 (45 slots), slot data from
    // offset 3; slot 10 begins at 3 + 10*7 when every slot is 7 bytes
    // only for full stacks — instead verify via the saved file below
    // and the final click result. Here we only require the packet.
    let _ = items;
    click(&mut client, 0, 10, 3);

    // The racing click: place the cursor stack into slot 11, and kick
    // the player from the console at the same moment, without waiting
    // for the click's confirmation.
    let mut p = vec![0u8];
    p.extend_from_slice(&11i16.to_be_bytes());
    p.push(0);
    p.extend_from_slice(&4i16.to_be_bytes());
    p.push(0);
    p.extend_from_slice(&(-1i16).to_be_bytes());
    client.send(&frame(0x0e, &p));
    {
        let stdin = server.stdin.as_mut().unwrap();
        stdin.write_all(b"/kick Keeper017\n").unwrap();
        stdin.flush().unwrap();
    }

    // The kick must actually disconnect the session.
    let kicked = client.until(Duration::from_secs(10), |id, _| id == 0x40);
    assert!(kicked.is_some(), "kick must send a Disconnect packet");
    drop(client);

    // Saved state: exactly one of the two legal outcomes, total 10.
    let inventory_path = dir.join("players/keeper017.inventory.json");
    let deadline = Instant::now() + Duration::from_secs(15);
    let saved = loop {
        if let Some(value) = read_json(&inventory_path) {
            let total = count_in_slots(&value["slots"], 4)
                + if value["cursor"]["id"].as_i64() == Some(4) {
                    value["cursor"]["count"].as_i64().unwrap_or(0)
                } else {
                    0
                };
            if total == 10 {
                break value;
            }
        }
        assert!(
            Instant::now() < deadline,
            "saved inventory never reached a consistent total of 10"
        );
        std::thread::sleep(Duration::from_millis(200));
    };
    let in_slot11 = saved["slots"][11]["id"].as_i64() == Some(4)
        && saved["slots"][11]["count"].as_i64() == Some(10);
    let on_cursor =
        saved["cursor"]["id"].as_i64() == Some(4) && saved["cursor"]["count"].as_i64() == Some(10);
    assert!(
        in_slot11 ^ on_cursor,
        "exactly one legal outcome must hold (click applied XOR cursor kept):          slot11={in_slot11} cursor={on_cursor}"
    );
    assert_eq!(
        count_in_slots(&saved["slots"], 4),
        if in_slot11 { 10 } else { 0 }
    );
    // No other slot may hold cobblestone (no duplication anywhere).
    let total_slots = count_in_slots(&saved["slots"], 4);
    let total_cursor = if saved["cursor"]["id"].as_i64() == Some(4) {
        saved["cursor"]["count"].as_i64().unwrap_or(0)
    } else {
        0
    };
    assert_eq!(total_slots + total_cursor, 10);

    // Reconnect: the same consistent state is what the player resumes
    // with — still exactly 10 in exactly one place.
    let (mut client2, _) = join(port, "Keeper017");
    client2.collect(Duration::from_millis(800));
    let after = read_json(&inventory_path).expect("inventory must exist after rejoin");
    let total_after = count_in_slots(&after["slots"], 4)
        + if after["cursor"]["id"].as_i64() == Some(4) {
            after["cursor"]["count"].as_i64().unwrap_or(0)
        } else {
            0
        };
    assert_eq!(total_after, 10);
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
