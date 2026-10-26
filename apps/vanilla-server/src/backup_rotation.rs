//! J92 offline rotating snapshot helper. Retention never touches unmarked dirs.
use crate::{backup_world, session_lock::WorldSessionLock, verify_world};
use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MARKER: &[u8] = b"IndustrialRust1710 verified offline backup v1\n";
fn invalid(msg: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, msg)
}

/// World must be STOPPED. Create-only snapshot, independently validate it,
/// then prune oldest backups bearing exactly the marker written by this tool.
pub fn rotate(
    world_file: &Path,
    root: &Path,
    keep: usize,
    lock: &mut WorldSessionLock,
) -> io::Result<(PathBuf, backup_world::BackupStats)> {
    if !(1..=100).contains(&keep) {
        return Err(invalid("retention must be 1..=100"));
    }
    let source = world_file
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| invalid("world path has no parent"))?;
    if !fs::symlink_metadata(source)?.file_type().is_dir() {
        return Err(invalid("world directory must be real"));
    }
    if !root.exists() {
        fs::create_dir_all(root)?;
    }
    if !fs::symlink_metadata(root)?.file_type().is_dir() {
        return Err(invalid("backup root must be real"));
    }
    let source = fs::canonicalize(source)?;
    let backup_root = fs::canonicalize(root)?;
    if backup_root.starts_with(&source) || source.starts_with(&backup_root) {
        return Err(invalid("backup root overlaps source"));
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| invalid("clock before epoch"))?
        .as_millis();
    let destination = root.join(format!("backup-{stamp}-{}", std::process::id()));
    let stats = backup_world::create(&source, &destination, lock)?;
    let named_world = destination.join(
        world_file
            .file_name()
            .ok_or_else(|| invalid("world file has no basename"))?,
    );
    if named_world.is_file() {
        verify_world::inspect(&named_world)?;
    } else {
        verify_world::inspect(&destination)?;
    }
    let marker = destination.join(".industrial1710-backup");
    fs::write(&marker, MARKER)?;
    OpenOptions::new().write(true).open(&marker)?.sync_all()?;
    let mut owned = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_name().to_string_lossy().starts_with("backup-")
            || !entry.file_type()?.is_dir()
        {
            continue;
        }
        let dir = entry.path();
        let marker = dir.join(".industrial1710-backup");
        if fs::symlink_metadata(&marker).is_ok_and(|meta| meta.file_type().is_file())
            && fs::read(&marker)? == MARKER
        {
            owned.push(dir);
        }
    }
    owned.sort();
    let delete_count = owned.len().saturating_sub(keep);
    for old in owned.into_iter().take(delete_count) {
        // Revalidate immediately before removal; never follow marked symlinks.
        if old != destination
            && fs::symlink_metadata(&old)?.file_type().is_dir()
            && fs::symlink_metadata(old.join(".industrial1710-backup"))?
                .file_type()
                .is_file()
            && fs::read(old.join(".industrial1710-backup"))? == MARKER
        {
            fs::remove_dir_all(&old)?;
        }
    }
    Ok((destination, stats))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::World;
    #[test]
    fn rotation_validates_and_never_deletes_unmarked_directory() {
        let root = std::env::temp_dir().join(format!("j92-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let world = root.join("world");
        fs::create_dir_all(&world).unwrap();
        let file = world.join("flat.irw");
        World::new().save_new(&file).unwrap();
        let mut lock = WorldSessionLock::acquire(&world).unwrap();
        let backups = root.join("backups");
        fs::create_dir_all(backups.join("backup-external")).unwrap();
        let (first, _) = rotate(&file, &backups, 1, &mut lock).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let (second, _) = rotate(&file, &backups, 1, &mut lock).unwrap();
        assert!(!first.exists());
        assert!(second.join("flat.irw").exists());
        assert!(backups.join("backup-external").exists());
        drop(lock);
        fs::remove_dir_all(root).unwrap();
    }
}
