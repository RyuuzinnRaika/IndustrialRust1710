//! Real loopback connection with a deterministic, test-only write failure.
use super::*;
use std::fs;
use std::io::Read;
use std::net::Shutdown;
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

struct Scratch {
    path: PathBuf,
    safe_to_remove: bool,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if self.safe_to_remove {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn context(root: &Path) -> ServerContext {
    ServerContext {
        hub: Arc::new(Hub::default()),
        login_names: Arc::new(LoginIdentityRegistry::default()),
        world: Arc::new(Mutex::new(World::new())),
        dimension_worlds: None,
        path: Arc::new(root.join("flat.irw")),
        whitelist_path: Arc::new(root.join("whitelist.json")),
        administration: Arc::new(Administration::new(root.to_path_buf()).unwrap()),
        clock: Arc::new(Clock::default()),
        ground_items: Arc::new(Mutex::new(GroundItems::default())),
        ground_path: Arc::new(root.join("ground.json")),
        mobs: Arc::new(Mutex::new(Mobs::default())),
        throwables: Arc::new(Mutex::new(Throwables::default())),
        boats: Arc::new(Mutex::new(Boats::default())),
        minecarts: Arc::new(Mutex::new(Minecarts::default())),
        fishing: Arc::new(Mutex::new(Fishing::default())),
        chests: Arc::new(Mutex::new(Chests::default())),
        chest_path: Arc::new(root.join("chests.json")),
        furnaces: Arc::new(Mutex::new(Furnaces::default())),
        furnace_path: Arc::new(root.join("furnaces.json")),
        hoppers: Arc::new(Mutex::new(Hoppers::default())),
        hopper_path: Arc::new(root.join("hoppers.json")),
        devices: Arc::new(Mutex::new(Devices::default())),
        device_path: Arc::new(root.join("devices.json")),
        brewers: Arc::new(Mutex::new(Brewers::default())),
        brewer_path: Arc::new(root.join("brewers.json")),
        decorations: Arc::new(Mutex::new(Decorations::default())),
        decoration_path: Arc::new(root.join("decorations.json")),
        maps: Arc::new(Mutex::new(Maps::default())),
        map_path: Arc::new(root.join("maps.json")),
        hangings: Arc::new(Mutex::new(Hangings::default())),
        hanging_path: Arc::new(root.join("hangings.json")),
        survival: false,
        hardcore: false,
        // Production default; this store has no operators, so the square is
        // automatically disabled (spawn protection is not under test here).
        spawn_protection: server_config::DEFAULT_SPAWN_PROTECTION,
        difficulty: Arc::new(AtomicU8::new(1)),
        game_rules: Arc::new(Mutex::new(GameRules::default())),
        scoreboard: Arc::new(Mutex::new(Scoreboard::default())),
        scoreboard_path: Arc::new(root.join("scoreboard.json")),
        online_mode: false,
        auth_key: None,
    }
}

struct RecordingRead<'a> {
    stream: &'a mut TcpStream,
    bytes: &'a mut Vec<u8>,
}

impl Read for RecordingRead<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = self.stream.read(output)?;
        self.bytes.extend_from_slice(&output[..count]);
        Ok(count)
    }
}

fn write_packet(stream: &mut TcpStream, id: u32, body: &[u8]) -> io::Result<()> {
    let mut packet = Vec::new();
    write_varint(id, &mut packet);
    packet.extend_from_slice(body);
    write_frame(stream, &packet)
}

