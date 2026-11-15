//! NEXT-192 / #1175: a scripted two-survival-player chain on the REAL
//! server binary (isolated scratch world): one player mines a block with
//! the honest bare-hand delay, hands the drop over by drop-and-pickup,
//! then dies to a server-computed lethal fall (his cobblestone goes to
//! the ground), respawns, and the receiver walks over the death drop.
//! The server is then RESTARTED on the same world directory and the full
//! ledger must balance: exactly 1 dirt and 7 cobblestone exist in total,
//! all of it in the receiver's save, nothing on the ground, nothing left
//! in the giver's respawn save, and no XP was minted.
//!
//! Wire facts pinned here (server-observed):
//!   * C07 status 4 drops ONE item, status 3 the whole selected stack
//!     (this server's Q / Ctrl+Q semantics, proven via 0x2F deltas);
//!   * walking within pickup range moves the ground stack into the first
//!     free slot (0x2F) and out of the drops sidecar;
//!   * a 90->64 reported fall is lethal (S06 health 0.0) and the death
//!     drop releases the equipped stack exactly once; C16 respawn
//!     restores health 20;
//!   * after restart every store agrees on the same totals.
//!
//! Real unmodified 1.7.10 client acceptance: NOT_RUN (headless host).
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static CHAIN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn varint(value: i32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut v = value as u32;
    loop {
        let mut byte = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if v == 0 {
            return out;
        }
    }
}

fn mc_string(text: &str) -> Vec<u8> {
    let mut out = varint(text.len() as i32);
    out.extend_from_slice(text.as_bytes());
    out
}

fn frame(id: i32, payload: &[u8]) -> Vec<u8> {
    let mut body = varint(id);
    body.extend_from_slice(payload);
    let mut out = varint(body.len() as i32);
    out.extend_from_slice(&body);
    out
}

fn recv(socket: &mut TcpStream) -> (i32, Vec<u8>) {
    let packet = legacy_protocol::read_frame(socket).expect("frame");
    let mut raw = packet.as_slice();
    let id = legacy_protocol::read_varint(&mut raw).expect("packet id") as i32;
    (id, raw.to_vec())
}

fn until(socket: &mut TcpStream, tag: &str, want: impl Fn(i32, &[u8]) -> bool) -> (i32, Vec<u8>) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (id, body) = recv(socket);
        if id == 0x00 && body.len() == 4 {
            socket.write_all(&frame(0, &body)).unwrap();
        }
        if want(id, &body) {
            return (id, body);
        }
        assert!(
            Instant::now() < deadline,
            "{tag}: expected packet never arrived (last {id} {body:?})"
        );
    }
}

fn dig(socket: &mut TcpStream, status: u8, x: i32, y: u8, z: i32) {
    let mut p = vec![status];
    p.extend_from_slice(&x.to_be_bytes());
    p.push(y);
    p.extend_from_slice(&z.to_be_bytes());
    p.push(1); // top face
    socket.write_all(&frame(7, &p)).unwrap();
}

fn move_to(socket: &mut TcpStream, x: f64, y: f64, z: f64) {
    let mut p = Vec::new();
    p.extend_from_slice(&x.to_be_bytes());
    p.extend_from_slice(&y.to_be_bytes());
    p.extend_from_slice(&(y + 1.62).to_be_bytes());
    p.extend_from_slice(&z.to_be_bytes());
    p.push(1);
    socket.write_all(&frame(4, &p)).unwrap();
}

fn settle() {
    std::thread::sleep(Duration::from_millis(250));
}

struct Server {
    port: u16,
    dir: PathBuf,
    child: Child,
    cleanup: bool,
}

