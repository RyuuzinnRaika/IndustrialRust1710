//! Export a snapshot of IRW1 sparse flat-world blocks to Minecraft 1.7.10
//! Anvil .mca regions. This never modifies or overwrites the IRW1 source.
//! Uses independently licensed MIT / Apache-2.0 fastnbt and MIT fastanvil.
use crate::world::{World, VIEW_DISTANCE};
use fastanvil::Region;
use fastnbt::{ByteArray, IntArray};
use flate2::{write::GzEncoder, Compression};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Seek, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_EXPORT_CHUNKS: usize = 256;

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}
fn wrap(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}
fn signed_bytes(bytes: Vec<u8>) -> ByteArray {
    ByteArray::new(bytes.into_iter().map(|byte| byte as i8).collect())
}

#[derive(Serialize)]
struct AnvilRoot {
    #[serde(rename = "Level")]
    level: AnvilLevel,
}
#[derive(Serialize)]
struct AnvilLevel {
    #[serde(rename = "xPos")]
    x: i32,
    #[serde(rename = "zPos")]
    z: i32,
    #[serde(rename = "LastUpdate")]
    last_update: i64,
    #[serde(rename = "TerrainPopulated")]
    terrain_populated: i8,
    #[serde(rename = "LightPopulated")]
    light_populated: i8,
    #[serde(rename = "InhabitedTime")]
    inhabited_time: i64,
    #[serde(rename = "Biomes")]
    biomes: ByteArray,
    #[serde(rename = "HeightMap")]
    height_map: IntArray,
    #[serde(rename = "Sections")]
    sections: Vec<AnvilSection>,
    #[serde(rename = "Entities")]
    entities: Vec<fastnbt::Value>,
    #[serde(rename = "TileEntities")]
    tile_entities: Vec<fastnbt::Value>,
}
#[derive(Serialize)]
struct AnvilSection {
    #[serde(rename = "Y")]
    y: i8,
    #[serde(rename = "Blocks")]
    blocks: ByteArray,
    #[serde(rename = "Data")]
    metadata: ByteArray,
    #[serde(rename = "BlockLight")]
    block_light: ByteArray,
    #[serde(rename = "SkyLight")]
    sky_light: ByteArray,
}
#[derive(Serialize)]
struct LevelDat {
    #[serde(rename = "Data")]
    data: LevelInfo,
}
#[derive(Serialize)]
struct LevelInfo {
    #[serde(rename = "LevelName")]
    name: String,
    #[serde(rename = "GameType")]
    game_type: i32,
    #[serde(rename = "version")]
    anvil_version: i32,
    #[serde(rename = "RandomSeed")]
    random_seed: i64,
    #[serde(rename = "generatorName")]
    generator: String,
    #[serde(rename = "generatorVersion")]
    generator_version: i32,
    #[serde(rename = "MapFeatures")]
    map_features: i8,
    #[serde(rename = "SpawnX")]
    spawn_x: i32,
    #[serde(rename = "SpawnY")]
    spawn_y: i32,
    #[serde(rename = "SpawnZ")]
    spawn_z: i32,
    #[serde(rename = "Time")]
    time: i64,
    #[serde(rename = "DayTime")]
    day_time: i64,
    #[serde(rename = "allowCommands")]
    allow_commands: i8,
    #[serde(rename = "initialized")]
    initialized: i8,
}

fn nbt_for_chunk(
    world: &World,
    cx: i32,
    cz: i32,
    tiles: &[fastnbt::Value],
) -> io::Result<Vec<u8>> {
    let mut sections = Vec::new();
    let base_x = cx * 16;
    let base_z = cz * 16;
    for section_y in 0..16i32 {
        let mut blocks = vec![0u8; 4096];
        let mut metadata = vec![0u8; 2048];
        let mut any_block = false;
        for dy in 0..16i32 {
            for z in 0..16i32 {
                for x in 0..16i32 {
                    let block = world.get(base_x + x, section_y * 16 + dy, base_z + z);
                    let idx = ((dy * 16 + z) * 16 + x) as usize;
                    blocks[idx] = (block >> 4) as u8;
                    metadata[idx / 2] |= ((block & 15) as u8) << ((idx % 2) * 4);
                    any_block |= block != 0;
                }
            }
        }
        if any_block {
            sections.push(AnvilSection {
                y: section_y as i8,
                blocks: signed_bytes(blocks),
                metadata: signed_bytes(metadata),
                block_light: signed_bytes(vec![0; 2048]),
                sky_light: signed_bytes(vec![255; 2048]),
            });
        }
    }
    let mut heights = vec![64i32; 256];
    for z in 0..16 {
        for x in 0..16 {
            let top = (0..256i32)
                .rev()
                .find(|&y| world.get(base_x + x, y, base_z + z) != 0)
                .map_or(0, |y| y + 1);
            heights[(z * 16 + x) as usize] = top;
        }
    }
    let root = AnvilRoot {
        level: AnvilLevel {
            x: cx,
            z: cz,
            last_update: 0,
            terrain_populated: 1,
            light_populated: 1,
            inhabited_time: 0,
            biomes: signed_bytes(vec![1; 256]),
            height_map: IntArray::new(heights),
            sections,
            entities: Vec::new(),
            tile_entities: tiles.to_vec(),
        },
    };
    fastnbt::to_bytes(&root).map_err(wrap)
}

