//! REG-190 / #826: world time and crop state across a graceful stop and
//! a restart of the real server binary.
//!
//! Scenario: plant wheat on hydrated farmland in an isolated world, let
//! the server run, stop it gracefully with /stop, leave it stopped, then
//! restart on the same world directory. The world age must neither
//! rewind nor jump forward by the stopped duration (no catch-up storm),
//! and the crop/farmland blocks must survive the restart unchanged.
//!
//! The 20-minute soak of the same scenario was executed once as a
//! measured procedure (recorded on the issue); this test pins the
//! invariants in the regular suite with a shorter stopped interval.
//! An unmodified Minecraft Java 1.7.10 client run remains NOT_RUN.
use flate2::read::ZlibDecoder;
use legacy_protocol::{read_frame, read_varint, write_frame, write_string, write_varint, Cursor};
use serde_json::Value;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const STOPPED: Duration = Duration::from_secs(20);

struct Server {
    child: Child,
    stdin: ChildStdin,
    /// Kept so the scratch log path stays addressable for diagnostics.
    #[allow(dead_code)]
    stderr: PathBuf,
}

fn packet(id: u32, body: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();
    write_varint(id, &mut payload);
    payload.extend_from_slice(body);
    let mut framed = Vec::new();
    write_frame(&mut framed, &payload).unwrap();
    framed
}

fn handshake(port: u16, next: u32) -> Vec<u8> {
    let mut body = Vec::new();
    write_varint(5, &mut body);
    write_string("localhost", &mut body).unwrap();
    body.extend_from_slice(&port.to_be_bytes());
    write_varint(next, &mut body);
    body
}

fn status(port: u16) -> io::Result<Value> {
    let mut socket = TcpStream::connect(("127.0.0.1", port))?;
    socket.set_read_timeout(Some(Duration::from_secs(8)))?;
    socket.write_all(&packet(0, &handshake(port, 1)))?;
    socket.write_all(&packet(0, &[]))?;
    let framed = read_frame(&mut socket)?;
    let mut body = framed.as_slice();
    if read_varint(&mut body)? != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "missing Status JSON",
        ));
    }
    let mut reader = Cursor::new(body);
    let json: Value = serde_json::from_str(reader.read_string(32768)?)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let token: i64 = 0x1020_3040_5060_7080;
    socket.write_all(&packet(1, &token.to_be_bytes()))?;
    let framed = read_frame(&mut socket)?;
    let mut body = framed.as_slice();
    if read_varint(&mut body)? != 1 || body != token.to_be_bytes() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid Status Pong",
        ));
    }
    Ok(json)
}

fn start_server(root: &std::path::Path, world_name: &str) -> (Server, u16) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let stderr = root.join(format!("server-{world_name}-{port}.log"));
    let mut child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
        .args(["--bind", &format!("127.0.0.1:{port}"), "--world"])
        .arg(root.join(world_name))
        .arg("--whitelist")
        .arg(root.join("whitelist.json"))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::from(File::create(&stderr).unwrap()))
        .spawn()
        .expect("launch native Rust server");
    let stdin = child.stdin.take().unwrap();
    let server = Server {
        child,
        stdin,
        stderr,
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if status(port).is_ok() {
            return (server, port);
        }
        assert!(Instant::now() < deadline, "server readiness timed out");
        thread::sleep(Duration::from_millis(50));
    }
}

