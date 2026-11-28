//! ADD-025 / #449: a player who disconnects around a portal transfer
//! must resume in exactly one safe dimension/position: before the
//! transfer commits, the old dimension; after it commits, the
//! destination (whose state is saved before the session switches).
//!
//! Building a working portal end to end also pins the end-frame
//! placement fix: frames placed by a client now face the placer
//! (legacy metadata 0=S, 1=W, 2=N, 3=E), so a ring built from its
//! center faces inward and can activate. Before the fix, frames
//! always landed with metadata 0 and no client-built ring could
//! ever activate; nether portals likewise cannot be lit by clients
//! yet (no flint-and-steel ignition path), so this test uses an
//! End portal, whose transfer is instant and shares the same
//! commit path in the session handler.
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
    let dir = std::env::temp_dir().join(format!("ir1710-add025-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("whitelist.json"), r#"[{"name":"Keeper025"}]"#).unwrap();
    let stderr = std::fs::File::create(dir.join("server-stderr.log")).unwrap();
    // Portal transfers only run in multiworld mode (--dimension-root),
    // where the three dimension worlds share one root.
    let child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
        .args([
            "--bind",
            &format!("127.0.0.1:{port}"),
            "--survival",
            "--expanded-seed",
            "42",
            "--dimension-root",
        ])
        .arg(dir.join("dims"))
        .args(["--dimension", "overworld", "--whitelist"])
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
    place_at(client, x, y, z, 1, item);
}

/// Block placement / use against `face` of the target cell
/// (0=-y, 1=+y, 2=-z, 3=+z, 4=-x, 5=+x).
fn place_at(client: &mut Client, x: i32, y: u8, z: i32, face: u8, item: Option<(i16, u8)>) {
    let mut p = x.to_be_bytes().to_vec();
    p.push(y);
    p.extend_from_slice(&z.to_be_bytes());
    p.push(face);
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
fn move_to(client: &mut Client, x: f64, feet_y: f64, z: f64) {
    let mut p = x.to_be_bytes().to_vec();
    p.extend_from_slice(&feet_y.to_be_bytes());
    p.extend_from_slice(&(feet_y + 1.62).to_be_bytes());
    p.extend_from_slice(&z.to_be_bytes());
    p.extend_from_slice(&0f32.to_be_bytes());
    p.extend_from_slice(&0f32.to_be_bytes());
    p.push(1);
    client.send(&frame(0x06, &p));
}

fn held_change(client: &mut Client, hotbar_index: i16) {
    client.send(&frame(0x09, &hotbar_index.to_be_bytes()));
}

/// Join and return (client, spawn, dimension from Join Game).
fn join_dim(port: u16, name: &str) -> (Client, (f64, f64, f64), i8) {
    join_dim_stage(port, name, "join")
}

fn join_dim_stage(port: u16, name: &str, stage: &str) -> (Client, (f64, f64, f64), i8) {
    let mut client = login(port, name);
    let join_game = client
        .until(Duration::from_secs(15), |id, _| id == 0x01)
        .unwrap_or_else(|| panic!("{stage}: join must deliver Join Game"));
    let dimension = join_game[5] as i8;
    let pos = client
        .until(Duration::from_secs(10), |id, _| id == 0x08)
        .expect("join must deliver Position & Look");
    client.collect(Duration::from_millis(500));
    (
        client,
        (be_f64(&pos, 0), be_f64(&pos, 8), be_f64(&pos, 16)),
        dimension,
    )
}

fn block_change(client: &mut Client, x: i32, y: u8, z: i32, want_id: u8) -> Option<Vec<u8>> {
    block_change_within(client, x, y, z, want_id, Duration::from_secs(5))
}

fn block_change_within(
    client: &mut Client,
    x: i32,
    y: u8,
    z: i32,
    want_id: u8,
    timeout: Duration,
) -> Option<Vec<u8>> {
    client.until(timeout, |id, payload| {
        id == 0x23
            && payload.len() >= 11
            && i32::from_be_bytes(payload[0..4].try_into().unwrap()) == x
            && payload[4] == y
            && i32::from_be_bytes(payload[5..9].try_into().unwrap()) == z
            && payload[9] == want_id
    })
}

/// Rejoin after an abrupt drop. The dropped session can take a while
/// to be reaped server-side (especially right after a dimension
/// transfer, when the session was mid chunk-stream), and a login that
/// arrives before that may be held without a Join Game. Retry like a
/// real player reconnecting until the join completes.
fn join_dim_retry(port: u16, name: &str, stage: &str) -> (Client, (f64, f64, f64), i8) {
    for attempt in 0..10 {
        let mut client = login(port, name);
        match client.until(Duration::from_secs(6), |id, _| id == 0x01) {
            Some(join_game) => {
                let dimension = join_game[5] as i8;
                let pos = client
                    .until(Duration::from_secs(10), |id, _| id == 0x08)
                    .unwrap_or_else(|| panic!("{stage}: join must deliver Position & Look"));
                client.collect(Duration::from_millis(500));
                return (
                    client,
                    (be_f64(&pos, 0), be_f64(&pos, 8), be_f64(&pos, 16)),
                    dimension,
                );
            }
            None => {
                println!("{stage}: join attempt {attempt} held; retrying");
                drop(client);
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    }
    panic!("{stage}: join never delivered Join Game after retries");
}

#[test]
fn add025_portal_disconnect_resumes_in_exactly_one_dimension() {
    let (mut server, dir, port) = spawn_server("portal");
    wait_port(port);
    let (mut client, spawn, dim) = join_dim(port, "Keeper025");
    assert_eq!(dim, 0, "first login must start in the Overworld");
    // Multiworld boots in survival; the console promotes the builder to
    // creative so blocks can be staged via Creative Inventory Action.
    {
        let stdin = server.stdin.as_mut().unwrap();
        stdin.write_all(b"/gamemode creative Keeper025\n").unwrap();
        stdin.flush().unwrap();
    }
    let mode = client.until(Duration::from_secs(5), |id, payload| {
        id == 0x2b && payload.first() == Some(&3)
    });
    assert!(
        mode.is_some(),
        "gamemode change to creative must be delivered"
    );
    let cx = spawn.0.floor() as i32;
    let cz = spawn.2.floor() as i32;
    // (ground discovery happens after the gamemode switch, below)
    // Find the surface of the spawn column by probing downward with
    // placement clicks: clicking a solid cell lands a stone on top of
    // it; clicking air is refused. A candidate is only accepted when a
    // second stone lands on top of the first — under load a single
    // response can arrive late and poison a naive scan. The probe
    // stones form the bottom of the platform column.
    creative_set(&mut client, 36, 1, 64);
    let mut ground_top = None;
    for y in (48..=72).rev() {
        client.backlog.clear();
        use_block(&mut client, cx, y as u8, cz, Some((1, 1)));
        let first = block_change_within(
            &mut client,
            cx,
            (y + 1) as u8,
            cz,
            1,
            Duration::from_millis(900),
        );
        if first.is_none() {
            continue;
        }
        // Verify: another stone must land on top of the probe stone.
        use_block(&mut client, cx, (y + 1) as u8, cz, Some((1, 1)));
        let second = block_change_within(
            &mut client,
            cx,
            (y + 2) as u8,
            cz,
            1,
            Duration::from_secs(5),
        );
        if second.is_some() {
            ground_top = Some(y);
            break;
        }
    }
    // Build a flat 5x5 stone platform a few blocks above the terrain so
    // the ring has a level, known support everywhere.
    let ground_top = ground_top.expect("surface probe must find solid ground");
    println!("surface probe: ground_top={ground_top} at ({cx},{cz})");
    let plat = ground_top + 4;
    // Step aside: the center column rises through the cell the player
    // spawns in, and a block cannot be placed inside the player.
    move_to(
        &mut client,
        cx as f64 + 2.5,
        (ground_top + 1) as f64,
        cz as f64 + 0.5,
    );
    client.collect(Duration::from_millis(300));
    // Center column up from the ground (the probe already placed
    // the stones at ground_top + 1 and ground_top + 2).
    for y in (ground_top + 2)..plat {
        // One retry: the multiworld server can be busy generating
        // dimension chunks right after boot.
        let mut landed = false;
        for _ in 0..2 {
            use_block(&mut client, cx, y as u8, cz, Some((1, 1)));
            if block_change_within(
                &mut client,
                cx,
                (y + 1) as u8,
                cz,
                1,
                Duration::from_secs(10),
            )
            .is_some()
            {
                landed = true;
                break;
            }
        }
        assert!(landed, "platform column placement must land at y={}", y + 1);
    }
    // Expand the top layer outward in BFS order: every new cell is
    // placed against the side face of an already-placed neighbor.
    {
        let mut placed: Vec<(i32, i32)> = vec![(cx, cz)];
        let mut queue: Vec<(i32, i32)> = vec![(cx, cz)];
        while let Some((px, pz)) = queue.pop() {
            for (dx, dz, face) in [(1, 0, 5u8), (-1, 0, 4), (0, 1, 3), (0, -1, 2)] {
                let (tx, tz) = (px + dx, pz + dz);
                if (tx - cx).abs() > 2 || (tz - cz).abs() > 2 || placed.contains(&(tx, tz)) {
                    continue;
                }
                place_at(&mut client, px, plat as u8, pz, face, Some((1, 1)));
                block_change(&mut client, tx, plat as u8, tz, 1)
                    .unwrap_or_else(|| panic!("platform cell ({tx},{tz}) placement must land"));
                placed.push((tx, tz));
                queue.push((tx, tz));
            }
        }
        assert_eq!(placed.len(), 25);
    }

    // Stand in the middle of the future ring, on the platform.
    move_to(
        &mut client,
        cx as f64 + 0.5,
        (plat + 1) as f64,
        cz as f64 + 0.5,
    );
    client.collect(Duration::from_millis(300));

    // Build the 12-frame ring on the platform. Frames must face the
    // placer (the center), or the ring can never activate.
    creative_set(&mut client, 36, 120, 12);
    let frame_y = plat + 1;
    let mut ring: Vec<(i32, i32, u16)> = Vec::new();
    for dx in -2i32..=2 {
        for dz in -2i32..=2 {
            if (dx.abs() == 2 || dz.abs() == 2) && !(dx.abs() == 2 && dz.abs() == 2) {
                let facing: u16 = if dz == -2 {
                    0 // north side faces south
                } else if dz == 2 {
                    2 // south side faces north
                } else if dx == -2 {
                    3 // west side faces east
                } else {
                    1 // east side faces west
                };
                ring.push((cx + dx, cz + dz, facing));
            }
        }
    }
    assert_eq!(ring.len(), 12);
    for &(fx, fz, facing) in &ring {
        use_block(&mut client, fx, plat as u8, fz, Some((120, 1)));
        let change = block_change(&mut client, fx, frame_y as u8, fz, 120)
            .expect("frame placement must produce a Block Change");
        assert_eq!(
            change[10] as u16, facing,
            "frame at ({fx},{fz}) must face the placer/center"
        );
    }

    // Insert eyes into 11 of the 12 frames.
    creative_set(&mut client, 38, 381, 12);
    held_change(&mut client, 2);
    for &(fx, fz, facing) in &ring[..11] {
        use_block(&mut client, fx, frame_y as u8, fz, Some((381, 1)));
        let change = block_change(&mut client, fx, frame_y as u8, fz, 120)
            .expect("eye insertion must produce a Block Change");
        assert_eq!(
            change[10] as u16,
            facing | 4,
            "eye bit must be set on the frame at ({fx},{fz})"
        );
    }

    // Stage 1: disconnect BEFORE the transfer can commit (the portal
    // is not even active). The resume point must be the Overworld.
    drop(client);
    std::thread::sleep(Duration::from_millis(800));
    let (mut client, _, dim) = join_dim(port, "Keeper025");
    assert_eq!(
        dim, 0,
        "pre-activation disconnect must resume in the Overworld"
    );

    // Complete the portal from outside the ring.
    move_to(
        &mut client,
        cx as f64 + 0.5,
        (plat + 1) as f64,
        cz as f64 + 3.5,
    );
    client.collect(Duration::from_millis(300));
    let &(fx, fz, _) = &ring[11];
    use_block(&mut client, fx, frame_y as u8, fz, Some((381, 1)));
    let filled = client.until(Duration::from_secs(8), |id, payload| {
        id == 0x23 && payload.len() >= 11 && payload[9] == 119
    });
    assert!(
        filled.is_some(),
        "the completed ring must fill with End Portal blocks"
    );

    // Stage 2: step in, the transfer commits instantly; disconnect
    // immediately. The destination state was saved before the switch,
    // so the resume point must be The End.
    move_to(
        &mut client,
        cx as f64 + 0.5,
        (plat + 1) as f64,
        cz as f64 + 0.5,
    );
    let respawn = client.until(Duration::from_secs(8), |id, _| id == 0x07);
    assert!(
        respawn.is_some(),
        "stepping into an End portal must transfer (Respawn)"
    );
    drop(client);
    std::thread::sleep(Duration::from_millis(800));
    let (client3, _, dim) = join_dim_retry(port, "Keeper025", "stage-2 rejoin");
    assert_eq!(dim, 1, "post-transfer disconnect must resume in The End");
    drop(client3);

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
