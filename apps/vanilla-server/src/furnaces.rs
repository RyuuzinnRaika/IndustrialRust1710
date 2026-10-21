//! Persistent, server-authoritative furnace tile state for Minecraft 1.7.10.
//! 20 game ticks = one second. One smelt takes 200 ticks (10 seconds).
//! Items, burn and cook progress are committed in ONE atomic JSON replacement.
//! World block and player inventories remain separate crash-consistency domains.
use crate::inventory::Stack;
use crate::world;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

pub type Position = (i32, i32, i32);
pub const FURNACE_SLOTS: usize = 3;
pub const COOK_TICKS: u16 = 200;
const MAX_FURNACES: usize = 512;
const MAX_BYTES: u64 = 256 * 1024;
const VERSION: u8 = 1;

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, reason)
}
fn key((x, y, z): Position) -> String {
    format!("{x},{y},{z}")
}

/// Legacy 1.7.10 subset. All outputs are a single item and have no NBT.
pub fn recipe(input: Stack) -> Option<Stack> {
    let (id, damage) = match (input.id, input.damage) {
        (15, _) => (265, 0),                 // iron ore -> iron ingot
        (14, _) => (266, 0),                 // gold ore -> gold ingot
        (12, _) => (20, 0),                  // sand -> glass
        (4, _) => (1, 0),                    // cobblestone -> stone
        (319, _) => (320, 0),                // raw pork
        (363, _) => (364, 0),                // raw beef
        (365, _) => (366, 0),                // raw chicken
        (392, _) => (393, 0),                // potato
        (337, _) => (336, 0),                // clay ball -> brick
        (82, _) => (172, 0),                 // clay block -> hardened clay
        (17 | 162, _) => (263, 1),           // logs -> charcoal
        (87, _) => (405, 0),                 // netherrack -> nether brick
        (81, _) => (351, 2),                 // cactus -> cactus green
        (349, 0 | 1) => (350, input.damage), // raw fish/salmon
        _ => return None,
    };
    Some(Stack {
        id,
        count: 1,
        damage,
    })
}
/// Return fuel burn duration in 20Hz ticks. Reject metadata variants that
/// are not fuel, and do not treat modern post-1.7.10 items as valid fuel.
pub fn burn_duration(item: Stack) -> Option<u16> {
    let ticks = match item.id {
        263 if item.damage == 0 || item.damage == 1 => 1600,
        173 => 16000,                    // coal block
        327 if item.count == 1 => 20000, // lava bucket (leaves empty bucket)
        369 => 2400,                     // blaze rod
        280 => 100,                      // stick
        6 => 100,                        // sapling
        5 | 17 | 162 | 54 | 58 | 53 | 85 | 107 | 134 | 135 | 136 | 125 | 126 | 143 | 158 | 47
        | 84 | 96 | 64 => 300,
        268..=271 | 290 => 200, // wooden tools
        _ => return None,
    };
    Some(ticks)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Furnace {
    pub slots: [Option<Stack>; FURNACE_SLOTS],
    pub burn_left: u16,
    pub burn_total: u16,
    pub cook: u16,
}
impl Furnace {
    fn valid(&self) -> bool {
        self.slots.iter().flatten().all(|s| s.is_valid())
            && self.burn_left <= 20000
            && self.burn_total <= 20000
            && self.burn_left <= self.burn_total
            && self.cook < COOK_TICKS
    }
    pub fn is_burning(&self) -> bool {
        self.burn_left > 0
    }
    pub fn occupied(&self) -> bool {
        self.slots.iter().any(Option::is_some) || self.burn_left > 0 || self.cook > 0
    }
    fn can_smelt(&self) -> Option<Stack> {
        let result = recipe(self.slots[0]?)?;
        match self.slots[2] {
            None => Some(result),
            Some(existing)
                if existing.id == result.id
                    && existing.damage == result.damage
                    && existing.count <= 63 =>
            {
                Some(result)
            }
            _ => None,
        }
    }
    /// Returns whether items were consumed or produced (requires immediate
    /// durable save). Progress changes are checkpointed every 20 ticks.
    fn tick_once(&mut self) -> bool {
        if self.burn_left > 0 {
            self.burn_left -= 1;
        }
        let mut items_changed = false;
        let can_smelt = self.can_smelt().is_some();
        if self.burn_left == 0 && can_smelt {
            if let Some(fuel) = self.slots[1] {
                if let Some(duration) = burn_duration(fuel) {
                    self.burn_total = duration;
                    self.burn_left = duration;
                    self.slots[1] = if fuel.id == 327 {
                        Some(Stack {
                            id: 325,
                            count: 1,
                            damage: 0,
                        })
                    } else if fuel.count > 1 {
                        Some(Stack {
                            count: fuel.count - 1,
                            ..fuel
                        })
                    } else {
                        None
                    };
                    items_changed = true;
                }
            }
        }
        if self.burn_left > 0 && can_smelt {
            self.cook += 1;
            if self.cook == COOK_TICKS {
                self.cook = 0;
                let result = self.can_smelt().expect("verified smelting recipe");
                if let Some(mut input) = self.slots[0] {
                    input.count -= 1;
                    self.slots[0] = (input.count > 0).then_some(input);
                }
                self.slots[2] = Some(match self.slots[2] {
                    Some(mut current) => {
                        current.count += 1;
                        current
                    }
                    None => result,
                });
                items_changed = true;
            }
        } else {
            self.cook = 0;
        }
        items_changed
    }
    /// Protocol 5 Window Property packet 0x31: window, property short, value short.
    /// Legacy properties 0 cook, 1 burn remaining, 2 original fuel duration.
    pub fn properties(&self, window: u8) -> [Vec<u8>; 3] {
        [self.cook, self.burn_left, self.burn_total]
            .map(|value| {
                let mut packet = vec![window];
                // Replaced by index in the loop below.
                packet.extend_from_slice(&0i16.to_be_bytes());
                packet.extend_from_slice(&(value as i16).to_be_bytes());
                packet
            })
            .into_iter()
            .enumerate()
            .map(|(id, mut value)| {
                value[1..3].copy_from_slice(&(id as i16).to_be_bytes());
                value
            })
            .collect::<Vec<_>>()
            .try_into()
            .expect("three furnace properties")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Furnaces {
    version: u8,
    entries: BTreeMap<String, Furnace>,
    #[serde(skip)]
    revision: u64,
}
impl Default for Furnaces {
    fn default() -> Self {
        Self {
            version: VERSION,
            entries: BTreeMap::new(),
            revision: 0,
        }
    }
}
impl Furnaces {
    pub fn location(world: &Path) -> PathBuf {
        world.with_extension("furnaces.json")
    }
    fn validate(&self) -> io::Result<()> {
        if self.version != VERSION || self.entries.len() > MAX_FURNACES {
            return Err(invalid("invalid furnace version or count"));
        }
        for (name, state) in &self.entries {
            let coords = name.split(',').map(str::parse::<i32>).collect::<Vec<_>>();
            if coords.len() != 3 || coords.iter().any(Result::is_err) || !state.valid() {
                return Err(invalid("invalid furnace coordinate or state"));
            }
            let pos = (
                coords[0].as_ref().copied().unwrap(),
                coords[1].as_ref().copied().unwrap(),
                coords[2].as_ref().copied().unwrap(),
            );
            if !world::in_bounds(pos.0, pos.1, pos.2) || key(pos) != *name {
                return Err(invalid("invalid furnace position"));
            }
        }
        Ok(())
    }
    pub fn load(path: &Path) -> io::Result<Self> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(v) => v,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        if !metadata.file_type().is_file() || metadata.len() > MAX_BYTES {
            return Err(invalid("invalid furnace save metadata"));
        }
        let bytes = fs::read(path)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid("oversized furnace save"));
        }
        let state: Self =
            serde_json::from_slice(&bytes).map_err(|_| invalid("furnace JSON malformed"))?;
        state.validate()?;
        Ok(state)
    }
    fn save(&self, path: &Path) -> io::Result<()> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| invalid("encode furnace data"))?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid("furnace save exceeds cap"));
        }
        if let Some(parent) = path.parent().filter(|v| !v.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension("furnaces.tmp");
        let mut out = File::create(&temp)?;
        out.write_all(&bytes)?;
        out.sync_all()?;
        drop(out);
        fs::rename(temp, path)
    }
    pub fn encoded(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| invalid("encode furnace data"))?;
        if bytes.len() as u64 > MAX_BYTES { return Err(invalid("furnace save exceeds cap")); }
        Ok(bytes)
    }
    pub fn staged_update(&self, pos: Position, next: Furnace) -> io::Result<Self> {
        if !world::in_bounds(pos.0, pos.1, pos.2) || !next.valid() {
            return Err(invalid("invalid furnace staged update"));
        }
        let mut candidate = self.clone();
        if next == Furnace::default() { candidate.entries.remove(&key(pos)); }
        else { candidate.entries.insert(key(pos), next); }
        candidate.encoded()?;
        candidate.revision = self.revision.wrapping_add(1);
        Ok(candidate)
    }
    pub fn get(&self, pos: Position) -> Furnace {
        self.entries.get(&key(pos)).copied().unwrap_or_default()
    }
    pub fn update(&mut self, path: &Path, pos: Position, next: Furnace) -> io::Result<()> {
        if !world::in_bounds(pos.0, pos.1, pos.2) || !next.valid() {
            return Err(invalid("invalid furnace update"));
        }
        let k = key(pos);
        let old = self.entries.get(&k).copied();
        if old.unwrap_or_default() == next {
            return Ok(());
        }
        if next == Furnace::default() {
            self.entries.remove(&k);
        } else {
            self.entries.insert(k.clone(), next);
        }
        if let Err(error) = self.save(path) {
            match old {
                Some(old) => {
                    self.entries.insert(k, old);
                }
                None => {
                    self.entries.remove(&k);
                }
            }
            return Err(error);
        }
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }
    pub fn advance(&mut self, path: &Path, elapsed_ticks: u32, now_tick: u64) -> io::Result<()> {
        // No real-time catch-up while the server is stopped.
        // Bounded catch-up preserves stability after a stalled host tick.
        let steps = elapsed_ticks.min(20);
        if steps == 0 || self.entries.is_empty() {
            return Ok(());
        }
        let snapshot = self.entries.clone();
        let mut different = false;
        let mut durable = false;
        for furnace in self.entries.values_mut() {
            let before = *furnace;
            for _ in 0..steps {
                durable |= furnace.tick_once();
            }
            different |= before != *furnace;
        }
        if !different {
            return Ok(());
        }
        // Burn is written every second; input/fuel/output commits immediately.
        if durable || now_tick % 20 < u64::from(steps) {
            if let Err(error) = self.save(path) {
                self.entries = snapshot;
                return Err(error);
            }
        }
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }
    pub fn lighting(&self) -> Vec<(Position, bool)> {
        self.entries
            .iter()
            .filter_map(|(name, state)| {
                let coords = name
                    .split(',')
                    .map(|v| v.parse::<i32>().ok())
                    .collect::<Vec<_>>();
                Some((
                    (
                        coords.first()?.as_ref().copied()?,
                        coords.get(1)?.as_ref().copied()?,
                        coords.get(2)?.as_ref().copied()?,
                    ),
                    state.is_burning(),
                ))
            })
            .collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn stack(id: i16, count: u8) -> Option<Stack> {
        Some(Stack {
            id,
            count,
            damage: 0,
        })
    }
    #[test]
    fn recipes_and_fuels_are_legacy_bounded() {
        assert_eq!(recipe(stack(15, 3).unwrap()), stack(265, 1));
        assert_eq!(recipe(stack(4, 1).unwrap()), stack(1, 1));
        assert_eq!(recipe(stack(17, 1).unwrap()).unwrap().damage, 1);
        assert_eq!(recipe(stack(999, 1).unwrap()), None);
        assert_eq!(burn_duration(stack(263, 1).unwrap()), Some(1600));
        assert_eq!(burn_duration(stack(280, 1).unwrap()), Some(100));
        assert_eq!(burn_duration(stack(327, 1).unwrap()), Some(20000));
        assert_eq!(burn_duration(stack(327, 2).unwrap()), None);
        assert_eq!(burn_duration(stack(265, 1).unwrap()), None);
    }
    #[test]
    fn consumes_one_coal_and_smelts_eight_ores_in_200_tick_steps() {
        let mut f = Furnace {
            slots: [stack(15, 8), stack(263, 1), None],
            ..Furnace::default()
        };
        let mut products = 0;
        for _ in 0..1600 {
            f.tick_once();
            if let Some(v) = f.slots[2] {
                products = v.count;
            }
        }
        assert_eq!(products, 8);
        assert_eq!(f.slots[0], None);
        assert_eq!(f.slots[1], None);
        assert_eq!(f.slots[2], stack(265, 8));
        assert_eq!(f.burn_left, 1);
        assert_eq!(f.cook, 0);
        assert!(!f.tick_once());
        assert_eq!(f.burn_left, 0);
        assert_eq!(f.slots[2], stack(265, 8));
    }
    #[test]
    fn blocked_output_does_not_burn_new_fuel_or_consume_input() {
        let mut f = Furnace {
            slots: [stack(15, 1), stack(263, 1), stack(266, 3)],
            ..Furnace::default()
        };
        for _ in 0..201 {
            f.tick_once();
        }
        assert_eq!(f.slots[1], stack(263, 1));
        assert_eq!(f.slots[0], stack(15, 1));
        f.slots[2] = stack(265, 64);
        f.tick_once();
        assert_eq!(f.slots[1], stack(263, 1));
    }
    #[test]
    fn lava_bucket_leaves_empty_bucket_and_state_persists() {
        let f = Furnace {
            slots: [stack(12, 1), stack(327, 1), None],
            ..Furnace::default()
        };
        let mut f = f;
        assert!(f.tick_once());
        assert_eq!(f.slots[1], stack(325, 1));
        assert_eq!(f.burn_total, 20000);
        let path =
            std::env::temp_dir().join(format!("ir1710-{}-furnaces.json", std::process::id()));
        let _ = fs::remove_file(&path);
        let pos = (0, 64, 0);
        let mut all = Furnaces::default();
        all.update(&path, pos, f).unwrap();
        let restored = Furnaces::load(&path).unwrap();
        assert_eq!(restored.get(pos), f);
        all.advance(&path, 20, 20).unwrap();
        assert!(all.get(pos).cook > f.cook);
        assert_eq!(Furnaces::load(&path).unwrap().get(pos), all.get(pos));
        let _ = fs::remove_file(path);
    }
}
