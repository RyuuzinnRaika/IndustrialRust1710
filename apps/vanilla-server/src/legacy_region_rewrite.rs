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

/// Backwards-compatible single-chunk create-only entrypoint.
pub(crate) fn rewrite_chunk_create_only(
    source: &Path,
    destination: &Path,
    cx: i32,
    cz: i32,
    replacement_nbt: &[u8],
) -> io::Result<()> {
    rewrite_chunks_create_only(source, destination, &[((cx, cz), replacement_nbt)])
}

/// Rebuild one EXISTING Anvil region while replacing several existing chunks
/// in a single create-only publication. The caller must not mix region files.
/// Every unchanged chunk retains its uncompressed NBT bytes, including unknown
/// tags; validation and full-region verification finish before publication.
pub(crate) fn rewrite_chunks_create_only(
    source: &Path,
    destination: &Path,
    replacements: &[((i32, i32), &[u8])],
) -> io::Result<()> {
    build_region_create_only(Some(source), destination, replacements, false)
}

/// Opt-in E46 helper. Allow inserting previously ungenerated chunks, or
/// creating a completely new region, without changing the existing
/// update-only API. This only creates a *staged* file; the caller must publish
/// it through a verified session-locked redo transaction.
fn build_region_create_only(
    source: Option<&Path>,
    destination: &Path,
    replacements: &[((i32, i32), &[u8])],
    allow_new_chunks: bool,
) -> io::Result<()> {
    let (first, _) = replacements
        .first()
        .ok_or_else(|| invalid("Anvil region update must contain chunks"))?;
    if replacements.len() > 1024 {
        return Err(invalid("too many Anvil chunks in one region update"));
    }
    let (cx, cz) = *first;
    let region = (cx.div_euclid(32), cz.div_euclid(32));
    let filename = format!("r.{}.{}.mca", region.0, region.1);
    if source.is_some_and(|path| {
        path.file_name().and_then(|name| name.to_str()) != Some(filename.as_str())
    }) || destination.file_name().and_then(|name| name.to_str()) != Some(filename.as_str())
    {
        return Err(invalid(
            "Anvil region name does not match replacement chunk",
        ));
    }
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
    let mut overlay = std::collections::BTreeMap::new();
    for &((x, z), nbt) in replacements {
        if (x.div_euclid(32), z.div_euclid(32)) != region {
            return Err(invalid("Anvil batch crosses region boundary"));
        }
        crate::legacy_nbt::parse_chunk(nbt, x, z)?;
        if overlay
            .insert((x.rem_euclid(32) as usize, z.rem_euclid(32) as usize), nbt)
            .is_some()
        {
            return Err(invalid("duplicate Anvil chunk in batch"));
        }
    }

    // Existing sources are parsed completely before any staging mutation.
    let mut input = if let Some(path) = source {
        if !fs::symlink_metadata(path)?.file_type().is_file() {
            return Err(invalid("source Anvil region is not a regular file"));
        }
        crate::legacy_region::inspect_region(path)?;
        Some(Region::from_stream(File::open(path)?).map_err(wrap)?)
    } else {
        if !allow_new_chunks {
            return Err(invalid(
                "missing Anvil region requires explicit insert mode",
            ));
        }
        None
    };
    if !allow_new_chunks {
        for &(x, z) in overlay.keys() {
            if input
                .as_mut()
                .ok_or_else(|| invalid("missing source Anvil region"))?
                .read_chunk(x, z)
                .map_err(wrap)?
                .is_none()
            {
                return Err(invalid(
                    "source chunk does not exist; new generation is separate",
                ));
            }
        }
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
                let original = match input.as_mut() {
                    Some(reader) => reader.read_chunk(x, z).map_err(wrap)?,
                    None => None,
                };
                let contents = overlay.get(&(x, z)).copied().or(original.as_deref());
                if let Some(bytes) = contents {
                    output.write_chunk(x, z, bytes).map_err(wrap)?;
                }
            }
        }
        let mut file = output.into_inner().map_err(wrap)?;
        let length = file.stream_position()?;
        file.set_len(length)?;
        file.sync_all()?;
        drop(file);
        // Validate every sector/chunk, not only the newly written positions.
        crate::legacy_region::inspect_region_bytes(destination, &fs::read(&staging)?)?;
        let mut verify = Region::from_stream(File::open(&staging)?).map_err(wrap)?;
        for z in 0..32 {
            for x in 0..32 {
                let observed = verify.read_chunk(x, z).map_err(wrap)?;
                let expected = match overlay.get(&(x, z)) {
                    Some(bytes) => Some(bytes.to_vec()),
                    None => match input.as_mut() {
                        Some(reader) => reader.read_chunk(x, z).map_err(wrap)?,
                        None => None,
                    },
                };
                if observed != expected {
                    return Err(invalid("reconstructed region lost an original chunk"));
                }
            }
        }
        // This stage cannot overwrite an existing destination.
        fs::hard_link(&staging, destination)?;
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    let _ = fs::remove_file(&staging);
    written
}

