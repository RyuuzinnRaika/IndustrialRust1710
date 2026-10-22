//! Minecraft 1.7.10 special blocks, driven by World::block_tick (20TPS).
//! All multi-cell player actions are staged and saved as a single IRW snapshot.
use super::{in_bounds, Position, World};
use crate::inventory::Stack;
use std::collections::BTreeMap;
use std::io;
use std::path::Path;

/// Primed TNT is an entity and can no longer be mined as a TNT block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PrimedTnt {
    entity_id: i32,
    pos: (i32, i32, i32),
    explode_at: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TntEvent {
    Spawn { entity_id: i32, pos: (i32, i32, i32) },
    Explode { entity_id: i32, pos: (i32, i32, i32) },
}

impl World {
    fn spawn_primed_tnt(&mut self, x: i32, y: i32, z: i32, fuse: u64) {
        self.next_tnt_id = if self.next_tnt_id < 10_000_000 {
            10_000_001
        } else {
            self.next_tnt_id.saturating_add(1)
        };
        let entity_id = self.next_tnt_id;
        self.primed_tnt.insert(entity_id, PrimedTnt {
            entity_id,
            pos: (x, y, z),
            explode_at: self.fluid_phase.saturating_add(fuse),
        });
        self.tnt_events.push(TntEvent::Spawn { entity_id, pos: (x, y, z) });
    }

    /// Direct flint/redstone ignition commits removal before entity spawn.
    pub fn ignite_tnt(
        &mut self, path: &Path, x: i32, y: i32, z: i32,
    ) -> io::Result<bool> {
        if self.get(x, y, z) >> 4 != 46 {
            return Ok(false);
        }
        if !self.update_and_save(path, x, y, z, 0)? {
            return Ok(false);
        }
        self.spawn_primed_tnt(x, y, z, 80);
        Ok(true)
    }

    /// Publish only events from committed state.
    pub fn drain_tnt_events(&mut self) -> Vec<TntEvent> {
        std::mem::take(&mut self.tnt_events)
    }

    pub(super) fn finalize_tnt(&mut self, newly_primed: &[(i32, i32, i32)]) {
        let expired: Vec<_> = self.primed_tnt.values()
            .filter(|t| t.explode_at <= self.fluid_phase)
            .take(4)
            .copied()
            .collect();
        for tnt in expired {
            self.primed_tnt.remove(&tnt.entity_id);
            self.tnt_events.push(TntEvent::Explode {
                entity_id: tnt.entity_id, pos: tnt.pos,
            });
        }
        for &(x, y, z) in newly_primed {
            self.spawn_primed_tnt(x, y, z, 25);
        }
    }

