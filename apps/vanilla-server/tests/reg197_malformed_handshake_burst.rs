//! REG-197 / #833: malformed Handshake bursts must not exhaust the server,
//! grow unauthenticated stderr without bound, or prevent later Status probes.
//!
//! This test launches only the native Rust server with a fresh, isolated world.
//! Linux additionally reports process CPU ticks and checks for an FD leak.
//! It does not substitute for an external 24-hour soak or real 1.7.10 client.
use legacy_protocol::{read_frame, read_varint, write_frame, write_string, write_varint, Cursor};
use serde_json::Value;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const BURST: usize = 96;
const PARALLEL_WAVES: usize = 4;
const PARALLEL_CLIENTS: usize = 8;
const SECRET_MARKER: &str = "REG197_PRIVATE_INPUT_MUST_NOT_BE_LOGGED";

struct Server {
    child: Child,
    root: PathBuf,
    log: PathBuf,
    port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if thread::panicking() {
            if let Ok(text) = fs::read_to_string(&self.log) {
                eprintln!("REG-197 server stderr:\n{text}");
            }
        } else {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

impl Server {
    fn start() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "industrialrust-reg197-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create-only scratch directory");
        fs::write(
            root.join("whitelist.json"),
            r#"[{"name":"RustTester"}]"#,
        )
        .expect("write isolated whitelist");
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("reserve loopback port");
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let log = root.join("stderr.log");
        let stderr = File::create(&log).expect("open isolated stderr");
        let child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .args(["--bind", &format!("127.0.0.1:{port}"), "--world"])
            .arg(root.join("flat.irw"))
            .arg("--whitelist")
            .arg(root.join("whitelist.json"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr))
            .spawn()
            .expect("launch Rust-only server");
        let mut server = Self {
            child,
            root,
            log,
            port,
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = server.child.try_wait().expect("child status") {
                panic!("server exited before readiness: {status}");
            }
            if status_probe(port).is_ok() {
                return server;
            }
            assert!(Instant::now() < deadline, "Protocol 5 Status readiness timed out");
            thread::sleep(Duration::from_millis(50));
        }
    }
}

fn connect(port: u16) -> io::Result<TcpStream> {
    let socket = TcpStream::connect(("127.0.0.1", port))?;
    socket.set_read_timeout(Some(Duration::from_secs(3)))?;
    socket.set_write_timeout(Some(Duration::from_secs(3)))?;
    Ok(socket)
}

fn send_packet(socket: &mut TcpStream, id: u32, body: &[u8]) -> io::Result<()> {
    let mut packet = Vec::new();
    write_varint(id, &mut packet);
    packet.extend_from_slice(body);
    let mut frame = Vec::new();
    write_frame(&mut frame, &packet)?;
    socket.write_all(&frame)
}

fn status_probe(port: u16) -> io::Result<()> {
    let mut socket = connect(port)?;
    let mut handshake = Vec::new();
    write_varint(5, &mut handshake);
    write_string("localhost", &mut handshake)?;
    handshake.extend_from_slice(&port.to_be_bytes());
    write_varint(1, &mut handshake);
    send_packet(&mut socket, 0, &handshake)?;
    send_packet(&mut socket, 0, &[])?;

    let frame = read_frame(&mut socket)?;
    let mut body = frame.as_slice();
    if read_varint(&mut body)? != 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "missing status response"));
    }
    let mut parser = Cursor::new(body);
    let response: Value = serde_json::from_str(parser.read_string(32768)?)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    parser.finish()?;
    if response["version"]["protocol"] != 5 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "wrong protocol"));
    }

    let token: i64 = 0x1234_5678_7654_3210;
    send_packet(&mut socket, 1, &token.to_be_bytes())?;
    let pong = read_frame(&mut socket)?;
    let mut payload = pong.as_slice();
    if read_varint(&mut payload)? != 1 || payload != token.to_be_bytes() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "wrong ping echo"));
    }
    Ok(())
}

