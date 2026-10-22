//! Bounded read-only per-chunk gateway for 1.7.10 Anvil worlds.
//! This is an FS07 building block, not a replacement for the live IRW World.
//! Never writes to .mca, playerdata, session.lock, or level.dat.
use crate::dimensions::Dimension;
use crate::legacy_nbt::ChunkRoot;
use flate2::read::{GzDecoder, ZlibDecoder};
use std::collections::{HashMap, VecDeque};
use std::fs::{self, File};
use std::io::{self, ErrorKind, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const SECTOR_BYTES: usize = 4096;
const MAX_REGION_BYTES: u64 = 128 * 1024 * 1024;
const MAX_CHUNK_NBT: u64 = 2 * 1024 * 1024;
const MAX_CACHE_CHUNKS: usize = 256;

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, reason)
}

fn checked_directory(path: &Path, required: bool) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_dir() => Ok(true),
        Ok(_) => Err(invalid("Anvil chunk path contains a non-directory")),
        Err(err) if err.kind() == ErrorKind::NotFound && !required => Ok(false),
        Err(err) => Err(err),
    }
}

/// Validate all 1024 entries before trusting any allocation. Checking only
/// the requested chunk misses a second entry aliasing the same disk sectors,
/// which could otherwise return another chunk's NBT as a legitimate result.
fn validate_locations(locations: &[u8; SECTOR_BYTES], sectors: usize) -> io::Result<()> {
    let mut occupied = vec![false; sectors];
    occupied[0] = true; // Location table
    occupied[1] = true; // Timestamp table
    for entry in locations.chunks_exact(4) {
        let offset = (usize::from(entry[0]) << 16)
            | (usize::from(entry[1]) << 8)
            | usize::from(entry[2]);
        let count = usize::from(entry[3]);
        if offset == 0 && count == 0 {
            continue;
        }
        if offset < 2 || count == 0 || offset.checked_add(count).is_none_or(|end| end > sectors) {
            return Err(invalid("invalid Anvil location-table sector bounds"));
        }
        for sector in &mut occupied[offset..offset + count] {
            if *sector {
                return Err(invalid("overlapping Anvil chunk allocations"));
            }
            *sector = true;
        }
    }
    Ok(())
}

fn read_chunk_file(path: &Path, cx: i32, cz: i32) -> io::Result<Option<ChunkRoot>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    if !meta.file_type().is_file()
        || meta.len() < (2 * SECTOR_BYTES) as u64
        || meta.len() > MAX_REGION_BYTES
        || meta.len() % SECTOR_BYTES as u64 != 0
    {
        return Err(invalid("invalid or oversized Anvil region file"));
    }
    let mut file = File::open(path)?;
    let mut locations = [0u8; SECTOR_BYTES];
    file.read_exact(&mut locations)?;
    validate_locations(&locations, meta.len() as usize / SECTOR_BYTES)?;
    let local_x = cx.rem_euclid(32) as usize;
    let local_z = cz.rem_euclid(32) as usize;
    let idx = (local_z * 32 + local_x) * 4;
    let sector_offset = (usize::from(locations[idx]) << 16)
        | (usize::from(locations[idx + 1]) << 8)
        | usize::from(locations[idx + 2]);
    let sector_count = usize::from(locations[idx + 3]);
    if sector_offset == 0 && sector_count == 0 {
        return Ok(None);
    }
    if sector_offset < 2
        || sector_count == 0
        || sector_offset
            .checked_add(sector_count)
            .is_none_or(|end| end > meta.len() as usize / SECTOR_BYTES)
    {
        return Err(invalid("invalid Anvil chunk sector allocation"));
    }
    let start = sector_offset * SECTOR_BYTES;
    file.seek(SeekFrom::Start(start as u64))?;
    let mut bytes = vec![0u8; sector_count * SECTOR_BYTES];
    file.read_exact(&mut bytes)?;
    let payload_len = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
    if payload_len < 2 || payload_len + 4 > bytes.len() {
        return Err(invalid("Anvil compressed chunk exceeds allocated sectors"));
    }
    let compressed = &bytes[5..payload_len + 4];
    let mut decoded = Vec::new();
    match bytes[4] {
        1 => GzDecoder::new(compressed)
            .take(MAX_CHUNK_NBT + 1)
            .read_to_end(&mut decoded)?,
        2 => ZlibDecoder::new(compressed)
            .take(MAX_CHUNK_NBT + 1)
            .read_to_end(&mut decoded)?,
        _ => return Err(invalid("unknown Anvil compression type")),
    };
    if decoded.len() as u64 > MAX_CHUNK_NBT {
        return Err(invalid("Anvil chunk NBT exceeds decompression limit"));
    }
    crate::legacy_nbt::parse_chunk(&decoded, cx, cz).map(Some)
}

type ChunkKey = (Dimension, i32, i32);

pub(crate) struct ReadOnlyChunkCache {
    root: PathBuf,
    capacity: usize,
    entries: HashMap<ChunkKey, Arc<ChunkRoot>>,
    order: VecDeque<ChunkKey>,
}

