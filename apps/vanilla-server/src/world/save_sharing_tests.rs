//! Real Windows delete-sharing failures, using only disposable test files.
use super::publish_staged_world_save;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Fixture(PathBuf);

impl Fixture {
    fn new(tag: &str) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "ir1710-world-sharing-{tag}-{}-{stamp}",
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

fn deny_delete(path: &Path) -> File {
    // Permit readers and writers, but deliberately omit FILE_SHARE_DELETE.
    // This is a real Windows sharing condition, independent of scanners.
    OpenOptions::new()
        .read(true)
        .share_mode(0x0000_0001 | 0x0000_0002)
        .open(path)
        .unwrap()
}

#[test]
fn windows_world_save_waits_for_delete_sharing_release() {
    let fixture = Fixture::new("release");
    let destination = fixture.0.join("flat.irw");
    let staging = fixture.0.join("flat.tmp");
    fs::write(&destination, b"previous complete snapshot").unwrap();
    fs::write(&staging, b"next complete snapshot").unwrap();
    let blocker = deny_delete(&destination);
    let (start_release, first_denial) = mpsc::sync_channel(1);
    let release = thread::spawn(move || {
        first_denial.recv_timeout(Duration::from_secs(5)).unwrap();
        thread::sleep(Duration::from_millis(120));
        drop(blocker);
    });
    let mut start_release = Some(start_release);
    let mut attempts = 0;
    let result = publish_staged_world_save(&staging, &destination, |source, target| {
        let renamed = fs::rename(source, target);
        attempts += 1;
        if let Some(signal) = start_release.take() {
            // Start the delay only AFTER an actual denied rename. Releasing
            // before serialization finishes would let a slow baseline pass.
            assert!(
                renamed.is_err(),
                "delete-sharing blocker did not deny rename"
            );
            signal.send(()).unwrap();
        }
        renamed
    });
    release.join().unwrap();
    result.unwrap();
    assert!(attempts > 1);
    assert_eq!(fs::read(&destination).unwrap(), b"next complete snapshot");
    assert!(!staging.exists());
}

#[test]
fn windows_world_save_permanent_sharing_denial_preserves_published_snapshot() {
    let fixture = Fixture::new("permanent");
    let destination = fixture.0.join("flat.irw");
    let staging = fixture.0.join("flat.tmp");
    fs::write(&destination, b"previous complete snapshot").unwrap();
    fs::write(&staging, b"next complete snapshot").unwrap();
    let blocker = deny_delete(&destination);
    let started = Instant::now();
    let error = publish_staged_world_save(&staging, &destination, |source, target| {
        fs::rename(source, target)
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(
        fs::read(&destination).unwrap(),
        b"previous complete snapshot"
    );
    assert!(
        !staging.exists(),
        "only this save's staging file must be cleaned"
    );
    drop(blocker);

    // A failure must leave the destination intact and permit a later save.
    fs::write(&staging, b"later complete snapshot").unwrap();
    publish_staged_world_save(&staging, &destination, |source, target| {
        fs::rename(source, target)
    })
    .unwrap();
    assert_eq!(fs::read(&destination).unwrap(), b"later complete snapshot");
    assert!(!staging.exists());
}
