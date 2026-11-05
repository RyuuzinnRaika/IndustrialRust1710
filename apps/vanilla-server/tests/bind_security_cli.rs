//! Issue #505: startup must not accidentally publish an offline-mode server.
//! Execute the real CLI only in a new disposable working directory. The
//! intentionally missing whitelist makes an accepted bind fail *before*
//! creating a listener, player state, or world data.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

fn isolated_directory(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let process_id = std::process::id();
    let directory_name = format!("ir1710-bind-security-{tag}-{process_id}-{nanos}");
    let path = std::env::temp_dir().join(directory_name);
    // Create-only: never delete a pre-existing directory or real world.
    fs::create_dir(&path).unwrap();
    path
}

fn start_with_missing_whitelist(
    working_dir: &Path,
    bind: Option<&str>,
    flags: &[&str],
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_vanilla-server"));
    command
        .current_dir(working_dir)
        .args(["--world", "isolated/flat.irw"])
        .args(["--whitelist", "missing-whitelist.json"]);
    if let Some(address) = bind {
        command.args(["--bind", address]);
    }
    command.args(flags).output().unwrap()
}

#[test]
fn offline_mode_refuses_all_non_loopback_listeners_before_world_creation() {
    for (index, address) in ["0.0.0.0:0", "[::]:0", "192.0.2.8:0"]
        .into_iter()
        .enumerate()
    {
        let root = isolated_directory(&format!("reject-{index}"));
        let output = start_with_missing_whitelist(&root, Some(address), &[]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{address}");
        assert!(
            stderr.contains("non-loopback bind requires explicit --insecure-offline-lan"),
            "{address}: expected explicit offline exposure refusal, got: {stderr}"
        );
        assert!(!root.join("isolated").exists());
        assert!(!root.join("session.lock").exists());
        fs::remove_dir(&root).unwrap();
    }
}

#[test]
fn loopback_and_explicit_exposure_options_pass_the_bind_permission_gate() {
    // The missing whitelist must fail before any listener, even when the
    // address choice itself is accepted. Check the stable Rust error kind,
    // not a localized OS error message (Windows error 2 is translated).
    // This is not proof that starting a public server is safe.
    let scenarios: [(Option<&str>, &[&str]); 5] = [
        (None, &[]),
        (Some("127.0.0.1:0"), &[]),
        (Some("[::1]:0"), &[]),
        (Some("0.0.0.0:0"), &["--insecure-offline-lan"]),
        (Some("0.0.0.0:0"), &["--online-mode"]),
    ];
    for (index, (address, flags)) in scenarios.into_iter().enumerate() {
        let root = isolated_directory(&format!("accepted-{index}"));
        let output = start_with_missing_whitelist(&root, address, flags);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{address:?}");
        assert!(
            !stderr.contains("non-loopback bind requires explicit"),
            "{address:?} {flags:?}: rejected valid explicit/loopback bind: {stderr}"
        );
        assert!(
            stderr.contains("kind: NotFound"),
            "{address:?} {flags:?}: expected missing whitelist before server startup, got: {stderr}"
        );
        assert!(!root.join("isolated").exists());
        fs::remove_dir(&root).unwrap();
    }
}