impl Server {
    fn stop(mut self) {
        writeln!(self.stdin, "/stop").unwrap();
        self.stdin.flush().unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            assert!(Instant::now() < deadline, "graceful /stop timed out");
            thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Play client that tracks Time Update ages and Block Changes for the two
/// farm cells, and can decode a block out of a Chunk Data packet.
struct Client {
    socket: TcpStream,
    last_age: Option<i64>,
    farmland: Option<(u8, u8)>,
    wheat: Option<(u8, u8)>,
    /// Farm cells as decoded from the most recent Chunk Data for (0,0);
    /// captured during the join burst, when chunks actually flow.
    chunk_farm: Option<((u8, u8), (u8, u8))>,
}

impl Client {
    fn join(port: u16, name: &str) -> Client {
        let socket = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        socket
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        let mut client = Client {
            socket,
            last_age: None,
            farmland: None,
            wheat: None,
            chunk_farm: None,
        };
        client.send(0, &handshake(port, 2));
        let mut login_start = Vec::new();
        write_string(name, &mut login_start).unwrap();
        client.send(0, &login_start);
        client.until(2, |_| true);
        client.until(0x08, |_| true); // Player Position & Look
        client
    }

    fn send(&mut self, id: u32, body: &[u8]) {
        self.socket.write_all(&packet(id, body)).unwrap();
    }

    fn read_packet(&mut self) -> (u32, Vec<u8>) {
        loop {
            let framed = read_frame(&mut self.socket).expect("read framed packet");
            let mut body = framed.as_slice();
            let id = read_varint(&mut body).expect("packet id");
            let payload = body.to_vec();
            match id {
                0x00 if payload.len() == 4 => {
                    self.send(0x00, &payload); // KeepAlive echo
                    continue;
                }
                0x03 if payload.len() == 16 => {
                    self.last_age = Some(i64::from_be_bytes(payload[..8].try_into().unwrap()));
                    return (id, payload);
                }
                0x21 => {
                    if let (Some(f), Some(w)) = (
                        Self::chunk_block(&payload, 2, 63, 2),
                        Self::chunk_block(&payload, 2, 64, 2),
                    ) {
                        self.chunk_farm = Some((f, w));
                    }
                    return (id, payload);
                }
                0x23 if payload.len() >= 11 => {
                    let x = i32::from_be_bytes(payload[..4].try_into().unwrap());
                    let y = payload[4];
                    let z = i32::from_be_bytes(payload[5..9].try_into().unwrap());
                    let cell = (payload[9], payload[10] & 0x0f);
                    if (x, y, z) == (2, 63, 2) {
                        self.farmland = Some(cell);
                    } else if (x, y, z) == (2, 64, 2) {
                        self.wheat = Some(cell);
                    }
                    return (id, payload);
                }
                _ => return (id, payload),
            }
        }
    }

    fn until(&mut self, wanted: u32, accept: impl Fn(&[u8]) -> bool) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let (id, payload) = self.read_packet();
            if id == wanted && accept(&payload) {
                return payload;
            }
            assert!(
                Instant::now() < deadline,
                "packet {wanted:#x} never arrived"
            );
        }
    }

    /// Read packets for a wall-clock duration, updating tracked state.
    fn observe(&mut self, duration: Duration) {
        let deadline = Instant::now() + duration;
        self.socket
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        while Instant::now() < deadline {
            let _ = self.read_packet_lenient();
        }
        self.socket
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
    }

    fn read_packet_lenient(&mut self) -> Option<(u32, Vec<u8>)> {
        // read_frame on a timed-out socket errors; treat as "no packet".
        let framed = read_frame(&mut self.socket).ok()?;
        let mut body = framed.as_slice();
        let id = read_varint(&mut body).ok()?;
        let payload = body.to_vec();
        match id {
            0x00 if payload.len() == 4 => {
                self.send(0x00, &payload);
                None
            }
            0x03 if payload.len() == 16 => {
                self.last_age = Some(i64::from_be_bytes(payload[..8].try_into().unwrap()));
                None
            }
            0x21 => {
                if let (Some(f), Some(w)) = (
                    Self::chunk_block(&payload, 2, 63, 2),
                    Self::chunk_block(&payload, 2, 64, 2),
                ) {
                    self.chunk_farm = Some((f, w));
                }
                Some((id, payload))
            }
            0x23 if payload.len() >= 11 => {
                let x = i32::from_be_bytes(payload[..4].try_into().unwrap());
                let y = payload[4];
                let z = i32::from_be_bytes(payload[5..9].try_into().unwrap());
                let cell = (payload[9], payload[10] & 0x0f);
                if (x, y, z) == (2, 63, 2) {
                    self.farmland = Some(cell);
                } else if (x, y, z) == (2, 64, 2) {
                    self.wheat = Some(cell);
                }
                Some((id, payload))
            }
            _ => Some((id, payload)),
        }
    }

    fn creative_set(&mut self, slot: i16, item: i16) {
        let mut body = slot.to_be_bytes().to_vec();
        body.extend_from_slice(&item.to_be_bytes());
        body.push(1);
        body.extend_from_slice(&0i16.to_be_bytes());
        body.extend_from_slice(&(-1i16).to_be_bytes());
        self.send(0x10, &body);
        let slot_bytes = slot.to_be_bytes();
        self.until(0x2f, |p| {
            p.len() >= 3 && p[0] == 0 && p[1] == slot_bytes[0] && p[2] == slot_bytes[1]
        });
    }

    fn block_use(&mut self, x: i32, y: u8, z: i32, held: i16) {
        let mut body = x.to_be_bytes().to_vec();
        body.push(y);
        body.extend_from_slice(&z.to_be_bytes());
        body.push(1); // top face
        body.extend_from_slice(&held.to_be_bytes());
        body.push(1);
        body.extend_from_slice(&0i16.to_be_bytes());
        body.extend_from_slice(&(-1i16).to_be_bytes());
        body.extend_from_slice(&[8, 8, 8]);
        self.send(0x08, &body);
    }

    fn select_slot(&mut self, slot: i16) {
        self.send(0x09, &slot.to_be_bytes());
    }