    /// A bounded, transactional edit for structures larger than a door.
    /// The original World (including wake queues) survives any save failure.
    pub(super) fn special_batch(
        &mut self,
        path: &Path,
        changes: &BTreeMap<Position, u16>,
    ) -> io::Result<Vec<(i32, i32, i32)>> {
        if changes.is_empty() {
            return Ok(Vec::new());
        }
        if changes.len() > 128 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "special block batch too large"));
        }
        let mut candidate = self.clone();
        let mut changed = Vec::new();
        for (&(x, y, z), &value) in changes {
            if !in_bounds(x, i32::from(y), z) || value >> 4 > 255 {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "special block out of bounds"));
            }
            let old = candidate.get(x, i32::from(y), z);
            if old != value {
                if value == candidate.base_block(x, y, z) {
                    candidate.edits.remove(&(x, y, z));
                    candidate.wake_gravity(x, i32::from(y), z);
                    candidate.wake_block_updates(x, i32::from(y), z);
                } else {
                    candidate.import_block(x, i32::from(y), z, value)?;
                }
                candidate.mark_lighting_changed(x, z, old, value);
                changed.push((x, i32::from(y), z));
            }
        }
        if !changed.is_empty() {
            candidate.save(path)?;
            *self = candidate;
        }
        Ok(changed)
    }

    /// ID 118 encodes 0..3 water levels directly in persistent block metadata.
    /// Returns a replacement item only for a valid interaction. The caller
    /// must commit the held-stack exchange before exposing the block edit.
    pub fn cauldron_exchange(&self, x: i32, y: i32, z: i32, held: Stack) -> Option<(u16, Stack)> {
        let state = self.get(x, y, z);
        if state >> 4 != 118 {
            return None;
        }
        let level = state & 3;
        let (next, output) = match (held.id, held.damage) {
            (326, _) if level != 3 => (3, Stack { id: 325, count: 1, damage: 0 }),
            (373, 0) if level < 3 => (level + 1, Stack { id: 374, count: 1, damage: 0 }),
            (374, _) if level > 0 => (level - 1, Stack { id: 373, count: 1, damage: 0 }),
            (325, _) if level == 3 => (0, Stack { id: 326, count: 1, damage: 0 }),
            _ => return None,
        };
        Some(((118 << 4) | next, output))
    }

    /// Returns the number of complete beacon layers (0..=4), with a clear
    /// vertical beam. Only vanilla pyramid minerals count as base material.
    pub fn beacon_level(&self, x: i32, y: i32, z: i32) -> u8 {
        if self.get(x, y, z) >> 4 != 138 {
            return 0;
        }
        for sy in y + 1..=255 {
            if !matches!(self.get(x, sy, z) >> 4, 0 | 20 | 95 | 102 | 160) {
                return 0;
            }
        }
        let mut level = 0u8;
        for layer in 1..=4 {
            if y - layer < 0 {
                break;
            }
            let mut valid = true;
            for dx in -layer..=layer {
                for dz in -layer..=layer {
                    if !matches!(self.get(x + dx, y - layer, z + dz) >> 4, 41 | 42 | 57 | 133) {
                        valid = false;
                        break;
                    }
                }
                if !valid {
                    break;
                }
            }
            if !valid {
                break;
            }
            level += 1;
        }
        level
    }

    /// Beam radius is 10 + 10 per complete layer in 1.7.10.
    /// A full beacon selection GUI/TileEntity is not yet modelled.
    pub fn beacon_in_range(&self, x: f64, y: f64, z: f64) -> bool {
        self.edits.iter().any(|(&(bx, by, bz), &block)| {
            if block >> 4 != 138 {
                return false;
            }
            let level = self.beacon_level(bx, i32::from(by), bz);
            let radius = f64::from(level) * 10.0 + 10.0;
            level > 0
                && (x - f64::from(bx)).abs() <= radius
                && (z - f64::from(bz)).abs() <= radius
                && (y - f64::from(by)).abs() <= radius
        })
    }

    /// Legacy portal frames have orientation in bits 0..1 and Eye in bit 2.
    /// Only a complete 3x3 ring is activated; the center is updated atomically.
    pub fn insert_end_eye(
        &mut self, path: &Path, x: i32, y: i32, z: i32,
    ) -> io::Result<Vec<(i32, i32, i32)>> {
        let old = self.get(x, y, z);
        if old >> 4 != 120 || old & 4 != 0 {
            return Ok(Vec::new());
        }
        let mut changes = BTreeMap::new();
        changes.insert((x, y as u8, z), old | 4);
        for cx in x - 2..=x + 2 {
            for cz in z - 2..=z + 2 {
                // Each possible center is at most two blocks away from the new eye.
                if !(((cx - x).abs() == 2 && (cz - z).abs() <= 1)
                    || ((cz - z).abs() == 2 && (cx - x).abs() <= 1)) {
                    continue;
                }
                let mut ring = true;
                for dx in -1..=1i32 {
                    for dz in -1..=1i32 {
                        for (rx, rz) in [(dx, -2), (dx, 2), (-2, dz), (2, dz)] {
                            let state = if (cx + rx, cz + rz) == (x, z) {
                                old | 4
                            } else {
                                self.get(cx + rx, y, cz + rz)
                            };
                            if state >> 4 != 120 || state & 4 == 0 {
                                ring = false;
                                break;
                            }
                        }
                    }
                }
                if ring {
                    let empty = (-1..=1).all(|dx| (-1..=1).all(|dz| {
                        matches!(self.get(cx + dx, y, cz + dz) >> 4, 0 | 119)
                    }));
                    if empty {
                        for dx in -1..=1 {
                            for dz in -1..=1 {
                                changes.insert((cx + dx, y as u8, cz + dz), 119 << 4);
                            }
                        }
                        return self.special_batch(path, &changes);
                    }
                }
            }
        }
        self.special_batch(path, &changes)
    }

    /// Bone meal applies vanilla-style full crop maturity, one sapling tree,
    /// or short-grass/flowers; it never overwrites an occupied build.
    pub fn apply_bonemeal(
        &mut self, path: &Path, x: i32, y: i32, z: i32,
    ) -> io::Result<Vec<(i32, i32, i32)>> {
        if !in_bounds(x, y, z) {
            return Ok(Vec::new());
        }
        let block = self.get(x, y, z);
        let mut changes = BTreeMap::new();
        match block >> 4 {
            59 | 141 | 142 | 104 | 105 if block & 7 < 7 => {
                changes.insert((x, y as u8, z), (block & !7) | 7);
            }
            6 => self.grow_tree(x, y, z, &mut changes),
            2 => {
                for dx in -2..=2 {
                    for dz in -2..=2 {
                        let px = x + dx;
                        let pz = z + dz;
                        if in_bounds(px, y + 1, pz)
                            && self.get(px, y, pz) >> 4 == 2
                            && self.get(px, y + 1, pz) == 0
                        {
                            let flower = if (px ^ pz) & 3 == 0 { 37 } else { 31 };
                            changes.insert((px, (y + 1) as u8, pz), flower << 4);
                        }
                    }
                }
            }
            _ => {}
        }
        self.special_batch(path, &changes)
    }

    /// Approximate 1.7.10 daylight detector curve: noon=15, midnight=0,
    /// cloud/rain dim the sensor. This runs only on registered sensor blocks.
    pub(super) fn daylight_strength(&self) -> u16 {
        let angle = (self.day_time.rem_euclid(24_000) as f64 / 24_000.0)
            * std::f64::consts::TAU;
        let day = ((angle.sin() + 0.15) * 15.0).round().clamp(0.0, 15.0) as u16;
        day.saturating_sub(if self.raining { 3 } else { 0 })
    }

    /// Additional scheduled state changes are merged into the existing
    /// fluid/ecology/devices single-snapshot commit.
    pub(super) fn special_changes(&self, budget: usize) -> BTreeMap<Position, u16> {
        let mut changes = BTreeMap::new();
        let len = self.pending_devices.len();
        if len > 0 {
            let start = (self.fluid_phase as usize).wrapping_mul(budget) % len;
            for &(x, y, z) in self.pending_devices.iter().cycle().skip(start).take(budget.min(len)) {
                let state = self.get(x, i32::from(y), z);
                if state >> 4 == 151 {
                    let level = if (i32::from(y) + 1..=255).all(|sy| {
                        matches!(self.get(x, sy, z) >> 4, 0 | 20 | 95 | 102 | 160)
                    }) { self.daylight_strength() } else { 0 };
                    if state & 15 != level {
                        changes.insert((x, y, z), (151 << 4) | level);
                    }
                }
            }
        }
        // Redstone-powered TNT becomes an entity, not an immediately
        // exploding block. Chain-primed blocks use the shorter legacy fuse.
        for &(x, y, z) in self.pending_devices.iter().take(budget) {
            if self.get(x, i32::from(y), z) >> 4 == 46
                && self.powered_near(x, i32::from(y), z)
            {
                changes.insert((x, y, z), 0);
            }
        }
        for tnt in self.primed_tnt.values()
            .filter(|t| t.explode_at <= self.fluid_phase)
            .take(4)
        {
            let (x, y, z) = tnt.pos;
            for dx in -3i32..=3 {
                for dy in -3i32..=3 {
                    for dz in -3i32..=3 {
                        if dx * dx + dy * dy + dz * dz > 9 { continue; }
                        let (px, py, pz) = (x + dx, y + dy, z + dz);
                        if !in_bounds(px, py, pz) { continue; }
                        let id = self.get(px, py, pz) >> 4;
                        if id != 0 && !matches!(id, 7 | 49 | 90 | 119 | 120) {
                            changes.insert((px, py as u8, pz), 0);
                        }
                    }
                }
            }
        }
        changes
    }

    /// A weighted pressure plate's metadata is its 0..15 analog output.
    /// Gold (147) needs one entity per level; iron (148) needs ten.
    pub fn press_weighted_plate(
        &mut self, path: &Path, x: i32, y: i32, z: i32, count: usize,
    ) -> io::Result<bool> {
        if !in_bounds(x, y, z) {
            return Ok(false);
        }
        let state = self.get(x, y, z);
        let level = match state >> 4 {
            147 => count.min(15) as u16,
            148 => count.div_ceil(10).min(15) as u16,
            _ => return Ok(false),
        };
        // Keep the plate awake in the existing device timer, even when the
        // strength did not change on this packet.
        if count > 0 {
            self.pending_buttons.insert((x, y as u8, z), self.fluid_phase + 10);
        }
        self.update_and_save(path, x, y, z, (state & !15) | level)
    }

    /// Probe at player feet or shoulder height. Two hooks connected by
    /// contiguous string in one axis become powered for ten server ticks.
    pub fn touch_tripwire(
        &mut self, path: &Path, x: i32, y: i32, z: i32,
    ) -> io::Result<Vec<(i32, i32, i32)>> {
        if !in_bounds(x, y, z) || self.get(x, y, z) >> 4 != 132 {
            return Ok(Vec::new());
        }
        for (dx, dz) in [(1, 0), (0, 1)] {
            let mut hooks = Vec::new();
            for direction in [-1, 1] {
                for distance in 1..=40 {
                    let px = x + dx * distance * direction;
                    let pz = z + dz * distance * direction;
                    if !in_bounds(px, y, pz) {
                        break;
                    }
                    let block = self.get(px, y, pz);
                    if block >> 4 == 131 {
                        hooks.push((px, pz));
                        break;
                    }
                    if block >> 4 != 132 {
                        break;
                    }
                }
            }
            if hooks.len() == 2 {
                let cells: Vec<_> = hooks.iter().map(|&(px, pz)| {
                    (px, y, pz, self.get(px, y, pz) | 8)
                }).collect();
                let changed = self.update_many_and_save(path, &cells)?;
                for &(px, pz) in &hooks {
                    self.pending_buttons.insert((px, y as u8, pz), self.fluid_phase + 10);
                }
                if changed {
                    return Ok(hooks.into_iter().map(|(px, pz)| (px, y, pz)).collect());
                }
                return Ok(Vec::new());
            }
        }
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn file(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("ir1710-h-{name}-{}.irw", std::process::id()))
    }

    #[test]
    fn cauldron_metadata_and_item_conservation() {
        let mut w = World::new();
        let path = file("cauldron");
        w.update_and_save(&path, 1, 64, 1, 118 << 4).unwrap();
        let bucket = Stack { id: 326, count: 1, damage: 0 };
        let (state, returned) = w.cauldron_exchange(1, 64, 1, bucket).unwrap();
        assert_eq!(state & 15, 3);
        assert_eq!(returned.id, 325);
        w.update_and_save(&path, 1, 64, 1, state).unwrap();
        assert_eq!(World::load(&path).unwrap().get(1, 64, 1) & 15, 3);
        fs::remove_file(path).ok();
    }

    #[test]
    fn beacon_requires_every_layer_and_open_sky() {
        let mut w = World::new();
        w.import_block(0, 70, 0, 138 << 4).unwrap();
        for x in -1..=1 {
            for z in -1..=1 {
                w.import_block(x, 69, z, 42 << 4).unwrap();
            }
        }
        assert_eq!(w.beacon_level(0, 70, 0), 1);
        w.import_block(1, 69, 1, 0).unwrap();
        assert_eq!(w.beacon_level(0, 70, 0), 0);
    }

    #[test]
    fn end_eye_never_consumes_an_existing_eye() {
        let mut w = World::new();
        let path = file("eye");
        w.update_and_save(&path, 3, 65, 3, (120 << 4) | 4).unwrap();
        assert!(w.insert_end_eye(&path, 3, 65, 3).unwrap().is_empty());
        fs::remove_file(path).ok();
    }

    #[test]
    fn weighted_plate_strength_and_timer() {
        let path = file("plate");
        let mut w = World::new();
        w.update_and_save(&path, 1, 64, 1, 148 << 4).unwrap();
        assert!(w.press_weighted_plate(&path, 1, 64, 1, 11).unwrap());
        assert_eq!(w.get(1, 64, 1) & 15, 2);
        fs::remove_file(path).ok();
    }
}