impl ReadOnlyChunkCache {
    pub fn open(root: impl AsRef<Path>, capacity: usize) -> io::Result<Self> {
        let root = root.as_ref();
        if !(1..=MAX_CACHE_CHUNKS).contains(&capacity) {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "invalid bounded Anvil chunk-cache capacity",
            ));
        }
        checked_directory(root, true)?;
        crate::level_dat::read_level_file(&root.join("level.dat"))?;
        Ok(Self {
            root: root.to_path_buf(),
            capacity,
            entries: HashMap::new(),
            order: VecDeque::new(),
        })
    }

    fn touch(&mut self, key: ChunkKey) {
        self.order.retain(|entry| *entry != key);
        self.order.push_back(key);
    }

    pub fn get(
        &mut self,
        dimension: Dimension,
        cx: i32,
        cz: i32,
    ) -> io::Result<Option<Arc<ChunkRoot>>> {
        let key = (dimension, cx, cz);
        if let Some(hit) = self.entries.get(&key).cloned() {
            self.touch(key);
            return Ok(Some(hit));
        }
        let region_root = match dimension {
            Dimension::Overworld => self.root.clone(),
            Dimension::Nether => self.root.join("DIM-1"),
            Dimension::End => self.root.join("DIM1"),
        };
        if !checked_directory(&region_root, dimension == Dimension::Overworld)? {
            return Ok(None);
        }
        let region_dir = region_root.join("region");
        if !checked_directory(&region_dir, false)? {
            return Ok(None);
        }
        let filename = format!("r.{}.{}.mca", cx.div_euclid(32), cz.div_euclid(32));
        let parsed = match read_chunk_file(&region_dir.join(filename), cx, cz)? {
            Some(parsed) => Arc::new(parsed),
            None => return Ok(None),
        };
        self.entries.insert(key, Arc::clone(&parsed));
        self.touch(key);
        if self.entries.len() > self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
        Ok(Some(parsed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{anvil_export, world::World};

    fn setup(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "ir1710-anvil-chunk-cache-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        anvil_export::export(&World::new(), &root.join("world")).unwrap();
        root
    }

    #[test]
    fn reads_negative_chunk_coordinates_and_misses_without_modifying_disk() {
        let root = setup("negative");
        let world = root.join("world");
        let region = world.join("region/r.-1.0.mca");
        let snapshot = fs::read(&region).unwrap();
        let mut cache = ReadOnlyChunkCache::open(&world, 2).unwrap();
        let chunk = cache.get(Dimension::Overworld, -1, 0).unwrap().unwrap();
        assert_eq!((chunk.level.x, chunk.level.z), (-1, 0));
        let again = cache.get(Dimension::Overworld, -1, 0).unwrap().unwrap();
        assert!(Arc::ptr_eq(&chunk, &again));
        assert!(cache.get(Dimension::Overworld, 100, 100).unwrap().is_none());
        assert_eq!(fs::read(&region).unwrap(), snapshot);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bounded_cache_evicts_oldest_and_separates_dimensions() {
        let root = setup("eviction");
        let world = root.join("world");
        let dim_region = world.join("DIM-1/region");
        fs::create_dir_all(&dim_region).unwrap();
        fs::copy(world.join("region/r.0.0.mca"), dim_region.join("r.0.0.mca")).unwrap();
        let mut cache = ReadOnlyChunkCache::open(&world, 2).unwrap();
        let overworld = cache.get(Dimension::Overworld, 0, 0).unwrap().unwrap();
        let nether = cache.get(Dimension::Nether, 0, 0).unwrap().unwrap();
        assert!(!Arc::ptr_eq(&overworld, &nether));
        let _third = cache.get(Dimension::Overworld, 1, 0).unwrap().unwrap();
        assert_eq!(cache.entries.len(), 2);
        assert!(!cache.entries.contains_key(&(Dimension::Overworld, 0, 0)));
        assert!(ReadOnlyChunkCache::open(&world, 0).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refuses_overlapping_region_allocations_before_reading_chunk() {
        let root = setup("overlap");
        let world = root.join("world");
        let file = world.join("region/r.0.0.mca");
        let mut bytes = fs::read(&file).unwrap();
        // A second chunk claiming the first chunk's sectors must make *all*
        // reads of the region fail, even if the selected entry was unmodified.
        let first = bytes[..4].to_vec();
        assert_ne!(first, [0, 0, 0, 0]);
        bytes[4..8].copy_from_slice(&first);
        fs::write(&file, &bytes).unwrap();
        let mut cache = ReadOnlyChunkCache::open(&world, 2).unwrap();
        assert!(cache.get(Dimension::Overworld, 0, 0).is_err());
        assert_eq!(fs::read(&file).unwrap(), bytes);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupted_chunk_payload_fails_closed() {
        let root = setup("corrupt");
        let world = root.join("world");
        let file = world.join("region/r.0.0.mca");
        let mut bytes = fs::read(&file).unwrap();
        let sector =
            (usize::from(bytes[0]) << 16) | (usize::from(bytes[1]) << 8) | usize::from(bytes[2]);
        bytes[sector * SECTOR_BYTES + 4] = 7;
        fs::write(&file, bytes).unwrap();
        let mut cache = ReadOnlyChunkCache::open(&world, 2).unwrap();
        assert!(cache.get(Dimension::Overworld, 0, 0).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
