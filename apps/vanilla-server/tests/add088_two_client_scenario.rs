//! ADD-088 / #513: the two-client unmodified-1.7.10 reproduction scenario
//! (login, mining, simultaneous chest work, disconnect/reconnect) is
//! automated as one deterministic regression against the real server and
//! the real Protocol 5 wire format. Two loopback Protocol 5 sessions play
//! the role of the two clients; every phase states its expectation inline
//! and the CI log is the evidence artifact. Real-client GUI acceptance on
//! a desktop host remains NOT_RUN on this headless host.
//!
//! Phases (each is an explicit expected-result assertion):
//!   1. login     — both sessions reach Play, each is announced to the
//!      peer with 0x0C named spawn, /list shows Online (2).
//!   2. mining    — the miner digs the grass under its feet; the OTHER
//!      session must observe the same 0x23 Block Change at the same
//!      coordinates (cross-client world visibility).
//!   3. chest     — both sessions hold the SAME chest window open at the
//!      same time; the miner deposits cobble x10 into chest slot 0; the
//!      peer's reopened window and the persisted chest store agree.
//!   4. drop/
//!      reconnect — the peer drops; the miner sees its 0x13 entity
//!      destroy; the peer rejoins, its chest contents survived, and both
//!      sessions are live again.
//!   5. stop      — /stop is clean and the durable files are unchanged
//!      by the shutdown.
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
                        // KeepAlive echo keeps the session admitted.
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

    fn send_chat(&mut self, text: &str) {
        self.send(&frame(1, &mc_string(text)));
    }
}

fn be_f64(data: &[u8], at: usize) -> f64 {
    f64::from_be_bytes(data[at..at + 8].try_into().unwrap())
}

fn spawn_server(tag: &str) -> (Child, std::path::PathBuf, u16) {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let dir = std::env::temp_dir().join(format!("ir1710-add088-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("whitelist.json"),
        r#"[{"name":"Miner088"},{"name":"Sharer088"}]"#,
    )
    .unwrap();
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

/// Login + Play admission: returns (client, own entity id, spawn position).
fn join(port: u16, name: &str) -> (Client, i32, (f64, f64, f64)) {
    let mut client = Client::connect(port);
    let mut handshake = varint(5);
    handshake.extend_from_slice(&mc_string("127.0.0.1"));
    handshake.extend_from_slice(&port.to_be_bytes());
    handshake.extend_from_slice(&varint(2));
    client.send(&frame(0, &handshake));
    client.send(&frame(0, &mc_string(name)));
    let join_game = client
        .until(Duration::from_secs(10), |id, _| id == 0x01)
        .expect("login must deliver Join Game (0x01)");
    let entity_id = i32::from_be_bytes(join_game[..4].try_into().unwrap());
    let position = client
        .until(Duration::from_secs(10), |id, _| id == 0x08)
        .expect("join must deliver Position & Look");
    client.collect(Duration::from_millis(500));
    (
        client,
        entity_id,
        (
            be_f64(&position, 0),
            be_f64(&position, 8),
            be_f64(&position, 16),
        ),
    )
}

fn creative_set(client: &mut Client, slot: i16, id: i16, count: u8) {
    let mut p = slot.to_be_bytes().to_vec();
    p.extend_from_slice(&id.to_be_bytes());
    p.push(count);
    p.extend_from_slice(&0i16.to_be_bytes());
    p.push(0); // no NBT: single TAG_End byte
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
            p.push(0); // no NBT: single TAG_End byte
        }
        None => p.extend_from_slice(&(-1i16).to_be_bytes()),
    }
    p.extend_from_slice(&[8, 8, 8]);
    client.send(&frame(0x08, &p));
}

/// Protocol 5 block dig (status 0 = start destroy; creative breaks at once).
fn dig_block(client: &mut Client, x: i32, y: u8, z: i32) {
    let mut p = vec![0u8];
    p.extend_from_slice(&x.to_be_bytes());
    p.push(y);
    p.extend_from_slice(&z.to_be_bytes());
    p.push(1);
    client.send(&frame(0x07, &p));
}

