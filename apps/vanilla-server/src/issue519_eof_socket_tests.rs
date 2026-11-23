//! Synthetic loopback regression for clean and incomplete Play-frame EOF.
//!
//! A subprocess isolates the global stderr budget for each case. The child
//! uses an owned listener, a synthetic world and a write-half-close; no peer
//! intent or full-duplex connection failure is inferred from receive EOF.
use super::*;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CASE_ENV: &str = "IR1710_ISSUE519_SYNTHETIC_CASE";
const ROOT_ENV: &str = "IR1710_ISSUE519_SYNTHETIC_ROOT";
const LIMIT: u64 = 1_048_576;
const CATEGORY: &str = "category=truncated_frame_eof";

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn create_new_file(path: &Path) -> io::Result<File> {
    OpenOptions::new().create_new(true).write(true).open(path)
}

fn bounded_read(path: &Path) -> io::Result<String> {
    let mut bytes = Vec::new();
    File::open(path)?.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        return Err(io::Error::other("synthetic log exceeded limit"));
    }
    String::from_utf8(bytes).map_err(|_| io::Error::other("synthetic log is not UTF-8"))
}

fn no_reparse_points(root: &Path) -> io::Result<()> {
    let mut pending = vec![root.to_path_buf()];
    let mut examined = 0usize;
    while let Some(path) = pending.pop() {
        examined += 1;
        if examined > 10_000 {
            return Err(io::Error::other("too many synthetic fixture entries"));
        }
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(io::Error::other("synthetic fixture contains a symlink"));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(io::Error::other(
                    "synthetic fixture contains a reparse point",
                ));
            }
        }
        if metadata.is_dir() {
            for entry in fs::read_dir(path)? {
                pending.push(entry?.path());
            }
        }
    }
    Ok(())
}

fn write_packet(stream: &mut TcpStream, id: u32, body: &[u8]) -> io::Result<()> {
    let mut frame = Vec::new();
    write_varint(id, &mut frame);
    frame.extend_from_slice(body);
    write_frame(stream, &frame)
}

fn next_frame(stream: &mut TcpStream, deadline: Instant) -> io::Result<(u32, Vec<u8>)> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(io::Error::new(
            ErrorKind::TimedOut,
            "synthetic receive deadline",
        ));
    }
    stream.set_read_timeout(Some(remaining))?;
    let frame = read_frame(stream)?;
    let mut body = frame.as_slice();
    let id = read_varint(&mut body)?;
    Ok((id, body.to_vec()))
}

fn synthetic_session(root: &Path, partial: bool) -> io::Result<()> {
    fs::write(root.join("whitelist.json"), r#"[{"name":"FrameProbe"}]"#)?;
    let context = crate::join_partial_socket_tests::context(root);
    let hub = Arc::clone(&context.hub);
    let player_path = PlayerPosition::location(&context.path, "FrameProbe");
    let inventory_path = Inventory::location(&player_path);

    // Connect to a listener still owned by this process; no free-port reserve
    // and rebind race with another process is involved.
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let address: SocketAddr = listener.local_addr()?;
    let mut client = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
    client.set_write_timeout(Some(Duration::from_secs(5)))?;
    let (accepted, peer) = listener.accept()?;
    if peer != client.local_addr()? {
        return Err(io::Error::other("synthetic loopback peer mismatch"));
    }
    drop(listener);
    let limiter = Arc::new(ConnectionLimiter::default());
    let permit = limiter
        .acquire(address.ip())
        .ok_or_else(|| io::Error::other("synthetic permit unavailable"))?;
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        let result = handle(accepted, context, permit);
        let _ = done_tx.send(result);
    });

    let mut handshake = Vec::new();
    write_varint(5, &mut handshake);
    write_string("localhost", &mut handshake)?;
    handshake.extend_from_slice(&address.port().to_be_bytes());
    write_varint(2, &mut handshake);
    write_packet(&mut client, 0, &handshake)?;
    let mut login = Vec::new();
    write_string("FrameProbe", &mut login)?;
    write_packet(&mut client, 0, &login)?;

    let deadline = Instant::now() + Duration::from_secs(40);
    let mut login_seen = false;
    let mut entity_id = None;
    for _ in 0..256 {
        let (id, body) = next_frame(&mut client, deadline)?;
        if !login_seen {
            if id != 0x02 {
                return Err(io::Error::other("synthetic login did not succeed"));
            }
            login_seen = true;
        } else if id == 0x01 {
            let raw: [u8; 4] = body
                .get(..4)
                .ok_or_else(|| io::Error::other("short Join Game"))?
                .try_into()
                .map_err(|_| io::Error::other("short Join Game"))?;
            entity_id = Some(i32::from_be_bytes(raw));
            break;
        }
    }
    let entity_id = entity_id.ok_or_else(|| io::Error::other("missing Join Game"))?;
    writeln!(create_new_file(&root.join("join-id.txt"))?, "{entity_id}")?;

    // The response is sent by Play processing, so receipt is an ordering
    // barrier after the initial join transfer; it is not an independent
    // measurement of the server's internal tick.
    let mut list = Vec::new();
    write_string("/list", &mut list)?;
    write_packet(&mut client, 1, &list)?;
    let mut barrier_seen = false;
    for _ in 0..512 {
        let (id, body) = next_frame(&mut client, deadline)?;
        if id == 0x02 {
            let mut cursor = legacy_protocol::Cursor::new(&body);
            if cursor.read_string(32_767)?.contains("Online (1):") {
                barrier_seen = true;
                break;
            }
        }
    }
    if !barrier_seen {
        return Err(io::Error::other("synthetic Play barrier not observed"));
    }
    if partial {
        // A synthetic frame length followed by only one of three declared
        // bytes. The contents are never printed by the expected diagnostic.
        client.write_all(&[3, 3])?;
    }
    client.shutdown(Shutdown::Write)?;

    let result = done_rx
        .recv_timeout(Duration::from_secs(30))
        .map_err(|_| io::Error::new(ErrorKind::TimedOut, "synthetic worker did not finish"))?;
    worker
        .join()
        .map_err(|_| io::Error::other("synthetic worker panicked"))?;
    result?;
    if hub.count()? != 0 {
        return Err(io::Error::other("synthetic player was not released"));
    }
    if !player_path.is_file() || !inventory_path.is_file() {
        return Err(io::Error::other("synthetic player cleanup did not save"));
    }
    if !PlayerPosition::load_state_with_dimension(&player_path)?
        .0
        .is_valid()
    {
        return Err(io::Error::other("synthetic player save invalid"));
    }
    let _ = Inventory::load(&inventory_path)?;
    Ok(())
}

