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

#[test]
fn offline_recovery_import_export_and_backup_refuse_parent_paths_before_io() {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!(
        "ir1710-storage-path-cli-{}-{stamp}",
        std::process::id()
    ));
    let working = root.join("working");
    let outside = root.join("outside");
    fs::create_dir_all(&working).unwrap();
    fs::create_dir(&outside).unwrap();
    let outside_marker = outside.join("level.dat_old");
    fs::write(&outside_marker, b"must stay unchanged").unwrap();
    let traversing = working.join("..").join("outside");
    let outside_text = traversing.to_str().unwrap();
    let valid_world = "world/flat.irw";
    let cases = [
        vec!["--recover-level-dat", outside_text],
        vec!["--prepare-anvil-copy", outside_text, "copy"],
        vec!["--prepare-anvil-copy", "safe-source", outside_text],
        vec!["--world", valid_world, "--import-anvil", outside_text],
        vec!["--world", valid_world, "--export-anvil", outside_text],
        vec!["--world", valid_world, "--backup-world", outside_text],
        vec!["--world", valid_world, "--backup-rotate", outside_text, "1"],
    ];
    for arguments in cases {
        let output = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .current_dir(&working)
            .args(&arguments)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{arguments:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("unsafe path: empty or parent-directory traversal"),
            "{arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fs::read(&outside_marker).unwrap(),
            b"must stay unchanged".to_vec()
        );
        assert!(!outside.join("session.lock").exists());
        assert!(!outside.join("flat.irw").exists());
    }
    fs::remove_dir_all(root).unwrap();
}
