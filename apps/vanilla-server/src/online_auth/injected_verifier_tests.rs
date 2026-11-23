//! In-process RSA/AES authentication tests with an explicit fake verifier.
//! These do not exercise the production HTTP transport or the login handler.
use super::*;
use rsa::pkcs8::DecodePublicKey;
use std::net::{Shutdown, TcpListener};
use std::sync::atomic::{AtomicUsize, Ordering};

const NAME: &str = "RustTester";
const KEY: [u8; 16] = [0x35; 16];
const PROBE: &[u8] = b"synthetic-encrypted-probe";
const REPLY: &[u8] = b"synthetic-encrypted-reply";

struct Request {
    der: Vec<u8>,
    challenge: [u8; 4],
}

struct Outcome {
    result: io::Result<VerifiedProfile>,
    encrypted_reader: bool,
    encrypted_writer: bool,
}

fn read_request(client: &mut Connection) -> Request {
    let (id, body) = crate::receive(client).expect("read encryption request");
    assert_eq!(id, 1);
    let mut bytes = body.as_slice();
    assert_eq!(legacy_protocol::read_varint(&mut bytes).unwrap(), 0);
    let mut size = [0; 2];
    bytes.read_exact(&mut size).unwrap();
    let mut der = vec![0; u16::from_be_bytes(size) as usize];
    bytes.read_exact(&mut der).unwrap();
    bytes.read_exact(&mut size).unwrap();
    assert_eq!(u16::from_be_bytes(size), 4);
    let mut challenge = [0; 4];
    bytes.read_exact(&mut challenge).unwrap();
    assert!(bytes.is_empty());
    Request { der, challenge }
}

fn response(request: &Request, token: [u8; 4]) -> Vec<u8> {
    let public = RsaPublicKey::from_public_key_der(&request.der).unwrap();
    let mut rng = OsRng;
    let secret = public.encrypt(&mut rng, Pkcs1v15Encrypt, &KEY).unwrap();
    let token = public.encrypt(&mut rng, Pkcs1v15Encrypt, &token).unwrap();
    let mut body = Vec::new();
    body.extend_from_slice(&u16::try_from(secret.len()).unwrap().to_be_bytes());
    body.extend_from_slice(&secret);
    body.extend_from_slice(&u16::try_from(token.len()).unwrap().to_be_bytes());
    body.extend_from_slice(&token);
    body
}

fn profile() -> VerifiedProfile {
    VerifiedProfile {
        name: NAME.to_owned(),
        uuid: "01234567-89ab-cdef-0123-456789abcdef".to_owned(),
    }
}

fn exchange<V, A, T>(
    identity: &ServerIdentity,
    probe_success: bool,
    verifier: V,
    action: A,
) -> (T, Outcome)
where
    V: FnOnce(&str, &str) -> io::Result<VerifiedProfile> + Send,
    A: FnOnce(&mut Connection, &Request) -> T,
{
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    // Establish both socket endpoints before spawning a scoped worker. If
    // connect fails, no worker can remain blocked in listener.accept().
    listener.set_nonblocking(true).unwrap();
    let socket = TcpStream::connect_timeout(&address, Duration::from_secs(3)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let (accepted, peer) = loop {
        match listener.accept() {
            Ok(pair) => break pair,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "accept timed out");
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("accept failed: {error}"),
        }
    };
    assert_eq!(peer, socket.local_addr().unwrap(), "unexpected TCP peer");
    accepted.set_nonblocking(false).unwrap();
    drop(listener);
    std::thread::scope(|scope| {
        let worker = scope.spawn(move || {
            accepted
                .set_read_timeout(Some(Duration::from_secs(8)))
                .unwrap();
            accepted
                .set_write_timeout(Some(Duration::from_secs(8)))
                .unwrap();
            let mut server = Connection::new(accepted);
            let result = identity.authenticate_with(&mut server, NAME, verifier);
            let encrypted_reader = server.reader.is_some();
            let encrypted_writer = server.writer.is_some();
            if result.is_ok() && probe_success {
                // Probe the encrypted Connection itself; this is not Login Success.
                send(&mut server, 0x7e, PROBE).unwrap();
                let (id, payload) = crate::receive(&mut server).unwrap();
                assert_eq!(id, 0x7d);
                assert_eq!(payload, REPLY);
            }
            Outcome {
                result,
                encrypted_reader,
                encrypted_writer,
            }
        });
        socket
            .set_read_timeout(Some(Duration::from_secs(8)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(8)))
            .unwrap();
        let mut client = Connection::new(socket);
        let request = read_request(&mut client);
        let value = action(&mut client, &request);
        // Keep client open while waiting so the timeout case tests a silent peer.
        let outcome = worker.join().expect("authentication worker panicked");
        (value, outcome)
    })
}

