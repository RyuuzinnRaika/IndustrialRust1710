//! NEXT-143: mine a seeded cane column through the native server's TCP path.
//! Real unmodified 1.7.10 client acceptance remains separately untested.
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
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

fn until(
    socket: &mut TcpStream,
    tag: &str,
    mut want: impl FnMut(i32, &[u8]) -> bool,
) -> (i32, Vec<u8>) {
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
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let stdout_path = dir.join(format!("server-{stamp}.stdout.log"));
        let stderr_path = dir.join(format!("server-{stamp}.stderr.log"));
        let stdout_file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stdout_path)
            .unwrap();
        let stderr_file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stderr_path)
            .unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .args(["--bind", "127.0.0.1:0"])
            .arg("--survival")
            .arg("--world")
            .arg(dir.join("flat.irw"))
            .arg("--whitelist")
            .arg(dir.join("whitelist.json"))
            .stdin(Stdio::piped())
            .stdout(Stdio::from(stdout_file))
            .stderr(Stdio::from(stderr_file))
            .spawn()
            .unwrap();
        // Own the child before probing readiness so a bind failure or panic
        // cannot leave a test server running.
        let mut server = Server {
            port: 0,
            dir: dir.to_path_buf(),
            child,
            cleanup: true,
        };
        eprintln!(
            "owned test server pid={} fixture={dir:?}",
            server.child.id()
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            assert!(
                server.child.try_wait().unwrap().is_none(),
                "server exited before binding; fixture {dir:?}"
            );
            let log = fs::read_to_string(&stdout_path).unwrap();
            if let Some(address) = log.lines().find_map(|line| {
                line.strip_prefix("IndustrialRust1710: listening on ")
                    .and_then(|tail| tail.split_whitespace().next())
                    .and_then(|text| text.parse::<SocketAddr>().ok())
            }) {
                assert_eq!(address.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
                assert_ne!(address.port(), 0);
                server.port = address.port();
                break;
            }
            assert!(Instant::now() < deadline, "server never bound");
            std::thread::sleep(Duration::from_millis(100));
        }
        server
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
        // Fail on a collision rather than adopting and later deleting a
        // directory that this test did not create.
        fs::create_dir(&dir).unwrap();
        fs::create_dir(dir.join("players")).unwrap();
        fs::write(
            dir.join("whitelist.json"),
            r#"[{"name":"Miner"},{"name":"Receiver"}]"#,
        )
        .unwrap();
        let edits = [
            (0i32, 63u8, 0i32, 3u16 << 4),
            (1, 63, 0, 9u16 << 4),
            (0, 64, 0, 83u16 << 4),
            (0, 65, 0, 83u16 << 4),
            (0, 66, 0, 83u16 << 4),
        ];
        let mut bytes = b"IRW1".to_vec();
        bytes.extend_from_slice(&(edits.len() as u32).to_be_bytes());
        for (x, y, z, block) in edits {
            bytes.extend_from_slice(&x.to_be_bytes());
            bytes.push(y);
            bytes.extend_from_slice(&z.to_be_bytes());
            bytes.extend_from_slice(&block.to_be_bytes());
        }
        fs::write(dir.join("flat.irw"), bytes).unwrap();
        Self::bind_and_spawn(&dir)
    }

    fn join(&mut self, name: &str) -> TcpStream {
        assert!(
            self.child.try_wait().unwrap().is_none(),
            "server exited before join"
        );
        let mut socket = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        assert!(
            self.child.try_wait().unwrap().is_none(),
            "server exited during join"
        );
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
fn next143_real_tcp_mining_cane_stack_persists_three_correct_drops() {
    let mut server = Server::start("cane");
    let mut miner = server.join("Miner");
    move_to(&mut miner, 4.5, 64.0, 0.5);
    settle();
    // Cane has zero hardness and the server handles Start as instant mining.
    dig(&mut miner, 0, 0, 64, 0);
    let mut seen_base = false;
    let mut seen_top = false;
    until(&mut miner, "remove cane column", |id, body| {
        if id == 0x23
            && body.len() >= 11
            && body[0..4] == 0i32.to_be_bytes()
            && body[5..9] == 0i32.to_be_bytes()
            && body[9] == 0
        {
            seen_base |= body[4] == 64;
            seen_top |= body[4] == 66;
        }
        seen_base && seen_top
    });
    drop(miner);
    let dir = server.stop();
    let drops = dir.join("flat.drops.json");
    assert_eq!(drops_total(&drops, 338), 3, "fixture {dir:?}");
    assert_eq!(drops_total(&drops, 83), 0, "fixture {dir:?}");
    let restarted = Server::bind_and_spawn(&dir);
    std::thread::sleep(Duration::from_secs(1));
    let dir = restarted.stop();
    let drops = dir.join("flat.drops.json");
    assert_eq!(drops_total(&drops, 338), 3, "fixture after restart {dir:?}");
    assert_eq!(drops_total(&drops, 83), 0, "fixture after restart {dir:?}");
    let document: Value = serde_json::from_slice(&fs::read(&drops).unwrap()).unwrap();
    assert!(
        document["entries"]
            .as_object()
            .unwrap()
            .values()
            .all(|entry| entry["stack"]["id"].as_i64() == Some(338)
                && entry["stack"]["damage"].as_i64() == Some(0)),
        "incorrect item or damage; fixture {dir:?}"
    );
    let all_items: i64 = document["entries"]
        .as_object()
        .unwrap()
        .values()
        .map(|entry| entry["stack"]["count"].as_i64().unwrap())
        .sum();
    assert_eq!(
        all_items, 3,
        "unexpected items after restart; fixture {dir:?}"
    );
    let bytes = fs::read(dir.join("flat.irw")).unwrap();
    assert_eq!(&bytes[..4], b"IRW1");
    let count = u32::from_be_bytes(bytes[4..8].try_into().unwrap()) as usize;
    for y in 64..=66u8 {
        let found = bytes[8..8 + 11 * count].chunks_exact(11).find(|r| {
            i32::from_be_bytes(r[0..4].try_into().unwrap()) == 0
                && r[4] == y
                && i32::from_be_bytes(r[5..9].try_into().unwrap()) == 0
        });
        assert!(
            found.is_none_or(|r| u16::from_be_bytes(r[9..11].try_into().unwrap()) == 0),
            "remaining cane at y={y}, fixture {dir:?}"
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
