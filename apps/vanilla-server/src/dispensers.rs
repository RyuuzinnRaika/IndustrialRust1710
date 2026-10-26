//! Server-owned nine-slot dispenser/dropper inventory foundation.
//! This module does not itself broadcast items or attach a GUI; callers must
//! commit item removal and item spawning transactionally.
use crate::inventory::Stack;
use crate::world;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

pub type Position = (i32, i32, i32);
pub const DEVICE_SLOTS: usize = 9;
const MAX_DEVICES: usize = 512;
const MAX_BYTES: usize = 512 * 1024;
const VERSION: u8 = 1;

fn invalid(s: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, s)
}

fn key((x, y, z): Position) -> String {
    format!("{x},{y},{z}")
}
fn parse_key(k: &str) -> io::Result<Position> {
    let parts: Vec<_> = k.split(',').map(str::parse::<i32>).collect();
    if parts.len() != 3 || parts.iter().any(Result::is_err) {
        return Err(invalid("invalid dispenser coordinates"));
    }
    let pos = (
        *parts[0].as_ref().map_err(|_| invalid("bad x"))?,
        *parts[1].as_ref().map_err(|_| invalid("bad y"))?,
        *parts[2].as_ref().map_err(|_| invalid("bad z"))?,
    );
    if !world::in_bounds(pos.0, pos.1, pos.2) || key(pos) != k {
        return Err(invalid("noncanonical dispenser position"));
    }
    Ok(pos)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Devices {
    version: u8,
    entries: BTreeMap<String, [Option<Stack>; DEVICE_SLOTS]>,
}

impl Default for Devices {
    fn default() -> Self {
        Self {
            version: VERSION,
            entries: BTreeMap::new(),
        }
    }
}

impl Devices {
    pub fn location(world_path: &Path) -> PathBuf {
        world_path.with_extension("dispensers.json")
    }

    pub fn validate(&self) -> io::Result<()> {
        if self.version != VERSION || self.entries.len() > MAX_DEVICES {
            return Err(invalid("invalid device inventory version or count"));
        }
        for (pos, slots) in &self.entries {
            parse_key(pos)?;
            if slots.iter().flatten().any(|item| !item.is_valid()) {
                return Err(invalid("invalid device stack"));
            }
        }
        Ok(())
    }

    pub fn encoded(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| invalid("device encoding failed"))?;
        if bytes.len() > MAX_BYTES {
            return Err(invalid("device save too large"));
        }
        Ok(bytes)
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        let meta = match fs::symlink_metadata(path) {
            Ok(v) => v,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        if !meta.file_type().is_file() || meta.len() as usize > MAX_BYTES {
            return Err(invalid("invalid device file metadata"));
        }
        let bytes = fs::read(path)?;
        if bytes.len() > MAX_BYTES {
            return Err(invalid("device save too large"));
        }
        let value: Self =
            serde_json::from_slice(&bytes).map_err(|_| invalid("malformed device inventory"))?;
        value.validate()?;
        Ok(value)
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let bytes = self.encoded()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("dispensers.tmp");
        let mut file = File::create(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(tmp, path)
    }

    pub fn slots(&self, pos: Position) -> [Option<Stack>; DEVICE_SLOTS] {
        self.entries
            .get(&key(pos))
            .copied()
            .unwrap_or([None; DEVICE_SLOTS])
    }

    /// Snapshot only nonempty, validated analog sources for the world tick.
    /// Empty or removed inventories are absent, so stale signals are cleared.
    pub fn comparator_inputs(&self) -> BTreeMap<(i32, u8, i32), u8> {
        self.entries
            .iter()
            .filter_map(|(key, slots)| {
                let pos = parse_key(key).ok()?;
                let strength = comparator_strength(slots);
                (strength > 0).then_some(((pos.0, pos.1 as u8, pos.2), strength))
            })
            .collect()
    }

    /// Stage a validated update. Existing state is untouched until commit.
    pub fn staged_update(
        &self,
        pos: Position,
        slots: [Option<Stack>; DEVICE_SLOTS],
    ) -> io::Result<Self> {
        if !world::in_bounds(pos.0, pos.1, pos.2) {
            return Err(invalid("device position outside world"));
        }
        let mut next = self.clone();
        if slots.iter().all(Option::is_none) {
            next.entries.remove(&key(pos));
        } else {
            next.entries.insert(key(pos), slots);
        }
        next.validate()?;
        next.encoded()?;
        Ok(next)
    }

    /// Return a staged inventory and one emitted item. The caller owns the
    /// world/entity journal and must not publish the item before persistence.
    /// The caller supplies a tick seed; mix in all three coordinates to avoid
    /// selecting the same slot on every activation of nearby devices. This is
    /// deterministic for redo-journal replay, not Java Random compatibility.
    pub fn stage_eject(&self, pos: Position, seed: u64) -> io::Result<Option<(Self, Stack)>> {
        if !world::in_bounds(pos.0, pos.1, pos.2) {
            return Err(invalid("device position outside world"));
        }
        let slots = self.slots(pos);
        let filled: Vec<usize> = slots
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| slot.filter(|s| s.count > 0).map(|_| i))
            .collect();
        if filled.is_empty() {
            return Ok(None);
        }
        // Mix sequential tick seeds before reducing to an occupied slot.
        // Multiply-high avoids the low-bit cycles of seed % slot_count.
        let mut random = seed
            ^ (pos.0 as u64).wrapping_mul(0x9e3779b97f4a7c15)
            ^ (pos.1 as u64).wrapping_mul(0xbf58476d1ce4e5b9)
            ^ (pos.2 as u64).wrapping_mul(0x94d049bb133111eb);
        random = random.wrapping_add(0x9e3779b97f4a7c15);
        random = (random ^ (random >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        random = (random ^ (random >> 27)).wrapping_mul(0x94d049bb133111eb);
        random ^= random >> 31;
        let choice = ((u128::from(random) * filled.len() as u128) >> 64) as usize;
        let index = filled[choice];
        let mut updated = slots;
        let mut stack = updated[index].ok_or_else(|| invalid("missing stack"))?;
        let emitted = Stack { count: 1, ..stack };
        stack.count -= 1;
        updated[index] = (stack.count > 0).then_some(stack);
        Ok(Some((self.staged_update(pos, updated)?, emitted)))
    }

    /// Atomically stage a one-item dropper transfer between device inventories.
    /// A full target or empty source preserves the entire source snapshot.
    pub fn stage_transfer_one(
        &self,
        source: Position,
        destination: Position,
        seed: u64,
    ) -> io::Result<Option<Self>> {
        if !world::in_bounds(source.0, source.1, source.2)
            || !world::in_bounds(destination.0, destination.1, destination.2)
        {
            return Err(invalid("device position outside world"));
        }
        if source == destination {
            return Ok(None);
        }
        let Some((staged, item)) = self.stage_eject(source, seed)? else {
            return Ok(None);
        };
        let mut target = staged.slots(destination);
        if !insert_one(&mut target, item) {
            return Ok(None);
        }
        Ok(Some(staged.staged_update(destination, target)?))
    }
}

/// Vanilla 1.7.10 comparator override for a nine-slot device:
/// floor(14 * average slot fullness) + 1 when any slot is occupied.
/// All supported item stack caps (1, 16, 64) divide 64 exactly.
fn comparator_strength(slots: &[Option<Stack>; DEVICE_SLOTS]) -> u8 {
    let mut scaled_fill = 0u32;
    let mut occupied = false;
    for stack in slots.iter().flatten() {
        if stack.is_valid() {
            occupied = true;
            scaled_fill += u32::from(stack.count) * 64 / u32::from(stack.max_count());
        }
    }
    if occupied {
        1 + ((14 * scaled_fill) / (64 * DEVICE_SLOTS as u32)) as u8
    } else {
        0
    }
}

/// Insert exactly one item into a destination container, preferring
/// compatible partial stacks. Returns false without mutating if it is full.
pub fn insert_one<const N: usize>(slots: &mut [Option<Stack>; N], item: Stack) -> bool {
    if item.count != 1 || !item.is_valid() {
        return false;
    }
    for entry in slots.iter_mut() {
        if let Some(stack) = entry.as_mut() {
            if stack.id == item.id
                && stack.damage == item.damage
                && stack.nbt == item.nbt
                && stack.count < stack.max_count()
            {
                stack.count += 1;
                return true;
            }
        }
    }
    for entry in slots.iter_mut() {
        if entry.is_none() {
            *entry = Some(item);
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_comparator_signal_respects_stack_caps_and_empty_slots() {
        let mut slots = [None; DEVICE_SLOTS];
        assert_eq!(comparator_strength(&slots), 0);
        slots[0] = Some(Stack {
            id: 1,
            count: 1,
            damage: 0,
        });
        assert_eq!(comparator_strength(&slots), 1);
        slots[0] = Some(Stack {
            id: 1,
            count: 64,
            damage: 0,
        });
        assert_eq!(comparator_strength(&slots), 2);
        slots.fill(Some(Stack {
            id: 1,
            count: 64,
            damage: 0,
        }));
        assert_eq!(comparator_strength(&slots), 15);
        slots[0] = None;
        assert_eq!(comparator_strength(&slots), 13);
        slots.fill(Some(Stack {
            id: 267,
            count: 1,
            damage: 0,
        }));
        assert_eq!(comparator_strength(&slots), 15);
        slots.fill(Some(Stack {
            id: 323,
            count: 16,
            damage: 0,
        }));
        assert_eq!(comparator_strength(&slots), 15);
    }

    #[test]
    fn device_inventory_changes_reach_comparator_after_delay() {
        let path = std::env::temp_dir().join(format!(
            "ir1710-device-comparator-{}.irw",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut world = world::World::new();
        world.update_and_save(&path, -1, 64, 0, 23 << 4).unwrap();
        world
            .update_and_save(&path, 0, 64, 0, (149 << 4) | 1)
            .unwrap();
        let pos = (-1, 64, 0);
        let full = [Some(Stack {
            id: 1,
            count: 64,
            damage: 0,
        }); DEVICE_SLOTS];
        let storage = Devices::default().staged_update(pos, full).unwrap();
        world.set_device_comparator_inputs(storage.comparator_inputs());
        for _ in 0..3 {
            world.block_tick(&path, 64).unwrap();
        }
        assert_eq!(world.get(0, 64, 0) >> 4, 150);
        assert_eq!(world.signal_into((0, 64, 0), (1, 64, 0)), 15);
        assert_eq!(world.signal_into((-1, 64, 0), (0, 64, 0)), 0);

        let empty = storage.staged_update(pos, [None; DEVICE_SLOTS]).unwrap();
        world.set_device_comparator_inputs(empty.comparator_inputs());
        for _ in 0..3 {
            world.block_tick(&path, 64).unwrap();
        }
        assert_eq!(world.get(0, 64, 0) >> 4, 149);
        assert_eq!(world.signal_into((0, 64, 0), (1, 64, 0)), 0);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn device_to_device_transfer_is_atomic_when_destination_is_full() {
        let from = (0, 65, 0);
        let to = (1, 65, 0);
        let mut source = [None; DEVICE_SLOTS];
        source[0] = Some(Stack {
            id: 1,
            count: 2,
            damage: 0,
            nbt: None,
        });
        let full = [Some(Stack {
            id: 1,
            count: 64,
            damage: 0,
            nbt: None,
        }); DEVICE_SLOTS];
        let original = Devices::default()
            .staged_update(from, source)
            .unwrap()
            .staged_update(to, full)
            .unwrap();
        assert!(original.stage_transfer_one(from, to, 0).unwrap().is_none());
        assert_eq!(original.slots(from)[0].unwrap().count, 2);
        let mut available = full;
        available[0] = Some(Stack {
            id: 1,
            count: 63,
            damage: 0,
            nbt: None,
        });
        let original = original.staged_update(to, available).unwrap();
        let transferred = original.stage_transfer_one(from, to, 0).unwrap().unwrap();
        assert_eq!(transferred.slots(from)[0].unwrap().count, 1);
        assert_eq!(transferred.slots(to)[0].unwrap().count, 64);
        assert_eq!(original.slots(from)[0].unwrap().count, 2);
        assert!(original
            .stage_transfer_one(from, from, 0)
            .unwrap()
            .is_none());
    }

    #[test]
    fn transfer_merges_or_uses_empty_slot_without_overflow() {
        let item = Stack {
            id: 4,
            count: 1,
            damage: 0,
            nbt: None,
        };
        let mut slots = [None; 3];
        slots[0] = Some(Stack { count: 63, ..item });
        assert!(insert_one(&mut slots, item));
        assert_eq!(slots[0].unwrap().count, 64);
        assert!(insert_one(&mut slots, item));
        assert_eq!(slots[1], Some(item));
        let full = Stack { count: 64, ..item };
        slots.fill(Some(full));
        assert!(!insert_one(&mut slots, item));
        assert_eq!(slots, [Some(full); 3]);
    }

    #[test]
    fn transfer_rejects_multi_item_batches() {
        let mut slots = [None; 2];
        assert!(!insert_one(
            &mut slots,
            Stack {
                id: 4,
                count: 2,
                damage: 0,
                nbt: None,
            }
        ));
        assert_eq!(slots, [None; 2]);
    }

    #[test]
    fn nine_slots_are_durable_and_ejection_is_staged() {
        let path =
            std::env::temp_dir().join(format!("ir1710-dispensers-{}.json", std::process::id()));
        let _ = fs::remove_file(&path);
        let pos = (2, 64, 3);
        let mut slots = [None; DEVICE_SLOTS];
        slots[8] = Some(Stack {
            id: 4,
            count: 2,
            damage: 0,
            nbt: None,
        });
        let storage = Devices::default().staged_update(pos, slots).unwrap();
        storage.save(&path).unwrap();
        let loaded = Devices::load(&path).unwrap();
        let (next, emitted) = loaded.stage_eject(pos, 7).unwrap().unwrap();
        assert_eq!(emitted.count, 1);
        assert_eq!(emitted.id, 4);
        assert_eq!(loaded.slots(pos)[8].unwrap().count, 2);
        assert_eq!(next.slots(pos)[8].unwrap().count, 1);
        next.save(&path).unwrap();
        assert_eq!(
            Devices::load(&path).unwrap().slots(pos)[8].unwrap().count,
            1
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn ejection_uses_only_occupied_slots_and_varies_across_tick_seeds() {
        let pos = (12, 64, -7);
        let mut slots = [None; DEVICE_SLOTS];
        for index in [0, 4, 8] {
            slots[index] = Some(Stack {
                id: (index + 1) as i16,
                count: 2,
                damage: 0,
            });
        }
        let devices = Devices::default().staged_update(pos, slots).unwrap();
        let mut seen = [false; DEVICE_SLOTS];
        for seed in 0..512 {
            let (next, emitted) = devices.stage_eject(pos, seed).unwrap().unwrap();
            let selected = [0, 4, 8]
                .into_iter()
                .find(|&index| next.slots(pos)[index] != slots[index])
                .unwrap();
            assert_eq!(emitted.id, (selected + 1) as i16);
            assert_eq!(emitted.count, 1);
            assert_eq!(next.slots(pos)[selected].unwrap().count, 1);
            for (index, slot) in slots.iter().enumerate() {
                if index != selected {
                    assert_eq!(next.slots(pos)[index], *slot);
                }
            }
            // Selection must be reproducible for the same redo-journal seed.
            assert_eq!(
                next.slots(pos),
                devices
                    .stage_eject(pos, seed)
                    .unwrap()
                    .unwrap()
                    .0
                    .slots(pos)
            );
            seen[selected] = true;
        }
        assert!(seen[0] && seen[4] && seen[8]);
        assert!(seen
            .iter()
            .enumerate()
            .all(|(index, hit)| [0, 4, 8].contains(&index) || !hit));
        assert_eq!(devices.slots(pos), slots);
    }

    #[test]
    fn invalid_device_coordinates_fail_closed_before_ejection_or_transfer() {
        let source = (0, 65, 0);
        let invalid_position = (0, -1, 0);
        let mut slots = [None; DEVICE_SLOTS];
        slots[0] = Some(Stack {
            id: 1,
            count: 1,
            damage: 0,
        });
        let devices = Devices::default().staged_update(source, slots).unwrap();
        assert!(devices.stage_eject(invalid_position, 0).is_err());
        assert!(devices
            .stage_transfer_one(source, invalid_position, 0)
            .is_err());
        assert!(devices
            .stage_transfer_one(invalid_position, source, 0)
            .is_err());
        assert_eq!(devices.slots(source), slots);
    }

    #[test]
    fn failed_device_staging_preserves_the_next_valid_activation() {
        let valid = (1, 64, 1);
        let invalid = (1, -1, 1);
        let mut slots = [None; DEVICE_SLOTS];
        slots[0] = Some(Stack {
            id: 4,
            count: 2,
            damage: 0,
        });
        let devices = Devices::default().staged_update(valid, slots).unwrap();

        // The worker must skip a failed activation, not abort its 20TPS loop.
        assert!(devices.stage_eject(invalid, 7).is_err());
        assert!(devices.stage_transfer_one(valid, invalid, 7).is_err());
        assert_eq!(devices.slots(valid), slots);

        let (next, emitted) = devices.stage_eject(valid, 8).unwrap().unwrap();
        assert_eq!(emitted.id, 4);
        assert_eq!(emitted.count, 1);
        assert_eq!(next.slots(valid)[0].unwrap().count, 1);
        assert_eq!(devices.slots(valid), slots);
    }

    #[test]
    fn empty_ejection_cannot_create_items() {
        assert!(Devices::default()
            .stage_eject((0, 64, 0), 0)
            .unwrap()
            .is_none());
    }
}