/// Backwards-compatible single-chunk session-locked entrypoint.
pub(crate) fn rewrite_chunk_under_lock(
    lock: &mut WorldSessionLock,
    dimension: Dimension,
    cx: i32,
    cz: i32,
    replacement_nbt: &[u8],
) -> io::Result<()> {
    rewrite_chunks_under_lock(lock, dimension, &[((cx, cz), replacement_nbt)])
}

/// Atomically publish a batch of chunks within ONE EXISTING region under a
/// valid world session lock. This remains an opt-in FS08 building block; it
/// does NOT enable live Anvil saving or cross-region transactions.
pub(crate) fn rewrite_chunks_under_lock(
    lock: &mut WorldSessionLock,
    dimension: Dimension,
    replacements: &[((i32, i32), &[u8])],
) -> io::Result<()> {
    rewrite_chunks_under_lock_expected(lock, dimension, replacements, None)
}

/// Refuse to publish an inventory edit computed from an older region image.
/// The optional expected bytes are captured before decoding the tile NBT.
pub(crate) fn rewrite_chunks_under_lock_expected(
    lock: &mut WorldSessionLock,
    dimension: Dimension,
    replacements: &[((i32, i32), &[u8])],
    expected_source: Option<&[u8]>,
) -> io::Result<()> {
    lock.verify()?;
    let (first, _) = replacements
        .first()
        .ok_or_else(|| invalid("Anvil region update must contain chunks"))?;
    let (cx, cz) = *first;
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
    let before = fs::read(&source)?;
    if expected_source.is_some_and(|snapshot| snapshot != before.as_slice()) {
        return Err(invalid(
            "Anvil region changed since the tile inventory was read",
        ));
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(wrap)?
        .as_nanos();
    let stage = region_dir.join(format!(
        ".industrial1710-region-cow-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&stage)?;
    let destination = stage.join(&filename);
    let result = (|| -> io::Result<()> {
        rewrite_chunks_create_only(&source, &destination, replacements)?;
        lock.verify()?;
        let current = fs::symlink_metadata(&source)?;
        if !current.file_type().is_file()
            || current.len() != before.len() as u64
            || fs::read(&source)? != before
        {
            return Err(invalid("Anvil source changed during copy-on-write rewrite"));
        }
        fs::rename(&destination, &source)?;
        #[cfg(unix)]
        File::open(&region_dir)?.sync_all()?;
        Ok(())
    })();
    let _ = fs::remove_dir_all(&stage);
    result
}

/// Opt-in E45/E46 bridge: atomically redo one existing .mca region and one
/// native JSON sidecar after a crash. This is a migration building block; it
/// is NOT automatically invoked by the active IRW world worker.
///
/// The journal lives next to the world's IRW identity and is recovered by the
/// existing startup path before containers or world state are loaded.
/// If commit fails after journal publication, the caller MUST stop writing and
/// let normal startup recovery finish the transaction.
pub(crate) fn rewrite_chunks_with_sidecar_under_lock_expected(
    lock: &mut WorldSessionLock,
    dimension: Dimension,
    replacements: &[((i32, i32), &[u8])],
    expected_source: Option<&[u8]>,
    world_file: &Path,
    sidecar_file: &Path,
    sidecar_json: &[u8],
) -> io::Result<()> {
    lock.verify()?;
    let root = lock.world_dir().to_path_buf();
    if world_file.parent() != Some(root.as_path())
        || world_file.extension() != Some(std::ffi::OsStr::new("irw"))
        || sidecar_file.parent() != Some(root.as_path())
    {
        return Err(invalid(
            "region transaction must use the locked IRW world root",
        ));
    }
    let name = sidecar_file
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| invalid("invalid sidecar name"))?;
    if ![
        ".chests.json",
        ".furnaces.json",
        ".hoppers.json",
        ".brewers.json",
        ".dispensers.json",
        ".inventory.json",
    ]
    .iter()
    .any(|suffix| name.ends_with(suffix))
    {
        return Err(invalid("unsupported TileEntity transaction sidecar"));
    }
    serde_json::from_slice::<serde_json::Value>(sidecar_json)
        .map_err(|_| invalid("sidecar is not JSON"))?;
    let (first, _) = replacements
        .first()
        .ok_or_else(|| invalid("region transaction cannot be empty"))?;
    let dim_root = match dimension {
        Dimension::Overworld => root.clone(),
        Dimension::Nether => root.join("DIM-1"),
        Dimension::End => root.join("DIM1"),
    };
    if !fs::symlink_metadata(&dim_root)?.file_type().is_dir() {
        return Err(invalid("invalid Anvil dimension directory"));
    }
    let regions = dim_root.join("region");
    if !fs::symlink_metadata(&regions)?.file_type().is_dir() {
        return Err(invalid("invalid Anvil region directory"));
    }
    let filename = format!(
        "r.{}.{}.mca",
        first.0.div_euclid(32),
        first.1.div_euclid(32)
    );
    let source = regions.join(&filename);
    if !fs::symlink_metadata(&source)?.file_type().is_file() {
        return Err(invalid("invalid or symlinked Anvil source"));
    }
    let journal_path = world_file.with_extension("pending-txn.json");
    if journal_path.exists() {
        return Err(invalid(
            "pending redo journal must recover before another save",
        ));
    }
    // Both participants are derived from this one consistent source image.
    let before = fs::read(&source)?;
    if expected_source.is_some_and(|bytes| bytes != before.as_slice()) {
        return Err(invalid("stale source region before staged transaction"));
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(wrap)?
        .as_nanos();
    let stage = regions.join(format!(
        ".industrial1710-journal-stage-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&stage)?;
    let destination = stage.join(&filename);
    let result = (|| -> io::Result<()> {
        rewrite_chunks_create_only(&source, &destination, replacements)?;
        let rewritten = fs::read(&destination)?;
        lock.verify()?;
        if !fs::symlink_metadata(&source)?.file_type().is_file() || fs::read(&source)? != before {
            return Err(invalid("source region changed before journal commit"));
        }
        crate::journal::commit(
            &journal_path,
            &[
                (source.as_path(), rewritten),
                (sidecar_file, sidecar_json.to_vec()),
            ],
        )
    })();
    let _ = fs::remove_dir_all(&stage);
    result
}

/// E45/E46 foundation: publish edits to multiple EXISTING Anvil regions and
/// native sidecars in one crash-recoverable redo generation. The caller must
/// supply the *exact* bytes observed when computing every chunk replacement.
/// This prevents a later write from silently overwriting a newer region image.
///
/// Only a valid session owner can call this. No new chunks are invented, and
/// this deliberately does not switch the live IRW world to Anvil storage.
pub(crate) fn rewrite_regions_with_sidecars_under_lock_expected(
    lock: &mut WorldSessionLock,
    dimension: Dimension,
    replacements: &[((i32, i32), &[u8])],
    expected_sources: &std::collections::BTreeMap<(i32, i32), Vec<u8>>,
    world_file: &Path,
    sidecars: &[(&Path, Vec<u8>)],
) -> io::Result<()> {
    let source_images = expected_sources
        .iter()
        .map(|(region, bytes)| (*region, Some(bytes.clone())))
        .collect();
    commit_region_batch(
        lock,
        dimension,
        replacements,
        &source_images,
        world_file,
        sidecars,
        RegionCommitMode::ExistingOnly,
    )
}

/// E46 opt-in bridge: atomically publish explicitly changed EXISTING Anvil
/// chunks and the corresponding IRW snapshot in a single redo generation.
/// The IRW bytes must be serialized BEFORE taking this session-lock guard.
/// This is not wired into the live save worker until migration/recovery gates
/// for entities, player files, and all containers are satisfied.
pub(crate) fn flush_irw_block_edits_under_lock_expected(
    lock: &mut WorldSessionLock,
    dimension: Dimension,
    changes: &[crate::legacy_chunk_patch::BlockPatch],
    expected_sources: &std::collections::BTreeMap<(i32, i32), Vec<u8>>,
    world_file: &Path,
    expected_irw: &[u8],
    next_irw: &[u8],
) -> io::Result<()> {
    use std::collections::BTreeMap;
    lock.verify()?;
    if changes.is_empty() || changes.len() > 2048 {
        return Err(invalid("invalid dirty Anvil block batch size"));
    }
    // A block change cannot be represented by a bare .mca rewrite unless
    // its IRW save generation is also committed and recoverable.
    let valid_irw = expected_irw.len() >= 8
        && next_irw.len() >= 8
        && matches!(&expected_irw[..4], b"IRW1" | b"IRW2" | b"IRW3" | b"IRW4")
        && next_irw[..4] == expected_irw[..4];
    if !valid_irw {
        return Err(invalid("invalid or mismatched IRW snapshot version"));
    }
    let root = lock.world_dir();
    if world_file.parent() != Some(root) {
        return Err(invalid("dirty flush IRW must belong to locked world"));
    }
    let dimension_root = match dimension {
        Dimension::Overworld => root.to_path_buf(),
        Dimension::Nether => root.join("DIM-1"),
        Dimension::End => root.join("DIM1"),
    };
    let mut patches: BTreeMap<(i32, i32), Vec<crate::legacy_chunk_patch::BlockPatch>> =
        BTreeMap::new();
    for &change in changes {
        patches
            .entry((change.0.div_euclid(16), change.2.div_euclid(16)))
            .or_default()
            .push(change);
    }
    let mut replacements = Vec::with_capacity(patches.len());
    for (&(cx, cz), updates) in &patches {
        let region_key = (cx.div_euclid(32), cz.div_euclid(32));
        let snapshot = expected_sources
            .get(&region_key)
            .ok_or_else(|| invalid("dirty flush missing expected region bytes"))?;
        let region_path = dimension_root
            .join("region")
            .join(format!("r.{}.{}.mca", region_key.0, region_key.1));
        if snapshot_region(&region_path)?.as_deref() != Some(snapshot.as_slice()) {
            return Err(invalid("dirty Anvil source changed before patching"));
        }
        let mut reader = Region::from_stream(std::io::Cursor::new(snapshot)).map_err(wrap)?;
        let original = reader
            .read_chunk(cx.rem_euclid(32) as usize, cz.rem_euclid(32) as usize)
            .map_err(wrap)?
            .ok_or_else(|| invalid("dirty chunk must exist before block-only flush"))?;
        let encoded = crate::legacy_chunk_patch::patch_chunk(&original, cx, cz, updates)?;
        replacements.push(((cx, cz), encoded));
    }
    let borrowed: Vec<_> = replacements
        .iter()
        .map(|(coord, bytes)| (*coord, bytes.as_slice()))
        .collect();
    let region_images = expected_sources
        .iter()
        .map(|(key, bytes)| (*key, Some(bytes.clone())))
        .collect();
    commit_region_batch(
        lock,
        dimension,
        &borrowed,
        &region_images,
        world_file,
        &[(world_file, next_irw.to_vec())],
        RegionCommitMode::IrwImage(expected_irw),
    )
}

/// E46 opt-in migration primitive: add previously absent chunks or entire
/// region files while publishing 2..8 related files as one redo generation.
/// Every region must have an explicit expected byte image or explicit None
/// (expected to be absent). No active gameplay writer calls this yet.
pub(crate) fn insert_regions_with_sidecars_under_lock_expected(
    lock: &mut WorldSessionLock,
    dimension: Dimension,
    replacements: &[((i32, i32), &[u8])],
    expected_sources: &std::collections::BTreeMap<(i32, i32), Option<Vec<u8>>>,
    world_file: &Path,
    sidecars: &[(&Path, Vec<u8>)],
) -> io::Result<()> {
    commit_region_batch(
        lock,
        dimension,
        replacements,
        expected_sources,
        world_file,
        sidecars,
        RegionCommitMode::InsertMissing,
    )
}

/// E46 write-through of NEW chunks plus their authoritative IRW generation.
/// Unlike the older offline insert primitive, this transaction can ONLY add
/// absent chunks, never overwrite an existing chunk or unknown native NBT.
/// Existing region images must match their caller-supplied snapshots exactly.
pub(crate) fn insert_irw_chunks_under_lock_expected(
    lock: &mut WorldSessionLock,
    dimension: Dimension,
    replacements: &[((i32, i32), &[u8])],
    expected_sources: &std::collections::BTreeMap<(i32, i32), Option<Vec<u8>>>,
    world_file: &Path,
    expected_irw: &[u8],
    next_irw: &[u8],
) -> io::Result<()> {
    if expected_irw.len() < 8
        || next_irw.len() < 8
        || !matches!(&expected_irw[..4], b"IRW1" | b"IRW2" | b"IRW3" | b"IRW4")
        || next_irw[..4] != expected_irw[..4]
    {
        return Err(invalid("E46 chunk insert requires matching IRW versions"));
    }
    commit_region_batch(
        lock,
        dimension,
        replacements,
        expected_sources,
        world_file,
        &[(world_file, next_irw.to_vec())],
        RegionCommitMode::IrwInsert(expected_irw),
    )
}

fn snapshot_region(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => fs::read(path).map(Some),
        Ok(_) => Err(invalid("region source must be a regular file")),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

enum RegionCommitMode<'a> {
    ExistingOnly,
    InsertMissing,
    IrwImage(&'a [u8]),
    IrwInsert(&'a [u8]),
}

fn commit_region_batch(
    lock: &mut WorldSessionLock,
    dimension: Dimension,
    replacements: &[((i32, i32), &[u8])],
    expected_sources: &std::collections::BTreeMap<(i32, i32), Option<Vec<u8>>>,
    world_file: &Path,
    sidecars: &[(&Path, Vec<u8>)],
    mode: RegionCommitMode<'_>,
) -> io::Result<()> {
    use std::collections::{BTreeMap, BTreeSet};
    let allow_new_chunks = matches!(
        mode,
        RegionCommitMode::InsertMissing | RegionCommitMode::IrwInsert(_)
    );
    let require_missing_chunks = matches!(mode, RegionCommitMode::IrwInsert(_));
    let expected_irw = match mode {
        RegionCommitMode::IrwImage(bytes) | RegionCommitMode::IrwInsert(bytes) => Some(bytes),
        _ => None,
    };
    lock.verify()?;
    let root = lock.world_dir().to_path_buf();
    if world_file.parent() != Some(root.as_path())
        || world_file.extension() != Some(std::ffi::OsStr::new("irw"))
    {
        return Err(invalid(
            "region transaction must use the locked IRW world root",
        ));
    }
    if let Some(expected) = expected_irw {
        if snapshot_region(world_file)?.as_deref() != Some(expected) {
            return Err(invalid("IRW source changed before atomic dirty flush"));
        }
    }
    let journal_path = world_file.with_extension("pending-txn.json");
    if journal_path.exists() {
        return Err(invalid(
            "pending redo journal must recover before another save",
        ));
    }
    let dim_root = match dimension {
        Dimension::Overworld => root.clone(),
        Dimension::Nether => root.join("DIM-1"),
        Dimension::End => root.join("DIM1"),
    };
    if !fs::symlink_metadata(&dim_root)?.file_type().is_dir() {
        return Err(invalid("invalid Anvil dimension directory"));
    }
    let regions = dim_root.join("region");
    if !fs::symlink_metadata(&regions)?.file_type().is_dir() {
        return Err(invalid("invalid Anvil region directory"));
    }
    type RegionReplacements<'a> = BTreeMap<(i32, i32), Vec<((i32, i32), &'a [u8])>>;
    let mut grouped: RegionReplacements<'_> = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for &((cx, cz), nbt) in replacements {
        if !seen.insert((cx, cz)) {
            return Err(invalid("duplicate Anvil chunk in multi-region transaction"));
        }
        grouped
            .entry((cx.div_euclid(32), cz.div_euclid(32)))
            .or_default()
            .push(((cx, cz), nbt));
    }
    if grouped.is_empty()
        || grouped.len() != expected_sources.len()
        || grouped.len() + sidecars.len() < 2
        || grouped.len() + sidecars.len() > 8
        || replacements.len() > 2048
    {
        return Err(invalid(
            "invalid number of regions or transaction participants",
        ));
    }
    for (path, bytes) in sidecars {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| invalid("sidecar name"))?;
        let root_sidecar = path.parent() == Some(root.as_path())
            && [
                ".chests.json",
                ".furnaces.json",
                ".hoppers.json",
                ".brewers.json",
                ".dispensers.json",
                ".inventory.json",
            ]
            .iter()
            .any(|extension| name.ends_with(extension));
        let player_inventory = path.parent() == Some(root.join("players").as_path())
            && name.ends_with(".inventory.json");
        let world_snapshot = expected_irw.is_some() && *path == world_file;
        if !root_sidecar && !player_inventory && !world_snapshot {
            return Err(invalid("unsupported native transaction sidecar"));
        }
        if !world_snapshot {
            serde_json::from_slice::<serde_json::Value>(bytes)
                .map_err(|_| invalid("native sidecar is not JSON"))?;
        }
    }

    // Preflight *all* region images before creating any staging file. Refuse
    // changed originals and symlinked paths, including negative coordinates.
    let mut sources = Vec::with_capacity(grouped.len());
    for &(rx, rz) in grouped.keys() {
        let expected = expected_sources
            .get(&(rx, rz))
            .ok_or_else(|| invalid("missing source region snapshot"))?;
        let source = regions.join(format!("r.{rx}.{rz}.mca"));
        let observed = snapshot_region(&source)?;
        if observed.as_ref() != expected.as_ref() {
            return Err(invalid("source Anvil region changed since snapshot"));
        }
        if observed.is_none() && !allow_new_chunks {
            return Err(invalid(
                "missing region cannot be created by update-only API",
            ));
        }
        if let Some(snapshot) = observed.as_ref() {
            crate::legacy_region::inspect_region(&source)?;
            // No legitimate new-chunk transaction may silently clobber NBT
            // that a second server or an earlier migration already created.
            if require_missing_chunks {
                let mut reader =
                    Region::from_stream(std::io::Cursor::new(snapshot)).map_err(wrap)?;
                for &((cx, cz), _) in &grouped[&(rx, rz)] {
                    if reader
                        .read_chunk(cx.rem_euclid(32) as usize, cz.rem_euclid(32) as usize)
                        .map_err(wrap)?
                        .is_some()
                    {
                        return Err(invalid(
                            "E46 insert would overwrite an existing Anvil chunk",
                        ));
                    }
                }
            }
        }
        sources.push(((rx, rz), source));
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(wrap)?
        .as_nanos();
    let stage = regions.join(format!(
        ".industrial1710-multi-region-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&stage)?;
    let result = (|| -> io::Result<()> {
        let mut changes: Vec<(&Path, Vec<u8>)> = Vec::with_capacity(grouped.len() + sidecars.len());
        for ((rx, rz), source) in &sources {
            let destination = stage.join(format!("r.{rx}.{rz}.mca"));
            let observed = &expected_sources[&(*rx, *rz)];
            build_region_create_only(
                observed.as_ref().map(|_| source.as_path()),
                &destination,
                &grouped[&(*rx, *rz)],
                allow_new_chunks,
            )?;
            changes.push((source.as_path(), fs::read(&destination)?));
        }
        lock.verify()?;
        // A racing non-cooperative editor is detected before redo publication.
        for ((rx, rz), source) in &sources {
            if snapshot_region(source)?.as_ref() != expected_sources[&(*rx, *rz)].as_ref() {
                return Err(invalid("source region changed before multi-region commit"));
            }
        }
        if let Some(expected) = expected_irw {
            if snapshot_region(world_file)?.as_deref() != Some(expected) {
                return Err(invalid("IRW source changed before redo publication"));
            }
        }
        for (path, bytes) in sidecars {
            changes.push((*path, bytes.clone()));
        }
        crate::journal::commit(&journal_path, &changes)
    })();
    // A committed redo log is never removed by this cleanup. Errors after
    // publication remain fail-closed until startup recovery replays the log.
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
    fn e46_creates_absent_region_and_chunk_in_existing_region_with_one_redo() {
        use std::collections::BTreeMap;
        let base = setup();
        let world = base.join("world");
        let old_region = world.join("region/r.0.0.mca");
        let new_region = world.join("region/r.1.-1.mca");
        let old_bytes = fs::read(&old_region).unwrap();
        let mut reader = Region::from_stream(File::open(&old_region).unwrap()).unwrap();
        let unchanged = reader.read_chunk(1, 0).unwrap().unwrap();
        let raw = reader.read_chunk(0, 0).unwrap().unwrap();
        let mut template = legacy_nbt::parse_chunk(&raw, 0, 0).unwrap();
        template.level.x = 31;
        template.level.z = 31;
        template
            .level
            .extra
            .insert("UnknownNewChunk".into(), Value::String("retain me".into()));
        let added_to_old = fastnbt::to_bytes(&template).unwrap();
        template.level.x = 32;
        template.level.z = -1;
        let added_to_new = fastnbt::to_bytes(&template).unwrap();
        let expectations = BTreeMap::from([((0, 0), Some(old_bytes)), ((1, -1), None)]);
        let sidecar = world.join("flat.chests.json");
        let world_file = world.join("flat.irw");
        let mut lock = WorldSessionLock::acquire(&world).unwrap();
        insert_regions_with_sidecars_under_lock_expected(
            &mut lock,
            Dimension::Overworld,
            &[((31, 31), &added_to_old), ((32, -1), &added_to_new)],
            &expectations,
            &world_file,
            &[(sidecar.as_path(), br#"{"chests":[]}"#.to_vec())],
        )
        .unwrap();
        let mut existing = Region::from_stream(File::open(&old_region).unwrap()).unwrap();
        assert_eq!(existing.read_chunk(31, 31).unwrap().unwrap(), added_to_old);
        assert_eq!(existing.read_chunk(1, 0).unwrap().unwrap(), unchanged);
        let mut fresh = Region::from_stream(File::open(&new_region).unwrap()).unwrap();
        assert_eq!(fresh.read_chunk(0, 31).unwrap().unwrap(), added_to_new);
        assert_eq!(
            crate::legacy_region::inspect_region(&new_region).unwrap(),
            1
        );
        assert_eq!(fs::read(&sidecar).unwrap(), br#"{"chests":[]}"#);
        assert!(!world_file.with_extension("pending-txn.json").exists());
        drop(lock);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn e46_refuses_new_region_when_snapshot_claims_absence_but_file_exists() {
        use std::collections::BTreeMap;
        let base = setup();
        let world = base.join("world");
        let source = world.join("region/r.0.0.mca");
        let original = fs::read(&source).unwrap();
        let mut reader = Region::from_stream(File::open(&source).unwrap()).unwrap();
        let replacement = reader.read_chunk(0, 0).unwrap().unwrap();
        let sidecar = world.join("flat.chests.json");
        let world_file = world.join("flat.irw");
        let expectations = BTreeMap::from([((0, 0), None)]);
        let mut lock = WorldSessionLock::acquire(&world).unwrap();
        assert!(insert_regions_with_sidecars_under_lock_expected(
            &mut lock,
            Dimension::Overworld,
            &[((0, 0), &replacement)],
            &expectations,
            &world_file,
            &[(sidecar.as_path(), b"{}".to_vec())],
        )
        .is_err());
        assert_eq!(fs::read(&source).unwrap(), original);
        assert!(!sidecar.exists());
        assert!(!world_file.with_extension("pending-txn.json").exists());
        drop(lock);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn e46_rejects_bad_new_chunk_without_creating_a_region_or_sidecar() {
        use std::collections::BTreeMap;
        let base = setup();
        let world = base.join("world");
        let absent = world.join("region/r.2.0.mca");
        let sidecar = world.join("flat.chests.json");
        let world_file = world.join("flat.irw");
        let expectations = BTreeMap::from([((2, 0), None)]);
        let mut lock = WorldSessionLock::acquire(&world).unwrap();
        assert!(insert_regions_with_sidecars_under_lock_expected(
            &mut lock,
            Dimension::Overworld,
            &[((64, 0), b"invalid NBT")],
            &expectations,
            &world_file,
            &[(sidecar.as_path(), b"{}".to_vec())],
        )
        .is_err());
        assert!(!absent.exists());
        assert!(!sidecar.exists());
        assert!(!world_file.with_extension("pending-txn.json").exists());
        drop(lock);
        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn e46_refuses_symlink_at_absent_region_path() {
        use std::collections::BTreeMap;
        use std::os::unix::fs::symlink;
        let base = setup();
        let world = base.join("world");
        let destination = world.join("region/r.2.0.mca");
        let outside = base.join("outside.mca");
        fs::write(&outside, b"untouched").unwrap();
        symlink(&outside, &destination).unwrap();
        let mut reader =
            Region::from_stream(File::open(world.join("region/r.0.0.mca")).unwrap()).unwrap();
        let mut chunk =
            legacy_nbt::parse_chunk(&reader.read_chunk(0, 0).unwrap().unwrap(), 0, 0).unwrap();
        chunk.level.x = 64;
        let nbt = fastnbt::to_bytes(&chunk).unwrap();
        let sidecar = world.join("flat.chests.json");
        let mut lock = WorldSessionLock::acquire(&world).unwrap();
        assert!(insert_regions_with_sidecars_under_lock_expected(
            &mut lock,
            Dimension::Overworld,
            &[((64, 0), &nbt)],
            &BTreeMap::from([((2, 0), None)]),
            &world.join("flat.irw"),
            &[(sidecar.as_path(), b"{}".to_vec())],
        )
        .is_err());
        assert_eq!(fs::read(&outside).unwrap(), b"untouched");
        assert!(!sidecar.exists());
        drop(lock);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn dirty_anvil_block_and_irw_are_atomic_with_unknown_nbt_preserved() {
        use std::collections::BTreeMap;
        let base = setup();
        let root = base.join("world");
        let region = root.join("region/r.0.0.mca");
        let before = fs::read(&region).unwrap();
        let original_irw = World::new().encoded().unwrap();
        let irw = root.join("flat.irw");
        fs::write(&irw, &original_irw).unwrap();
        let mut next = World::new();
        next.import_block(2, 90, 3, 5 << 4).unwrap();
        let next_irw = next.encoded().unwrap();
        let unchanged = {
            let mut reader = Region::from_stream(File::open(&region).unwrap()).unwrap();
            reader.read_chunk(1, 0).unwrap().unwrap()
        };
        let mut lock = WorldSessionLock::acquire(&root).unwrap();
        flush_irw_block_edits_under_lock_expected(
            &mut lock,
            Dimension::Overworld,
            &[(2, 90, 3, 5 << 4)],
            &BTreeMap::from([((0, 0), before)]),
            &irw,
            &original_irw,
            &next_irw,
        )
        .unwrap();
        assert_eq!(fs::read(&irw).unwrap(), next_irw);
        let mut reader = Region::from_stream(File::open(&region).unwrap()).unwrap();
        let nbt = reader.read_chunk(0, 0).unwrap().unwrap();
        let chunk = legacy_nbt::parse_chunk(&nbt, 0, 0).unwrap();
        assert_eq!(
            chunk
                .level
                .sections
                .iter()
                .find(|s| s.y == 5)
                .unwrap()
                .block_at((90 % 16) * 256 + 3 * 16 + 2),
            Some((5, 0))
        );
        assert_eq!(reader.read_chunk(1, 0).unwrap().unwrap(), unchanged);
        assert!(!irw.with_extension("pending-txn.json").exists());
        drop(lock);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn stale_irw_prevents_any_anvil_publication() {
        use std::collections::BTreeMap;
        let base = setup();
        let root = base.join("world");
        let region = root.join("region/r.0.0.mca");
        let before = fs::read(&region).unwrap();
        let irw = root.join("flat.irw");
        let actual_irw = World::new().encoded().unwrap();
        fs::write(&irw, &actual_irw).unwrap();
        let mut lock = WorldSessionLock::acquire(&root).unwrap();
        assert!(flush_irw_block_edits_under_lock_expected(
            &mut lock,
            Dimension::Overworld,
            &[(2, 90, 3, 5 << 4)],
            &BTreeMap::from([((0, 0), before.clone())]),
            &irw,
            b"IRW1xxxx",
            &actual_irw,
        )
        .is_err());
        assert_eq!(fs::read(&region).unwrap(), before);
        assert_eq!(fs::read(&irw).unwrap(), actual_irw);
        assert!(!irw.with_extension("pending-txn.json").exists());
        drop(lock);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn multi_region_and_native_chest_commit_preserves_unknown_tags() {
        use std::collections::BTreeMap;
        let base = setup();
        let root = base.join("world");
        let pos = root.join("region/r.0.0.mca");
        let neg = root.join("region/r.-1.0.mca");
        let before_pos = fs::read(&pos).unwrap();
        let before_neg = fs::read(&neg).unwrap();
        let mut positive = Region::from_stream(File::open(&pos).unwrap()).unwrap();
        let mut negative = Region::from_stream(File::open(&neg).unwrap()).unwrap();
        let untouched = positive.read_chunk(1, 0).unwrap().unwrap();
        let mut chunk_pos =
            legacy_nbt::parse_chunk(&positive.read_chunk(0, 0).unwrap().unwrap(), 0, 0).unwrap();
        let mut chunk_neg =
            legacy_nbt::parse_chunk(&negative.read_chunk(31, 0).unwrap().unwrap(), -1, 0).unwrap();
        chunk_pos
            .level
            .extra
            .insert("ModdedPos".into(), Value::String("preserve".into()));
        chunk_neg
            .level
            .extra
            .insert("ModdedNeg".into(), Value::String("preserve".into()));
        let changed_pos = fastnbt::to_bytes(&chunk_pos).unwrap();
        let changed_neg = fastnbt::to_bytes(&chunk_neg).unwrap();
        let sidecar = root.join("flat.chests.json");
        let world_file = root.join("flat.irw");
        let sidecar_bytes = br#"{"chests":[]}"#.to_vec();
        let sources = BTreeMap::from([((0, 0), before_pos), ((-1, 0), before_neg)]);
        let mut lock = WorldSessionLock::acquire(&root).unwrap();
        rewrite_regions_with_sidecars_under_lock_expected(
            &mut lock,
            Dimension::Overworld,
            &[((0, 0), &changed_pos), ((-1, 0), &changed_neg)],
            &sources,
            &world_file,
            &[(sidecar.as_path(), sidecar_bytes.clone())],
        )
        .unwrap();
        let mut written_pos = Region::from_stream(File::open(&pos).unwrap()).unwrap();
        let mut written_neg = Region::from_stream(File::open(&neg).unwrap()).unwrap();
        assert_eq!(written_pos.read_chunk(0, 0).unwrap().unwrap(), changed_pos);
        assert_eq!(written_neg.read_chunk(31, 0).unwrap().unwrap(), changed_neg);
        assert_eq!(written_pos.read_chunk(1, 0).unwrap().unwrap(), untouched);
        assert_eq!(fs::read(&sidecar).unwrap(), sidecar_bytes);
        assert!(!world_file.with_extension("pending-txn.json").exists());
        drop(lock);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn multi_region_rejects_stale_later_region_without_partial_commit() {
        use std::collections::BTreeMap;
        let base = setup();
        let root = base.join("world");
        let pos = root.join("region/r.0.0.mca");
        let neg = root.join("region/r.-1.0.mca");
        let before_pos = fs::read(&pos).unwrap();
        let before_neg = fs::read(&neg).unwrap();
        let mut reader_pos = Region::from_stream(File::open(&pos).unwrap()).unwrap();
        let mut reader_neg = Region::from_stream(File::open(&neg).unwrap()).unwrap();
        let nbt_pos = reader_pos.read_chunk(0, 0).unwrap().unwrap();
        let nbt_neg = reader_neg.read_chunk(31, 0).unwrap().unwrap();
        let sidecar = root.join("flat.chests.json");
        let world_file = root.join("flat.irw");
        let sources = BTreeMap::from([
            ((0, 0), before_pos.clone()),
            ((-1, 0), b"stale snapshot".to_vec()),
        ]);
        let mut lock = WorldSessionLock::acquire(&root).unwrap();
        assert!(rewrite_regions_with_sidecars_under_lock_expected(
            &mut lock,
            Dimension::Overworld,
            &[((0, 0), &nbt_pos), ((-1, 0), &nbt_neg)],
            &sources,
            &world_file,
            &[(sidecar.as_path(), b"{}".to_vec())],
        )
        .is_err());
        assert_eq!(fs::read(&pos).unwrap(), before_pos);
        assert_eq!(fs::read(&neg).unwrap(), before_neg);
        assert!(!sidecar.exists());
        assert!(!world_file.with_extension("pending-txn.json").exists());
        drop(lock);
        fs::remove_dir_all(base).unwrap();
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
        assert!(rewrite_chunk_under_lock(&mut lock, Dimension::Nether, 0, 0, b"invalid").is_err());
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
    #[test]
    fn locked_batch_updates_two_chunks_and_preserves_unmodified_nbt() {
        let root = setup();
        let world = root.join("world");
        let source = world.join("region/r.0.0.mca");
        let mut original = Region::from_stream(File::open(&source).unwrap()).unwrap();
        let untouched = original.read_chunk(0, 1).unwrap().unwrap();
        let mut first =
            legacy_nbt::parse_chunk(&original.read_chunk(0, 0).unwrap().unwrap(), 0, 0).unwrap();
        first
            .level
            .extra
            .insert("UnknownBatchA".into(), Value::String("retained-a".into()));
        let mut second =
            legacy_nbt::parse_chunk(&original.read_chunk(1, 0).unwrap().unwrap(), 1, 0).unwrap();
        second
            .level
            .extra
            .insert("UnknownBatchB".into(), Value::String("retained-b".into()));
        let a = fastnbt::to_bytes(&first).unwrap();
        let b = fastnbt::to_bytes(&second).unwrap();
        let mut lock = WorldSessionLock::acquire(&world).unwrap();
        rewrite_chunks_under_lock(
            &mut lock,
            Dimension::Overworld,
            &[((0, 0), &a), ((1, 0), &b)],
        )
        .unwrap();
        let mut actual = Region::from_stream(File::open(&source).unwrap()).unwrap();
        assert_eq!(actual.read_chunk(0, 0).unwrap().unwrap(), a);
        assert_eq!(actual.read_chunk(1, 0).unwrap().unwrap(), b);
        assert_eq!(actual.read_chunk(0, 1).unwrap().unwrap(), untouched);
        assert!(lock.verify().is_ok());
        drop(lock);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn batch_preflight_rejects_cross_region_duplicate_and_bad_nbt() {
        let root = setup();
        let source = root.join("world/region/r.0.0.mca");
        let dest = root.join("out/r.0.0.mca");
        let before = fs::read(&source).unwrap();
        let mut input = Region::from_stream(File::open(&source).unwrap()).unwrap();
        let a = input.read_chunk(0, 0).unwrap().unwrap();
        assert!(rewrite_chunks_create_only(&source, &dest, &[]).is_err());
        assert!(
            rewrite_chunks_create_only(&source, &dest, &[((0, 0), &a), ((32, 0), &a)],).is_err()
        );
        assert!(
            rewrite_chunks_create_only(&source, &dest, &[((0, 0), &a), ((0, 0), &a)],).is_err()
        );
        assert!(rewrite_chunks_create_only(
            &source,
            &dest,
            &[((0, 0), &a), ((1, 0), b"invalid NBT")],
        )
        .is_err());
        assert_eq!(fs::read(&source).unwrap(), before);
        assert!(!dest.exists());
        assert!(!dest.with_extension("creating").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
