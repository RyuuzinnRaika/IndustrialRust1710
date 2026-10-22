//! J93: offline, read-only integrity smoke check. Never acquires a write
//! session lock or invokes transaction recovery. This is NOT full semantic
//! cross-reference validation of all Minecraft 1.7.10 entities and tiles.
use crate::{clock::Clock, legacy_world_inspect, world::World};
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

const MAX_JSON_BYTES: u64 = 16 * 1024 * 1024;
const MAX_JSON_FILES: usize = 10_000;

#[derive(Debug, PartialEq, Eq)]
pub struct Report {
    pub format: &'static str,
    pub files: usize,
    pub regions: usize,
    pub chunks: usize,
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

fn regular_file(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_file() => Ok(true),
        Ok(_) => Err(invalid("world contains a non-regular or linked file")),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

fn check_json(directory: &Path, count: &mut usize) -> io::Result<()> {
    for item in fs::read_dir(directory)? {
        let item = item?;
        let path = item.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(invalid("world verification refuses symbolic links"));
        }
        if metadata.file_type().is_dir() {
            continue;
        }
        if !metadata.file_type().is_file() {
            return Err(invalid("world contains a special file"));
        }
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        *count += 1;
        if *count > MAX_JSON_FILES || metadata.len() > MAX_JSON_BYTES {
            return Err(invalid("world JSON verification budget exceeded"));
        }
        let bytes = fs::read(&path)?;
        if bytes.len() as u64 > MAX_JSON_BYTES {
            return Err(invalid("JSON file grew during verification"));
        }
        let _: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|_| invalid("corrupt world JSON file"))?;
    }
    Ok(())
}

/// Inspects a legacy Anvil directory or IRW file/world directory without
/// creating files, replaying journals, or opening any TCP listener.
pub fn verify(input: &Path) -> io::Result<Report> {
    let input_meta = fs::symlink_metadata(input)?;
    let (dir, irw): (PathBuf, Option<PathBuf>) = if input_meta.file_type().is_file()
        && input.extension().is_some_and(|ext| ext == "irw")
    {
        (input.parent().unwrap_or_else(|| Path::new(".")).to_path_buf(), Some(input.to_path_buf()))
    } else if input_meta.file_type().is_dir() {
        (input.to_path_buf(), None)
    } else {
        return Err(invalid("world input must be a real directory or .irw file"));
    };
    if !fs::symlink_metadata(&dir)?.file_type().is_dir() {
        return Err(invalid("world parent must be a real directory"));
    }
    let has_level = regular_file(&dir.join("level.dat"))?;
    let has_region = match fs::symlink_metadata(dir.join("region")) {
        Ok(meta) if meta.file_type().is_dir() => true,
        Ok(_) => return Err(invalid("region must be a real directory")),
        Err(err) if err.kind() == ErrorKind::NotFound => false,
        Err(err) => return Err(err),
    };
    if has_level || has_region {
        if irw.is_some() || regular_file(&dir.join("flat.irw"))? {
            return Err(invalid("mixed IRW/Anvil storage is unsafe"));
        }
        let summary = legacy_world_inspect::inspect_world(&dir)?;
        return Ok(Report {
            format: "Anvil 1.7.10",
            files: summary.regions + summary.player_files + 1,
            regions: summary.regions,
            chunks: summary.chunks,
        });
    }
    let irw = irw.unwrap_or_else(|| dir.join("flat.irw"));
    if !regular_file(&irw)? {
        return Err(invalid("IRW world file is missing"));
    }
    let _ = World::load(&irw)?;
    let mut files = 1;
    let clock_file = irw.with_extension("clock");
    if regular_file(&clock_file)? {
        let _ = Clock::load_state(&clock_file)?;
        files += 1;
    }
    // A pending journal must be recovered by the regular controlled start,
    // never by this read-only inspection operation.
    for pending in [irw.with_extension("pending-txn.json"),
        irw.with_extension("chests.pending-txn.json"),
        irw.with_extension("furnaces.pending-txn.json")]
    {
        if regular_file(&pending)? {
            return Err(invalid("unrecovered world transaction is present"));
        }
    }
    check_json(&dir, &mut files)?;
    let players = dir.join("players");
    match fs::symlink_metadata(&players) {
        Ok(meta) if meta.file_type().is_dir() => check_json(&players, &mut files)?,
        Ok(_) => return Err(invalid("players must be a real directory")),
        Err(err) if err.kind() == ErrorKind::NotFound => {},
        Err(err) => return Err(err),
    }
    Ok(Report { format: "IRW prototype", files, regions: 0, chunks: 0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{anvil_export, world::World};

    #[test]
    fn checks_irw_without_modifying_it_and_detects_corruption() {
        let root = std::env::temp_dir().join(format!("ir1710-j93-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let world = root.join("flat.irw");
        World::new().save_new(&world).unwrap();
        let original = fs::read(&world).unwrap();
        assert_eq!(verify(&root).unwrap().format, "IRW prototype");
        assert_eq!(fs::read(&world).unwrap(), original);
        assert!(!root.join("session.lock").exists());
        fs::write(world.with_extension("clock"), b"broken").unwrap();
        assert!(verify(&root).is_err());
        assert_eq!(fs::read(&world).unwrap(), original);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn uses_existing_anvil_inspector_and_refuses_mixed_world() {
        let root = std::env::temp_dir().join(format!("ir1710-j93-anvil-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let dest = root.join("anvil");
        anvil_export::export(&World::new(), &dest).unwrap();
        assert_eq!(verify(&dest).unwrap().chunks, 25);
        fs::write(dest.join("flat.irw"), b"IRW1").unwrap();
        assert!(verify(&dest).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
