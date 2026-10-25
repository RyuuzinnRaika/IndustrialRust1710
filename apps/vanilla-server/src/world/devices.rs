//! Time-bounded redstone inputs and basic mechanical block devices.
//! Changes are staged, then committed by World::block_tick atomically.
use super::{in_bounds, Position, World};
use std::collections::BTreeMap;
use std::io;
use std::path::Path;

const NEIGHBORS: [(i32, i32, i32); 6] = [
    (1, 0, 0),
    (-1, 0, 0),
    (0, 1, 0),
    (0, -1, 0),
    (0, 0, 1),
    (0, 0, -1),
];
fn facing(meta: u16) -> (i32, i32, i32) {
    match meta & 7 {
        0 => (0, -1, 0),
        1 => (0, 1, 0),
        2 => (0, 0, -1),
        3 => (0, 0, 1),
        4 => (-1, 0, 0),
        _ => (1, 0, 0),
    }
}
/// 1.7.10 repeater metadata 0=N, 1=E, 2=S, 3=W.
fn repeater_direction(meta: u16) -> (i32, i32) {
    match meta & 3 {
        0 => (0, -1),
        1 => (1, 0),
        2 => (0, 1),
        _ => (-1, 0),
    }
}

/// Full opaque cubes can pass weak current after a *strong* input.
fn full_conductor(id: u16) -> bool {
    matches!(
        id,
        1..=5 | 7 | 12..=17 | 19 | 22..=25 | 41..=43 | 45..=49
            | 56..=58 | 61 | 62 | 73 | 74 | 80 | 82 | 86..=89
            | 98..=100 | 103 | 110 | 112 | 121 | 129 | 133
            | 137 | 152 | 155 | 159 | 173
    )
}

fn movable(block: u16) -> bool {
    !matches!(
        block >> 4,
        0 | 7 | 8
            ..=11
                | 18
                | 23 // dispenser (tile entity)
                | 25 // note block (tile entity)
                | 29
                | 33
                | 34
                | 49 // obsidian
                | 52 // spawner
                | 54
                | 61
                | 62
                | 64
                | 71
                | 75
                | 76
                | 84 // jukebox (tile entity)
                | 93
                | 94
                | 96
                | 116
                | 117
                | 120
                | 127 // attached cocoa
                | 130
                | 137 // command block
                | 146 // trapped chest
                | 154 // hopper
                | 158 // dropper
                | 176 // standing banner
                | 177 // wall banner
                | 140 // flower pot (tile entity)
                | 144 // skull (tile entity)
                | 151 // daylight sensor (tile entity)
                | 138
    )
}
/// Coordinates of the two physical rail endpoints, including ascending slopes.
fn rail_endpoints(x: i32, y: i32, z: i32, metadata: u16) -> [(i32, i32, i32); 2] {
    match metadata & 7 {
        0 => [(x, y, z - 1), (x, y, z + 1)],
        1 => [(x - 1, y, z), (x + 1, y, z)],
        2 => [(x - 1, y, z), (x + 1, y + 1, z)],
        3 => [(x - 1, y + 1, z), (x + 1, y, z)],
        4 => [(x, y + 1, z - 1), (x, y, z + 1)],
        5 => [(x, y, z - 1), (x, y + 1, z + 1)],
        _ => [(x, y, z), (x, y, z)],
    }
}

/// Only rails with matching physical endpoints conduct power to each other.
fn rails_connected(
    from: (i32, i32, i32),
    from_meta: u16,
    to: (i32, i32, i32),
    to_meta: u16,
) -> bool {
    // At a slope boundary, the endpoints describe the neighboring track cell.
    let from_ends = rail_endpoints(from.0, from.1, from.2, from_meta);
    let to_ends = rail_endpoints(to.0, to.1, to.2, to_meta);
    from_ends.iter().any(|&(fx, fy, fz)| {
        fx == to.0
            && fz == to.2
            && to_ends
                .iter()
                .any(|&(tx, ty, tz)| tx == from.0 && tz == from.2 && fy == ty)
    })
}