#[test]
fn child_case() {
    let Some(case) = std::env::var_os(CASE_ENV) else {
        return;
    };
    let partial = match case.to_str() {
        Some("clean") => false,
        Some("partial") => true,
        _ => panic!("invalid synthetic case"),
    };
    let root = PathBuf::from(std::env::var_os(ROOT_ENV).expect("synthetic root missing"));
    let before = crate::disconnect_log::diagnostic_write_counts();
    synthetic_session(&root, partial).expect("synthetic session failed");
    if let Ok(expected) = std::env::var("IR1710_ISSUE519_EXPECT_WRITE") {
        let expected = match expected.as_str() {
            "none" => (0, 0),
            "ok" => (1, 0),
            "error" => (1, 1),
            _ => panic!("invalid expected synthetic write outcome"),
        };
        let after = crate::disconnect_log::diagnostic_write_counts();
        assert_eq!((after.0 - before.0, after.1 - before.1), expected);
    }
}

fn parent_case(partial: bool) -> io::Result<()> {
    let temp = fs::canonicalize(std::env::temp_dir())?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let root = temp.join(format!("ir1710-issue519-{}-{stamp}", std::process::id()));
    fs::create_dir(&root)?;
    let root = fs::canonicalize(root)?;
    if root.parent() != Some(temp.as_path()) {
        return Err(io::Error::other("synthetic root escaped TEMP"));
    }
    let stdout_path = root.join("child.stdout.log");
    let stderr_path = root.join("child.stderr.log");
    let fixture = root.join("world");
    fs::create_dir(&fixture)?;
    let mut child = OwnedChild(
        Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "issue519_eof_socket_tests::child_case",
                "--nocapture",
            ])
            .env(CASE_ENV, if partial { "partial" } else { "clean" })
            .env(ROOT_ENV, &fixture)
            .stdin(Stdio::null())
            .stdout(Stdio::from(create_new_file(&stdout_path)?))
            .stderr(Stdio::from(create_new_file(&stderr_path)?))
            .spawn()?,
    );
    let deadline = Instant::now() + Duration::from_secs(80);
    loop {
        if fs::metadata(&stdout_path)?.len() > LIMIT || fs::metadata(&stderr_path)?.len() > LIMIT {
            return Err(io::Error::other("synthetic child log exceeded limit"));
        }
        if let Some(status) = child.0.try_wait()? {
            if !status.success() {
                return Err(io::Error::other(format!(
                    "synthetic child failed; preserved {}",
                    root.display()
                )));
            }
            break;
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                ErrorKind::TimedOut,
                "synthetic child deadline",
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
    let _stdout = bounded_read(&stdout_path)?;
    let stderr = bounded_read(&stderr_path)?;
    let entity_id = bounded_read(&fixture.join("join-id.txt"))?
        .trim()
        .parse::<i32>()
        .map_err(io::Error::other)?;
    let expected = format!(
        "client_receive_end category=truncated_frame_eof phase=play entity_id={}",
        entity_id
    );
    let observed: Vec<&str> = stderr
        .lines()
        .filter(|line| line.contains(CATEGORY))
        .collect();
    if partial {
        if observed != [expected.as_str()] {
            return Err(io::Error::other(format!(
                "synthetic incomplete EOF diagnostic missing; preserved {}",
                root.display()
            )));
        }
    } else if !observed.is_empty() {
        return Err(io::Error::other(
            "boundary EOF produced incomplete-frame diagnostic",
        ));
    }
    no_reparse_points(&root)?;
    if fs::canonicalize(&root)?.parent() != Some(temp.as_path()) {
        return Err(io::Error::other("synthetic root changed before cleanup"));
    }
    fs::remove_dir_all(&root)?;
    Ok(())
}

#[test]
fn clean_eof_releases_and_saves_without_incomplete_diagnostic() {
    parent_case(false).unwrap();
}

#[test]
fn incomplete_frame_eof_is_diagnosed_and_still_releases_and_saves() {
    parent_case(true).unwrap();
}
