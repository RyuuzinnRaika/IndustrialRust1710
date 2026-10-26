//! Strict and bounded importer for snapshots created by this server's Anvil
//! exporter. Arbitrary Minecraft/Forge worlds are *not* supported: importing
//! them as sparse flat worlds could silently destroy terrain or tile entities.
//! All bytes are validated before a new IRW1 save can be published.
use crate::world::{self, World};
use fastnbt::ByteArray;
use flate2::{read::GzDecoder, read::ZlibDecoder};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read};
use std::path::Path;

const MAX_REGION_BYTES: u64 = 32 * 1024 * 1024;
const MAX_CHUNK_NBT: u64 = 2 * 1024 * 1024;
const MAX_LEVEL_DAT: u64 = 1024 * 1024;
const MAX_CHUNKS: usize = 256;
const MAX_REGIONS: usize = 256;

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}
fn wrap(err: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, err.to_string())
}

#[derive(Deserialize)]
struct SnapshotLevel {
    #[serde(rename = "Data")]
    data: SnapshotInfo,
}
#[derive(Deserialize)]
struct SnapshotInfo {
    #[serde(rename = "LevelName")]
    name: String,
    #[serde(rename = "generatorName")]
    generator: String,
    #[serde(rename = "version")]
    version: i32,
}
#[derive(Deserialize)]
struct ChunkRoot {
    #[serde(rename = "Level")]
    level: ChunkLevel,
}
#[derive(Deserialize)]
struct ChunkLevel {
    #[serde(rename = "xPos")]
    x: i32,
    #[serde(rename = "zPos")]
    z: i32,
    #[serde(rename = "Sections")]
    sections: Vec<ChunkSection>,
    #[serde(rename = "Entities", default)]
    entities: Vec<fastnbt::Value>,
    #[serde(rename = "TileEntities", default)]
    tile_entities: Vec<fastnbt::Value>,
    #[serde(rename = "TileTicks", default)]
    tile_ticks: Vec<fastnbt::Value>,
}
#[derive(Deserialize)]
struct ChunkSection {
    #[serde(rename = "Y")]
    y: i8,
    #[serde(rename = "Blocks")]
    blocks: ByteArray,
    #[serde(rename = "Data")]
    metadata: ByteArray,
    #[serde(rename = "Add", default)]
    add: Option<ByteArray>,
}

fn bounded_read(path: &Path, max: u64) -> io::Result<Vec<u8>> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_file() || meta.len() > max {
        return Err(invalid("Anvil input file type or size is not supported"));
    }
    let raw = fs::read(path)?;
    if raw.len() as u64 > max {
        return Err(invalid("Anvil input grew beyond its permitted size"));
    }
    Ok(raw)
}
fn bounded_decompress<R: Read>(decoder: R, max: u64) -> io::Result<Vec<u8>> {
    let mut decoded = Vec::new();
    decoder.take(max + 1).read_to_end(&mut decoded)?;
    if decoded.len() as u64 > max {
        return Err(invalid("NBT decompression limit exceeded"));
    }
    Ok(decoded)
}
fn snapshot_marker(source: &Path) -> io::Result<()> {
    let _ = crate::level_dat::read_level_file(&source.join("level.dat"))?;
    let level = bounded_read(&source.join("level.dat"), MAX_LEVEL_DAT)?;
    let bytes = bounded_decompress(GzDecoder::new(&level[..]), MAX_LEVEL_DAT)?;
    let root: SnapshotLevel = fastnbt::from_bytes(&bytes).map_err(wrap)?;
    if root.data.name != "IndustrialRust1710 Export"
        || root.data.generator != "flat"
        || root.data.version != 19133
    {
        return Err(invalid("not an IndustrialRust1710 flat Anvil export"));
    }
    Ok(())
}

fn region_coords(name: &str) -> io::Result<(i32, i32)> {
    let fields: Vec<_> = name.split('.').collect();
    if fields.len() != 4 || fields[0] != "r" || fields[3] != "mca" {
        return Err(invalid("unsupported region filename"));
    }
    let x = fields[1]
        .parse::<i32>()
        .map_err(|_| invalid("invalid region x"))?;
    let z = fields[2]
        .parse::<i32>()
        .map_err(|_| invalid("invalid region z"))?;
    // A region is 32 chunks across. Imported blocks must lie within the
    // server's bounded sparse world.
    let region_limit = world::WORLD_LIMIT / 16 / 32;
    if !(-region_limit..region_limit).contains(&x) || !(-region_limit..region_limit).contains(&z) {
        return Err(invalid("region outside bounded flat world"));
    }
    Ok((x, z))
}

