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
fn movable(block: u16) -> bool {
    !matches!(
        block >> 4,
        0 | 7 | 8
            ..=11
                | 18
                | 29
                | 33
                | 34
                | 54
                | 61
                | 62
                | 64
                | 71
                | 75
                | 76
                | 93
                | 94
                | 96
                | 116
                | 117
                | 120
                | 130
                | 138
    )
}
impl World {
    fn power_at(&self, x: i32, y: i32, z: i32) -> bool {
        let value = self.get(x, y, z);
        match value >> 4 {
            55 => value & 15 > 0,
            69 | 77 | 143 => value & 8 != 0,
            70 | 72 => value & 1 != 0,
            76 | 94 | 152 => true,
            _ => false,
        }
    }
    fn powered_near(&self, x: i32, y: i32, z: i32) -> bool {
        NEIGHBORS
            .iter()
            .any(|&(dx, dy, dz)| self.power_at(x + dx, y + dy, z + dz))
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
        let on = self.powered_near(x, y, z);
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
                if !in_bounds(target.0, target.1, target.2) || occupied.len() >= 12 {
                    return;
                }
                let found = self.get(target.0, target.1, target.2);
                if found == 0 {
                    break;
                }
                if !movable(found) {
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
                let flag = if matches!(current >> 4, 70 | 72) {
                    1
                } else {
                    8
                };
                if matches!(current >> 4, 70 | 72 | 77 | 143) && current & flag != 0 {
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
                    let direction = match current & 3 {
                        0 => (0, 0, -1),
                        1 => (1, 0, 0),
                        2 => (0, 0, 1),
                        _ => (-1, 0, 0),
                    };
                    let desired = self.power_at(x - direction.0, y, z - direction.2);
                    let active = current >> 4 == 94;
                    let pos = (x, y as u8, z);
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
        let lower = (x, y as u8, z);
        let upper = (x, (y + 1) as u8, z);
        let old_lower = self.edits.get(&lower).copied();
        let old_upper = self.edits.get(&upper).copied();
        self.set_gravity_cell(lower, (id << 4) | (orientation & 3));
        self.set_gravity_cell(upper, (id << 4) | 8);
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
        self.wake_block_updates(x, y, z);
        self.wake_block_updates(x, y + 1, z);
        Ok(true)
    }

    /// Pressure plates stay down for ten ticks after last player contact.
    pub fn press_plate(&mut self, path: &Path, x: i32, y: i32, z: i32) -> io::Result<bool> {
        let block = self.get(x, y, z);
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
