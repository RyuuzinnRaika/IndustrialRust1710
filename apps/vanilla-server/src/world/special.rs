//! Minecraft 1.7.10 special blocks, driven by World::block_tick (20TPS).
//! All multi-cell player actions are staged and saved as a single IRW snapshot.
use super::{in_bounds, Position, World, MAX_PRIMED_TNT};
use crate::inventory::Stack;
use std::collections::BTreeMap;
use std::io;
use std::path::Path;

/// Primed TNT is an entity and can no longer be mined as a TNT block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PrimedTnt {
    pub(super) entity_id: i32,
    pub(super) pos: (i32, i32, i32),
    pub(super) explode_at: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TntEvent {
    Spawn {
        entity_id: i32,
        pos: (i32, i32, i32),
    },
    Explode {
        entity_id: i32,
        pos: (i32, i32, i32),
    },
}

// Resistance approximations for vanilla 1.7.10's ray attenuation. Keep
// unsupported inventories intact until explosion drops become transactional.
fn blast_resistance(id: u16) -> f64 {
    match id {
        7 | 119 | 120 | 137 => 18_000_000.0,
        49 | 130 => 1_200.0,
        8..=11 => 500.0,
        1 | 4 | 14..=16 | 21 | 41 | 42 | 48 | 56 | 57 | 73 | 74 | 98 => 6.0,
        2 | 3 | 12 | 13 | 24 | 80 | 82 | 159 | 172 => 2.5,
        5 | 17 | 35 | 47 | 53 | 58 | 85 | 86 | 96 | 162 => 3.0,
        20 | 95 | 102 | 160 => 1.5,
        18 | 31 | 32 | 37 | 38 | 50 | 55 | 59 | 78 | 79 | 83 | 106 => 0.2,
        46 => 0.0,
        _ => 1.0,
    }
}

fn blast_breakable(id: u16) -> bool {
    !matches!(
        id,
        7 | 49 | 90 | 119 | 120 | 130
            // Containers have separate persistent inventories and tile NBT.
            // Never silently erase their blocks without journaling their contents.
            | 23 | 54 | 61 | 62 | 63 | 68 | 84 | 116 | 117
            | 137 | 138 | 145 | 146 | 154 | 158
    )
}

type StagedBlockBatch = (World, Vec<(i32, i32, i32)>);
pub type StagedSpecialItem = (World, Vec<(i32, i32, i32)>, Option<Stack>, bool);

impl World {
    fn spawn_primed_tnt(&mut self, x: i32, y: i32, z: i32, fuse: u64) -> bool {
        if self.primed_tnt.len() >= MAX_PRIMED_TNT {
            return false;
        }
        self.next_tnt_id = if self.next_tnt_id < 10_000_000 {
            10_000_001
        } else {
            self.next_tnt_id.saturating_add(1)
        };
        let entity_id = self.next_tnt_id;
        self.primed_tnt.insert(
            entity_id,
            PrimedTnt {
                entity_id,
                pos: (x, y, z),
                explode_at: self.fluid_phase.saturating_add(fuse),
            },
        );
        self.tnt_events.push(TntEvent::Spawn {
            entity_id,
            pos: (x, y, z),
        });
        true
    }

    /// Test-only direct ignition uses the same atomic block/entity staging
    /// as a player's flint-and-steel action, including persisted fuse state.
    #[cfg(test)]
    pub fn ignite_tnt(&mut self, path: &Path, x: i32, y: i32, z: i32) -> io::Result<bool> {
        let held = Stack {
            id: 259,
            count: 1,
            damage: 0,
            nbt: None,
        };
        let Some((staged, changed, _, _)) = self.stage_special_item(x, y, z, held)? else {
            return Ok(false);
        };
        if changed.is_empty() {
            return Ok(false);
        }
        staged.save(path)?;
        *self = staged;
        Ok(true)
    }

    /// The existing world worker can take a pre-detonation snapshot only
    /// on a tick that will actually explode TNT. This preserves blast
    /// occlusion without cloning an entire world on every 20Hz tick.
    pub fn tnt_detonates_next_tick(&self) -> bool {
        let next = self.fluid_phase.saturating_add(1);
        self.primed_tnt.values().any(|tnt| tnt.explode_at <= next)
    }

    /// Preserve pre-explosion ray occlusion without cloning every wake queue,
    /// light cache, and distant block edit in a large active world.
    /// Blast damage reaches at most 8 blocks from TNT (power 4); a 12-block
    /// envelope also covers all 27 body-sample ray endpoints.
    pub fn blast_exposure_snapshot(&self) -> World {
        let mut edits = BTreeMap::new();
        let deadline = self.fluid_phase.saturating_add(1);
        for tnt in self
            .primed_tnt
            .values()
            .filter(|tnt| tnt.explode_at <= deadline)
            .take(4)
        {
            let (cx, cy, cz) = tnt.pos;
            for (&(x, y, z), &block) in self.edits.range(
                (cx.saturating_sub(12), 0, i32::MIN)..=(cx.saturating_add(12), u8::MAX, i32::MAX),
            ) {
                if (i64::from(y) - i64::from(cy)).abs() <= 12
                    && (i64::from(z) - i64::from(cz)).abs() <= 12
                {
                    edits.insert((x, y, z), block);
                }
            }
        }
        World {
            edits,
            generation: self.generation,
            ..World::default()
        }
    }

    /// The positions are needed when a player joins after TNT was ignited.
    /// Read-only: joining does not consume the worker's spawn-event queue.
    pub fn visible_primed_tnt(
        &self,
        around: crate::player::PlayerPosition,
    ) -> Vec<(i32, (i32, i32, i32))> {
        self.primed_tnt
            .values()
            .filter(|tnt| {
                (f64::from(tnt.pos.0) + 0.5 - around.x).abs() <= 48.0
                    && (f64::from(tnt.pos.1) + 0.5 - around.y).abs() <= 48.0
                    && (f64::from(tnt.pos.2) + 0.5 - around.z).abs() <= 48.0
            })
            .map(|tnt| (tnt.entity_id, tnt.pos))
            .collect()
    }