fn malformed_handshake(port: u16) {
    let mut socket = connect(port).expect("connect malformed client");
    // A nonzero packet ID is not a Handshake. The appended attacker-supplied
    // string must never be reflected into server logs.
    if let Err(error) = send_packet(&mut socket, 127, SECRET_MARKER.as_bytes()) {
        // More than four simultaneous unauthenticated clients from one IP
        // are deliberately rejected before their packets can be read.
        assert!(
            matches!(
                error.kind(),
                io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::BrokenPipe
            ),
            "unexpected malformed send failure: {error}"
        );
        return;
    }
    // A concurrent admission rejection may already have closed the socket.
    let _ = socket.shutdown(Shutdown::Write);
    let mut one = [0u8; 1];
    match socket.read(&mut one) {
        Ok(0) => {}
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
            ) => {}
        other => panic!("invalid Handshake must be rejected promptly: {other:?}"),
    }
}

#[cfg(target_os = "linux")]
fn proc_fd_count(pid: u32) -> Option<usize> {
    Some(fs::read_dir(format!("/proc/{pid}/fd")).ok()?.count())
}

#[cfg(not(target_os = "linux"))]
fn proc_fd_count(_pid: u32) -> Option<usize> {
    None
}

#[cfg(target_os = "linux")]
fn proc_cpu_ticks(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = stat.rsplit_once(") ")?.1;
    let fields: Vec<_> = after_comm.split_whitespace().collect();
    // Fields 14 and 15 (utime/stime), indexed relative to field 3 (state).
    Some(fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?)
}

#[cfg(not(target_os = "linux"))]
fn proc_cpu_ticks(_pid: u32) -> Option<u64> {
    None
}

#[test]
fn reg197_malformed_burst_has_bounded_logs_and_recovers_status() {
    let mut server = Server::start();
    let pid = server.child.id();
    let fd_before = proc_fd_count(pid);
    let cpu_before = proc_cpu_ticks(pid);
    let started = Instant::now();

    for i in 0..BURST {
        malformed_handshake(server.port);
        if i % 12 == 11 {
            status_probe(server.port).expect("valid Status must survive invalid traffic");
            assert!(
                server.child.try_wait().expect("server status").is_none(),
                "server exited during malformed burst"
            );
        }
    }
    // Exercise admission saturation as well as sequential malformed frames.
    // A rejected excess connection may fail its write; it must not panic the
    // server or strand one of the four per-IP slots.
    for _ in 0..PARALLEL_WAVES {
        let port = server.port;
        let clients: Vec<_> = (0..PARALLEL_CLIENTS)
            .map(|_| thread::spawn(move || malformed_handshake(port)))
            .collect();
        for client in clients {
            client.join().expect("parallel malformed client panicked");
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if status_probe(port).is_ok() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "valid Status failed to recover after concurrent admission burst"
            );
            thread::sleep(Duration::from_millis(25));
        }
        assert!(
            server.child.try_wait().expect("server status").is_none(),
            "server exited under parallel malformed Handshakes"
        );
    }
    status_probe(server.port).expect("normal Protocol 5 status/pong after burst");

    let log = fs::read_to_string(&server.log).expect("read isolated stderr");
    let malformed_lines = log
        .lines()
        .filter(|line| line.contains("category=invalid_packet_or_state"))
        .count();
    assert!(malformed_lines > 0, "malformed packet rejection was not observed");
    assert!(
        malformed_lines <= 16,
        "invalid-packet log budget exceeded: {malformed_lines} lines"
    );
    assert!(
        !log.contains(SECRET_MARKER),
        "untrusted packet bytes escaped into server diagnostics"
    );

    let fd_after = proc_fd_count(pid);
    if let (Some(before), Some(after)) = (fd_before, fd_after) {
        assert!(
            after <= before + 8,
            "server FD count grew across bounded malformed burst: {before} -> {after}"
        );
    } else {
        eprintln!("REG-197 FD comparison NOT_RUN (procfs unavailable)");
    }
    let cpu_after = proc_cpu_ticks(pid);
    if let (Some(before), Some(after)) = (cpu_before, cpu_after) {
        eprintln!(
            "REG-197 CPU delta={} scheduler ticks over {} malformed connections",
            after.saturating_sub(before),
            BURST
        );
    } else {
        eprintln!("REG-197 CPU sample NOT_RUN (procfs unavailable)");
    }
    eprintln!(
        "REG-197 elapsed_ms={} malformed={} parallel={} logged={}",
        started.elapsed().as_millis(),
        BURST,
        PARALLEL_WAVES * PARALLEL_CLIENTS,
        malformed_lines
    );
}
