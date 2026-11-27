//! #1318 [Audit][P1]: vanilla spawn-protection square enforcement.
//!
//! Pinned semantics (see src/spawn_protection.rs for the full honesty note):
//! a `spawn-protection` radius protects the closed square (Chebyshev
//! distance <= R) around the single global world spawn; non-operators get
//! the authoritative block back as a 0x23 single-block change with no world
//! mutation and no disconnect; an empty operators list automatically
//! disables protection; R = 0 disables protection.
//!
//! Verification status: the exact NMS 1.7.10 enforcement call-site and
//! per-dimension applicability were NOT verified from decompiled primary
//! source; these tests pin the documented minecraft.wiki semantics chosen
//! for this repository. An end-to-end "OP breaks inside the square" case is
//! NOT_RUN here by design: this server grants operator authority only to a
//! Mojang-session-verified UUID (online mode), and the online login path
//! fail-closes without a session server (see add073), so no raw TCP client
//! in these tests can be an operator. The OP bypass, the name-claim refusal
//! and the single-spawn-across-dimensions choice are pinned at unit level
//! in src/spawn_protection.rs instead.
//!
//! Real 1.7.10 client verification: NOT_RUN.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

// Server spawns reserve ephemeral ports; serialize reservation + boot to
// avoid cross-test port races inside this binary.
static SERVER_TEST_LOCK: Mutex<()> = Mutex::new(());

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

    /// Wait for a packet matching `pred`, consuming it from the backlog.
    fn until(&mut self, timeout: Duration, mut pred: impl FnMut(i32, &[u8]) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(pos) = self
                .backlog
                .iter()
                .position(|(id, payload)| pred(*id, payload))
            {
                self.backlog.remove(pos);
                return true;
            }
            if Instant::now() >= deadline {
                return false;
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

    /// Discard everything received so far (used after join bursts).
    fn drain(&mut self) {
        self.backlog.clear();
    }

    /// True (without consuming) if a 0x23 for exactly this position with
    /// this block id was seen.
    fn saw_block_change(&self, x: i32, y: u8, z: i32, block: u8) -> bool {
        self.backlog
            .iter()
            .any(|(id, body)| is_block_change(*id, body, x, y, z) && body[9] == block)
    }
}

/// An S->C 0x23 single-block change: Int X, Byte Y, Int Z, VarInt block,
/// Byte metadata.
fn is_block_change(id: i32, body: &[u8], x: i32, y: u8, z: i32) -> bool {
    if id != 0x23 || body.len() < 11 {
        return false;
    }
    i32::from_be_bytes(body[..4].try_into().unwrap()) == x
        && body[4] == y
        && i32::from_be_bytes(body[5..9].try_into().unwrap()) == z
}

/// C->S 0x07 player digging: Byte status, Int X, Byte Y, Int Z, Byte face.
fn dig(status: u8, x: i32, y: u8, z: i32, face: i8) -> Vec<u8> {
    let mut p = vec![status];
    p.extend_from_slice(&x.to_be_bytes());
    p.push(y);
    p.extend_from_slice(&z.to_be_bytes());
    p.push(face as u8);
    frame(7, &p)
}

/// C->S 0x08 block placement with a cobblestone (id 4) slot snapshot,
/// byte-for-byte the shape used by the protocol5_e2e suite (damage 0,
/// NBT absent as Short -1; a negative damage fails the server's slot
/// validation and drops the connection).
fn place_cobble(x: i32, y: u8, z: i32, face: i8) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&x.to_be_bytes());
    p.push(y);
    p.extend_from_slice(&z.to_be_bytes());
    p.push(face as u8);
    p.extend_from_slice(&4i16.to_be_bytes()); // item id (slot Short)
    p.push(1); // count
    p.extend_from_slice(&0i16.to_be_bytes()); // damage
    p.extend_from_slice(&(-1i16).to_be_bytes()); // no NBT
    p.extend_from_slice(&[8, 8, 8]); // cursor
    frame(8, &p)
}

/// C->S 0x10 creative inventory give: hotbar slot 36 = cobblestone x64.
fn creative_give_cobble() -> Vec<u8> {
    let mut p = 36i16.to_be_bytes().to_vec();
    p.extend_from_slice(&[0, 4, 64, 0, 0, 255, 255]);
    frame(0x10, &p)
}

