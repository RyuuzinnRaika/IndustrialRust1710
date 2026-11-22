//! Bounded, read-only backup tree snapshots for the offline restore drill.
//!
//! The caller must supply a stopped backup that no other process can modify.
//! Path checks detect observed changes; they do not eliminate hostile races.
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, Metadata};
use std::io::{self, ErrorKind, Read};
use std::path::{Component, Path, PathBuf};

const MARKER_NAME: &str = ".industrial1710-backup";
const MARKER: &[u8] = b"IndustrialRust1710 verified offline backup v1\n";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    Directory,
    File { bytes: u64, sha256: [u8; 32] },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub(crate) entries: BTreeMap<PathBuf, Entry>,
    pub(crate) files: u64,
    pub(crate) bytes: u64,
    path_bytes: u64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub(crate) max_files: u64,
    pub(crate) max_bytes: u64,
    pub(crate) max_entries: u64,
    pub(crate) max_depth: usize,
    pub(crate) max_path_bytes: usize,
    pub(crate) max_total_path_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_files: 100_000,
            max_bytes: 32 * 1024 * 1024 * 1024,
            max_entries: 200_000,
            max_depth: 64,
            max_path_bytes: 32 * 1024,
            max_total_path_bytes: 64 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorldLayout {
    Irw(OsString),
    Anvil,
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

fn no_reparse(meta: &Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return false;
        }
    }
    true
}

pub(crate) fn is_plain_file(meta: &Metadata) -> bool {
    no_reparse(meta) && meta.file_type().is_file()
}

/// Refuse linked or non-directory ancestors before canonicalizing a path.
/// The caller must also keep those ancestors under trusted control; another
/// process could otherwise replace an entry after this check.
pub(crate) fn ensure_no_reparse_ancestors(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty()
        || path.components().any(|component| {
            component == Component::ParentDir || component.as_os_str() == OsStr::new("..")
        })
    {
        return Err(invalid(
            "directory path is empty or contains parent traversal",
        ));
    }
    if !path.is_absolute()
        && (path.has_root()
            || path
                .components()
                .any(|component| matches!(component, Component::Prefix(_))))
    {
        return Err(invalid("directory path has an ambiguous root or drive"));
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut current = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(_) => {
                current.push(component.as_os_str());
                continue;
            }
            Component::ParentDir => {
                return Err(invalid("directory path contains parent traversal"))
            }
            Component::RootDir | Component::CurDir | Component::Normal(_) => {
                current.push(component.as_os_str());
            }
        }
        let metadata = fs::symlink_metadata(&current)?;
        if !no_reparse(&metadata) || !metadata.file_type().is_dir() {
            return Err(invalid(
                "directory path contains a link, reparse point, or non-directory",
            ));
        }
    }
    Ok(())
}

fn check_marker(root: &Path) -> io::Result<()> {
    let path = root.join(MARKER_NAME);
    let metadata = fs::symlink_metadata(&path)?;
    if !no_reparse(&metadata)
        || !metadata.file_type().is_file()
        || metadata.len() != MARKER.len() as u64
    {
        return Err(invalid(
            "backup ownership marker must be an exact regular file",
        ));
    }
    let mut file = File::open(&path)?;
    let opened = file.metadata()?;
    if !opened.file_type().is_file() || opened.len() != MARKER.len() as u64 {
        return Err(invalid("backup ownership marker changed before inspection"));
    }
    let mut bytes = Vec::with_capacity(MARKER.len() + 1);
    (&mut file)
        .take((MARKER.len() + 1) as u64)
        .read_to_end(&mut bytes)?;
    let after = fs::symlink_metadata(&path)?;
    if bytes.as_slice() != MARKER
        || file.metadata()?.len() != MARKER.len() as u64
        || !no_reparse(&after)
        || !after.file_type().is_file()
        || after.len() != MARKER.len() as u64
    {
        return Err(invalid("backup ownership marker changed or is invalid"));
    }
    Ok(())
}

fn determine_layout(snapshot: &Snapshot) -> io::Result<WorldLayout> {
    let mut irw = None;
    let mut anvil = false;
    for (path, entry) in &snapshot.entries {
        if path.components().count() != 1 {
            continue;
        }
        let is_level = path.file_name() == Some(OsStr::new("level.dat"));
        let is_irw = path.extension() == Some(OsStr::new("irw"));
        if !is_level && !is_irw {
            continue;
        }
        if !matches!(entry, Entry::File { .. }) {
            return Err(invalid("world candidate must be a regular file"));
        }
        if is_level {
            anvil = true;
        }
        if is_irw {
            let name = path
                .file_name()
                .ok_or_else(|| invalid("world candidate has no filename"))?;
            if irw.replace(name.to_os_string()).is_some() {
                return Err(invalid("backup contains multiple IRW files"));
            }
        }
    }
    match (anvil, irw) {
        (true, None) => Ok(WorldLayout::Anvil),
        (false, Some(name)) => Ok(WorldLayout::Irw(name)),
        (true, Some(_)) => Err(invalid("backup mixes Anvil and IRW worlds")),
        (false, None) => Err(invalid("backup has no supported world")),
    }
}

fn hash_reader(
    reader: &mut impl Read,
    expected_len: u64,
    remaining: u64,
) -> io::Result<(u64, [u8; 32])> {
    if expected_len > remaining {
        return Err(invalid("backup exceeds byte limit"));
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .ok_or_else(|| invalid("backup byte count overflow"))?;
        if total > expected_len || total > remaining {
            return Err(invalid("file grew or backup exceeds byte limit"));
        }
        hasher.update(&buffer[..read]);
    }
    if total != expected_len {
        return Err(invalid("file size changed during hashing"));
    }
    Ok((total, hasher.finalize().into()))
}

