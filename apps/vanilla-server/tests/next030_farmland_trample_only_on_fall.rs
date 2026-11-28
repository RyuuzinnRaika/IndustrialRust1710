//! NEXT-030 / #1013: farmland changes and syncs ONLY when the landing
//! condition is met. Uses the native Rust server in Survival with a raised
//! farmland shelf (its collision top is 15/16, so a supported landing rests
//! at cell + 0.9375 — the exact height the trample probe must resolve back
//! to the farmland cell, not the cell beneath it). Every cell is planted,
//! because the ecology tick deliberately reverts DRY EMPTY farmland to dirt
//! (that is the server's documented rule, not a trample). The wire contract
//! pinned here: walking (even across the planted shelf) and sub-1.0 drops
//! never emit a block update; a ≥1.0 block landing on farmland —
//! client-reported OR server-side idle gravity — tramples exactly that cell
//! 60→3 with exactly one 0x23; landing on a trampled or non-farmland cell
//! stays silent; and the trample write never names the crop cell above the
//! soil. Unmodified Minecraft 1.7.10 client acceptance remains untested
//! here; disposable data only.

#[path = "support/packet_client.rs"]
mod packet_client;

use legacy_protocol::{write_frame, write_string, write_varint};
use packet_client::BufferedClient;
use std::fs::{self, File};
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Feet height of a player standing on a farmland cell top (15/16).
const FARM_TOP: f64 = 64.9375;

struct Server {
    child: Child,
    root: PathBuf,
    log: PathBuf,
    port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        if !thread::panicking() {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

impl Server {
    fn start() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "industrialrust-next030-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("whitelist.json"), r#"[{"name":"Trampler"}]"#).unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let log = root.join("server.log");
        let output = File::create(&log).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .current_dir(&root)
            .args(["--bind", &format!("127.0.0.1:{port}")])
            .arg("--survival")
            .arg("--world")
            .arg(root.join("flat.irw"))
            .arg("--whitelist")
            .arg(root.join("whitelist.json"))
            .stdin(Stdio::piped())
            .stdout(Stdio::from(output.try_clone().unwrap()))
            .stderr(Stdio::from(output))
            .spawn()
            .unwrap();
        let mut server = Self {
            child,
            root,
            log,
            port,
        };
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            if let Some(status) = server.child.try_wait().unwrap() {
                panic!(
                    "server exited before listening: {status}\n{}",
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

    fn console(&mut self, command: &str) {
        self.child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(format!("{command}\n").as_bytes())
            .unwrap();
    }

    fn join(&self, name: &str) -> (BufferedClient, [f64; 3]) {
        let mut socket = BufferedClient::new(TcpStream::connect(("127.0.0.1", self.port)).unwrap());
        socket.set_nodelay(true).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(250)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut handshake = Vec::new();
        write_varint(5, &mut handshake);
        write_string("localhost", &mut handshake).unwrap();
        handshake.extend_from_slice(&self.port.to_be_bytes());
        write_varint(2, &mut handshake);
        socket.write_all(&packet(0, &handshake)).unwrap();
        let mut start = Vec::new();
        write_string(name, &mut start).unwrap();
        socket.write_all(&packet(0, &start)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            assert!(Instant::now() < deadline, "{name}: join timed out");
            let Some((id, body)) = pump(&mut socket) else {
                continue;
            };
            match id {
                0x00 if body.len() == 4 => socket.write_all(&packet(0, &body)).unwrap(),
                0x01 if body.len() >= 6 => {
                    assert_eq!(body[4] & 7, 0, "test session must be Survival");
                }
                0x08 if body.len() == 33 => {
                    let position = [
                        f64::from_be_bytes(body[..8].try_into().unwrap()),
                        f64::from_be_bytes(body[8..16].try_into().unwrap()) - f64::from(1.62_f32),
                        f64::from_be_bytes(body[16..24].try_into().unwrap()),
                    ];
                    return (socket, position);
                }
                0x40 => panic!("{name}: disconnected during join: {body:?}"),
                _ => {}
            }
        }
    }

    fn stop(&mut self) {
        self.child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"/stop\n")
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(
                    status.success(),
                    "shutdown failed: {status}\n{}",
                    fs::read_to_string(&self.log).unwrap_or_default()
                );
                return;
            }
            assert!(Instant::now() < deadline, "server shutdown timed out");
            thread::sleep(Duration::from_millis(40));
        }
    }
}

fn packet(id: u32, body: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();
    write_varint(id, &mut payload);
    payload.extend_from_slice(body);
    let mut framed = Vec::new();
    write_frame(&mut framed, &payload).unwrap();
    framed
}

fn pump(socket: &mut BufferedClient) -> Option<(u32, Vec<u8>)> {
    socket.poll_packet().expect("read Protocol 5 packet")
}

/// C06 Position and Look with onGround=true (feet at the given Y).
fn position(xyz: [f64; 3]) -> Vec<u8> {
    let mut body = Vec::new();
    for value in [xyz[0], xyz[1], xyz[1] + 1.62, xyz[2]] {
        body.extend_from_slice(&value.to_be_bytes());
    }
    body.extend_from_slice(&0.0f32.to_be_bytes());
    body.extend_from_slice(&0.0f32.to_be_bytes());
    body.push(1);
    packet(6, &body)
}

fn feet(s08_body: &[u8]) -> [f64; 2] {
    [
        f64::from_be_bytes(s08_body[..8].try_into().unwrap()),
        f64::from_be_bytes(s08_body[8..16].try_into().unwrap()) - f64::from(1.62_f32),
    ]
}

fn wait_for(
    client: &mut BufferedClient,
    id: u32,
    predicate: impl Fn(&[u8]) -> bool,
    context: &str,
) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "{context}: timed out");
        let Some((packet_id, body)) = pump(client) else {
            continue;
        };
        if packet_id == 0x00 && body.len() == 4 {
            client.write_all(&packet(0, &body)).unwrap();
        }
        if packet_id == 0x40 {
            panic!("{context}: disconnected: {body:?}");
        }
        if packet_id == id && predicate(&body) {
            return body;
        }
    }
}

