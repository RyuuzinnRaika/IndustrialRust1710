//! REG-122 / #758: Protocol 5 Handshake host and advertised port boundaries.
//! These are loopback TCP fixtures, not a real public DNS SRV lookup.
//! Never read or overwrite a user's Minecraft world.
use legacy_protocol::{read_frame, read_varint, write_frame, write_string, write_varint, Cursor};
use serde_json::Value;
use std::fs::{self, File};
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct TestServer {
    process: Child,
    root: PathBuf,
    stderr: PathBuf,
    port: u16,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
        if thread::panicking() {
            let log = fs::read(&self.stderr).unwrap_or_default();
            let start = log.len().saturating_sub(8 * 1024);
            eprintln!(
                "REG-122 server diagnostic tail: {}",
                String::from_utf8_lossy(&log[start..])
            );
        }
        let _ = fs::remove_dir_all(&self.root);
        let _ = fs::remove_file(&self.stderr);
    }
}

impl TestServer {
    fn start() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "ir1710-reg122-{}-{stamp}",
            std::process::id()
        ));
        // Create-only isolation: fail if this path already exists.
        fs::create_dir(&root).unwrap();
        let whitelist = root.join("whitelist.json");
        fs::write(
            &whitelist,
            r#"[{"name":"RustTester"},{"name":"TwinAlpha"}]"#,
        )
        .unwrap();

        let port_reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = port_reservation.local_addr().unwrap().port();
        drop(port_reservation);

        let stderr = root.with_extension("server-stderr.log");
        let log_file = File::create(&stderr).unwrap();
        let process = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .args(["--bind", &format!("127.0.0.1:{port}"), "--world"])
            .arg(root.join("flat.irw"))
            .arg("--whitelist")
            .arg(&whitelist)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::from(log_file))
            .spawn()
            .expect("start isolated Protocol 5 fixture");

        let mut server = Self {
            process,
            root,
            stderr,
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
                "isolated vanilla-server exited before binding"
            );
            assert!(Instant::now() < deadline, "vanilla-server startup timed out");
            thread::sleep(Duration::from_millis(40));
        }
    }

    fn connect(&self) -> TcpStream {
        let stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(8)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(8)))
            .unwrap();
        stream
    }
}

fn packet(id: u32, body: &[u8]) -> Vec<u8> {
    let mut raw = Vec::new();
    write_varint(id, &mut raw);
    raw.extend_from_slice(body);
    raw
}

fn handshake(host: &str, advertised_port: u16, state: u32) -> Vec<u8> {
    let mut body = Vec::new();
    write_varint(5, &mut body); // Minecraft Java Edition 1.7.10, Protocol 5.
    write_string(host, &mut body).unwrap();
    body.extend_from_slice(&advertised_port.to_be_bytes());
    write_varint(state, &mut body);
    packet(0, &body)
}

fn transmit(socket: &mut TcpStream, id: u32, body: &[u8]) {
    write_frame(socket, &packet(id, body)).unwrap();
}

fn next_packet(socket: &mut TcpStream) -> (u32, Vec<u8>) {
    let frame = read_frame(socket).unwrap();
    let mut raw = frame.as_slice();
    let id = read_varint(&mut raw).unwrap();
    (id, raw.to_vec())
}

fn status(server: &TestServer, host: &str, advertised_port: u16) -> Value {
    let mut client = server.connect();
    write_frame(&mut client, &handshake(host, advertised_port, 1)).unwrap();
    transmit(&mut client, 0, &[]);
    let (id, response) = next_packet(&mut client);
    assert_eq!(id, 0, "Status response packet ID");
    let mut cursor = Cursor::new(&response);
    let value: Value = serde_json::from_str(cursor.read_string(65536).unwrap()).unwrap();
    cursor.finish().unwrap();
    value
}

fn login(server: &TestServer, host: &str, advertised_port: u16, username: &str) {
    let mut client = server.connect();
    write_frame(&mut client, &handshake(host, advertised_port, 2)).unwrap();
    let mut name = Vec::new();
    write_string(username, &mut name).unwrap();
    transmit(&mut client, 0, &name);
    let (id, body) = next_packet(&mut client);
    assert_eq!(id, 2, "Login Success required before Play");
    let mut cursor = Cursor::new(&body);
    assert_eq!(cursor.read_string(40).unwrap().len(), 36);
    assert_eq!(cursor.read_string(16).unwrap(), username);
    cursor.finish().unwrap();
}

#[test]
fn reg122_srv_style_handshake_preserves_status_and_login_behavior() {
    let server = TestServer::start();
    let direct = status(&server, "localhost", server.port);
    let srv = status(&server, "play.example.test", 25565);
    assert_eq!(direct["version"]["protocol"], 5);
    assert_eq!(direct["version"], srv["version"]);
    assert_eq!(direct["description"], srv["description"]);
    assert_eq!(direct["players"]["max"], srv["players"]["max"]);

    // DNS service discovery would occur at the client, before the TCP socket.
    // The server must process the hostname and advertised port from the
    // Handshake independently of the actual loopback listener address.
    login(&server, "localhost", server.port, "RustTester");
    login(&server, "play.example.test", 25565, "TwinAlpha");
}

#[test]
fn reg122_hostname_byte_and_java_utf16_boundaries() {
    let server = TestServer::start();
    // ASCII and multibyte UTF-8 host strings at the 255-unit limit.
    assert_eq!(
        status(&server, &"a".repeat(255), server.port)["version"]["protocol"],
        5
    );
    assert_eq!(
        status(&server, &"界".repeat(255), server.port)["version"]["protocol"],
        5
    );
    assert_eq!(
        status(&server, &format!("{}x", "😀".repeat(127)), server.port)["version"]["protocol"],
        5
    );

    // A 256-unit hostname must be rejected rather than treated as a valid
    // connection. The supplementary-plane case also regresses #504/#846.
    for oversized in ["a".repeat(256), "😀".repeat(128)] {
        let mut client = server.connect();
        // Send both frames in one buffer, so an immediate server rejection
        // cannot make a second write race with TCP connection closure.
        let mut frames = Vec::new();
        write_frame(&mut frames, &handshake(&oversized, server.port, 1)).unwrap();
        write_frame(&mut frames, &packet(0, &[])).unwrap();
        client.write_all(&frames).unwrap();
        assert!(
            read_frame(&mut client).is_err(),
            "accepted 256 UTF-16 units in Protocol 5 hostname"
        );
    }
}
