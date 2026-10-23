//! E47: opt-in, session-locked 1.7.10 Anvil TileEntity inventory updates.
//! Only existing, known vanilla container inventories can change. Unrecognized
//! TileEntities, custom tags, entities, terrain and other chunks stay intact.
//! Not connected to the live IRW save path; this is a safe migration primitive.
use crate::dimensions::Dimension;
use crate::legacy_inventory_nbt::{self, SlotItem, TileEntity};
use crate::session_lock::WorldSessionLock;
use fastanvil::Region;
use fastnbt::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

const MAX_PATCHES: usize = 128;

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, reason)
}
fn wrap(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, error.to_string())
}

pub(crate) struct TileInventoryPatch {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    /// Prevent accidental edits when a block's TileEntity changed type.
    pub expected_id: String,
    /// An empty Vec means a deliberately emptied inventory, not deletion.
    pub items: Vec<SlotItem>,
}

fn supported_container(id: &str) -> bool {
    matches!(
        id,
        "Chest"
            | "Trap"
            | "Dispenser"
            | "Dropper"
            | "Furnace"
            | "Hopper"
            | "BrewingStand"
            | "Cauldron"
    )
}

fn checked_dir(path: &Path) -> io::Result<()> {
    if !fs::symlink_metadata(path)?.file_type().is_dir() {
        return Err(invalid(
            "Anvil dimension or region path is not a real directory",
        ));
    }
    Ok(())
}

fn region_file(
    lock: &mut WorldSessionLock,
    dimension: Dimension,
    rx: i32,
    rz: i32,
) -> io::Result<PathBuf> {
    lock.verify()?;
    let root = lock.world_dir();
    checked_dir(root)?;
    let dimension_dir = match dimension {
        Dimension::Overworld => root.to_path_buf(),
        Dimension::Nether => root.join("DIM-1"),
        Dimension::End => root.join("DIM1"),
    };
    checked_dir(&dimension_dir)?;
    let directory = dimension_dir.join("region");
    checked_dir(&directory)?;
    let path = directory.join(format!("r.{rx}.{rz}.mca"));
    if !fs::symlink_metadata(&path)?.file_type().is_file() {
        return Err(invalid("Anvil region must be a regular file"));
    }
    Ok(path)
}

fn decode_tile(original: &Value) -> io::Result<TileEntity> {
    fastnbt::from_bytes(&fastnbt::to_bytes(original).map_err(wrap)?).map_err(wrap)
}

/// Apply inventory-only edits to existing containers in ONE region. All
/// changes are validated in memory before the first on-disk mutation, then
/// published as a single region COW with session.lock and stale-source checks.
/// No new chunks or TileEntities are created. This is NOT live IRW integration.
pub(crate) fn rewrite_inventories_under_lock(
    lock: &mut WorldSessionLock,
    dimension: Dimension,
    changes: &[TileInventoryPatch],
) -> io::Result<()> {
    rewrite_inventories_impl(lock, dimension, changes, None)
}

/// Atomically persist an existing chest's Anvil inventory with the typed
/// native chest sidecar. This opt-in bridge rejects NBT tags that the native
/// Stack representation cannot preserve and requires identical inventory
/// contents in both formats. It is not wired to live world saves.
pub(crate) fn rewrite_chests_with_sidecar_under_lock(
    lock: &mut WorldSessionLock,
    dimension: Dimension,
    world_file: &Path,
    native_chests: &crate::chests::Chests,
    changes: &[TileInventoryPatch],
) -> io::Result<()> {
    if changes.is_empty() {
        return Err(invalid("chest transaction cannot be empty"));
    }
    for patch in changes {
        if patch.expected_id != "Chest" {
            return Err(invalid("native chest sidecar cannot store other TileEntity types"));
        }
        let mut expected = [None; crate::chests::CHEST_SLOTS];
        for item in &patch.items {
            if item.slot < 0
                || item.slot as usize >= expected.len()
                || item.tag.is_some()
                || !item.extra.is_empty()
            {
                return Err(invalid("native chest cannot represent NBT item extras or slot"));
            }
            let slot = &mut expected[item.slot as usize];
            if slot.is_some() {
                return Err(invalid("duplicate chest slot"));
            }
            if item.count <= 0 {
                return Err(invalid("invalid chest item count"));
            }
            let stack = crate::inventory::Stack {
                id: item.id,
                count: item.count as u8,
                damage: item.damage,
            };
            if !stack.is_valid() {
                return Err(invalid("invalid native chest stack"));
            }
            *slot = Some(stack);
        }
        if native_chests.slots((patch.x, patch.y, patch.z)) != expected {
            return Err(invalid("Anvil and native chest snapshot mismatch"));
        }
    }
    let encoded = native_chests.encoded()?;
    let sidecar = crate::chests::Chests::location(world_file);
    rewrite_inventories_impl(
        lock,
        dimension,
        changes,
        Some((world_file, sidecar.as_path(), encoded.as_slice())),
    )
}

