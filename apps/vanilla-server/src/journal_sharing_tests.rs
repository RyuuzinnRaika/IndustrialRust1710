//! A blocked Windows publication must keep durable redo recoverable.
use super::{commit, recover};
use std::fs::{self, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "ir1710-journal-sharing-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if !thread::panicking() {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
}

#[test]
fn windows_journal_sharing_failure_keeps_redo_and_recovers_exactly_once() {
    let fixture = Fixture::new();
    let journal = fixture.0.join("flat.pending-txn.json");
    let first_inventory = fixture.0.join("first.inventory.json");
    let inventory = fixture.0.join("test.inventory.json");
    let staging = inventory.with_extension("txn.tmp");
    let first_original = br#"{"version":1,"items":[8]}"#;
    let first_replacement = br#"{"version":1,"items":[7]}"#;
    let original = br#"{"version":1,"items":[1]}"#;
    let replacement = br#"{"version":1,"items":[2]}"#;
    fs::write(&first_inventory, first_original).unwrap();
    fs::write(&inventory, original).unwrap();
    let blocker = OpenOptions::new()
        .read(true)
        .share_mode(0x0000_0001 | 0x0000_0002) // no FILE_SHARE_DELETE
        .open(&inventory)
        .unwrap();
    let started = Instant::now();
    let error = commit(
        &journal,
        &[
            (&first_inventory, first_replacement.to_vec()),
            (&inventory, replacement.to_vec()),
        ],
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(started.elapsed() < Duration::from_secs(3));
    // The first participant has published before the blocked second one.
    // Durable redo, rather than rollback, must complete their new generation.
    assert_eq!(fs::read(&first_inventory).unwrap(), first_replacement);
    assert_eq!(fs::read(&inventory).unwrap(), original);
    let durable_redo = fs::read(&journal).expect("committed redo must survive failed publication");
    assert!(!durable_redo.is_empty());
    // A failed publication may retain its complete staging image or clean it.
    // Either way the committed redo is authoritative and must remain usable.
    match fs::read(&staging) {
        Ok(bytes) => assert_eq!(bytes, replacement),
        Err(error) => assert_eq!(error.kind(), io::ErrorKind::NotFound),
    }
    let refusal = commit(&journal, &[(&inventory, original.to_vec())]).unwrap_err();
    assert_eq!(refusal.kind(), io::ErrorKind::InvalidData);
    assert_eq!(fs::read(&journal).unwrap(), durable_redo);
    assert_eq!(fs::read(&first_inventory).unwrap(), first_replacement);
    assert_eq!(fs::read(&inventory).unwrap(), original);
    drop(blocker);

    recover(&journal).unwrap();
    assert_eq!(fs::read(&first_inventory).unwrap(), first_replacement);
    assert_eq!(fs::read(&inventory).unwrap(), replacement);
    assert!(!journal.exists());
    assert!(!staging.exists());

    // Replay the identical durable record, as after a crash between target
    // rename and journal removal. Replaying must not spend or duplicate again.
    fs::write(&journal, &durable_redo).unwrap();
    recover(&journal).unwrap();
    recover(&journal).unwrap();
    assert_eq!(fs::read(&first_inventory).unwrap(), first_replacement);
    assert_eq!(fs::read(&inventory).unwrap(), replacement);
    assert!(!journal.exists());
    assert!(!staging.exists());
}
