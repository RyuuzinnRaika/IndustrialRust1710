//! Bounded read-only Anvil region inspector for Java Edition 1.7.10.
//! Does not import, export, rewrite, or publish world data.
use flate2::read::{GzDecoder, ZlibDecoder};
use std::fs;
use std::io::{self, ErrorKind, Read};
use std::path::Path;

const SECTOR_SIZE: usize = 4096;
const MAX_REGION_SIZE: u64 = 128 * 1024 * 1024;
const MAX_CHUNK_NBT: u64 = 2 * 1024 * 1024;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

/// Parse only a canonical r.X.Z.mca filename. Region coordinates may be negative.
fn coordinates(file: &Path) -> io::Result<(i32, i32)> {
    let name = file
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("invalid Anvil region file name"))?;
    let parts: Vec<_> = name.split('.').collect();
    if parts.len() != 4 || parts[0] != "r" || parts[3] != "mca" {
        return Err(invalid("expected Anvil r.X.Z.mca"));
    }
    let x = parts[1]
        .parse()
        .map_err(|_| invalid("invalid Anvil region X"))?;
    let z = parts[2]
        .parse()
        .map_err(|_| invalid("invalid Anvil region Z"))?;
    Ok((x, z))
}

fn decompress(chunk: &[u8], compression: u8) -> io::Result<Vec<u8>> {
    let mut decoded = Vec::new();
    match compression {
        1 => GzDecoder::new(chunk)
            .take(MAX_CHUNK_NBT + 1)
            .read_to_end(&mut decoded)?,
        2 => ZlibDecoder::new(chunk)
            .take(MAX_CHUNK_NBT + 1)
            .read_to_end(&mut decoded)?,
        _ => return Err(invalid("unsupported 1.7.10 Anvil chunk compression")),
    };
    if decoded.len() as u64 > MAX_CHUNK_NBT {
        return Err(invalid("Anvil chunk expands beyond the NBT size limit"));
    }
    Ok(decoded)
}

/// Check all chunk sector mappings (including unused sectors), their coordinate
/// claims and legacy numeric block/NBT layouts without modifying the input.
pub(crate) fn inspect_region(path: &Path) -> io::Result<usize> {
    let (rx, rz) = coordinates(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_REGION_SIZE {
        return Err(invalid("Anvil region is not a bounded regular file"));
    }
    inspect_region_bytes(path, &fs::read(path)?)
}

/// Validate an in-memory region image *before* a transactional journal is
/// published. The exact same sector and NBT checks apply to crash recovery.
pub(crate) fn inspect_region_bytes(path: &Path, raw: &[u8]) -> io::Result<usize> {
    let (rx, rz) = coordinates(path)?;
    if raw.len() < 2 * SECTOR_SIZE
        || raw.len() % SECTOR_SIZE != 0
        || raw.len() as u64 > MAX_REGION_SIZE
    {
        return Err(invalid("Anvil region has invalid sector alignment"));
    }
    let sectors = raw.len() / SECTOR_SIZE;
    let mut occupied = vec![false; sectors];
    occupied[0] = true;
    occupied[1] = true;
    let mut count = 0usize;
    for z in 0..32usize {
        for x in 0..32usize {
            let table_index = (z * 32 + x) * 4;
            let offset = (usize::from(raw[table_index]) << 16)
                | (usize::from(raw[table_index + 1]) << 8)
                | usize::from(raw[table_index + 2]);
            let used = usize::from(raw[table_index + 3]);
            if offset == 0 && used == 0 {
                continue;
            }
            if offset < 2 || used == 0 || offset.checked_add(used).is_none_or(|end| end > sectors) {
                return Err(invalid("invalid Anvil region location entry"));
            }
            for sector in &mut occupied[offset..offset + used] {
                if *sector {
                    return Err(invalid("Anvil region contains overlapping sectors"));
                }
                *sector = true;
            }
            let start = offset * SECTOR_SIZE;
            let size = u32::from_be_bytes(raw[start..start + 4].try_into().unwrap()) as usize;
            if size < 2 || size + 4 > used * SECTOR_SIZE {
                return Err(invalid("Anvil chunk length exceeds its sector allocation"));
            }
            let compression = raw[start + 4];
            let chunk = &raw[start + 5..start + 4 + size];
            let decoded = decompress(chunk, compression)?;
            let cx = rx
                .checked_mul(32)
                .and_then(|v| v.checked_add(x as i32))
                .ok_or_else(|| invalid("Anvil region X overflow"))?;
            let cz = rz
                .checked_mul(32)
                .and_then(|v| v.checked_add(z as i32))
                .ok_or_else(|| invalid("Anvil region Z overflow"))?;
            let _ = crate::legacy_nbt::parse_chunk(&decoded, cx, cz)?;
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{anvil_export, world::World};

    #[test]
    fn reads_own_1710_regions_across_zero_and_negative_indices() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-region-read-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let target = root.join("snapshot");
        anvil_export::export(&World::new(), &target).unwrap();
        assert_eq!(inspect_region(&target.join("region/r.0.0.mca")).unwrap(), 9);
        assert_eq!(
            inspect_region(&target.join("region/r.-1.0.mca")).unwrap(),
            6
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_region_sector_overlap_and_invalid_compression() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-region-corrupt-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let target = root.join("snapshot");
        anvil_export::export(&World::new(), &target).unwrap();
        let region = target.join("region/r.0.0.mca");
        let valid = fs::read(&region).unwrap();

        let mut overlap = valid.clone();
        overlap[4..8].copy_from_slice(&valid[0..4]);
        fs::write(&region, overlap).unwrap();
        assert!(inspect_region(&region).is_err());

        let mut invalid_compression = valid;
        let first_sector = (usize::from(invalid_compression[2]) << 12)
            | (usize::from(invalid_compression[1]) << 20)
            | (usize::from(invalid_compression[0]) << 28);
        invalid_compression[first_sector + 4] = 9;
        fs::write(&region, invalid_compression).unwrap();
        assert!(inspect_region(&region).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
