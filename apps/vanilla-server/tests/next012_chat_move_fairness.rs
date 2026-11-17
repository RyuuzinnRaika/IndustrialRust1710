//! NEXT-012 / #995: a Chat flood on one connection must not starve the
//! movement of other sessions, and the spam filter must stay bounded.
//! Against the REAL server binary (isolated scratch world, creative):
//!
//!   1. a flood connection joins, then sends the same Chat message
//!      repeatedly: the first copy is broadcast, the repeats are rejected,
//!      and the third rejection disconnects that connection with Play
//!      Disconnect reason "Repeated chat spam" — the flood cannot continue
//!      indefinitely;
//!   2. while the flood runs, an unrelated mover session interleaves Chat
//!      and position packets: every step is ACCEPTED (no S08 correction),
//!      and the observer session still receives the mover's EntityTeleport
//!      (0x18) fan-out for every step (movement is never starved behind
//!      the flood traffic);
//!   3. the observer sees each unique mover message once and the flooded
//!      message exactly once (repeats were dropped before broadcast).
//!
//! Real 1.7.10 client acceptance: NOT_RUN (headless host; the packets are
//! protocol-level and verified directly on the wire).
#[path = "support/packet_client.rs"]
mod packet_client;

use legacy_protocol::{write_string, write_varint};
use packet_client::BufferedClient;
use std::collections::HashMap;
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Server {
    child: Child,
    root: std::path::PathBuf,
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
    fn start(tag: &str) -> Server {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "ir1710-next012-{tag}-{}-{stamp}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("whitelist.json"),
            r#"[{"name":"FairMover"},{"name":"FairObserver"},{"name":"SpamFlood"}]"#,
        )
        .unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .args(["--bind", &format!("127.0.0.1:{port}"), "--world"])
            .arg(root.join("flat.irw"))
            .arg("--whitelist")
            .arg(root.join("whitelist.json"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "server did not open {port}");
            thread::sleep(Duration::from_millis(100));
        }
        Server { child, root, port }
    }

    fn connect(&self, name: &str) -> BufferedClient {
        let mut socket = BufferedClient::new(TcpStream::connect(("127.0.0.1", self.port)).unwrap());
        socket
            .set_read_timeout(Some(Duration::from_millis(250)))
            .unwrap();
        let mut handshake = Vec::new();
        write_varint(5, &mut handshake);
        write_string("127.0.0.1", &mut handshake).unwrap();
        handshake.extend_from_slice(&self.port.to_be_bytes());
        write_varint(2, &mut handshake);
        socket.write_all(&frame(0, &handshake)).unwrap();
        socket.write_all(&frame(0, &mc_string(name))).unwrap();
        socket
    }
}

fn vi(value: u32) -> Vec<u8> {
    let mut out = Vec::new();
    write_varint(value, &mut out);
    out
}

fn mc_string(value: &str) -> Vec<u8> {
    let mut out = Vec::new();
    write_string(value, &mut out).unwrap();
    out
}

fn frame(id: u32, data: &[u8]) -> Vec<u8> {
    let mut packet = vi(id);
    packet.extend_from_slice(data);
    let mut message = vi(packet.len() as u32);
    message.extend_from_slice(&packet);
    message
}

/// Read one frame, treating a read timeout as "no packet right now".
#[track_caller]
fn pump(socket: &mut BufferedClient) -> Option<(u32, Vec<u8>)> {
    match socket.poll_packet() {
        Ok(packet) => packet,
        Err(error) => panic!("unexpected socket error: {error}"),
    }
}

/// Drain the join burst (echoing KeepAlive) until S08; return entity id and
/// authoritative position from the join sequence.
fn await_join(socket: &mut BufferedClient, who: &str) -> (i32, [f64; 3]) {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut entity_id = None;
    loop {
        assert!(Instant::now() < deadline, "{who}: join never completed");
        let Some((id, body)) = pump(socket) else {
            continue;
        };
        match id {
            0x00 if body.len() == 4 => {
                let _ = socket.write_all(&frame(0, &body));
            }
            0x01 if body.len() >= 6 => {
                entity_id = Some(i32::from_be_bytes(body[0..4].try_into().unwrap()));
            }
            0x08 if body.len() >= 24 => {
                let position = [
                    f64::from_be_bytes(body[0..8].try_into().unwrap()),
                    f64::from_be_bytes(body[8..16].try_into().unwrap()) - f64::from(1.62_f32),
                    f64::from_be_bytes(body[16..24].try_into().unwrap()),
                ];
                return (entity_id.expect("JoinGame before first S08"), position);
            }
            _ => {}
        }
    }
}