/// A chat echo follows everything processed earlier on the same stream.
/// Self-throttled below the server's 5-messages-per-5s ChatLimiter so the
/// many small phase barriers never trip the spam filter (forbidden packets
/// queue in the socket during the pause and are drained by the pump below).
fn barrier(
    client: &mut BufferedClient,
    marker: &str,
    reject: impl Fn(u32, &[u8]) -> Option<&'static str>,
) {
    thread::sleep(Duration::from_millis(1100));
    let mut body = Vec::new();
    write_string(marker, &mut body).unwrap();
    client.write_all(&packet(1, &body)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            Instant::now() < deadline,
            "chat barrier {marker:?} timed out"
        );
        let Some((id, body)) = pump(client) else {
            continue;
        };
        if id == 0x00 && body.len() == 4 {
            client.write_all(&packet(0, &body)).unwrap();
        }
        if let Some(reason) = reject(id, &body) {
            panic!("{reason} during {marker:?}: {body:?}");
        }
        if id == 0x40 {
            panic!("disconnected during {marker:?}");
        }
        if id == 0x02 && body.windows(marker.len()).any(|w| w == marker.as_bytes()) {
            return;
        }
    }
}

/// Creative-give a stack (with item damage) and wait for the 0x2f slot echo.
fn give(client: &mut BufferedClient, id: i16, count: u8, damage: i16) {
    let mut give_packet = 36i16.to_be_bytes().to_vec();
    give_packet.extend_from_slice(&id.to_be_bytes());
    give_packet.push(count);
    give_packet.extend_from_slice(&damage.to_be_bytes());
    give_packet.extend_from_slice(&(-1i16).to_be_bytes());
    client.write_all(&packet(0x10, &give_packet)).unwrap();
    wait_for(
        client,
        0x2f,
        |body| body.len() == 10 && body[3..5] == id.to_be_bytes(),
        "slot echo",
    );
}

/// Place a stack (with item damage) on the top face of the clicked block
/// (x, clicked_y, 0) and wait for the 0x23 block update naming the
/// resulting `block` id.
fn place_and_wait(
    client: &mut BufferedClient,
    x: i32,
    item: i16,
    clicked_y: u8,
    block: u8,
    damage: i16,
) {
    let mut place = x.to_be_bytes().to_vec();
    place.push(clicked_y);
    place.extend_from_slice(&0i32.to_be_bytes());
    place.push(1); // top face
    place.extend_from_slice(&item.to_be_bytes());
    place.push(1);
    place.extend_from_slice(&damage.to_be_bytes());
    place.extend_from_slice(&(-1i16).to_be_bytes());
    place.extend_from_slice(&[8, 8, 8]);
    client.write_all(&packet(8, &place)).unwrap();
    wait_for(
        client,
        0x23,
        |body| {
            body.len() == 11
                && body[0..4] == x.to_be_bytes()
                && body[4] == clicked_y + 1
                && body[9] == block
        },
        &format!("block update item {item} at ({x}, {}, 0)", clicked_y + 1),
    );
}