fn export_level_dat(directory: &Path) -> io::Result<()> {
    let content = LevelDat {
        data: LevelInfo {
            name: "IndustrialRust1710 Export".to_string(),
            game_type: 1,
            anvil_version: 19133,
            random_seed: 0,
            generator: "flat".to_string(),
            generator_version: 0,
            map_features: 0,
            spawn_x: 0,
            spawn_y: 65,
            spawn_z: 0,
            time: 0,
            day_time: 6000,
            allow_commands: 1,
            initialized: 1,
        },
    };
    let bytes = fastnbt::to_bytes(&content).map_err(wrap)?;
    let output = File::create(directory.join("level.dat"))?;
    let mut encoder = GzEncoder::new(output, Compression::default());
    encoder.write_all(&bytes)?;
    let file = encoder.finish()?;
    file.sync_all()
}

/// Writes into a private sibling directory and publishes it only once the
/// region files and level.dat are fully synchronized. The active IRW source
/// is never modified; this is still an offline snapshot, not live Anvil IO.
fn write_snapshot(
    world: &World,
    stage: &Path,
    chunks: &BTreeSet<(i32, i32)>,
    tiles: &crate::native_tile_export::TileColumns,
) -> io::Result<()> {
    let regions_dir = stage.join("region");
    fs::create_dir(&regions_dir)?;
    let mut regions: BTreeMap<(i32, i32), Vec<(i32, i32)>> = BTreeMap::new();
    for &(cx, cz) in chunks {
        regions
            .entry((cx.div_euclid(32), cz.div_euclid(32)))
            .or_default()
            .push((cx, cz));
    }
    for ((rx, rz), columns) in regions {
        let filename = regions_dir.join(format!("r.{rx}.{rz}.mca"));
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(filename)?;
        let mut region = Region::create(file).map_err(wrap)?;
        for (cx, cz) in columns {
            let bytes = nbt_for_chunk(world, cx, cz, tiles.get(&(cx, cz)).map_or(&[], Vec::as_slice))?;
            region
                .write_chunk(
                    cx.rem_euclid(32) as usize,
                    cz.rem_euclid(32) as usize,
                    &bytes,
                )
                .map_err(wrap)?;
        }
        let mut file = region.into_inner().map_err(wrap)?;
        let len = file.stream_position()?;
        file.set_len(len)?;
        file.sync_all()?;
    }
    export_level_dat(stage)?;
    #[cfg(unix)]
    {
        // File sync alone does not persist the new region directory entries
        // across a power failure.
        File::open(&regions_dir)?.sync_all()?;
        File::open(stage)?.sync_all()?;
    }
    Ok(())
}

/// Exports only spawn 5x5 and changed chunk columns, never an unbounded map.
/// The destination must not exist. Failed serialization cleans up the private
/// staging tree rather than exposing a partially written Minecraft world.
pub fn export(world: &World, dest: &Path) -> io::Result<usize> {
    export_tiles(world, dest, &Default::default())
}

/// Offline exported worlds include persisted native container slots when
/// requested by the CLI; ordinary test exports remain side-effect free.
pub fn export_with_storage(world: &World, dest: &Path, source: &Path) -> io::Result<usize> {
    let tiles = crate::native_tile_export::load(world, source)?;
    export_tiles(world, dest, &tiles)
}