/// C04 absolute position wire order: X, feet Y, stance, Z, onGround.
fn position_packet(x: f64, feet_y: f64, z: f64) -> Vec<u8> {
    let mut body = Vec::new();
    for value in [x, feet_y, feet_y + 1.62, z] {
        body.extend_from_slice(&value.to_be_bytes());
    }
    body.push(1);
    frame(4, &body)
}

fn chat_packet(message: &str) -> Vec<u8> {
    frame(1, &mc_string(message))
}

#[test]
fn next012_packet_reader_retains_partial_frame_across_timeout() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let payload = vec![0; 32 * 1024];
    let message = frame(0x21, &payload);
    let (prefix_written, prefix_ready) = std::sync::mpsc::sync_channel(1);
    let (release_tail, tail_released) = std::sync::mpsc::sync_channel(1);
    let (finish, finished) = std::sync::mpsc::sync_channel(1);
    let peer = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_nodelay(true).unwrap();
        // The complete length and ID arrive, followed by only part of the
        // body. The remainder cannot arrive until both polls below finish.
        stream.write_all(&message[..32]).unwrap();
        prefix_written.send(()).unwrap();
        if tail_released.recv_timeout(Duration::from_secs(5)).is_ok() {
            stream.write_all(&message[32..]).unwrap();
            // Keep EOF from obscuring the framing assertion on either side.
            let _ = finished.recv_timeout(Duration::from_secs(5));
        }
    });
    let mut client = BufferedClient::new(TcpStream::connect(address).unwrap());
    client
        .set_read_timeout(Some(Duration::from_millis(30)))
        .unwrap();
    prefix_ready.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(pump(&mut client).is_none());
    // A resumable reader may first report an incomplete read. With the peer
    // still gated, this next poll necessarily crosses a receive timeout too.
    assert!(pump(&mut client).is_none());
    release_tail.send(()).unwrap();

    let deadline = Instant::now() + Duration::from_secs(3);
    let packet = loop {
        assert!(
            Instant::now() < deadline,
            "fragmented frame never completed"
        );
        if let Some(packet) = pump(&mut client) {
            break packet;
        }
    };
    assert_eq!(packet, (0x21, payload));
    finish.send(()).unwrap();
    peer.join().unwrap();
}

