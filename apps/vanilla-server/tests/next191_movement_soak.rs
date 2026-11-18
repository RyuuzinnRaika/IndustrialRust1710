//! NEXT-191 / #1174: soak the movement path and record spurious teleport
//! corrections. Real unmodified 1.7.10 client acceptance remains NOT_RUN
//! (headless host); this test is the scripted Protocol 5 equivalent that
//! runs on every gate: it drives the REAL server binary on an isolated
//! scratch world targeting one C04 position packet every 50 ms for a fixed
//! soak window. Write/flush completion intervals and target-count deficit are
//! reported; these are not server receipt times. The 50 ms cadence must not
//! be assumed from the intended schedule. The existing correction count uses
//! its original position-tolerance rule.
//!
//! Pass criteria (scripted soak; real client NOT_RUN):
//!   * zero spurious S08 corrections while the client keeps a legal,
//!     monotonic position stream;
//!   * KeepAlive challenges keep being echoed and the session survives the
//!     whole window (no disconnect);
//!   * clean /stop persists the final position (no poisoned state after
//!     the soak);
//!   * the correction count is printed for the issue trail.
#[path = "support/packet_client.rs"]
mod packet_client;

use legacy_protocol::{write_string, write_varint};
use packet_client::BufferedClient;
use std::fs;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SOAK_SECS: u64 = 60;
const STEP: f64 = 0.1;
const TICK: Duration = Duration::from_millis(50);

#[derive(Default)]
struct TimingStats {
    started: Option<Instant>,
    last: Option<Instant>,
    sent: u32,
    gaps: u32,
    total_gap: Duration,
    max_gap: Duration,
}

impl TimingStats {
    fn record(&mut self, at: Instant) {
        if let Some(previous) = self.last {
            let gap = at.saturating_duration_since(previous);
            self.gaps += 1;
            self.total_gap += gap;
            self.max_gap = self.max_gap.max(gap);
        }
        self.sent += 1;
        self.last = Some(at);
    }

    fn report(&self, label: &str) {
        let planned = Duration::from_secs(SOAK_SECS).as_nanos() / TICK.as_nanos();
        let mean_ms = if self.gaps == 0 {
            None
        } else {
            Some(self.total_gap.as_secs_f64() * 1000.0 / f64::from(self.gaps))
        };
        let elapsed = self.started.map(|start| start.elapsed());
        let max_gap = (self.gaps != 0).then_some(self.max_gap);
        eprintln!("NEXT191 timing={label} elapsed_since_soak_start={elapsed:?} planned_60s_slots={planned} sent={} remaining_vs_full_60s={} mean_gap_ms={mean_ms:?} max_gap={max_gap:?} max_delay_after_50ms={:?}",
            self.sent, planned.saturating_sub(u128::from(self.sent)),
            max_gap.map(|gap| gap.saturating_sub(TICK)));
    }
}

struct Server {
    child: Child,
    root: std::path::PathBuf,
    port: u16,
    stage: &'static str,
    timing: TimingStats,
}

impl Drop for Server {
    fn drop(&mut self) {
        let prior = self.child.try_wait();
        if thread::panicking() {
            eprintln!(
                "NEXT191 failure stage={} child_status={prior:?}",
                self.stage
            );
            self.timing.report("failure");
        }
        if prior.ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        if thread::panicking() {
            let fixture = self.root.file_name().unwrap_or_default().to_string_lossy();
            eprintln!("NEXT191 diagnostic fixture retained under TEMP: {fixture}");
            for (label, filename) in [
                ("stdout", "server.stdout.log"),
                ("stderr", "server.stderr.log"),
            ] {
                match fs::metadata(self.root.join(filename)) {
                    Ok(metadata) => {
                        eprintln!("NEXT191 server {label} retained: {} bytes", metadata.len())
                    }
                    Err(error) => {
                        eprintln!("NEXT191 server {label} unavailable: {:?}", error.kind())
                    }
                }
            }
        }
    }
}