    pub(super) fn tnt_hit_by_blast(&self, pos: (i32, i32, i32)) -> bool {
        self.primed_tnt
            .values()
            .filter(|tnt| tnt.explode_at <= self.fluid_phase)
            .take(4)
            .any(|tnt| {
                let (dx, dy, dz) = (
                    i64::from(pos.0) - i64::from(tnt.pos.0),
                    i64::from(pos.1) - i64::from(tnt.pos.1),
                    i64::from(pos.2) - i64::from(tnt.pos.2),
                );
                dx * dx + dy * dy + dz * dz <= 36
            })
    }

    /// Publish only events from committed state.
    pub fn drain_tnt_events(&mut self) -> Vec<TntEvent> {
        std::mem::take(&mut self.tnt_events)
    }

    pub(super) fn finalize_tnt(&mut self, newly_primed: &[(i32, i32, i32)]) {
        let expired: Vec<_> = self
            .primed_tnt
            .values()
            .filter(|t| t.explode_at <= self.fluid_phase)
            .take(4)
            .copied()
            .collect();
        // Preserve the 1.7.10 10..29 tick chain fuse even for TNT that
        // waited in the durable overflow queue across a server restart.
        let short_fuse = |pos: (i32, i32, i32)| -> u64 {
            let mut seed = self.fluid_phase
                ^ (pos.0 as u32 as u64).wrapping_mul(0x9E3779B185EBCA87)
                ^ (pos.1 as u32 as u64).wrapping_mul(0xC2B2AE3D27D4EB4F)
                ^ (pos.2 as u32 as u64).wrapping_mul(0x165667B19E3779F9);
            seed ^= seed >> 30;
            seed = seed.wrapping_mul(0xBF58476D1CE4E5B9);
            seed ^= seed >> 27;
            seed = seed.wrapping_mul(0x94D049BB133111EB);
            10 + ((seed ^ (seed >> 31)) % 20)
        };
        let primed_fuses: Vec<_> = newly_primed
            .iter()
            .map(|&pos| {
                let queued = self
                    .pending_chain_tnt
                    .contains(&(pos.0, pos.1 as u8, pos.2));
                (
                    pos,
                    if queued || self.tnt_hit_by_blast(pos) {
                        short_fuse(pos)
                    } else {
                        80
                    },
                )
            })
            .collect();
        for tnt in expired {
            self.primed_tnt.remove(&tnt.entity_id);
            self.tnt_events.push(TntEvent::Explode {
                entity_id: tnt.entity_id,
                pos: tnt.pos,
            });
        }
        for ((x, y, z), fuse) in primed_fuses {
            // The block worker reserves free slots before removing TNT blocks.
            // Never create an unpersistable 4097th entity.
            let spawned = self.spawn_primed_tnt(x, y, z, fuse);
            debug_assert!(spawned, "unreserved TNT ignition");
        }
    }

