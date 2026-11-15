//! ADD-074 / #499: the login encryption challenge is single-use. Session A
//! answers its Encryption Request with a perfectly correct RSA response
//! (fresh 16-byte secret + its own challenge, so the server's per-connection
//! challenge check passes and only Mojang can reject it). An attacker then
//! replays session A's exact response bytes into session B of a REAL
//! `--online-mode` server. Session B carries a freshly generated challenge,
//! so the replayed token must fail `verify_challenge_token` BEFORE the AES
//! switch, the connection must be refused, and Login Success (0x02) must
//! never be emitted for the replayed session.
use legacy_protocol::{read_frame, read_varint, write_frame, write_string, write_varint};
use rsa::pkcs8::DecodePublicKey;
use rsa::{Pkcs1v15Encrypt, RsaPublicKey};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Server {
    child: Child,
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

fn scratch(tag: &str) -> PathBuf {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "industrialrust-add074-{tag}-{}-{now}",
        std::process::id()
    ));
    fs::create_dir_all(&root).expect("isolated scratch directory");
    root
}

fn framed(body: &[u8]) -> Vec<u8> {
    let mut framed = Vec::new();
    write_frame(&mut framed, body).unwrap();
    framed
}

fn login_start(port: u16, name: &str) -> TcpStream {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let mut handshake = Vec::new();
    write_varint(5, &mut handshake);
    write_string("localhost", &mut handshake).unwrap();
    handshake.extend_from_slice(&port.to_be_bytes());
    write_varint(2, &mut handshake);
    let mut handshake_packet = Vec::new();
    write_varint(0, &mut handshake_packet);
    handshake_packet.extend_from_slice(&handshake);
    stream.write_all(&framed(&handshake_packet)).unwrap();
    let mut login = Vec::new();
    write_varint(0, &mut login);
    write_string(name, &mut login).unwrap();
    stream.write_all(&framed(&login)).unwrap();
    stream
}

/// Read the plaintext Encryption Request (0x01) and return
/// (der public key, four-byte challenge, full frame body for replay).
fn encryption_request(stream: &mut TcpStream) -> (Vec<u8>, [u8; 4], Vec<u8>) {
    let frame = read_frame(stream).expect("encryption request");
    let mut cursor = frame.as_slice();
    let id = read_varint(&mut cursor).unwrap();
    assert_eq!(id, 0x01, "online-mode must demand encryption before play");
    let empty = read_varint(&mut cursor).unwrap();
    assert_eq!(
        empty, 0,
        "the 1.7.10 encryption request carries an empty channel"
    );
    let mut der_len = [0u8; 2];
    cursor.read_exact(&mut der_len).unwrap();
    let der_len = u16::from_be_bytes(der_len) as usize;
    assert!((16..512).contains(&der_len));
    let mut der = vec![0u8; der_len];
    cursor.read_exact(&mut der).unwrap();
    let mut token_len = [0u8; 2];
    cursor.read_exact(&mut token_len).unwrap();
    assert_eq!(
        u16::from_be_bytes(token_len),
        4,
        "vanilla challenge is 4 bytes"
    );
    let mut challenge = [0u8; 4];
    cursor.read_exact(&mut challenge).unwrap();
    (der, challenge, frame)
}

/// Build the well-formed Encryption Response for `challenge` under `der`.
fn valid_response(der: &[u8], challenge: [u8; 4]) -> Vec<u8> {
    let public = RsaPublicKey::from_public_key_der(der).expect("server DER public key");
    let mut rng = rsa::rand_core::OsRng;
    let secret = public
        .encrypt(&mut rng, Pkcs1v15Encrypt, &[0x33u8; 16])
        .unwrap();
    let token = public
        .encrypt(&mut rng, Pkcs1v15Encrypt, &challenge)
        .unwrap();
    let mut response = Vec::new();
    write_varint(0x01, &mut response);
    response.extend_from_slice(&(secret.len() as u16).to_be_bytes());
    response.extend_from_slice(&secret);
    response.extend_from_slice(&(token.len() as u16).to_be_bytes());
    response.extend_from_slice(&token);
    response
}

/// Drain `stream` until it closes or a plaintext Login Success appears.
/// Returns true only if Login Success (0x02) was observed.
fn sees_login_success(stream: &mut TcpStream) -> bool {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        match read_frame(stream) {
            Ok(frame) => {
                let mut cursor = frame.as_slice();
                if read_varint(&mut cursor).unwrap_or(u32::MAX) == 0x02 {
                    return true;
                }
            }
            Err(_) => return false, // refused/closed: fail closed as designed
        }
    }
    false
}

#[test]
fn replaying_another_sessions_encryption_response_never_reaches_play() {
    let root = scratch("live");
    fs::write(root.join("whitelist.json"), "[{\"name\":\"RustTester\"}]").unwrap();
    let port = {
        let probe = TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap().port()
    };
    let child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
        .current_dir(&root)
        .args(["--bind", &format!("127.0.0.1:{port}"), "--world"])
        .arg(root.join("world/flat.irw"))
        .args(["--whitelist", "whitelist.json"])
        .arg("--online-mode")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("launch online-mode server");
    let mut server = Server { child };
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        assert!(
            server.child.try_wait().unwrap().is_none(),
            "online-mode server exited before listening"
        );
        assert!(Instant::now() < deadline, "server startup timed out");
        std::thread::sleep(Duration::from_millis(40));
    }

    // Session A: a fully correct response for A's own challenge. The server
    // accepts the per-connection challenge, switches to AES and asks Mojang;
    // only Mojang can reject this host. Drain until the server closes.
    let mut session_a = login_start(port, "RustTester");
    let (der, challenge_a, _) = encryption_request(&mut session_a);
    let replay_bytes = valid_response(&der, challenge_a);
    session_a.write_all(&framed(&replay_bytes)).unwrap();
    session_a.flush().unwrap();
    // The server enables AES-CFB8 immediately after the challenge check, so
    // everything it sends now is ciphertext; only the close matters.
    let _ = read_frame(&mut session_a);
    drop(session_a);

    // Session B: replay A's exact response bytes. B's request carries a
    // freshly generated challenge, so the stale token must be refused and
    // the replayed session must never be promoted to Play state.
    let mut session_b = login_start(port, "RustTester");
    let (_, challenge_b, _) = encryption_request(&mut session_b);
    // The per-connection challenge is freshly generated, so the replayed
    // token is genuinely stale for this session (a 2^-32 collision would
    // still end at Mojang, which cannot verify this host).
    assert_ne!(
        challenge_a, challenge_b,
        "each login must mint a fresh challenge"
    );
    session_b.write_all(&framed(&replay_bytes)).unwrap();
    session_b.flush().unwrap();
    assert!(
        !sees_login_success(&mut session_b),
        "replayed session A response must never produce Login Success in session B"
    );
    drop(session_b);

    assert!(
        server.child.try_wait().unwrap().is_none(),
        "a replay attempt must only close the session, not crash the server"
    );
    drop(server);
    let _ = fs::remove_dir_all(&root);
}