/// A booted test server. Drop ALWAYS reaps the child: an earlier version
/// of this file leaked the still-running server on assertion failure, and
/// the leaked process kept the cargo test pipeline's inherited write
/// handle open on Windows, hanging the whole test run indefinitely.
struct Server {
    child: Child,
    dir: PathBuf,
}

impl Server {
    /// Prefer the graceful console `/stop` (bounded, polled); fall back to
    /// the hard kill the other e2e harnesses use if the child misbehaves.
    fn stop(mut self) {
        if let Some(stdin) = self.child.stdin.as_mut() {
            let _ = stdin.write_all(b"/stop\n");
            let _ = stdin.flush();
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = self.child.kill();
                        let _ = self.child.wait();
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Boot with `spawn_protection` and the given operators list; returns the
/// handle and the listening port.
fn spawn_server(tag: &str, spawn_protection: &str, ops_json: &str) -> (Server, u16) {
    let _guard = SERVER_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let dir = std::env::temp_dir().join(format!(
        "ir1710-audit1318-{tag}-{}-{}",
        std::process::id(),
        port
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("whitelist.json"), r#"[{"name":"RustTester"}]"#).unwrap();
    // Seed the operators list BEFORE boot: Administration::new only creates
    // ops.json when missing, so the seeded content always survives.
    std::fs::write(dir.join("ops.json"), ops_json).unwrap();
    // Absolute --world keeps ops.json and the world spawn sidecar inside
    // this test's private directory (see administration_root in main()).
    let stderr = std::fs::File::create(dir.join("server-stderr.log")).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
        .args(["--bind", &format!("127.0.0.1:{port}"), "--world"])
        .arg(dir.join("flat.irw"))
        .arg("--whitelist")
        .arg(dir.join("whitelist.json"))
        .arg("--spawn-protection")
        .arg(spawn_protection)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr))
        .spawn()
        .unwrap();
    (Server { child, dir }, port)
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

/// Handshake + offline login + join; returns the client and the reported
/// spawn position. The flat world spawn column is (0, 0).
fn join(port: u16, name: &str) -> (Client, (f64, f64, f64)) {
    let mut client = Client::connect(port);
    let mut handshake = varint(5);
    handshake.extend_from_slice(&mc_string("127.0.0.1"));
    handshake.extend_from_slice(&port.to_be_bytes());
    handshake.extend_from_slice(&varint(2));
    client.send(&frame(0, &handshake));
    client.send(&frame(0, &mc_string(name)));
    let pos_deadline = Instant::now() + Duration::from_secs(10);
    let pos = loop {
        client.poll();
        if let Some(pos) = client
            .backlog
            .iter()
            .find(|(id, body)| *id == 0x08 && body.len() >= 24)
            .map(|(_, body)| {
                (
                    f64::from_be_bytes(body[0..8].try_into().unwrap()),
                    f64::from_be_bytes(body[8..16].try_into().unwrap()),
                    f64::from_be_bytes(body[16..24].try_into().unwrap()),
                )
            })
        {
            break pos;
        }
        assert!(
            Instant::now() < pos_deadline,
            "join must deliver Position & Look (0x08)"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    client.collect(Duration::from_millis(400));
    client.drain();
    (client, pos)
}

/// One configured (but offline-unknown) operator entry is enough to flip
/// spawn protection from "auto-disabled" to active.
const SEEDED_OPS: &str =
    r#"[{"name":"OpGuy","uuid":"069a79f4-44e9-4726-a5be-fca90e38aaf5","level":4}]"#;

#[test]
fn audit1318_non_op_is_refused_inside_the_square_with_authoritative_resync() {
    // R=3 protects the closed square x,z in [-3, 3] around spawn (0,0).
    let (server, port) = spawn_server("active", "3", SEEDED_OPS);
    wait_port(port);
    let (mut client, spawn) = join(port, "RustTester");
    assert_eq!(
        (spawn.0.floor() as i32, spawn.2.floor() as i32),
        (0, 0),
        "the flat test world must spawn inside the protected square"
    );

    // Center break: creative dig normally completes on the START packet.
    // Protection must instead answer with the authoritative grass block
    // (0x23, block id 2) and leave the world untouched.
    client.send(&dig(0, 0, 63, 0, 1));
    assert!(
        client.until(Duration::from_secs(5), |id, body| {
            is_block_change(id, body, 0, 63, 0) && body[9] == 2
        }),
        "protected dig at the spawn center must be refused with a 0x23 resync"
    );
    client.collect(Duration::from_millis(750));
    assert!(
        !client.saw_block_change(0, 63, 0, 0),
        "the protected center block must never turn to air"
    );

    // Corner (R, R) is inside the closed square: also refused.
    client.send(&dig(0, 3, 63, 3, 1));
    assert!(
        client.until(Duration::from_secs(5), |id, body| {
            is_block_change(id, body, 3, 63, 3) && body[9] == 2
        }),
        "the (R,R) corner must be protected (inclusive boundary)"
    );
    client.collect(Duration::from_millis(750));
    assert!(
        !client.saw_block_change(3, 63, 3, 0),
        "the protected corner block must never turn to air"
    );

    // R+1 on the same row is editable: the identical creative dig now
    // succeeds and the break broadcasts block id 0.
    client.send(&dig(0, 4, 63, 3, 1));
    assert!(
        client.until(Duration::from_secs(5), |id, body| {
            is_block_change(id, body, 4, 63, 3) && body[9] == 0
        }),
        "the R+1 column outside the square must remain editable"
    );

    // Placement reaching INTO the square from an unprotected clicked block
    // is refused: click (4,63,0) on its -X face would place into (3,63,0).
    client.send(&creative_give_cobble());
    client.drain();
    client.send(&place_cobble(4, 63, 0, 4));
    assert!(
        client.until(Duration::from_secs(5), |id, body| {
            is_block_change(id, body, 4, 63, 0) && body[9] == 2
        }),
        "the refused placement must answer with the clicked block state"
    );
    client.collect(Duration::from_millis(750));
    assert!(
        !client.saw_block_change(3, 63, 0, 4),
        "no cobblestone may be placed inside the protected square"
    );

    // Placement from a protected clicked block is refused outright.
    client.send(&place_cobble(0, 63, 0, 1));
    assert!(
        client.until(Duration::from_secs(5), |id, body| {
            is_block_change(id, body, 0, 63, 0) && body[9] == 2
        }),
        "clicking the protected center with a block must resync, not place"
    );
    client.collect(Duration::from_millis(750));
    assert!(
        !client.saw_block_change(0, 64, 0, 4),
        "no cobblestone may be placed on top of the protected spawn block"
    );

    // Refusals never disconnect: the session is still answering dig
    // packets afterwards (the very next packet below gets a 0x23 again).
    client.send(&dig(0, 0, 63, 0, 1));
    assert!(
        client.until(Duration::from_secs(5), |id, body| {
            is_block_change(id, body, 0, 63, 0) && body[9] == 2
        }),
        "repeated refusals must not kick the session"
    );

    server.stop();
}

#[test]
fn audit1318_empty_operators_auto_disables_and_zero_radius_disables() {
    // (a) R=3 but NO operators configured: vanilla auto-disables the
    // square, so the same creative dig at the spawn center succeeds.
    let (server, port) = spawn_server("auto-off", "3", "[]\n");
    wait_port(port);
    let (mut client, _) = join(port, "RustTester");
    client.send(&dig(0, 0, 63, 0, 1));
    assert!(
        client.until(Duration::from_secs(5), |id, body| {
            is_block_change(id, body, 0, 63, 0) && body[9] == 0
        }),
        "an empty operators list must auto-disable spawn protection"
    );
    client.send(&dig(0, 3, 63, 3, 1));
    assert!(
        client.until(Duration::from_secs(5), |id, body| {
            is_block_change(id, body, 3, 63, 3) && body[9] == 0
        }),
        "the former (R,R) corner must break while protection is auto-off"
    );
    // Stop (a) before (b): stop() consumes the handle, and its Drop then
    // reaps the child and removes the scratch directory.
    server.stop();

    // (b) Operators ARE configured, but R=0 disables protection.
    let (server, port) = spawn_server("r-zero", "0", SEEDED_OPS);
    wait_port(port);
    let (mut client, _) = join(port, "RustTester");
    client.send(&dig(0, 0, 63, 0, 1));
    assert!(
        client.until(Duration::from_secs(5), |id, body| {
            is_block_change(id, body, 0, 63, 0) && body[9] == 0
        }),
        "spawn-protection=0 must disable the square even with operators"
    );
    server.stop();
}