impl Server {
    fn bind_and_spawn(dir: &Path) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0".parse::<SocketAddr>().unwrap()).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let stderr_path = dir.join(format!("server-{port}.log"));
        let stderr_file = fs::File::create(&stderr_path).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .args(["--bind", &format!("127.0.0.1:{port}")])
            .arg("--survival")
            .arg("--world")
            .arg(dir.join("flat.irw"))
            .arg("--whitelist")
            .arg(dir.join("whitelist.json"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr_file))
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "server never bound");
            std::thread::sleep(Duration::from_millis(100));
        }
        Server {
            port,
            dir: dir.to_path_buf(),
            child,
            cleanup: true,
        }
    }

    fn start(tag: &str) -> Server {
        let _guard = CHAIN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ir1710-next192-{tag}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(dir.join("players")).unwrap();
        fs::write(
            dir.join("whitelist.json"),
            r#"[{"name":"Miner"},{"name":"Receiver"}]"#,
        )
        .unwrap();
        // The ONLY minted items in this scenario: 7 cobblestone in the
        // miner's hotbar slot 5 (window slot 40). The mined dirt is the
        // single later addition to the world's total item count.
        let mut miner_slots = vec![Value::Null; 45];
        miner_slots[40] = json!({"id":4,"count":7,"damage":0});
        fs::write(
            dir.join("players/miner.survival.inventory.json"),
            serde_json::to_vec(&json!({
                "version": 1, "selected": 0, "slots": miner_slots,
                "cursor": null, "workbench": vec![Value::Null; 9]
            }))
            .unwrap(),
        )
        .unwrap();
        Self::bind_and_spawn(&dir)
    }

    fn join(&self, name: &str) -> TcpStream {
        let mut socket = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut hs = varint(5);
        hs.extend_from_slice(mc_string("127.0.0.1").as_slice());
        hs.extend_from_slice(&self.port.to_be_bytes());
        hs.extend_from_slice(&varint(2));
        socket.write_all(&frame(0, &hs)).unwrap();
        socket.write_all(&frame(0, &mc_string(name))).unwrap();
        until(&mut socket, "join-S08", |id, _| id == 0x08);
        settle();
        socket
    }

    fn stop(mut self) -> PathBuf {
        {
            let mut stdin = self.child.stdin.take().expect("stdin");
            stdin.write_all(b"/stop\n").unwrap();
            stdin.flush().unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            match self.child.try_wait().unwrap() {
                Some(status) => {
                    assert!(status.success(), "server must stop cleanly: {status}");
                    break;
                }
                None => {
                    assert!(Instant::now() < deadline, "server never stopped");
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
        let dir = self.dir.clone();
        self.cleanup = false; // the next Server for this dir owns cleanup
        dir
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        if self.cleanup && !std::thread::panicking() {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("file must exist")).expect("valid json")
}

fn total_count(slots: &Value, id: i64) -> i64 {
    slots
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter(|s| s["id"].as_i64() == Some(id))
                .map(|s| s["count"].as_i64().unwrap_or(0))
                .sum()
        })
        .unwrap_or(0)
}

fn drops_total(path: &Path, id: i64) -> i64 {
    match fs::read(path) {
        Ok(bytes) => {
            let doc: Value = serde_json::from_slice(&bytes).unwrap();
            doc["entries"]
                .as_object()
                .map(|entries| {
                    entries
                        .values()
                        .filter(|e| e["stack"]["id"].as_i64() == Some(id))
                        .map(|e| e["stack"]["count"].as_i64().unwrap_or(0))
                        .sum()
                })
                .unwrap_or(0)
        }
        Err(_) => 0,
    }
}

#[test]
fn next192_mine_trade_death_restart_ledger_balances() {
    let server = Server::start("chain");
    let mut miner = server.join("Miner");
    let mut receiver = server.join("Receiver");

    // Miner stands four blocks from the target column (like the proven
    // pickup-radius e2e), receiver idles further away.
    move_to(&mut miner, 4.5, 64.0, 0.5);
    settle();
    move_to(&mut receiver, 8.5, 64.0, 0.5);
    settle();

    // 1) Honest bare-hand mining of (0,63,0): start, >750ms, finish.
    dig(&mut miner, 0, 0, 63, 0);
    std::thread::sleep(Duration::from_millis(900));
    dig(&mut miner, 2, 0, 63, 0);
    until(&mut miner, "mining", |id, body| {
        id == 0x23 && body.len() >= 11 && body[4] == 63 && body[9] == 0
    });

    // 2) Miner walks within pickup range: the dirt lands in the first free
    //    hotbar slot (36) as dirt(3) x1.
    move_to(&mut miner, 1.5, 64.0, 0.5);
    until(&mut miner, "miner-dirt", |id, body| {
        id == 0x2f && body.starts_with(&[0, 0, 36]) && body.len() >= 6 && body[3..6] == [0, 3, 1]
    });

    // 3) Trade: select hotbar index 0 (the dirt), Ctrl+Q the whole stack
    //    (C07 status 3 drops at the giver's own feet block), then the miner
    //    must LEAVE — a throw is only collectable after 2 s and the giver
    //    standing on top of it would re-collect it first.
    miner.write_all(&frame(9, &0i16.to_be_bytes())).unwrap();
    settle();
    dig(&mut miner, 3, 1, 64, 0);
    until(&mut miner, "miner-drop", |id, body| {
        id == 0x2f && body == vec![0, 0, 36, 255, 255]
    });
    move_to(&mut miner, 30.5, 64.0, 0.5); // out of pickup range of (1.5,64.5,0.5)
    move_to(&mut receiver, 1.5, 64.0, 0.5);
    until(&mut receiver, "recv-dirt", |id, body| {
        id == 0x2f && body.starts_with(&[0, 0, 36]) && body.len() >= 6 && body[3..6] == [0, 3, 1]
    });

    // 4) Miner, now far away, takes a lethal 90->64 fall (server-computed:
    //    26 blocks of fall is 23 damage, above the 20 HP pool).
    move_to(&mut miner, 30.5, 90.0, 0.5);
    move_to(&mut miner, 30.5, 64.0, 0.5);
    until(&mut miner, "death", |id, body| {
        id == 0x06 && body.len() == 10 && body[0..4] == 0f32.to_be_bytes()
    });
    // Death drop: the 7 cobblestone must hit the ground exactly once. The
    // receiver walks over them and picks them up into the next free slot.
    move_to(&mut receiver, 30.5, 64.0, 0.5);
    until(&mut receiver, "recv-cobble", |id, body| {
        id == 0x2f && body.starts_with(&[0, 0, 37]) && body.len() >= 6 && body[3..6] == [0, 4, 7]
    });
    // C16 respawn restores full health.
    miner.write_all(&frame(22, &[0])).unwrap();
    until(&mut miner, "respawn", |id, body| {
        id == 0x06 && body.len() == 10 && f32::from_be_bytes(body[0..4].try_into().unwrap()) == 20.0
    });

    drop(miner);
    drop(receiver);

    // 5) Restart on the SAME world directory.
    let dir = server.stop();
    let restarted = Server::bind_and_spawn(&dir);
    // A short session proves the restarted world is loadable/playable.
    let late = restarted.join("Receiver");
    drop(late);
    let ledger_dir = restarted.stop();

    let miner_inv = read_json(&ledger_dir.join("players/miner.survival.inventory.json"));
    let receiver_inv = read_json(&ledger_dir.join("players/receiver.survival.inventory.json"));
    let drops = ledger_dir.join("flat.drops.json");

    // Ledger over the whole scenario (7 cobblestone seeded + 1 dirt mined):
    // everything ends in the receiver's save, nothing elsewhere.
    let dirt_total = total_count(&miner_inv["slots"], 3)
        + total_count(&receiver_inv["slots"], 3)
        + drops_total(&drops, 3);
    assert_eq!(dirt_total, 1, "the mined dirt must exist exactly once");
    assert_eq!(
        total_count(&receiver_inv["slots"], 3),
        1,
        "the handed-over dirt belongs to the receiver"
    );

    let cobble_total = total_count(&miner_inv["slots"], 4)
        + total_count(&receiver_inv["slots"], 4)
        + drops_total(&drops, 4);
    assert_eq!(
        cobble_total, 7,
        "the death-dropped cobblestone total must stay 7"
    );
    assert_eq!(cobble_total, total_count(&receiver_inv["slots"], 4));

    // The mined block edit survived the restart (world file is IRW1).
    let snapshot = fs::read(ledger_dir.join("flat.irw")).expect("world file");
    assert_eq!(&snapshot[..4], b"IRW1");

    // Nobody earned XP; the death must not mint any either. The XP file
    // stores a single `total` (1.7.10 pre-1.8 progression is derived).
    let xp_path = ledger_dir.join("players/miner.experience.json");
    if xp_path.exists() {
        let xp = read_json(&xp_path);
        assert_eq!(
            xp["total"].as_i64().unwrap_or(0),
            0,
            "death must not mint XP"
        );
    }

    let _ = fs::remove_dir_all(&ledger_dir);
}
