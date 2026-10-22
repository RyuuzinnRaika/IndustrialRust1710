//! Minecraft Java Edition 1.7.10 level.dat: bounded validation and
//! create-only offline serialization. Live write-back is NOT yet enabled.
use fastnbt::Value;
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, ErrorKind, Read, Write};
use std::path::Path;

const MAX_COMPRESSED_BYTES: u64 = 1024 * 1024;
const MAX_DECOMPRESSED_BYTES: usize = 4 * 1024 * 1024;
const ANVIL_1710_VERSION: i32 = 19133;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct LevelRoot {
    #[serde(rename = "Data")]
    pub data: LevelData,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct LevelData {
    #[serde(rename = "version")]
    pub format_version: i32,
    #[serde(rename = "LevelName")]
    pub name: String,
    #[serde(rename = "RandomSeed")]
    pub seed: i64,
    #[serde(rename = "SpawnX")]
    pub spawn_x: i32,
    #[serde(rename = "SpawnY")]
    pub spawn_y: i32,
    #[serde(rename = "SpawnZ")]
    pub spawn_z: i32,
    #[serde(rename = "Time")]
    pub time: i64,
    #[serde(rename = "DayTime", default, skip_serializing_if = "Option::is_none")]
    pub day_time: Option<i64>,
    #[serde(rename = "GameType", default, skip_serializing_if = "Option::is_none")]
    pub game_type: Option<i32>,
    #[serde(
        rename = "generatorName",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub generator: Option<String>,
    #[serde(rename = "raining", default, skip_serializing_if = "Option::is_none")]
    pub raining: Option<i8>,
    #[serde(
        rename = "thundering",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub thundering: Option<i8>,
    #[serde(rename = "rainTime", default, skip_serializing_if = "Option::is_none")]
    pub rain_time: Option<i32>,
    #[serde(
        rename = "thunderTime",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub thunder_time: Option<i32>,
    #[serde(rename = "GameRules", default, skip_serializing_if = "Option::is_none")]
    pub game_rules: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl LevelRoot {
    fn validate(&self) -> io::Result<()> {
        if self.data.format_version != ANVIL_1710_VERSION {
            return Err(invalid("level.dat is not Anvil version 19133"));
        }
        if self.data.name.is_empty()
            || self.data.name.len() > 255
            || self.data.spawn_x.unsigned_abs() > 30_000_000
            || self.data.spawn_z.unsigned_abs() > 30_000_000
            || !(0..256).contains(&self.data.spawn_y)
            || self.data.game_type.is_some_and(|x| !(0..=2).contains(&x))
            || self.data.raining.is_some_and(|x| x != 0 && x != 1)
            || self.data.thundering.is_some_and(|x| x != 0 && x != 1)
        {
            return Err(invalid("invalid 1.7.10 Anvil level.dat state"));
        }
        Ok(())
    }
}

/// Decode an uncompressed NBT payload. This function is intentionally read-only.
pub(crate) fn parse_level(bytes: &[u8]) -> io::Result<LevelRoot> {
    if bytes.len() > MAX_DECOMPRESSED_BYTES {
        return Err(invalid("oversized level.dat NBT"));
    }
    let root: LevelRoot =
        fastnbt::from_bytes(bytes).map_err(|_| invalid("invalid level.dat NBT"))?;
    root.validate()?;
    Ok(root)
}

/// Create a new, validated 1.7.10 level.dat without overwriting a world.
/// A full live-world save / level.dat_old rotation requires FS03/FS08 and
/// must not call this staging primitive on an occupied world.
// Staging API: intentionally not connected to live-world saves before FS08.
#[allow(dead_code)]
pub(crate) fn create_level_file(path: &Path, root: &LevelRoot) -> io::Result<()> {
    root.validate()?;
    let nbt = fastnbt::to_bytes(root).map_err(|_| invalid("cannot encode 1.7.10 level NBT"))?;
    if nbt.len() > MAX_DECOMPRESSED_BYTES {
        return Err(invalid("level NBT exceeds size limit"));
    }
    let mut compressor = GzEncoder::new(Vec::new(), Compression::default());
    compressor.write_all(&nbt)?;
    let compressed = compressor.finish()?;
    if compressed.len() as u64 > MAX_COMPRESSED_BYTES {
        return Err(invalid("compressed level NBT exceeds size limit"));
    }
    let parent = path
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !fs::symlink_metadata(parent)?.file_type().is_dir() {
        return Err(invalid(
            "level.dat destination parent is not a real directory",
        ));
    }
    match fs::symlink_metadata(path) {
        Ok(_) => {
            return Err(io::Error::new(
                ErrorKind::AlreadyExists,
                "level.dat already exists: create-only writer refuses overwrite",
            ))
        }
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    let staged = path.with_extension("creating");
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged)?;
    let written = output
        .write_all(&compressed)
        .and_then(|_| output.sync_all());
    drop(output);
    if let Err(error) = written {
        let _ = fs::remove_file(&staged);
        return Err(error);
    }
    // Hard-link publishing is atomic and create-only; another process that
    // created level.dat after the existence check wins without replacement.
    let published = fs::hard_link(&staged, path);
    let _ = fs::remove_file(&staged);
    published?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

/// Explicit OFFLINE recovery for a missing level.dat, preserving the original
/// compressed NBT bytes from level.dat_old. Never repairs an existing level.dat.
/// The caller must hold the world session lock; this cannot protect against
/// a non-cooperating Java server, so only run it on a stopped world.
pub(crate) fn restore_missing_level(
    world_dir: &Path,
    lock: &mut crate::session_lock::WorldSessionLock,
) -> io::Result<()> {
    if !fs::symlink_metadata(world_dir)?.file_type().is_dir() {
        return Err(invalid("recovery requires a real world directory"));
    }
    let current = world_dir.join("level.dat");
    match fs::symlink_metadata(&current) {
        Ok(_) => {
            return Err(io::Error::new(
                ErrorKind::AlreadyExists,
                "level.dat exists; refusing any automatic replacement",
            ));
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let previous = world_dir.join("level.dat_old");
    // First validate the source according to the full 1.7.10 NBT parser.
    let _ = read_level_file(&previous)?;
    let compressed = fs::read(&previous)?;
    if compressed.len() as u64 > MAX_COMPRESSED_BYTES {
        return Err(invalid("recovery source exceeds compressed NBT limit"));
    }
    lock.verify()?;
    let staging = world_dir.join("level.dat.recovering");
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staging)?;
    let result = (|| {
        output.write_all(&compressed)?;
        output.sync_all()?;
        drop(output);
        // Check the exact bytes we intend to publish, without converting
        // unknown NBT fields to a different serialization.
        let _ = read_level_file(&staging)?;
        lock.verify()?;
        fs::hard_link(&staging, &current)?;
        #[cfg(unix)]
        fs::File::open(world_dir)?.sync_all()?;
        Ok(())
    })();
    let _ = fs::remove_file(&staging);
    result
}

/// Return the checked level.dat, including all unknown NBT fields in memory.
pub(crate) fn read_level_file(path: &Path) -> io::Result<LevelRoot> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_COMPRESSED_BYTES {
        return Err(invalid("invalid level.dat input type or compressed size"));
    }
    let mut uncompressed = Vec::new();
    GzDecoder::new(fs::File::open(path)?)
        .take((MAX_DECOMPRESSED_BYTES + 1) as u64)
        .read_to_end(&mut uncompressed)?;
    parse_level(&uncompressed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::GzEncoder, Compression};
    use std::io::Write;

    fn sample() -> LevelRoot {
        let mut extra = BTreeMap::new();
        extra.insert("CustomWorldTag".into(), Value::String("unchanged".into()));
        LevelRoot {
            data: LevelData {
                format_version: ANVIL_1710_VERSION,
                name: "1.7.10 test world".into(),
                seed: -123,
                spawn_x: -16,
                spawn_y: 64,
                spawn_z: 32,
                time: 24000,
                day_time: Some(6000),
                game_type: Some(0),
                generator: Some("default".into()),
                raining: Some(0),
                thundering: Some(0),
                rain_time: Some(1200),
                thunder_time: Some(2400),
                game_rules: None,
                extra: BTreeMap::new(),
            },
            extra,
        }
    }

    #[test]
    fn anvil_19133_level_roundtrip_preserves_unknown_nbt() {
        let bytes = fastnbt::to_bytes(&sample()).unwrap();
        let parsed = parse_level(&bytes).unwrap();
        assert_eq!(parsed.data.seed, -123);
        assert_eq!(parsed.data.day_time, Some(6000));
        let second = parse_level(&fastnbt::to_bytes(&parsed).unwrap()).unwrap();
        assert!(
            matches!(second.extra.get("CustomWorldTag"), Some(Value::String(value)) if value == "unchanged")
        );

        let path = std::env::temp_dir().join(format!(
            "ir1710-level-read-{}-{:?}.dat",
            std::process::id(),
            std::thread::current().id()
        ));
        let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
        gzip.write_all(&bytes).unwrap();
        fs::write(&path, gzip.finish().unwrap()).unwrap();
        assert_eq!(read_level_file(&path).unwrap().data.spawn_x, -16);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn create_only_level_dat_preserves_unknown_tags_and_never_overwrites() {
        let folder = std::env::temp_dir().join(format!(
            "ir1710-level-create-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir(&folder).unwrap();
        let path = folder.join("level.dat");
        let mut initial = sample();
        initial.data.time = 90_000;
        create_level_file(&path, &initial).unwrap();
        let loaded = read_level_file(&path).unwrap();
        assert_eq!(loaded.data.time, 90_000);
        assert!(matches!(
            loaded.extra.get("CustomWorldTag"),
            Some(Value::String(tag)) if tag == "unchanged"
        ));
        let original = fs::read(&path).unwrap();
        assert!(create_level_file(&path, &initial).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(!path.with_extension("creating").exists());
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn create_only_rejects_corrupt_metadata_without_side_effects() {
        let folder = std::env::temp_dir().join(format!(
            "ir1710-level-invalid-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir(&folder).unwrap();
        let path = folder.join("level.dat");
        let mut invalid_root = sample();
        invalid_root.data.spawn_y = 256;
        assert!(create_level_file(&path, &invalid_root).is_err());
        assert!(!path.exists());
        assert!(!path.with_extension("creating").exists());
        fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn restores_missing_level_from_old_without_changing_gzip_bytes() {
        let world = std::env::temp_dir().join(format!(
            "ir1710-level-recovery-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&world);
        fs::create_dir(&world).unwrap();
        let previous = world.join("level.dat_old");
        let mut compressor = GzEncoder::new(Vec::new(), Compression::default());
        compressor
            .write_all(&fastnbt::to_bytes(&sample()).unwrap())
            .unwrap();
        let compressed = compressor.finish().unwrap();
        fs::write(&previous, &compressed).unwrap();
        let mut lock = crate::session_lock::WorldSessionLock::acquire(&world).unwrap();
        restore_missing_level(&world, &mut lock).unwrap();
        assert_eq!(fs::read(world.join("level.dat")).unwrap(), compressed);
        assert_eq!(fs::read(&previous).unwrap(), compressed);
        assert!(restore_missing_level(&world, &mut lock).is_err());
        drop(lock);
        fs::remove_dir_all(world).unwrap();
    }

    #[test]
    fn refuses_corrupt_old_level_and_never_creates_target() {
        let world = std::env::temp_dir().join(format!(
            "ir1710-level-recovery-invalid-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&world);
        fs::create_dir(&world).unwrap();
        fs::write(world.join("level.dat_old"), b"not gzip or NBT").unwrap();
        let mut lock = crate::session_lock::WorldSessionLock::acquire(&world).unwrap();
        assert!(restore_missing_level(&world, &mut lock).is_err());
        assert!(!world.join("level.dat").exists());
        assert!(!world.join("level.dat.recovering").exists());
        drop(lock);
        fs::remove_dir_all(world).unwrap();
    }

    #[test]
    fn wrong_version_invalid_spawn_and_corrupt_nbt_fail_closed() {
        let mut wrong = sample();
        wrong.data.format_version = 19132;
        assert!(parse_level(&fastnbt::to_bytes(&wrong).unwrap()).is_err());
        let mut wrong = sample();
        wrong.data.spawn_y = 300;
        assert!(parse_level(&fastnbt::to_bytes(&wrong).unwrap()).is_err());
        assert!(parse_level(b"not NBT").is_err());
        assert!(parse_level(&vec![0u8; MAX_DECOMPRESSED_BYTES + 1]).is_err());
    }
}
