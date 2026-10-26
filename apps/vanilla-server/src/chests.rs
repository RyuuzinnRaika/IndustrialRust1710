//! Small, world-owned single-chest storage for protocol 5.
//!
//! Entries are keyed by block coordinates rather than player name. All chest
//! access goes through one shared mutex so multiple users cannot race.
//! This is *not* full Anvil TileEntity persistence or multi-file transactions.
use crate::inventory::Stack;
use crate::world;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

pub type Position = (i32, i32, i32);
pub const CHEST_SLOTS: usize = 27;
const VERSION: u8 = 1;
const MAX_CHESTS: usize = 512;
const MAX_BYTES: u64 = 400 * 1024;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}
fn key((x, y, z): Position) -> String {
    format!("{x},{y},{z}")
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chests {
    version: u8,
    entries: BTreeMap<String, [Option<Stack>; CHEST_SLOTS]>,
    #[serde(default)]
    generated_claimed: BTreeSet<String>,
    #[serde(skip)]
    revision: u64,
}
impl Default for Chests {
    fn default() -> Self {
        Self {
            version: VERSION,
            entries: BTreeMap::new(),
            generated_claimed: BTreeSet::new(),
            revision: 0,
        }
    }
}
impl Chests {
    pub fn location(world: &Path) -> PathBuf {
        world.with_extension("chests.json")
    }
    fn validate(&self) -> io::Result<()> {
        if self.version != VERSION || self.entries.len() > MAX_CHESTS {
            return Err(invalid("invalid chest version or count"));
        }
        if self.generated_claimed.len() > MAX_CHESTS {
            return Err(invalid("too many generated chest claims"));
        }
        for position in &self.generated_claimed {
            let parts: Vec<_> = position.split(',').map(str::parse::<i32>).collect();
            if parts.len() != 3 || parts.iter().any(Result::is_err) {
                return Err(invalid("invalid generated chest claim"));
            }
        }
        for (name, slots) in &self.entries {
            let coords: Vec<_> = name.split(',').map(str::parse::<i32>).collect();
            if coords.len() != 3 || coords.iter().any(Result::is_err) {
                return Err(invalid("invalid chest coordinate key"));
            }
            let pos = (
                coords[0].as_ref().copied().unwrap(),
                coords[1].as_ref().copied().unwrap(),
                coords[2].as_ref().copied().unwrap(),
            );
            if !world::in_bounds(pos.0, pos.1, pos.2)
                || key(pos) != *name
                || slots.iter().flatten().any(|stack| !stack.is_valid())
            {
                return Err(invalid("invalid chest coordinates or item stack"));
            }
        }
        Ok(())
    }
    pub fn load(path: &Path) -> io::Result<Self> {
        let meta = match fs::symlink_metadata(path) {
            Ok(v) => v,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        if !meta.file_type().is_file() || meta.len() > MAX_BYTES {
            return Err(invalid("invalid chest file type or size"));
        }
        let bytes = fs::read(path)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid("chest file exceeds limit"));
        }
        let value: Self =
            serde_json::from_slice(&bytes).map_err(|_| invalid("invalid chest data"))?;
        value.validate()?;
        Ok(value)
    }
    pub fn encoded(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| invalid("encode chest"))?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid("chest capacity reached"));
        }
        Ok(bytes)
    }
    pub fn staged_update(
        &self,
        pos: Position,
        slots: [Option<Stack>; CHEST_SLOTS],
    ) -> io::Result<Self> {
        if !world::in_bounds(pos.0, pos.1, pos.2)
            || slots.iter().flatten().any(|item| !item.is_valid())
        {
            return Err(invalid("invalid chest staged update"));
        }
        let mut candidate = self.clone();
        if slots.iter().all(Option::is_none) {
            candidate.entries.remove(&key(pos));
        } else {
            candidate.entries.insert(key(pos), slots);
        }
        candidate.encoded()?;
        candidate.revision = self.revision.wrapping_add(1);
        Ok(candidate)
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    /// Persist a previously validated, fully staged window without exposing
    /// half-updated storage to other players.
    pub fn commit_staged(&mut self, path: &Path, candidate: Self) -> io::Result<()> {
        candidate.save(path)?;
        *self = candidate;
        Ok(())
    }
    /// Initializes an IRW4 dungeon loot chest exactly once. The claimed marker
    /// remains after the chest has been emptied or mined, preventing duplication.
    /// The new slots and marker are committed atomically in the chest file.
    pub fn initialize_generated(
        &mut self,
        path: &Path,
        pos: Position,
        loot: [(i16, u8); 3],
    ) -> io::Result<()> {
        if !world::in_bounds(pos.0, pos.1, pos.2) {
            return Err(invalid("generated chest outside world"));
        }
        let key = key(pos);
        if self.generated_claimed.contains(&key) {
            return Ok(());
        }
        let mut staged = self.clone();
        staged.generated_claimed.insert(key.clone());
        if let std::collections::btree_map::Entry::Vacant(entry) = staged.entries.entry(key) {
            let mut slots = [None; CHEST_SLOTS];
            for (index, (id, count)) in loot.into_iter().enumerate() {
                let stack = Stack {
                    id,
                    count,
                    damage: 0,
                    nbt: None,
                };
                if !stack.is_valid() {
                    return Err(invalid("invalid generated loot stack"));
                }
                slots[index * 5 + 2] = Some(stack);
            }
            entry.insert(slots);
        }
        staged.revision = self.revision.wrapping_add(1);
        staged.save(path)?;
        *self = staged;
        Ok(())
    }

    pub fn slots(&self, pos: Position) -> [Option<Stack>; CHEST_SLOTS] {
        self.entries
            .get(&key(pos))
            .copied()
            .unwrap_or([None; CHEST_SLOTS])
    }
    /// Analog signal from the committed chest inventory, never from a client window.
    /// Empty chests are omitted so the world clears their previous signal.
    pub fn comparator_inputs(&self) -> BTreeMap<(i32, u8, i32), u8> {
        self.entries
            .iter()
            .filter_map(|(name, slots)| {
                let mut coords = name.split(',');
                let x = coords.next()?.parse::<i32>().ok()?;
                let y = coords.next()?.parse::<u8>().ok()?;
                let z = coords.next()?.parse::<i32>().ok()?;
                if coords.next().is_some() {
                    return None;
                }
                let strength = crate::dispensers::comparator_strength(slots);
                (strength > 0).then_some(((x, y, z), strength))
            })
            .collect()
    }
    /// Clone the committed chest halves before taking the world mutex.
    /// This preserves the inventory -> world lock ordering of the tick worker.
    pub fn comparator_snapshot(&self) -> BTreeMap<Position, [Option<Stack>; CHEST_SLOTS]> {
        self.entries
            .iter()
            .filter_map(|(name, slots)| {
                let mut coords = name.split(',');
                let x = coords.next()?.parse::<i32>().ok()?;
                let y = coords.next()?.parse::<i32>().ok()?;
                let z = coords.next()?.parse::<i32>().ok()?;
                if coords.next().is_some() {
                    return None;
                }
                Some(((x, y, z), *slots))
            })
            .collect()
    }
    pub fn is_nonempty(&self, pos: Position) -> bool {
        self.slots(pos).iter().any(Option::is_some)
    }
    fn save(&self, path: &Path) -> io::Result<()> {
        self.validate()?;
        let encoded = serde_json::to_vec(self).map_err(|_| invalid("encode chest"))?;
        if encoded.len() as u64 > MAX_BYTES {
            return Err(invalid("chest capacity reached"));
        }
        if let Some(parent) = path.parent().filter(|part| !part.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("chests.tmp");
        let mut file = File::create(&tmp)?;
        file.write_all(&encoded)?;
        file.sync_all()?;
        drop(file);
        fs::rename(tmp, path)?;
        Ok(())
    }
    /// Commit an individual chest atomically *within the chest file*.
    /// Caller must hold the shared mutex for the entire transaction.
    #[cfg(test)]
    pub fn update(
        &mut self,
        path: &Path,
        pos: Position,
        slots: [Option<Stack>; CHEST_SLOTS],
    ) -> io::Result<()> {
        if !world::in_bounds(pos.0, pos.1, pos.2)
            || slots.iter().flatten().any(|item| !item.is_valid())
        {
            return Err(invalid("invalid chest update"));
        }
        if self.slots(pos) == slots {
            return Ok(());
        }
        let name = key(pos);
        let previous = self.entries.get(&name).copied();
        if slots.iter().all(Option::is_none) {
            self.entries.remove(&name);
        } else {
            self.entries.insert(name.clone(), slots);
        }
        if let Err(error) = self.save(path) {
            match previous {
                Some(existing) => {
                    self.entries.insert(name, existing);
                }
                None => {
                    self.entries.remove(&name);
                }
            }
            return Err(error);
        }
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }
}
/// Recalculate both halves of a double chest from its combined 54 slots.
/// Only world-confirmed, same-type adjacent chests can form a pair. An empty
/// half has no persisted entry but still receives the shared analog strength.
pub fn combine_chest_comparator_inputs(
    world: &world::World,
    snapshots: &BTreeMap<Position, [Option<Stack>; CHEST_SLOTS]>,
    mut inputs: BTreeMap<(i32, u8, i32), u8>,
) -> BTreeMap<(i32, u8, i32), u8> {
    // A chest inventory entry may outlive its block after a replacement.
    // Never expose its old analog strength to the world update pipeline.
    inputs.retain(|&(x, y, z), _| matches!(world.get(x, i32::from(y), z) >> 4, 54 | 146));
    for (&(x, y, z), slots) in snapshots {
        let id = world.get(x, y, z) >> 4;
        if !matches!(id, 54 | 146) {
            continue;
        }
        let partner = [(-1, 0), (1, 0), (0, -1), (0, 1)]
            .into_iter()
            .find_map(|(dx, dz)| {
                let nx = x.checked_add(dx)?;
                let nz = z.checked_add(dz)?;
                (world.get(nx, y, nz) >> 4 == id).then_some((nx, y, nz))
            });
        let Some(partner) = partner else {
            continue;
        };
        let mut combined = [None; CHEST_SLOTS * 2];
        combined[..CHEST_SLOTS].copy_from_slice(slots);
        if let Some(other) = snapshots.get(&partner) {
            combined[CHEST_SLOTS..].copy_from_slice(other);
        }
        let strength = crate::dispensers::comparator_strength(&combined);
        for (px, py, pz) in [(x, y, z), partner] {
            let key = (px, py as u8, pz);
            if strength == 0 {
                inputs.remove(&key);
            } else {
                inputs.insert(key, strength);
            }
        }
    }
    inputs
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chest_comparator_snapshot_tracks_staged_inventory_and_emptying() {
        let pos = (-4, 64, 9);
        let stack = Stack {
            id: 1,
            count: 64,
            damage: 0,
            nbt: None,
        };
        let mut slots = [None; CHEST_SLOTS];
        slots[0] = Some(stack);
        let partial = Chests::default().staged_update(pos, slots).unwrap();
        assert_eq!(partial.comparator_inputs().get(&(-4, 64, 9)), Some(&1));
        let full = partial
            .staged_update(pos, [Some(stack); CHEST_SLOTS])
            .unwrap();
        assert_eq!(full.comparator_inputs().get(&(-4, 64, 9)), Some(&15));
        let empty = full.staged_update(pos, [None; CHEST_SLOTS]).unwrap();
        assert!(!empty.comparator_inputs().contains_key(&(-4, 64, 9)));
    }

    #[test]
    fn double_chest_comparator_uses_all_fifty_four_slots_and_both_halves() {
        let left = (2, 64, 3);
        let right = (3, 64, 3);
        let full_stack = Stack {
            id: 1,
            count: 64,
            damage: 0,
            nbt: None,
        };
        let mut world = world::World::new();
        world.import_block(left.0, left.1, left.2, 54 << 4).unwrap();
        world
            .import_block(right.0, right.1, right.2, 54 << 4)
            .unwrap();
        let left_only = Chests::default()
            .staged_update(left, [Some(full_stack); CHEST_SLOTS])
            .unwrap();
        let inputs = combine_chest_comparator_inputs(
            &world,
            &left_only.comparator_snapshot(),
            left_only.comparator_inputs(),
        );
        assert_eq!(inputs.get(&(2, 64, 3)), Some(&8));
        assert_eq!(inputs.get(&(3, 64, 3)), Some(&8));

        let both = left_only
            .staged_update(right, [Some(full_stack); CHEST_SLOTS])
            .unwrap();
        let inputs = combine_chest_comparator_inputs(
            &world,
            &both.comparator_snapshot(),
            both.comparator_inputs(),
        );
        assert_eq!(inputs.get(&(2, 64, 3)), Some(&15));
        assert_eq!(inputs.get(&(3, 64, 3)), Some(&15));

        // Removing the other half restores the single chest's 27-slot scale.
        world.import_block(right.0, right.1, right.2, 0).unwrap();
        let inputs = combine_chest_comparator_inputs(
            &world,
            &left_only.comparator_snapshot(),
            left_only.comparator_inputs(),
        );
        assert_eq!(inputs.get(&(2, 64, 3)), Some(&15));
        assert!(!inputs.contains_key(&(3, 64, 3)));
    }

    #[test]
    fn chest_analog_pairing_rejects_mismatched_and_replaced_blocks() {
        let left = (0, 64, 0);
        let right = (1, 64, 0);
        let full_stack = Stack {
            id: 1,
            count: 64,
            damage: 0,
            nbt: None,
        };
        let chests = Chests::default()
            .staged_update(left, [Some(full_stack); CHEST_SLOTS])
            .unwrap();
        let mut world = world::World::new();
        world.import_block(left.0, left.1, left.2, 54 << 4).unwrap();
        // A trapped chest cannot pair with a normal chest.
        world
            .import_block(right.0, right.1, right.2, 146 << 4)
            .unwrap();
        let inputs = combine_chest_comparator_inputs(
            &world,
            &chests.comparator_snapshot(),
            chests.comparator_inputs(),
        );
        assert_eq!(inputs.get(&(0, 64, 0)), Some(&15));
        assert!(!inputs.contains_key(&(1, 64, 0)));

        // Replaced blocks must not create a pair or receive a stale signal.
        world.import_block(left.0, left.1, left.2, 1 << 4).unwrap();
        let inputs = combine_chest_comparator_inputs(
            &world,
            &chests.comparator_snapshot(),
            chests.comparator_inputs(),
        );
        assert!(!inputs.contains_key(&(0, 64, 0)));
        // The snapshot combiner itself must drop stale single-chest entries.
    }

    #[test]
    fn double_chest_comparator_drops_replaced_half_with_stale_inventory() {
        let left = (-2, 64, 4);
        let right = (-1, 64, 4);
        let stack = Stack {
            id: 1,
            count: 64,
            damage: 0,
            nbt: None,
        };
        let chests = Chests::default()
            .staged_update(left, [Some(stack); CHEST_SLOTS])
            .unwrap()
            .staged_update(right, [Some(stack); CHEST_SLOTS])
            .unwrap();
        let mut world = world::World::new();
        world.import_block(left.0, left.1, left.2, 54 << 4).unwrap();
        world
            .import_block(right.0, right.1, right.2, 54 << 4)
            .unwrap();
        let signals = combine_chest_comparator_inputs(
            &world,
            &chests.comparator_snapshot(),
            chests.comparator_inputs(),
        );
        assert_eq!(signals.get(&(-2, 64, 4)), Some(&15));
        assert_eq!(signals.get(&(-1, 64, 4)), Some(&15));

        // The persisted right-half inventory has not been cleared yet, but
        // replacing the block must immediately remove its comparator signal.
        world
            .import_block(right.0, right.1, right.2, 1 << 4)
            .unwrap();
        let signals = combine_chest_comparator_inputs(
            &world,
            &chests.comparator_snapshot(),
            chests.comparator_inputs(),
        );
        assert_eq!(signals.get(&(-2, 64, 4)), Some(&15));
        assert!(!signals.contains_key(&(-1, 64, 4)));
    }

    #[test]
    fn generated_chest_loot_is_claimed_once_even_after_emptying() {
        let file =
            std::env::temp_dir().join(format!("ir1710-dungeon-chest-{}.json", std::process::id()));
        let _ = fs::remove_file(&file);
        let pos = (-12, 25, 44);
        let mut chests = Chests::default();
        let loot = [(265, 2), (296, 3), (331, 1)];
        chests.initialize_generated(&file, pos, loot).unwrap();
        let contents = chests.slots(pos);
        assert_eq!(contents[2].unwrap().id, 265);
        chests.update(&file, pos, [None; CHEST_SLOTS]).unwrap();
        let mut restored = Chests::load(&file).unwrap();
        restored.initialize_generated(&file, pos, loot).unwrap();
        assert!(!restored.is_nonempty(pos));
        assert!(restored.generated_claimed.contains("-12,25,44"));
        let _ = fs::remove_file(file);
    }

    #[test]
    fn shared_chest_persists_and_invalid_entries_fail_closed() {
        let file = std::env::temp_dir().join(format!("ir1710-{}-chests.json", std::process::id()));
        let _ = fs::remove_file(&file);
        let mut chests = Chests::load(&file).unwrap();
        let mut slots = [None; CHEST_SLOTS];
        slots[0] = Some(Stack {
            id: 3,
            count: 7,
            damage: 0,
            nbt: None,
        });
        let pos = (2, 64, -1);
        chests.update(&file, pos, slots).unwrap();
        assert_eq!(chests.revision(), 1);
        assert_eq!(Chests::load(&file).unwrap().slots(pos), slots);
        assert!(chests.is_nonempty(pos));
        slots[0] = None;
        chests.update(&file, pos, slots).unwrap();
        assert!(!chests.is_nonempty(pos));
        assert!(Chests::load(&file).unwrap().entries.is_empty());
        fs::write(&file, b"{\"version\":1,\"entries\":{\"bad\":[]}}").unwrap();
        assert!(Chests::load(&file).is_err());
        let _ = fs::remove_file(file);
    }
}
