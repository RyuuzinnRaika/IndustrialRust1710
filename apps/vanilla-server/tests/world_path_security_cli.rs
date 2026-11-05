//! P0 Issue #500: a CLI world path containing ".." cannot escape its root.
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn cli_rejects_parent_traversal_before_world_or_lock_creation() {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!(
        "ir1710-world-path-cli-{}-{stamp}",
        std::process::id()
    ));
    let working = root.join("working");
    let outside = root.join("outside");
    fs::create_dir_all(&working).unwrap();
    fs::create_dir(&outside).unwrap();
    let world: PathBuf = working.join("..").join("outside").join("flat.irw");
    for arguments in [
        vec!["--world", world.to_str().unwrap()],
        vec!["--world", world.to_str().unwrap(), "--backup-world", "backup"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .current_dir(&working)
            .args(arguments)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("unsafe path: empty or parent-directory traversal")
        );
        assert!(!outside.join("flat.irw").exists());
        assert!(!outside.join("session.lock").exists());
        assert!(!outside.join("flat.tmp").exists());
    }
    fs::remove_dir_all(root).unwrap();
}
