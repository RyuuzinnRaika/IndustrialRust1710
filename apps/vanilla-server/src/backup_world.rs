//! Create-only offline directory backups. Never replace a source or destination.
//! The caller holds a WorldSessionLock for the full snapshot operation.
//! Backups are staged alongside their final path; failed copies are removed.
use crate::session_lock::WorldSessionLock;
use std::fs::{self, File};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub struct BackupStats {
    pub files: usize,
    pub bytes: u64,
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, message)
}

fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn copy_directory(src: &Path, dst: &Path, stats: &mut BackupStats) -> io::Result<()> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let source = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(invalid("backup refuses symbolic links"));
        }
        let target = dst.join(entry.file_name());
        if file_type.is_dir() {
            fs::create_dir(&target)?;
            copy_directory(&source, &target, stats)?;
            sync_directory(&target)?;
        } else if file_type.is_file() {
            let length = entry.metadata()?.len();
            let copied = fs::copy(&source, &target)?;
            if copied != length || fs::metadata(&source)?.len() != length {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    "file changed while copying the world",
                ));
            }
            File::open(&target)?.sync_all()?;
            stats.files += 1;
            stats.bytes = stats.bytes.checked_add(copied).ok_or_else(|| invalid("backup size overflow"))?;
        } else {
            return Err(invalid("backup refuses special files"));
        }
    }
    Ok(())
}

/// Copy a stopped world to a new, non-nested directory while holding its lock.
/// A live unmodified Java server that does not cooperate with the OS lock is
/// not fully excluded; the timestamp is checked both before and after copying.
pub fn create(
    source: &Path,
    destination: &Path,
    session: &mut WorldSessionLock,
) -> io::Result<BackupStats> {
    if !fs::symlink_metadata(source)?.file_type().is_dir() {
        return Err(invalid("source must be a real world directory"));
    }
    let canonical_source = fs::canonicalize(source)?;
    let dest_name = destination
        .file_name()
        .ok_or_else(|| invalid("destination must be a directory name"))?;
    let parent = destination.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let canonical_parent = fs::canonicalize(parent)?;
    if canonical_parent.starts_with(&canonical_source) {
        return Err(invalid("backup destination must not be inside the source world"));
    }
    match fs::symlink_metadata(destination) {
        Ok(_) => return Err(io::Error::new(ErrorKind::AlreadyExists, "backup destination exists")),
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut staging_name = dest_name.to_os_string();
    staging_name.push(".incomplete");
    let staging = parent.join(staging_name);
    session.verify()?;
    fs::create_dir(&staging)?; // Never remove a pre-existing incomplete backup.
    let result = (|| {
        let mut stats = BackupStats { files: 0, bytes: 0 };
        copy_directory(source, &staging, &mut stats)?;
        session.verify()?;
        sync_directory(&staging)?;
        // Rename is on the same filesystem because staging is a sibling.
        // Check again to reject a destination concurrently created elsewhere.
        if fs::symlink_metadata(destination).is_ok() {
            return Err(io::Error::new(ErrorKind::AlreadyExists, "backup destination appeared"));
        }
        fs::rename(&staging, destination)?;
        sync_directory(parent)?;
        Ok(stats)
    })();
    if result.is_err() {
        // Only remove staging that *this* call created.
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_is_complete_create_only_and_does_not_overwrite() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-backup-{}-{:?}", std::process::id(), std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let world = root.join("world");
        fs::create_dir_all(world.join("players")).unwrap();
        fs::write(world.join("flat.irw"), b"IRW1").unwrap();
        fs::write(world.join("players/test.dat"), b"save-data").unwrap();
        let mut session = WorldSessionLock::acquire(&world).unwrap();
        let dest = root.join("backup");
        let stats = create(&world, &dest, &mut session).unwrap();
        assert_eq!(stats.files, 3); // Two saves and session.lock.
        assert_eq!(fs::read(dest.join("flat.irw")).unwrap(), b"IRW1");
        assert_eq!(fs::read(dest.join("players/test.dat")).unwrap(), b"save-data");
        assert!(create(&world, &dest, &mut session).is_err());
        assert!(create(&world, &world.join("nested"), &mut session).is_err());
        assert_eq!(fs::read(world.join("flat.irw")).unwrap(), b"IRW1");
        drop(session);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn replaced_session_timestamp_prevents_backup_publication() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-backup-race-{}-{:?}", std::process::id(), std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("world")).unwrap();
        let mut session = WorldSessionLock::acquire(&root.join("world")).unwrap();
        fs::write(root.join("world/session.lock"), 0i64.to_be_bytes()).unwrap();
        let dest = root.join("backup");
        assert!(create(&root.join("world"), &dest, &mut session).is_err());
        assert!(!dest.exists());
        drop(session);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn backup_rejects_symlink_inputs_and_never_publishes_partial_data() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!(
            "ir1710-backup-link-{}-{:?}", std::process::id(), std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("world")).unwrap();
        let mut session = WorldSessionLock::acquire(&root.join("world")).unwrap();
        symlink("/etc/hosts", root.join("world/not-a-save")).unwrap();
        let dest = root.join("backup");
        assert!(create(&root.join("world"), &dest, &mut session).is_err());
        assert!(!dest.exists());
        assert!(!root.join("backup.incomplete").exists());
        drop(session);
        fs::remove_dir_all(root).unwrap();
    }
}