#[test]
fn accepted_challenge_calls_fake_and_enables_encrypted_stream() {
    let identity = ServerIdentity::generate().unwrap();
    let expected_hash = signed_server_hash(&KEY, &identity.der);
    let calls = AtomicUsize::new(0);
    let (_, outcome) = exchange(
        &identity,
        true,
        |name, hash| {
            calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(name, NAME);
            assert_eq!(hash, expected_hash);
            Ok(profile())
        },
        |client, request| {
            send(client, 1, &response(request, request.challenge)).unwrap();
            let mut packet = Vec::new();
            legacy_protocol::write_varint(0x7e, &mut packet);
            packet.extend_from_slice(PROBE);
            let mut plaintext = Vec::new();
            legacy_protocol::write_frame(&mut plaintext, &packet).unwrap();
            let mut ciphertext = vec![0; plaintext.len()];
            client.socket.read_exact(&mut ciphertext).unwrap();
            assert_ne!(ciphertext, plaintext, "probe must not be plaintext");
            let mut wrong_key = ciphertext.clone();
            Cfb8::new(&[0x36; 16], false).apply(&mut wrong_key);
            assert_ne!(wrong_key, plaintext, "wrong key decoded the probe");
            Cfb8::new(&KEY, false).apply(&mut ciphertext);
            assert_eq!(ciphertext, plaintext);
            client.encrypted(&KEY);
            send(client, 0x7d, REPLY).unwrap();
        },
    );
    let verified = match outcome.result {
        Ok(value) => value,
        Err(error) => panic!("unexpected synthetic auth error: {error}"),
    };
    assert_eq!(verified.name, NAME);
    assert_eq!(verified.uuid, profile().uuid);
    assert!(outcome.encrypted_reader && outcome.encrypted_writer);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn verifier_failure_after_valid_challenge_does_not_return_profile() {
    let identity = ServerIdentity::generate().unwrap();
    let expected_hash = signed_server_hash(&KEY, &identity.der);
    let calls = AtomicUsize::new(0);
    let (_, outcome) = exchange(
        &identity,
        false,
        |name, hash| {
            calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(name, NAME);
            assert_eq!(hash, expected_hash);
            Err(invalid("synthetic verifier denial"))
        },
        |client, request| {
            send(client, 1, &response(request, request.challenge)).unwrap();
            client.encrypted(&KEY);
            let mut byte = [0];
            assert_eq!(
                client.read(&mut byte).unwrap(),
                0,
                "expected EOF, not timeout"
            );
        },
    );
    assert!(
        matches!(outcome.result, Err(ref error) if error.kind() == io::ErrorKind::PermissionDenied && error.to_string() == "synthetic verifier denial")
    );
    assert!(outcome.encrypted_reader && outcome.encrypted_writer);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn mismatched_challenge_never_enables_cipher_or_calls_verifier() {
    let identity = ServerIdentity::generate().unwrap();
    let calls = AtomicUsize::new(0);
    let (_, outcome) = exchange(
        &identity,
        false,
        |_, _| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(profile())
        },
        |client, request| {
            let mut wrong = request.challenge;
            wrong[0] ^= 1;
            send(client, 1, &response(request, wrong)).unwrap();
        },
    );
    assert!(
        matches!(outcome.result, Err(ref error) if error.kind() == io::ErrorKind::PermissionDenied && error.to_string() == "invalid encryption response")
    );
    assert!(!outcome.encrypted_reader && !outcome.encrypted_writer);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn replayed_response_is_rejected_before_cipher_or_verifier() {
    let identity = ServerIdentity::generate().unwrap();
    let expected_hash = signed_server_hash(&KEY, &identity.der);
    let accepted_calls = AtomicUsize::new(0);
    let ((first, bytes), accepted) = exchange(
        &identity,
        true,
        |name, hash| {
            accepted_calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(name, NAME);
            assert_eq!(hash, expected_hash);
            Ok(profile())
        },
        |client, request| {
            let bytes = response(request, request.challenge);
            send(client, 1, &bytes).unwrap();
            client.encrypted(&KEY);
            let (id, payload) = crate::receive(client).unwrap();
            assert_eq!((id, payload.as_slice()), (0x7e, PROBE));
            send(client, 0x7d, REPLY).unwrap();
            (request.challenge, bytes)
        },
    );
    assert!(accepted.result.is_ok());
    assert!(accepted.encrypted_reader && accepted.encrypted_writer);
    assert_eq!(accepted_calls.load(Ordering::SeqCst), 1);

    // Random challenges can coincide; close that session before transmitting
    // anything and try a fresh connection rather than making a flaky claim.
    let mut tested = false;
    for _ in 0..8 {
        let calls = AtomicUsize::new(0);
        let (distinct, replay) = exchange(
            &identity,
            false,
            |_, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(profile())
            },
            |client, request| {
                assert_eq!(request.der, identity.der);
                if request.challenge == first {
                    client.shutdown(Shutdown::Both).unwrap();
                    return false;
                }
                send(client, 1, &bytes).unwrap();
                true
            },
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(!replay.encrypted_reader && !replay.encrypted_writer);
        if distinct {
            assert!(
                matches!(replay.result, Err(ref error) if error.kind() == io::ErrorKind::PermissionDenied && error.to_string() == "invalid encryption response")
            );
            tested = true;
            break;
        }
        assert!(replay.result.is_err());
    }
    assert!(tested, "could not obtain a distinct session challenge");
}

#[test]
fn silent_peer_times_out_before_cipher_or_verifier() {
    let identity = ServerIdentity::generate().unwrap();
    let calls = AtomicUsize::new(0);
    let (_, outcome) = exchange(
        &identity,
        false,
        |_, _| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(profile())
        },
        |_, _| {},
    );
    assert!(matches!(outcome.result, Err(ref error) if error.kind() == io::ErrorKind::TimedOut));
    assert!(!outcome.encrypted_reader && !outcome.encrypted_writer);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
