//! Recovery journal for a bounded set of owned save files.
//!
//! A committed journal is replayed to completion before any participant is loaded.
//! Callers MUST serialize all access to participants, use a dedicated journal per
//! transaction domain and call recover with the exact same ordered target paths.
//! This module is a primitive; it is not yet wired to all game transactions.
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::Path;

const MAX_FILES: usize = 8;
const MAX_ENTRY_BYTES: usize = 2 * 1024 * 1024;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}
fn sync_parent(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent().filter(|part| !part.as_os_str().is_empty()) {
        // Directory syncing is supported on Unix. Some Windows filesystems
        // reject opening directories; atomically replaced file data is synced.
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        #[cfg(not(unix))]
        let _ = parent;
    }
    Ok(())
}
fn staged(journal: &Path, index: usize) -> std::path::PathBuf {
    journal.join(format!("stage-{index:02}"))
}
fn validate_targets(targets: &[&Path]) -> io::Result<()> {
    if targets.is_empty() || targets.len() > MAX_FILES {
        return Err(invalid("invalid journal participant count"));
    }
    for (i, path) in targets.iter().enumerate() {
        if path.file_name().is_none() || targets[..i].contains(path) {
            return Err(invalid("duplicate or invalid journal participant"));
        }
    }
    Ok(())
}
fn marker(journal: &Path) -> std::path::PathBuf {
    journal.join("COMMITTED")
}
/// Replay a previously committed set of replacements. The ordered list of
/// targets must be identical to the list passed to commit.
pub fn recover(journal: &Path, targets: &[&Path]) -> io::Result<()> {
    validate_targets(targets)?;
    if !journal.exists() {
        return Ok(());
    }
    if !journal.is_dir() || journal.is_symlink() {
        return Err(invalid("journal is not a regular directory"));
    }
    let marker = marker(journal);
    if marker.exists() {
        let manifest = fs::read(&marker)?;
        if manifest != (targets.len() as u64).to_le_bytes() {
            return Err(invalid("journal participant mismatch"));
        }
        for (index, target) in targets.iter().enumerate() {
            let staged = staged(journal, index);
            if staged.exists() {
                if staged.is_symlink()
                    || !staged.is_file()
                    || staged.metadata()?.len() > MAX_ENTRY_BYTES as u64
                {
                    return Err(invalid("unsafe staged recovery entry"));
                }
                if let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty()) {
                    fs::create_dir_all(parent)?;
                }
                fs::rename(&staged, target)?;
                sync_parent(target)?;
            } else if !target.is_file() {
                return Err(invalid("missing committed journal participant"));
            }
        }
    }
    fs::remove_dir_all(journal)?;
    sync_parent(journal)?;
    Ok(())
}
/// Atomically choose *commit* for all participants before replacing any of them.
/// On failure after COMMITTED, caller MUST stop handling transactions and run
/// recover; it must not continue with stale in-memory state.
pub fn commit(journal: &Path, writes: &[(&Path, &[u8])]) -> io::Result<()> {
    let targets: Vec<&Path> = writes.iter().map(|entry| entry.0).collect();
    validate_targets(&targets)?;
    if journal.exists() {
        return Err(io::Error::new(
            ErrorKind::AlreadyExists,
            "journal recovery required",
        ));
    }
    fs::create_dir(journal)?;
    sync_parent(journal)?;
    for (index, (_, bytes)) in writes.iter().enumerate() {
        if bytes.len() > MAX_ENTRY_BYTES {
            return Err(invalid("journal entry exceeds size limit"));
        }
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(staged(journal, index))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    sync_parent(&staged(journal, 0))?;
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(marker(journal))?;
    f.write_all(&(writes.len() as u64).to_le_bytes())?;
    f.sync_all()?;
    sync_parent(&marker(journal))?;
    recover(journal, &targets)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn sandbox(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ir1710-journal-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
    #[test]
    fn commit_replaces_two_files_and_cleans_journal() {
        let dir = sandbox("commit");
        let a = dir.join("inv.json");
        let b = dir.join("chests.json");
        fs::write(&a, b"old-a").unwrap();
        fs::write(&b, b"old-b").unwrap();
        let log = dir.join("pending");
        commit(&log, &[(&a, b"new-a"), (&b, b"new-b")]).unwrap();
        assert_eq!(fs::read(&a).unwrap(), b"new-a");
        assert_eq!(fs::read(&b).unwrap(), b"new-b");
        assert!(!log.exists());
        recover(&log, &[&a, &b]).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn committed_partial_rename_rolls_forward() {
        let dir = sandbox("replay");
        let a = dir.join("a");
        let b = dir.join("b");
        fs::write(&a, b"new-a").unwrap();
        fs::write(&b, b"old-b").unwrap();
        let log = dir.join("pending");
        fs::create_dir(&log).unwrap();
        fs::write(staged(&log, 1), b"new-b").unwrap();
        fs::write(marker(&log), 2u64.to_le_bytes()).unwrap();
        recover(&log, &[&a, &b]).unwrap();
        assert_eq!(fs::read(&b).unwrap(), b"new-b");
        assert!(!log.exists());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn uncommitted_staging_does_not_touch_targets() {
        let dir = sandbox("aborted");
        let a = dir.join("a");
        fs::write(&a, b"original").unwrap();
        let log = dir.join("pending");
        fs::create_dir(&log).unwrap();
        fs::write(staged(&log, 0), b"aborted").unwrap();
        recover(&log, &[&a]).unwrap();
        assert_eq!(fs::read(&a).unwrap(), b"original");
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn mismatched_count_does_not_discard_committed_data() {
        let dir = sandbox("mismatch");
        let a = dir.join("a");
        let log = dir.join("pending");
        fs::create_dir(&log).unwrap();
        fs::write(marker(&log), 2u64.to_le_bytes()).unwrap();
        assert!(recover(&log, &[&a]).is_err());
        assert!(log.exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
