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

/// Container depth fastnbt's recursive decoder is allowed to walk. NBT is
/// untrusted; deep nesting must be rejected *before* the recursive decode so
/// hostile input cannot overflow the stack (STATUS_STACK_OVERFLOW on Windows
/// is an uncatchable process abort, not a catchable panic).
const MAX_NBT_DEPTH: usize = 512;

#[derive(Clone, Copy)]
enum Frame {
    Compound,
    List { kind: u8, remaining: u32 },
}

/// Iterative (never recursive) NBT structure walk: rejects truncation,
/// unknown tags, oversized declared lengths and nesting beyond
/// `MAX_NBT_DEPTH`, and always terminates in time linear in the input.
/// Callers feed only its output to fastnbt's recursive decoder.
pub(crate) fn bounded_nbt(bytes: &[u8]) -> io::Result<()> {
    fn take(bytes: &[u8], pos: &mut usize, n: usize) -> io::Result<()> {
        let end = pos
            .checked_add(n)
            .ok_or_else(|| invalid("nbt length overflow"))?;
        if end > bytes.len() {
            return Err(invalid("truncated nbt"));
        }
        *pos = end;
        Ok(())
    }
    fn byte(bytes: &[u8], pos: &mut usize) -> io::Result<u8> {
        let value = *bytes
            .get(*pos)
            .ok_or_else(|| invalid("truncated nbt header"))?;
        *pos += 1;
        Ok(value)
    }
    fn u16v(bytes: &[u8], pos: &mut usize) -> io::Result<u16> {
        take(bytes, pos, 2)?;
        Ok(u16::from_be_bytes([bytes[*pos - 2], bytes[*pos - 1]]))
    }
    fn u32v(bytes: &[u8], pos: &mut usize) -> io::Result<u32> {
        take(bytes, pos, 4)?;
        Ok(u32::from_be_bytes([
            bytes[*pos - 4],
            bytes[*pos - 3],
            bytes[*pos - 2],
            bytes[*pos - 1],
        ]))
    }
    let mut pos = 0usize;
    // Root: a named compound, exactly like every 1.7.10 payload the server
    // parses (chunk Level, playerdata, level.dat, written books, ItemStacks).
    if byte(bytes, &mut pos)? != 10 {
        return Err(invalid("nbt root must be a compound"));
    }
    let root_name = u16v(bytes, &mut pos)?;
    take(bytes, &mut pos, root_name as usize)?;
    let mut stack: Vec<Frame> = vec![Frame::Compound];
    while let Some(top) = stack.last_mut() {
        let kind = match top {
            Frame::Compound => {
                let kind = byte(bytes, &mut pos)?;
                if kind == 0 {
                    stack.pop(); // TAG_End closes the compound.
                    continue;
                }
                let name = u16v(bytes, &mut pos)?;
                take(bytes, &mut pos, name as usize)?;
                kind
            }
            Frame::List { kind, remaining } => {
                if *remaining == 0 {
                    stack.pop();
                    continue;
                }
                *remaining -= 1;
                *kind
            }
        };
        let child = match kind {
            1 => {
                take(bytes, &mut pos, 1)?;
                None
            }
            2 => {
                take(bytes, &mut pos, 2)?;
                None
            }
            3 | 5 => {
                take(bytes, &mut pos, 4)?;
                None
            }
            4 | 6 => {
                take(bytes, &mut pos, 8)?;
                None
            }
            7 => {
                let count = u32v(bytes, &mut pos)?;
                take(bytes, &mut pos, count as usize)?;
                None
            }
            8 => {
                let count = u16v(bytes, &mut pos)?;
                take(bytes, &mut pos, count as usize)?;
                None
            }
            11 => {
                let count = u32v(bytes, &mut pos)?;
                take(
                    bytes,
                    &mut pos,
                    count
                        .checked_mul(4)
                        .ok_or_else(|| invalid("nbt length overflow"))?
                        as usize,
                )?;
                None
            }
            12 => {
                let count = u32v(bytes, &mut pos)?;
                take(
                    bytes,
                    &mut pos,
                    count
                        .checked_mul(8)
                        .ok_or_else(|| invalid("nbt length overflow"))?
                        as usize,
                )?;
                None
            }
            9 => Some(Frame::List {
                kind: byte(bytes, &mut pos)?,
                remaining: u32v(bytes, &mut pos)?,
            }),
            10 => Some(Frame::Compound),
            _ => return Err(invalid("unknown nbt tag")),
        };
        if let Some(child) = child {
            if stack.len() >= MAX_NBT_DEPTH {
                return Err(invalid("nbt nesting limit"));
            }
            stack.push(child);
        }
    }
    if pos != bytes.len() {
        return Err(invalid("trailing nbt bytes"));
    }
    Ok(())
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
        // block_at is also called by future readers that may inspect a
        // detached, not-yet-validated section. Never panic for short Add.
        let high = match self.add.as_ref() {
            Some(layer) => u16::from((*layer.get(pair)? as u8 >> shift) & 15),
            None => 0,
        };
        Some((low | high << 8, metadata))
    }
}