    /// Decode one block (id, data) from a Chunk Data payload for (0,0).
    fn chunk_block(payload: &[u8], bx: i32, by: i32, bz: i32) -> Option<(u8, u8)> {
        let cx = i32::from_be_bytes(payload[..4].try_into().unwrap());
        let cz = i32::from_be_bytes(payload[4..8].try_into().unwrap());
        if cx != bx >> 4 || cz != bz >> 4 {
            return None;
        }
        let primary = u16::from_be_bytes(payload[9..11].try_into().unwrap());
        let compressed = i32::from_be_bytes(payload[13..17].try_into().unwrap()) as usize;
        let mut raw = Vec::new();
        ZlibDecoder::new(&payload[17..17 + compressed])
            .read_to_end(&mut raw)
            .ok()?;
        let section = (by >> 4) as usize;
        if primary >> section & 1 == 0 {
            return Some((0, 0));
        }
        let present: Vec<usize> = (0..16).filter(|s| primary >> s & 1 == 1).collect();
        let spos = present.iter().position(|&s| s == section)?;
        let sections = present.len();
        let idx = ((by & 15) * 256 + (bz & 15) * 16 + (bx & 15)) as usize;
        let id = raw[spos * 4096 + idx];
        let data_byte = raw[sections * 4096 + spos * 2048 + idx / 2];
        let data = if idx.is_multiple_of(2) {
            data_byte & 0x0f
        } else {
            data_byte >> 4
        };
        Some((id, data))
    }

    /// The farm cells from the most recent (0,0) chunk, captured while
    /// packets flowed past (join burst / observe windows).
    fn farm_cells_from_chunk(&self) -> ((u8, u8), (u8, u8)) {
        self.chunk_farm.expect("chunk (0,0) was never received")
    }
}

#[test]
fn reg190_time_and_crops_survive_graceful_stop_and_restart() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "industrialrust-reg190-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&root).unwrap();
    fs::write(root.join("whitelist.json"), r#"[{"name":"TimeKeeper"}]"#).unwrap();
    let panicked = std::panic::catch_unwind(|| {
        let (server, port) = start_server(&root, "flat.irw");
        let mut client = Client::join(port, "TimeKeeper");
        // Hoe in the selected hotbar slot, seeds in the next one: on this
        // server dry crop-less farmland reverts within a second, so the
        // hoe -> select -> plant sequence must not pause to read.
        client.creative_set(36, 290);
        client.creative_set(37, 295);
        client.block_use(2, 63, 2, 290);
        client.select_slot(1);
        client.block_use(2, 63, 2, 295);
        client.observe(Duration::from_secs(3));
        assert_eq!(
            client.farmland.map(|c| c.0),
            Some(60),
            "hoeing must make farmland"
        );
        assert_eq!(
            client.wheat.map(|c| c.0),
            Some(59),
            "seeds must plant wheat"
        );
        // Hydrate last: the hoe requires air above the soil.
        client.creative_set(38, 326);
        client.select_slot(2);
        client.block_use(3, 63, 2, 326);
        let age_before_stop = client
            .last_age
            .expect("Time Update must flow while running");
        let wheat_before = client.wheat;
        let farmland_before = client.farmland;
        drop(client);
        server.stop();

        // ---- Stopped interval: no server process at all.
        thread::sleep(STOPPED);

        // ---- Phase B: restart on the same world and compare.
        let (server, port) = start_server(&root, "flat.irw");
        let mut client = Client::join(port, "TimeKeeper");
        client.observe(Duration::from_secs(3));
        let age_after = client
            .last_age
            .expect("Time Update must flow after restart");
        let (farmland_after, wheat_after) = client.farm_cells_from_chunk();
        drop(client);
        server.stop();

        assert!(
            age_after >= age_before_stop - 2,
            "world age rewound across restart: {age_before_stop} -> {age_after}"
        );
        assert!(
            age_after - age_before_stop <= 150,
            "stopped ticks were caught up across restart: {age_before_stop} -> {age_after}"
        );
        assert_eq!(
            farmland_after.0, 60,
            "farmland did not survive restart: {farmland_after:?} (before {farmland_before:?})"
        );
        assert_eq!(
            wheat_after.0, 59,
            "wheat did not survive restart: {wheat_after:?} (before {wheat_before:?})"
        );
        println!(
            "REG-190 measured: age_before_stop={age_before_stop} age_after_restart={age_after} \
             wheat_before={wheat_before:?} wheat_after={wheat_after:?} farmland_after={farmland_after:?}"
        );
    });
    if let Err(err) = panicked {
        eprintln!("REG-190 scratch retained: {}", root.display());
        for entry in fs::read_dir(&root).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".log") {
                eprintln!(
                    "--- {name}\n{}",
                    fs::read_to_string(entry.path()).unwrap_or_default()
                );
            }
        }
        std::panic::resume_unwind(err);
    }
    fs::remove_dir_all(&root).unwrap();
}
