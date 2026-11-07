//! ADD-081 / #506: a live Rust server must persist a single minecart
//! checkpoint after 20 TPS ticks and restart without a transaction error.
//! Test data lives in an isolated create-only directory, never a user world.
use legacy_protocol::{read_frame, read_varint, write_frame, write_string, write_varint, Cursor};
use serde_json::Value;
use std::fs::{self, File};
use std::io::{self, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "ir-add081-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create-only scratch directory");
        fs::write(
            root.join("whitelist.json"),
            r#"[{"name":"RustTester"}]"#,
        )
        .expect("isolated test whitelist");
        Self(root)
    }

    fn at(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if thread::panicking() {
            eprintln!("ADD-081 test diagnostics preserved at {}", self.0.display());
        } else {
            fs::remove_dir_all(&self.0).expect("remove own scratch directory");
        }
    }
}

struct Server {
    child: Child,
    stderr: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn packet(id: u32, body: &[u8]) -> io::Result<Vec<u8>> {
    let mut payload = Vec::new();
    write_varint(id, &mut payload);
    payload.extend_from_slice(body);
    let mut frame = Vec::new();
    write_frame(&mut frame, &payload)?;
    Ok(frame)
}

fn status_probe(port: u16) -> io::Result<()> {
    let mut socket = TcpStream::connect(("127.0.0.1", port))?;
    socket.set_read_timeout(Some(Duration::from_secs(3)))?;
    socket.set_write_timeout(Some(Duration::from_secs(3)))?;

    let mut handshake = Vec::new();
    write_varint(5, &mut handshake);
    write_string("localhost", &mut handshake)?;
    handshake.extend_from_slice(&port.to_be_bytes());
    write_varint(1, &mut handshake);
    socket.write_all(&packet(0, &handshake)?)?;
    socket.write_all(&packet(0, &[])?)?;

    let frame = read_frame(&mut socket)?;
    let mut body = frame.as_slice();
    if read_varint(&mut body)? != 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "missing Status"));
    }
    let mut cursor = Cursor::new(body);
    let reply: Value = serde_json::from_str(cursor.read_string(32768)?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    cursor.finish()?;
    if reply["version"]["protocol"] != 5 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "wrong protocol"));
    }

    // Complete the Status state to release its unauthenticated admission slot.
    let nonce: i64 = 0x12_34_56_78;
    socket.write_all(&packet(1, &nonce.to_be_bytes())?)?;
    let pong = read_frame(&mut socket)?;
    let mut pong_body = pong.as_slice();
    if read_varint(&mut pong_body)? != 1 || pong_body != nonce.to_be_bytes() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "wrong Pong echo"));
    }
    Ok(())
}

impl Server {
    fn launch(scratch: &Scratch, world: &Path, label: &str) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("reserve port");
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let stderr = scratch.at(&format!("{label}.stderr.log"));
        let log = File::create(&stderr).expect("create server log");
        let child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .args(["--bind", &format!("127.0.0.1:{port}"), "--world"])
            .arg(world)
            .arg("--whitelist")
            .arg(scratch.at("whitelist.json"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::from(log))
            .spawn()
            .expect("launch native Rust server");
        let mut server = Self { child, stderr };
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = server.child.try_wait().expect("query server exit") {
                panic!(
                    "server exited before readiness: {status}\n{}",
                    server.diagnostics()
                );
            }
            if status_probe(port).is_ok() {
                return server;
            }
            assert!(
                Instant::now() < deadline,
                "Protocol 5 Status readiness timeout\n{}",
                server.diagnostics()
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn diagnostics(&self) -> String {
        fs::read_to_string(&self.stderr).unwrap_or_else(|error| error.to_string())
    }

    fn stop_cleanly(&mut self) {
        let input = self.child.stdin.as_mut().expect("server stdin available");
        input.write_all(b"/stop\n").expect("request safe stop");
        self.child.stdin.take();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = self.child.try_wait().expect("poll server exit") {
                assert!(status.success(), "server exit {status}\n{}", self.diagnostics());
                return;
            }
            assert!(
                Instant::now() < deadline,
                "server failed to stop\n{}",
                self.diagnostics()
            );
            thread::sleep(Duration::from_millis(50));
        }
    }
}

#[test]
fn add081_minecart_checkpoint_persists_and_restarts_after_twenty_ticks() {
    let scratch = Scratch::new();
    let world = scratch.at("flat.irw");
    let minecarts = world.with_extension("minecarts.json");
    let pending = world.with_extension("pending-txn.json");
    let mut first_snapshot: Option<Vec<u8>> = None;
    for cycle in 0..2 {
        let mut server = Server::launch(&scratch, &world, &format!("cycle-{cycle}"));
        // The projectile worker writes its initial minecart snapshot at >=20
        // ticks. No actual minecart entity needs to exist for this checkpoint.
        thread::sleep(Duration::from_millis(1750));
        assert!(
            server.child.try_wait().expect("server liveness").is_none(),
            "server exited before checkpoint\n{}",
            server.diagnostics()
        );
        server.stop_cleanly();
        let diagnostics = server.diagnostics();
        assert!(
            !diagnostics.contains("MINECART CHECKPOINT ERROR")
                && !diagnostics.contains("MINECART FINAL CHECKPOINT ERROR"),
            "minecart checkpoint failed in cycle {cycle}:\n{diagnostics}"
        );
        assert!(
            minecarts.is_file(),
            "expected a minecart checkpoint after at least 20 ticks"
        );
        assert!(
            !pending.exists(),
            "server left an unfinished journal on clean stop"
        );
        let saved = fs::read(&minecarts).expect("read persisted minecarts");
        let snapshot: Value = serde_json::from_slice(&saved).expect("valid minecart JSON");
        assert_eq!(snapshot["version"], 1);
        assert_eq!(snapshot["next"], 0);
        assert_eq!(snapshot["carts"], serde_json::json!([]));
        if let Some(previous) = &first_snapshot {
            assert_eq!(&saved, previous, "empty minecarts changed on restart");
        } else {
            first_snapshot = Some(saved);
        }
    }
}