impl World {
    /// Strict strong-power path. Never recursively query a conductor: this
    /// keeps the signal graph bounded even for rings of solid blocks.
    fn strong_signal_into(&self, from: (i32, i32, i32), to: (i32, i32, i32)) -> u8 {
        let (x, y, z) = from;
        let block = self.get(x, y, z);
        match block >> 4 {
            // Vanilla dust strongly powers the supporting block beneath it.
            // Other faces receive only its ordinary weak power; treating all
            // faces as strong would incorrectly energize adjacent conductors.
            55 if to == (x, y - 1, z) => (block & 15) as u8,
            152 => 15,
            76 if to == (x, y + 1, z) => 15,
            94 => {
                let (dx, dz) = repeater_direction(block);
                if to == (x + dx, y, z + dz) {
                    15
                } else {
                    0
                }
            }
            150 => {
                let (dx, dz) = repeater_direction(block);
                if to == (x + dx, y, z + dz) {
                    self.comparator_outputs
                        .get(&(x, y as u8, z))
                        .copied()
                        .unwrap_or(0)
                } else {
                    0
                }
            }
            // Buttons and levers strongly power the block they are attached to.
            69 | 77 | 143 if block & 8 != 0 => {
                let attached = match block & 7 {
                    1 => (x - 1, y, z),
                    2 => (x + 1, y, z),
                    3 => (x, y, z - 1),
                    4 => (x, y, z + 1),
                    5 | 6 => (x, y - 1, z),
                    _ => (x, y + 1, z),
                };
                if to == attached {
                    15
                } else {
                    0
                }
            }
            _ => 0,
        }
    }

    /// Directional analog redstone strength; repeaters/comparators have a
    /// single output face. Dust propagates its stored 0..15 strength.
    pub(super) fn signal_into(&self, from: (i32, i32, i32), to: (i32, i32, i32)) -> u8 {
        let (x, y, z) = from;
        let block = self.get(x, y, z);
        match block >> 4 {
            55 => (block & 15) as u8,
            69 | 77 | 143 => {
                if block & 8 != 0 {
                    15
                } else {
                    0
                }
            }
            70 | 72 => {
                if block & 1 != 0 {
                    15
                } else {
                    0
                }
            }
            131 => {
                if block & 8 != 0 {
                    15
                } else {
                    0
                }
            }
            147 | 148 | 151 => (block & 15) as u8,
            76 => {
                // A redstone torch weakly powers every adjacent face except
                // the block supporting it. Its strong output points up only.
                let attached = match block & 7 {
                    1 => (x - 1, y, z),
                    2 => (x + 1, y, z),
                    3 => (x, y, z - 1),
                    4 => (x, y, z + 1),
                    _ => (x, y - 1, z),
                };
                if to == attached {
                    0
                } else {
                    15
                }
            }
            152 => 15,
            94 => {
                let (dx, dz) = repeater_direction(block);
                if to == (x + dx, y, z + dz) {
                    15
                } else {
                    0
                }
            }
            150 => {
                let (dx, dz) = repeater_direction(block);
                if to == (x + dx, y, z + dz) {
                    self.comparator_outputs
                        .get(&(x, y as u8, z))
                        .copied()
                        .unwrap_or(0)
                } else {
                    0
                }
            }
            id if full_conductor(id) => NEIGHBORS
                .iter()
                .filter_map(|&(dx, dy, dz)| {
                    let neighbor = (x + dx, y + dy, z + dz);
                    (neighbor != to).then(|| self.strong_signal_into(neighbor, from))
                })
                .max()
                .unwrap_or(0),
            _ => 0,
        }
    }