fn hash_file(path: &Path, expected_len: u64, remaining: u64) -> io::Result<(u64, [u8; 32])> {
    let before = fs::symlink_metadata(path)?;
    if !no_reparse(&before) || !before.file_type().is_file() || before.len() != expected_len {
        return Err(invalid("file changed or is not a regular file"));
    }
    let mut file = File::open(path)?;
    let opened = file.metadata()?;
    if !opened.file_type().is_file() || opened.len() != expected_len {
        return Err(invalid("file changed before hashing"));
    }
    let result = hash_reader(&mut file, expected_len, remaining)?;
    if file.metadata()?.len() != expected_len {
        return Err(invalid("file size changed during hashing"));
    }
    let after = fs::symlink_metadata(path)?;
    if !no_reparse(&after) || !after.file_type().is_file() || after.len() != expected_len {
        return Err(invalid("file path changed during hashing"));
    }
    Ok(result)
}

fn walk_tree(
    root: &Path,
    rel: &Path,
    depth: usize,
    limits: &Limits,
    result: &mut Snapshot,
) -> io::Result<()> {
    if depth > limits.max_depth {
        return Err(invalid("backup nesting exceeds depth limit"));
    }
    for entry in fs::read_dir(root.join(rel))? {
        let entry = entry?;
        let child_rel = rel.join(entry.file_name());
        let child_depth = depth
            .checked_add(1)
            .ok_or_else(|| invalid("backup nesting depth overflow"))?;
        if child_depth > limits.max_depth {
            return Err(invalid("backup nesting exceeds depth limit"));
        }
        let path_len = child_rel.as_os_str().as_encoded_bytes().len();
        if path_len > limits.max_path_bytes {
            return Err(invalid("backup relative path exceeds limit"));
        }
        let path_len =
            u64::try_from(path_len).map_err(|_| invalid("backup path length overflow"))?;
        let new_path_bytes = result
            .path_bytes
            .checked_add(path_len)
            .ok_or_else(|| invalid("backup path storage overflow"))?;
        if new_path_bytes > limits.max_total_path_bytes {
            return Err(invalid("backup path storage exceeds limit"));
        }
        let existing_entries = u64::try_from(result.entries.len())
            .map_err(|_| invalid("backup entry count overflow"))?;
        if existing_entries >= limits.max_entries {
            return Err(invalid("backup exceeds entry limit"));
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if !no_reparse(&metadata) {
            return Err(invalid("backup contains a symbolic link or reparse point"));
        }
        let file_type = metadata.file_type();
        if file_type.is_dir() {
            if result
                .entries
                .insert(child_rel.clone(), Entry::Directory)
                .is_some()
            {
                return Err(invalid("backup contains duplicate entry names"));
            }
            result.path_bytes = new_path_bytes;
            walk_tree(root, &child_rel, child_depth, limits, result)?;
        } else if file_type.is_file() {
            let new_files = result
                .files
                .checked_add(1)
                .ok_or_else(|| invalid("backup file count overflow"))?;
            if new_files > limits.max_files {
                return Err(invalid("backup exceeds file count limit"));
            }
            let remaining = limits
                .max_bytes
                .checked_sub(result.bytes)
                .ok_or_else(|| invalid("backup byte accounting overflow"))?;
            let (bytes, sha256) = hash_file(&path, metadata.len(), remaining)?;
            let new_bytes = result
                .bytes
                .checked_add(bytes)
                .ok_or_else(|| invalid("backup byte count overflow"))?;
            if result
                .entries
                .insert(child_rel, Entry::File { bytes, sha256 })
                .is_some()
            {
                return Err(invalid("backup contains duplicate entry names"));
            }
            result.files = new_files;
            result.bytes = new_bytes;
            result.path_bytes = new_path_bytes;
        } else {
            return Err(invalid("backup contains a special file"));
        }
    }
    Ok(())
}

/// Capture file hashes and directory names without creating or altering data.
pub(crate) fn snapshot_tree(root: &Path, limits: &Limits) -> io::Result<Snapshot> {
    ensure_no_reparse_ancestors(root)?;
    let metadata = fs::symlink_metadata(root)?;
    if !no_reparse(&metadata) || !metadata.file_type().is_dir() {
        return Err(invalid("backup root must be a real directory"));
    }
    let mut snapshot = Snapshot::default();
    walk_tree(root, Path::new(""), 0, limits, &mut snapshot)?;
    ensure_no_reparse_ancestors(root)?;
    let metadata_after = fs::symlink_metadata(root)?;
    if !no_reparse(&metadata_after) || !metadata_after.file_type().is_dir() {
        return Err(invalid("backup root changed during inspection"));
    }
    Ok(snapshot)
}

/// Check the ownership marker and select the target for native inspection.
pub(crate) fn inspect_backup(root: &Path, limits: &Limits) -> io::Result<(WorldLayout, Snapshot)> {
    ensure_no_reparse_ancestors(root)?;
    let metadata = fs::symlink_metadata(root)?;
    if !no_reparse(&metadata) || !metadata.file_type().is_dir() {
        return Err(invalid("backup root must be a real directory"));
    }
    check_marker(root)?;
    let snapshot = snapshot_tree(root, limits)?;
    let expected_hash: [u8; 32] = Sha256::digest(MARKER).into();
    if snapshot.entries.get(Path::new(MARKER_NAME))
        != Some(&Entry::File {
            bytes: MARKER.len() as u64,
            sha256: expected_hash,
        })
    {
        return Err(invalid("backup ownership marker changed during inspection"));
    }
    check_marker(root)?;
    let layout = determine_layout(&snapshot)?;
    Ok((layout, snapshot))
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;
