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

#[cfg(windows)]
#[repr(C)]
#[derive(Default)]
struct WindowsFileInformation {
    _attributes: u32,
    _creation_time: [u32; 2],
    _last_access_time: [u32; 2],
    _last_write_time: [u32; 2],
    volume_serial_number: u32,
    _file_size_high: u32,
    _file_size_low: u32,
    _number_of_links: u32,
    file_index_high: u32,
    file_index_low: u32,
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetFileInformationByHandle(
        file: *mut std::ffi::c_void,
        information: *mut WindowsFileInformation,
    ) -> i32;
}

#[cfg(windows)]
fn windows_file_identity(file: &File) -> io::Result<(u32, u64)> {
    use std::mem::MaybeUninit;
    use std::os::windows::io::AsRawHandle;

    let mut information = MaybeUninit::<WindowsFileInformation>::uninit();
    // SAFETY: `file` is an open handle and `information` points to a writable
    // buffer with the layout expected by GetFileInformationByHandle.
    let succeeded =
        unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the Windows API initialized the output buffer on success.
    let information = unsafe { information.assume_init() };
    let file_index =
        (u64::from(information.file_index_high) << 32) | u64::from(information.file_index_low);
    Ok((information.volume_serial_number, file_index))
}

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

    pub(crate) fn is_lock_path(&self, path: &Path) -> io::Result<bool> {
        if path.file_name() != self.path.file_name() {
            return Ok(false);
        }
        Ok(fs::canonicalize(path)? == fs::canonicalize(&self.path)?)
    }

    /// Copy the vanilla marker through the handle that owns its exclusive
    /// lock. On Windows, opening the locked byte range through a second handle
    /// can fail even when this process owns the lock.
    pub(crate) fn copy_marker_to(&mut self, destination: &Path) -> io::Result<u64> {
        self.verify()?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)?;
        self.file.seek(SeekFrom::Start(0))?;
        let mut marker = [0u8; 8];
        self.file.read_exact(&mut marker)?;
        output.write_all(&marker)?;
        output.sync_all()?;
        self.verify()?;
        Ok(marker.len() as u64)
    }

    #[cfg(test)]
    pub(crate) fn replace_timestamp_for_test(&mut self, timestamp_ms: i64) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&timestamp_ms.to_be_bytes())?;
        self.file.sync_all()
    }

    #[cfg(test)]
    pub(crate) fn timestamp_for_test(&mut self) -> io::Result<i64> {
        self.file.seek(SeekFrom::Start(0))?;
        let mut marker = [0u8; 8];
        self.file.read_exact(&mut marker)?;
        Ok(i64::from_be_bytes(marker))
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
            .open(&lock_file)
            .map_err(|error| {
                if cfg!(windows) && error.raw_os_error() == Some(33) {
                    io::Error::new(
                        error.kind(),
                        format!(
                            "cannot open Minecraft world lock {}: {error}. Another process may have locked the file; stop the other server and retry. Do not delete session.lock.",
                            lock_file.display()
                        ),
                    )
                } else {
                    error
                }
            })?;
        FileExt::try_lock_exclusive(&file).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "cannot acquire Minecraft world lock {}: {error}. Another server or process may be using this world; stop it gracefully before restarting. Do not delete session.lock.",
                    lock_file.display()
                ),
            )
        })?;
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
        let visible = File::open(&self.path)?;
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

        #[cfg(windows)]
        if windows_file_identity(&self.file)? != windows_file_identity(&visible)? {
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                "Minecraft session.lock was replaced while the world was open",
            ));
        }

        // Read through the handle that owns the exclusive byte-range lock.
        // Windows rejects reads of this locked range through a second handle.
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
        assert_eq!(first.timestamp_for_test().unwrap(), first.started_ms);
        assert!(first.verify().is_ok());
        let contention = WorldSessionLock::acquire(&root).unwrap_err();
        assert!(contention.to_string().contains("session.lock"));
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
        lock.replace_timestamp_for_test(1).unwrap();
        assert!(lock.verify().is_err());
        drop(lock);
        fs::remove_dir_all(root).unwrap();
    }
}
