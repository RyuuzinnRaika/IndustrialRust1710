//! REG-124 / #760: Login Disconnect reasons remain valid Protocol 5 JSON
//! and rejection never grants a player session. This is a native Rust TCP
//! regression; a vanilla Java 1.7.10 screen check remains a separate gate.
use legacy_protocol::{read_frame, read_varint, write_frame, write_string, write_varint, Cursor};
use serde_json::Value;
use std::fs::{self, File};
use std::io::{self, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Server {
    child: Child,
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
                "REG-124 isolated server log:\n{}",
                fs::read_to_string(&self.stderr).unwrap_or_default()
            );
            eprintln!("REG-124 scratch retained: {}", self.root.display());
        } else {
            fs::remove_dir_all(&self.root).expect("remove only this test's scratch world");
        }
    }
}

fn connect(port: u16) -> io::Result<TcpStream> {
    let socket = TcpStream::connect(("127.0.0.1", port))?;
    socket.set_read_timeout(Some(Duration::from_secs(8)))?;
    socket.set_write_timeout(Some(Duration::from_secs(8)))?;
    Ok(socket)
}

fn packet(id: u32, body: &[u8]) -> io::Result<Vec<u8>> {
    let mut payload = Vec::new();
    write_varint(id, &mut payload);
    payload.extend_from_slice(body);
    let mut framed = Vec::new();
    write_frame(&mut framed, &payload)?;
    Ok(framed)
}

fn send(socket: &mut TcpStream, id: u32, body: &[u8]) -> io::Result<()> {
    socket.write_all(&packet(id, body)?)
}

fn handshake(port: u16, version: u32, next: u32) -> io::Result<Vec<u8>> {
    let mut body = Vec::new();
    write_varint(version, &mut body);
    write_string("localhost", &mut body)?;
    body.extend_from_slice(&port.to_be_bytes());
    write_varint(next, &mut body);
    Ok(body)
}

fn receive(socket: &mut TcpStream) -> io::Result<(u32, Vec<u8>)> {
    let framed = read_frame(socket)?;
    let mut body = framed.as_slice();
    let id = read_varint(&mut body)?;
    Ok((id, body.to_vec()))
}

fn status(port: u16) -> io::Result<Value> {
    let mut socket = connect(port)?;
    send(&mut socket, 0, &handshake(port, 5, 1)?)?;
    send(&mut socket, 0, &[])?;
    let (id, body) = receive(&mut socket)?;
    if id != 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "missing Status JSON"));
    }
    let mut reader = Cursor::new(&body);
    let json: Value = serde_json::from_str(reader.read_string(32768)?)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    reader.finish()?;

    // Finish Status by completing Ping/Pong, instead of leaking an IP slot.
    let token: i64 = 0x1020_3040_5060_7080;
    send(&mut socket, 1, &token.to_be_bytes())?;
    let (echo_id, payload) = receive(&mut socket)?;
    if echo_id != 1 || payload != token.to_be_bytes() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid Status Pong"));
    }
    if json["version"]["protocol"] != 5 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not Protocol 5"));
    }
    Ok(json)
}

fn reject_reason(port: u16, version: u32, username: Option<&str>) -> Value {
    let mut socket = connect(port).expect("connect rejected login");
    send(&mut socket, 0, &handshake(port, version, 2).unwrap()).unwrap();
    if let Some(name) = username {
        let mut body = Vec::new();
        write_string(name, &mut body).unwrap();
        send(&mut socket, 0, &body).unwrap();
    }
    let (id, data) = receive(&mut socket).expect("Login Disconnect packet");
    assert_eq!(id, 0, "rejection must use Login Disconnect, not Play");
    let mut reader = Cursor::new(&data);
    let json: Value = serde_json::from_str(reader.read_string(4096).unwrap())
        .expect("Disconnect reason must be valid JSON");
    reader.finish().unwrap();
    assert!(
        json["text"].as_str().is_some_and(|s| !s.is_empty()),
        "client must receive a nonempty text component"
    );
    json
}

impl Server {
    fn start() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "industrialrust-reg124-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create-only isolated world");
        fs::write(
            root.join("whitelist.json"),
            r#"[{"name":"RustTester"}]"#,
        )
        .unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let stderr = root.join("server-stderr.log");
        let child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .args(["--bind", &format!("127.0.0.1:{port}"), "--world"])
            .arg(root.join("flat.irw"))
            .arg("--whitelist")
            .arg(root.join("whitelist.json"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(File::create(&stderr).unwrap()))
            .spawn()
            .expect("launch native Rust server");

        let mut server = Self {
            child,
            root,
            stderr,
            port,
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if server.child.try_wait().unwrap().is_some() {
                panic!("server died before Protocol 5 readiness");
            }
            if status(port).is_ok() {
                return server;
            }
            assert!(Instant::now() < deadline, "server readiness timed out");
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn wait_online(&self, target: u64) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if status(self.port).unwrap()["players"]["online"].as_u64() == Some(target) {
                return;
            }
            assert!(Instant::now() < deadline, "online count never reached {target}");
            thread::sleep(Duration::from_millis(50));
        }
    }
}

#[test]
fn reg124_login_rejection_reasons_are_json_and_never_admit_players() {
    let server = Server::start();
    server.wait_online(0);

    let not_whitelisted = reject_reason(server.port, 5, Some("GuestUser"));
    assert_eq!(
        not_whitelisted["text"],
        "You are not on the server whitelist."
    );
    server.wait_online(0);

    // Invalid client version must fail before Login Start, but still send the
    // same Protocol 5 Login Disconnect packet with a readable reason.
    let wrong_protocol = reject_reason(server.port, 47, None);
    assert_eq!(
        wrong_protocol["text"],
        "Use an unmodified Minecraft Java 1.7.10 (protocol 5) client."
    );
    server.wait_online(0);

    // A failed login must not block the next valid one from entering Play.
    let mut legitimate = connect(server.port).unwrap();
    send(&mut legitimate, 0, &handshake(server.port, 5, 2).unwrap()).unwrap();
    let mut login_start = Vec::new();
    write_string("RustTester", &mut login_start).unwrap();
    send(&mut legitimate, 0, &login_start).unwrap();
    let (success_id, success) = receive(&mut legitimate).expect("valid login");
    assert_eq!(success_id, 2, "valid user must receive Login Success");
    let mut reader = Cursor::new(&success);
    assert_eq!(reader.read_string(40).unwrap().len(), 36);
    assert_eq!(reader.read_string(16).unwrap(), "RustTester");
    reader.finish().unwrap();
    server.wait_online(1);
    drop(legitimate);
    server.wait_online(0);
}
