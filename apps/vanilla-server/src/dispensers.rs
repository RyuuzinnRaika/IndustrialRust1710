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
        let value: Self = serde_json::from_slice(&bytes)
            .map_err(|_| invalid("malformed device inventory"))?;
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
        self.entries.get(&key(pos)).copied().unwrap_or([None; DEVICE_SLOTS])
    }

    /// Stage a validated update. Existing state is untouched until commit.
    pub fn staged_update(&self, pos: Position, slots: [Option<Stack>; DEVICE_SLOTS]) -> io::Result<Self> {
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
        let filled: Vec<usize> = slots.iter().enumerate()
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nine_slots_are_durable_and_ejection_is_staged() {
        let path = std::env::temp_dir().join(format!(
            "ir1710-dispensers-{}.json", std::process::id()
        ));
        let _ = fs::remove_file(&path);
        let pos = (2, 64, 3);
        let mut slots = [None; DEVICE_SLOTS];
        slots[8] = Some(Stack { id: 4, count: 2, damage: 0 });
        let storage = Devices::default().staged_update(pos, slots).unwrap();
        storage.save(&path).unwrap();
        let loaded = Devices::load(&path).unwrap();
        let (next, emitted) = loaded.stage_eject(pos, 7).unwrap().unwrap();
        assert_eq!(emitted.count, 1);
        assert_eq!(emitted.id, 4);
        assert_eq!(loaded.slots(pos)[8].unwrap().count, 2);
        assert_eq!(next.slots(pos)[8].unwrap().count, 1);
        next.save(&path).unwrap();
        assert_eq!(Devices::load(&path).unwrap().slots(pos)[8].unwrap().count, 1);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn empty_ejection_cannot_create_items() {
        assert!(Devices::default().stage_eject((0, 64, 0), 0).unwrap().is_none());
    }
}