fn decode_chunk(world: &mut World, nbt: &[u8], cx: i32, cz: i32) -> io::Result<()> {
    // Shared Minecraft 1.7.10 legacy NBT validation before the strict,
    // self-export-only IRW import conversion below.
    let _ = crate::legacy_nbt::parse_chunk(nbt, cx, cz)?;
    let root: ChunkRoot = fastnbt::from_bytes(nbt).map_err(wrap)?;
    let chunk = root.level;
    if (chunk.x, chunk.z) != (cx, cz) {
        return Err(invalid(
            "Anvil chunk NBT coordinates do not match region header",
        ));
    }
    if !chunk.entities.is_empty() || !chunk.tile_entities.is_empty() || !chunk.tile_ticks.is_empty()
    {
        return Err(invalid(
            "entities, tile entities or scheduled ticks cannot be imported",
        ));
    }
    if chunk.sections.len() > 16 {
        return Err(invalid("too many Anvil sections"));
    }
    let mut section_ys = BTreeSet::new();
    for section in chunk.sections {
        let sy = i32::from(section.y);
        if !(0..16).contains(&sy) || !section_ys.insert(sy) {
            return Err(invalid("duplicate or invalid Anvil section Y"));
        }
        if section.blocks.len() != 4096 || section.metadata.len() != 2048 || section.add.is_some() {
            return Err(invalid(
                "unsupported Anvil block arrays or Add high-ID tags",
            ));
        }
        for (idx, &raw_id) in section.blocks.iter().enumerate() {
            let id = (raw_id as u8) as u16;
            let nibble = section.metadata[idx / 2] as u8;
            let meta = if idx & 1 == 0 {
                nibble & 15
            } else {
                nibble >> 4
            };
            if (id == 0 && meta != 0) || (id != 0 && !world::is_placeable(id)) {
                return Err(invalid(
                    "unsupported block ID or metadata in Anvil snapshot",
                ));
            }
            let y = sy * 16 + (idx / 256) as i32;
            let z = cz * 16 + ((idx / 16) % 16) as i32;
            let x = cx * 16 + (idx % 16) as i32;
            let block = (id << 4) | u16::from(meta);
            world.import_block(x, y, z, block)?;
        }
    }
    // This world assumes bedrock/dirt/grass at 60..63. Without section 3,
    // "missing blocks" would be mistaken for its flat baseline.
    if !section_ys.contains(&3) {
        return Err(invalid(
            "Anvil chunk is missing the mandatory flat ground section",
        ));
    }
    Ok(())
}

/// Read a region's location table and bounded zlib chunk bodies, rejecting
/// overlapping sectors, forged NBT coordinates and decompression bombs.
fn read_region(
    world: &mut World,
    path: &Path,
    rx: i32,
    rz: i32,
    total: &mut usize,
) -> io::Result<()> {
    let raw = bounded_read(path, MAX_REGION_BYTES)?;
    if raw.len() < 8192 || raw.len() % 4096 != 0 {
        return Err(invalid("invalid Anvil region sector alignment"));
    }
    let sectors = raw.len() / 4096;
    let mut used = vec![false; sectors];
    used[0] = true;
    used[1] = true;
    for z in 0..32usize {
        for x in 0..32usize {
            let entry = (z * 32 + x) * 4;
            let offset = ((raw[entry] as usize) << 16)
                | ((raw[entry + 1] as usize) << 8)
                | raw[entry + 2] as usize;
            let count = raw[entry + 3] as usize;
            if offset == 0 && count == 0 {
                continue;
            }
            if offset < 2 || count == 0 || offset.checked_add(count).is_none_or(|end| end > sectors)
            {
                return Err(invalid("invalid Anvil location table offset"));
            }
            for sector in &mut used[offset..offset + count] {
                if *sector {
                    return Err(invalid("overlapping Anvil chunk sectors"));
                }
                *sector = true;
            }
            *total += 1;
            if *total > MAX_CHUNKS {
                return Err(invalid("Anvil import exceeds 256-column limit"));
            }
            let begin = offset * 4096;
            let compressed_size =
                u32::from_be_bytes(raw[begin..begin + 4].try_into().unwrap()) as usize;
            if compressed_size < 2 || compressed_size + 4 > count * 4096 || raw[begin + 4] != 2 {
                return Err(invalid("unsupported Anvil chunk length or compression"));
            }
            let body = &raw[begin + 5..begin + 4 + compressed_size];
            let nbt = bounded_decompress(ZlibDecoder::new(body), MAX_CHUNK_NBT)?;
            let cx = rx * 32 + x as i32;
            let cz = rz * 32 + z as i32;
            decode_chunk(world, &nbt, cx, cz)?;
        }
    }
    Ok(())
}

