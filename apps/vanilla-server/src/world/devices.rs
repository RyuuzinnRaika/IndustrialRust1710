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
    /// Whether a source powers a specific neighboring destination.
    /// Powered repeaters only output toward their face, never into their
    /// own input or perpendicular dust/pistons.
    pub(super) fn power_into(
        &self,
        from: (i32, i32, i32),
        to: (i32, i32, i32),
    ) -> bool {
        let (x, y, z) = from;
        let block = self.get(x, y, z);
        match block >> 4 {
            55 => block & 15 > 0,
            69 | 77 | 143 => block & 8 != 0,
            70 | 72 => block & 1 != 0,
            76 | 152 => true,
            94 => {
                let (dx, dz) = repeater_direction(block);
                to == (x + dx, y, z + dz)
            }
            _ => false,
        }
    }

    fn powered_near(&self, x: i32, y: i32, z: i32) -> bool {
        NEIGHBORS.iter().any(|&(dx, dy, dz)| {
            self.power_into((x + dx, y + dy, z + dz), (x, y, z))
        })
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
        self.mark_lighting_changed(x, z, 0, id << 4);
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

#[cfg(test)]
mod redstone_regression_tests {
    use super::*;
    use std::fs;

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
        let path = std::env::temp_dir().join(format!(
            "ir1710-redstone-facing-{}.irw",
            std::process::id()
        ));
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