#[test]
fn next012_chat_flood_cannot_starve_other_sessions_movement() {
    let mut server = Server::start("fairness");
    let mut mover = server.connect("FairMover");
    let (mover_id, mover_pos) = await_join(&mut mover, "FairMover");
    let mut observer = server.connect("FairObserver");
    let (_observer_id, _) = await_join(&mut observer, "FairObserver");

    // Mover steps +X while interleaving unique chat. Observer collects the
    // mover's 0x18 fan-out; a flood connection hammers duplicate chat on
    // the same server concurrently (separate socket, separate thread).
    let mut flood = server.connect("SpamFlood");
    await_join(&mut flood, "SpamFlood");
    let flood_end = Instant::now() + Duration::from_secs(6);
    let mut flood_closed = false;
    while Instant::now() < flood_end && !flood_closed {
        if flood.write_all(&chat_packet("spam spam spam")).is_err() {
            // A failed write after the server's disconnect close is the
            // same fail-closed outcome as observing EOF.
            flood_closed = true;
            break;
        }
        let _ = flood.flush();
        flood
            .set_read_timeout(Some(Duration::from_millis(120)))
            .unwrap();
        match flood.read(&mut [0u8; 4096]) {
            Ok(0) => flood_closed = true,
            Ok(_) => {}
            Err(error)
                if error.kind() == ErrorKind::WouldBlock || error.kind() == ErrorKind::TimedOut => {
            }
            Err(error)
                if error.kind() == ErrorKind::ConnectionReset
                    || error.kind() == ErrorKind::BrokenPipe =>
            {
                flood_closed = true;
            }
            Err(error) => panic!("flood socket error: {error}"),
        }
    }
    assert!(flood_closed, "spam flood connection was never disconnected");
    drop(flood);

    let mut teleports: HashMap<i32, Vec<(i32, i32)>> = HashMap::new();
    let mut chats: HashMap<String, usize> = HashMap::new();
    let mut mover_saw_correction = false;
    mover
        .set_read_timeout(Some(Duration::from_millis(250)))
        .unwrap();

    let mut x = mover_pos[0];
    for step in 1..=6u8 {
        // Interleave Chat and Move on the mover session: 0x01 then 0x06/0x04.
        if step <= 4 {
            mover
                .write_all(&chat_packet(&format!("fair step {step}")))
                .unwrap();
        }
        x += 0.5;
        mover
            .write_all(&position_packet(x, mover_pos[1], mover_pos[2]))
            .unwrap();
        let _ = mover.flush();
        thread::sleep(Duration::from_millis(60));
        // Drain both sockets while the server processes.
        while let Some((id, body)) = pump(&mut mover) {
            if id == 0x00 && body.len() == 4 {
                let _ = mover.write_all(&frame(0, &body));
            }
            if id == 0x08 {
                mover_saw_correction = true;
            }
        }
        while let Some((id, body)) = pump(&mut observer) {
            match id {
                0x00 if body.len() == 4 => {
                    let _ = observer.write_all(&frame(0, &body));
                }
                0x18 if body.len() >= 18 => {
                    let id32 = i32::from_be_bytes(body[0..4].try_into().unwrap());
                    let tx = i32::from_be_bytes(body[4..8].try_into().unwrap());
                    let tz = i32::from_be_bytes(body[12..16].try_into().unwrap());
                    teleports.entry(id32).or_default().push((tx, tz));
                }
                0x02 => {
                    let text = String::from_utf8_lossy(&body).into_owned();
                    *chats.entry(text).or_default() += 1;
                }
                _ => {}
            }
        }
    }

    // Movement fan-out: the observer saw the mover's position advance.
    let steps = teleports.get(&mover_id).map_or(0, Vec::len);
    assert!(
        steps >= 4,
        "observer must keep receiving mover teleports during/after the flood, got {steps}"
    );
    let xs: Vec<f64> = teleports[&mover_id]
        .iter()
        .map(|(tx, _)| f64::from(*tx) / 32.0)
        .collect();
    let max_x = xs.iter().cloned().fold(f64::MIN, f64::max);
    assert!(
        max_x >= mover_pos[0] + 1.5,
        "mover teleports must track the stepped x positions: {xs:?} from {}",
        mover_pos[0]
    );
    assert!(
        !mover_saw_correction,
        "legitimate interleaved movement must not be rejected with S08"
    );

    // Chat fairness: every unique mover message broadcast exactly once;
    // the flooded message was broadcast at most once (repeats dropped).
    for step in 1..=4u8 {
        let needle = format!("fair step {step}");
        let count = chats
            .iter()
            .filter(|(text, _)| text.contains(&needle))
            .map(|(_, count)| count)
            .sum::<usize>();
        assert_eq!(
            count, 1,
            "chat {needle:?} must broadcast exactly once, saw {count}"
        );
    }
    let spam = chats
        .iter()
        .filter(|(text, _)| text.contains("spam spam spam"))
        .map(|(_, count)| count)
        .sum::<usize>();
    assert!(
        spam <= 1,
        "flood repeats must be dropped before broadcast, saw {spam}"
    );

    // Clean stop proves the mover session survived the whole flood.
    {
        let mut stdin = server.child.stdin.take().expect("server stdin");
        stdin.write_all(b"/stop\n").unwrap();
        stdin.flush().unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match server.child.try_wait().unwrap() {
            Some(status) => {
                assert!(status.success(), "server must stop cleanly: {status}");
                break;
            }
            None => {
                assert!(Instant::now() < deadline, "server did not stop");
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
    let _ = fs::remove_dir_all(&server.root);
}