#[test]
fn partial_s21_over_tcp_has_no_disconnect_and_cleanup_closes_socket() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp_root = fs::canonicalize(std::env::temp_dir()).unwrap();
    let root = temp_root.join(format!(
        "ir1710-join-partial-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&root).unwrap();
    let root = fs::canonicalize(&root).unwrap();
    assert_eq!(root.parent(), Some(temp_root.as_path()));
    let mut scratch = Scratch {
        path: root,
        safe_to_remove: false,
    };
    fs::write(
        scratch.path.join("whitelist.json"),
        r#"[{"name":"FrameTest"}]"#,
    )
    .unwrap();
    let context = context(&scratch.path);
    let hub = Arc::clone(&context.hub);
    let player_path = PlayerPosition::location(&context.path, "FrameTest");
    let inventory_path = Inventory::location(&player_path);
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let mut client = TcpStream::connect(address).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    client
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let (accepted, _) = listener.accept().unwrap();
    let limiter = Arc::new(ConnectionLimiter::default());
    let permit = limiter.acquire(address.ip()).unwrap();
    let (fault_tx, fault_rx) = mpsc::sync_channel(1);
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let server = thread::spawn(move || {
        TEST_PARTIAL_S21.with(|slot| *slot.borrow_mut() = Some(fault_tx));
        let result = handle(accepted, context, permit);
        let _ = done_tx.send(result);
    });

    let outcome = (|| -> Result<(), String> {
        let io_error = |e: io::Error| e.to_string();
        let mut handshake = Vec::new();
        write_varint(5, &mut handshake);
        write_string("localhost", &mut handshake).map_err(io_error)?;
        handshake.extend_from_slice(&address.port().to_be_bytes());
        write_varint(2, &mut handshake);
        write_packet(&mut client, 0, &handshake).map_err(io_error)?;
        let mut login = Vec::new();
        write_string("FrameTest", &mut login).map_err(io_error)?;
        write_packet(&mut client, 0, &login).map_err(io_error)?;

        let (mut login_seen, mut join_seen) = (false, false);
        let mut partial = None;
        for _ in 0..64 {
            let mut raw = Vec::new();
            let result = read_frame(&mut RecordingRead {
                stream: &mut client,
                bytes: &mut raw,
            });
            match result {
                Ok(frame) => {
                    let mut body = frame.as_slice();
                    let id = read_varint(&mut body).map_err(io_error)?;
                    if !login_seen {
                        if id != 0x02 {
                            return Err(format!("login did not succeed: {id:#x}"));
                        }
                        login_seen = true;
                    } else if id == 0x01 {
                        join_seen = true;
                    } else if id == 0x40 {
                        return Err("unexpected complete play disconnect".into());
                    }
                }
                Err(error) if error.kind() == ErrorKind::UnexpectedEof => {
                    partial = Some(raw);
                    break;
                }
                Err(error) => return Err(format!("reading initial frames: {error}")),
            }
        }
        if !login_seen || !join_seen {
            return Err(format!(
                "missing login or join: login={login_seen}, join={join_seen}"
            ));
        }
        fault_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|e| format!("fault hook not reached: {e}"))?;
        let partial = partial.ok_or("no EOF after partial frame")?;
        let mut remaining = partial.as_slice();
        let declared = read_varint(&mut remaining).map_err(io_error)?;
        if declared <= 1 || remaining != [0x21] {
            return Err(format!("partial S21 contained extra bytes or wrong length: declared={declared}, remaining={remaining:?}"));
        }
        Ok(())
    })();

    let _ = client.shutdown(Shutdown::Both);
    drop(client);
    let completed = done_rx.recv_timeout(Duration::from_secs(20));
    if completed.is_err() {
        drop(server);
        panic!(
            "server did not finish; preserved test directory {}",
            scratch.path.display()
        );
    }
    let result = completed.unwrap();
    server.join().unwrap();
    scratch.safe_to_remove = true;
    let error = result.expect_err("injected write failure must be returned");
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    assert!(error
        .to_string()
        .contains("join initialization: injected after partial S21"));
    assert_eq!(hub.count().unwrap(), 0, "admission must be released");
    assert!(
        player_path.is_file(),
        "player cleanup must save the disposable fixture"
    );
    assert!(
        inventory_path.is_file(),
        "inventory cleanup must save the disposable fixture"
    );
    assert!(PlayerPosition::load_state_with_dimension(&player_path)
        .unwrap()
        .0
        .is_valid());
    outcome.unwrap();
    fs::remove_dir_all(&scratch.path).unwrap();
    assert!(!scratch.path.exists(), "temporary fixture was not removed");
    scratch.safe_to_remove = false;
}
