//! NEXT-007 / #990: the accept/reject criteria for player names must be the
//! same function at every boundary. Login applies `whitelist::valid_name`
//! (3-16 ASCII letters/digits/underscore), the whitelist file is loaded with
//! the same check, and PlayerData paths fold the name to ascii lowercase.
//! Drives the real Rust server on loopback with a disposable world; no Java
//! client, Minecraft distribution, or operator world data is accessed.
use legacy_protocol::{read_frame, write_frame, write_string, write_varint, Cursor};
use std::fs::{self, File};
use std::io::{self, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ACCOUNT: &str = "Next007Tester";

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
        // Preserve the temporary directory when the test fails for diagnosis.
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
            "industrialrust-next007-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create disposable test directory");
        fs::write(
            root.join("whitelist.json"),
            format!(r#"[{{"name":"{ACCOUNT}"}}]"#),
        )
        .expect("write isolated whitelist");
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("choose loopback port");
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let log = root.join("server.log");
        let output = File::create(&log).expect("create server log");
        let child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .args(["--bind", &format!("127.0.0.1:{port}"), "--world"])
            .arg(root.join("next007.irw"))
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
}

/// One full Handshake(next=2) + LoginStart(name) attempt. Returns the first
/// response frame (None when the server refuses without answering, such as a
/// parse-level reject) together with the socket, so a successful session can
/// be kept open for lease assertions.
fn attempt_login(port: u16, name: &str) -> io::Result<(Option<Vec<u8>>, TcpStream)> {
    let mut socket = TcpStream::connect(("127.0.0.1", port))?;
    socket.set_read_timeout(Some(Duration::from_secs(10)))?;
    socket.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut handshake = Vec::new();
    write_varint(0, &mut handshake);
    write_varint(5, &mut handshake);
    write_string("localhost", &mut handshake).unwrap();
    handshake.extend_from_slice(&port.to_be_bytes());
    write_varint(2, &mut handshake);
    write_frame(&mut socket, &handshake)?;
    let mut start = Vec::new();
    write_varint(0, &mut start);
    write_string(name, &mut start).unwrap();
    write_frame(&mut socket, &start)?;
    match read_frame(&mut socket) {
        Ok(frame) => Ok((Some(frame), socket)),
        Err(_) => Ok((None, socket)),
    }
}

fn packet_id(frame: &[u8]) -> i32 {
    Cursor::new(frame)
        .read_varint()
        .expect("response frame starts with a packet id")
}

/// Login Disconnect (0x00) carries the reason as a JSON chat string.
fn disconnect_reason(frame: &[u8]) -> String {
    let mut cursor = Cursor::new(frame);
    assert_eq!(
        cursor.read_varint().unwrap(),
        0,
        "expected Login Disconnect"
    );
    let reason = cursor
        .read_string(4096)
        .expect("disconnect reason string")
        .to_string();
    cursor
        .finish()
        .expect("disconnect frame ends after the reason");
    reason
}

/// Require a genuine Login Success (0x02, uuid, echoed name) for `name`.
fn expect_success(frame: Option<&[u8]>, name: &str) {
    let frame = frame.unwrap_or_else(|| panic!("{name}: server never answered"));
    let mut cursor = Cursor::new(frame);
    assert_eq!(
        cursor.read_varint().unwrap(),
        2,
        "{name}: expected Login Success, got a {}-byte reject-like frame",
        frame.len()
    );
    cursor.read_string(40).expect("uuid string");
    let echoed = cursor.read_string(16).expect("name string").to_string();
    assert_eq!(echoed, name, "{name}: success echoes the login name");
    cursor
        .finish()
        .expect("Login Success has no trailing bytes");
}