/// Convert ONLY snapshots previously generated with --export-anvil.
/// The destination is created only after the whole input validates. Existing
/// paths are never overwritten, regardless of validation outcome.
pub fn import(source: &Path, destination: &Path) -> io::Result<(usize, usize)> {
    if destination.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "destination IRW1 already exists",
        ));
    }
    if !source.is_dir() {
        return Err(invalid("Anvil source must be a directory"));
    }
    snapshot_marker(source)?;
    let mut regions = Vec::new();
    for entry in fs::read_dir(source.join("region"))? {
        let entry = entry?;
        let filename = entry.file_name();
        let name = filename.to_string_lossy();
        if !name.ends_with(".mca") {
            continue;
        }
        let (x, z) = region_coords(&name)?;
        regions.push((x, z, entry.path()));
        if regions.len() > MAX_REGIONS {
            return Err(invalid("too many Anvil region files"));
        }
    }
    if regions.is_empty() {
        return Err(invalid("Anvil snapshot contains no region files"));
    }
    regions.sort_by_key(|(x, z, _)| (*x, *z));
    let mut world = World::new();
    let mut chunks = 0usize;
    for (x, z, file) in regions {
        read_region(&mut world, &file, x, z, &mut chunks)?;
    }
    // Prevent declaring an empty or partial source to be an imported world.
    if chunks < 25 {
        return Err(invalid("Anvil snapshot has fewer than 25 spawn columns"));
    }
    let edited = world.edited_chunks().len();
    world.save_new(destination)?;
    Ok((chunks, edited))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anvil_export::export;

    #[test]
    fn roundtrip_and_no_overwrite() {
        let root = std::env::temp_dir().join(format!("ir1710-anvil-import-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let src = root.join("source.irw");
        let mut world = World::new();
        for (x, y, z, b) in [
            (-49, 145, 19, (4 << 4) | 2),
            (0, 63, 0, 0),
            (60, 78, -42, 5 << 4),
            // A valid 1.7.10 trapped chest must survive the strict self-export roundtrip.
            (1, 64, 1, (146 << 4) | 3),
        ] {
            world.update_and_save(&src, x, y, z, b).unwrap();
        }
        let source_bytes = fs::read(&src).unwrap();
        let snap = root.join("snapshot");
        assert_eq!(export(&world, &snap).unwrap(), 27);
        let target = root.join("recovered.irw");
        assert_eq!(import(&snap, &target).unwrap(), (27, 3));
        assert_eq!(fs::read(&target).unwrap(), source_bytes);
        assert_eq!(World::load(&target).unwrap().get(1, 64, 1), (146 << 4) | 3);
        assert_eq!(fs::read(&src).unwrap(), source_bytes);
        assert!(import(&snap, &target).is_err());
        assert_eq!(fs::read(&target).unwrap(), source_bytes);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_region_must_not_create_target() {
        let root = std::env::temp_dir().join(format!("ir1710-anvil-tamper-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let snap = root.join("snapshot");
        export(&World::new(), &snap).unwrap();
        let region = snap.join("region/r.0.0.mca");
        let mut content = fs::read(&region).unwrap();
        content[0] = 255; // Offset points beyond region's end.
        fs::write(&region, content).unwrap();
        let dest = root.join("recovered.irw");
        assert!(import(&snap, &dest).is_err());
        assert!(!dest.exists());
        fs::remove_dir_all(root).unwrap();
    }
}
