//! REG-201: an admitted session must persist even when the initial join
//! transfer is cut short by a socket reset.
//!
//! Before the fix, `handle()` returned early on **any** failure between Login
//! Success and the gameplay loop, skipping the persistence and entity-release
//! tail. A first-time joiner whose chunk transfer was interrupted therefore
//! ended with no playerdata at all, logged no `left:` line, and never released
//! the shared Mob/boat/minecart/fishing state.
//!
//! A reset landing exactly inside the transfer cannot be forced from user space
//! with certainty, so the test admits several players and aborts every
//! connection with unread frames still queued (which makes the peer send a TCP
//! reset) while the server is still pushing their initial world data. Every
//! admitted session must be persisted, whichever write the reset interrupts.
//! Without the fix at least one session takes the early-return path and is
//! never written; that is the same failure mode REG-183 exposed on the second
//! joiner (`ConnectionReset` inside the join transfer, no `left:` line).
//!
//! The test drives the real server binary over loopback Protocol 5 against an
//! exclusively created disposable world. It never opens an existing save.
use legacy_protocol::{read_frame, read_varint, write_frame, write_string, write_varint, Cursor};
use serde_json::Value;
use std::fs::{self, File};
use std::io::Write;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ATTEMPTS: usize = 8;

struct Server {
    process: Child,
    log: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.process.try_wait().ok().flatten().is_none() {
            let _ = self.process.kill();
        }
        let _ = self.process.wait();
    }
}

fn port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn start(exe: &str, world: &Path, whitelist: &Path, root: &Path, label: &str) -> (Server, u16) {
    let bound = port();
    let log = root.join(format!("{label}.log"));
    let output = File::create(&log).unwrap();
    let process = Command::new(exe)
        .args(["--bind", &format!("127.0.0.1:{bound}"), "--world"])
        .arg(world)
        .arg("--whitelist")
        .arg(whitelist)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(output.try_clone().unwrap()))
        .stderr(Stdio::from(output))
        .spawn()
        .unwrap();
    let mut server = Server { process, log };
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        if TcpStream::connect(("127.0.0.1", bound)).is_ok() {
            assert!(server.process.try_wait().unwrap().is_none());
            return (server, bound);
        }
        if let Some(status) = server.process.try_wait().unwrap() {
            panic!(
                "server exited before listen: {status}; {}",
                fs::read_to_string(&server.log).unwrap_or_default()
            );
        }
        assert!(Instant::now() < deadline, "server startup timed out");
        thread::sleep(Duration::from_millis(40));
    }
}

fn stop(server: &mut Server) {
    server
        .process
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"/stop\n")
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = server.process.try_wait().unwrap() {
            assert!(
                status.success(),
                "server /stop failed ({status}): {}",
                fs::read_to_string(&server.log).unwrap_or_default()
            );
            return;
        }
        assert!(Instant::now() < deadline, "server /stop did not complete");
        thread::sleep(Duration::from_millis(50));
    }
}

fn packet(id: u32, body: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_varint(id, &mut bytes);
    bytes.extend_from_slice(body);
    let mut framed = Vec::new();
    write_frame(&mut framed, &bytes).unwrap();
    framed
}

fn connect_login(port: u16, name: &str) -> TcpStream {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    let mut handshake = Vec::new();
    write_varint(5, &mut handshake);
    write_string("localhost", &mut handshake).unwrap();
    handshake.extend_from_slice(&port.to_be_bytes());
    write_varint(2, &mut handshake);
    stream.write_all(&packet(0, &handshake)).unwrap();
    let mut login = Vec::new();
    write_string(name, &mut login).unwrap();
    stream.write_all(&packet(0, &login)).unwrap();
    stream
}

fn receive(stream: &mut TcpStream) -> (u32, Vec<u8>) {
    let frame = read_frame(stream).unwrap();
    let mut body = frame.as_slice();
    let id = read_varint(&mut body).unwrap();
    (id, body.to_vec())
}

/// Log in, take only Login Success, then reset the connection with the rest of
/// the initial world transfer still unread, so the server keeps writing into a
/// socket the peer has already aborted.
fn login_then_reset(port: u16, name: &str) {
    let mut stream = connect_login(port, name);
    for _ in 0..16 {
        let (id, bytes) = receive(&mut stream);
        if id == 2 {
            let mut cursor = Cursor::new(&bytes);
            assert_eq!(cursor.read_string(40).unwrap().len(), 36);
            assert_eq!(cursor.read_string(16).unwrap(), name);
            cursor.finish().unwrap();
            let _ = stream.shutdown(Shutdown::Both);
            drop(stream);
            return;
        }
        if id == 0 {
            panic!("player {name} was rejected during login");
        }
    }
    panic!("player {name} never received Login Success");
}

