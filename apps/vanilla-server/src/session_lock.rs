//! World session lock for the Minecraft Java 1.7.10 on-disk layout.
//! The 1.7.10 session.lock contains an 8-byte big-endian millisecond timestamp.
//! We additionally hold an advisory OS lock while our process is running.
//! This does NOT guarantee coordination with unmodified Java servers: they
//! generally use timestamp validation, not the OS-level advisory lock.
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug)]
pub struct WorldSessionLock {
    file: File,
    path: PathBuf,
    started_ms: i64,
}

impl WorldSessionLock {
    /// The verified world directory; callers must still call verify() before
    /// publishing any world files. Never accept an unrelated Anvil path.
    pub(crate) fn world_dir(&self) -> &Path {
        self.path.parent().expect("session.lock has a parent")
    }

    pub fn acquire(world_dir: &Path) -> io::Result<Self> {
        match fs::symlink_metadata(world_dir) {
            Ok(metadata) if !metadata.file_type().is_dir() => {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    "world directory must be a real directory",
                ));
            }
            Ok(_) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => {
                fs::create_dir_all(world_dir)?;
            }
            Err(err) => return Err(err),
        }

        let lock_file = world_dir.join("session.lock");
        match fs::symlink_metadata(&lock_file) {
            Ok(metadata) if !metadata.file_type().is_file() => {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    "session.lock must be a regular file",
                ));
            }
            Ok(_) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false) // Never alter the file before acquiring its lock.
            .open(&lock_file)?;
        FileExt::try_lock_exclusive(&file)?;
        let duration = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| io::Error::new(ErrorKind::InvalidData, "system clock before epoch"))?;
        let started_ms = i64::try_from(duration.as_millis())
            .map_err(|_| io::Error::new(ErrorKind::InvalidData, "system clock overflow"))?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&started_ms.to_be_bytes())?;
        file.set_len(8)?;
        file.sync_all()?;
        Ok(Self {
            file,
            path: lock_file,
            started_ms,
        })
    }

    /// Catch an overwritten 1.7.10 timestamp when this method is invoked.
    /// Future storage integration must invoke it on every save boundary.
    pub fn verify(&mut self) -> io::Result<()> {
        // An advisory OS lock only protects the original file handle. A
        // second process can replace the directory entry and leave that
        // original inode locked but no longer visible from the world folder.
        let named = fs::symlink_metadata(&self.path)?;
        if !named.file_type().is_file() || named.len() != 8 {
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                "Minecraft session.lock was replaced or is not a regular 8-byte file",
            ));
        }
        let mut visible = File::open(&self.path)?;
        let opened = visible.metadata()?;
        if opened.len() != 8 {
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                "Minecraft session.lock changed while checking it",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let locked = self.file.metadata()?;
            if named.dev() != locked.dev()
                || named.ino() != locked.ino()
                || opened.dev() != locked.dev()
                || opened.ino() != locked.ino()
            {
                return Err(io::Error::new(
                    ErrorKind::PermissionDenied,
                    "Minecraft session.lock was replaced while the world was open",
                ));
            }
        }
        let mut visible_marker = [0u8; 8];
        visible.read_exact(&mut visible_marker)?;
        if i64::from_be_bytes(visible_marker) != self.started_ms {
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                "Minecraft session.lock on disk no longer matches the active session",
            ));
        }
        self.file.seek(SeekFrom::Start(0))?;
        let mut marker = [0u8; 8];
        self.file.read_exact(&mut marker)?;
        if i64::from_be_bytes(marker) != self.started_ms {
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                "session.lock changed: another Minecraft server may own the world",
            ));
        }
        if self.file.metadata()?.len() != 8 {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "invalid Minecraft 1.7.10 session.lock length",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_vanilla_timestamp_and_rejects_concurrent_rust_writer() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-session-lock-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let mut first = WorldSessionLock::acquire(&root).unwrap();
        let bytes = fs::read(root.join("session.lock")).unwrap();
        assert_eq!(bytes.len(), 8);
        assert_eq!(
            i64::from_be_bytes(bytes.try_into().unwrap()),
            first.started_ms
        );
        assert!(first.verify().is_ok());
        assert!(WorldSessionLock::acquire(&root).is_err());
        drop(first);
        let mut next = WorldSessionLock::acquire(&root).unwrap();
        next.verify().unwrap();
        drop(next);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_replaced_lock_file_even_when_timestamp_matches() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-session-replacement-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let mut guard = WorldSessionLock::acquire(&root).unwrap();
        let lock_path = root.join("session.lock");
        // A Java-style replacement of the directory entry leaves the Rust
        // process with a lock on the old inode. The marker can even match.
        fs::rename(&lock_path, root.join("old-session.lock")).unwrap();
        fs::write(&lock_path, guard.started_ms.to_be_bytes()).unwrap();
        assert!(guard.verify().is_err());
        drop(guard);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn detects_timestamp_replaced_by_another_writer() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-session-race-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let mut lock = WorldSessionLock::acquire(&root).unwrap();
        // Unmodified Java servers can overwrite the marker despite advisory
        // locking. Always fail closed when the marker differs.
        fs::write(root.join("session.lock"), 1i64.to_be_bytes()).unwrap();
        assert!(lock.verify().is_err());
        drop(lock);
        fs::remove_dir_all(root).unwrap();
    }
}
