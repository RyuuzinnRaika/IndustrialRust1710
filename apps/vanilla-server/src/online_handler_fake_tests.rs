//! Handler-level loopback tests with a synthetic session verifier.
//!
//! This exercises the real handler and RSA/AES transition inside a unit-test
//! binary; it is not a released-server, HTTPS, or independent cipher oracle.
use super::*;
use rsa::pkcs8::DecodePublicKey;
use rsa::{Pkcs1v15Encrypt, RsaPublicKey};
use std::fs;
use std::io::Read;
use std::net::Shutdown;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const NAME: &str = "FakeLogin";
const UUID: &str = "01234567-89ab-cdef-0123-456789abcdef";
const KEY: [u8; 16] = [0x35; 16];

#[derive(Clone, Copy)]
enum Decision {
    Accept,
    Deny,
}

fn synthetic_root() -> (PathBuf, PathBuf) {
    let temporary = fs::canonicalize(std::env::temp_dir()).unwrap();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = temporary.join(format!(
        "ir1710-fake-handler-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&path).unwrap();
    let root = fs::canonicalize(path).unwrap();
    assert_eq!(root.parent(), Some(temporary.as_path()));
    (temporary, root)
}

fn check_no_reparse(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Err(io::Error::other("unexpected symlink in fixture"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return Err(io::Error::other("unexpected reparse point in fixture"));
        }
    }
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            check_no_reparse(&entry?.path())?;
        }
    } else if !meta.is_file() {
        return Err(io::Error::other("unexpected fixture entry type"));
    }
    Ok(())
}

// Compare exact synthetic fixture contents without printing their bytes.
// Size and count limits keep an unexpected file from making this unbounded.
fn fixture_snapshot(root: &Path) -> io::Result<(BTreeSet<PathBuf>, BTreeMap<PathBuf, Vec<u8>>)> {
    fn collect(
        root: &Path,
        path: &Path,
        directories: &mut BTreeSet<PathBuf>,
        files: &mut BTreeMap<PathBuf, Vec<u8>>,
        total: &mut usize,
    ) -> io::Result<()> {
        let meta = fs::symlink_metadata(path)?;
        if meta.file_type().is_symlink() {
            return Err(io::Error::other("unexpected symlink in fixture snapshot"));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                return Err(io::Error::other(
                    "unexpected reparse point in fixture snapshot",
                ));
            }
        }
        let relative = path
            .strip_prefix(root)
            .map_err(io::Error::other)?
            .to_path_buf();
        if meta.is_dir() {
            directories.insert(relative);
            if directories.len() > 128 {
                return Err(io::Error::other("too many fixture directories"));
            }
            for entry in fs::read_dir(path)? {
                collect(root, &entry?.path(), directories, files, total)?;
            }
        } else if meta.is_file() {
            if meta.len() > 1_048_576 || files.len() >= 128 {
                return Err(io::Error::other("fixture file bound exceeded"));
            }
            let mut bytes = Vec::new();
            fs::File::open(path)?
                .take(1_048_577)
                .read_to_end(&mut bytes)?;
            *total = total
                .checked_add(bytes.len())
                .ok_or_else(|| io::Error::other("fixture size overflow"))?;
            if bytes.len() as u64 != meta.len() || *total > 8 * 1_048_576 {
                return Err(io::Error::other("fixture changed or exceeded size bound"));
            }
            files.insert(relative, bytes);
        } else {
            return Err(io::Error::other("unexpected fixture entry type"));
        }
        Ok(())
    }
    let mut directories = BTreeSet::new();
    let mut files = BTreeMap::new();
    collect(root, root, &mut directories, &mut files, &mut 0)?;
    Ok((directories, files))
}

fn remove_owned_root(temporary: &Path, root: &Path) {
    let canonical = fs::canonicalize(root).unwrap();
    assert_eq!(canonical, root);
    assert_eq!(canonical.parent(), Some(temporary));
    check_no_reparse(root).unwrap();
    fs::remove_dir_all(root).unwrap();
}

