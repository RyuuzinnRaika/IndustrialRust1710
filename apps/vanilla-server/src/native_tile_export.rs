//! E47: Offline, read-only projection of the server's persisted container
//! inventories into Minecraft Java 1.7.10 TileEntity NBT for Anvil export.
//! This does not enable live Anvil writes or discard unknown imported NBT.
use crate::brewing::{Brewer, Brewers};
use crate::chests::{Chests, CHEST_SLOTS};
use crate::furnaces::{Furnace, Furnaces};
use crate::hoppers::{Hoppers, HOPPER_SLOTS};
use crate::inventory::Stack;
use crate::legacy_inventory_nbt::{self, SlotItem, TileEntity};
use crate::world::World;
use fastnbt::Value;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::{self, ErrorKind};
use std::path::Path;

type Column = (i32, i32);
type Position = (i32, i32, i32);
pub(crate) type TileColumns = BTreeMap<Column, Vec<Value>>;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

#[derive(Deserialize)]
struct NativeEntries<T> {
    entries: BTreeMap<String, T>,
}

/// Reuse the source store's own load/validate/encode path before decoding its
/// private coordinate map. This avoids accepting unchecked JSON on export.
fn entries<T: DeserializeOwned>(validated_bytes: Vec<u8>) -> io::Result<BTreeMap<String, T>> {
    let decoded: NativeEntries<T> = serde_json::from_slice(&validated_bytes)
        .map_err(|_| invalid("native container export schema changed"))?;
    Ok(decoded.entries)
}

fn position(key: &str) -> io::Result<Position> {
    let mut fields = key.split(',');
    let parse = |field: Option<&str>| {
        field
            .ok_or_else(|| invalid("native tile coordinate missing"))?
            .parse::<i32>()
            .map_err(|_| invalid("native tile coordinate invalid"))
    };
    let xyz = (parse(fields.next())?, parse(fields.next())?, parse(fields.next())?);
    if fields.next().is_some() || format!("{},{},{}", xyz.0, xyz.1, xyz.2) != key {
        return Err(invalid("noncanonical native tile coordinate"));
    }
    Ok(xyz)
}

fn items(slots: &[Option<Stack>]) -> io::Result<Vec<SlotItem>> {
    slots
        .iter()
        .enumerate()
        .filter_map(|(slot, stack)| stack.map(|stack| (slot, stack)))
        .map(|(slot, stack)| {
            if !stack.is_valid() {
                return Err(invalid("invalid item in native tile"));
            }
            Ok(SlotItem {
                slot: i8::try_from(slot).map_err(|_| invalid("tile slot exceeds signed byte"))?,
                id: stack.id,
                count: i8::try_from(stack.count).map_err(|_| invalid("tile stack too large"))?,
                damage: stack.damage,
                tag: None,
                extra: BTreeMap::new(),
            })
        })
        .collect()
}

fn push_tile(
    world: &World,
    columns: &mut TileColumns,
    pos: Position,
    expected_block: &[u16],
    id: &str,
    slots: &[Option<Stack>],
    extra: BTreeMap<String, Value>,
) -> io::Result<()> {
    let block = world.get(pos.0, pos.1, pos.2) >> 4;
    if !expected_block.contains(&block) {
        // Do not export orphaned inventories attached to air or the wrong
        // block: that could silently destroy or misattribute user items.
        return Err(invalid("native tile inventory does not match world block"));
    }
    let tile = TileEntity {
        id: id.to_owned(),
        x: pos.0,
        y: pos.1,
        z: pos.2,
        items: Some(items(slots)?),
        extra,
    };
    let nbt = fastnbt::to_bytes(&tile).map_err(|_| invalid("cannot encode native tile NBT"))?;
    let value = fastnbt::from_bytes::<Value>(&nbt)
        .map_err(|_| invalid("cannot decode native tile NBT"))?;
    columns
        .entry((pos.0.div_euclid(16), pos.2.div_euclid(16)))
        .or_default()
        .push(value);
    Ok(())
}

/// Load persisted stores only, never currently mutable in-memory state. The
/// export caller must hold the world session lock and reject pending journals.
pub(crate) fn load(world: &World, world_path: &Path) -> io::Result<TileColumns> {
    let mut columns = TileColumns::new();
    let chests = Chests::load(&Chests::location(world_path))?;
    for (key, slots) in entries::<[Option<Stack>; CHEST_SLOTS]>(chests.encoded()?)? {
        push_tile(world, &mut columns, position(&key)?, &[54, 146], "Chest", &slots, BTreeMap::new())?;
    }

    let furnaces = Furnaces::load(&Furnaces::location(world_path))?;
    for (key, furnace) in entries::<Furnace>(furnaces.encoded()?)? {
        let mut extra = BTreeMap::new();
        extra.insert("BurnTime".into(), Value::Short(furnace.burn_left as i16));
        extra.insert("CookTime".into(), Value::Short(furnace.cook as i16));
        push_tile(world, &mut columns, position(&key)?, &[61, 62], "Furnace", &furnace.slots, extra)?;
    }

    let hoppers = Hoppers::load(&Hoppers::location(world_path))?;
    for (key, slots) in entries::<[Option<Stack>; HOPPER_SLOTS]>(hoppers.encoded()?)? {
        push_tile(world, &mut columns, position(&key)?, &[154], "Hopper", &slots, BTreeMap::new())?;
    }

    let brewers = Brewers::load(&Brewers::location(world_path))?;
    for (key, stand) in entries::<Brewer>(brewers.encoded()?)? {
        let mut extra = BTreeMap::new();
        extra.insert("BrewTime".into(), Value::Short(stand.brew_time as i16));
        // Vanilla 1.7.10 registers the brewing-stand TileEntity as "Cauldron".
        push_tile(world, &mut columns, position(&key)?, &[117], "Cauldron", &stand.slots, extra)?;
    }

    for (&chunk, values) in &columns {
        legacy_inventory_nbt::validate_tiles(values, Some(chunk))?;
    }
    Ok(columns)
}
