//! ADD-022 / #446: extreme world coordinates injected through movement,
//! digging and placement packets must not panic the server, allocate
//! absurd chunks, or corrupt the session. The defenses exist (finite /
//! world-limit validation with position correction, `in_bounds` + `near`
//! gates on edits); this test pins them end to end against the real
//! server binary.
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

struct Client {
    stream: TcpStream,
    buffer: Vec<u8>,
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
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).unwrap();
    }

    /// Non-blocking drain of every complete frame currently available.
    fn poll(&mut self) -> Vec<(i32, Vec<u8>)> {
        let mut chunk = [0u8; 65536];
        match self.stream.read(&mut chunk) {
            Ok(n) if n > 0 => self.buffer.extend_from_slice(&chunk[..n]),
            _ => {}
        }
        let mut frames = Vec::new();
        loop {
            match try_parse_frame(&self.buffer) {
                Some((id, payload, used)) => {
                    self.buffer.drain(..used);
                    frames.push((id, payload));
                }
                None => return frames,
            }
        }
    }

    fn collect(&mut self, dur: Duration) -> Vec<(i32, Vec<u8>)> {
        let deadline = Instant::now() + dur;
        let mut all = Vec::new();
        while Instant::now() < deadline {
            all.extend(self.poll());
            std::thread::sleep(Duration::from_millis(30));
        }
        all
    }
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

fn be_f64(data: &[u8], at: usize) -> f64 {
    f64::from_be_bytes(data[at..at + 8].try_into().unwrap())
}