    /// Vanilla comparator override inputs are not ordinary power sources:
    /// do not expose their values to adjacent dust, repeaters or pistons.
    /// Inventory-backed tile entities are handled separately by the server.
    fn comparator_rear_input(&self, from: (i32, i32, i32), to: (i32, i32, i32)) -> u8 {
        let state = self.get(from.0, from.1, from.2);
        match state >> 4 {
            92 => 14u8.saturating_sub((state as u8 & 7).min(7) * 2), // Cake slices.
            118 => (state & 3) as u8,                                // Cauldron water level.
            120 => {
                if state & 4 != 0 {
                    15
                } else {
                    0
                }
            } // End portal eye.
            _ => self.signal_into(from, to),
        }
    }

    pub(super) fn power_into(&self, from: (i32, i32, i32), to: (i32, i32, i32)) -> bool {
        self.signal_into(from, to) > 0
    }

    pub(super) fn powered_near(&self, x: i32, y: i32, z: i32) -> bool {
        NEIGHBORS
            .iter()
            .any(|&(dx, dy, dz)| self.power_into((x + dx, y + dy, z + dz), (x, y, z)))
    }

    /// An actively powered repeater facing perpendicular into either side
    /// locks the target's output state until the side source turns off.
    fn repeater_locked(&self, x: i32, y: i32, z: i32, meta: u16) -> bool {
        let (dx, dz) = repeater_direction(meta);
        for (sx, sz) in [(-dz, dx), (dz, -dx)] {
            let source = (x + sx, y, z + sz);
            if self.get(source.0, source.1, source.2) >> 4 == 94
                && self.power_into(source, (x, y, z))
            {
                return true;
            }
        }
        false
    }
    /// Power cannot travel infinitely across powered/activator rails.
    /// Bound traversal to eight rail segments as in 1.7.10.
    fn rail_powered(&self, x: i32, y: i32, z: i32, id: u16) -> bool {
        use std::collections::{BTreeSet, VecDeque};
        let mut seen = BTreeSet::from([(x, y, z)]);
        let mut next = VecDeque::from([(x, y, z, 0u8)]);
        while let Some((rx, ry, rz, distance)) = next.pop_front() {
            if self.powered_near(rx, ry, rz) {
                return true;
            }
            if distance >= 8 {
                continue;
            }
            let from_meta = self.get(rx, ry, rz) & 15;
            for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                for dy in [-1, 0, 1] {
                    let at = (rx + dx, ry + dy, rz + dz);
                    if !in_bounds(at.0, at.1, at.2) {
                        continue;
                    }
                    let neighbor = self.get(at.0, at.1, at.2);
                    if neighbor >> 4 == id
                        && rails_connected((rx, ry, rz), from_meta, at, neighbor & 15)
                        && seen.insert(at)
                    {
                        next.push_back((at.0, at.1, at.2, distance + 1));
                    }
                }
            }
        }
        false
    }

    fn push_piston(
        &self,
        x: i32,
        y: i32,
        z: i32,
        block: u16,
        changes: &mut BTreeMap<Position, u16>,
    ) {
        let (dx, dy, dz) = facing(block);
        let extended = block & 8 != 0;
        // Pistons in 1.7.10 are also quasi-connected by power at the
        // block directly above them (BUD behavior).
        let on = self.powered_near(x, y, z)
            || (in_bounds(x, y + 1, z) && self.powered_near(x, y + 1, z));
        if extended == on {
            return;
        }
        let head = (x + dx, y + dy, z + dz);
        if !in_bounds(head.0, head.1, head.2) {
            return;
        }
        if on {
            // Push at most twelve contiguous movable blocks. Never overwrite
            // a chest/furnace inventory or an unbreakable block.
            let mut occupied = Vec::new();
            let mut target = head;
            loop {
                if !in_bounds(target.0, target.1, target.2) {
                    return;
                }
                let found = self.get(target.0, target.1, target.2);
                if found == 0 {
                    break;
                }
                if occupied.len() >= 12 || !movable(found) {
                    return;
                }
                occupied.push((target, found));
                target = (target.0 + dx, target.1 + dy, target.2 + dz);
            }
            for (pos, value) in occupied.into_iter().rev() {
                let dest = (pos.0 + dx, pos.1 + dy, pos.2 + dz);
                changes.insert((dest.0, dest.1 as u8, dest.2), value);
            }
            changes.insert((head.0, head.1 as u8, head.2), (34 << 4) | (block & 7));
            changes.insert((x, y as u8, z), block | 8);
        } else {
            if self.get(head.0, head.1, head.2) >> 4 == 34 {
                changes.insert((head.0, head.1 as u8, head.2), 0);
                if block >> 4 == 29 {
                    let far = (head.0 + dx, head.1 + dy, head.2 + dz);
                    if in_bounds(far.0, far.1, far.2) {
                        let pulled = self.get(far.0, far.1, far.2);
                        if pulled != 0 && movable(pulled) {
                            changes.insert((head.0, head.1 as u8, head.2), pulled);
                            changes.insert((far.0, far.1 as u8, far.2), 0);
                        }
                    }
                }
            }
            changes.insert((x, y as u8, z), block & !8);
        }
    }
    pub(super) fn device_changes(&mut self, budget: usize) -> BTreeMap<Position, u16> {
        let mut changes = BTreeMap::new();
        for (&pos, &when) in &self.pending_buttons {
            if self.fluid_phase >= when {
                let (x, y, z) = pos;
                let current = self.get(x, i32::from(y), z);
                let flag = match current >> 4 {
                    70 | 72 => 1,
                    147 | 148 => 15,
                    _ => 8,
                };
                if matches!(current >> 4, 70 | 72 | 77 | 131 | 143 | 147 | 148)
                    && current & flag != 0
                {
                    changes.insert(pos, current & !flag);
                }
            }
        }
        if self.pending_devices.is_empty() {
            return changes;
        }
        let len = self.pending_devices.len();
        let start = (self.fluid_phase as usize).wrapping_mul(budget) % len;
        let scheduled: Vec<_> = self
            .pending_devices
            .iter()
            .cycle()
            .skip(start)
            .take(budget.min(len))
            .copied()
            .collect();
        for (x, y, z) in scheduled {
            let y = i32::from(y);
            let current = self.get(x, y, z);
            match current >> 4 {
                93 | 94 => {
                    let (dx, dz) = repeater_direction(current);
                    let pos = (x, y as u8, z);
                    if self.repeater_locked(x, y, z, current) {
                        self.pending_repeaters.remove(&pos);
                        continue;
                    }
                    let desired = self.power_into((x - dx, y, z - dz), (x, y, z));
                    let active = current >> 4 == 94;
                    if desired == active {
                        self.pending_repeaters.remove(&pos);
                    } else {
                        let delay = (((current >> 2) & 3) + 1) as u64 * 2;
                        let entry = self
                            .pending_repeaters
                            .entry(pos)
                            .or_insert((self.fluid_phase + delay, desired));
                        if entry.1 != desired {
                            *entry = (self.fluid_phase + delay, desired);
                        }
                        if entry.0 <= self.fluid_phase {
                            changes.insert(
                                pos,
                                ((if desired { 94u16 } else { 93u16 }) << 4) | (current & 15),
                            );
                            self.pending_repeaters.remove(&pos);
                        }
                    }
                }
                149 | 150 => {
                    // Rear input and two side inputs are sampled independently.
                    // Metadata bit 2 chooses subtraction mode; bits 0..1 face.
                    let (dx, dz) = repeater_direction(current);
                    let rear = self.comparator_rear_input((x - dx, y, z - dz), (x, y, z));
                    let side = [(-dz, dx), (dz, -dx)]
                        .into_iter()
                        .map(|(sx, sz)| self.signal_into((x + sx, y, z + sz), (x, y, z)))
                        .max()
                        .unwrap_or(0);
                    let desired = if current & 4 != 0 {
                        rear.saturating_sub(side)
                    } else if rear >= side {
                        rear
                    } else {
                        0
                    };
                    let pos = (x, y as u8, z);
                    let output = self.comparator_outputs.get(&pos).copied().unwrap_or(0);
                    if desired == output {
                        self.pending_comparators.remove(&pos);
                    } else {
                        let entry = self
                            .pending_comparators
                            .entry(pos)
                            .or_insert((self.fluid_phase + 2, desired));
                        if entry.1 != desired {
                            *entry = (self.fluid_phase + 2, desired);
                        }
                        if entry.0 <= self.fluid_phase {
                            // Analog strength is applied after successful save
                            // in block_tick, so failed disk writes do not power clients.
                            self.comparator_ready.insert(pos, desired);
                            let active = desired != 0;
                            if active != (current >> 4 == 150) {
                                changes.insert(
                                    pos,
                                    ((if active { 150u16 } else { 149u16 }) << 4) | (current & 15),
                                );
                            }
                            self.pending_comparators.remove(&pos);
                        }
                    }
                }
                23 | 158 => {
                    // The 1.7.10 dispenser/dropper triggered bit is metadata
                    // bit 3. Fire only on a rising edge, never every tick.
                    let powered = self.powered_near(x, y, z);
                    if powered != (current & 8 != 0) {
                        changes.insert((x, y as u8, z), current ^ 8);
                    }
                }
                27 | 157 => {
                    let powered = self.rail_powered(x, y, z, current >> 4);
                    if powered != (current & 8 != 0) {
                        changes.insert((x, y as u8, z), current ^ 8);
                    }
                }
                29 | 33 => self.push_piston(x, y, z, current, &mut changes),
                _ => {}
            }
        }
        changes
    }

    /// Activate levers, buttons, doors, trapdoors or rotate repeater delays.
    /// The returned positions must be broadcast to Protocol 5 clients.
    pub fn interact_device(
        &mut self,
        path: &Path,
        x: i32,
        y: i32,
        z: i32,
    ) -> io::Result<Vec<(i32, i32, i32)>> {
        if !in_bounds(x, y, z) {
            return Ok(Vec::new());
        }
        let old = self.get(x, y, z);
        let id = old >> 4;
        let mut touched = Vec::new();
        let new = match id {
            69 => old ^ 8,
            77 | 143 => old | 8,
            96 => old ^ 4,
            93 | 94 => (old & !12) | (((((old & 12) >> 2) + 1) & 3) << 2),
            149 | 150 => old ^ 4, // Toggle comparator compare/subtract mode.
            107 => old ^ 4,       // Fence gate: open/closed.
            71 => {
                // Iron doors are powered only, not hand-operated.
                return Ok(Vec::new());
            }
            64 => {
                // Top-half clicks operate on the bottom-half metadata.
                let lower_y = if old & 8 != 0 { y - 1 } else { y };
                let bottom = self.get(x, lower_y, z);
                if bottom >> 4 == id
                    && bottom & 8 == 0
                    && self.update_and_save(path, x, lower_y, z, bottom ^ 4)?
                {
                    touched.push((x, lower_y, z));
                }
                return Ok(touched);
            }
            _ => return Ok(Vec::new()),
        };
        if self.update_and_save(path, x, y, z, new)? {
            touched.push((x, y, z));
        }
        if matches!(id, 77 | 143) {
            self.pending_buttons.insert(
                (x, y as u8, z),
                self.fluid_phase + if id == 77 { 20 } else { 30 },
            );
        }
        Ok(touched)
    }

    /// Place both halves of a door in one save; failures roll back both cells.
    pub fn place_door(
        &mut self,
        path: &Path,
        x: i32,
        y: i32,
        z: i32,
        id: u16,
        orientation: u16,
    ) -> io::Result<bool> {
        if !matches!(id, 64 | 71)
            || !in_bounds(x, y, z)
            || !in_bounds(x, y + 1, z)
            || self.get(x, y, z) != 0
            || self.get(x, y + 1, z) != 0
            || !self.can_support(x, y, z, id << 4)
        {
            return Ok(false);
        }
        // Vanilla's top-half bit 0 selects the hinge side. Prefer a hinge
        // away from an adjacent matching door, otherwise prefer the side
        // with more adjacent opaque blocks (at foot and head heights).
        let (sx, sz) = match orientation & 3 {
            0 => (0, 1),
            1 => (-1, 0),
            2 => (0, -1),
            _ => (1, 0),
        };
        let left = (x - sx, z - sz);
        let right = (x + sx, z + sz);
        let adjacent_door = |(nx, nz): (i32, i32)| self.get(nx, y, nz) >> 4 == id;
        let solid_count = |(nx, nz): (i32, i32)| {
            [y, y + 1]
                .iter()
                .filter(|&&ny| full_conductor(self.get(nx, ny, nz) >> 4))
                .count()
        };
        let right_hinge = (adjacent_door(left) && !adjacent_door(right))
            || solid_count(right) > solid_count(left);
        let lower = (x, y as u8, z);
        let upper = (x, (y + 1) as u8, z);
        let old_lower = self.edits.get(&lower).copied();
        let old_upper = self.edits.get(&upper).copied();
        self.set_gravity_cell(lower, (id << 4) | (orientation & 3));
        self.set_gravity_cell(upper, (id << 4) | 8 | u16::from(right_hinge));
        let result = if self.edits.len() > super::MAX_EDITS {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "too many door edits",
            ))
        } else {
            self.save(path)
        };
        if let Err(error) = result {
            for (key, previous) in [(lower, old_lower), (upper, old_upper)] {
                match previous {
                    Some(value) => {
                        self.edits.insert(key, value);
                    }
                    None => {
                        self.edits.remove(&key);
                    }
                }
            }
            return Err(error);
        }
        self.mark_lighting_changed(x, z, 0, id << 4);
        self.wake_block_updates(x, y, z);
        self.wake_block_updates(x, y + 1, z);
        Ok(true)
    }

    /// Pressure plates stay down for ten ticks after last player contact.
    pub fn press_plate(&mut self, path: &Path, x: i32, y: i32, z: i32) -> io::Result<bool> {
        let block = self.get(x, y, z);
        if matches!(block >> 4, 147 | 148) {
            return self.press_weighted_plate(path, x, y, z, 1);
        }
        if !matches!(block >> 4, 70 | 72) {
            return Ok(false);
        }
        self.pending_buttons
            .insert((x, y as u8, z), self.fluid_phase + 10);
        if block & 1 != 0 {
            return Ok(false);
        }
        self.update_and_save(path, x, y, z, block | 1)
    }
}