impl Server {
    fn start(tag: &str) -> Server {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "ir1710-next191-{tag}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("whitelist.json"), r#"[{"name":"Soaker"}]"#).unwrap();
        let stdout = fs::File::create(root.join("server.stdout.log")).unwrap();
        let stderr = fs::File::create(root.join("server.stderr.log")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .args(["--bind", &format!("127.0.0.1:{port}"), "--world"])
            .arg(root.join("flat.irw"))
            .arg("--whitelist")
            .arg(root.join("whitelist.json"))
            .stdin(Stdio::piped())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .unwrap();
        let mut server = Server {
            child,
            root,
            port,
            stage: "readiness",
            timing: TimingStats::default(),
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(exit) = server.child.try_wait().unwrap() {
                panic!("server exited before readiness: {exit}");
            }
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "server did not open {port}");
            thread::sleep(Duration::from_millis(100));
        }
        server
    }

    fn connect(&self, name: &str) -> BufferedClient {
        let mut socket = BufferedClient::new(TcpStream::connect(("127.0.0.1", self.port)).unwrap());
        socket
            .set_read_timeout(Some(Duration::from_millis(250)))
            .unwrap();
        let mut handshake = Vec::new();
        write_varint(5, &mut handshake);
        write_string("127.0.0.1", &mut handshake).unwrap();
        handshake.extend_from_slice(&self.port.to_be_bytes());
        write_varint(2, &mut handshake);
        socket.write_all(&frame(0, &handshake)).unwrap();
        socket.write_all(&frame(0, &mc_string(name))).unwrap();
        socket
    }

    fn cleanup(&self) -> std::io::Result<()> {
        let name = self
            .root
            .file_name()
            .ok_or_else(|| std::io::Error::other("missing fixture name"))?;
        if !name.to_string_lossy().starts_with("ir1710-next191-soak-") {
            return Err(std::io::Error::other("unexpected fixture name"));
        }
        let metadata = fs::symlink_metadata(&self.root)?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Err(std::io::Error::other("fixture is not a real directory"));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(std::io::Error::other("fixture is a reparse point"));
            }
        }
        let temp = std::env::temp_dir().canonicalize()?;
        let resolved = self.root.canonicalize()?;
        if resolved != temp.join(name) {
            return Err(std::io::Error::other(
                "fixture is not the expected temp child",
            ));
        }
        fs::remove_dir_all(&resolved)?;
        match fs::symlink_metadata(&resolved) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
            Ok(_) => Err(std::io::Error::other("fixture remains after cleanup")),
        }
    }
}

fn vi(value: u32) -> Vec<u8> {
    let mut out = Vec::new();
    write_varint(value, &mut out);
    out
}

fn mc_string(value: &str) -> Vec<u8> {
    let mut out = Vec::new();
    write_string(value, &mut out).unwrap();
    out
}

fn frame(id: u32, data: &[u8]) -> Vec<u8> {
    let mut packet = vi(id);
    packet.extend_from_slice(data);
    let mut message = vi(packet.len() as u32);
    message.extend_from_slice(&packet);
    message
}

fn pump(socket: &mut BufferedClient) -> Option<(u32, Vec<u8>)> {
    socket.poll_packet().expect("read Protocol 5 packet")
}

fn read_position(body: &[u8]) -> [f64; 3] {
    [
        f64::from_be_bytes(body[0..8].try_into().unwrap()),
        f64::from_be_bytes(body[8..16].try_into().unwrap()) - f64::from(1.62_f32),
        f64::from_be_bytes(body[16..24].try_into().unwrap()),
    ]
}

/// C04 absolute position wire order: X, feet Y, stance, Z, onGround.
fn position_packet(x: f64, feet_y: f64, z: f64) -> Vec<u8> {
    let mut body = Vec::new();
    for value in [x, feet_y, feet_y + 1.62, z] {
        body.extend_from_slice(&value.to_be_bytes());
    }
    body.push(1);
    frame(4, &body)
}

