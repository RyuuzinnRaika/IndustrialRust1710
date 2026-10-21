//! Crash-recoverable, bounded transaction log for related server save files.
//! A committed journal is replayed in full before any file is opened on startup.
//! A failure during application leaves the journal in place and locks further
//! journal writes until the operator restarts and recovery succeeds.
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const VERSION: u8 = 1;
const MAX_RECORD_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TARGET_BYTES: usize = 1024 * 1024;
static SERIAL: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    relative: PathBuf,
    contents: Vec<u8>,
    checksum: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u8,
    files: Vec<Target>,
}
fn invalid(detail: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, detail)
}
fn fingerprint(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}
fn relative(root: &Path, target: &Path) -> io::Result<PathBuf> {
    let rel = target
        .strip_prefix(root)
        .map_err(|_| invalid("journal target outside world"))?;
    if rel.components().count() == 0
        || rel
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(invalid("journal contains unsafe path"));
    }
    // Only expected, bounded native server state can be replayed.
    let name = rel
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| invalid("file name"))?;
    if !(name.ends_with(".inventory.json")
        || name.ends_with(".chests.json")
        || name.ends_with(".furnaces.json")
        || name.ends_with(".hoppers.json")
        || name.ends_with(".brewers.json")
        || name.ends_with(".drops.json")
        || name.ends_with(".irw"))
    {
        return Err(invalid("journal target type not allowed"));
    }
    Ok(rel.to_path_buf())
}
fn root(journal: &Path) -> &Path {
    journal
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}
fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("txn.tmp");
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    if let Err(err) = output.write_all(bytes).and_then(|_| output.sync_all()) {
        let _ = fs::remove_file(&temp);
        return Err(err);
    }
    drop(output);
    fs::rename(&temp, path)?;
    Ok(())
}
fn sync_directory(dir: &Path) -> io::Result<()> {
    // Directory fsync is not supported uniformly on Windows.
    #[cfg(unix)]
    {
        File::open(dir)?.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
    Ok(())
}
fn apply(journal: &Path, record: &Record) -> io::Result<()> {
    if record.version != VERSION || record.files.is_empty() || record.files.len() > 3 {
        return Err(invalid("invalid journal version or file count"));
    }
    let base = root(journal);
    let mut seen = std::collections::HashSet::new();
    for file in &record.files {
        let checked = relative(base, &base.join(&file.relative))?;
        if !seen.insert(checked)
            || file.contents.len() > MAX_TARGET_BYTES
            || fingerprint(&file.contents) != file.checksum
        {
            return Err(invalid("journal checksum, size or duplicate target"));
        }
    }
    for file in &record.files {
        let destination = base.join(&file.relative);
        // Recovery of an interrupted rename can leave the temporary file.
        let temp = destination.with_extension("txn.tmp");
        if temp.exists() {
            fs::remove_file(&temp)?;
        }
        atomic_write(&destination, &file.contents)?;
        // A file rename in a nested players/ directory is only crash-durable
        // after that directory is synced as well. Do not remove the redo
        // journal while any target parent has an unsynced rename.
        if let Some(parent) = destination.parent() {
            sync_directory(parent)?;
        }
    }
    sync_directory(base)?;
    fs::remove_file(journal)?;
    sync_directory(base)
}
/// Replay an entire prepared transaction before world/player/container loads.
/// Corrupt journals fail closed; no partial or speculative salvage is attempted.
pub fn recover(journal: &Path) -> io::Result<()> {
    let _lock = SERIAL
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| invalid("journal lock poisoned"))?;
    let meta = match fs::symlink_metadata(journal) {
        Ok(meta) => meta,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if !meta.file_type().is_file() || meta.len() > MAX_RECORD_BYTES {
        return Err(invalid("journal file invalid or oversized"));
    }
    let bytes = fs::read(journal)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(invalid("journal oversized"));
    }
    let record: Record =
        serde_json::from_slice(&bytes).map_err(|_| invalid("corrupt transaction journal"))?;
    apply(journal, &record)
}
/// Durable two-file commit. An error must stop further writes; recovery on
/// restart completes the *new* state for all files (redo journal semantics).
pub fn commit(journal: &Path, changes: &[(&Path, Vec<u8>)]) -> io::Result<()> {
    let _lock = SERIAL
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| invalid("journal lock poisoned"))?;
    if journal.exists() {
        return Err(invalid("unrecovered transaction: restart required"));
    }
    if changes.len() < 2 || changes.len() > 3 {
        return Err(invalid("expected multi-file transaction"));
    }
    let base = root(journal);
    let files: Vec<Target> = changes
        .iter()
        .map(|(path, bytes)| {
            if bytes.len() > MAX_TARGET_BYTES {
                return Err(invalid("journal target too big"));
            }
            Ok(Target {
                relative: relative(base, path)?,
                contents: bytes.clone(),
                checksum: fingerprint(bytes),
            })
        })
        .collect::<io::Result<_>>()?;
    // Reject duplicates *before* publishing the durable redo record.
    // Otherwise a bad request would strand a journal and prevent startup.
    let mut seen = std::collections::HashSet::new();
    if files.iter().any(|file| !seen.insert(&file.relative)) {
        return Err(invalid("duplicate journal target"));
    }
    let record = Record {
        version: VERSION,
        files,
    };
    let serialized = serde_json::to_vec(&record).map_err(|_| invalid("serialize journal"))?;
    if serialized.len() as u64 > MAX_RECORD_BYTES {
        return Err(invalid("journal too big"));
    }
    if let Some(parent) = journal.parent() {
        fs::create_dir_all(parent)?;
    }
    // Atomic preparation: readers must never observe an incomplete journal.
    let prepare = journal.with_extension("journal.tmp");
    if prepare.exists() {
        fs::remove_file(&prepare)?;
    }
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&prepare)?;
    out.write_all(&serialized)?;
    out.sync_all()?;
    drop(out);
    fs::rename(&prepare, journal)?;
    sync_directory(base)?;
    apply(journal, &record)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interrupted_two_file_commit_replays_on_boot() {
        let home = std::env::temp_dir().join(format!("ir-journal-{}-{}", std::process::id(), 42));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(home.join("players")).unwrap();
        let a = home.join("players/alex.inventory.json");
        let b = home.join("world.chests.json");
        let journal = home.join("world.pending-txn.json");
        fs::write(&a, b"before").unwrap();
        fs::write(&b, b"before").unwrap();
        // Simulate a crash after committing a journal and only one target.
        let r = Record {
            version: VERSION,
            files: vec![
                Target {
                    relative: relative(&home, &a).unwrap(),
                    contents: b"after-a".to_vec(),
                    checksum: fingerprint(b"after-a"),
                },
                Target {
                    relative: relative(&home, &b).unwrap(),
                    contents: b"after-b".to_vec(),
                    checksum: fingerprint(b"after-b"),
                },
            ],
        };
        fs::write(&journal, serde_json::to_vec(&r).unwrap()).unwrap();
        fs::write(&a, b"after-a").unwrap();
        recover(&journal).unwrap();
        assert_eq!(fs::read(&a).unwrap(), b"after-a");
        assert_eq!(fs::read(&b).unwrap(), b"after-b");
        assert!(!journal.exists());
        fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn duplicate_targets_cannot_strand_a_committed_journal() {
        let home =
            std::env::temp_dir().join(format!("ir-journal-duplicate-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(home.join("players")).unwrap();
        let journal = home.join("world.pending-txn.json");
        let target = home.join("players/test.inventory.json");
        let error = commit(
            &journal,
            &[
                (target.as_path(), b"first".to_vec()),
                (target.as_path(), b"second".to_vec()),
            ],
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert!(!journal.exists());
        assert!(!target.exists());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn three_participant_crash_recovery_replays_all_and_is_idempotent() {
        let home = std::env::temp_dir().join(format!("ir-journal-three-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(home.join("players")).unwrap();
        let a = home.join("players/test.inventory.json");
        let b = home.join("world.chests.json");
        let c = home.join("world.hoppers.json");
        let journal = home.join("world.pending-txn.json");
        let names = [&a, &b, &c];
        let record = Record {
            version: VERSION,
            files: names
                .iter()
                .enumerate()
                .map(|(i, path)| {
                    let contents = format!("version-2-{i}").into_bytes();
                    Target {
                        relative: relative(&home, path).unwrap(),
                        checksum: fingerprint(&contents),
                        contents,
                    }
                })
                .collect(),
        };
        for name in names {
            fs::write(name, b"version-1").unwrap();
        }
        fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
        fs::write(&a, b"version-2-0").unwrap(); // crash after first rename
        recover(&journal).unwrap();
        for (i, name) in [&a, &b, &c].iter().enumerate() {
            assert_eq!(fs::read(name).unwrap(), format!("version-2-{i}").as_bytes());
        }
        recover(&journal).unwrap(); // no further changes
        assert!(!journal.exists());
        fs::remove_dir_all(home).unwrap();
    }
    #[test]
    fn world_inventory_chest_crash_replay_and_parent_fsync() {
        let home = std::env::temp_dir().join(format!("ir-journal-world-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(home.join("players")).unwrap();
        let world = home.join("flat.irw");
        let inventory = home.join("players/alex.inventory.json");
        let chest = home.join("flat.chests.json");
        let journal = home.join("flat.pending-txn.json");
        for path in [&world, &inventory, &chest] {
            fs::write(path, b"old").unwrap();
        }
        // Model a power loss after the commit record and first file rename.
        // The redo log must restore all three targets before normal loading.
        let contents = [
            (world.as_path(), b"IRW1-new".as_slice()),
            (inventory.as_path(), b"inventory-new".as_slice()),
            (chest.as_path(), b"chest-new".as_slice()),
        ];
        let files = contents
            .iter()
            .map(|(path, data)| Target {
                relative: relative(&home, path).unwrap(),
                contents: data.to_vec(),
                checksum: fingerprint(data),
            })
            .collect();
        fs::write(
            &journal,
            serde_json::to_vec(&Record {
                version: VERSION,
                files,
            })
            .unwrap(),
        )
        .unwrap();
        fs::write(&world, contents[0].1).unwrap();
        recover(&journal).unwrap();
        for (path, data) in contents {
            assert_eq!(fs::read(path).unwrap(), data);
        }
        recover(&journal).unwrap();
        assert!(!journal.exists());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn corrupt_journal_fails_closed() {
        let home = std::env::temp_dir().join(format!("ir-journal-corrupt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&home).unwrap();
        let journal = home.join("world.pending-txn.json");
        fs::write(&journal, b"{not valid").unwrap();
        assert!(recover(&journal).is_err());
        assert!(journal.exists());
        fs::remove_dir_all(home).unwrap();
    }
}
