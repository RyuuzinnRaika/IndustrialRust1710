//! Create-only offline directory backups. Never replace a source or destination.
//! The caller holds a WorldSessionLock for the full snapshot operation.
//! Backups are staged alongside their final path; failed copies are removed.
use crate::session_lock::WorldSessionLock;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Read};
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub struct BackupStats {
    pub files: usize,
    pub bytes: u64,
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, message)
}

fn context(error: io::Error, operation: &str) -> io::Error {
    io::Error::new(error.kind(), format!("{operation}: {error}"))
}

fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Verify the bytes, not just the lengths, of each offline backup entry.
fn verify_file_copy(source: &Path, target: &Path, expected_len: u64) -> io::Result<()> {
    let source_metadata = fs::symlink_metadata(source)?;
    let target_metadata = fs::symlink_metadata(target)?;
    if !source_metadata.file_type().is_file()
        || !target_metadata.file_type().is_file()
        || source_metadata.len() != expected_len
        || target_metadata.len() != expected_len
    {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "backup source or destination changed size or type",
        ));
    }
    let mut source_file = File::open(source)?;
    let mut target_file = File::open(target)?;
    let mut left = [0u8; 64 * 1024];
    let mut right = [0u8; 64 * 1024];
    loop {
        let read = source_file.read(&mut left)?;
        if read == 0 {
            break;
        }
        target_file.read_exact(&mut right[..read])?;
        if left[..read] != right[..read] {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "backup copy differs from source contents",
            ));
        }
    }
    if source_file.metadata()?.len() != expected_len
        || target_file.metadata()?.len() != expected_len
    {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "backup source or destination changed during verification",
        ));
    }
    Ok(())
}

fn copy_directory(
    src: &Path,
    dst: &Path,
    stats: &mut BackupStats,
    session: &mut WorldSessionLock,
) -> io::Result<()> {
    for entry in
        fs::read_dir(src).map_err(|error| context(error, "read backup source directory"))?
    {
        let entry = entry?;
        let source = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(invalid("backup refuses symbolic links"));
        }
        let target = dst.join(entry.file_name());
        if file_type.is_dir() {
            fs::create_dir(&target)
                .map_err(|error| context(error, "create backup subdirectory"))?;
            copy_directory(&source, &target, stats, session)
                .map_err(|error| context(error, "copy backup subdirectory"))?;
            sync_directory(&target).map_err(|error| context(error, "sync backup subdirectory"))?;
        } else if file_type.is_file() {
            if session.is_lock_path(&source)? {
                let copied = session
                    .copy_marker_to(&target)
                    .map_err(|error| context(error, "copy locked session marker"))?;
                stats.files += 1;
                stats.bytes = stats
                    .bytes
                    .checked_add(copied)
                    .ok_or_else(|| invalid("backup size overflow"))?;
                continue;
            }
            let length = entry
                .metadata()
                .map_err(|error| context(error, "read backup source metadata"))?
                .len();
            let copied = fs::copy(&source, &target)
                .map_err(|error| context(error, &format!("copy {}", source.display())))?;
            if copied != length
                || fs::metadata(&source)
                    .map_err(|error| context(error, "recheck backup source metadata"))?
                    .len()
                    != length
            {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    "file changed while copying the world",
                ));
            }
            verify_file_copy(&source, &target, length)
                .map_err(|error| context(error, "verify copied world file"))?;
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(&target)
                .and_then(|file| file.sync_all())
                .map_err(|error| context(error, "sync copied world file"))?;
            stats.files += 1;
            stats.bytes = stats
                .bytes
                .checked_add(copied)
                .ok_or_else(|| invalid("backup size overflow"))?;
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
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let canonical_parent = fs::canonicalize(parent)?;
    if canonical_parent.starts_with(&canonical_source) {
        return Err(invalid(
            "backup destination must not be inside the source world",
        ));
    }
    match fs::symlink_metadata(destination) {
        Ok(_) => {
            return Err(io::Error::new(
                ErrorKind::AlreadyExists,
                "backup destination exists",
            ))
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut staging_name = dest_name.to_os_string();
    staging_name.push(".incomplete");
    let staging = parent.join(staging_name);
    session
        .verify()
        .map_err(|error| context(error, "verify session before backup"))?;
    fs::create_dir(&staging).map_err(|error| context(error, "create backup staging directory"))?;
    // Never remove a pre-existing incomplete backup.
    let result = (|| {
        let mut stats = BackupStats { files: 0, bytes: 0 };
        copy_directory(source, &staging, &mut stats, session)
            .map_err(|error| context(error, "copy world directory"))?;
        session
            .verify()
            .map_err(|error| context(error, "verify session after backup copy"))?;
        sync_directory(&staging)?;
        // Rename is on the same filesystem because staging is a sibling.
        // Check again to reject a destination concurrently created elsewhere.
        if fs::symlink_metadata(destination).is_ok() {
            return Err(io::Error::new(
                ErrorKind::AlreadyExists,
                "backup destination appeared",
            ));
        }
        fs::rename(&staging, destination)
            .map_err(|error| context(error, "publish completed backup directory"))?;
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
            "ir1710-backup-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let world = root.join("world");
        fs::create_dir_all(world.join("players")).unwrap();
        fs::write(world.join("flat.irw"), b"IRW1").unwrap();
        fs::write(world.join("players/test.dat"), b"save-data").unwrap();
        let mut session = WorldSessionLock::acquire(&world).unwrap();
        let dest = root.join("backup");
        let stats = create(&world, &dest, &mut session)
            .unwrap_or_else(|error| panic!("backup create failed: {error}"));
        assert_eq!(stats.files, 3); // Two saves and session.lock.
        assert_eq!(fs::read(dest.join("flat.irw")).unwrap(), b"IRW1");
        assert_eq!(
            fs::read(dest.join("players/test.dat")).unwrap(),
            b"save-data"
        );
        assert!(create(&world, &dest, &mut session).is_err());
        assert!(create(&world, &world.join("nested"), &mut session).is_err());
        assert_eq!(fs::read(world.join("flat.irw")).unwrap(), b"IRW1");
        drop(session);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn backup_content_validation_rejects_same_length_mutation() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-backup-integrity-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root).unwrap();
        let source = root.join("original");
        let target = root.join("snapshot");
        let bytes = vec![0x55u8; 131_073];
        fs::write(&source, &bytes).unwrap();
        fs::copy(&source, &target).unwrap();
        verify_file_copy(&source, &target, bytes.len() as u64).unwrap();
        // Equal size does not mean equal bytes.
        let mut modified = bytes.clone();
        modified[65_537] = 0x57;
        fs::write(&source, modified).unwrap();
        assert_eq!(fs::metadata(&source).unwrap().len(), bytes.len() as u64);
        assert!(verify_file_copy(&source, &target, bytes.len() as u64).is_err());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn replaced_session_timestamp_prevents_backup_publication() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-backup-race-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("world")).unwrap();
        let mut session = WorldSessionLock::acquire(&root.join("world")).unwrap();
        session.replace_timestamp_for_test(0).unwrap();
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
            "ir1710-backup-link-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
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