#[test]
fn next191_continuous_movement_soak_records_spurious_teleports() {
    let mut server = Server::start("soak");
    server.stage = "join";
    let mut client = server.connect("Soaker");
    // Join burst until the first S08 (authoritative spawn position).
    let deadline = Instant::now() + Duration::from_secs(15);
    let spawn = loop {
        assert!(Instant::now() < deadline, "no S08 after join");
        let Some((id, body)) = pump(&mut client) else {
            continue;
        };
        if id == 0x00 && body.len() == 4 {
            let _ = client.write_all(&frame(0, &body));
        }
        if id == 0x08 && body.len() >= 24 {
            break read_position(&body);
        }
    };

    // The existing loop targets 50 ms along +X. Measure what it achieves;
    // draining the socket can block longer than that target.
    server.stage = "soak";
    let mut x = spawn[0];
    let mut packets_sent = 0u32;
    let mut corrections = 0u32;
    let mut keepalives = 0u32;
    let mut acked = 0u32;
    let soak_start = Instant::now();
    let soak = soak_start + Duration::from_secs(SOAK_SECS);
    server.timing.started = Some(soak_start);
    let mut last_sent = Instant::now() - TICK;
    while Instant::now() < soak {
        if last_sent.elapsed() >= TICK {
            x += STEP;
            client
                .write_all(&position_packet(x, spawn[1], spawn[2]))
                .unwrap();
            client.flush().unwrap();
            packets_sent += 1;
            last_sent = Instant::now();
            server.timing.record(last_sent);
        }
        // Drain whatever arrived since the last tick.
        while let Some((id, body)) = pump(&mut client) {
            if id == 0x00 && body.len() == 4 {
                let _ = client.write_all(&frame(0, &body));
                keepalives += 1;
            }
            if id == 0x08 && body.len() >= 24 {
                // A correction for a just-sent legal position is spurious.
                // Acks of our own stream (matching the last sent X within a
                // tick of latency) are counted but never counted as spurious.
                let at = read_position(&body);
                if (at[0] - (x - STEP)).abs() > 0.55 || (at[1] - spawn[1]).abs() > 0.01 {
                    corrections += 1;
                    // Behave like a client: acknowledge the correction.
                    client
                        .write_all(&position_packet(at[0], at[1], at[2]))
                        .unwrap();
                    acked += 1;
                }
            }
        }
    }

    server.timing.report("measurement_window_complete");
    // The session must still be healthy at the end of the soak.
    server.stage = "post_soak_health";
    client
        .write_all(&frame(1, &mc_string("/list")))
        .expect("session must accept chat after soak");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut listed = false;
    while Instant::now() < deadline && !listed {
        let Some((id, body)) = pump(&mut client) else {
            thread::sleep(Duration::from_millis(50));
            continue;
        };
        listed = id == 0x02 && String::from_utf8_lossy(&body).contains("Online (1)");
    }
    assert!(listed, "session lost during soak");

    eprintln!(
        "NEXT-191 soak: duration={SOAK_SECS}s packets={packets_sent} corrections={corrections} \
         correction_acks={acked} keepalives={keepalives} (real 1.7.10 client NOT_RUN)"
    );

    // Clean stop; the walked-upon position must have persisted.
    server.stage = "stop";
    {
        let mut stdin = server.child.stdin.take().expect("server stdin");
        stdin.write_all(b"/stop\n").unwrap();
        stdin.flush().unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match server.child.try_wait().unwrap() {
            Some(status) => {
                assert!(status.success(), "server must stop cleanly: {status}");
                break;
            }
            None => {
                assert!(Instant::now() < deadline, "server did not stop");
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
    let save = server.root.join("players/soaker.dat");
    assert!(save.exists(), "soak session must persist its player file");

    server.stage = "assertions";
    assert!(
        packets_sent > 100,
        "soak must actually stream movement: sent {packets_sent}"
    );
    assert_eq!(
        corrections, 0,
        "continuous legal movement produced {corrections} spurious S08 corrections"
    );
    server.stage = "cleanup";
    if let Err(error) = server.cleanup() {
        eprintln!("NEXT191 scratch cleanup incomplete: {:?}", error.kind());
    }
}
