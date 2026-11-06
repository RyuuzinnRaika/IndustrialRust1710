//! REG-121 / #757: Status JSON stays coherent during login/disconnect churn.
//! Only an isolated temporary world and loopback connections are used.
use legacy_protocol::{read_frame, read_varint, write_frame, write_string, write_varint, Cursor};
use serde_json::Value;
use std::fs::{self, File};
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Server {
    process: Child,
    root: PathBuf,
    stderr_path: PathBuf,
    port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
        if thread::panicking() {
            let log = fs::read(&self.stderr_path).unwrap_or_default();
            let start = log.len().saturating_sub(8 * 1024);
            eprintln!(
                "REG-121 server diagnostic tail: {}",
                String::from_utf8_lossy(&log[start..])
            );
        }
        let _ = fs::remove_dir_all(&self.root);
        let _ = fs::remove_file(&self.stderr_path);
    }
}

fn connect(port: u16) -> TcpStream {
    let socket = TcpStream::connect(("127.0.0.1", port)).unwrap();
    socket.set_read_timeout(Some(Duration::from_secs(8))).unwrap();
    socket.set_write_timeout(Some(Duration::from_secs(8))).unwrap();
    socket
}

impl Server {
    fn start() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "ir1710-reg121-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create-only isolated test world");
        fs::write(
            root.join("whitelist.json"),
            r#"[{"name":"RustTester"},{"name":"TwinAlpha"}]"#,
        )
        .unwrap();

        let reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);
        let stderr_path = root.with_extension("server-stderr.log");
        let log = File::create(&stderr_path).unwrap();
        let process = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .args(["--bind", &format!("127.0.0.1:{port}"), "--world"])
            .arg(root.join("flat.irw"))
            .arg("--whitelist")
            .arg(root.join("whitelist.json"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::from(log))
            .spawn()
            .expect("start isolated vanilla-server");

        let mut server = Self {
            process,
            root,
            stderr_path,
            port,
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                assert!(server.process.try_wait().unwrap().is_none());
                return server;
            }
            assert!(
                server.process.try_wait().unwrap().is_none(),
                "server exited before binding TCP"
            );
            assert!(Instant::now() < deadline, "server readiness timed out");
            thread::sleep(Duration::from_millis(50));
        }
    }
}

fn frame(id: u32, body: &[u8]) -> Vec<u8> {
    let mut packet = Vec::new();
    write_varint(id, &mut packet);
    packet.extend_from_slice(body);
    let mut output = Vec::new();
    write_frame(&mut output, &packet).unwrap();
    output
}

fn handshake(port: u16, state: u32) -> Vec<u8> {
    let mut body = Vec::new();
    write_varint(5, &mut body); // Minecraft Java Edition 1.7.10, Protocol 5.
    write_string("localhost", &mut body).unwrap();
    body.extend_from_slice(&port.to_be_bytes());
    write_varint(state, &mut body);
    frame(0, &body)
}

fn receive(socket: &mut TcpStream) -> (i32, Vec<u8>) {
    let bytes = read_frame(socket).unwrap();
    let mut remaining = bytes.as_slice();
    let id = read_varint(&mut remaining).unwrap();
    (id, remaining.to_vec())
}

fn status(port: u16) -> Value {
    let mut client = connect(port);
    client.write_all(&handshake(port, 1)).unwrap();
    client.write_all(&frame(0, &[])).unwrap();
    let (id, payload) = receive(&mut client);
    assert_eq!(id, 0, "Status Response packet ID");
    let mut parser = Cursor::new(&payload);
    let response: Value = serde_json::from_str(parser.read_string(65536).unwrap()).unwrap();
    parser.finish().unwrap();
    response
}

