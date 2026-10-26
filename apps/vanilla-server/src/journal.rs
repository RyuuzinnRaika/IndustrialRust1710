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
// JSON stores payload bytes as integer arrays (up to four characters per
// byte); keep an explicit on-disk limit for bounded participants.
const MAX_RECORD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TRANSACTION_FILES: usize = 8;
const MAX_TARGET_BYTES: usize = 1024 * 1024;
// Region images are larger than player JSON. A separate bounded cap applies;
// the serialized, committed redo record still has a 64MiB aggregate limit.
const MAX_REGION_TARGET_BYTES: usize = 8 * 1024 * 1024;
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
/// Only canonical region names in their vanilla dimension directory are
/// accepted. No path supplied through a committed record can escape its world.
fn is_anvil_region(relative: &Path) -> bool {
    use std::ffi::OsStr;
    let parts: Vec<_> = relative.iter().collect();
    let region_name = match parts.as_slice() {
        [directory, filename] if *directory == OsStr::new("region") => *filename,
        [dimension, directory, filename]
            if (*dimension == OsStr::new("DIM-1") || *dimension == OsStr::new("DIM1"))
                && *directory == OsStr::new("region") =>
        {
            *filename
        }
        _ => return false,
    };
    let Some(name) = region_name.to_str() else {
        return false;
    };
    let fields: Vec<_> = name.split('.').collect();
    if let ["r", x, z, "mca"] = fields.as_slice() {
        match (x.parse::<i32>(), z.parse::<i32>()) {
            (Ok(rx), Ok(rz)) => name == format!("r.{rx}.{rz}.mca"),
            _ => false,
        }
    } else {
        false
    }
}

fn target_limit(relative: &Path) -> usize {
    if is_anvil_region(relative) {
        MAX_REGION_TARGET_BYTES
    } else {
        MAX_TARGET_BYTES
    }
}

