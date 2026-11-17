//! NEXT-091 / #1074: after editing a large number of slots back to back,
//! a clean logout must persist EXACTLY the live state. Against the REAL
//! server binary (isolated scratch world, creative):
//!
//!   1. 36 window slots (all main-inventory + hotbar slots 9..44) are set
//!      in rapid succession with Creative Inventory Action (C10); every
//!      edit is confirmed by Set Slot (0x2F) with its own slot header;
//!   2. one stack is picked up onto the cursor with a normal left click
//!      (0x0E) and left there;
//!   3. /stop performs a clean logout;
//!   4. the persisted inventory file is compared slot-by-slot against the
//!      expected live state: all 35 remaining slots keep their exact
//!      id/count, the clicked slot is empty, and the cursor holds the
//!      picked-up stack — full equality, not just totals.
//!
//! Real 1.7.10 client acceptance: NOT_RUN (headless host; the window
//! packets and the persisted file are checked directly).
#[path = "support/packet_client.rs"]
mod packet_client;

use legacy_protocol::{write_string, write_varint};
use packet_client::BufferedClient;
use serde_json::Value;
use std::fs;
use std::io::Write;
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
            "ir1710-next091-{tag}-{}-{stamp}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("whitelist.json"), r#"[{"name":"SlotEditor"}]"#).unwrap();
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

    fn join(&self, name: &str) -> BufferedClient {
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
        // Drain the join burst (echoing KeepAlive) until the first S08.
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            assert!(Instant::now() < deadline, "{name}: join never completed");
            let Some((id, body)) = pump(&mut socket) else {
                continue;
            };
            if id == 0x00 && body.len() == 4 {
                let _ = socket.write_all(&frame(0, &body));
            }
            if id == 0x08 {
                return socket;
            }
        }
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

fn pump(socket: &mut BufferedClient) -> Option<(u32, Vec<u8>)> {
    socket.poll_packet().expect("read Protocol 5 packet")
}

/// C10 Creative Inventory Action: slot, blockId i16, count u8,
/// damage i16, itemName MC string (-1 = none).
fn creative_set(socket: &mut BufferedClient, slot: i16, id: i16, count: u8) {
    let mut p = slot.to_be_bytes().to_vec();
    p.extend_from_slice(&id.to_be_bytes());
    p.push(count);
    p.extend_from_slice(&0i16.to_be_bytes());
    p.extend_from_slice(&(-1i16).to_be_bytes());
    socket.write_all(&frame(0x10, &p)).unwrap();
    socket.flush().unwrap();
    // Set Slot (0x2F) confirmation for window 0, this exact slot.
    let want = [0u8, (slot >> 8) as u8, slot as u8];
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            Instant::now() < deadline,
            "no Set Slot confirmation for {slot}"
        );
        let Some((id2, body)) = pump(socket) else {
            continue;
        };
        if id2 == 0x00 && body.len() == 4 {
            let _ = socket.write_all(&frame(0, &body));
        }
        if id2 == 0x2f {
            assert!(
                body.starts_with(&want),
                "Set Slot must confirm window 0 slot {slot}: {body:?}"
            );
            assert_eq!(
                i16::from_be_bytes(body[3..5].try_into().unwrap()),
                id,
                "confirmed block id changed for slot {slot}"
            );
            assert_eq!(body[5], count, "confirmed count changed for slot {slot}");
            return;
        }
    }
}

/// Normal left click (0x0E mode 0); waits for the transaction result (0x32)
/// and the following Window Items (0x30) snapshot.
fn left_click(socket: &mut BufferedClient, window: u8, slot: i16, action: i16) {
    let mut p = vec![window];
    p.extend_from_slice(&slot.to_be_bytes());
    p.push(0); // left button
    p.extend_from_slice(&action.to_be_bytes());
    p.push(0); // mode: normal click
    p.extend_from_slice(&(-1i16).to_be_bytes());
    socket.write_all(&frame(0x0e, &p)).unwrap();
    socket.flush().unwrap();
    let want = [window, action.to_be_bytes()[0], action.to_be_bytes()[1], 1];
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut accepted = false;
    loop {
        assert!(
            Instant::now() < deadline,
            "click {window}/{slot} not confirmed"
        );
        let Some((id, body)) = pump(socket) else {
            continue;
        };
        if id == 0x00 && body.len() == 4 {
            let _ = socket.write_all(&frame(0, &body));
        }
        if id == 0x32 && body == want {
            accepted = true;
        }
        if accepted && id == 0x30 && body.first() == Some(&window) {
            return;
        }
    }
}

#[test]
fn next091_mass_slot_edits_persist_exactly_on_clean_logout() {
    let mut server = Server::start("massedit");
    let mut client = server.join("SlotEditor");

    // 36 consecutive edits: every main-inventory and hotbar slot
    // (window 9..44) receives a distinct id/count.
    let expected: Vec<(i16, u8)> = (9..45).map(|w| (w, (w % 7) as u8 + 1)).collect();
    for &(id, count) in &expected {
        // Window slot == id here: ids 9..=44 are all registered blocks.
        creative_set(&mut client, id, id, count);
    }
    // Pick the window-9 stack onto the cursor and leave it there.
    left_click(&mut client, 0, 9, 9);

    // Clean logout.
    {
        let mut stdin = server.child.stdin.take().expect("server stdin");
        stdin.write_all(b"/stop\n").unwrap();
        stdin.flush().unwrap();
    }
    drop(client);
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

    let path = server.root.join("players/sloteditor.inventory.json");
    let saved: Value = serde_json::from_slice(&fs::read(&path).expect("inventory file must exist"))
        .expect("inventory file must be JSON");
    let slots = saved["slots"].as_array().expect("slots array");
    // The persisted array is window-indexed (0 result, 1..4 crafting,
    // 5..8 armor, 9..35 inventory, 36..44 hotbar).
    assert!(
        slots.len() >= 45,
        "expected 45 window-indexed slots, got {}",
        slots.len()
    );
    for &(id, count) in &expected {
        let slot = &slots[id as usize];
        if id == 9 {
            // That stack is on the cursor at logout.
            assert!(
                slot.is_null(),
                "clicked slot must be empty at logout, got {slot}"
            );
            continue;
        }
        assert_eq!(
            slot["id"].as_i64(),
            Some(i64::from(id)),
            "window slot {id} id drifted"
        );
        assert_eq!(
            slot["count"].as_i64(),
            Some(i64::from(count)),
            "window slot {id} count drifted"
        );
    }
    assert_eq!(saved["cursor"]["id"].as_i64(), Some(9), "cursor id");
    assert_eq!(
        saved["cursor"]["count"].as_i64(),
        Some(3),
        "cursor count must be the picked-up stack"
    );
    let _ = fs::remove_dir_all(&server.root);
}