fn join(port: u16, username: &str) -> TcpStream {
    let mut client = connect(port);
    client.write_all(&handshake(port, 2)).unwrap();
    let mut name = Vec::new();
    write_string(username, &mut name).unwrap();
    client.write_all(&frame(0, &name)).unwrap();
    let mut logged_in = false;
    // Consume the initial 1.7.10 Play/chunk packets so the native server
    // does not block sending the player's initial view.
    for _ in 0..160 {
        let (id, payload) = receive(&mut client);
        if id == 2 && !logged_in {
            let mut parser = Cursor::new(&payload);
            assert_eq!(parser.read_string(40).unwrap().len(), 36);
            assert_eq!(parser.read_string(16).unwrap(), username);
            parser.finish().unwrap();
            logged_in = true;
        } else if id == 0 && payload.len() == 4 {
            client.write_all(&frame(0, &payload)).unwrap();
        } else if id == 8 {
            assert!(logged_in, "Play position received before Login Success");
            return client;
        }
    }
    panic!("no complete Play join after 160 Protocol 5 packets");
}

fn serialized_status(port: u16, gate: &Mutex<()>) -> Value {
    // MAX_ACTIVE_PER_IP is four. Two logged-in players and two concurrent
    // Status probes would exhaust the admission budget at connection churn.
    // Serialize only Status probes; login/disconnect still race against them.
    let _guard = gate.lock().unwrap_or_else(|e| e.into_inner());
    status(port)
}

fn wait_for_online(port: u16, target: u64, gate: &Mutex<()>) {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let state = serialized_status(port, gate);
        if state["players"]["online"].as_u64() == Some(target) {
            return;
        }
        assert!(Instant::now() < deadline, "online count failed to reach {target}");
        thread::sleep(Duration::from_millis(80));
    }
}

#[test]
fn reg121_status_json_is_coherent_during_player_churn() {
    let server = Server::start();
    let baseline = status(server.port);
    assert_eq!(baseline["version"]["protocol"], 5);
    assert_eq!(baseline["players"]["online"], 0);
    assert_eq!(baseline["players"]["max"], 8);
    let description = baseline["description"].clone();
    assert!(
        description["text"].as_str().is_some_and(|text| !text.is_empty()),
        "Status MOTD must be a nonempty JSON text component"
    );

    let gate = Arc::new(Mutex::new(()));
    let running = Arc::new(AtomicBool::new(true));
    let monitor_running = Arc::clone(&running);
    let monitor_gate = Arc::clone(&gate);
    let port = server.port;
    let monitor = thread::spawn(move || {
        let mut samples = 0usize;
        while monitor_running.load(Ordering::Relaxed) || samples < 12 {
            let sample = serialized_status(port, &monitor_gate);
            assert_eq!(sample["version"]["protocol"], 5);
            assert_eq!(sample["players"]["max"], 8);
            let online = sample["players"]["online"]
                .as_u64()
                .expect("Status online player count must be nonnegative integer");
            assert!(online <= 2, "Status count exceeded admitted players");
            assert_eq!(sample["description"], description);
            samples += 1;
            assert!(samples <= 300, "concurrent Status monitor did not terminate");
            thread::sleep(Duration::from_millis(80));
        }
        samples
    });

    let alpha = join(server.port, "RustTester");
    wait_for_online(server.port, 1, &gate);
    let beta = join(server.port, "TwinAlpha");
    wait_for_online(server.port, 2, &gate);
    drop(beta);
    wait_for_online(server.port, 1, &gate);
    drop(alpha);
    wait_for_online(server.port, 0, &gate);
    let reconnect = join(server.port, "RustTester");
    wait_for_online(server.port, 1, &gate);
    drop(reconnect);
    wait_for_online(server.port, 0, &gate);

    running.store(false, Ordering::Relaxed);
    assert!(monitor.join().unwrap() >= 12);

    // A malformed Status Request must be rejected without leaving phantom
    // players or preventing a later, valid Status request.
    let mut invalid = connect(server.port);
    let mut frames = handshake(server.port, 1);
    frames.extend_from_slice(&frame(0, &[1]));
    invalid.write_all(&frames).unwrap();
    assert!(read_frame(&mut invalid).is_err());
    drop(invalid);
    assert_eq!(status(server.port)["players"]["online"], 0);
}
