//! End-to-end restore drill against the actual native verifier and synthetic data.
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const MARKER: &[u8] = b"IndustrialRust1710 verified offline backup v1\n";
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    parent: PathBuf,
    backup: PathBuf,
    scratch: PathBuf,
}

fn plain_directory(path: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    if !meta.file_type().is_dir() || meta.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    true
}

impl Fixture {
    fn new(contents: &[u8]) -> Self {
        // The build target and its parents must be privately controlled.
        let binary = std::env::current_exe().unwrap();
        let target = fs::canonicalize(binary.parent().unwrap()).unwrap();
        let parent = target.join("restore-drill-native-fixtures");
        match fs::create_dir(&parent) {
            Ok(()) => (),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => (),
            Err(error) => panic!("fixture parent: {error}"),
        }
        assert!(plain_directory(&parent));
        let parent = fs::canonicalize(parent).unwrap();
        assert_eq!(parent.parent(), Some(target.as_path()));
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        for _ in 0..100 {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let root = parent.join(format!("native-{}-{stamp}-{id}", std::process::id()));
            match fs::create_dir(&root) {
                Ok(()) => {
                    let backup = root.join("backup");
                    let scratch = root.join("scratch");
                    fs::create_dir(&backup).unwrap();
                    fs::create_dir(&scratch).unwrap();
                    fs::write(backup.join(".industrial1710-backup"), MARKER).unwrap();
                    fs::write(backup.join("flat.irw"), contents).unwrap();
                    return Self {
                        root,
                        parent,
                        backup,
                        scratch,
                    };
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("fixture root: {error}"),
            }
        }
        panic!("could not create a unique fixture");
    }

    fn drill(&self) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_restore-drill"))
            .arg("--server")
            .arg(env!("CARGO_BIN_EXE_vanilla-server"))
            .arg("--backup")
            .arg(&self.backup)
            .arg("--scratch-root")
            .arg(&self.scratch)
            .args(["--timeout-seconds", "20"])
            .output()
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let owned = plain_directory(&self.root)
            && self
                .root
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("native-"))
            && fs::canonicalize(&self.root)
                .is_ok_and(|path| path.parent() == Some(self.parent.as_path()));
        if !owned {
            eprintln!("fixture ownership unknown: {}", self.root.display());
            return;
        }
        let result =
            fs::remove_dir_all(&self.root).and_then(|()| match fs::symlink_metadata(&self.root) {
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
                Ok(_) => Err(std::io::Error::other("fixture still exists")),
                Err(error) => Err(error),
            });
        if let Err(error) = result {
            if std::thread::panicking() {
                eprintln!("fixture cleanup: {error}");
            } else {
                panic!("fixture cleanup: {error}");
            }
        }
    }
}

#[test]
fn real_native_valid_irw_succeeds_without_changing_backup_and_cleans_copy() {
    let world = b"IRW1\0\0\0\0";
    let fixture = Fixture::new(world);
    let result = fixture.drill();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        String::from_utf8_lossy(&result.stdout).contains("offline restore verification succeeded")
    );
    assert_eq!(fs::read(fixture.backup.join("flat.irw")).unwrap(), world);
    assert_eq!(
        fs::read(fixture.backup.join(".industrial1710-backup")).unwrap(),
        MARKER
    );
    assert_eq!(fs::read_dir(&fixture.scratch).unwrap().count(), 0);
}

#[test]
fn real_native_invalid_irw_fails_and_keeps_only_isolated_copy() {
    let world = b"bad";
    let fixture = Fixture::new(world);
    let result = fixture.drill();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("bad world header"));
    assert_eq!(fs::read(fixture.backup.join("flat.irw")).unwrap(), world);
    let entries: Vec<_> = fs::read_dir(&fixture.scratch).unwrap().collect();
    assert_eq!(entries.len(), 1);
    let retained = entries[0].as_ref().unwrap().path();
    assert_eq!(fs::read(retained.join("flat.irw")).unwrap(), world);
    assert_eq!(
        fs::read(retained.join(".industrial1710-backup")).unwrap(),
        MARKER
    );
}
