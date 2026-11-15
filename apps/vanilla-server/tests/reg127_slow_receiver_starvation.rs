//! REG-127 / #763: one slow TCP receiver must not starve the other
//! players' state notifications, and must not lose its own. Against the
//! REAL server binary on an isolated scratch world:
//!
//!   1. three sessions join: a SLOW client that drains its socket at
//!      ~256 bytes / 300 ms, a mover that streams 12 position steps, and
//!      an observer that reads normally;
//!   2. while the slow client is still many kilobytes behind (its join
//!      burst has not even drained), the observer must keep receiving
//!      the mover's EntityTeleport (0x18) fan-out: at least 10 of 12
//!      steps with the final position included, and the mover itself
//!      must receive ZERO S08 corrections (its movement was accepted,
//!      never rubber-banded);
//!   3. `/list` on the observer still reports Online (3) — the slow
//!      connection evicted nobody and was not itself cascaded out;
//!   4. when the slow client finally drains its backlog it holds the
//!      mover's LATEST teleport and the broadcast chat queued behind
//!      it (delayed, never starved away);
//!   5. clean /stop afterwards.
//!
//! Companion rejection-side test: multiplayer::tests::
//! queue_overflow_marks_session_unhealthy_instead_of_silent_desync
//! (per-session overflow disconnects only the overflowing session).
//!
//! Real 1.7.10 client acceptance: NOT_RUN (headless host; wire-level
//! verification below).
use legacy_protocol::{read_frame, read_varint, write_string, write_varint};
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
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
            "ir1710-reg127-{tag}-{}-{stamp}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("whitelist.json"),
            r#"[{"name":"Slowpoke"},{"name":"Mover"},{"name":"Observer"}]"#,
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

    fn connect(&self, name: &str) -> TcpStream {
        let mut socket = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
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

fn pump(socket: &mut TcpStream) -> Option<(u32, Vec<u8>)> {
    match read_frame(socket) {
        Ok(packet) => {
            let mut raw = packet.as_slice();
            let id = read_varint(&mut raw).expect("packet id");
            Some((id, raw.to_vec()))
        }
        Err(error)
            if error.kind() == ErrorKind::WouldBlock || error.kind() == ErrorKind::TimedOut =>
        {
            None
        }
        Err(error) => panic!("unexpected socket error: {error}"),
    }
}

/// Drain the join burst (echoing KeepAlive) until S08; return the entity
/// id from JoinGame (S01) and the authoritative position.
fn await_join(socket: &mut TcpStream, who: &str) -> (i32, [f64; 3]) {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut entity_id = None;
    loop {
        assert!(Instant::now() < deadline, "{who}: join never completed");
        let Some((id, body)) = pump(socket) else {
            continue;
        };
        if id == 0x00 && body.len() == 4 {
            let _ = socket.write_all(&frame(0, &body));
        }
        if id == 0x01 && body.len() >= 6 {
            entity_id = Some(i32::from_be_bytes(body[0..4].try_into().unwrap()));
        }
        if id == 0x08 && body.len() >= 24 {
            let position = [
                f64::from_be_bytes(body[0..8].try_into().unwrap()),
                f64::from_be_bytes(body[8..16].try_into().unwrap()) - f64::from(1.62_f32),
                f64::from_be_bytes(body[16..24].try_into().unwrap()),
            ];
            return (entity_id.expect("JoinGame before first S08"), position);
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

/// Parse a raw backlog into (packet id, body) pairs by walking the
/// varint length prefixes. Trailing partial frames are dropped.
fn parse_frames(buf: &[u8]) -> Vec<(u32, Vec<u8>)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < buf.len() {
        let mut len: usize = 0;
        let mut shift = 0;
        loop {
            let Some(&byte) = buf.get(i) else { return out };
            i += 1;
            len |= ((byte & 0x7f) as usize) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                break;
            }
            if shift > 28 {
                return out;
            }
        }
        let Some(end) = i.checked_add(len) else {
            return out;
        };
        if end > buf.len() {
            return out;
        }
        let mut frame_bytes = &buf[i..end];
        i = end;
        let mut id: u32 = 0;
        let mut shift = 0;
        loop {
            let Some((&byte, rest)) = frame_bytes.split_first() else {
                break;
            };
            frame_bytes = rest;
            id |= ((byte & 0x7f) as u32) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                break;
            }
            if shift > 28 {
                break;
            }
        }
        out.push((id, frame_bytes.to_vec()));
    }
    out
}

/// X coordinate of an EntityTeleport (0x18) body for `mover` in fixed /32.
fn teleport_x(body: &[u8], mover: i32) -> Option<i32> {
    let id = i32::from_be_bytes(body.get(0..4)?.try_into().ok()?);
    if id != mover {
        return None;
    }
    Some(i32::from_be_bytes(body.get(4..8)?.try_into().ok()?))
}

#[test]
fn reg127_slow_tcp_receiver_cannot_starve_other_sessions() {
    let mut server = Server::start("starve");
    let mut mover = server.connect("Mover");
    let (mover_id, mover_pos) = await_join(&mut mover, "Mover");

    let mut observer = server.connect("Observer");
    await_join(&mut observer, "Observer");

    // Slowpoke joins, then drains 256 bytes every 300 ms in the
    // background: its backlog (join burst of ~15 chunks) keeps the
    // connection many kilobytes behind the live broadcast stream.
    let mut slowpoke = server.connect("Slowpoke");
    await_join(&mut slowpoke, "Slowpoke");
    slowpoke
        .set_read_timeout(Some(Duration::from_millis(250)))
        .unwrap();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let (buf_tx, buf_rx) = mpsc::channel::<Vec<u8>>();
    let drainer = thread::spawn(move || {
        let mut buf: Vec<u8> = Vec::new();
        let mut tmp = [0u8; 256];
        loop {
            match stop_rx.try_recv() {
                Ok(()) | Err(mpsc::TryRecvError::Disconnected) => break,
                Err(mpsc::TryRecvError::Empty) => {}
            }
            match slowpoke.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
                Err(error)
                    if error.kind() == ErrorKind::WouldBlock
                        || error.kind() == ErrorKind::TimedOut => {}
                Err(_) => break,
            }
            thread::sleep(Duration::from_millis(300));
        }
        // Final fast drain: everything the server queued for us.
        loop {
            match slowpoke.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
                Err(error)
                    if error.kind() == ErrorKind::WouldBlock
                        || error.kind() == ErrorKind::TimedOut =>
                {
                    break
                }
                Err(_) => break,
            }
        }
        let _ = buf_tx.send(buf);
        slowpoke
    });

    // The mover streams 12 steps along +X at walking cadence.
    let mut mover_corrections = 0u32;
    let mut x = mover_pos[0];
    for step in 1..=12u32 {
        x += 0.5;
        mover
            .write_all(&position_packet(x, mover_pos[1], mover_pos[2]))
            .unwrap();
        let _ = mover.flush();
        thread::sleep(Duration::from_millis(60));
        if step % 3 == 0 {
            // Let the server's 50 ms broadcast window elapse and drain
            // the mover socket so its own stream cannot wedge.
            while let Some((id, body)) = pump(&mut mover) {
                if id == 0x00 && body.len() == 4 {
                    let _ = mover.write_all(&frame(0, &body));
                }
                if id == 0x08 {
                    mover_corrections += 1;
                }
            }
        }
    }
    // Final mover drain: the session must have been accepted throughout.
    let mover_deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < mover_deadline {
        let Some((id, body)) = pump(&mut mover) else {
            break;
        };
        if id == 0x00 && body.len() == 4 {
            let _ = mover.write_all(&frame(0, &body));
        }
        if id == 0x08 {
            mover_corrections += 1;
        }
    }

    // The observer drained everything while Slowpoke stayed behind.
    let mut observer_x: Vec<i32> = Vec::new();
    let observer_deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < observer_deadline {
        let Some((id, body)) = pump(&mut observer) else {
            break;
        };
        if id == 0x00 && body.len() == 4 {
            let _ = observer.write_all(&frame(0, &body));
        }
        if id == 0x18 {
            if let Some(tx) = teleport_x(&body, mover_id) {
                observer_x.push(tx);
            }
        }
    }
    let final_fixed = ((mover_pos[0] + 6.0) * 32.0).floor() as i32;
    assert!(
        observer_x.len() >= 10,
        "observer must keep receiving mover teleports during the slow \
         receiver stall, got {}",
        observer_x.len()
    );
    assert!(
        observer_x.windows(2).all(|pair| pair[0] <= pair[1]),
        "teleports must stay ordered: {observer_x:?}"
    );
    assert_eq!(
        observer_x.last().copied(),
        Some(final_fixed),
        "observer must have the mover's final position"
    );
    assert_eq!(
        mover_corrections, 0,
        "legitimate movement must not be corrected while a slow receiver lags"
    );

    // All three sessions are still online: the slow connection evicted
    // nobody and was not itself evicted by cascading pressure.
    observer
        .write_all(&frame(1, &mc_string("/list")))
        .expect("observer chat");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut listed = false;
    while Instant::now() < deadline && !listed {
        let Some((id, body)) = pump(&mut observer) else {
            thread::sleep(Duration::from_millis(50));
            continue;
        };
        listed = id == 0x02 && String::from_utf8_lossy(&body).contains("Online (3)");
    }
    assert!(listed, "all three sessions must still be online");

    // A unique BROADCAST chat (normal messages fan out to every
    // session) queued behind the slow client's backlog; it must arrive
    // in that backlog when drained, proving delayed-but-complete
    // delivery.
    observer
        .write_all(&frame(1, &mc_string("reg127 lag probe")))
        .expect("observer broadcast chat");
    // Let the server's per-session writer threads flush the broadcast
    // into every socket before the slow client's final drain.
    thread::sleep(Duration::from_millis(750));

    // Now let the slow client drain its backlog.
    drop(stop_tx);
    let backlog = buf_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("slow drainer finished");
    let slowpoke = drainer.join().expect("drainer thread");
    let frames = parse_frames(&backlog);
    let slow_x: Vec<i32> = frames
        .iter()
        .filter(|(id, _)| *id == 0x18)
        .filter_map(|(_, body)| teleport_x(body, mover_id))
        .collect();
    assert!(
        slow_x.contains(&final_fixed),
        "the slow client must eventually receive the mover's latest \
         teleport (delayed but never starved); saw {slow_x:?}"
    );
    assert!(
        frames.iter().any(|(id, body)| {
            *id == 0x02 && String::from_utf8_lossy(body).contains("reg127 lag probe")
        }),
        "the slow client's backlog must still contain the broadcast chat \
         queued behind its backlog (delayed, never starved away)"
    );

    // Clean stop with all three sessions connected.
    drop(mover);
    drop(observer);
    drop(slowpoke);
    {
        let mut stdin = server.child.stdin.take().expect("server stdin");
        stdin.write_all(b"/stop\n").unwrap();
        stdin.flush().unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(20);
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
