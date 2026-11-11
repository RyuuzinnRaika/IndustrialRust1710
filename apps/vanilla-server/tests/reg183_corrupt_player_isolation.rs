//! REG-183 / #819: one corrupted player's saved NBT must not block or
//! overwrite another player's healthy inventory/session.
//! Uses an exclusive disposable world and loopback Protocol 5 sockets only.
use legacy_protocol::{read_frame, read_varint, write_frame, write_string, write_varint, Cursor};
use serde_json::Value;
use std::fs::{self, File};
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Server {
    process: Child,
    log: PathBuf,
    port: u16,
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

fn start(exe: &str, world: &Path, whitelist: &Path, root: &Path, label: &str) -> Server {
    let port = port();
    let log = root.join(format!("{label}.log"));
    let output = File::create(&log).unwrap();
    let process = Command::new(exe)
        .args(["--bind", &format!("127.0.0.1:{port}"), "--world"])
        .arg(world)
        .arg("--whitelist")
        .arg(whitelist)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(output.try_clone().unwrap()))
        .stderr(Stdio::from(output))
        .spawn()
        .unwrap();
    let mut server = Server { process, log, port };
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if TcpStream::connect(("127.0.0.1", server.port)).is_ok() {
            assert!(server.process.try_wait().unwrap().is_none());
            return server;
        }
        if let Some(status) = server.process.try_wait().unwrap() {
            panic!(
                "server exited before listen: {status}; {}",
                fs::read_to_string(&server.log).unwrap_or_default()
            );
        }
        assert!(Instant::now() < deadline, "server readiness timed out");
        thread::sleep(Duration::from_millis(40));
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

fn join(port: u16, name: &str) -> TcpStream {
    let mut stream = connect_login(port, name);
    let mut authenticated = false;
    for _ in 0..160 {
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
            assert!(authenticated, "S08 before Login Success");
            return stream;
        } else if id == 0 && !authenticated {
            panic!("healthy {name} was rejected unexpectedly");
        }
    }
    panic!("healthy {name} did not reach Protocol 5 Play");
}

fn reject_corrupt_player(port: u16, name: &str) {
    let mut stream = connect_login(port, name);
    let (id, message) = receive(&mut stream);
    assert_eq!(
        id, 0,
        "corrupt player must receive Login Disconnect, not Play"
    );
    let mut cursor = Cursor::new(&message);
    let reason = cursor.read_string(256).unwrap();
    cursor.finish().unwrap();
    assert!(
        reason.contains("Player save is unreadable"),
        "expected explicit playerdata error; received {reason:?}"
    );
}

fn stop(server: &mut Server) {
    server
        .process
        .stdin
        .take()
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

fn stone_count(inventory: &Path) -> u32 {
    let saved: Value = serde_json::from_slice(&fs::read(inventory).unwrap()).unwrap();
    let slots = saved["slots"].as_array().expect("45 player slots");
    assert_eq!(slots.len(), 45);
    slots
        .iter()
        .filter(|slot| slot["id"] == 1)
        .map(|slot| slot["count"].as_u64().unwrap() as u32)
        .sum()
}

fn wait_inventory(inventory: &Path, count: u32) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if inventory.exists() && stone_count(inventory) == count {
            return;
        }
        assert!(Instant::now() < deadline, "player inventory did not commit");
        thread::sleep(Duration::from_millis(40));
    }
}

fn wait_playerdata(players: &Path, name: &str) {
    // A player who joins and disconnects in the same instant has
    // no mutated state to persist yet, so the fixture must wait
    // until the server has materialized the saved profile before
    // treating it as an existing account.
    let dat = players.join(format!("{name}.dat"));
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if dat.is_file() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "saved playerdata for {name} did not materialize"
        );
        thread::sleep(Duration::from_millis(40));
    }
}

#[test]
fn reg183_corrupt_one_player_and_preserve_second_player_across_restart() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("ir1710-reg183-{}-{nanos}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let world = root.join("world/flat.irw");
    let whitelist = root.join("whitelist.json");
    fs::write(&whitelist, r#"[{"name":"TwinAlpha"},{"name":"TwinBeta"}]"#).unwrap();
    let exe = env!("CARGO_BIN_EXE_vanilla-server");

    // First process creates valid compressed player NBT for both users.
    let mut initial = start(exe, &world, &whitelist, &root, "initial");
    let alpha = join(initial.port, "TwinAlpha");
    let beta = join(initial.port, "TwinBeta");
    let players_dir = world.parent().unwrap().join("players");
    wait_playerdata(&players_dir, "twinalpha");
    wait_playerdata(&players_dir, "twinbeta");
    drop(alpha);
    drop(beta);
    stop(&mut initial);
    drop(initial);

    let players = world.parent().unwrap().join("players");
    let broken = players.join("twinalpha.dat");
    let healthy = players.join("twinbeta.dat");
    assert!(broken.is_file() && healthy.is_file());
    let healthy_before = fs::read(&healthy).unwrap();
    let corrupted = b"truncated-gzip-player-nbt";
    fs::write(&broken, corrupted).unwrap();

    let mut restarted = start(exe, &world, &whitelist, &root, "corrupt");
    reject_corrupt_player(restarted.port, "TwinAlpha");
    assert_eq!(
        fs::read(&broken).unwrap(),
        corrupted,
        "failed login must never silently repair or truncate corrupt player NBT"
    );
    let beta = join(restarted.port, "TwinBeta");
    // The healthy player's inventory is still writable while one account
    // is rejected. This exercises the real /give and Inventory::save path.
    restarted
        .process
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"/give TwinBeta 1 8\n")
        .unwrap();
    let beta_inventory = players.join("twinbeta.inventory.json");
    wait_inventory(&beta_inventory, 8);
    drop(beta);
    stop(&mut restarted);
    drop(restarted);

    assert_eq!(fs::read(&broken).unwrap(), corrupted);
    assert!(fs::read(&healthy).unwrap().len() > 16);
    assert!(healthy_before.len() > 16);
    assert_eq!(stone_count(&beta_inventory), 8);
    // Reboot again to prove the surviving account and saved inventory
    // remain admissible and the corrupt account is still excluded.
    let mut final_server = start(exe, &world, &whitelist, &root, "final");
    reject_corrupt_player(final_server.port, "TwinAlpha");
    let beta = join(final_server.port, "TwinBeta");
    assert_eq!(stone_count(&beta_inventory), 8);
    drop(beta);
    stop(&mut final_server);
    drop(final_server);

    assert_eq!(fs::read(&broken).unwrap(), corrupted);
    fs::remove_dir_all(root).unwrap();
}
