//! NEXT-168 / #1151: a restore candidate with a leftover IRW redo journal
//! must not be certified as safe or modified by offline world verification.
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("ir1710-next168-{}-{stamp}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_vanilla-server"));
    command.current_dir(root);
    command
}

fn success(output: Output) {
    assert!(
        output.status.success(),
        "CLI failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn snapshot(directory: &Path) -> BTreeMap<String, Vec<u8>> {
    fs::read_dir(directory)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            assert!(entry.file_type().unwrap().is_file());
            (
                entry.file_name().into_string().unwrap(),
                fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

fn create_generation(root: &Path, block: u16) -> PathBuf {
    let mut bytes = b"IRW1".to_vec();
    bytes.extend(1u32.to_be_bytes());
    bytes.extend(0i32.to_be_bytes());
    bytes.push(64);
    bytes.extend(0i32.to_be_bytes());
    bytes.extend((block << 4).to_be_bytes());
    fs::write(root.join("source/flat.irw"), &bytes).unwrap();
    let backups = root.join("backups");
    let existing: Vec<_> = fs::read_dir(&backups)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    success(
        command(root)
            .args([
                "--world",
                "source/flat.irw",
                "--backup-rotate",
                "backups",
                "2",
            ])
            .output()
            .unwrap(),
    );
    let mut added = fs::read_dir(backups)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| !existing.contains(path));
    let generation = added.next().expect("new backup generation");
    assert!(added.next().is_none());
    assert_eq!(fs::read(generation.join("flat.irw")).unwrap(), bytes);
    assert!(generation.join(".industrial1710-backup").is_file());
    generation
}

#[test]
fn next168_offline_verify_never_replays_stale_journals_in_a_restored_generation() {
    let scratch = Scratch::new();
    let root = &scratch.0;
    fs::create_dir(root.join("source")).unwrap();
    fs::create_dir(root.join("backups")).unwrap();

    let first = create_generation(root, 1);
    let first_before = snapshot(&first);
    let second = create_generation(root, 4);
    let second_before = snapshot(&second);
    assert_ne!(first_before["flat.irw"], second_before["flat.irw"]);

    // A valid v1 redo record carrying an ordinary IRW snapshot from the
    // other generation, not just a malformed file the parser would reject.
    let old_world = &first_before["flat.irw"];
    let checksum = old_world.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    });
    let journal = serde_json::to_vec(&json!({
        "version": 1,
        "files": [{ "relative": "flat.irw", "contents": old_world, "checksum": checksum }]
    }))
    .unwrap();

    let restored = root.join("restore-candidate");
    fs::create_dir(&restored).unwrap();
    for (name, contents) in &second_before {
        fs::write(restored.join(name), contents).unwrap();
    }
    let verify = || {
        command(root)
            .args(["--verify-world", "restore-candidate/flat.irw"])
            .output()
            .unwrap()
    };
    success(verify());
    assert_eq!(snapshot(&restored), second_before);

    // Test every redo location startup knows, including legacy container logs.
    for name in [
        "flat.pending-txn.json",
        "flat.chests.pending-txn.json",
        "flat.furnaces.pending-txn.json",
    ] {
        let stale = restored.join(name);
        fs::write(&stale, &journal).unwrap();
        let before = snapshot(&restored);
        let refused = verify();
        assert!(!refused.status.success(), "accepted stale {name}");
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("unfinished transaction journal"),
            "{name}: {}",
            String::from_utf8_lossy(&refused.stderr)
        );
        assert_eq!(snapshot(&restored), before, "verification modified {name}");
        // Only the test owns this disposable copy. Do not infer that deleting
        // a journal is a safe recovery procedure on a real save.
        fs::remove_file(stale).unwrap();
        success(verify());
        assert_eq!(snapshot(&restored), second_before);
    }

    assert_eq!(snapshot(&first), first_before);
    assert_eq!(snapshot(&second), second_before);
}