fn connected_pair() -> (TcpStream, TcpStream, u16) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let client = TcpStream::connect_timeout(&address, Duration::from_secs(3)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let (server, peer) = loop {
        match listener.accept() {
            Ok(pair) => break pair,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "fixture accept timed out");
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("fixture accept failed: {error}"),
        }
    };
    assert_eq!(peer, client.local_addr().unwrap());
    server.set_nonblocking(false).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    client
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    (client, server, address.port())
}

fn start_encrypted_login(client: &mut Connection, port: u16) {
    let mut handshake = Vec::new();
    write_varint(5, &mut handshake);
    write_string("localhost", &mut handshake).unwrap();
    handshake.extend_from_slice(&port.to_be_bytes());
    write_varint(2, &mut handshake);
    send(client, 0, &handshake).unwrap();
    let mut login = Vec::new();
    write_string(NAME, &mut login).unwrap();
    send(client, 0, &login).unwrap();

    let (id, body) = receive(client).unwrap();
    assert_eq!(id, 1, "expected Encryption Request");
    let mut request = Input(&body);
    assert!(request.string(0).unwrap().is_empty());
    let der_length = usize::from(request.u16().unwrap());
    assert!((16..512).contains(&der_length));
    let der = request.take(der_length).unwrap();
    assert_eq!(request.u16().unwrap(), 4);
    let token = request.take(4).unwrap();
    request.finish().unwrap();

    let public = RsaPublicKey::from_public_key_der(der).unwrap();
    let mut rng = rand::rngs::OsRng;
    let encrypted_key = public.encrypt(&mut rng, Pkcs1v15Encrypt, &KEY).unwrap();
    let encrypted_token = public.encrypt(&mut rng, Pkcs1v15Encrypt, token).unwrap();
    let mut response = Vec::new();
    response.extend_from_slice(&u16::try_from(encrypted_key.len()).unwrap().to_be_bytes());
    response.extend_from_slice(&encrypted_key);
    response.extend_from_slice(&u16::try_from(encrypted_token.len()).unwrap().to_be_bytes());
    response.extend_from_slice(&encrypted_token);
    send(client, 1, &response).unwrap();
    client.enable_test_encryption(&KEY);
}

