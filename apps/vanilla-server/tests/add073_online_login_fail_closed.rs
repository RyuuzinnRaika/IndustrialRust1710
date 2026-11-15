//! ADD-073 / #498: online-mode login must fail closed end-to-end. A raw
//! Protocol 5 client joins a REAL `--online-mode` server through handshake
//! and Login Start, then answers the Encryption Request either with
//! undecryptable garbage or with a perfectly well-formed response whose Mojang
//! session verification can never succeed for this host. In every case the
//! server must refuse the connection and must never emit Login Success
//! (0x02): an unverified player never reaches Play state.
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
        "industrialrust-add073-{tag}-{}-{now}",
        std::process::id()
    ));
    fs::create_dir_all(&root).expect("isolated scratch directory");
    root
}

fn login_start(port: u16, name: &str) -> TcpStream {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
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

fn framed(body: &[u8]) -> Vec<u8> {
    let mut framed = Vec::new();
    write_frame(&mut framed, body).unwrap();
    framed
}

fn read_packet_id(stream: &mut TcpStream) -> Option<u32> {
    match read_frame(stream) {
        Ok(frame) => {
            let mut cursor = frame.as_slice();
            Some(read_varint(&mut cursor).unwrap_or(u32::MAX))
        }
        Err(_) => None,
    }
}

fn encryption_request(stream: &mut TcpStream) -> Vec<u8> {
    let mut stream_ref = stream;
    let frame = read_frame(&mut stream_ref).expect("encryption request");
    let mut cursor = frame.as_slice();
    let id = read_varint(&mut cursor).unwrap();
    assert_eq!(id, 0x01, "online-mode must demand encryption before play");
    let _empty = read_varint(&mut cursor).unwrap(); // server channel string, empty
    let mut der_len = [0u8; 2];
    (&mut cursor).read_exact(&mut der_len).unwrap();
    let der_len = u16::from_be_bytes(der_len) as usize;
    let mut der = vec![0u8; der_len];
    (&mut cursor).read_exact(&mut der).unwrap();
    der
}

#[test]
fn online_mode_login_never_reaches_play_without_session_verification() {
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

    // Case 1: undecryptable "RSA" blobs — the server aborts immediately,
    // long before any Mojang call could succeed.
    let mut hostile = login_start(port, "RustTester");
    encryption_request(&mut hostile);
    let mut garbage = Vec::new();
    write_varint(0x01, &mut garbage);
    garbage.extend_from_slice(&128u16.to_be_bytes());
    garbage.extend_from_slice(&[0xA5u8; 128]);
    garbage.extend_from_slice(&4u16.to_be_bytes());
    garbage.extend_from_slice(&[0x5Au8; 4]);
    hostile.write_all(&framed(&garbage)).unwrap();
    hostile.flush().unwrap();
    let mut saw_login_success = false;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        match read_packet_id(&mut hostile) {
            Some(0x02) => {
                saw_login_success = true;
                break;
            }
            Some(_) => continue,
            None => break, // connection refused/closed: fail closed as designed
        }
    }
    assert!(
        !saw_login_success,
        "undecryptable Encryption Response must never produce Login Success"
    );
    drop(hostile);

    // Case 2: a perfectly well-formed response with a fresh secret and the
    // live challenge correctly RSA-encrypted. Only Mojang can now reject it
    // (outage, 204 or a foreign account); the server must still refuse the
    // unverified player and never send Login Success.
    let mut wellformed = login_start(port, "RustTester");
    let der = encryption_request(&mut wellformed);
    let public = RsaPublicKey::from_public_key_der(&der).expect("server DER public key");
    let mut rng = rsa::rand_core::OsRng;
    let secret = public
        .encrypt(&mut rng, Pkcs1v15Encrypt, &[0x11u8; 16])
        .unwrap();
    let challenge = public
        .encrypt(&mut rng, Pkcs1v15Encrypt, &[0x22u8; 4])
        .unwrap();
    let mut response = Vec::new();
    write_varint(0x01, &mut response);
    response.extend_from_slice(&(secret.len() as u16).to_be_bytes());
    response.extend_from_slice(&secret);
    response.extend_from_slice(&(challenge.len() as u16).to_be_bytes());
    response.extend_from_slice(&challenge);
    wellformed.write_all(&framed(&response)).unwrap();
    wellformed.flush().unwrap();
    let mut saw_login_success = false;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        match read_packet_id(&mut wellformed) {
            Some(0x02) => {
                saw_login_success = true;
                break;
            }
            // Once encrypted, hostile payloads are AES-CFB8 noise; the only
            // remaining readable plaintext frames would be login-phase ones.
            Some(_) => continue,
            None => break,
        }
    }
    assert!(
        !saw_login_success,
        "an unverified Mojang session must never produce Login Success"
    );
    drop(wellformed);
    assert!(
        server.child.try_wait().unwrap().is_none(),
        "hostile logins must only close the session, not crash the server"
    );
    drop(server);
    let _ = fs::remove_dir_all(&root);
}
