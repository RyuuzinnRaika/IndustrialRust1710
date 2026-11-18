//! Real session, with a test-only pickup gate and manually driven ground tick.
use super::*;
use std::fs;
use std::net::Shutdown;
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

// A panic must not detach a live session into the next test. Fixtures remain
// available for diagnosis on both success and failure.
struct SessionGuard {
    client: TcpStream,
    done: mpsc::Receiver<io::Result<()>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl SessionGuard {
    fn finish(&mut self) -> io::Result<()> {
        self.client.shutdown(Shutdown::Write)?;
        let result = self
            .done
            .recv_timeout(Duration::from_secs(20))
            .map_err(|error| {
                io::Error::new(
                    ErrorKind::TimedOut,
                    format!("session did not finish: {error}"),
                )
            })?;
        self.worker
            .take()
            .unwrap()
            .join()
            .expect("session thread panicked");
        result
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = self.client.shutdown(Shutdown::Both);
            if self.done.recv_timeout(Duration::from_secs(20)).is_err() {
                let _ = writeln!(
                    io::stderr().lock(),
                    "qdrop session has not exited; waiting for its thread"
                );
            }
            if worker.join().is_err() {
                let _ = writeln!(io::stderr().lock(), "qdrop session thread panicked");
            }
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
        ground_path: Arc::new(root.join("flat.drops.json")),
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
        survival: true,
        hardcore: false,
        difficulty: Arc::new(AtomicU8::new(1)),
        game_rules: Arc::new(Mutex::new(GameRules::default())),
        scoreboard: Arc::new(Mutex::new(Scoreboard::default())),
        scoreboard_path: Arc::new(root.join("scoreboard.json")),
        online_mode: false,
        auth_key: None,
    }
}

fn send_packet(stream: &mut TcpStream, id: u32, body: &[u8]) {
    let mut packet = Vec::new();
    write_varint(id, &mut packet);
    packet.extend_from_slice(body);
    write_frame(stream, &packet).unwrap();
}

fn read_packet(stream: &mut TcpStream) -> (u32, Vec<u8>) {
    let packet = read_frame(stream).unwrap();
    let mut body = packet.as_slice();
    let id = read_varint(&mut body).unwrap();
    if id == 0 && body.len() == 4 {
        send_packet(stream, 0, body);
    }
    (id, body.to_vec())
}

fn observe_drop(stream: &mut TcpStream, slot: &[u8], count: u8, prior: Option<i32>) -> i32 {
    let mut slot_seen = false;
    let mut spawned = Vec::new();
    let mut metadata = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(25);
    for _ in 0..160 {
        assert!(Instant::now() < deadline, "drop packet deadline");
        let (id, body) = read_packet(stream);
        if id == 0x2f && body == slot {
            slot_seen = true;
        } else if id == 0x0e {
            let mut object = body.as_slice();
            if let Ok(id) = read_varint(&mut object) {
                if let Ok(id) = i32::try_from(id) {
                    if object.len() == 19
                        && object[0] == 2
                        && object[1..13] == [0, 0, 0, 16, 0, 0, 8, 16, 0, 0, 0, 16]
                        && id > 1_000_000_000
                        && Some(id) != prior
                    {
                        spawned.push(id);
                    }
                }
            }
        } else if id == 0x1c
            && body.len() == 13
            && body[4..] == [0xaa, 0, 3, count, 0, 0, 255, 255, 127]
        {
            metadata.push(i32::from_be_bytes(body[..4].try_into().unwrap()));
        }
        if slot_seen {
            if let Some(id) = spawned.iter().find(|id| metadata.contains(id)) {
                return *id;
            }
        }
    }
    panic!("missing slot, dropped object, or matching item metadata");
}

#[test]
fn sequential_q_and_ctrl_q_packets_persist_before_and_after_merge() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp = fs::canonicalize(std::env::temp_dir()).unwrap();
    let root = temp.join(format!(
        "ir1710-qdrop-session-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&root).unwrap();
    let root = fs::canonicalize(root).unwrap();
    assert_eq!(root.parent(), Some(temp.as_path()));
    eprintln!(
        "qdrop fixture: {}",
        root.file_name().unwrap().to_string_lossy()
    );
    fs::write(root.join("whitelist.json"), r#"[{"name":"QDrop"}]"#).unwrap();
    let context = context(&root);
    let ground = Arc::clone(&context.ground_items);
    let world = Arc::clone(&context.world);
    let ground_path = Arc::clone(&context.ground_path);
    let hub = Arc::clone(&context.hub);
    ground.lock().unwrap().test_hold_pickups(true);
    let player_path = PlayerPosition::location(&context.path, "QDrop");
    let inventory_path = player_path.with_extension("survival.inventory.json");
    let mut inventory = Inventory::default();
    inventory
        .creative_set(
            36,
            Some(Stack {
                id: 3,
                count: 12,
                damage: 0,
                nbt: None,
            }),
        )
        .unwrap();
    inventory.save(&inventory_path).unwrap();

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let client = TcpStream::connect(address).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    client
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let (accepted, _) = listener.accept().unwrap();
    let limiter = Arc::new(ConnectionLimiter::default());
    let permit = limiter.acquire(address.ip()).unwrap();
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        let result = handle(accepted, context, permit);
        let _ = done_tx.send(result);
    });
    let mut session = SessionGuard {
        client,
        done: done_rx,
        worker: Some(worker),
    };

    let mut handshake = Vec::new();
    write_varint(5, &mut handshake);
    write_string("localhost", &mut handshake).unwrap();
    handshake.extend_from_slice(&address.port().to_be_bytes());
    write_varint(2, &mut handshake);
    send_packet(&mut session.client, 0, &handshake);
    let mut login = Vec::new();
    write_string("QDrop", &mut login).unwrap();
    send_packet(&mut session.client, 0, &login);
    assert_eq!(read_packet(&mut session.client).0, 2, "login success");
    let mut joined = false;
    for _ in 0..160 {
        let (id, body) = read_packet(&mut session.client);
        if id == 0x08 {
            assert_eq!(f64::from_be_bytes(body[0..8].try_into().unwrap()), 0.5);
            joined = true;
            break;
        }
    }
    assert!(joined, "missing initial position");

    let mut action = vec![4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255];
    send_packet(&mut session.client, 7, &action);
    let first = observe_drop(
        &mut session.client,
        &[0, 0, 36, 0, 3, 11, 0, 0, 255, 255],
        1,
        None,
    );
    {
        let mut guard = ground.lock().unwrap();
        guard.test_make_pickups_ready();
    }
    // Do not just assume the gate mattered: wait until the live session has
    // attempted to pick up the now-eligible first drop and the gate held it.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if ground.lock().unwrap().test_blocked_pickups() > 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "session never reached pickup gate"
        );
        thread::sleep(Duration::from_millis(10));
    }
    action[0] = 3;
    send_packet(&mut session.client, 7, &action);
    let second = observe_drop(&mut session.client, &[0, 0, 36, 255, 255], 11, Some(first));
    assert!(second > first);
    // Half-close only the client-to-server direction. The session treats a
    // clean EOF as Ok and retains the other direction for any final writes.
    let result = session.finish();
    assert!(result.is_ok(), "session cleanup failed: {result:?}");
    drop(session);
    assert_eq!(hub.count().unwrap(), 0);
    assert!(Inventory::load(&inventory_path).unwrap().held().is_none());

    let mut before = GroundItems::load(&ground_path).unwrap();
    let before_items: Vec<_> = before
        .items()
        .map(|item| (item.id, item.stack.count))
        .collect();
    assert_eq!(before_items, [(first, 1), (second, 11)]);
    assert!(before.items().all(|item| {
        item.position == [16, 2064, 16]
            && item.stack.id == 3
            && item.stack.damage == 0
            && item.stack.nbt.is_none()
    }));
    before.test_make_pickups_ready();
    assert!(
        before.nearby_ready((0.5, 64.0, 0.5)).is_some(),
        "loading defaults to normal pickup"
    );
    {
        let world = world.lock().unwrap();
        let mut ground = ground.lock().unwrap();
        ground.test_make_pickups_ready();
        assert!(ground.nearby_ready((0.5, 64.0, 0.5)).is_none());
        let gated_encoding = ground.encoded().unwrap();
        ground.test_hold_pickups(false);
        assert!(ground.nearby_ready((0.5, 64.0, 0.5)).is_some());
        assert_eq!(
            ground.encoded().unwrap(),
            gated_encoding,
            "gate must not enter persistence"
        );
        let events = ground.advance(&world, &ground_path).unwrap();
        let mut destroyed = vec![1];
        destroyed.extend_from_slice(&second.to_be_bytes());
        let mut merged = first.to_be_bytes().to_vec();
        merged.extend_from_slice(&[0xaa, 0, 3, 12, 0, 0, 255, 255, 127]);
        assert!(events
            .iter()
            .any(|event| event.id == 0x13 && event.body == destroyed));
        assert!(events
            .iter()
            .any(|event| event.id == 0x1c && event.body == merged));
    }
    let after = GroundItems::load(&ground_path).unwrap();
    let after_items: Vec<_> = after
        .items()
        .map(|item| (item.id, item.stack.count))
        .collect();
    assert_eq!(after_items, [(first, 12)]);
    // Retain even successful disposable fixtures for review; no recursive
    // delete is needed during this investigation.
}