fn export_tiles(
    world: &World,
    dest: &Path,
    tiles: &crate::native_tile_export::TileColumns,
) -> io::Result<usize> {
    let mut chunks: BTreeSet<(i32, i32)> = world.edited_chunks();
    for z in -VIEW_DISTANCE..=VIEW_DISTANCE {
        for x in -VIEW_DISTANCE..=VIEW_DISTANCE {
            chunks.insert((x, z));
        }
    }
    chunks.extend(tiles.keys().copied());
    if chunks.len() > MAX_EXPORT_CHUNKS {
        return Err(invalid("Anvil snapshot exceeds 256 chunk limit"));
    }
    match fs::symlink_metadata(dest) {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "destination Minecraft world already exists",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let parent = dest
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let basename = dest
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("Anvil destination must have a name"))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(wrap)?
        .as_nanos();
    let stage = parent.join(format!(
        ".{basename}.ir-export-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&stage)?;
    let result = (|| {
        write_snapshot(world, &stage, &chunks, tiles)?;
        // Do not rename over a world that appeared during export. The check
        // prevents accidental overwrite during normal operation; concurrent
        // uncooperative directory creation still requires external locking.
        match fs::symlink_metadata(dest) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "destination Minecraft world appeared during export",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        fs::rename(&stage, dest)?;
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        Ok(chunks.len())
    })();
    if result.is_err() {
        // A crash may leave a hidden staging directory. Such directories are
        // never loaded as worlds, and ordinary errors clean up immediately.
        let _ = fs::remove_dir_all(&stage);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct ParsedRoot {
        #[serde(rename = "Level")]
        level: ParsedLevel,
    }
    #[derive(Deserialize)]
    struct ParsedLevel {
        #[serde(rename = "xPos")]
        x: i32,
        #[serde(rename = "zPos")]
        z: i32,
        #[serde(rename = "Sections")]
        sections: Vec<ParsedSection>,
        #[serde(rename = "HeightMap")]
        height_map: IntArray,
    }
    #[derive(Deserialize)]
    struct ParsedSection {
        #[serde(rename = "Y")]
        y: i8,
        #[serde(rename = "Blocks")]
        blocks: ByteArray,
        #[serde(rename = "Data")]
        metadata: ByteArray,
    }

    #[test]
    fn exports_four_persisted_container_tiles_with_items_and_legacy_timers() {
        use crate::brewing::{Brewer, Brewers};
        use crate::chests::{Chests, CHEST_SLOTS};
        use crate::furnaces::{Furnace, Furnaces};
        use crate::hoppers::{Hoppers, HOPPER_SLOTS};
        use crate::inventory::Stack;
        use crate::legacy_inventory_nbt::TileEntity;

        let base = std::env::temp_dir().join(format!(
            "industrial-anvil-native-tiles-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir(&base).unwrap();
        let source = base.join("flat.irw");
        let dest = base.join("snapshot");
        let mut world = World::new();
        for (x, block) in [(1, 54u16), (2, 61), (3, 154), (4, 117)] {
            world.update_and_save(&source, x, 64, 1, block << 4).unwrap();
        }
        let stack = Stack {
            id: 4,
            count: 7,
            damage: 0,
        };
        let mut chest_slots = [None; CHEST_SLOTS];
        chest_slots[26] = Some(stack);
        let chests = Chests::default().staged_update((1, 64, 1), chest_slots).unwrap();
        fs::write(Chests::location(&source), chests.encoded().unwrap()).unwrap();

        let furnace = Furnace {
            slots: [Some(stack), None, None],
            burn_left: 50,
            burn_total: 100,
            cook: 10,
        };
        let furnaces = Furnaces::default().staged_update((2, 64, 1), furnace).unwrap();
        fs::write(Furnaces::location(&source), furnaces.encoded().unwrap()).unwrap();

        let mut hopper_slots = [None; HOPPER_SLOTS];
        hopper_slots[4] = Some(stack);
        let hoppers = Hoppers::default().staged_update((3, 64, 1), hopper_slots).unwrap();
        fs::write(Hoppers::location(&source), hoppers.encoded().unwrap()).unwrap();

        let brewer = Brewer {
            slots: [Some(stack), None, None, None],
            brew_time: 5,
        };
        let brewers = Brewers::default().staged_update((4, 64, 1), brewer).unwrap();
        fs::write(Brewers::location(&source), brewers.encoded().unwrap()).unwrap();

        let world_before = fs::read(&source).unwrap();
        assert_eq!(export_with_storage(&world, &dest, &source).unwrap(), 25);
        assert_eq!(fs::read(&source).unwrap(), world_before);
        let file = File::open(dest.join("region/r.0.0.mca")).unwrap();
        let mut region = Region::from_stream(file).unwrap();
        let raw = region.read_chunk(0, 0).unwrap().unwrap();
        let parsed = crate::legacy_nbt::parse_chunk(&raw, 0, 0).unwrap();
        let mut tiles = BTreeMap::new();
        for value in &parsed.level.tile_entities {
            let decoded: TileEntity = fastnbt::from_bytes(&fastnbt::to_bytes(value).unwrap()).unwrap();
            tiles.insert(decoded.x, decoded);
        }
        assert_eq!(tiles.len(), 4);
        for (x, name, slot) in [
            (1, "Chest", 26i8),
            (2, "Furnace", 0),
            (3, "Hopper", 4),
            (4, "Cauldron", 0),
        ] {
            let tile = &tiles[&x];
            assert_eq!(tile.id, name);
            let items = tile.items.as_ref().unwrap();
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].slot, slot);
            assert_eq!(items[0].id, 4);
            assert_eq!(items[0].count, 7);
        }
        assert_eq!(tiles[&2].extra["BurnTime"], fastnbt::Value::Short(50));
        assert_eq!(tiles[&2].extra["CookTime"], fastnbt::Value::Short(10));
        assert_eq!(tiles[&4].extra["BrewTime"], fastnbt::Value::Short(5));
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn mismatched_container_block_aborts_snapshot_without_writing_destination() {
        use crate::chests::{Chests, CHEST_SLOTS};
        use crate::inventory::Stack;
        let base = std::env::temp_dir().join(format!(
            "industrial-anvil-orphan-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir(&base).unwrap();
        let source = base.join("flat.irw");
        let dest = base.join("snapshot");
        let world = World::new();
        world.save(&source).unwrap();
        let mut slots = [None; CHEST_SLOTS];
        slots[0] = Some(Stack { id: 4, count: 1, damage: 0 });
        let state = Chests::default().staged_update((1, 64, 1), slots).unwrap();
        fs::write(Chests::location(&source), state.encoded().unwrap()).unwrap();
        assert!(export_with_storage(&world, &dest, &source).is_err());
        assert!(!dest.exists());
        assert!(source.is_file());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn export_refuses_existing_world_without_touching_its_contents() {
        let base = std::env::temp_dir().join(format!(
            "industrial-anvil-no-overwrite-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir(&base).unwrap();
        let output = base.join("minecraft");
        fs::create_dir(&output).unwrap();
        fs::write(output.join("sentinel"), b"must-remain").unwrap();
        let error = export(&World::new(), &output).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(output.join("sentinel")).unwrap(), b"must-remain");
        assert_eq!(fs::read_dir(&base).unwrap().count(), 1);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn exports_regions_and_legacy_sections_without_touching_irw1() {
        let base =
            std::env::temp_dir().join(format!("industrial-anvil-export-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir(&base).unwrap();
        let original = base.join("flat.irw");
        let output = base.join("snapshot");
        let mut world = World::new();
        world
            .update_and_save(&original, -49, 145, 19, (4 << 4) | 2)
            .unwrap();
        let original_contents = fs::read(&original).unwrap();
        assert_eq!(export(&world, &output).unwrap(), 26);
        assert_eq!(original_contents, fs::read(&original).unwrap());
        assert!(output.join("level.dat").is_file());
        let file = File::open(output.join("region/r.-1.0.mca")).unwrap();
        let mut region = Region::from_stream(file).unwrap();
        let raw = region.read_chunk(28, 1).unwrap().unwrap();
        let parsed: ParsedRoot = fastnbt::from_bytes(&raw).unwrap();
        assert_eq!((parsed.level.x, parsed.level.z), (-4, 1));
        let sec = parsed.level.sections.iter().find(|s| s.y == 9).unwrap();
        // local x 15, z 3, y 1 -> (1*256 + 3*16 + 15)=319
        assert_eq!(sec.blocks[319], 4);
        assert_eq!((sec.metadata[319 / 2] as u8) >> 4, 2);
        assert_eq!(parsed.level.height_map[3 * 16 + 15], 146);
        assert!(export(&world, &output).is_err());
        fs::remove_dir_all(base).unwrap();
    }
}
