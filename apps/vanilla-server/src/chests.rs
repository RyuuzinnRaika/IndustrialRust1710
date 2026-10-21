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
        if !staged.entries.contains_key(&key) {
            let mut slots = [None; CHEST_SLOTS];
            for (index, (id, count)) in loot.into_iter().enumerate() {
                let stack = Stack {
                    id,
                    count,
                    damage: 0,
                };
                if !stack.is_valid() {
                    return Err(invalid("invalid generated loot stack"));
                }
                slots[index * 5 + 2] = Some(stack);
            }
            staged.entries.insert(key, slots);
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
#[cfg(test)]
mod tests {
    use super::*;
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