/// Read-only parse. Unknown tags and entity payloads remain present in the
/// typed tree but callers MUST NOT write this back until FS08 approval.
pub(crate) fn parse_chunk(nbt: &[u8], cx: i32, cz: i32) -> io::Result<ChunkRoot> {
    if nbt.len() > MAX_NBT_BYTES {
        return Err(invalid("legacy chunk NBT exceeds size bound"));
    }
    // Iterative structural bound FIRST: fastnbt's decoder is recursive and
    // deep nesting would abort the process instead of returning an error.
    bounded_nbt(nbt)?;
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
    crate::legacy_entity_nbt::validate_entities(&parsed.level.entities)?;
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADD-085 / #510: minimized fuzz seeds for the anvil_nbt target.
    /// fastnbt's decoder is recursive: deep nesting aborted the WHOLE
    /// process (STATUS_STACK_OVERFLOW on Windows, not a catchable panic)
    /// and hostile array lengths were only bounded by the machine. The
    /// iterative `bounded_nbt` pre-scan must reject every seed cleanly and
    /// the production entry `parse_chunk` must return `Err` for all of them.
    #[test]
    fn anvil_nbt_fuzz_corpus_is_rejected_without_process_abort() {
        for (label, bytes) in [
            (
                "empty",
                include_bytes!("../../../fuzz/corpus/anvil_nbt/empty").as_slice(),
            ),
            (
                "truncated_root",
                include_bytes!("../../../fuzz/corpus/anvil_nbt/truncated_root").as_slice(),
            ),
            (
                "huge_long_array",
                include_bytes!("../../../fuzz/corpus/anvil_nbt/huge_long_array").as_slice(),
            ),
            (
                "negative_list_count",
                include_bytes!("../../../fuzz/corpus/anvil_nbt/negative_list_count").as_slice(),
            ),
            (
                "deep_nesting",
                include_bytes!("../../../fuzz/corpus/anvil_nbt/deep_nesting").as_slice(),
            ),
        ] {
            assert!(
                bounded_nbt(bytes).is_err(),
                "{label}: hostile seed accepted by the pre-scan"
            );
            assert!(
                parse_chunk(bytes, 0, 0).is_err(),
                "{label}: hostile seed accepted by parse_chunk"
            );
        }
    }

    /// Deterministic xorshift sweep over bounded inputs: every seed must
    /// return Ok or a clean Err from BOTH the pre-scan and parse_chunk
    /// without panicking (a panic fails the harness), and a valid minimal
    /// document must still pass the pre-scan.
    #[test]
    fn random_nbt_sweep_never_panics_and_stays_bounded() {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        for seed in 0..4096u32 {
            let length = (seed % 512) as usize + 1;
            let mut body = Vec::with_capacity(length);
            for _ in 0..length {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                body.push(state as u8);
            }
            let _ = bounded_nbt(&body);
            let _ = parse_chunk(&body, 0, 0);
        }
        // root compound "level" {} still parses through the pre-scan.
        assert!(bounded_nbt(b"\x0A\x00\x05level\x00").is_ok());
    }

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
    fn truncated_optional_add_returns_none_instead_of_panicking() {
        let mut section = sample().level.sections.remove(0);
        section.add = Some(ByteArray::new(vec![]));
        assert_eq!(section.block_at(0), None);
        section.add = Some(ByteArray::new(vec![0]));
        assert_eq!(section.block_at(4095), None);
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