fn ls(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|entry| {
                    entry
                        .ok()
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                })
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn next007_name_criteria_are_identical_at_login_whitelist_and_player_data() {
    let mut server = TestServer::start();

    // 1. Malformed names are rejected at Login with the shared criterion:
    //    the same valid_name() the whitelist file itself is loaded with.
    for name in ["ab", "no space", "wrong-hyphen", "ＡＢＣＤ"] {
        let (frame, _socket) = attempt_login(server.port, name)
            .unwrap_or_else(|err| panic!("{name:?}: connection failed: {err}"));
        let frame = frame.unwrap_or_else(|| {
            panic!("{name:?}: server dropped a name it should explicitly reject")
        });
        assert_eq!(packet_id(&frame), 0, "{name:?}: must be Login Disconnect");
        assert!(
            disconnect_reason(&frame).contains("Invalid player name."),
            "{name:?}: unexpected reject reason"
        );
    }
    // A 17-character name violates the Protocol 5 16-character string limit
    // itself (parse-level reject); it must still never receive Login Success.
    let (frame, _socket) =
        attempt_login(server.port, "SeventeenChrsName").expect("overlong attempt");
    assert!(
        frame.as_deref().is_none_or(|f| packet_id(f) != 2),
        "a 17-character name must never reach Play"
    );

    // 2. Whitelist admission at Login is case-insensitive (same lowercasing
    //    as Whitelist::allows), and the identity lease folds case: while one
    //    spelling is connected, another spelling is the same account.
    let (lower, lower_session) =
        attempt_login(server.port, "next007tester").expect("lowercase attempt");
    expect_success(lower.as_deref(), "next007tester");

    let (busy, _busy_socket) = attempt_login(server.port, "NEXT007TESTER")
        .expect("uppercase attempt while the lowercase session is online");
    let busy = busy.expect("case-variant double admission must be answered");
    assert_eq!(packet_id(&busy), 0, "case variant must not double-admit");
    assert!(
        disconnect_reason(&busy).contains("already connecting or connected"),
        "the case-folded identity lease must reject the second spelling"
    );

    // 3. Once the first session leaves, the other spelling logs in — proving
    //    Login and Whitelist agree on case folding in both directions.
    drop(lower_session);
    let deadline = Instant::now() + Duration::from_secs(10);
    let (upper, upper_session) = loop {
        let (frame, socket) = attempt_login(server.port, "NEXT007TESTER").expect("retry attempt");
        match &frame {
            Some(f) if packet_id(f) == 2 => break (frame, socket),
            Some(f) if packet_id(f) == 0 => {
                // The previous session's lease may still be draining.
                if Instant::now() >= deadline {
                    panic!(
                        "uppercase variant never admitted after release: {}",
                        disconnect_reason(f)
                    );
                }
                drop(socket);
                thread::sleep(Duration::from_millis(100));
            }
            other => panic!("unexpected answer for uppercase variant: {other:?}"),
        }
    };
    expect_success(upper.as_deref(), "NEXT007TESTER");
    drop(upper_session);

    // 4. Stop the server so the shutdown drain persists PlayerData.
    server
        .child
        .stdin
        .as_mut()
        .expect("server stdin")
        .write_all(b"/stop\n")
        .expect("send /stop");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(exit) = server.child.try_wait().expect("poll shutdown") {
            assert!(exit.success(), "graceful shutdown failed: {exit}");
            break;
        }
        assert!(Instant::now() < deadline, "/stop timed out");
        thread::sleep(Duration::from_millis(50));
    }

    // 5. PlayerData: exactly one canonical lowercase save identity exists;
    //    neither the rejected names nor the case variants forked a file.
    let players = server.root.join("players");
    let dat = players.join("next007tester.dat");
    assert!(
        dat.is_file(),
        "canonical lowercase PlayerData missing: {:?}",
        ls(&players)
    );
    assert!(fs::read(&dat).unwrap().starts_with(&[0x1f, 0x8b]));
    let stems: Vec<String> = fs::read_dir(&players)
        .expect("players directory")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.ends_with(".dat") || name.ends_with(".inventory.json"))
        .map(|name| {
            name.trim_end_matches(".inventory.json")
                .trim_end_matches(".dat")
                .to_string()
        })
        .collect();
    let unique: std::collections::BTreeSet<String> = stems.iter().cloned().collect();
    assert_eq!(
        unique.into_iter().collect::<Vec<_>>(),
        vec!["next007tester".to_string()],
        "player saves must fold to one lowercase identity, found: {stems:?}"
    );
}
