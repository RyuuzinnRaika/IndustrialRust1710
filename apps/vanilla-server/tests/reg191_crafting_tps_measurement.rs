//! REG-191 / #827: measured load regression against the real server binary.
//!
//! Eight Protocol 5 clients join one isolated world, each repeatedly crafts
//! sticks at its own workbench (2 planks -> 4 sticks), while the test
//! measures the server's tick rate from Time Update packets and the Status
//! request latency over TCP. Item conservation is asserted on every craft:
//! exactly two planks are consumed and exactly four sticks appear, and the
//! accumulated stick count in each inventory grows by four per craft.
//!
//! This is a native Rust measurement, not a vanilla-client test: an
//! unmodified Minecraft Java 1.7.10 run remains NOT_RUN. The measured TPS
//! and latency percentiles are printed so they can be quoted in the PR.
use legacy_protocol::{read_frame, read_varint, write_frame, write_string, write_varint, Cursor};
use serde_json::Value;
use std::fs::{self, File};
use std::io::{self, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const PLAYERS: usize = 8;
/// Shared Time Update samples: (arrival instant, world age in ticks).
type AgeSamples = Arc<Mutex<Vec<(Instant, i64)>>>;
const CRAFTS_PER_PLAYER: usize = 30;
const PLANKS: i16 = 5;
/// Workbench columns for each player: a ring around the shared spawn
/// point. Every cell is within normal block-placement reach of the
/// players, who all spawn at (0.5, 64, 0.5).
const BENCHES: [(i32, i32); PLAYERS] = [
    (1, 0),
    (-1, 0),
    (0, 1),
    (0, -1),
    (1, 1),
    (1, -1),
    (-1, 1),
    (-1, -1),
];
const STICKS: i16 = 280;

struct Server {
    child: Child,
    stdin: ChildStdin,
    root: PathBuf,
    stderr: PathBuf,
    port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if thread::panicking() {
            eprintln!(
                "REG-191 isolated server log:\n{}",
                fs::read_to_string(&self.stderr).unwrap_or_default()
            );
            eprintln!("REG-191 scratch retained: {}", self.root.display());
        } else {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
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

/// One Play-state client. Records Time Update ages when a sampler is
/// attached and always answers KeepAlive challenges.
struct Client {
    socket: TcpStream,
    sampler: Option<AgeSamples>,
}

impl Client {
    fn send(&mut self, id: u32, body: &[u8]) {
        self.socket.write_all(&packet(id, body)).unwrap();
    }

    fn read_packet(&mut self) -> (u32, Vec<u8>) {
        loop {
            let framed = read_frame(&mut self.socket).expect("read framed packet");
            let mut body = framed.as_slice();
            let id = read_varint(&mut body).expect("packet id");
            let payload = body.to_vec();
            if id == 0x00 && payload.len() == 4 {
                // Play KeepAlive: echo the challenge immediately.
                self.send(0x00, &payload);
                continue;
            }
            if id == 0x03 && payload.len() == 16 {
                if let Some(samples) = &self.sampler {
                    let age = i64::from_be_bytes(payload[..8].try_into().unwrap());
                    samples.lock().unwrap().push((Instant::now(), age));
                }
                continue;
            }
            return (id, payload);
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

    fn join(port: u16, name: &str, sampler: Option<AgeSamples>) -> Client {
        let socket = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        socket
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut client = Client { socket, sampler };
        client.send(0, &handshake(port, 2));
        let mut login_start = Vec::new();
        write_string(name, &mut login_start).unwrap();
        client.send(0, &login_start);
        let success = client.until(2, |_| true);
        let mut reader = Cursor::new(&success);
        assert_eq!(reader.read_string(40).unwrap().len(), 36);
        assert_eq!(reader.read_string(16).unwrap(), name);
        // Drain the initial burst until Player Position & Look.
        client.until(0x08, |_| true);
        client
    }

    fn creative_set(&mut self, slot: i16, item: i16, count: u8) {
        let mut body = slot.to_be_bytes().to_vec();
        body.extend_from_slice(&item.to_be_bytes());
        body.push(count);
        body.extend_from_slice(&0i16.to_be_bytes());
        body.extend_from_slice(&(-1i16).to_be_bytes());
        self.send(0x10, &body);
        let slot_bytes = slot.to_be_bytes();
        self.until(0x2f, |p| {
            p.len() >= 3 && p[1] == slot_bytes[0] && p[2] == slot_bytes[1]
        });
    }

    fn open_workbench(&mut self, x: i32, z: i32) -> u8 {
        let mut body = x.to_be_bytes().to_vec();
        body.push(64);
        body.extend_from_slice(&z.to_be_bytes());
        body.push(1); // top face
        body.extend_from_slice(&(-1i16).to_be_bytes()); // empty held item
        body.extend_from_slice(&[8, 8, 8]); // cursor position
        self.send(0x08, &body);
        let opened = self.until(0x2d, |_| true);
        let window = opened[0];
        self.until(0x30, |p| p.first() == Some(&window));
        self.until(0x2f, |p| p.starts_with(&[255, 255, 255]));
        window
    }

    /// One window click; returns the parsed Window Items slots afterwards.
    fn click(&mut self, window: u8, slot: i16, action: i16, button: u8) -> Vec<Option<(i16, u8)>> {
        let mut body = vec![window];
        body.extend_from_slice(&slot.to_be_bytes());
        body.push(button);
        body.extend_from_slice(&action.to_be_bytes());
        body.push(0); // normal click mode
        body.extend_from_slice(&(-1i16).to_be_bytes()); // untrusted empty claim
        self.send(0x0e, &body);
        let action_bytes = action.to_be_bytes();
        self.until(0x32, |p| {
            p.len() == 4
                && p[0] == window
                && p[1] == action_bytes[0]
                && p[2] == action_bytes[1]
                && p[3] == 1
        });
        let items = self.until(0x30, |p| p.first() == Some(&window));
        self.until(0x2f, |p| p.starts_with(&[255, 255, 255]));
        parse_window_items(&items)
    }

    /// Craft a workbench in the 2x2 player grid (1 log -> 4 planks ->
    /// 1 table), exactly as a client would, then place it at (x, 64, 16).
    /// Console /setblock refuses TileEntity blocks, so the table must come
    /// from real crafting and placement packets.
    fn make_and_place_workbench(&mut self, x: i32, z: i32, action: &mut i16) {
        fn click0(
            client: &mut Client,
            action: &mut i16,
            slot: i16,
            button: u8,
        ) -> Vec<Option<(i16, u8)>> {
            let items = client.click(0, slot, *action, button);
            *action += 1;
            items
        }
        self.creative_set(36, 17, 1); // one oak log
        click0(self, action, 36, 0); // log to cursor
        let items = click0(self, action, 1, 0); // log into the 2x2 grid
        assert_eq!(items[0], Some((PLANKS, 4)), "log must offer 4 planks");
        click0(self, action, 0, 0); // planks to cursor, log consumed
        for slot in 1..=4 {
            click0(self, action, slot, 1); // one plank per grid slot
        }
        let items = click0(self, action, 0, 0); // workbench to cursor
        assert!(
            items[0].is_none(),
            "grid must be spent after taking the table"
        );
        click0(self, action, 36, 0); // table into hotbar slot 36
                                     // Place it on the flat ground (grass top is y=63) at z=16.
        let mut body = x.to_be_bytes().to_vec();
        body.push(63);
        body.extend_from_slice(&z.to_be_bytes());
        body.push(1); // top face
        body.extend_from_slice(&58i16.to_be_bytes()); // held: workbench
        body.push(1);
        body.extend_from_slice(&0i16.to_be_bytes());
        body.extend_from_slice(&(-1i16).to_be_bytes());
        body.extend_from_slice(&[8, 8, 8]);
        self.send(0x08, &body);
        let x_bytes = x.to_be_bytes();
        let z_bytes = z.to_be_bytes();
        self.until(0x23, |p| {
            p.len() >= 11 && p[..4] == x_bytes && p[4] == 64 && p[5..9] == z_bytes && p[9] == 58
        });
    }

    fn close_window(&mut self, window: u8) {
        self.send(0x0d, &[window]);
    }
}

fn parse_window_items(payload: &[u8]) -> Vec<Option<(i16, u8)>> {
    let count = i16::from_be_bytes(payload[1..3].try_into().unwrap()) as usize;
    let mut offset = 3;
    let mut slots = Vec::with_capacity(count);
    for _ in 0..count {
        let id = i16::from_be_bytes(payload[offset..offset + 2].try_into().unwrap());
        offset += 2;
        if id == -1 {
            slots.push(None);
            continue;
        }
        let amount = payload[offset];
        offset += 1;
        let _damage = i16::from_be_bytes(payload[offset..offset + 2].try_into().unwrap());
        offset += 2;
        let nbt = i16::from_be_bytes(payload[offset..offset + 2].try_into().unwrap());
        offset += 2;
        assert_eq!(nbt, -1, "test items must not carry NBT");
        slots.push(Some((id, amount)));
    }
    slots
}

fn status(port: u16) -> io::Result<(Value, Duration)> {
    let started = Instant::now();
    let mut socket = TcpStream::connect(("127.0.0.1", port))?;
    socket.set_read_timeout(Some(Duration::from_secs(8)))?;
    socket.write_all(&packet(0, &handshake(port, 1)))?;
    socket.write_all(&packet(0, &[]))?;
    let framed = read_frame(&mut socket)?;
    let mut body = framed.as_slice();
    let id = read_varint(&mut body)?;
    if id != 0 {
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
    let echo = read_varint(&mut body)?;
    if echo != 1 || body != token.to_be_bytes() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid Status Pong",
        ));
    }
    Ok((json, started.elapsed()))
}

impl Server {
    fn start() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "industrialrust-reg191-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create-only isolated world");
        let names: Vec<String> = (0..PLAYERS).map(|i| format!("Craft{i}")).collect();
        let whitelist: Vec<Value> = names
            .iter()
            .map(|name| serde_json::json!({"name": name}))
            .collect();
        fs::write(
            root.join("whitelist.json"),
            serde_json::to_vec(&whitelist).unwrap(),
        )
        .unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let stderr = root.join("server-stderr.log");
        let mut child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .args(["--bind", &format!("127.0.0.1:{port}"), "--world"])
            .arg(root.join("flat.irw"))
            .arg("--whitelist")
            .arg(root.join("whitelist.json"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::from(File::create(&stderr).unwrap()))
            .spawn()
            .expect("launch native Rust server");
        let stdin = child.stdin.take().unwrap();
        let mut server = Self {
            child,
            stdin,
            root,
            stderr,
            port,
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if server.child.try_wait().unwrap().is_some() {
                panic!("server died before Protocol 5 readiness");
            }
            if status(port).is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "server readiness timed out");
            thread::sleep(Duration::from_millis(50));
        }
        server
    }
}

fn percentile(values: &[f64], pct: f64) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let index = ((pct / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[index.saturating_sub(1).min(sorted.len() - 1)]
}

#[test]
fn reg191_eight_players_craft_with_measured_tps_and_item_conservation() {
    let server = Server::start();
    let samples: AgeSamples = Arc::new(Mutex::new(Vec::new()));
    let ready = Arc::new(AtomicUsize::new(0));
    let go = Arc::new(AtomicBool::new(false));

    // Join everyone first so the load phase starts from a full server.
    let mut clients = Vec::new();
    for i in 0..PLAYERS {
        let sampler = if i == 0 { Some(samples.clone()) } else { None };
        clients.push(Client::join(server.port, &format!("Craft{i}"), sampler));
    }

    let mut workers = Vec::new();
    for (i, mut client) in clients.into_iter().enumerate() {
        let ready = ready.clone();
        let go = go.clone();
        workers.push(thread::spawn(move || {
            ready.fetch_add(1, Ordering::SeqCst);
            while !go.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(2));
            }
            let (bench_x, bench_z) = BENCHES[i];
            let mut action: i16 = 1;
            client.make_and_place_workbench(bench_x, bench_z, &mut action);
            for craft in 0..CRAFTS_PER_PLAYER {
                client.creative_set(36, PLANKS, 2);
                let window = client.open_workbench(bench_x, bench_z);
                let mut click = |client: &mut Client, slot: i16, button: u8| {
                    let items = client.click(window, slot, action, button);
                    action += 1;
                    items
                };
                click(&mut client, 37, 0); // lift the two planks to the cursor
                click(&mut client, 1, 1); // one plank into grid slot 1
                let items = click(&mut client, 4, 1); // one plank into grid slot 4
                assert_eq!(
                    items[0],
                    Some((STICKS, 4)),
                    "craft {} must offer exactly 4 sticks in the result slot",
                    craft + 1
                );
                assert_eq!(items[1], Some((PLANKS, 1)));
                assert_eq!(items[4], Some((PLANKS, 1)));
                click(&mut client, 0, 0); // take the sticks to the cursor
                                          // Sticks stack to 64 like in vanilla, so every 16 crafts
                                          // fill one hotbar slot and move to the next.
                let deposit_slot = 38 + (craft / 16) as i16;
                let expected = (4 * ((craft % 16) + 1)) as u8;
                let items = click(&mut client, deposit_slot, 0);
                assert_eq!(
                    items[deposit_slot as usize],
                    Some((STICKS, expected)),
                    "stick conservation after craft {}",
                    craft + 1
                );
                assert!(items[0].is_none() && items[1].is_none() && items[4].is_none());
                assert!(items[37].is_none(), "both planks must be consumed");
                client.close_window(window);
            }
        }));
    }

    // Measure Status latency over TCP while the crafting load runs.
    let latencies = Arc::new(Mutex::new(Vec::new()));
    let measuring = Arc::new(AtomicBool::new(true));
    let latency_thread = {
        let latencies = latencies.clone();
        let measuring = measuring.clone();
        let port = server.port;
        thread::spawn(move || {
            while measuring.load(Ordering::SeqCst) {
                if let Ok((_, rtt)) = status(port) {
                    latencies.lock().unwrap().push(rtt.as_secs_f64() * 1000.0);
                }
                thread::sleep(Duration::from_millis(200));
            }
        })
    };

    let load_started = Instant::now();
    while ready.load(Ordering::SeqCst) < PLAYERS {
        thread::sleep(Duration::from_millis(2));
    }
    // Discard Time Updates that buffered in Craft0's socket while the
    // other players were still joining; from here on its reader drains
    // in real time, so sample timestamps match their world ages.
    samples.lock().unwrap().clear();
    go.store(true, Ordering::SeqCst);
    for worker in workers {
        worker.join().expect("crafting worker panicked");
    }
    let load_seconds = load_started.elapsed().as_secs_f64();
    measuring.store(false, Ordering::SeqCst);
    latency_thread.join().unwrap();

    let samples = samples.lock().unwrap();
    assert!(
        samples.len() >= 6,
        "too few Time Update samples: {}",
        samples.len()
    );
    // Pairwise rates between consecutive Time Updates. A pair's rate is
    // (age delta) / (arrival delta), which stays truthful even when the
    // sampler thread is descheduled and packets arrive in bursts: both
    // endpoints shift together. Pairs shorter than 0.5s are burst noise.
    let mut rates: Vec<f64> = Vec::new();
    let mut total_ticks: i64 = 0;
    let mut total_span = 0.0f64;
    for pair in samples.windows(2) {
        let dt = pair[1].0.duration_since(pair[0].0).as_secs_f64();
        let da = pair[1].1 - pair[0].1;
        if dt >= 0.5 && da >= 0 {
            rates.push(da as f64 / dt);
            total_ticks += da;
            total_span += dt;
        }
    }
    assert!(
        rates.len() >= 4,
        "too few usable Time Update pairs: {}",
        rates.len()
    );
    assert!(
        total_span >= 3.0,
        "measured span {total_span:.1}s is too short"
    );
    let overall_tps = total_ticks as f64 / total_span;
    let bucket_p95 = percentile(&rates, 95.0);
    let bucket_min = rates.iter().cloned().fold(f64::INFINITY, f64::min);
    let latencies = latencies.lock().unwrap();
    assert!(!latencies.is_empty(), "no Status latency samples");
    let latency_p95 = percentile(&latencies, 95.0);
    let latency_mean = latencies.iter().sum::<f64>() / latencies.len() as f64;

    println!(
        "REG-191 measured: players={PLAYERS} crafts_per_player={CRAFTS_PER_PLAYER} \
         load_seconds={load_seconds:.1} overall_tps={overall_tps:.2} \
         tps_pair_p95={bucket_p95:.0} tps_pair_min={bucket_min:.0} \
         status_rtt_mean_ms={latency_mean:.1} status_rtt_p95_ms={latency_p95:.1} \
         status_samples={}",
        latencies.len()
    );
    assert!(
        overall_tps >= 17.5,
        "overall TPS {overall_tps:.2} collapsed under 8-player crafting load"
    );
    assert!(
        latency_p95 < 1000.0,
        "Status P95 latency {latency_p95:.1}ms is unresponsive under load"
    );
    // Graceful console shutdown; Drop remains as the kill fallback.
    let mut server = server;
    let _ = writeln!(server.stdin, "/stop");
    let _ = server.stdin.flush();
    let deadline = Instant::now() + Duration::from_secs(10);
    while server.child.try_wait().unwrap().is_none() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(100));
    }
    drop(server);
}