    /// A bounded, transactional edit for structures larger than a door.
    /// The original World (including wake queues) survives any save failure.
    fn stage_special_batch(
        &self,
        changes: &BTreeMap<Position, u16>,
    ) -> io::Result<StagedBlockBatch> {
        if changes.is_empty() {
            return Ok((self.clone(), Vec::new()));
        }
        if changes.len() > 128 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "special block batch too large",
            ));
        }
        let mut candidate = self.clone();
        let mut changed = Vec::new();
        for (&(x, y, z), &value) in changes {
            if !in_bounds(x, i32::from(y), z) || value >> 4 > 255 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "special block out of bounds",
                ));
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
        Ok((candidate, changed))
    }

    #[cfg(test)]
    pub(super) fn special_batch(
        &mut self,
        path: &Path,
        changes: &BTreeMap<Position, u16>,
    ) -> io::Result<Vec<(i32, i32, i32)>> {
        let (candidate, changed) = self.stage_special_batch(changes)?;
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
            (326, _) if level != 3 => (
                3,
                Stack {
                    id: 325,
                    count: 1,
                    damage: 0,
                    nbt: None,
                },
            ),
            (373, 0) if level < 3 => (
                level + 1,
                Stack {
                    id: 374,
                    count: 1,
                    damage: 0,
                    nbt: None,
                },
            ),
            (374, _) if level > 0 => (
                level - 1,
                Stack {
                    id: 373,
                    count: 1,
                    damage: 0,
                    nbt: None,
                },
            ),
            (325, _) if level == 3 => (
                0,
                Stack {
                    id: 326,
                    count: 1,
                    damage: 0,
                    nbt: None,
                },
            ),
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
    fn end_eye_changes(&self, x: i32, y: i32, z: i32) -> BTreeMap<Position, u16> {
        let old = self.get(x, y, z);
        if old >> 4 != 120 || old & 4 != 0 {
            return BTreeMap::new();
        }
        let mut changes = BTreeMap::new();
        changes.insert((x, y as u8, z), old | 4);
        for cx in x - 2..=x + 2 {
            for cz in z - 2..=z + 2 {
                // Each possible center is at most two blocks away from the new eye.
                if !(((cx - x).abs() == 2 && (cz - z).abs() <= 1)
                    || ((cz - z).abs() == 2 && (cx - x).abs() <= 1))
                {
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
                            // Legacy 1.7.10 metadata: 0=S, 1=W, 2=N, 3=E.
                            // Every side of the ring must face the portal center.
                            let facing = if rz == -2 {
                                0
                            } else if rz == 2 {
                                2
                            } else if rx == -2 {
                                3
                            } else {
                                1
                            };
                            if state >> 4 != 120 || state & 4 == 0 || state & 3 != facing {
                                ring = false;
                                break;
                            }
                        }
                    }
                }
                if ring {
                    let empty = (-1..=1).all(|dx| {
                        (-1..=1).all(|dz| matches!(self.get(cx + dx, y, cz + dz) >> 4, 0 | 119))
                    });
                    if empty {
                        for dx in -1..=1 {
                            for dz in -1..=1 {
                                changes.insert((cx + dx, y as u8, cz + dz), 119 << 4);
                            }
                        }
                        return changes;
                    }
                }
            }
        }
        changes
    }

    #[cfg(test)]
    pub fn insert_end_eye(
        &mut self,
        path: &Path,
        x: i32,
        y: i32,
        z: i32,
    ) -> io::Result<Vec<(i32, i32, i32)>> {
        let changes = self.end_eye_changes(x, y, z);
        self.special_batch(path, &changes)
    }

    /// Bone meal applies vanilla-style full crop maturity, one sapling tree,
    /// or short-grass/flowers; it never overwrites an occupied build.
    fn bonemeal_changes(&self, x: i32, y: i32, z: i32) -> BTreeMap<Position, u16> {
        if !in_bounds(x, y, z) {
            return BTreeMap::new();
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
        changes
    }

    /// Stage a player special-item interaction entirely in memory. The caller
    /// atomically commits inventory and world snapshots through the redo journal
    /// before publishing either state or any TNT spawn packet.
    pub fn stage_special_item(
        &self,
        x: i32,
        y: i32,
        z: i32,
        held: Stack,
    ) -> io::Result<Option<StagedSpecialItem>> {
        if !in_bounds(x, y, z) {
            return Ok(None);
        }
        let target = self.get(x, y, z) >> 4;
        let mut changes = BTreeMap::new();
        let (exchange, wears_tool) =
            if let Some((block, replacement)) = self.cauldron_exchange(x, y, z, held) {
                changes.insert((x, y as u8, z), block);
                (Some(replacement), false)
            } else if target == 120 && held.id == 381 {
                changes = self.end_eye_changes(x, y, z);
                (None, false)
            } else if held.id == 351 && held.damage == 15 {
                changes = self.bonemeal_changes(x, y, z);
                (None, false)
            } else if target == 46 && held.id == 259 {
                if self.primed_tnt.len() >= MAX_PRIMED_TNT {
                    // Do not remove the TNT block if no entity can be saved.
                    return Ok(None);
                }
                changes.insert((x, y as u8, z), 0);
                (None, true)
            } else {
                return Ok(None);
            };
        let (mut next, changed) = self.stage_special_batch(&changes)?;
        if wears_tool && !changed.is_empty() {
            next.spawn_primed_tnt(x, y, z, 80);
        }
        Ok(Some((next, changed, exchange, wears_tool)))
    }

    /// Approximate 1.7.10 daylight detector curve: noon=15, midnight=0,
    /// cloud/rain dim the sensor. This runs only on registered sensor blocks.
    pub(super) fn daylight_strength(&self) -> u16 {
        let angle = (self.day_time.rem_euclid(24_000) as f64 / 24_000.0) * std::f64::consts::TAU;
        let day = ((angle.sin() + 0.15) * 15.0).round().clamp(0.0, 15.0) as u16;
        day.saturating_sub(if self.raining { 3 } else { 0 })
    }

    /// Validate a facing hook against a contiguous, non-disarmed wire and
    /// an oppositely facing hook. Both endpoints must be at most 40 strings
    /// apart, as in the legacy pre-flattening data layout.
    fn hook_has_valid_wire(&self, x: i32, y: i32, z: i32, state: u16) -> bool {
        let (dx, dz) = match state & 3 {
            0 => (0, 1),
            1 => (-1, 0),
            2 => (0, -1),
            _ => (1, 0),
        };
        for distance in 1..=41 {
            let px = x + dx * distance;
            let pz = z + dz * distance;
            if !in_bounds(px, y, pz) {
                return false;
            }
            let other = self.get(px, y, pz);
            if other >> 4 == 132 && other & 8 == 0 {
                continue;
            }
            return distance >= 2 && other >> 4 == 131 && other & 3 == ((state & 3) + 2) % 4;
        }
        false
    }

    /// Additional scheduled state changes are merged into the existing
    /// fluid/ecology/devices single-snapshot commit.
    pub(super) fn special_changes(&self, budget: usize) -> BTreeMap<Position, u16> {
        let mut changes = BTreeMap::new();
        let len = self.pending_devices.len();
        if len > 0 {
            let start = (self.fluid_phase as usize).wrapping_mul(budget) % len;
            for &(x, y, z) in self
                .pending_devices
                .iter()
                .cycle()
                .skip(start)
                .take(budget.min(len))
            {
                let state = self.get(x, i32::from(y), z);
                if state >> 4 == 131 {
                    let linked = self.hook_has_valid_wire(x, i32::from(y), z, state);
                    let updated = if linked { state | 4 } else { state & !12 };
                    if state != updated {
                        changes.insert((x, y, z), updated);
                    }
                }
                if state >> 4 == 151 {
                    let level = if (i32::from(y) + 1..=255)
                        .all(|sy| matches!(self.get(x, sy, z) >> 4, 0 | 20 | 95 | 102 | 160))
                    {
                        self.daylight_strength()
                    } else {
                        0
                    };
                    if state & 15 != level {
                        changes.insert((x, y, z), (151 << 4) | level);
                    }
                }
            }
        }
        // Snow and ice use nearby edited surface columns, never a global
        // biome/chunk scan. Only a small share of this tick's existing budget.
        let weather_len = self.pending_weather.len();
        if self.raining && weather_len != 0 && self.fluid_phase.is_multiple_of(4) {
            let offset = (self.fluid_phase as usize).wrapping_mul(budget) % weather_len;
            for &(x, y, z) in self
                .pending_weather
                .iter()
                .cycle()
                .skip(offset)
                .take(budget.min(weather_len))
            {
                let y = i32::from(y);
                if !self.is_cold_biome(x, z)
                    || !in_bounds(x, y + 1, z)
                    || !(y + 1..=255).all(|sy| self.get(x, sy, z) == 0)
                {
                    continue;
                }
                match self.get(x, y, z) >> 4 {
                    1 | 2 | 3 | 4 | 5 | 12 | 13 | 24 | 80 => {
                        changes.insert((x, (y + 1) as u8, z), 78 << 4);
                    }
                    8 | 9 if self.get(x, y, z) & 15 == 0 => {
                        changes.insert((x, y as u8, z), 79 << 4);
                    }
                    _ => {}
                }
            }
        }
        // Redstone-powered TNT becomes an entity, not an immediately
        // exploding block. Chain-primed blocks use the shorter legacy fuse.
        if len != 0 {
            let offset = (self.fluid_phase as usize).wrapping_mul(budget) % len;
            for &(x, y, z) in self
                .pending_devices
                .iter()
                .cycle()
                .skip(offset)
                .take(budget.min(len))
            {
                if self.get(x, i32::from(y), z) >> 4 == 46 && self.powered_near(x, i32::from(y), z)
                {
                    changes.insert((x, y, z), 0);
                }
            }
        }
        for &(x, y, z) in self.pending_chain_tnt.iter().take(budget) {
            if self.get(x, i32::from(y), z) >> 4 == 46 {
                changes.insert((x, y, z), 0);
            }
        }
        for tnt in self
            .primed_tnt
            .values()
            .filter(|t| t.explode_at <= self.fluid_phase)
            .take(4)
        {
            // Vanilla Explosion.doExplosionA: cast 1352 rays from the faces
            // of a 16^3 cube, stepping by 0.3 and attenuating by resistance.
            // Per-entity seeded strengths make restart/replay deterministic.
            let (tx, ty, tz) = tnt.pos;
            for ix in 0..16 {
                for iy in 0..16 {
                    for iz in 0..16 {
                        if ix != 0 && ix != 15 && iy != 0 && iy != 15 && iz != 0 && iz != 15 {
                            continue;
                        }
                        let (dx, dy, dz) = (
                            f64::from(ix) / 15.0 * 2.0 - 1.0,
                            f64::from(iy) / 15.0 * 2.0 - 1.0,
                            f64::from(iz) / 15.0 * 2.0 - 1.0,
                        );
                        let length = (dx * dx + dy * dy + dz * dz).sqrt();
                        let mut px = f64::from(tx) + 0.5;
                        let mut py = f64::from(ty) + 0.5;
                        let mut pz = f64::from(tz) + 0.5;
                        let mut seed = (tnt.entity_id as u64)
                            ^ self.fluid_phase.rotate_left(19)
                            ^ ((ix * 256 + iy * 16 + iz) as u64);
                        seed ^= seed >> 30;
                        seed = seed.wrapping_mul(0xBF58476D1CE4E5B9);
                        seed ^= seed >> 27;
                        let random =
                            (seed.wrapping_mul(0x94D049BB133111EB) >> 40) as f64 / 16_777_216.0;
                        let mut strength = 4.0 * (0.7 + random * 0.6);
                        while strength > 0.0 {
                            let (x, y, z) =
                                (px.floor() as i32, py.floor() as i32, pz.floor() as i32);
                            if !in_bounds(x, y, z) {
                                break;
                            }
                            let id = self.get(x, y, z) >> 4;
                            if id != 0 {
                                strength -= (blast_resistance(id) + 0.3) * 0.3;
                                if strength > 0.0 && blast_breakable(id) {
                                    changes.insert((x, y as u8, z), 0);
                                }
                            }
                            px += dx / length * 0.3;
                            py += dy / length * 0.3;
                            pz += dz / length * 0.3;
                            strength -= 0.225;
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
        &mut self,
        path: &Path,
        x: i32,
        y: i32,
        z: i32,
        count: usize,
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
            self.pending_buttons
                .insert((x, y as u8, z), self.fluid_phase + 10);
        }
        self.update_and_save(path, x, y, z, (state & !15) | level)
    }

    /// Probe at player feet or shoulder height. A valid tripwire must join
    /// two inward-facing hooks over at most 40 string blocks. Publish the
    /// hook and wire metadata together, using one world snapshot.
    pub fn touch_tripwire(
        &mut self,
        path: &Path,
        x: i32,
        y: i32,
        z: i32,
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
                        // Hook metadata is its outward-facing orientation,
                        // not the facing of the supporting wall.
                        let facing = match (dx, dz, direction) {
                            (1, 0, -1) => 3, // west hook points east
                            (1, 0, 1) => 1,  // east hook points west
                            (0, 1, -1) => 0, // north hook points south
                            _ => 2,          // south hook points north
                        };
                        if block & 3 == facing {
                            hooks.push((px, pz, distance, direction));
                        }
                        break;
                    }
                    if block >> 4 != 132 || block & 8 != 0 {
                        break; // A missing or disarmed string breaks the circuit.
                    }
                }
            }
            if hooks.len() != 2 || hooks[0].2 + hooks[1].2 - 1 > 40 {
                continue;
            }
            let mut batch = BTreeMap::new();
            for &(px, pz, _, _) in &hooks {
                batch.insert((px, y as u8, pz), self.get(px, y, pz) | 4 | 8);
            }
            // Bits 0 and 2 indicate triggered and attached tripwire.
            for distance in -(hooks[0].2 - 1)..=(hooks[1].2 - 1) {
                let px = x + dx * distance;
                let pz = z + dz * distance;
                let old = self.get(px, y, pz);
                if old >> 4 != 132 {
                    return Ok(Vec::new());
                }
                batch.insert((px, y as u8, pz), old | 1 | 4);
            }
            let expires = self.fluid_phase.saturating_add(10);
            if batch
                .iter()
                .all(|(&(px, py, pz), &state)| self.get(px, i32::from(py), pz) == state)
            {
                // Holding an activated wire must not clone/save the entire
                // world on every contact sample. Just renew the timers.
                for &pos in batch.keys() {
                    self.pending_buttons.insert(pos, expires);
                }
                return Ok(Vec::new());
            }
            let (mut candidate, changed) = self.stage_special_batch(&batch)?;
            for (&pos, &block) in &batch {
                if matches!(block >> 4, 131 | 132) {
                    candidate.pending_buttons.insert(pos, expires);
                }
            }
            if !changed.is_empty() {
                candidate.save(path)?;
            }
            *self = candidate;
            return Ok(changed);
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
        let bucket = Stack {
            id: 326,
            count: 1,
            damage: 0,
            nbt: None,
        };
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
    fn portal_ring_activates_nine_tiles_and_persists() {
        let path = file("portal");
        let mut world = World::new();
        let y = 80;
        for dx in -1..=1 {
            for (px, pz) in [(dx, -2), (dx, 2), (-2, dx), (2, dx)] {
                let facing = if pz == -2 {
                    0
                } else if pz == 2 {
                    2
                } else if px == -2 {
                    3
                } else {
                    1
                };
                let eye = if (px, pz) == (0, -2) { 0 } else { 4 };
                world
                    .import_block(px, y, pz, (120 << 4) | facing | eye)
                    .unwrap();
            }
        }
        world.save(&path).unwrap();
        let changed = world.insert_end_eye(&path, 0, y, -2).unwrap();
        assert_eq!(changed.len(), 10);
        let saved = World::load(&path).unwrap();
        assert_eq!(saved.get(0, y, -2) & 4, 4);
        for dx in -1..=1 {
            for dz in -1..=1 {
                assert_eq!(saved.get(dx, y, dz) >> 4, 119);
            }
        }
        fs::remove_file(path).ok();
    }

    #[test]
    fn wrong_facing_end_frame_cannot_activate_portal() {
        let path = file("portal-facing");
        let mut world = World::new();
        let y = 80;
        for dx in -1..=1 {
            for (px, pz) in [(dx, -2), (dx, 2), (-2, dx), (2, dx)] {
                let facing = if pz == -2 {
                    0
                } else if pz == 2 {
                    2
                } else if px == -2 {
                    3
                } else {
                    1
                };
                let facing = if (px, pz) == (1, 2) { 0 } else { facing };
                let eye = if (px, pz) == (0, -2) { 0 } else { 4 };
                world
                    .import_block(px, y, pz, (120 << 4) | facing | eye)
                    .unwrap();
            }
        }
        world.save(&path).unwrap();
        let changed = world.insert_end_eye(&path, 0, y, -2).unwrap();
        assert_eq!(
            changed.len(),
            1,
            "only inserted eye, not the invalid portal"
        );
        assert_eq!(world.get(0, y, 0), 0);
        fs::remove_file(path).ok();
    }

    #[test]
    fn primed_tnt_waits_eighty_server_ticks() {
        let path = file("tnt");
        let mut world = World::new();
        world.update_and_save(&path, 1, 64, 1, 46 << 4).unwrap();
        assert!(world.ignite_tnt(&path, 1, 64, 1).unwrap());
        assert_eq!(world.drain_tnt_events().len(), 1);
        for _ in 0..79 {
            world.block_tick(&path, 64).unwrap();
            assert!(world.drain_tnt_events().is_empty());
        }
        world.block_tick(&path, 64).unwrap();
        assert!(world
            .drain_tnt_events()
            .iter()
            .any(|event| { matches!(event, TntEvent::Explode { .. }) }));
        fs::remove_file(path).ok();
    }

    #[test]
    fn chain_tnt_has_shortened_fuse_and_redstone_keeps_eighty_ticks() {
        let mut w = World::new();
        w.fluid_phase = 100;
        w.spawn_primed_tnt(0, 80, 0, 1);
        w.drain_tnt_events();
        w.fluid_phase = 101;
        w.finalize_tnt(&[(1, 80, 0), (20, 80, 0)]);
        let near = w.primed_tnt.values().find(|t| t.pos == (1, 80, 0)).unwrap();
        let far = w
            .primed_tnt
            .values()
            .find(|t| t.pos == (20, 80, 0))
            .unwrap();
        assert!((10..=29).contains(&(near.explode_at - w.fluid_phase)));
        assert_eq!(far.explode_at - w.fluid_phase, 80);
        assert_eq!(
            w.drain_tnt_events()
                .iter()
                .filter(|e| matches!(e, TntEvent::Explode { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn tnt_preblast_snapshot_is_only_needed_when_fuse_expires() {
        let path = file("tnt-snapshot-signal");
        let mut world = World::new();
        world.update_and_save(&path, 0, 80, 0, 46 << 4).unwrap();
        assert!(world.ignite_tnt(&path, 0, 80, 0).unwrap());
        assert!(!world.tnt_detonates_next_tick());
        for _ in 0..79 {
            world.block_tick(&path, 64).unwrap();
        }
        assert!(world.tnt_detonates_next_tick());
        world.block_tick(&path, 64).unwrap();
        assert!(!world.tnt_detonates_next_tick());
        fs::remove_file(path).ok();
    }

    #[test]
    fn tnt_restart_retains_fuse_and_reemits_spawn() {
        let path = file("tnt-restart");
        let mut w = World::new();
        w.update_and_save(&path, 2, 128, 2, 46 << 4).unwrap();
        assert!(w.ignite_tnt(&path, 2, 128, 2).unwrap());
        assert!(w
            .drain_tnt_events()
            .iter()
            .any(|event| { matches!(event, TntEvent::Spawn { .. }) }));
        for _ in 0..40 {
            w.block_tick(&path, 64).unwrap();
        }
        let mut recovered = World::load(&path).unwrap();
        assert_eq!(recovered.primed_tnt.len(), 1);
        assert!(recovered
            .drain_tnt_events()
            .iter()
            .any(|event| { matches!(event, TntEvent::Spawn { .. }) }));
        for _ in 0..39 {
            recovered.block_tick(&path, 64).unwrap();
            assert!(!recovered
                .drain_tnt_events()
                .iter()
                .any(|event| { matches!(event, TntEvent::Explode { .. }) }));
        }
        recovered.block_tick(&path, 64).unwrap();
        assert_eq!(recovered.primed_tnt.len(), 0);
        assert_eq!(
            recovered
                .drain_tnt_events()
                .iter()
                .filter(|event| { matches!(event, TntEvent::Explode { .. }) })
                .count(),
            1
        );
        // Even in empty air, the expired entity was removed durably.
        let again = World::load(&path).unwrap();
        assert!(again.primed_tnt.is_empty());
        fs::remove_file(path).ok();
    }

    #[test]
    fn tnt_exposure_snapshot_keeps_nearby_shields_without_distant_world_state() {
        let mut world = World::new();
        world.import_block(2, 80, 0, 49 << 4).unwrap();
        world.import_block(40, 80, 0, 49 << 4).unwrap();
        world.spawn_primed_tnt(0, 80, 0, 1);
        let snapshot = world.blast_exposure_snapshot();
        assert_eq!(snapshot.get(2, 80, 0), world.get(2, 80, 0));
        assert_eq!(snapshot.get(40, 80, 0), 0);
        assert_eq!(snapshot.edits.len(), 1);
        assert!(snapshot.pending_devices.is_empty());
        assert!(snapshot.primed_tnt.is_empty());
        assert_eq!(snapshot.generation, world.generation);
    }

    #[test]
    fn simultaneous_tnt_backlog_survives_tick_save_and_graceful_shutdown() {
        let path = file("tnt-overdue-backlog");
        let mut world = World::new();
        // Twelve entities expire together, more than the four allowed per tick.
        // Keep them well apart to avoid unrelated terrain/chain behavior.
        for x in 0..12 {
            world.spawn_primed_tnt(x * 12, 200, 0, 1);
        }
        world.save(&path).unwrap();
        world.drain_tnt_events();

        world.block_tick(&path, 64).unwrap();
        assert_eq!(world.primed_tnt.len(), 8);
        assert_eq!(
            world
                .drain_tnt_events()
                .iter()
                .filter(|event| matches!(event, TntEvent::Explode { .. }))
                .count(),
            4
        );
        // This is the real graceful-shutdown save path. Previously the eight
        // overdue fuses produced "invalid primed TNT fuse" and aborted stop.
        world.save(&path).unwrap();
        let mut restarted = World::load(&path).unwrap();
        assert_eq!(restarted.primed_tnt.len(), 8);
        restarted.drain_tnt_events();
        for expected in [4, 0] {
            restarted.block_tick(&path, 64).unwrap();
            assert_eq!(restarted.primed_tnt.len(), expected);
            assert_eq!(
                restarted
                    .drain_tnt_events()
                    .iter()
                    .filter(|event| matches!(event, TntEvent::Explode { .. }))
                    .count(),
                4
            );
        }
        assert!(World::load(&path).unwrap().primed_tnt.is_empty());
        fs::remove_file(path).ok();
    }

    #[test]
    fn explosion_near_irw_edit_limit_does_not_abort_tick_or_stop_save() {
        let path = file("tnt-edits-capacity");
        let mut world = World::new();
        // Fill the sparse-edit map with distant blocks; generated grass and
        // dirt near this blast would otherwise allocate unpersistable edits.
        for n in 0..super::super::MAX_EDITS {
            world.edits.insert(
                (1000 + (n % 256) as i32, 100, 1000 + (n / 256) as i32),
                49 << 4,
            );
        }
        assert!(world.spawn_primed_tnt(0, 63, 0, 1));
        world.save(&path).unwrap();
        world.block_tick(&path, 64).unwrap();
        assert!(world.primed_tnt.is_empty());
        assert_eq!(world.edits.len(), super::super::MAX_EDITS);
        world.save(&path).unwrap();
        assert!(World::load(&path).unwrap().primed_tnt.is_empty());
        fs::remove_file(path).ok();
    }

    #[test]
    fn tnt_capacity_defers_chain_blocks_instead_of_failing_the_save() {
        let path = file("tnt-capacity");
        let mut world = World::new();
        for x in 1..=3 {
            world.import_block(x, 100, 0, 46 << 4).unwrap();
        }
        for x in 0..(MAX_PRIMED_TNT - 1) {
            assert!(world.spawn_primed_tnt(100 + x as i32, 100, 40, 80));
        }
        assert!(world.spawn_primed_tnt(0, 100, 0, 1));
        assert!(!world.spawn_primed_tnt(9, 100, 0, 80));
        assert!(world
            .stage_special_item(
                1,
                100,
                0,
                Stack {
                    id: 259,
                    count: 1,
                    damage: 0,
                    nbt: None,
                },
            )
            .unwrap()
            .is_none());

        world.save(&path).unwrap();
        world.block_tick(&path, 64).unwrap();
        assert_eq!(world.primed_tnt.len(), MAX_PRIMED_TNT);
        assert_eq!(world.get(1, 100, 0), 0);
        assert_eq!(world.get(2, 100, 0) >> 4, 46);
        assert_eq!(world.get(3, 100, 0) >> 4, 46);
        let restored = World::load(&path).unwrap();
        assert_eq!(restored.primed_tnt.len(), MAX_PRIMED_TNT);
        assert_eq!(restored.get(3, 100, 0) >> 4, 46);
        fs::remove_file(path).ok();
    }

    #[test]
    fn two_hundred_fifty_six_simultaneous_tnt_resume_without_lost_explosions() {
        let path = file("tnt-256-burst");
        let mut world = World::new();
        for x in 0..256 {
            assert!(world.spawn_primed_tnt(x * 20, 200, 0, 1));
        }
        world.save(&path).unwrap();
        world.drain_tnt_events();
        let mut detonated = std::collections::BTreeSet::new();
        for tick in 0..64 {
            world.block_tick(&path, 64).unwrap();
            for event in world.drain_tnt_events() {
                if let TntEvent::Explode { entity_id, .. } = event {
                    assert!(detonated.insert(entity_id), "duplicate TNT detonation");
                }
            }
            assert_eq!(detonated.len(), ((tick + 1) * 4) as usize);
            if tick == 31 {
                // Simulate /stop and restart halfway through an overdue queue.
                world.save(&path).unwrap();
                world = World::load(&path).unwrap();
                world.drain_tnt_events();
            }
        }
        assert_eq!(detonated.len(), 256);
        assert!(world.primed_tnt.is_empty());
        assert!(World::load(&path).unwrap().primed_tnt.is_empty());
        fs::remove_file(path).ok();
    }

    #[test]
    fn blast_rays_respect_obsidian_and_do_not_erase_container_inventory() {
        let path = file("tnt-ray-resistance");
        let mut world = World::new();
        world.import_block(0, 100, 0, 49 << 4).unwrap();
        world.import_block(1, 100, 0, 1 << 4).unwrap();
        world.import_block(0, 100, 1, 54 << 4).unwrap();
        assert!(world.spawn_primed_tnt(0, 100, 0, 1));
        world.save(&path).unwrap();
        world.block_tick(&path, 64).unwrap();
        assert_eq!(world.get(0, 100, 0) >> 4, 49);
        assert_eq!(world.get(1, 100, 0) >> 4, 1);
        assert_eq!(world.get(0, 100, 1) >> 4, 54);
        let mut air_blast = World::new();
        air_blast.import_block(1, 100, 0, 1 << 4).unwrap();
        assert!(air_blast.spawn_primed_tnt(0, 100, 0, 1));
        air_blast.block_tick(&path, 64).unwrap();
        assert_eq!(air_blast.get(1, 100, 0), 0);
        fs::remove_file(path).ok();
    }

    #[test]
    fn overflow_chain_ignition_survives_shutdown_and_resumes_when_slots_free() {
        let path = file("tnt-overflow-queue");
        let mut world = World::new();
        for x in 1..=3 {
            world.import_block(x, 200, 0, 46 << 4).unwrap();
        }
        for _ in 0..(MAX_PRIMED_TNT - 1) {
            assert!(world.spawn_primed_tnt(1000, 200, 0, 1000));
        }
        assert!(world.spawn_primed_tnt(0, 200, 0, 1));
        world.save(&path).unwrap();
        world.drain_tnt_events();

        world.block_tick(&path, 64).unwrap();
        assert_eq!(world.primed_tnt.len(), MAX_PRIMED_TNT);
        assert_eq!(world.pending_chain_tnt.len(), 2);
        assert_eq!(world.get(1, 200, 0), 0);
        assert_eq!(world.get(2, 200, 0) >> 4, 46);
        assert_eq!(world.get(3, 200, 0) >> 4, 46);
        let mut recovered = World::load(&path).unwrap();
        assert_eq!(recovered.pending_chain_tnt.len(), 2);
        // Release artificial load, then save and restart once more.
        recovered.primed_tnt.retain(|_, tnt| tnt.pos.0 < 10);
        recovered.save(&path).unwrap();
        let mut resumed = World::load(&path).unwrap();
        resumed.block_tick(&path, 64).unwrap();
        assert!(resumed.pending_chain_tnt.is_empty());
        assert_eq!(resumed.get(2, 200, 0), 0);
        assert_eq!(resumed.get(3, 200, 0), 0);
        assert_eq!(resumed.primed_tnt.len(), 3);
        assert!(World::load(&path).unwrap().pending_chain_tnt.is_empty());
        fs::remove_file(path).ok();
    }

    #[test]
    fn joining_late_sees_primed_tnt_without_consuming_events() {
        let mut world = World::new();
        assert!(world.spawn_primed_tnt(5, 80, 5, 80));
        assert!(world.spawn_primed_tnt(200, 80, 5, 80));
        let near = world.visible_primed_tnt(crate::player::PlayerPosition {
            x: 5.5,
            y: 80.0,
            z: 5.5,
        });
        assert_eq!(near.len(), 1);
        assert_eq!(near[0].1, (5, 80, 5));
        assert_eq!(world.drain_tnt_events().len(), 2);
    }

    #[test]
    fn malformed_tnt_trailer_fails_closed() {
        let path = file("tnt-corrupt");
        let mut world = World::new();
        world.import_block(1, 128, 1, 46 << 4).unwrap();
        assert!(world.ignite_tnt(&path, 1, 128, 1).unwrap());
        let mut raw = fs::read(&path).unwrap();
        raw.push(255);
        fs::write(&path, raw).unwrap();
        assert!(World::load(&path).is_err());
        fs::remove_file(path).ok();
    }

    #[test]
    fn tripwire_only_powers_inward_facing_hooks_and_releases_on_tick() {
        let path = file("tripwire-facing-release");
        let mut w = World::new();
        // The outside support blocks keep each wall hook attached.
        for x in [-3, 3] {
            w.import_block(x, 64, 0, 1 << 4).unwrap();
        }
        w.import_block(-2, 64, 0, (131 << 4) | 3).unwrap();
        w.import_block(2, 64, 0, (131 << 4) | 1).unwrap();
        for x in -1..=1 {
            w.import_block(x, 64, 0, 132 << 4).unwrap();
        }
        w.save(&path).unwrap();
        let changed = w.touch_tripwire(&path, 0, 64, 0).unwrap();
        assert_eq!(changed.len(), 5);
        assert_eq!(w.get(-2, 64, 0) & 12, 12);
        assert_eq!(w.get(0, 64, 0) & 5, 5);
        // A second contact extends the timer without creating a disk edit.
        for _ in 0..5 {
            w.block_tick(&path, 64).unwrap();
        }
        assert!(w.touch_tripwire(&path, 0, 64, 0).unwrap().is_empty());
        for _ in 0..9 {
            w.block_tick(&path, 64).unwrap();
        }
        assert_ne!(w.get(-2, 64, 0) & 8, 0);
        w.block_tick(&path, 64).unwrap();
        assert_eq!(w.get(-2, 64, 0) & 8, 0);
        assert_eq!(w.get(2, 64, 0) & 8, 0);
        assert_eq!(w.get(0, 64, 0) & 1, 0);
        assert_eq!(w.get(0, 64, 0) & 4, 4);
        assert_eq!(World::load(&path).unwrap().get(0, 64, 0) & 1, 0);
        fs::remove_file(path).ok();
    }

    #[test]
    fn broken_tripwire_unlinks_and_unpowers_hooks() {
        let path = file("wire-break");
        let mut w = World::new();
        for x in [-3, 3] {
            w.import_block(x, 64, 0, 1 << 4).unwrap();
        }
        w.import_block(-2, 64, 0, (131 << 4) | 3).unwrap();
        w.import_block(2, 64, 0, (131 << 4) | 1).unwrap();
        for x in -1..=1 {
            w.import_block(x, 64, 0, 132 << 4).unwrap();
        }
        w.save(&path).unwrap();
        w.touch_tripwire(&path, 0, 64, 0).unwrap();
        assert_ne!(w.get(-2, 64, 0) & 8, 0);
        w.update_and_save(&path, 0, 64, 0, 0).unwrap();
        for _ in 0..3 {
            w.block_tick(&path, 64).unwrap();
        }
        assert_eq!(w.get(-2, 64, 0) & 12, 0);
        assert_eq!(w.get(2, 64, 0) & 12, 0);
        assert_eq!(World::load(&path).unwrap().get(-2, 64, 0) & 12, 0);
        fs::remove_file(path).ok();
    }

    #[test]
    fn tripwire_rejects_wrong_facing_hooks_without_writing() {
        let path = file("tripwire-invalid");
        let mut w = World::new();
        w.import_block(-2, 64, 0, (131 << 4) | 1).unwrap();
        w.import_block(2, 64, 0, (131 << 4) | 1).unwrap();
        for x in -1..=1 {
            w.import_block(x, 64, 0, 132 << 4).unwrap();
        }
        assert!(w.touch_tripwire(&path, 0, 64, 0).unwrap().is_empty());
        assert_eq!(w.get(0, 64, 0), 132 << 4);
        assert!(!path.exists());
    }

    #[test]
    fn bonemeal_preserves_sapling_species_and_rejects_lone_dark_oak() {
        let mut w = World::new();
        let meal = Stack {
            id: 351,
            count: 1,
            damage: 15,
            nbt: None,
        };
        w.import_block(0, 64, 0, (6 << 4) | 2).unwrap();
        let (_, changed, _, _) = w.stage_special_item(0, 64, 0, meal).unwrap().unwrap();
        assert!(!changed.is_empty());
        let mut batch = w.bonemeal_changes(0, 64, 0);
        assert_eq!(batch.get(&(0, 64, 0)), Some(&((17 << 4) | 2)));
        assert_eq!(batch.get(&(1, 68, 0)), Some(&((18 << 4) | 2)));
        w.import_block(5, 64, 0, (6 << 4) | 4).unwrap();
        batch = w.bonemeal_changes(5, 64, 0);
        assert_eq!(batch.get(&(5, 64, 0)), Some(&(162 << 4)));
        assert_eq!(batch.get(&(6, 68, 0)), Some(&(161 << 4)));
        w.import_block(10, 64, 0, (6 << 4) | 5).unwrap();
        assert!(w.bonemeal_changes(10, 64, 0).is_empty());
        let (_, changed, _, _) = w.stage_special_item(10, 64, 0, meal).unwrap().unwrap();
        assert!(
            changed.is_empty(),
            "unsupported dark oak cannot debit bone meal"
        );
    }

    #[test]
    fn weighted_plate_releases_and_saves_zero_signal() {
        let path = file("plate-release");
        let mut w = World::new();
        w.update_and_save(&path, 0, 64, 0, 148 << 4).unwrap();
        assert!(w.press_weighted_plate(&path, 0, 64, 0, 11).unwrap());
        for _ in 0..9 {
            w.block_tick(&path, 64).unwrap();
            assert_eq!(w.get(0, 64, 0) & 15, 2);
        }
        assert!(w.block_tick(&path, 64).unwrap().contains(&(0, 64, 0)));
        assert_eq!(w.get(0, 64, 0) & 15, 0);
        assert_eq!(World::load(&path).unwrap().get(0, 64, 0) & 15, 0);
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
