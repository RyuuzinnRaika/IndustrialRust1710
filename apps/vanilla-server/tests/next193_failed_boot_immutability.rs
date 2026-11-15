//! NEXT-193 / #1176: a failed boot on a corrupt world must fail closed and
//! must not mutate a single byte of the data that already existed.
//!
//! The real server binary is booted against a corrupt IRW fixture inside a
//! disposable temp directory (never a real operator world). Two properties
//! are pinned:
//!   1. the boot FAILS (non-zero exit) — a corrupt world is never accepted;
//!   2. every file that existed before the attempt — the corrupt world file
//!      and the whitelist — is byte-for-byte identical afterwards, and no
//!      published save/journal artifact is left behind.
//!
//! Side files the server legitimately creates while booting (session.lock,
//! default banned/ops/permissions files, metrics) are recorded but are NOT
//! part of the immutability claim: they are new server-owned files, not a
//! rewrite of pre-existing data. Real 1.7.10 client acceptance: NOT_RUN
//! (headless host).
use std::collections::BTreeMap;
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    for entry in walk(root) {
        let relative = entry
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        files.insert(relative, fs::read(&entry).unwrap());
    }
    files
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[test]
fn next193_failed_boot_on_corrupt_world_never_mutates_existing_files() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("ir1710-next193-{}-{stamp}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    // A valid 1-edit IRW1 fixture (magic + count + one 11-byte record) with
    // the FIRST byte corrupted: World::load rejects it with
    // "unknown IRW world format" before any write path can run.
    let world = root.join("corrupt.irw");
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"XIW1");
    bytes.extend_from_slice(&1u32.to_be_bytes());
    bytes.extend_from_slice(&0i32.to_be_bytes()); // x
    bytes.push(64); // y
    bytes.extend_from_slice(&0i32.to_be_bytes()); // z
    bytes.extend_from_slice(&16u16.to_be_bytes()); // stone
    fs::write(&world, &bytes).unwrap();
    let whitelist = root.join("whitelist.json");
    fs::write(&whitelist, r#"[{"name":"RustTester"}]"#).unwrap();

    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let before = snapshot(&root);
    assert!(before.contains_key("corrupt.irw"));
    assert!(before.contains_key("whitelist.json"));

    let child = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
        .args(["--bind", &format!("127.0.0.1:{port}"), "--world"])
        .arg(&world)
        .arg("--whitelist")
        .arg(&whitelist)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("server spawns");

    // Property 1: the boot must FAIL CLOSED, not linger as a live server.
    let mut child = child;
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        match child.try_wait().unwrap() {
            Some(status) => break status,
            None => {
                assert!(
                    Instant::now() < deadline,
                    "corrupt-world boot did not fail within 20s"
                );
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    };
    assert!(
        !status.success(),
        "server ACCEPTED a corrupt world (exit {status}); boot must fail closed"
    );

    // Property 2: nothing that existed before the attempt changed, and no
    // published save/journal/staging artifact appeared.
    let after = snapshot(&root);
    for (name, original) in &before {
        assert_eq!(
            after.get(name),
            Some(original),
            "failed boot mutated pre-existing file {name}"
        );
    }
    for name in after.keys() {
        assert!(
            !name.ends_with(".journal") && !name.ends_with(".importing"),
            "failed boot published journal/staging artifact {name}"
        );
    }
    let _ = fs::remove_dir_all(&root);
}
