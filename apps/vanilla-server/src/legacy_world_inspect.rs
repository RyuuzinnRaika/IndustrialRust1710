//! Validate a complete 1.7.10 Anvil world without modifying it.
//! Complements the single-file level.dat/region/player validators; the server
//! still uses IRW for its live world and must NOT treat inspection as migration.
use std::fs;
use std::io::{self, ErrorKind};
use std::path::Path;

const MAX_REGIONS: usize = 16_384;
const MAX_PLAYERS: usize = 10_000;
const MAX_TOTAL_REGION_BYTES: u64 = 32 * 1024 * 1024 * 1024;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Inspection {
    pub regions: usize,
    pub chunks: usize,
    pub player_files: usize,
    pub dimensions: usize,
    pub region_bytes: u64,
}

fn checked_directory(path: &Path, required: bool) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_dir() => Ok(true),
        Ok(_) => Err(invalid("Anvil directory is not a real directory")),
        Err(err) if err.kind() == ErrorKind::NotFound && !required => Ok(false),
        Err(err) => Err(err),
    }
}

/// Bounded, read-only recursive inspection of the three 1.7.10 dimensions.
/// Does not create session.lock and does not acquire a write lock.
pub(crate) fn inspect_world(root: &Path) -> io::Result<Inspection> {
    checked_directory(root, true)?;
    let _ = crate::level_dat::read_level_file(&root.join("level.dat"))?;
    let mut result = Inspection {
        regions: 0,
        chunks: 0,
        player_files: 0,
        dimensions: 0,
        region_bytes: 0,
    };
    for dimension in ["", "DIM-1", "DIM1"] {
        let world_dir = if dimension.is_empty() {
            root.to_path_buf()
        } else {
            root.join(dimension)
        };
        if !checked_directory(&world_dir, dimension.is_empty())? {
            continue;
        }
        let region_dir = world_dir.join("region");
        if !checked_directory(&region_dir, false)? {
            continue;
        }
        result.dimensions += 1;
        for entry in fs::read_dir(region_dir)? {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if !metadata.file_type().is_file() {
                return Err(invalid("Anvil region directory has a non-regular entry"));
            }
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| invalid("non-UTF8 Anvil region filename"))?;
            if !name.ends_with(".mca") {
                continue;
            }
            result.regions += 1;
            if result.regions > MAX_REGIONS {
                return Err(invalid("too many Anvil region files"));
            }
            result.region_bytes = result
                .region_bytes
                .checked_add(metadata.len())
                .ok_or_else(|| invalid("region total byte count overflow"))?;
            if result.region_bytes > MAX_TOTAL_REGION_BYTES {
                return Err(invalid("Anvil world exceeds offline inspection byte budget"));
            }
            let chunks = crate::legacy_region::inspect_region(&entry.path())?;
            result.chunks = result
                .chunks
                .checked_add(chunks)
                .ok_or_else(|| invalid("Anvil world chunk count overflow"))?;
        }
    }
    let player_dir = root.join("playerdata");
    if checked_directory(&player_dir, false)? {
        for entry in fs::read_dir(&player_dir)? {
            let entry = entry?;
            let meta = fs::symlink_metadata(entry.path())?;
            if !meta.file_type().is_file() {
                return Err(invalid("playerdata contains a non-regular entry"));
            }
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| invalid("non-UTF8 playerdata filename"))?;
            if !name.ends_with(".dat") {
                continue;
            }
            result.player_files += 1;
            if result.player_files > MAX_PLAYERS {
                return Err(invalid("too many playerdata files"));
            }
            crate::legacy_inventory_nbt::inspect_player(&entry.path())?;
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{anvil_export, world::World};

    fn setup(test: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "ir1710-world-inspection-{test}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let world = root.join("world");
        anvil_export::export(&World::new(), &world).unwrap();
        root
    }

    #[test]
    fn validates_generated_world_without_changing_any_file() {
        let root = setup("normal");
        let world = root.join("world");
        let file = world.join("region/r.0.0.mca");
        let before = fs::read(&file).unwrap();
        let summary = inspect_world(&world).unwrap();
        assert_eq!(summary.regions, 4);
        assert_eq!(summary.chunks, 25);
        assert_eq!(summary.dimensions, 1);
        assert_eq!(summary.player_files, 0);
        assert_eq!(fs::read(&file).unwrap(), before);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn includes_optional_nether_and_end_regions() {
        let root = setup("dimensions");
        let world = root.join("world");
        for dimension in ["DIM-1", "DIM1"] {
            let target = world.join(dimension).join("region");
            fs::create_dir_all(&target).unwrap();
            fs::copy(
                world.join("region/r.0.0.mca"),
                target.join("r.0.0.mca"),
            )
            .unwrap();
        }
        let summary = inspect_world(&world).unwrap();
        assert_eq!(summary.regions, 6);
        assert_eq!(summary.dimensions, 3);
        assert_eq!(summary.chunks, 43); // 25 overworld + 9 each dimension
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refuses_invalid_level_and_corrupted_region_without_mutation() {
        let root = setup("corruption");
        let world = root.join("world");
        let path = world.join("region/r.0.0.mca");
        let mut corrupted = fs::read(&path).unwrap();
        corrupted[4..8].copy_from_slice(&corrupted[0..4]); // overlapping sectors
        fs::write(&path, &corrupted).unwrap();
        assert!(inspect_world(&world).is_err());
        assert_eq!(fs::read(&path).unwrap(), corrupted);
        fs::remove_file(world.join("level.dat")).unwrap();
        assert!(inspect_world(&world).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_world_components_are_rejected() {
        use std::os::unix::fs::symlink;
        let root = setup("symlink");
        let world = root.join("world");
        symlink("/tmp", world.join("DIM-1")).unwrap();
        assert!(inspect_world(&world).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