#[cfg(test)]
mod redstone_regression_tests {
    use super::*;
    use std::fs;

    #[test]
    fn redstone_dust_strongly_powers_its_support_but_not_side_blocks() {
        let mut w = World::new();
        w.import_block(0, 64, 0, (55 << 4) | 9).unwrap();
        w.import_block(0, 63, 0, 1 << 4).unwrap();
        assert_eq!(w.strong_signal_into((0, 64, 0), (0, 63, 0)), 9);
        assert_eq!(w.signal_into((0, 63, 0), (1, 63, 0)), 9);
        for target in [(1, 64, 0), (-1, 64, 0), (0, 64, 1), (0, 64, -1), (0, 65, 0)] {
            assert_eq!(w.strong_signal_into((0, 64, 0), target), 0);
        }
        // Signal strength still determines the current carried through a cube.
        w.import_block(0, 64, 0, 55 << 4).unwrap();
        assert_eq!(w.signal_into((0, 63, 0), (1, 63, 0)), 0);
    }

    #[test]
    fn powered_repeater_only_outputs_in_its_facing_direction() {
        let mut w = World::new();
        w.import_block(0, 64, 0, (94 << 4) | 1).unwrap();
        assert!(w.power_into((0, 64, 0), (1, 64, 0)));
        for target in [(-1, 64, 0), (0, 64, -1), (0, 64, 1), (0, 65, 0)] {
            assert!(!w.power_into((0, 64, 0), target));
        }
        assert!(w.powered_near(1, 64, 0));
        assert!(!w.powered_near(-1, 64, 0));
        assert!(!w.powered_near(0, 64, 1));
    }