fn validate_target(relative: &Path, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > target_limit(relative) {
        return Err(invalid("journal target too big"));
    }
    if is_anvil_region(relative) {
        crate::legacy_region::inspect_region_bytes(relative, bytes)?;
    }
    Ok(())
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
        || name.ends_with(".books.json")
        || name.ends_with(".decorations.json")
        || name.ends_with(".dispensers.json")
        || name.ends_with(".maps.json")
        || name.ends_with(".minecarts.json")
        || name.ends_with(".hanging.json")
        || name.ends_with(".drops.json")
        || name.ends_with(".irw")
        || is_anvil_region(rel))
    {
        return Err(invalid("journal target type not allowed"));
    }
    // Both fresh commits and crash replay must refuse a symlinked players/
    // directory or target file. Otherwise an apparently safe relative path
    // could escape the trusted world directory during replay.
    let mut path = root.to_path_buf();
    let mut parts = rel.components().peekable();
    while let Some(component) = parts.next() {
        path.push(component.as_os_str());
        match fs::symlink_metadata(&path) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(invalid("journal target follows a symlink"));
                }
                if parts.peek().is_some() && !meta.is_dir() {
                    return Err(invalid("journal target parent is not a directory"));
                }
                if parts.peek().is_none() && !meta.is_file() {
                    return Err(invalid("journal target is not a regular file"));
                }
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
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
    if record.version != VERSION
        || record.files.is_empty()
        || record.files.len() > MAX_TRANSACTION_FILES
    {
        return Err(invalid("invalid journal version or file count"));
    }
    let base = root(journal);
    let mut seen = std::collections::HashSet::new();
    for file in &record.files {
        let checked = relative(base, &base.join(&file.relative))?;
        if !seen.insert(checked) || fingerprint(&file.contents) != file.checksum {
            return Err(invalid("journal checksum, size or duplicate target"));
        }
        validate_target(&file.relative, &file.contents)?;
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
/// Durable bounded commit (one to eight files). An error must stop writes; recovery on
/// restart completes the *new* state for all files (redo journal semantics).
pub fn commit(journal: &Path, changes: &[(&Path, Vec<u8>)]) -> io::Result<()> {
    let _lock = SERIAL
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| invalid("journal lock poisoned"))?;
    if journal.exists() {
        return Err(invalid("unrecovered transaction: restart required"));
    }
    if changes.is_empty() || changes.len() > MAX_TRANSACTION_FILES {
        return Err(invalid("expected 1 to 8 transaction participants"));
    }
    let base = root(journal);
    let files: Vec<Target> = changes
        .iter()
        .map(|(path, bytes)| {
            let checked = relative(base, path)?;
            validate_target(&checked, bytes)?;
            Ok(Target {
                relative: checked,
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
    fn single_minecart_checkpoint_is_durable_and_replayable() {
        let home = std::env::temp_dir().join(format!(
            "ir-journal-one-cart-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&home).unwrap();
        let journal = home.join("flat.pending-txn.json");
        let carts = home.join("flat.minecarts.json");
        let saved = br#"{"version":1,"next":0,"carts":[]}"#;

        // A periodic checkpoint writes one file, not a multi-file transaction.
        commit(&journal, &[(carts.as_path(), saved.to_vec())]).unwrap();
        assert_eq!(fs::read(&carts).unwrap(), saved);
        assert!(!journal.exists());

        // Simulate a crash after publishing the redo journal.
        let newer = br#"{"version":1,"next":1,"carts":[]}"#;
        let record = Record {
            version: VERSION,
            files: vec![Target {
                relative: PathBuf::from("flat.minecarts.json"),
                contents: newer.to_vec(),
                checksum: fingerprint(newer),
            }],
        };
        fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
        recover(&journal).unwrap();
        assert_eq!(fs::read(&carts).unwrap(), newer);
        recover(&journal).unwrap();
        assert!(!journal.exists());

        let unknown = home.join("flat.unknown.json");
        assert!(commit(&journal, &[(unknown.as_path(), saved.to_vec())]).is_err());
        assert!(commit(&journal, &[]).is_err());
        assert!(!journal.exists());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn book_sidecar_and_player_inventory_are_committed_together() {
        let home = std::env::temp_dir().join(format!("ir-journal-books-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(home.join("players")).unwrap();
        let inventory = home.join("players/reader.inventory.json");
        let books = home.join("world.books.json");
        let journal = home.join("world.pending-txn.json");
        commit(
            &journal,
            &[
                (inventory.as_path(), b"new inventory".to_vec()),
                (books.as_path(), b"book NBT records".to_vec()),
            ],
        )
        .unwrap();
        assert_eq!(fs::read(inventory).unwrap(), b"new inventory");
        assert_eq!(fs::read(books).unwrap(), b"book NBT records");
        assert!(!journal.exists());
        fs::remove_dir_all(home).unwrap();
    }
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
    fn four_participant_commit_recovers_all_file_types_in_one_generation() {
        let home = std::env::temp_dir().join(format!(
            "ir-journal-four-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(home.join("players")).unwrap();
        let world = home.join("world.irw");
        let player = home.join("players/worker.inventory.json");
        let chest = home.join("world.chests.json");
        let hopper = home.join("world.hoppers.json");
        let journal = home.join("world.pending-txn.json");
        let targets = [
            (world.as_path(), b"new-world".as_slice()),
            (player.as_path(), b"new-player".as_slice()),
            (chest.as_path(), b"new-chest".as_slice()),
            (hopper.as_path(), b"new-hopper".as_slice()),
        ];
        for (path, _) in &targets {
            fs::write(path, b"old-state").unwrap();
        }
        // Mimic a power cut between the first rename and the remaining three:
        // replay must publish the same generation to all four destinations.
        let record = Record {
            version: VERSION,
            files: targets
                .iter()
                .map(|(path, data)| Target {
                    relative: relative(&home, path).unwrap(),
                    contents: data.to_vec(),
                    checksum: fingerprint(data),
                })
                .collect(),
        };
        fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
        fs::write(&world, b"new-world").unwrap();
        recover(&journal).unwrap();
        for (path, expected) in &targets {
            assert_eq!(fs::read(path).unwrap().as_slice(), *expected);
        }
        assert!(!journal.exists());
        // Exercise the live four-file commit path as well as crash recovery.
        let updates = [
            (world.as_path(), b"updated-world".to_vec()),
            (player.as_path(), b"updated-player".to_vec()),
            (chest.as_path(), b"updated-chest".to_vec()),
            (hopper.as_path(), b"updated-hopper".to_vec()),
        ];
        commit(&journal, &updates).unwrap();
        for (path, data) in &updates {
            assert_eq!(&fs::read(path).unwrap(), data);
        }
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn eight_participants_commit_and_crash_recovery_are_bounded() {
        let home = std::env::temp_dir().join(format!(
            "ir-journal-eight-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&home).unwrap();
        let paths: Vec<_> = (0..8)
            .map(|index| home.join(format!("world-{index}.chests.json")))
            .collect();
        let journal = home.join("world.pending-txn.json");
        let updates: Vec<_> = paths
            .iter()
            .enumerate()
            .map(|(index, path)| (path.as_path(), format!("updated-{index}").into_bytes()))
            .collect();
        commit(&journal, &updates).unwrap();
        for (path, bytes) in &updates {
            assert_eq!(fs::read(path).unwrap(), *bytes);
        }
        let too_many: Vec<_> = (0..9)
            .map(|index| (paths[index % 8].as_path(), b"ignored".to_vec()))
            .collect();
        assert_eq!(
            commit(&journal, &too_many).unwrap_err().kind(),
            ErrorKind::InvalidData
        );
        assert!(!journal.exists());
        let record = Record {
            version: VERSION,
            files: updates
                .iter()
                .map(|(path, bytes)| Target {
                    relative: relative(&home, path).unwrap(),
                    contents: bytes.clone(),
                    checksum: fingerprint(bytes),
                })
                .collect(),
        };
        fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
        fs::write(&paths[0], b"torn-save").unwrap();
        fs::write(&paths[7], b"torn-save").unwrap();
        recover(&journal).unwrap();
        for (path, bytes) in &updates {
            assert_eq!(fs::read(path).unwrap(), *bytes);
        }
        assert!(!journal.exists());
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn corrupt_later_participant_aborts_recovery_without_partial_overwrite() {
        let home = std::env::temp_dir().join(format!(
            "ir-journal-corrupt-late-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&home).unwrap();
        let a = home.join("world.chests.json");
        let b = home.join("world.hoppers.json");
        let journal = home.join("world.pending-txn.json");
        fs::write(&a, b"before-a").unwrap();
        fs::write(&b, b"before-b").unwrap();
        let committed = Record {
            version: VERSION,
            files: vec![
                Target {
                    relative: relative(&home, &a).unwrap(),
                    contents: b"after-a".to_vec(),
                    checksum: fingerprint(b"after-a"),
                },
                Target {
                    relative: relative(&home, &b).unwrap(),
                    contents: b"tampered-after-b".to_vec(),
                    checksum: fingerprint(b"after-b"), // intentionally wrong
                },
            ],
        };
        fs::write(&journal, serde_json::to_vec(&committed).unwrap()).unwrap();
        let error = recover(&journal).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert_eq!(fs::read(&a).unwrap(), b"before-a");
        assert_eq!(fs::read(&b).unwrap(), b"before-b");
        assert!(journal.exists()); // operator intervention; fail closed
        fs::remove_dir_all(home).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_replay_targets_fail_closed_without_external_writes() {
        use std::os::unix::fs::symlink;

        let home = std::env::temp_dir().join(format!("ir-journal-symlink-{}", std::process::id()));
        let outside =
            std::env::temp_dir().join(format!("ir-journal-outside-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&outside);
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let outside_file = outside.join("test.inventory.json");
        fs::write(&outside_file, b"untouched").unwrap();
        symlink(&outside, home.join("players")).unwrap();

        let target = home.join("players/test.inventory.json");
        let chest = home.join("world.chests.json");
        let journal = home.join("world.pending-txn.json");
        assert!(commit(
            &journal,
            &[
                (target.as_path(), b"overwrite".to_vec()),
                (chest.as_path(), b"chest-new".to_vec()),
            ],
        )
        .is_err());
        assert!(!journal.exists());
        assert!(!chest.exists());

        // An attacker may plant a validly checksummed redo record and then
        // change a parent directory into a symlink before server restart.
        let record = Record {
            version: VERSION,
            files: vec![
                Target {
                    relative: PathBuf::from("players/test.inventory.json"),
                    contents: b"overwrite".to_vec(),
                    checksum: fingerprint(b"overwrite"),
                },
                Target {
                    relative: PathBuf::from("world.chests.json"),
                    contents: b"chest-new".to_vec(),
                    checksum: fingerprint(b"chest-new"),
                },
            ],
        };
        fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(recover(&journal).is_err());
        assert!(journal.exists()); // Retain evidence and require operator repair.
        assert!(!chest.exists()); // Check all participants before applying any.
        assert_eq!(fs::read(&outside_file).unwrap(), b"untouched");

        fs::remove_dir_all(home).unwrap();
        fs::remove_dir_all(outside).unwrap();
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
    #[test]
    fn anvil_region_and_chest_sidecar_recover_as_one_commit() {
        use crate::{anvil_export, world::World};
        let home = std::env::temp_dir().join(format!(
            "ir-journal-anvil-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&home).unwrap();
        anvil_export::export(&World::new(), &home.join("world")).unwrap();
        let root = home.join("world");
        let region = root.join("region/r.0.0.mca");
        let sidecar = root.join("flat.chests.json");
        let journal = root.join("flat.pending-txn.json");
        let region_bytes = fs::read(&region).unwrap();
        let original = b"old-chest";
        fs::write(&sidecar, original).unwrap();
        let updated = br#"{"chests":[]}"#;
        // Model power loss after durable log publication and the first rename.
        let record = Record {
            version: VERSION,
            files: vec![
                Target {
                    relative: relative(&root, &region).unwrap(),
                    contents: region_bytes.clone(),
                    checksum: fingerprint(&region_bytes),
                },
                Target {
                    relative: relative(&root, &sidecar).unwrap(),
                    contents: updated.to_vec(),
                    checksum: fingerprint(updated),
                },
            ],
        };
        fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
        fs::write(&sidecar, b"partially-updated").unwrap();
        recover(&journal).unwrap();
        assert_eq!(fs::read(&region).unwrap(), region_bytes);
        assert_eq!(fs::read(&sidecar).unwrap(), updated);
        assert!(!journal.exists());
        recover(&journal).unwrap(); // Idempotent after replay.
        fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn crash_between_two_regions_and_sidecar_replays_a_single_generation() {
        use crate::{anvil_export, world::World};
        let parent = std::env::temp_dir().join(format!(
            "ir-journal-multi-anvil-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&parent);
        fs::create_dir_all(&parent).unwrap();
        let root = parent.join("world");
        anvil_export::export(&World::new(), &root).unwrap();
        let east = root.join("region/r.0.0.mca");
        let west = root.join("region/r.-1.0.mca");
        let sidecar = root.join("flat.chests.json");
        let journal = root.join("flat.pending-txn.json");
        let east_bytes = fs::read(&east).unwrap();
        let west_bytes = fs::read(&west).unwrap();
        let after_chests = br#"{"chests":[]}"#;
        let record = Record {
            version: VERSION,
            files: [
                (east.as_path(), east_bytes.as_slice()),
                (west.as_path(), west_bytes.as_slice()),
                (sidecar.as_path(), after_chests.as_slice()),
            ]
            .into_iter()
            .map(|(path, data)| Target {
                relative: relative(&root, path).unwrap(),
                checksum: fingerprint(data),
                contents: data.to_vec(),
            })
            .collect(),
        };
        fs::write(&journal, serde_json::to_vec(&record).unwrap()).unwrap();
        // A power loss after the first region publication can leave the
        // remaining targets inconsistent until startup redo completes.
        fs::write(&west, b"power-loss-torn-region").unwrap();
        fs::write(&sidecar, b"old").unwrap();
        recover(&journal).unwrap();
        assert_eq!(fs::read(&east).unwrap(), east_bytes);
        assert_eq!(fs::read(&west).unwrap(), west_bytes);
        assert_eq!(fs::read(&sidecar).unwrap(), after_chests);
        assert!(!journal.exists());
        recover(&journal).unwrap();
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn anvil_journal_rejects_invalid_region_and_unsafe_region_path() {
        let root = std::env::temp_dir().join(format!(
            "ir-journal-reject-region-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("region")).unwrap();
        fs::create_dir_all(root.join("untrusted")).unwrap();
        let sidecar = root.join("flat.chests.json");
        let journal = root.join("flat.pending-txn.json");
        for path in [
            root.join("untrusted/r.0.0.mca"),
            root.join("region/r.01.0.mca"),
            root.join("region/r.0.0.mca"),
        ] {
            assert!(commit(
                &journal,
                &[
                    (path.as_path(), b"invalid region".to_vec()),
                    (sidecar.as_path(), b"{}".to_vec())
                ]
            )
            .is_err());
            assert!(!journal.exists());
        }
        fs::remove_dir_all(&root).unwrap();
    }
}
