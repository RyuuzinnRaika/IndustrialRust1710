//! Protocol 5 Status/Ping and explicit Login rejection, in Rust.
use legacy_protocol::{read_frame, read_varint, write_string, write_varint, Cursor};
use serde_json::Value;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn frame(id: u32, payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    write_varint(id, &mut body);
    body.extend_from_slice(payload);
    let mut out = Vec::new();
    write_varint(body.len() as u32, &mut out);
    out.extend_from_slice(&body);
    out
}
fn handshake(port: u16, state: u32) -> Vec<u8> {
    let mut payload = Vec::new();
    write_varint(5, &mut payload);
    write_string("localhost", &mut payload).unwrap();
    payload.extend_from_slice(&port.to_be_bytes());
    write_varint(state, &mut payload);
    frame(0, &payload)
}
fn received(stream: &mut TcpStream) -> (u32, Vec<u8>) {
    let bytes = read_frame(stream).unwrap();
    let mut remaining = bytes.as_slice();
    let id = read_varint(&mut remaining).unwrap();
    (id, remaining.to_vec())
}
#[test]
fn status_ping_and_honest_login_denial() {
    let reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = reservation.local_addr().unwrap().port();
    drop(reservation);
    let mut child = Command::new(env!("CARGO_BIN_EXE_legacy-status-server"))
        .args(["--bind", &format!("127.0.0.1:{port}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(40));
    }
    let run = || {
        let mut sock = TcpStream::connect(("127.0.0.1", port)).unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        sock.write_all(&handshake(port, 1)).unwrap();
        sock.write_all(&frame(0, &[])).unwrap();
        let (id, body) = received(&mut sock);
        assert_eq!(id, 0);
        let mut cursor = Cursor::new(&body);
        let status: Value = serde_json::from_str(cursor.read_string(32768).unwrap()).unwrap();
        assert_eq!(status["version"]["protocol"], 5);
        let ping = 123456789i64.to_be_bytes();
        sock.write_all(&frame(1, &ping)).unwrap();
        assert_eq!(received(&mut sock), (1, ping.to_vec()));

        let mut sock = TcpStream::connect(("127.0.0.1", port)).unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        sock.write_all(&handshake(port, 2)).unwrap();
        let mut login = Vec::new();
        write_string("RustTester", &mut login).unwrap();
        sock.write_all(&frame(0, &login)).unwrap();
        let (id, body) = received(&mut sock);
        assert_eq!(id, 0);
        assert!(String::from_utf8_lossy(&body).contains("not yet implemented"));
    };
    let result = std::panic::catch_unwind(run);
    let _ = child.kill();
    let _ = child.wait();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
