//! Read-only Minecraft Java 1.7.10 player, ItemStack and TileEntity NBT.
//! Keep unknown fields: dropping them could destroy vanilla or modded data.
use fastnbt::Value;
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, ErrorKind, Read};
use std::path::Path;

const MAX_PLAYER_FILE: u64 = 1024 * 1024;
const MAX_PLAYER_NBT: usize = 4 * 1024 * 1024;
const MAX_TILES_PER_CHUNK: usize = 4096;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SlotItem {
    #[serde(rename = "Slot")]
    pub slot: i8,
    pub id: i16,
    #[serde(rename = "Count")]
    pub count: i8,
    #[serde(rename = "Damage", default)]
    pub damage: i16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl SlotItem {
    fn validate(&self) -> io::Result<()> {
        if self.id <= 0 || self.count <= 0 || self.count > 64 {
            return Err(invalid("invalid legacy numeric item ID or count"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PlayerData {
    #[serde(rename = "Pos")]
    pub pos: Vec<f64>,
    #[serde(rename = "Motion", default, skip_serializing_if = "Option::is_none")]
    pub motion: Option<Vec<f64>>,
    #[serde(rename = "Rotation", default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<Vec<f32>>,
    #[serde(rename = "Dimension", default)]
    pub dimension: i32,
    #[serde(rename = "Inventory", default)]
    pub inventory: Vec<SlotItem>,
    #[serde(rename = "EnderItems", default)]
    pub ender_items: Vec<SlotItem>,
    #[serde(rename = "Health", default, skip_serializing_if = "Option::is_none")]
    pub health: Option<f32>,
    #[serde(rename = "foodLevel", default, skip_serializing_if = "Option::is_none")]
    pub food_level: Option<i32>,
    #[serde(rename = "XpLevel", default, skip_serializing_if = "Option::is_none")]
    pub xp_level: Option<i32>,
    #[serde(rename = "XpTotal", default, skip_serializing_if = "Option::is_none")]
    pub xp_total: Option<i32>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl PlayerData {
    fn validate(&self) -> io::Result<()> {
        if self.pos.len() != 3
            || !self
                .pos
                .iter()
                .all(|v| v.is_finite() && v.abs() <= 30_000_000.0)
            || self
                .motion
                .as_ref()
                .is_some_and(|v| v.len() != 3 || v.iter().any(|f| !f.is_finite()))
            || self
                .rotation
                .as_ref()
                .is_some_and(|v| v.len() != 2 || v.iter().any(|f| !f.is_finite()))
            || self.health.is_some_and(|v| !v.is_finite())
            || self.inventory.len() > 128
            || self.ender_items.len() > 27
        {
            return Err(invalid("invalid Minecraft 1.7.10 player data"));
        }
        let mut slots = std::collections::BTreeSet::new();
        for item in &self.inventory {
            item.validate()?;
            if !slots.insert(item.slot) {
                return Err(invalid("duplicate player inventory slot"));
            }
        }
        slots.clear();
        for item in &self.ender_items {
            item.validate()?;
            if !slots.insert(item.slot) {
                return Err(invalid("duplicate EnderItems slot"));
            }
        }
        Ok(())
    }
}

/// Load a 1.7.10 playerdata UUID.dat gzip NBT without modifying any file.
pub(crate) fn inspect_player(path: &Path) -> io::Result<(usize, usize)> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_PLAYER_FILE {
        return Err(invalid("invalid playerdata file type or compressed length"));
    }
    let input = fs::File::open(path)?;
    let mut bytes = Vec::new();
    GzDecoder::new(input)
        .take((MAX_PLAYER_NBT + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_PLAYER_NBT {
        return Err(invalid("playerdata decompression exceeds safety limit"));
    }
    let record: PlayerData = fastnbt::from_bytes(&bytes)
        .map_err(|_| invalid("invalid Minecraft 1.7.10 playerdata NBT"))?;
    record.validate()?;
    Ok((record.inventory.len(), record.ender_items.len()))
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct TileEntity {
    pub id: String,
    pub x: i32,
    pub y: i32,
    pub z: i32,
    #[serde(rename = "Items", default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<SlotItem>>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl TileEntity {
    fn validate(&self) -> io::Result<()> {
        if self.id.is_empty()
            || self.id.len() > 256
            || self.y < 0
            || self.y > 255
            || self.x.unsigned_abs() > 30_000_000
            || self.z.unsigned_abs() > 30_000_000
        {
            return Err(invalid("invalid 1.7.10 tile entity identity or position"));
        }
        // Vanilla 1.7.10 container slots have fixed bounds. Unknown/modded
        // tile IDs remain readable without inventing a capacity for them.
        let known_capacity = match self.id.as_str() {
            "Chest" => Some(27),
            "Trap" | "Dispenser" | "Dropper" => Some(9),
            "Furnace" => Some(3),
            "Hopper" => Some(5),
            "BrewingStand" | "Cauldron" => Some(4),
            _ => None,
        };
        if let Some(items) = &self.items {
            if items.len() > 256 {
                return Err(invalid("tile entity contains too many inventory slots"));
            }
            let mut slots = std::collections::BTreeSet::new();
            for item in items {
                item.validate()?;
                if let Some(max) = known_capacity {
                    if item.slot < 0 || item.slot as usize >= max {
                        return Err(invalid("tile entity inventory slot out of range"));
                    }
                }
                if !slots.insert(item.slot) {
                    return Err(invalid("duplicate tile entity inventory slot"));
                }
            }
        }
        Ok(())
    }
}

/// Validate every tile without discarding the original Value tree. When the
/// enclosing chunk is known, reject displaced or duplicate tile positions so
/// that a later read/write migration cannot silently attach contents elsewhere.
pub(crate) fn validate_tiles(tiles: &[Value], chunk: Option<(i32, i32)>) -> io::Result<()> {
    if tiles.len() > MAX_TILES_PER_CHUNK {
        return Err(invalid("too many tile entities in a chunk"));
    }
    let mut positions = std::collections::BTreeSet::new();
    for original in tiles {
        let bytes =
            fastnbt::to_bytes(original).map_err(|_| invalid("non-compound tile entity NBT"))?;
        let tile: TileEntity =
            fastnbt::from_bytes(&bytes).map_err(|_| invalid("invalid tile entity compound"))?;
        tile.validate()?;
        if !positions.insert((tile.x, tile.y, tile.z)) {
            return Err(invalid("duplicate tile entity position"));
        }
        if let Some((cx, cz)) = chunk {
            if tile.x.div_euclid(16) != cx || tile.z.div_euclid(16) != cz {
                return Err(invalid("tile entity is outside its owning chunk"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::GzEncoder, Compression};
    use std::io::Write;

    fn item(slot: i8) -> SlotItem {
        let mut extra = BTreeMap::new();
        extra.insert("UnknownItemField".into(), Value::String("kept".into()));
        SlotItem {
            slot,
            id: 264,
            count: 3,
            damage: 0,
            tag: None,
            extra,
        }
    }

    #[test]
    fn player_inventory_and_unknown_nbt_survive_roundtrip() {
        let mut extra = BTreeMap::new();
        extra.insert(
            "ModdedPlayerField".into(),
            Value::String("preserved".into()),
        );
        let original = PlayerData {
            pos: vec![0.5, 64.0, -1.5],
            motion: Some(vec![0.0, 0.0, 0.0]),
            rotation: Some(vec![90.0, 0.0]),
            dimension: -1,
            inventory: vec![item(0)],
            ender_items: vec![item(2)],
            health: Some(20.0),
            food_level: Some(19),
            xp_level: Some(5),
            xp_total: Some(50),
            extra,
        };
        let encoded = fastnbt::to_bytes(&original).unwrap();
        let decoded: PlayerData = fastnbt::from_bytes(&encoded).unwrap();
        decoded.validate().unwrap();
        assert_eq!(decoded.inventory[0].id, 264);
        assert!(
            matches!(decoded.inventory[0].extra.get("UnknownItemField"), Some(Value::String(s)) if s == "kept")
        );
        assert!(
            matches!(decoded.extra.get("ModdedPlayerField"), Some(Value::String(s)) if s == "preserved")
        );
        let root = std::env::temp_dir().join(format!(
            "ir1710-player-read-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let mut writer = GzEncoder::new(Vec::new(), Compression::default());
        writer.write_all(&encoded).unwrap();
        fs::write(&root, writer.finish().unwrap()).unwrap();
        assert_eq!(inspect_player(&root).unwrap(), (1, 1));
        fs::remove_file(root).unwrap();
    }

    #[test]
    fn malformed_items_and_duplicate_slots_are_rejected() {
        let mut player = PlayerData {
            pos: vec![1.0, 64.0, 1.0],
            motion: None,
            rotation: None,
            dimension: 0,
            inventory: vec![item(0), item(0)],
            ender_items: Vec::new(),
            health: None,
            food_level: None,
            xp_level: None,
            xp_total: None,
            extra: BTreeMap::new(),
        };
        assert!(player.validate().is_err());
        player.inventory.pop();
        player.inventory[0].count = 0;
        assert!(player.validate().is_err());
    }

    #[test]
    fn tile_entity_preserves_unknown_fields_without_dropping_items() {
        let mut extra = BTreeMap::new();
        extra.insert("CustomName".into(), Value::String("Treasure".into()));
        let tile = TileEntity {
            id: "Chest".into(),
            x: 16,
            y: 64,
            z: -1,
            items: Some(vec![item(0)]),
            extra,
        };
        let bytes = fastnbt::to_bytes(&tile).unwrap();
        let t: TileEntity = fastnbt::from_bytes(&bytes).unwrap();
        t.validate().unwrap();
        assert_eq!(t.items.as_ref().unwrap()[0].id, 264);
        assert!(matches!(t.extra.get("CustomName"), Some(Value::String(s)) if s == "Treasure"));
        let value: Value = fastnbt::from_bytes(&bytes).unwrap();
        validate_tiles(&[value], None).unwrap();
    }

    #[test]
    fn tile_position_and_vanilla_container_slot_bounds() {
        fn tile(id: &str, x: i32, z: i32, slot: i8) -> Value {
            let record = TileEntity {
                id: id.to_string(),
                x,
                y: 65,
                z,
                items: Some(vec![item(slot)]),
                extra: BTreeMap::new(),
            };
            fastnbt::from_bytes(&fastnbt::to_bytes(&record).unwrap()).unwrap()
        }

        // Negative block coordinates must use Euclidean chunk division.
        assert!(validate_tiles(&[tile("Chest", -1, -16, 26)], Some((-1, -1))).is_ok());
        assert!(validate_tiles(&[tile("Chest", -1, -16, 27)], Some((-1, -1))).is_err());
        assert!(validate_tiles(&[tile("Hopper", 0, 0, 4)], Some((0, 0))).is_ok());
        assert!(validate_tiles(&[tile("Hopper", 0, 0, 5)], Some((0, 0))).is_err());
        assert!(validate_tiles(&[tile("Furnace", 0, 0, 3)], Some((0, 0))).is_err());
        assert!(validate_tiles(&[tile("BrewingStand", 0, 0, -1)], Some((0, 0))).is_err());
        assert!(validate_tiles(&[tile("Dropper", 0, 0, 8)], Some((0, 0))).is_ok());
        // Unknown tiles are preserved, not assigned an assumed vanilla capacity.
        assert!(validate_tiles(&[tile("ModdedContainer", 0, 0, 100)], Some((0, 0))).is_ok());
        assert!(validate_tiles(&[tile("Chest", 16, 0, 0)], Some((0, 0))).is_err());
        assert!(validate_tiles(&[tile("Chest", -1, -16, 0)], Some((0, 0))).is_err());
        let duplicate = tile("Chest", 0, 0, 0);
        assert!(validate_tiles(&[duplicate.clone(), duplicate], Some((0, 0))).is_err());
    }
}