    #[test]
    fn redstone_torch_does_not_weakly_power_its_support() {
        let mut w = World::new();
        for (meta, support) in [
            (1, (-1, 64, 0)),
            (2, (1, 64, 0)),
            (3, (0, 64, -1)),
            (4, (0, 64, 1)),
            (5, (0, 63, 0)),
        ] {
            w.import_block(0, 64, 0, (76 << 4) | meta).unwrap();
            assert_eq!(w.signal_into((0, 64, 0), support), 0);
            assert_eq!(w.signal_into((0, 64, 0), (0, 65, 0)), 15);
            assert_eq!(w.strong_signal_into((0, 64, 0), (0, 65, 0)), 15);
        }
    }

    #[test]
    fn repeater_locks_when_a_powered_side_repeater_points_into_it() {
        let mut w = World::new();
        w.import_block(0, 64, 0, (93 << 4) | 1).unwrap();
        w.import_block(0, 64, 1, 94 << 4).unwrap(); // north-facing side lock
        assert!(w.repeater_locked(0, 64, 0, 1));
        w.import_block(0, 64, 1, 93 << 4).unwrap();
        assert!(!w.repeater_locked(0, 64, 0, 1));
        w.import_block(0, 64, 1, (94 << 4) | 2).unwrap();
        assert!(!w.repeater_locked(0, 64, 0, 1));
    }