fn rewrite_inventories_impl(
    lock: &mut WorldSessionLock,
    dimension: Dimension,
    changes: &[TileInventoryPatch],
    paired: Option<(&Path, &Path, &[u8])>,
) -> io::Result<()> {
    let first = changes
        .first()
        .ok_or_else(|| invalid("TileEntity update cannot be empty"))?;
    if changes.len() > MAX_PATCHES {
        return Err(invalid("too many TileEntity edits in one batch"));
    }
    let expected_region = (first.x.div_euclid(512), first.z.div_euclid(512));
    let mut seen = BTreeSet::new();
    let mut grouped: BTreeMap<(i32, i32), Vec<&TileInventoryPatch>> = BTreeMap::new();
    for patch in changes {
        if !(0..=255).contains(&patch.y)
            || patch.x.unsigned_abs() > 30_000_000
            || patch.z.unsigned_abs() > 30_000_000
            || !supported_container(&patch.expected_id)
        {
            return Err(invalid(
                "invalid or unsupported TileEntity inventory identity",
            ));
        }
        if (patch.x.div_euclid(512), patch.z.div_euclid(512)) != expected_region {
            return Err(invalid("TileEntity update crosses Anvil region boundary"));
        }
        if !seen.insert((patch.x, patch.y, patch.z)) {
            return Err(invalid("duplicate TileEntity inventory update"));
        }
        grouped
            .entry((patch.x.div_euclid(16), patch.z.div_euclid(16)))
            .or_default()
            .push(patch);
    }

    let source = region_file(lock, dimension, expected_region.0, expected_region.1)?;
    // Inspect the complete source first: fastanvil alone is not a full NBT or
    // sector validation layer. Capture the bytes before opening its chunks.
    crate::legacy_region::inspect_region(&source)?;
    let expected_bytes = fs::read(&source)?;
    let mut region = Region::from_stream(File::open(&source)?).map_err(wrap)?;
    let mut owned_replacements = Vec::with_capacity(grouped.len());
    for ((cx, cz), patches) in grouped {
        let raw = region
            .read_chunk(cx.rem_euclid(32) as usize, cz.rem_euclid(32) as usize)
            .map_err(wrap)?
            .ok_or_else(|| invalid("TileEntity owning Anvil chunk is missing"))?;
        let mut chunk = crate::legacy_nbt::parse_chunk(&raw, cx, cz)?;
        for patch in patches {
            let mut matched = None;
            for (idx, original) in chunk.level.tile_entities.iter().enumerate() {
                let tile = decode_tile(original)?;
                if (tile.x, tile.y, tile.z) == (patch.x, patch.y, patch.z) {
                    matched = Some((idx, tile));
                    break;
                }
            }
            let (index, mut tile) =
                matched.ok_or_else(|| invalid("TileEntity inventory target not found"))?;
            if tile.id != patch.expected_id {
                return Err(invalid("TileEntity type changed since inventory snapshot"));
            }
            tile.items = Some(patch.items.clone());
            let replacement: Value =
                fastnbt::from_bytes(&fastnbt::to_bytes(&tile).map_err(wrap)?).map_err(wrap)?;
            chunk.level.tile_entities[index] = replacement;
        }
        legacy_inventory_nbt::validate_tiles(&chunk.level.tile_entities, Some((cx, cz)))?;
        let encoded = fastnbt::to_bytes(&chunk).map_err(wrap)?;
        crate::legacy_nbt::parse_chunk(&encoded, cx, cz)?;
        owned_replacements.push(((cx, cz), encoded));
    }
    let refs: Vec<_> = owned_replacements
        .iter()
        .map(|(coords, bytes)| (*coords, bytes.as_slice()))
        .collect();
    if let Some((world_file, sidecar, serialized)) = paired {
        crate::legacy_region_rewrite::rewrite_chunks_with_sidecar_under_lock_expected(
            lock,
            dimension,
            &refs,
            Some(&expected_bytes),
            world_file,
            sidecar,
            serialized,
        )
    } else {
        crate::legacy_region_rewrite::rewrite_chunks_under_lock_expected(
            lock,
            dimension,
            &refs,
            Some(&expected_bytes),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{anvil_export, legacy_nbt, world::World};

    fn fixture_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "ir1710-e47-tile-cow-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        anvil_export::export(&World::new(), &root.join("world")).unwrap();
        root
    }

    fn tile(id: &str, x: i32, y: i32, z: i32, extra: &str) -> Value {
        let mut unknown = BTreeMap::new();
        unknown.insert("UnknownModTag".into(), Value::String(extra.into()));
        let record = TileEntity {
            id: id.into(),
            x,
            y,
            z,
            items: Some(Vec::new()),
            extra: unknown,
        };
        fastnbt::from_bytes(&fastnbt::to_bytes(&record).unwrap()).unwrap()
    }

    fn item(slot: i8) -> SlotItem {
        let mut extra = BTreeMap::new();
        extra.insert("UnknownItemModTag".into(), Value::String("kept".into()));
        SlotItem {
            slot,
            id: 264,
            count: 3,
            damage: 0,
            tag: None,
            extra,
        }
    }

    fn patch(id: &str, x: i32, y: i32, z: i32, slot: i8) -> TileInventoryPatch {
        TileInventoryPatch {
            x,
            y,
            z,
            expected_id: id.into(),
            items: vec![item(slot)],
        }
    }

    fn seed(root: &Path, lock: &mut WorldSessionLock) -> Value {
        let region_path = root.join("world/region/r.0.0.mca");
        let mut region = Region::from_stream(File::open(region_path).unwrap()).unwrap();
        let mut c0 =
            legacy_nbt::parse_chunk(&region.read_chunk(0, 0).unwrap().unwrap(), 0, 0).unwrap();
        let mut c1 =
            legacy_nbt::parse_chunk(&region.read_chunk(1, 0).unwrap().unwrap(), 1, 0).unwrap();
        let untouched = tile("CustomTile", 1, 65, 0, "custom-must-survive");
        c0.level.tile_entities = vec![
            tile("Chest", 0, 64, 0, "chest-must-survive"),
            untouched.clone(),
        ];
        c1.level.tile_entities = vec![tile("Furnace", 16, 64, 0, "furnace-must-survive")];
        let a = fastnbt::to_bytes(&c0).unwrap();
        let b = fastnbt::to_bytes(&c1).unwrap();
        crate::legacy_region_rewrite::rewrite_chunks_under_lock(
            lock,
            Dimension::Overworld,
            &[((0, 0), &a), ((1, 0), &b)],
        )
        .unwrap();
        untouched
    }

    #[test]
    fn two_chunks_update_in_one_region_without_losing_modded_tags_or_other_tiles() {
        let root = fixture_root();
        let mut lock = WorldSessionLock::acquire(&root.join("world")).unwrap();
        let untouched = seed(&root, &mut lock);
        let region_path = root.join("world/region/r.0.0.mca");
        rewrite_inventories_under_lock(
            &mut lock,
            Dimension::Overworld,
            &[patch("Chest", 0, 64, 0, 2), patch("Furnace", 16, 64, 0, 1)],
        )
        .unwrap();
        let mut region = Region::from_stream(File::open(&region_path).unwrap()).unwrap();
        let c0 = legacy_nbt::parse_chunk(&region.read_chunk(0, 0).unwrap().unwrap(), 0, 0).unwrap();
        let c1 = legacy_nbt::parse_chunk(&region.read_chunk(1, 0).unwrap().unwrap(), 1, 0).unwrap();
        assert_eq!(c0.level.tile_entities[1], untouched);
        for (original, id, slot, marker) in [
            (&c0.level.tile_entities[0], "Chest", 2, "chest-must-survive"),
            (
                &c1.level.tile_entities[0],
                "Furnace",
                1,
                "furnace-must-survive",
            ),
        ] {
            let decoded = decode_tile(original).unwrap();
            assert_eq!(decoded.id, id);
            assert_eq!(decoded.items.unwrap()[0].slot, slot);
            assert_eq!(
                decoded.extra.get("UnknownModTag"),
                Some(&Value::String(marker.into()))
            );
        }
        assert!(lock.verify().is_ok());
        drop(region);
        drop(lock);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_slot_duplicate_wrong_type_cross_region_and_lost_lock_never_publish() {
        let root = fixture_root();
        let mut lock = WorldSessionLock::acquire(&root.join("world")).unwrap();
        seed(&root, &mut lock);
        let source = root.join("world/region/r.0.0.mca");
        let before = fs::read(&source).unwrap();
        let bad_cases = vec![
            vec![patch("Chest", 0, 64, 0, 27)],
            vec![patch("Chest", 0, 64, 0, 1), patch("Chest", 0, 64, 0, 2)],
            vec![patch("Furnace", 0, 64, 0, 0)],
            vec![patch("Chest", 3, 64, 0, 0)],
            vec![patch("Chest", 0, 64, 0, 0), patch("Chest", 512, 64, 0, 0)],
            vec![patch("CustomTile", 1, 65, 0, 0)],
        ];
        for patches in bad_cases {
            assert!(
                rewrite_inventories_under_lock(&mut lock, Dimension::Overworld, &patches).is_err()
            );
            assert_eq!(fs::read(&source).unwrap(), before);
        }
        // Java-style session.lock tampering rejects even a valid edit.
        fs::write(root.join("world/session.lock"), 0i64.to_be_bytes()).unwrap();
        assert!(rewrite_inventories_under_lock(
            &mut lock,
            Dimension::Overworld,
            &[patch("Chest", 0, 64, 0, 2)],
        )
        .is_err());
        assert_eq!(fs::read(&source).unwrap(), before);
        drop(lock);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn anvil_chest_and_native_sidecar_commit_together_without_unknown_tag_loss() {
        use crate::chests::Chests;
        use crate::inventory::Stack;
        let root = fixture_root();
        let world = root.join("world");
        let world_file = world.join("flat.irw");
        World::new().save_new(&world_file).unwrap();
        let mut lock = WorldSessionLock::acquire(&world).unwrap();
        seed(&root, &mut lock);
        let mut updated = [None; crate::chests::CHEST_SLOTS];
        updated[2] = Some(Stack {
            id: 264,
            count: 3,
            damage: 0,
        });
        let native = Chests::default()
            .staged_update((0, 64, 0), updated)
            .unwrap();
        let patch = TileInventoryPatch {
            x: 0,
            y: 64,
            z: 0,
            expected_id: "Chest".into(),
            items: vec![SlotItem {
                slot: 2,
                id: 264,
                count: 3,
                damage: 0,
                tag: None,
                extra: BTreeMap::new(),
            }],
        };
        let region = world.join("region/r.0.0.mca");
        let before = fs::read(&region).unwrap();
        let sidecar = Chests::location(&world_file);
        // Reject mismatched snapshots without altering either participant.
        assert!(rewrite_chests_with_sidecar_under_lock(
            &mut lock,
            Dimension::Overworld,
            &world_file,
            &Chests::default(),
            std::slice::from_ref(&patch),
        )
        .is_err());
        assert_eq!(fs::read(&region).unwrap(), before);
        assert!(!sidecar.exists());

        rewrite_chests_with_sidecar_under_lock(
            &mut lock,
            Dimension::Overworld,
            &world_file,
            &native,
            &[patch],
        )
        .unwrap();
        assert_eq!(Chests::load(&sidecar).unwrap().slots((0, 64, 0)), updated);
        let mut opened = Region::from_stream(File::open(&region).unwrap()).unwrap();
        let chunk =
            legacy_nbt::parse_chunk(&opened.read_chunk(0, 0).unwrap().unwrap(), 0, 0).unwrap();
        let tile = decode_tile(&chunk.level.tile_entities[0]).unwrap();
        assert_eq!(tile.items.unwrap()[0].slot, 2);
        assert_eq!(
            tile.extra.get("UnknownModTag"),
            Some(&Value::String("chest-must-survive".into()))
        );
        assert!(!world_file.with_extension("pending-txn.json").exists());
        drop(opened);
        drop(lock);
        fs::remove_dir_all(root).unwrap();
    }
}