/// A complete Protocol 5 join: Login Success, Game Startup echo, then Play.
fn join(port: u16, name: &str) -> TcpStream {
    let mut stream = connect_login(port, name);
    let mut authenticated = false;
    for _ in 0..4096 {
        let (id, bytes) = receive(&mut stream);
        if id == 2 && !authenticated {
            let mut cursor = Cursor::new(&bytes);
            assert_eq!(cursor.read_string(40).unwrap().len(), 36);
            assert_eq!(cursor.read_string(16).unwrap(), name);
            cursor.finish().unwrap();
            authenticated = true;
        } else if id == 0 && bytes.len() == 4 {
            stream.write_all(&packet(0, &bytes)).unwrap();
        } else if id == 8 {
            return stream;
        } else if id == 0 && authenticated {
            panic!("player {name} was rejected unexpectedly");
        }
    }
    panic!("player {name} did not reach Protocol 5 Play");
}

fn slots_of(inventory: &Path) -> Vec<Value> {
    let saved: Value = serde_json::from_slice(&fs::read(inventory).unwrap()).unwrap();
    saved["slots"]
        .as_array()
        .unwrap_or_else(|| panic!("{} has no 45-slot inventory", inventory.display()))
        .clone()
}

fn account_files(players: &Path, account: &str) -> [PathBuf; 4] {
    [
        players.join(format!("{account}.dat")),
        players.join(format!("{account}.inventory.json")),
        players.join(format!("{account}.experience.json")),
        players.join(format!("{account}.achievements.json")),
    ]
}

#[test]
fn reg201_reset_during_join_transfer_persists_every_admitted_player() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    // Create-only root: this test never reuses or deletes an existing world.
    let root = std::env::temp_dir().join(format!("ir1710-reg201-{}-{nanos}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let world = root.join("world/flat.irw");
    let names: Vec<String> = (0..ATTEMPTS)
        .map(|index| format!("Reg201p{index}"))
        .collect();
    let whitelist = root.join("whitelist.json");
    let entries: Vec<String> = names
        .iter()
        .map(|name| format!(r#"{{"name":"{name}"}}"#))
        .collect();
    fs::write(&whitelist, format!("[{}]", entries.join(","))).unwrap();
    let exe = env!("CARGO_BIN_EXE_vanilla-server");

    let (mut server, bound) = start(exe, &world, &whitelist, &root, "join-reset");
    let players = world.parent().unwrap().join("players");

    // Admit every session, then abort it inside the world transfer.
    for name in &names {
        login_then_reset(bound, name);
    }

    // Each admitted session must still reach the persistence tail.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let missing: Vec<String> = names
            .iter()
            .filter(|name| {
                !players
                    .join(format!("{}.dat", name.to_lowercase()))
                    .is_file()
            })
            .cloned()
            .collect();
        if missing.is_empty() {
            break;
        }
        if Instant::now() >= deadline {
            let listing = fs::read_dir(&players)
                .map(|items| {
                    items
                        .filter_map(Result::ok)
                        .map(|item| item.file_name().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default();
            let log = fs::read_to_string(&server.log).unwrap_or_default();
            panic!(
                "join-transfer resets lost {} player(s): {}; players=({listing}) log:\n{log}",
                missing.len(),
                missing.join(",")
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
    for name in &names {
        let [dat, inventory, experience, achievements] =
            account_files(&players, &name.to_lowercase());
        for file in [&dat, &inventory, &experience, &achievements] {
            assert!(file.is_file(), "{} was never written", file.display());
        }
        assert!(
            fs::read(&dat).unwrap().len() > 16,
            "player NBT is empty for {name}"
        );
        assert_eq!(slots_of(&inventory).len(), 45, "inventory slots for {name}");
    }

    // The aborted sessions must also have released their hub slots and run the
    // entity cleanup: all accounts have to be admissible again, and each
    // session must have logged its disconnect tail exactly once.
    for name in &names {
        drop(join(bound, name));
    }
    stop(&mut server);
    let log = fs::read_to_string(&server.log).unwrap_or_default();
    for name in &names {
        assert_eq!(
            log.matches(&format!("left: {name}")).count(),
            2,
            "aborted and clean sessions must both run the disconnect tail for {name}; log:\n{log}"
        );
    }
    for name in &names {
        let [dat, inventory, ..] = account_files(&players, &name.to_lowercase());
        assert!(dat.is_file());
        assert_eq!(slots_of(&inventory).len(), 45);
    }
    fs::remove_dir_all(root).unwrap();
}
