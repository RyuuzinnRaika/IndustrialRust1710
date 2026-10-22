//! J96: bounded, non-destructive Anvil *working copy*, not full IRW conversion.
use std::fs::{self, File};
use std::io::{self, ErrorKind, Read};
use std::path::{Path, PathBuf};

const MAX_FILES: usize = 100_000;
const MAX_BYTES: u64 = 32 * 1024 * 1024 * 1024;
const MAX_DEPTH: usize = 12;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

#[derive(Debug, Default)]
pub struct CopyStats { pub files: usize, pub bytes: u64 }

fn compare_files(original: &Path, duplicate: &Path) -> io::Result<()> {
    let mut a = File::open(original)?;
    let mut b = File::open(duplicate)?;
    let (mut first, mut second) = ([0u8; 64 * 1024], [0u8; 64 * 1024]);
    loop {
        let n = a.read(&mut first)?;
        let m = b.read(&mut second)?;
        if n != m || first[..n] != second[..m] {
            return Err(invalid("source and copy bytes differ"));
        }
        if n == 0 { return Ok(()); }
    }
}

fn copy_tree(source: &Path, dest: &Path, depth: usize, stats: &mut CopyStats) -> io::Result<()> {
    if depth > MAX_DEPTH { return Err(invalid("Anvil world directory nesting too deep")); }
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let meta = fs::symlink_metadata(&from)?;
        if meta.file_type().is_dir() {
            fs::create_dir(&to)?;
            copy_tree(&from, &to, depth + 1, stats)?;
        } else if meta.file_type().is_file() {
            stats.files = stats.files.checked_add(1).ok_or_else(|| invalid("too many files"))?;
            stats.bytes = stats.bytes.checked_add(meta.len()).ok_or_else(|| invalid("size overflow"))?;
            if stats.files > MAX_FILES || stats.bytes > MAX_BYTES {
                return Err(invalid("Anvil world exceeds migration copy limits"));
            }
            let copied = fs::copy(&from, &to)?;
            if copied != meta.len() || fs::metadata(&from)?.len() != copied {
                return Err(invalid("source mutated during copy"));
            }
            compare_files(&from, &to)?;
            File::open(&to)?.sync_all()?;
        } else {
            return Err(invalid("refusing symlinks, sockets and special input files"));
        }
    }
    Ok(())
}

/// Inspect source without ever creating a lock there; produce a separately
/// validated, byte-for-byte copy. No transformation to IRW is implied.
pub fn prepare_anvil_copy(source: &Path, destination: &Path) -> io::Result<CopyStats> {
    if !fs::symlink_metadata(source)?.file_type().is_dir() {
        return Err(invalid("source must be a real directory"));
    }
    // Read-only format validation precedes any output creation.
    let before = crate::legacy_world_inspect::inspect_world(source)?;
    let canonical_source = fs::canonicalize(source)?;
    let dest_name = destination.file_name().ok_or_else(|| invalid("destination has no name"))?;
    let parent = destination.parent().filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let canonical_parent = fs::canonicalize(parent)?;
    if canonical_parent.starts_with(&canonical_source) ||
        canonical_source.starts_with(&canonical_parent) {
        return Err(invalid("destination must not overlap source tree"));
    }
    match fs::symlink_metadata(destination) {
        Ok(_) => return Err(io::Error::new(ErrorKind::AlreadyExists, "destination already exists")),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut stage_name = dest_name.to_os_string();
    stage_name.push(".incomplete");
    let staging = parent.join(stage_name);
    fs::create_dir(&staging)?; // An existing incomplete stage is never removed.
    let work = (|| -> io::Result<CopyStats> {
        let mut stats = CopyStats::default();
        copy_tree(source, &staging, 0, &mut stats)?;
        let after = crate::legacy_world_inspect::inspect_world(source)?;
        let verified = crate::legacy_world_inspect::inspect_world(&staging)?;
        if before != after || before != verified {
            return Err(invalid("Anvil structural summary changed while copying"));
        }
        if fs::symlink_metadata(destination).is_ok() {
            return Err(io::Error::new(ErrorKind::AlreadyExists, "destination appeared"));
        }
        fs::rename(&staging, destination)?;
        Ok(stats)
    })();
    if work.is_err() { let _ = fs::remove_dir_all(&staging); }
    work
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn copies_valid_anvil_without_modifying_source() {
        let root = std::env::temp_dir().join(format!("j96-copy-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source");
        crate::anvil_export::export(&crate::world::World::new(), &source).unwrap();
        let original = fs::read(source.join("level.dat")).unwrap();
        let dest = root.join("workcopy");
        assert!(prepare_anvil_copy(&source, &dest).unwrap().files >= 5);
        assert_eq!(fs::read(source.join("level.dat")).unwrap(), original);
        assert_eq!(fs::read(dest.join("level.dat")).unwrap(), original);
        assert!(!source.join("session.lock").exists());
        assert!(prepare_anvil_copy(&source, &dest).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_source_is_rejected_before_copy() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!("j96-symlink-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source");
        crate::anvil_export::export(&crate::world::World::new(), &source).unwrap();
        symlink("/etc/hosts", source.join("foreign")).unwrap();
        let destination = root.join("workcopy");
        assert!(prepare_anvil_copy(&source, &destination).is_err());
        assert!(!destination.exists());
        fs::remove_dir_all(root).unwrap();
    }
}