    #[test]
    fn repeater_has_two_tick_minimum_delay_and_blocks_side_output() {
        let path =
            std::env::temp_dir().join(format!("ir1710-redstone-facing-{}.irw", std::process::id()));
        let _ = fs::remove_file(&path);
        let mut w = World::new();
        w.update_and_save(&path, -1, 64, 0, (69 << 4) | 8).unwrap();
        w.update_and_save(&path, 0, 64, 0, (93 << 4) | 1).unwrap();
        w.update_and_save(&path, 1, 64, 0, 55 << 4).unwrap();
        w.update_and_save(&path, 0, 64, 1, 55 << 4).unwrap();
        w.block_tick(&path, 64).unwrap();
        w.block_tick(&path, 64).unwrap();
        assert_eq!(w.get(0, 64, 0) >> 4, 93);
        assert_eq!(w.get(1, 64, 0) & 15, 0);
        w.block_tick(&path, 64).unwrap();
        assert_eq!(w.get(0, 64, 0) >> 4, 94);
        w.block_tick(&path, 64).unwrap();
        assert_eq!(w.get(1, 64, 0) & 15, 15);
        assert_eq!(w.get(0, 64, 1) & 15, 0);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn comparators_read_block_analog_overrides_without_powering_dust() {
        let mut w = World::new();
        let rear = (-1, 64, 0);
        let target = (0, 64, 0);
        for (block, expected) in [
            ((92 << 4), 14),
            ((92 << 4) | 6, 2),
            ((118 << 4) | 3, 3),
            ((118 << 4), 0),
            ((120 << 4) | 4, 15),
            ((120 << 4), 0),
        ] {
            w.import_block(rear.0, rear.1, rear.2, block).unwrap();
            assert_eq!(w.comparator_rear_input(rear, target), expected);
            assert_eq!(w.signal_into(rear, target), 0);
        }
    }

    #[test]
    fn cauldron_level_reaches_comparator_after_two_tick_delay() {
        let path = std::env::temp_dir().join(format!(
            "ir1710-comparator-cauldron-{}.irw",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        let mut w = World::new();
        w.update_and_save(&path, 0, 64, 0, (149 << 4) | 1).unwrap();
        w.update_and_save(&path, -1, 64, 0, (118 << 4) | 3).unwrap();
        for _ in 0..3 {
            w.block_tick(&path, 64).unwrap();
        }
        assert_eq!(w.get(0, 64, 0) >> 4, 150);
        assert_eq!(w.comparator_outputs.get(&(0, 64, 0)), Some(&3));
        w.update_and_save(&path, -1, 64, 0, (118 << 4) | 1).unwrap();
        for _ in 0..3 {
            w.block_tick(&path, 64).unwrap();
        }
        assert_eq!(w.comparator_outputs.get(&(0, 64, 0)), Some(&1));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn door_hinge_uses_adjacent_walls_and_neighboring_doors() {
        let path =
            std::env::temp_dir().join(format!("ir1710-door-hinge-{}.irw", std::process::id()));
        let _ = fs::remove_file(&path);
        let mut world = World::new();
        // Facing 0 has side cells at z=-1 and z=+1. A solid wall
        // on the right selects top-half hinge bit 0.
        world.import_block(0, 64, 1, 1 << 4).unwrap();
        world.import_block(0, 65, 1, 1 << 4).unwrap();
        assert!(world.place_door(&path, 0, 64, 0, 64, 0).unwrap());
        assert_eq!(world.get(0, 65, 0) & 9, 9);
        assert_eq!(World::load(&path).unwrap().get(0, 65, 0) & 9, 9);
        fs::remove_file(&path).unwrap();

        let mut paired = World::new();
        assert!(paired.place_door(&path, 0, 64, -1, 64, 0).unwrap());
        assert!(paired.place_door(&path, 0, 64, 0, 64, 0).unwrap());
        assert_eq!(paired.get(0, 65, 0) & 9, 9);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn piston_rejects_registered_tile_entities_and_keeps_solid_blocks_movable() {
        for id in [
            23, 25, 52, 54, 61, 62, 84, 116, 117, 130, 137, 138, 140, 144, 146, 151, 154, 158,
        ] {
            assert!(!movable(id << 4), "tile entity block {id} must not move");
        }
        assert!(movable(1 << 4));
        assert!(movable(5 << 4));
    }

    #[test]
    fn piston_pushes_exactly_twelve_not_thirteen_solid_blocks() {
        for number in [12, 13] {
            let path = std::env::temp_dir().join(format!(
                "ir1710-piston-cap-{}-{number}.irw",
                std::process::id()
            ));
            let _ = fs::remove_file(&path);
            let mut w = World::new();
            w.update_and_save(&path, -2, 64, 0, (69 << 4) | 8).unwrap();
            w.update_and_save(&path, -1, 64, 0, (33 << 4) | 5).unwrap();
            for x in 0..number {
                w.update_and_save(&path, x, 64, 0, 1 << 4).unwrap();
            }
            w.block_tick(&path, 64).unwrap();
            if number == 12 {
                assert_eq!(w.get(-1, 64, 0), (33 << 4) | 13);
                assert_eq!(w.get(12, 64, 0), 1 << 4);
            } else {
                assert_eq!(w.get(-1, 64, 0), (33 << 4) | 5);
                assert_eq!(w.get(13, 64, 0), 0);
            }
            fs::remove_file(path).unwrap();
        }
    }
}

#[cfg(test)]
mod connected_rail_regressions {
    use super::rails_connected;

    #[test]
    fn perpendicular_tracks_do_not_power_each_other() {
        assert!(!rails_connected((0, 64, 0), 0, (1, 64, 0), 1));
    }

    #[test]
    fn straight_tracks_connect_bidirectionally() {
        assert!(rails_connected((0, 64, 0), 1, (1, 64, 0), 1));
        assert!(rails_connected((1, 64, 0), 1, (0, 64, 0), 1));
    }

    #[test]
    fn rising_track_connects_to_upper_track() {
        assert!(rails_connected((0, 64, 0), 2, (1, 65, 0), 1));
    }
}