fn trample_event(x: i32, y: u8) -> impl Fn(&[u8]) -> bool {
    move |body: &[u8]| {
        body.len() == 11 && body[0..4] == x.to_be_bytes() && body[4] == y && body[9] == 3
    }
}

/// Any 0x23 that turns a cell into dirt (id 3) is a trample broadcast.
fn reject_trample(id: u32, body: &[u8]) -> Option<&'static str> {
    if id == 0x23 && body.len() == 11 && body[9] == 3 {
        Some("unexpected trample broadcast")
    } else if id == 0x08 {
        Some("movement correction")
    } else if id == 0x06
        && body.len() >= 4
        && f32::from_be_bytes(body[..4].try_into().unwrap()) < 20.0
    {
        Some("unexpected damage")
    } else {
        None
    }
}

/// Send a movement packet and hold the line with a chat barrier.
fn move_and_hold(client: &mut BufferedClient, position_xyz: [f64; 3], marker: &str) {
    client.write_all(&position(position_xyz)).unwrap();
    barrier(client, marker, reject_trample);
}

/// /tp, wait for the authoritative S08 and answer with the exact position
/// so the teleport gate opens. No barrier here: the player stands in mid-air
/// until the caller's next packet, and a throttled pause would let idle
/// gravity legitimately start falling them.
fn teleport(client: &mut BufferedClient, server: &mut Server, target: [f64; 3]) {
    server.console(&format!(
        "/tp Trampler {} {} {}",
        target[0], target[1], target[2]
    ));
    let s08 = wait_for(
        client,
        0x08,
        |body| {
            body.len() == 33
                && (feet(body)[0] - target[0]).abs() <= 1.0 / 16.0
                && (feet(body)[1] - target[1]).abs() <= 1.0 / 16.0
        },
        "teleport S08",
    );
    assert_eq!(&s08[16..24], &target[2].to_be_bytes(), "tp S08 z");
    client.write_all(&position(target)).unwrap();
}

