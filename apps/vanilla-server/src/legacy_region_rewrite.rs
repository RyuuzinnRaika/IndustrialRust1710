//! FS08 foundation: copy-on-write rewrite of ONE EXISTING Anvil chunk into
//! a brand-new region file. This is deliberately OFFLINE and create-only.
//! Never modifies the source, replaces a destination, or activates live saves.
use crate::dimensions::Dimension;
use crate::session_lock::WorldSessionLock;
use fastanvil::Region;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Seek};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

fn wrap(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

/// Copy every source chunk's uncompressed NBT while replacing exactly one
/// existing chunk. Compression may change; unknown NBT tags are not parsed
/// away from untouched chunks. Neither source nor target can be overwritten.
pub(crate) fn rewrite_chunk_create_only(
    source: &Path,
    destination: &Path,
    cx: i32,
    cz: i32,
    replacement_nbt: &[u8],
) -> io::Result<()> {
    let file_name = format!("r.{}.{}.mca", cx.div_euclid(32), cz.div_euclid(32));
    if source.file_name().and_then(|name| name.to_str()) != Some(file_name.as_str())
        || destination.file_name().and_then(|name| name.to_str()) != Some(file_name.as_str())
    {
        return Err(invalid(
            "Anvil region name does not match replacement chunk",
        ));
    }
    // The existing read-only inspector validates ALL sector allocations and
    // chunk NBT, not just the modified chunk.
    crate::legacy_region::inspect_region(source)?;
    crate::legacy_nbt::parse_chunk(replacement_nbt, cx, cz)?;
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    if !fs::symlink_metadata(parent)?.file_type().is_dir() {
        return Err(invalid("destination parent is not a directory"));
    }
    match fs::symlink_metadata(destination) {
        Ok(_) => {
            return Err(io::Error::new(
                ErrorKind::AlreadyExists,
                "destination Anvil region already exists",
            ))
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let local_x = cx.rem_euclid(32) as usize;
    let local_z = cz.rem_euclid(32) as usize;
    let mut input = Region::from_stream(File::open(source)?).map_err(wrap)?;
    if input.read_chunk(local_x, local_z).map_err(wrap)?.is_none() {
        return Err(invalid(
            "source chunk does not exist; new generation is separate",
        ));
    }
    let staging = destination.with_extension("creating");
    let stage_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&staging)?;
    let written = (|| -> io::Result<()> {
        let mut output = Region::create(stage_file).map_err(wrap)?;
        for z in 0..32 {
            for x in 0..32 {
                if let Some(bytes) = input.read_chunk(x, z).map_err(wrap)? {
                    let data = if (x, z) == (local_x, local_z) {
                        replacement_nbt
                    } else {
                        &bytes
                    };
                    output.write_chunk(x, z, data).map_err(wrap)?;
                }
            }
        }
        let mut file = output.into_inner().map_err(wrap)?;
        let length = file.stream_position()?;
        file.set_len(length)?;
        file.sync_all()?;
        drop(file);
        // Verify all emitted chunks by comparing uncompressed NBT directly.
        // This catches write/truncation bugs without relying on NBT reordering.
        let mut verify = Region::from_stream(File::open(&staging)?).map_err(wrap)?;
        for z in 0..32 {
            for x in 0..32 {
                let observed = verify.read_chunk(x, z).map_err(wrap)?;
                let expected = if (x, z) == (local_x, local_z) {
                    Some(replacement_nbt.to_vec())
                } else {
                    input.read_chunk(x, z).map_err(wrap)?
                };
                if observed != expected {
                    return Err(invalid("reconstructed region lost an original chunk"));
                }
            }
        }
        // Atomic, create-only publication: hard_link fails if destination
        // exists even if another writer races between preflight and publish.
        fs::hard_link(&staging, destination)?;
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    let _ = fs::remove_file(&staging);
    written
}

/// Rewrite one existing Anvil region atomically under the SAME world session
/// lock used by the Rust server. This is an opt-in FS08 building block, not the
/// active game's IRW save path. Unknown tags in untouched chunks survive as
/// their original uncompressed NBT bytes.
///
/// The source is never written in place. Build and validate a same-filesystem
/// replacement; refuse symlinks and changes to the original before publishing.
/// The session lock coordinates cooperative writers; an external Java server
/// that ignores the Rust advisory lock cannot be made race-free by this API.
pub(crate) fn rewrite_chunk_under_lock(
    lock: &mut WorldSessionLock,
    dimension: Dimension,
    cx: i32,
    cz: i32,
    replacement_nbt: &[u8],
) -> io::Result<()> {
    lock.verify()?;
    let root = lock.world_dir().to_path_buf();
    let dim_root = match dimension {
        Dimension::Overworld => root,
        Dimension::Nether => root.join("DIM-1"),
        Dimension::End => root.join("DIM1"),
    };
    if !fs::symlink_metadata(&dim_root)?.file_type().is_dir() {
        return Err(invalid("Anvil dimension must be a real directory"));
    }
    let region_dir = dim_root.join("region");
    if !fs::symlink_metadata(&region_dir)?.file_type().is_dir() {
        return Err(invalid("Anvil region parent must be a real directory"));
    }
    let filename = format!("r.{}.{}.mca", cx.div_euclid(32), cz.div_euclid(32));
    let source = region_dir.join(&filename);
    if !fs::symlink_metadata(&source)?.file_type().is_file() {
        return Err(invalid("refusing non-regular or symlinked Anvil region"));
    }
    crate::legacy_region::inspect_region(&source)?;
    let before = fs::read(&source)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(wrap)?
        .as_nanos();
    let stage = region_dir.join(format!(
        ".industrial1710-region-cow-{}-{nonce}",
        std::process::id()
    ));
    // create_dir must fail if a stale/malicious stage already exists.
    fs::create_dir(&stage)?;
    let destination = stage.join(&filename);
    let result = (|| -> io::Result<()> {
        rewrite_chunk_create_only(&source, &destination, cx, cz, replacement_nbt)?;
        lock.verify()?;
        let current = fs::symlink_metadata(&source)?;
        if !current.file_type().is_file()
            || current.len() != before.len() as u64
            || fs::read(&source)? != before
        {
            return Err(invalid("Anvil source changed during copy-on-write rewrite"));
        }
        // Rename is atomic inside a filesystem; the old file is never
        // truncated in place. On unsupported filesystems/OS this fails closed.
        fs::rename(&destination, &source)?;
        #[cfg(unix)]
        File::open(&region_dir)?.sync_all()?;
        Ok(())
    })();
    let _ = fs::remove_dir_all(&stage);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{anvil_export, legacy_nbt, world::World};
    use fastnbt::Value;

    fn setup() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "ir1710-region-cow-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root).unwrap();
        anvil_export::export(&World::new(), &root.join("world")).unwrap();
        fs::create_dir(root.join("out")).unwrap();
        root
    }

    #[test]
    fn locked_cow_replaces_existing_region_and_preserves_other_chunks_and_tags() {
        let root = setup();
        let world = root.join("world");
        let source = world.join("region/r.0.0.mca");
        let mut lock = WorldSessionLock::acquire(&world).unwrap();
        let mut original = Region::from_stream(File::open(&source).unwrap()).unwrap();
        let untouched = original.read_chunk(1, 0).unwrap().unwrap();
        let before = original.read_chunk(0, 0).unwrap().unwrap();
        let mut chunk = legacy_nbt::parse_chunk(&before, 0, 0).unwrap();
        chunk.level.extra.insert(
            "IndustrialUnknownLive".into(),
            Value::String("retain".into()),
        );
        let replacement = fastnbt::to_bytes(&chunk).unwrap();
        rewrite_chunk_under_lock(&mut lock, Dimension::Overworld, 0, 0, &replacement).unwrap();
        assert!(lock.verify().is_ok());
        assert_eq!(crate::legacy_region::inspect_region(&source).unwrap(), 9);
        let mut rebuilt = Region::from_stream(File::open(&source).unwrap()).unwrap();
        assert_eq!(rebuilt.read_chunk(0, 0).unwrap().unwrap(), replacement);
        assert_eq!(rebuilt.read_chunk(1, 0).unwrap().unwrap(), untouched);
        assert!(!fs::read_dir(world.join("region"))
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("-cow-")));
        drop(lock);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn locked_cow_rejects_invalid_nbt_and_lost_lock_without_touching_source() {
        let root = setup();
        let world = root.join("world");
        let source = world.join("region/r.0.0.mca");
        let original = fs::read(&source).unwrap();
        let mut lock = WorldSessionLock::acquire(&world).unwrap();
        assert!(
            rewrite_chunk_under_lock(&mut lock, Dimension::Overworld, 0, 0, b"invalid NBT")
                .is_err()
        );
        assert_eq!(fs::read(&source).unwrap(), original);
        fs::write(world.join("session.lock"), 0i64.to_be_bytes()).unwrap();
        assert!(
            rewrite_chunk_under_lock(&mut lock, Dimension::Overworld, 0, 0, b"invalid NBT")
                .is_err()
        );
        assert_eq!(fs::read(&source).unwrap(), original);
        drop(lock);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn locked_cow_rejects_symlinked_region_without_following_it() {
        use std::os::unix::fs::symlink;
        let root = setup();
        let world = root.join("world");
        let source = world.join("region/r.0.0.mca");
        let preserved = root.join("original.mca");
        fs::rename(&source, &preserved).unwrap();
        symlink(&preserved, &source).unwrap();
        let original = fs::read(&preserved).unwrap();
        let mut lock = WorldSessionLock::acquire(&world).unwrap();
        assert!(
            rewrite_chunk_under_lock(&mut lock, Dimension::Overworld, 0, 0, b"invalid").is_err()
        );
        assert_eq!(fs::read(&preserved).unwrap(), original);
        drop(lock);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn locked_cow_refuses_symlinked_dimension_parent() {
        use std::os::unix::fs::symlink;
        let root = setup();
        let world = root.join("world");
        let outside = root.join("outside");
        fs::create_dir_all(outside.join("region")).unwrap();
        symlink(&outside, world.join("DIM-1")).unwrap();
        let mut lock = WorldSessionLock::acquire(&world).unwrap();
        assert!(
            rewrite_chunk_under_lock(&mut lock, Dimension::Nether, 0, 0, b"invalid").is_err()
        );
        assert!(!outside.join("region/r.0.0.mca").exists());
        drop(lock);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn changes_one_chunk_preserves_all_other_nbt_and_source() {
        let root = setup();
        let source = root.join("world/region/r.0.0.mca");
        let destination = root.join("out/r.0.0.mca");
        let original = fs::read(&source).unwrap();
        let mut reader = Region::from_stream(File::open(&source).unwrap()).unwrap();
        let before = reader.read_chunk(0, 0).unwrap().unwrap();
        let untouched = reader.read_chunk(1, 0).unwrap().unwrap();
        let mut chunk = legacy_nbt::parse_chunk(&before, 0, 0).unwrap();
        chunk.level.extra.insert(
            "IndustrialTestOnly".into(),
            Value::String("copy-on-write".into()),
        );
        let replacement = fastnbt::to_bytes(&chunk).unwrap();
        rewrite_chunk_create_only(&source, &destination, 0, 0, &replacement).unwrap();
        assert_eq!(fs::read(&source).unwrap(), original);
        assert_eq!(
            crate::legacy_region::inspect_region(&destination).unwrap(),
            9
        );
        let mut output = Region::from_stream(File::open(&destination).unwrap()).unwrap();
        assert_eq!(output.read_chunk(0, 0).unwrap().unwrap(), replacement);
        assert_eq!(output.read_chunk(1, 0).unwrap().unwrap(), untouched);
        assert!(rewrite_chunk_create_only(&source, &destination, 0, 0, &replacement).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_source_and_missing_chunk_never_publish_anything() {
        let root = setup();
        let source = root.join("world/region/r.0.0.mca");
        let destination = root.join("out/r.0.0.mca");
        assert!(rewrite_chunk_create_only(&source, &destination, 0, 0, b"invalid").is_err());
        let mut reader = Region::from_stream(File::open(&source).unwrap()).unwrap();
        let nbt = reader.read_chunk(0, 0).unwrap().unwrap();
        assert!(rewrite_chunk_create_only(&source, &destination, 31, 31, &nbt).is_err());
        assert!(!destination.exists());
        assert!(!destination.with_extension("creating").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
