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
    /// Deterministic slot selection is a safe foundation; vanilla random
    /// selection can be introduced when the world RNG is integrated.
    pub fn stage_eject(&self, pos: Position, seed: u64) -> io::Result<Option<(Self, Stack)>> {
        let slots = self.slots(pos);
        let filled: Vec<usize> = slots
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| slot.filter(|s| s.count > 0).map(|_| i))
            .collect();
        if filled.is_empty() {
            return Ok(None);
        }
        let index = filled[(seed as usize) % filled.len()];
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
    fn empty_ejection_cannot_create_items() {
        assert!(Devices::default()
            .stage_eject((0, 64, 0), 0)
            .unwrap()
            .is_none());
    }
}
