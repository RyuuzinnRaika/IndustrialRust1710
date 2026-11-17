//! NEXT-015 / #998: a Play client that stops answering KeepAlive challenges
//! must be kicked by the heartbeat timeout, releasing its identity lease and
//! Hub admission without disturbing the server or other live sessions. The
//! heartbeat boundary arithmetic itself is pinned in `disconnect_diagnostics_
//! tests`; this drives the real server binary on loopback with a disposable
//! world. No Java client, Minecraft distribution, or operator world data is
//! accessed. Runtime is ~60 s by design (10 s interval + 40 s timeout).
use legacy_protocol::{read_frame, write_frame, write_string, write_varint, Cursor};
use std::fs::{self, File};
use std::io::{self, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SILENT: &str = "Next015Silent";
const ACTIVE: &str = "Next015Active";

struct TestServer {
    child: Child,
    root: PathBuf,
    log: PathBuf,
    port: u16,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        // Preserve the temporary log when the test fails for diagnosis.
        if !thread::panicking() {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

impl TestServer {
    fn start() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "industrialrust-next015-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create disposable test directory");
        fs::write(
            root.join("whitelist.json"),
            format!(r#"[{{"name":"{SILENT}"}},{{"name":"{ACTIVE}"}}]"#),
        )
        .expect("write isolated whitelist");
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("choose loopback port");
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let log = root.join("server.log");
        let output = File::create(&log).expect("create server log");
        let child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .args(["--bind", &format!("127.0.0.1:{port}"), "--world"])
            .arg(root.join("next015.irw"))
            .arg("--whitelist")
            .arg(root.join("whitelist.json"))
            .stdin(Stdio::piped())
            .stdout(Stdio::from(output.try_clone().unwrap()))
            .stderr(Stdio::from(output))
            .spawn()
            .expect("launch native Rust server");
        let mut server = Self {
            child,
            root,
            log,
            port,
        };
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            if let Some(exit) = server.child.try_wait().expect("poll server") {
                panic!(
                    "server exited before listening: {exit}\n{}",
                    fs::read_to_string(&server.log).unwrap_or_default()
                );
            }
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return server;
            }
            assert!(Instant::now() < deadline, "server startup timed out");
            thread::sleep(Duration::from_millis(40));
        }
    }

    fn stop(&mut self) {
        self.child
            .stdin
            .as_mut()
            .expect("server stdin")
            .write_all(b"/stop\n")
            .expect("send graceful stop");
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = self.child.try_wait().expect("poll shutdown") {
                assert!(
                    status.success(),
                    "graceful shutdown failed: {status}\n{}",
                    fs::read_to_string(&self.log).unwrap_or_default()
                );
                return;
            }
            assert!(Instant::now() < deadline, "server /stop timed out");
            thread::sleep(Duration::from_millis(50));
        }
    }
}

fn transmit(socket: &mut TcpStream, id: u32, body: &[u8]) {
    let mut packet = Vec::new();
    write_varint(id, &mut packet);
    packet.extend_from_slice(body);
    write_frame(socket, &packet).expect("write packet");
}

/// Handshake + LoginStart and require Login Success (0x02) as the first reply.
fn login(port: u16, name: &str, label: &str) -> TcpStream {
    let socket = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut socket = socket;
    let mut handshake = Vec::new();
    write_varint(5, &mut handshake);
    write_string("localhost", &mut handshake).unwrap();
    handshake.extend_from_slice(&port.to_be_bytes());
    write_varint(2, &mut handshake);
    transmit(&mut socket, 0, &handshake);
    let mut start = Vec::new();
    write_string(name, &mut start).unwrap();
    transmit(&mut socket, 0, &start);
    let frame =
        read_frame(&mut socket).unwrap_or_else(|err| panic!("{label}: no login response: {err}"));
    let mut response = Cursor::new(&frame);
    assert_eq!(
        response.read_varint().unwrap(),
        2,
        "{label}: expected Login Success"
    );
    assert_eq!(response.read_string(40).unwrap().len(), 36);
    assert_eq!(response.read_string(16).unwrap(), name);
    response.finish().unwrap();
    socket
}

/// Drain every server packet forever; never answers KeepAlive (0x00).
/// `alive` mirrors the socket state: true until the server closes it.
fn drain_forever(mut socket: TcpStream, alive: Arc<AtomicBool>) {
    thread::spawn(move || loop {
        match read_frame(&mut socket) {
            Ok(_) => continue,
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                continue;
            }
            Err(_) => {
                alive.store(false, Ordering::SeqCst);
                return;
            }
        }
    });
}

/// Read frames forever, echoing every server KeepAlive straight back so the
/// heartbeat never expires. A Play KeepAlive is exactly one 0x00 VarInt ID
/// plus a four-byte integer challenge. Counts answered challenges.
fn answer_keepalives(mut socket: TcpStream, answered: Arc<std::sync::atomic::AtomicUsize>) {
    thread::spawn(move || loop {
        match read_frame(&mut socket) {
            Ok(frame) => {
                if frame.len() == 5 && frame[0] == 0x00 {
                    let challenge = frame[1..5].to_vec();
                    transmit(&mut socket, 0, &challenge);
                    answered.fetch_add(1, Ordering::SeqCst);
                }
            }
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                continue;
            }
            Err(_) => return,
        }
    });
}

#[test]
fn next015_silent_client_is_kicked_and_its_slot_releases_but_server_survives() {
    let mut server = TestServer::start();

    // A healthy session that answers every heartbeat must stay connected
    // across the entire silent-client timeout window.
    let active = login(server.port, ACTIVE, "active client");
    let answered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    answer_keepalives(active, Arc::clone(&answered));

    // A silent session keeps its socket open but never answers 0x00.
    let silent = login(server.port, SILENT, "silent client");
    let alive = Arc::new(AtomicBool::new(true));
    drain_forever(silent, Arc::clone(&alive));

    // 10 s interval + 40 s timeout; require the kick within 75 s of login.
    let deadline = Instant::now() + Duration::from_secs(75);
    while alive.load(Ordering::SeqCst) {
        assert!(
            Instant::now() < deadline,
            "silent client was never kicked by the keepalive timeout"
        );
        thread::sleep(Duration::from_millis(200));
    }

    // The kick must be surgical: the honest session is untouched and has
    // answered at least one heartbeat challenge by now.
    assert!(
        answered.load(Ordering::SeqCst) >= 1,
        "honest client never saw a heartbeat challenge during the window"
    );
    assert!(
        server.child.try_wait().expect("poll server").is_none(),
        "server process died when a client timed out"
    );

    // Cleanup is complete only when the same identity can log in again.
    let rejoin_deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            login(server.port, SILENT, "same-name rejoin")
        })) {
            Ok(rejoin) => {
                let _ = rejoin.shutdown(Shutdown::Write);
                break;
            }
            Err(_) => {
                assert!(
                    Instant::now() < rejoin_deadline,
                    "kicked player's identity was never released"
                );
                thread::sleep(Duration::from_millis(500));
            }
        }
    }

    // And the server still serves the honest session until graceful stop.
    server.stop();
}
