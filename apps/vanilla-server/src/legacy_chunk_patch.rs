//! E46: bounded projection of explicit block updates into one EXISTING
//! Minecraft 1.7.10 Anvil chunk. Unmentioned NBT and block positions remain
//! unchanged. This is NOT a standalone live Anvil world backend.
use crate::legacy_nbt::{self, Section};
use fastnbt::{ByteArray, IntArray, Value};
use std::collections::BTreeSet;
use std::io::{self, ErrorKind};

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

/// Absolute block coordinates and the Protocol 5 packed (id << 4) | metadata.
pub(crate) type BlockPatch = (i32, i32, i32, u16);

pub(crate) fn patch_chunk(
    original: &[u8],
    cx: i32,
    cz: i32,
    changes: &[BlockPatch],
) -> io::Result<Vec<u8>> {
    if changes.is_empty() || changes.len() > 4096 {
        return Err(invalid("empty or oversized Anvil block update batch"));
    }
    let mut chunk = legacy_nbt::parse_chunk(original, cx, cz)?;
    let mut seen = BTreeSet::new();
    let mut changed_columns = BTreeSet::new();
    for &(x, y, z, block) in changes {
        if x.div_euclid(16) != cx
            || z.div_euclid(16) != cz
            || !(0..256).contains(&y)
            || block >> 4 > 255
            || !seen.insert((x, y, z))
        {
            return Err(invalid("invalid or duplicate Anvil block update"));
        }
        // Block IDs larger than 255 belong to a different, possibly modded,
        // palette and must never be truncated by the IRW numeric block model.
        let local_x = x.rem_euclid(16) as usize;
        let local_z = z.rem_euclid(16) as usize;
        let section_y = (y / 16) as i8;
        let index = ((y as usize % 16) * 256) + local_z * 16 + local_x;
        let slot = chunk.level.sections.iter().position(|section| section.y == section_y);
        let old_id = slot
            .and_then(|position| chunk.level.sections[position].block_at(index))
            .map_or(0, |(id, _)| id);
        if old_id > 255 {
            return Err(invalid("refusing to overwrite extended Anvil block ID"));
        }
        // Without a full TileEntity state transition, changing its block
        // would create an orphaned inventory or silently erase its contents.
        for tile in &chunk.level.tile_entities {
            if let Value::Compound(fields) = tile {
                let position = (fields.get("x"), fields.get("y"), fields.get("z"));
                if let (Some(Value::Int(tx)), Some(Value::Int(ty)), Some(Value::Int(tz))) =
                    position
                {
                    if (*tx, *ty, *tz) == (x, y, z) {
                        return Err(invalid("block update overlaps existing TileEntity"));
                    }
                }
            }
        }
        let position = match slot {
            Some(position) => position,
            None if block == 0 => {
                changed_columns.insert((local_x, local_z));
                continue;
            }
            None => {
                chunk.level.sections.push(Section {
                    y: section_y,
                    blocks: ByteArray::new(vec![0; 4096]),
                    metadata: ByteArray::new(vec![0; 2048]),
                    add: None,
                    block_light: ByteArray::new(vec![0; 2048]),
                    sky_light: ByteArray::new(vec![-1; 2048]),
                    extra: Default::default(),
                });
                chunk.level.sections.len() - 1
            }
        };
        let section = &mut chunk.level.sections[position];
        let mut blocks: Vec<i8> = section.blocks.iter().copied().collect();
        blocks[index] = (block >> 4) as i8;
        section.blocks = ByteArray::new(blocks);
        let nibble_index = index / 2;
        let shift = (index & 1) * 4;
        let mask = !(15u8 << shift);
        let mut metadata: Vec<i8> = section.metadata.iter().copied().collect();
        metadata[nibble_index] =
            (((metadata[nibble_index] as u8) & mask) | (((block & 15) as u8) << shift)) as i8;
        section.metadata = ByteArray::new(metadata);
        if let Some(high) = section.add.take() {
            let mut add: Vec<i8> = high.iter().copied().collect();
            add[nibble_index] = ((add[nibble_index] as u8) & mask) as i8;
            section.add = Some(ByteArray::new(add));
        }
        changed_columns.insert((local_x, local_z));
    }
    if let Some(heights) = chunk.level.height_map.as_ref() {
        let mut updated: Vec<i32> = heights.iter().copied().collect();
        for (x, z) in changed_columns {
            let mut top = 0;
            'scan: for y in (0..256usize).rev() {
                if let Some(section) = chunk
                    .level
                    .sections
                    .iter()
                    .find(|section| section.y == (y / 16) as i8)
                {
                    let index = (y % 16) * 256 + z * 16 + x;
                    if section.block_at(index).is_some_and(|(id, _)| id != 0) {
                        top = (y + 1) as i32;
                        break 'scan;
                    }
                }
            }
            updated[z * 16 + x] = top;
        }
        chunk.level.height_map = Some(IntArray::new(updated));
    }
    // Request relighting by a later world-lighting pass. This projection
    // does not claim to recompute block/sky light for neighboring chunks.
    chunk
        .level
        .extra
        .insert("LightPopulated".to_owned(), Value::Byte(0));
    let encoded = fastnbt::to_bytes(&chunk)
        .map_err(|error| io::Error::new(ErrorKind::InvalidData, error.to_string()))?;
    legacy_nbt::parse_chunk(&encoded, cx, cz)?;
    Ok(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{anvil_export, world::World};
    use fastanvil::Region;
    use std::{fs, fs::File};

    fn source_chunk() -> (std::path::PathBuf, Vec<u8>) {
        let base = std::env::temp_dir().join(format!(
            "ir1710-block-patch-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        anvil_export::export(&World::new(), &base.join("world")).unwrap();
        let mut region =
            Region::from_stream(File::open(base.join("world/region/r.0.0.mca")).unwrap()).unwrap();
        (base, region.read_chunk(0, 0).unwrap().unwrap())
    }

    #[test]
    fn blocks_metadata_heightmap_and_unknown_nbt_are_preserved() {
        let (base, bytes) = source_chunk();
        let mut chunk = legacy_nbt::parse_chunk(&bytes, 0, 0).unwrap();
        chunk.level.extra.insert(
            "ModdedTag".into(),
            Value::String("do not drop".into()),
        );
        let bytes = fastnbt::to_bytes(&chunk).unwrap();
        let changed = patch_chunk(&bytes, 0, 0, &[(2, 90, 3, (5 << 4) | 7)]).unwrap();
        let reread = legacy_nbt::parse_chunk(&changed, 0, 0).unwrap();
        let section = reread.level.sections.iter().find(|s| s.y == 5).unwrap();
        let idx = (90 % 16) * 256 + 3 * 16 + 2;
        assert_eq!(section.block_at(idx), Some((5, 7)));
        assert_eq!(
            reread.level.extra.get("ModdedTag"),
            Some(&Value::String("do not drop".into()))
        );
        assert_eq!(reread.level.extra.get("LightPopulated"), Some(&Value::Byte(0)));
        assert_eq!(reread.level.height_map.as_ref().unwrap()[3 * 16 + 2], 91);
        assert_eq!(
            reread.level.sections.iter().find(|s| s.y == 3).unwrap().block_at(0),
            chunk.level.sections.iter().find(|s| s.y == 3).unwrap().block_at(0)
        );
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn invalid_and_duplicate_block_edits_never_return_nbt() {
        let (base, nbt) = source_chunk();
        assert!(patch_chunk(&nbt, 0, 0, &[(16, 70, 0, 1)]).is_err());
        assert!(patch_chunk(&nbt, 0, 0, &[(0, 256, 0, 1)]).is_err());
        assert!(patch_chunk(&nbt, 0, 0, &[(0, 70, 0, 4096)]).is_err());
        assert!(patch_chunk(&nbt, 0, 0, &[(0, 70, 0, 16), (0, 70, 0, 0)]).is_err());
        assert!(patch_chunk(&nbt, 0, 0, &[]).is_err());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn extended_block_ids_are_never_truncated() {
        let (base, nbt) = source_chunk();
        let mut chunk = legacy_nbt::parse_chunk(&nbt, 0, 0).unwrap();
        let section = chunk.level.sections.iter_mut().find(|s| s.y == 3).unwrap();
        section.add = Some(ByteArray::new(vec![0; 2048]));
        let mut high = vec![0i8; 2048];
        high[0] = 1;
        section.add = Some(ByteArray::new(high));
        let raw = fastnbt::to_bytes(&chunk).unwrap();
        assert!(patch_chunk(&raw, 0, 0, &[(0, 48, 0, 16)]).is_err());
        fs::remove_dir_all(base).unwrap();
    }
}
