//! Legacy Minecraft Java 1.7.10 Anvil chunk NBT model.
//! This is read-only production validation, not general vanilla Anvil import.
//! #[serde(flatten)] preserves unknown named tags for eventual safe roundtrips.
use fastnbt::{ByteArray, IntArray, Value};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, ErrorKind};

const MAX_NBT_BYTES: usize = 2 * 1024 * 1024;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

fn nbt_error(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, error.to_string())
}

#[derive(Serialize, Deserialize)]
pub(crate) struct ChunkRoot {
    #[serde(rename = "Level")]
    pub level: ChunkLevel,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct ChunkLevel {
    #[serde(rename = "xPos")]
    pub x: i32,
    #[serde(rename = "zPos")]
    pub z: i32,
    #[serde(rename = "Sections")]
    pub sections: Vec<Section>,
    #[serde(rename = "Biomes", default)]
    pub biomes: Option<ByteArray>,
    #[serde(rename = "HeightMap", default)]
    pub height_map: Option<IntArray>,
    #[serde(rename = "Entities", default)]
    pub entities: Vec<Value>,
    #[serde(rename = "TileEntities", default)]
    pub tile_entities: Vec<Value>,
    #[serde(rename = "TileTicks", default)]
    pub tile_ticks: Vec<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Section {
    #[serde(rename = "Y")]
    pub y: i8,
    #[serde(rename = "Blocks")]
    pub blocks: ByteArray,
    #[serde(rename = "Data")]
    pub metadata: ByteArray,
    #[serde(rename = "Add", default)]
    pub add: Option<ByteArray>,
    #[serde(rename = "BlockLight")]
    pub block_light: ByteArray,
    #[serde(rename = "SkyLight")]
    pub sky_light: ByteArray,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Section {
    /// Block index is YZX: y * 256 + z * 16 + x. Result is numeric
    /// block ID (12 bits max) and pre-1.13 metadata (4 bits).
    pub fn block_at(&self, index: usize) -> Option<(u16, u8)> {
        if index >= 4096 {
            return None;
        }
        let low = u16::from(*self.blocks.get(index)? as u8);
        let pair = index / 2;
        let shift = (index & 1) * 4;
        let metadata = ((*self.metadata.get(pair)? as u8) >> shift) & 15;
        let high = self
            .add
            .as_ref()
            .map(|layer| ((layer[pair] as u8 >> shift) & 15) as u16)
            .unwrap_or(0);
        Some((low | high << 8, metadata))
    }
}

/// Read-only parse. Unknown tags and entity payloads remain present in the
/// typed tree but callers MUST NOT write this back until FS08 approval.
pub(crate) fn parse_chunk(nbt: &[u8], cx: i32, cz: i32) -> io::Result<ChunkRoot> {
    if nbt.len() > MAX_NBT_BYTES {
        return Err(invalid("legacy chunk NBT exceeds size bound"));
    }
    let parsed: ChunkRoot = fastnbt::from_bytes(nbt).map_err(nbt_error)?;
    if (parsed.level.x, parsed.level.z) != (cx, cz) {
        return Err(invalid("legacy chunk position mismatch"));
    }
    if parsed.level.sections.len() > 16 {
        return Err(invalid("legacy chunk has too many sections"));
    }
    if parsed.level.biomes.as_ref().is_some_and(|a| a.len() != 256)
        || parsed
            .level
            .height_map
            .as_ref()
            .is_some_and(|a| a.len() != 256)
    {
        return Err(invalid("invalid legacy chunk biomes or heightmap"));
    }
    let mut ys = BTreeSet::new();
    for section in &parsed.level.sections {
        if !(0..16).contains(&section.y) || !ys.insert(section.y) {
            return Err(invalid("invalid or duplicate legacy section Y"));
        }
        if section.blocks.len() != 4096
            || section.metadata.len() != 2048
            || section.add.as_ref().is_some_and(|a| a.len() != 2048)
            || section.block_light.len() != 2048
            || section.sky_light.len() != 2048
        {
            return Err(invalid("invalid legacy section array length"));
        }
        // Once dimensions are validated, both byte/nibble arrays can be
        // accessed without panicking, including optional 12-bit block IDs.
        if section.block_at(0).is_none() || section.block_at(4095).is_none() {
            return Err(invalid("invalid legacy section block"));
        }
    }
    crate::legacy_inventory_nbt::validate_tiles(&parsed.level.tile_entities, Some((cx, cz)))?;
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ChunkRoot {
        let mut blocks = vec![0i8; 4096];
        blocks[0] = 1;
        let mut data = vec![0i8; 2048];
        data[0] = 10;
        let mut add = vec![0i8; 2048];
        add[0] = 1;
        let mut extra = BTreeMap::new();
        extra.insert("IndustrialUnknown".into(), Value::String("preserve".into()));
        ChunkRoot {
            level: ChunkLevel {
                x: -1,
                z: 2,
                sections: vec![Section {
                    y: 15,
                    blocks: ByteArray::new(blocks),
                    metadata: ByteArray::new(data),
                    add: Some(ByteArray::new(add)),
                    block_light: ByteArray::new(vec![0; 2048]),
                    sky_light: ByteArray::new(vec![-1; 2048]),
                    extra: BTreeMap::new(),
                }],
                biomes: Some(ByteArray::new(vec![1; 256])),
                height_map: Some(IntArray::new(vec![255; 256])),
                entities: Vec::new(),
                tile_entities: Vec::new(),
                tile_ticks: Vec::new(),
                extra: BTreeMap::new(),
            },
            extra,
        }
    }

    #[test]
    fn preserves_unknown_tags_and_add_nibbles_in_roundtrip() {
        let bytes = fastnbt::to_bytes(&sample()).unwrap();
        let chunk = parse_chunk(&bytes, -1, 2).unwrap();
        assert_eq!(chunk.level.sections[0].block_at(0), Some((257, 10)));
        assert_eq!(chunk.level.sections[0].block_at(1), Some((0, 0)));
        assert_eq!(chunk.level.sections[0].block_at(4096), None);
        let emitted = fastnbt::to_bytes(&chunk).unwrap();
        let restored = parse_chunk(&emitted, -1, 2).unwrap();
        assert!(matches!(
            restored.extra.get("IndustrialUnknown"),
            Some(Value::String(s)) if s == "preserve"
        ));
        assert_eq!(restored.level.sections[0].block_at(0), Some((257, 10)));
    }

    #[test]
    fn rejects_out_of_range_sections_and_invalid_nibble_arrays() {
        let mut bad = sample();
        bad.level.sections[0].metadata = ByteArray::new(vec![0; 8]);
        assert!(parse_chunk(&fastnbt::to_bytes(&bad).unwrap(), -1, 2).is_err());
        let mut bad = sample();
        bad.level.sections.push(sample().level.sections.remove(0));
        assert!(parse_chunk(&fastnbt::to_bytes(&bad).unwrap(), -1, 2).is_err());
        let mut bad = sample();
        bad.level.sections[0].y = -1;
        assert!(parse_chunk(&fastnbt::to_bytes(&bad).unwrap(), -1, 2).is_err());
        assert!(parse_chunk(&fastnbt::to_bytes(&sample()).unwrap(), 2, -1).is_err());
    }

    #[test]
    fn chunk_tile_entities_must_belong_to_the_enclosing_column() {
        fn tile(x: i32, z: i32) -> Value {
            let item = crate::legacy_inventory_nbt::TileEntity {
                id: "Chest".to_string(),
                x,
                y: 65,
                z,
                items: None,
                extra: BTreeMap::new(),
            };
            fastnbt::from_bytes(&fastnbt::to_bytes(&item).unwrap()).unwrap()
        }
        let mut chunk = sample(); // chunk (-1, 2): x -16..-1, z 32..47
        chunk.level.tile_entities = vec![tile(-1, 32)];
        assert!(parse_chunk(&fastnbt::to_bytes(&chunk).unwrap(), -1, 2).is_ok());

        chunk.level.tile_entities = vec![tile(0, 32)];
        assert!(parse_chunk(&fastnbt::to_bytes(&chunk).unwrap(), -1, 2).is_err());

        chunk.level.tile_entities = vec![tile(-1, 32), tile(-1, 32)];
        assert!(parse_chunk(&fastnbt::to_bytes(&chunk).unwrap(), -1, 2).is_err());
    }
}