fn spawn_server(tag: &str) -> (Child, std::path::PathBuf, u16) {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let dir = std::env::temp_dir().join(format!("ir1710-add022-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("whitelist.json"), r#"[{"name":"Probe022"}]"#).unwrap();
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

fn file_set(dir: &std::path::Path) -> Vec<String> {
    let mut names = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                names.push(
                    path.strip_prefix(dir)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    names.sort();
    names
}

#[test]
fn add022_extreme_coordinates_are_rejected_without_panic_or_allocation() {
    let (mut server, dir, port) = spawn_server("inject");
    wait_port(port);
    let mut client = login(port, "Probe022");
    // Join burst: capture the server-assigned spawn from Position & Look.
    let burst = client.collect(Duration::from_secs(3));
    let spawn = burst
        .iter()
        .find(|(id, _)| *id == 0x08)
        .map(|(_, payload)| (be_f64(payload, 0), be_f64(payload, 8), be_f64(payload, 16)))
        .expect("join burst must include a Position & Look packet");
    assert!(
        !burst.iter().any(|(id, _)| *id == 0x40),
        "clean login must not disconnect"
    );
    // Let the first periodic saves land (player .dat, mobs sidecar) so the
    // snapshot below contains every ordinary runtime artifact; anything
    // beyond this set afterwards would be caused by the injections.
    client.collect(Duration::from_millis(5500));
    let files_before = file_set(&dir);
    assert!(!files_before.is_empty());

    // 1) Movement with finite but extreme coordinates (i32 range as f64).
    let mut move_payload = Vec::new();
    move_payload.extend_from_slice(&2147483647.0f64.to_be_bytes()); // x
    move_payload.extend_from_slice(&70.0f64.to_be_bytes()); // feet y (in range)
    move_payload.extend_from_slice(&71.62f64.to_be_bytes()); // stance
    move_payload.extend_from_slice(&(-2147483648.0f64).to_be_bytes()); // z
    move_payload.push(1);
    client.send(&frame(0x04, &move_payload));
    let after_move = client.collect(Duration::from_millis(1500));
    assert!(
        !after_move.iter().any(|(id, _)| *id == 0x40),
        "extreme finite movement must not disconnect the session"
    );
    // The server must correct the client back to its held position.
    let corrected = after_move
        .iter()
        .filter(|(id, _)| *id == 0x08)
        .next_back()
        .map(|(_, payload)| (be_f64(payload, 0), be_f64(payload, 8), be_f64(payload, 16)));
    if let Some(pos) = corrected {
        assert!(
            (pos.0 - spawn.0).abs() < 0.01
                && (pos.1 - spawn.1).abs() < 0.01
                && (pos.2 - spawn.2).abs() < 0.01,
            "correction must restore the held spawn position, got {pos:?} vs spawn {spawn:?}"
        );
        // Vanilla-style acknowledgement of the correction.
        let mut ack = Vec::new();
        ack.extend_from_slice(&pos.0.to_be_bytes());
        ack.extend_from_slice(&pos.1.to_be_bytes());
        ack.extend_from_slice(&(pos.1 + 1.62).to_be_bytes());
        ack.extend_from_slice(&pos.2.to_be_bytes());
        ack.push(1);
        client.send(&frame(0x04, &ack));
        client.collect(Duration::from_millis(300));
    }

    // 2) Digging at extreme block coordinates (start + finish).
    for status in [0u8, 2] {
        let mut dig = vec![status];
        dig.extend_from_slice(&i32::MAX.to_be_bytes());
        dig.push(64);
        dig.extend_from_slice(&i32::MIN.to_be_bytes());
        dig.push(1);
        client.send(&frame(0x07, &dig));
    }
    // 3) Placement at extreme block coordinates with a stone item.
    let mut place = Vec::new();
    place.extend_from_slice(&i32::MIN.to_be_bytes());
    place.push(64);
    place.extend_from_slice(&i32::MAX.to_be_bytes());
    place.push(1); // direction: top face
    place.extend_from_slice(&1i16.to_be_bytes()); // item id 1 (stone)
    place.push(1); // count
    place.extend_from_slice(&0i16.to_be_bytes()); // damage
    place.extend_from_slice(&[8, 8, 8]); // cursor
    client.send(&frame(0x08, &place));
    let after_edits = client.collect(Duration::from_millis(1500));
    assert!(
        !after_edits.iter().any(|(id, _)| *id == 0x40),
        "extreme dig/place must not disconnect the session"
    );
    assert!(
        after_edits.iter().any(|(id, _)| *id == 0x03),
        "Time Update must keep flowing after the injections"
    );
    assert!(
        server.try_wait().unwrap().is_none(),
        "server process must still be alive"
    );

    // 4) The session is still functional at the real position: dig the
    // ground block under the spawn point and expect a Block Change.
    let gx = spawn.0.floor() as i32;
    let gz = spawn.2.floor() as i32;
    for status in [0u8, 2] {
        let mut dig = vec![status];
        dig.extend_from_slice(&gx.to_be_bytes());
        dig.push(63);
        dig.extend_from_slice(&gz.to_be_bytes());
        dig.push(1);
        client.send(&frame(0x07, &dig));
    }
    let after_dig = client.collect(Duration::from_millis(1500));
    let dug = after_dig.iter().any(|(id, payload)| {
        *id == 0x23
            && payload.len() >= 11
            && i32::from_be_bytes(payload[0..4].try_into().unwrap()) == gx
            && payload[4] == 63
            && i32::from_be_bytes(payload[5..9].try_into().unwrap()) == gz
    });
    assert!(
        dug,
        "a legitimate dig at the spawn must still produce a Block Change"
    );

    // 5) No absurd allocation: no coordinate-encoded world files may
    // appear. A few world-named sidecars are written on slow periodic
    // cadences (e.g. flat.metrics.jsonl) and are allowed; anything else
    // new — above all region/chunk files named after the injected
    // coordinates — fails the test.
    let new_files: Vec<String> = file_set(&dir)
        .into_iter()
        .filter(|name| !files_before.contains(name))
        .collect();
    assert!(
        new_files.iter().all(|name| name.starts_with("flat.")),
        "only world-named periodic sidecars may appear late, got: {new_files:?}"
    );

    // Clean shutdown still works.
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