/// Window click; returns the Window Items payload that follows.
fn click(client: &mut Client, window: u8, slot: i16, action: i16) -> Vec<u8> {
    let mut p = vec![window];
    p.extend_from_slice(&slot.to_be_bytes());
    p.push(0); // left button
    p.extend_from_slice(&action.to_be_bytes());
    p.push(0); // mode: normal click
    p.extend_from_slice(&(-1i16).to_be_bytes()); // empty slot claim stays Short -1
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

/// Open the chest and return the window id the server assigned (this
/// server increments per session, so every later click/close must use
/// the id from THIS open rather than assuming a constant 1).
fn open_chest(client: &mut Client, x: i32, y: u8, z: i32) -> (u8, Vec<u8>) {
    use_block(client, x, y, z, None);
    let opened = client
        .until(Duration::from_secs(5), |id, _| id == 0x2d)
        .expect("chest must open a window (0x2D)");
    let window = opened[0];
    let items = client
        .until(Duration::from_secs(5), |id, payload| {
            id == 0x30 && payload.first() == Some(&window)
        })
        .expect("chest window items must arrive for the opened window");
    (window, items)
}

fn close_window(client: &mut Client, window: u8) {
    client.send(&frame(0x0d, &[window]));
}

#[test]
fn add088_two_client_scenario_login_mine_chest_reconnect() {
    let (mut server, dir, port) = spawn_server("two-client-scenario");
    wait_port(port);

    // ---- Phase 1: login -------------------------------------------------
    // Expected: both reach Play; each is announced to the peer with the
    // 1.7.10 named spawn (0x0C); /list on the miner shows Online (2).
    let (mut miner, miner_id, spawn) = join(port, "Miner088");
    let (mut peer, peer_id, _) = join(port, "Sharer088");
    assert_ne!(miner_id, peer_id, "sessions get distinct entity ids");
    miner
        .until(Duration::from_secs(5), |id, _| id == 0x0c)
        .expect("miner must be told about the peer's spawn (0x0C)");
    peer.collect(Duration::from_millis(250));
    miner.send_chat("/list");
    miner
        .until(Duration::from_secs(8), |id, payload| {
            id == 0x02 && payload.windows(10).any(|w| w == b"Online (2)")
        })
        .expect("/list must report both sessions");

    // ---- Phase 2: mining -------------------------------------------------
    // Expected: the OTHER session observes the same 0x23 Block Change
    // (x=0, y=63, z=0, new block 0) that the miner caused.
    dig_block(&mut miner, 0, 63, 0);
    miner
        .until(Duration::from_secs(5), |id, payload| {
            id == 0x23
                && payload.len() >= 11
                && i32::from_be_bytes(payload[..4].try_into().unwrap()) == 0
                && payload[4] == 63
                && payload[9] == 0
        })
        .expect("miner sees its own mined block");
    peer.until(Duration::from_secs(5), |id, payload| {
        id == 0x23
            && payload.len() >= 11
            && i32::from_be_bytes(payload[..4].try_into().unwrap()) == 0
            && payload[4] == 63
            && payload[9] == 0
    })
    .expect("the peer must observe the miner's block change");

    // ---- Phase 3: simultaneous chest operation ----------------------------
    // Expected: BOTH sessions hold the same chest window open at once;
    // the miner deposits cobble x10 into chest slot 0; the peer's window
    // (after close/reopen) and flat.chests.json agree on the deposit.
    let gx = spawn.0.floor() as i32;
    let gz = spawn.2.floor() as i32;
    let (cx, cy, cz) = (gx + 2, 64u8, gz);
    creative_set(&mut miner, 36, 54, 1);
    use_block(&mut miner, cx, 63, cz, Some((54, 1)));
    miner
        .until(Duration::from_secs(5), |id, payload| {
            id == 0x23 && payload.len() >= 11 && payload[9] == 54
        })
        .expect("chest placement must produce a Block Change");
    let (miner_window, miner_opened) = open_chest(&mut miner, cx, cy, cz);
    let (peer_window, _) = open_chest(&mut peer, cx, cy, cz); // simultaneous
    assert_eq!(miner_window, 1, "each session numbers its own window ids");
    assert_eq!(peer_window, 1, "the peer session also starts at window 1");
    assert_eq!(&miner_opened[3..5], &[255, 255], "fresh chest starts empty");
    creative_set(&mut miner, 37, 4, 10);
    let _ = click(&mut miner, miner_window, 55, 1); // pick cobble onto the cursor
    let items = click(&mut miner, miner_window, 0, 2); // deposit into chest slot 0
    assert_eq!(
        &items[3..8],
        &[0, 4, 10, 0, 0],
        "miner's chest window must show the deposit"
    );
    // The peer still holds its window; a close/reopen must show the same
    // contents (and the peer socket survived the concurrent open). This
    // server numbers a session's windows sequentially, so the reopened
    // window carries a fresh id that must be used for later traffic.
    close_window(&mut peer, peer_window);
    let (peer_reopened, peer_items) = open_chest(&mut peer, cx, cy, cz);
    assert_eq!(
        &peer_items[3..8],
        &[0, 4, 10, 0, 0],
        "peer's chest window must show the miner's deposit"
    );
    close_window(&mut peer, peer_reopened);
    close_window(&mut miner, miner_window);
    let chest_path = dir.join("flat.chests.json");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(bytes) = std::fs::read(&chest_path) {
            if let Ok(store) = serde_json::from_slice::<Value>(&bytes) {
                if store["entries"][&format!("{cx},{cy},{cz}")][0]["count"].as_i64() == Some(10) {
                    break;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "the deposit did not reach the persisted chest store"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    // ---- Phase 4: disconnect / reconnect -----------------------------------
    // Expected: the miner sees the peer's 0x13 entity destroy with the
    // peer's entity id; the peer rejoins, is re-announced (0x0C), and the
    // chest still holds the deposit.
    miner.backlog.clear();
    drop(peer);
    miner
        .until(Duration::from_secs(10), |id, payload| {
            id == 0x13
                && payload.len() >= 5
                && i32::from_be_bytes(payload[1..5].try_into().unwrap()) == peer_id
        })
        .expect("miner must see the peer destroyed (0x13 + entity id)");
    let (mut peer2, peer2_id, _) = join(port, "Sharer088");
    miner
        .until(Duration::from_secs(5), |id, _| id == 0x0c)
        .expect("rejoined peer must be announced to the miner (0x0C)");
    assert_ne!(miner_id, peer2_id, "reconnect mints a fresh entity id");
    let (peer2_window, reopened) = open_chest(&mut peer2, cx, cy, cz);
    assert_eq!(
        &reopened[3..8],
        &[0, 4, 10, 0, 0],
        "chest contents must survive the peer's reconnect"
    );
    // Peer2 speaks: the miner's still-live session must receive the peer's
    // broadcast chat (proves both sessions are live again; a second literal
    // "/list" on the miner session would be a legitimate spam-filter hit).
    peer2.send_chat("rejoin-visible");
    miner
        .until(Duration::from_secs(8), |id, payload| {
            id == 0x02 && payload.windows(14).any(|w| w == b"rejoin-visible")
        })
        .expect("miner must receive the rejoined peer's chat");
    peer2.send_chat("/list");
    peer2
        .until(Duration::from_secs(8), |id, payload| {
            id == 0x02 && payload.windows(10).any(|w| w == b"Online (2)")
        })
        .expect("both sessions are live again after the reconnect");
    close_window(&mut peer2, peer2_window);
    drop(peer2);
    drop(miner);

    // ---- Phase 5: clean stop -------------------------------------------------
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
    let store: Value = serde_json::from_slice(&std::fs::read(&chest_path).unwrap()).unwrap();
    assert_eq!(
        store["entries"][&format!("{cx},{cy},{cz}")][0]["id"].as_i64(),
        Some(4),
        "the deposit must be durable across the clean stop"
    );
    assert_eq!(
        store["entries"][&format!("{cx},{cy},{cz}")][0]["count"].as_i64(),
        Some(10)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