#[test]
fn next030_farmland_tramples_only_on_a_full_block_landing() {
    let mut server = Server::start();
    let (mut client, origin) = server.join("Trampler");
    assert_eq!(origin, [0.5, 64.0, 0.5]);

    // Build a raised shelf of four farmland cells (x = 2..=5 at y 64) and
    // plant wheat on every cell. The farmland is given with moisture
    // metadata 7: the ecology tick deliberately reverts DRY (moisture 0)
    // EMPTY farmland to dirt on a visit (documented rule, not a trample),
    // and a fresh dry cell could otherwise revert inside the setup window.
    // Metadata 7 buys every visit needed to plant the crops, and the third
    // revert branch is then permanently blocked while a crop (59) stands
    // above the soil. All via creative give + place packets; the hoe path
    // is already covered elsewhere.
    server.console("/gamemode creative Trampler");
    wait_for(
        &mut client,
        0x39,
        |body| body.first() == Some(&0x0d),
        "creative abilities",
    );
    give(&mut client, 60, 4, 7);
    for x in 2..=5 {
        place_and_wait(&mut client, x, 60, 63, 60, 7);
    }
    give(&mut client, 59, 4, 0);
    for x in 2..=5 {
        place_and_wait(&mut client, x, 59, 64, 59, 0);
    }
    server.console("/gamemode survival Trampler");
    wait_for(
        &mut client,
        0x39,
        |body| body.first() == Some(&0),
        "survival abilities",
    );

    // Phase 1 — walking onto and across the shelf never changes farmland:
    // no fall, no 0x23, no correction. One chat barrier orders all five
    // movement packets. (A step-up move reports 0.9375 ≤ 1.0 of consumed
    // descent on the planted cell; that sub-threshold landing staying silent
    // is the "only on the fall condition" half of the contract.)
    client.write_all(&position([1.5, 64.0, 0.5])).unwrap();
    client.write_all(&position([2.5, FARM_TOP, 0.5])).unwrap();
    client.write_all(&position([3.5, FARM_TOP, 0.5])).unwrap();
    client.write_all(&position([4.5, FARM_TOP, 0.5])).unwrap();
    client.write_all(&position([5.5, FARM_TOP, 0.5])).unwrap();
    barrier(&mut client, "n030 walk holds", reject_trample);

    // Phase 2 — a ≥1.0 block landing (the real client's feet height on the
    // 15/16 farmland top) tramples exactly the cell stood on, once.
    client.write_all(&position([5.5, 66.1875, 0.5])).unwrap();
    client.write_all(&position([5.5, FARM_TOP, 0.5])).unwrap();
    wait_for(
        &mut client,
        0x23,
        trample_event(5, 64),
        "client-path trample (5, 64, 0)",
    );
    // The trampled cell rose to a full dirt block: step out of it, jump and
    // land again (1.249 ≥ 1.0) — dirt is not farmland, so nothing re-syncs.
    client.write_all(&position([5.5, 66.0, 0.5])).unwrap();
    client.write_all(&position([5.5, 66.249, 0.5])).unwrap();
    client.write_all(&position([5.5, 65.0, 0.5])).unwrap();
    barrier(&mut client, "n030 no re-trample", reject_trample);

    // Phase 3 — a sub-1.0 drop onto intact farmland (x = 4) leaves it tilled.
    teleport(&mut client, &mut server, [4.5, 65.75, 0.5]);
    move_and_hold(&mut client, [4.5, FARM_TOP, 0.5], "n030 0.8125 drop");

    // Phase 4 — a silent client falls under server-side idle gravity and the
    // landing tramples (3, 64, 0) exactly once, settling feet on the farmland
    // top; afterwards the session holds still without further corrections.
    teleport(&mut client, &mut server, [3.5, 66.5, 0.5]);
    let mut tramples = 0usize;
    let mut descents = 0usize;
    let mut last_feet = [0.0, 0.0];
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(1600) {
        let Some((id, body)) = pump(&mut client) else {
            continue;
        };
        match id {
            0x00 if body.len() == 4 => client.write_all(&packet(0, &body)).unwrap(),
            0x08 if body.len() == 33 => {
                descents += 1;
                last_feet = feet(&body);
            }
            0x23 if trample_event(3, 64)(&body) => tramples += 1,
            0x40 => panic!("disconnected during the idle fall"),
            _ => {}
        }
        if tramples == 1 && descents >= 3 && started.elapsed() > Duration::from_millis(900) {
            break;
        }
    }
    assert_eq!(
        tramples, 1,
        "idle-gravity landing must trample exactly once"
    );
    assert!(descents >= 3, "idle gravity never fell: {descents} steps");
    assert!(
        (last_feet[1] - FARM_TOP).abs() < 1e-6,
        "idle landing settled off the farmland top: {last_feet:?}"
    );
    // Settled: no endless correction loop, no second broadcast.
    let quiet_start = Instant::now();
    while quiet_start.elapsed() < Duration::from_millis(500) {
        let Some((id, body)) = pump(&mut client) else {
            continue;
        };
        match id {
            0x00 if body.len() == 4 => client.write_all(&packet(0, &body)).unwrap(),
            0x08 if body.len() == 33 => {
                panic!("corrected after settling: {:?}", feet(&body))
            }
            0x23 if body.len() == 11 && body[9] == 3 => panic!("second trample broadcast"),
            0x40 => panic!("disconnected after the idle fall"),
            _ => {}
        }
    }

    // Phase 5 — landing with a ≥1.0 drop on the PLANTED cell (x = 2) tramples
    // only the soil: exactly one 0x23 at (2, 64, 0); the trample write never
    // names the crop cell y = 65.
    client.write_all(&position([3.5, 65.0, 0.5])).unwrap();
    client.write_all(&position([2.5, FARM_TOP, 0.5])).unwrap();
    client.write_all(&position([2.5, 66.1875, 0.5])).unwrap();
    client.write_all(&position([2.5, FARM_TOP, 0.5])).unwrap();
    let mut planted_tramples = 0usize;
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(600) {
        let Some((id, body)) = pump(&mut client) else {
            continue;
        };
        match id {
            0x00 if body.len() == 4 => client.write_all(&packet(0, &body)).unwrap(),
            0x23 if trample_event(2, 64)(&body) => planted_tramples += 1,
            0x23 if body.len() == 11 && body[4] == 65 && body[9] == 3 => {
                panic!("trample wrote into the crop cell: {body:?}")
            }
            0x40 => panic!("disconnected during the planted landing"),
            _ => {}
        }
    }
    assert_eq!(
        planted_tramples, 1,
        "a ≥1.0 landing on the planted cell must trample its soil once"
    );

    // Landing with a ≥1.0 drop on ordinary ground next to the shelf changes
    // nothing (the probe cell is dirt, not farmland), and then leave cleanly.
    client.write_all(&position([2.5, 66.0, 0.5])).unwrap();
    client.write_all(&position([0.5, 64.0, 0.5])).unwrap();
    barrier(&mut client, "n030 ground landing silent", reject_trample);
    move_and_hold(&mut client, [0.5, 64.0, 0.5], "n030 settle");

    drop(client);
    server.stop();
}