fn exercise_handler(decision: Decision) {
    let (temporary, root) = synthetic_root();
    fs::write(root.join("whitelist.json"), r#"[{"name":"FakeLogin"}]"#).unwrap();
    let mut context = crate::join_partial_socket_tests::context(&root);
    context.online_mode = true;
    context.auth_key = Some(Arc::new(ServerIdentity::generate().unwrap()));
    let initial_fixture = fixture_snapshot(&root).unwrap();
    let hub = Arc::clone(&context.hub);
    let login_names = Arc::clone(&context.login_names);
    let player_file = PlayerPosition::authenticated_location(&context.path, UUID).unwrap();
    let offline_file = PlayerPosition::location(&context.path, NAME);
    let (socket, accepted, port) = connected_pair();
    let mut client = Connection::new(socket);
    let limiter = Arc::new(ConnectionLimiter::default());
    let permit = limiter.acquire("127.0.0.1".parse().unwrap()).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let worker_calls = Arc::clone(&calls);
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        let result = handle_with_auth(accepted, context, permit, |identity, socket, username| {
            identity.authenticate_with_test_verifier(socket, username, |name, hash| {
                worker_calls.fetch_add(1, Ordering::SeqCst);
                let digits = hash.strip_prefix('-').unwrap_or(hash);
                let valid_hash = !digits.is_empty()
                    && hash.len() <= 41
                    && digits.bytes().all(|b| b.is_ascii_hexdigit());
                entered_tx
                    .send(name == NAME && valid_hash)
                    .map_err(io::Error::other)?;
                release_rx
                    .recv_timeout(Duration::from_secs(8))
                    .map_err(io::Error::other)?;
                match decision {
                    Decision::Accept => Ok(online_auth::VerifiedProfile {
                        name: NAME.to_owned(),
                        uuid: UUID.to_owned(),
                    }),
                    Decision::Deny => Err(io::Error::new(
                        ErrorKind::TimedOut,
                        "synthetic verifier timeout",
                    )),
                }
            })
        });
        let _ = done_tx.send(result);
    });

    // Always release the worker and close the owned socket, even on assertion
    // failure. If it cannot finish, preserve the fixture for investigation.
    let observed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        start_encrypted_login(&mut client, port);
        assert!(entered_rx.recv_timeout(Duration::from_secs(5)).unwrap());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(hub.count().unwrap(), 0, "pending auth was admitted");
        assert!(!login_names
            .names
            .lock()
            .unwrap()
            .contains(&NAME.to_ascii_lowercase()));
        assert!(!player_file.exists() && !offline_file.exists());
        assert!(
            initial_fixture == fixture_snapshot(&root).unwrap(),
            "fixture changed before verification completed"
        );
        release_tx.send(()).unwrap();
        match decision {
            Decision::Deny => {
                let mut byte = [0u8; 1];
                match client.read(&mut byte) {
                    Ok(0) => {}
                    Err(error) if error.kind() == ErrorKind::ConnectionReset => {}
                    Ok(_) => panic!("unexpected bytes after verifier denial"),
                    Err(error) => panic!("denial did not close connection: {}", error.kind()),
                }
            }
            Decision::Accept => {
                let (id, payload) = receive(&mut client).unwrap();
                assert_eq!(id, 2, "expected encrypted Login Success");
                let mut input = Input(&payload);
                assert_eq!(input.string(36).unwrap(), UUID);
                assert_eq!(input.string(16).unwrap(), NAME);
                input.finish().unwrap();
                assert_eq!(hub.count().unwrap(), 1);
            }
        }
    }));
    let _ = release_tx.try_send(());
    let _ = client.shutdown(Shutdown::Both);
    drop(client);
    let result = match done_rx.recv_timeout(Duration::from_secs(20)) {
        Ok(result) => result,
        Err(error) => {
            drop(worker);
            panic!(
                "handler did not finish; fixture retained: {} ({error})",
                root.display()
            );
        }
    };
    worker.join().unwrap();
    if let Err(panic) = observed {
        std::panic::resume_unwind(panic);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(hub.count().unwrap(), 0, "admission guard must be released");
    assert!(!login_names
        .names
        .lock()
        .unwrap()
        .contains(&NAME.to_ascii_lowercase()));
    match decision {
        Decision::Deny => {
            assert!(
                matches!(result, Err(ref error) if error.kind() == ErrorKind::TimedOut && error.to_string() == "synthetic verifier timeout")
            );
            assert!(!player_file.exists() && !offline_file.exists());
            assert!(
                initial_fixture == fixture_snapshot(&root).unwrap(),
                "fixture changed after verifier denial"
            );
        }
        Decision::Accept => {
            // A deliberately closed client can cause a join/play I/O error;
            // distinguish that from a failure returned by the save tail.
            if let Err(error) = result {
                assert!(
                    error.to_string().starts_with("active play session: "),
                    "unexpected post-authentication error kind: {:?}",
                    error.kind()
                );
            }
            assert!(player_file.is_file(), "verified player was not saved");
            assert!(PlayerPosition::load_state_with_dimension(&player_file)
                .unwrap()
                .0
                .is_valid());
            let inventory = Inventory::location(&player_file);
            assert!(inventory.is_file(), "verified inventory was not saved");
            Inventory::load(&inventory).unwrap();
            assert!(
                !offline_file.exists(),
                "online identity wrote an offline player file"
            );
        }
    }
    remove_owned_root(&temporary, &root);
}

#[test]
fn handler_denies_synthetic_verifier_timeout_before_admission() {
    exercise_handler(Decision::Deny);
}

#[test]
fn handler_accepts_synthetic_verified_profile_and_releases_admission() {
    exercise_handler(Decision::Accept);
}
