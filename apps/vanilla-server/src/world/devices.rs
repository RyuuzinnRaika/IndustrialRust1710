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

/// Comparator compare mode preserves rear input only when it is at least
/// the strongest side input. Subtract mode removes that side strength.
fn comparator_strength(rear: u8, side: u8, subtract: bool) -> u8 {
    if subtract {
        rear.saturating_sub(side)
    } else if rear >= side {
        rear
    } else {
        0
    }
}

/// Full opaque cubes can pass weak current after a *strong* input.
fn full_conductor(id: u16) -> bool {
    matches!(
        id,
        1..=5 | 7 | 12..=17 | 19 | 21..=25 | 35 | 41..=43 | 45..=49
            | 56..=58 | 61 | 62 | 73 | 74 | 80 | 82 | 84 | 86..=89 | 91
            | 97..=100 | 103 | 110 | 112 | 121 | 123..=125 | 129 | 133
            | 137 | 152 | 153 | 155 | 158 | 159 | 162 | 170 | 172 | 173
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
                | 36 // moving piston (transient block, never pushable)
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
                | 90 // nether portal (immovable)
                | 93
                | 94
                | 96
                | 116
                | 117
                | 119 // end portal (immovable)
                | 120 // end portal frame (immovable)
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

/// Fragile attached redstone components have a piston destroy reaction
/// in vanilla 1.7.10. They terminate a push rather than being translated
/// as solid blocks, and sticky pistons cannot pull them back.
/// Item-drop handling is tracked separately.
fn piston_breakable(block: u16) -> bool {
    matches!(
        block >> 4,
        27 | 28 | 50 | 55 | 66 | 75 | 76 | 157
            | 69 // lever
            | 70 | 72 | 147 | 148 // pressure plates
            | 77 | 143 // buttons
            | 93 | 94 // redstone repeaters
            | 149 | 150 // comparators
            | 6 // saplings
            | 31 | 32 // tall grass and dead bush
            | 37
            ..=40 // flowers and mushrooms
            | 59 // wheat
            | 78 // snow layer
            | 83 // sugar cane
            | 104 | 105 // pumpkin and melon stems
            | 106 // vines
            | 115 // nether wart
            | 141 | 142 // carrots and potatoes
            | 171 // carpet
            | 175 // double-height plants
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
    pub(crate) fn signal_into(&self, from: (i32, i32, i32), to: (i32, i32, i32)) -> u8 {
        let (x, y, z) = from;
        let block = self.get(x, y, z);
        match block >> 4 {
            // Dust weakly powers horizontal neighbors and the block below,
            // but never the block directly above it (Java 1.7.10).
            55 if to == (x, y + 1, z) => 0,
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

    /// Read a vanilla comparator override without treating it as redstone power.
    /// Only the comparator's rear input is allowed to consume these values.
    fn comparator_override_at(&self, pos: (i32, i32, i32)) -> Option<u8> {
        let state = self.get(pos.0, pos.1, pos.2);
        match state >> 4 {
            // Inventory-backed blocks share the same analog snapshot map.
            // Their signals are never emitted as ordinary redstone power.
            23 | 54 | 61 | 62 | 146 | 154 | 158 => Some(
                self.device_comparator_inputs
                    .get(&(pos.0, pos.1 as u8, pos.2))
                    .copied()
                    .unwrap_or(0),
            ),
            92 => Some(14u8.saturating_sub((state as u8 & 7).min(7) * 2)),
            118 => Some((state & 3) as u8),
            120 => Some(if state & 4 != 0 { 15 } else { 0 }),
            _ => None,
        }
    }

    /// A comparator can read an inventory/analog override through exactly one
    /// full opaque block behind it, unless that block already supplies power 15.
    /// The intermediate block does not become an ordinary analog power source.
    fn comparator_rear_input(&self, from: (i32, i32, i32), to: (i32, i32, i32)) -> u8 {
        if let Some(strength) = self.comparator_override_at(from) {
            return strength;
        }
        let power = self.signal_into(from, to);
        if power < 15 && full_conductor(self.get(from.0, from.1, from.2) >> 4) {
            let behind = (
                from.0 + (from.0 - to.0),
                from.1 + (from.1 - to.1),
                from.2 + (from.2 - to.2),
            );
            if let Some(strength) = self.comparator_override_at(behind) {
                return strength;
            }
        }
        power
    }

    /// Refresh analog overrides from committed dispenser/dropper inventories.
    /// Wake adjacent comparators only when a source's strength changes.
    pub fn set_device_comparator_inputs(&mut self, inputs: BTreeMap<Position, u8>) {
        // Protocol 5 comparator output is always 0..=15. Clamp malformed
        // external inventory snapshots before comparing or storing them so
        // oversized values cannot leak into the redstone signal graph.
        let inputs = inputs
            .into_iter()
            .map(|(pos, strength)| (pos, strength.min(15)))
            .collect::<BTreeMap<_, _>>();
        let positions: std::collections::BTreeSet<_> = self
            .device_comparator_inputs
            .keys()
            .chain(inputs.keys())
            .copied()
            .collect();
        for pos in positions {
            if self.device_comparator_inputs.get(&pos) != inputs.get(&pos) {
                self.wake_block_updates(pos.0, i32::from(pos.1), pos.2);
            }
        }
        self.device_comparator_inputs = inputs;
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
            // Vanilla repeaters can also be locked by an active comparator
            // facing into either side, not just by another repeater.
            if matches!(self.get(source.0, source.1, source.2) >> 4, 94 | 150)
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
        // Only metadata 0..=5 denotes a piston face in 1.7.10.
        // Corrupt/unknown facings 6 and 7 must not default to east.
        if block & 7 > 5 {
            return;
        }
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
                // Pistons replace water/lava in their path instead of
                // treating the liquid as an immovable solid block.
                if found == 0 || matches!(found >> 4, 8..=11) || piston_breakable(found) {
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
            // A neighboring piston's head may occupy this cell. Only retract
            // the head that points in the same direction as this piston.
            let head_block = self.get(head.0, head.1, head.2);
            if head_block >> 4 == 34 && (head_block & 7) == (block & 7) {
                changes.insert((head.0, head.1 as u8, head.2), 0);
                if block >> 4 == 29 {
                    let far = (head.0 + dx, head.1 + dy, head.2 + dz);
                    if in_bounds(far.0, far.1, far.2) {
                        let pulled = self.get(far.0, far.1, far.2);
                        if pulled != 0 && movable(pulled) && !piston_breakable(pulled) {
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
                    let desired = comparator_strength(rear, side, current & 4 != 0);
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
    fn redstone_dust_never_weakly_powers_block_directly_above() {
        for strength in [0u16, 1, 7, 15] {
            let mut w = World::new();
            w.import_block(0, 63, 0, 1 << 4).unwrap();
            w.import_block(0, 64, 0, (55 << 4) | strength).unwrap();
            assert_eq!(
                w.signal_into((0, 64, 0), (0, 65, 0)),
                0,
                "dust strength {strength} must not power the block above"
            );
            assert!(!w.power_into((0, 64, 0), (0, 65, 0)));
            assert!(!w.powered_near(0, 65, 0));
            assert_eq!(
                w.signal_into((0, 64, 0), (1, 64, 0)),
                strength as u8,
                "horizontal dust output must be preserved"
            );
            assert_eq!(
                w.strong_signal_into((0, 64, 0), (0, 63, 0)),
                strength as u8,
                "dust must continue to strongly power its support"
            );
        }
    }

    #[test]
    fn vanilla_full_cubes_conduct_strong_redstone_but_partial_blocks_do_not() {
        let mut w = World::new();
        // Wool, double wooden slabs, quartz ore, newer logs, hay bales and
        // uncolored hardened clay are full opaque cubes in Java 1.7.10.
        // They must relay strong power just like stone.
        w.import_block(0, 65, 0, 152 << 4).unwrap();
        for id in [
            1u16, 21, 23, 25, 35, 84, 91, 97, 123, 124, 125, 153, 158, 162, 170, 172,
        ] {
            w.import_block(0, 64, 0, id << 4).unwrap();
            assert_eq!(
                w.signal_into((0, 64, 0), (1, 64, 0)),
                15,
                "full opaque block {id} should relay strong power"
            );
        }
        // Glass, wooden stairs and single slabs cannot relay it.
        for id in [20u16, 53, 44] {
            w.import_block(0, 64, 0, id << 4).unwrap();
            assert_eq!(
                w.signal_into((0, 64, 0), (1, 64, 0)),
                0,
                "non-full block {id} must not relay strong power"
            );
        }
    }

    #[test]
    fn opaque_device_blocks_relay_directional_strong_repeater_power() {
        let mut w = World::new();
        w.import_block(-1, 64, 0, (94 << 4) | 1).unwrap();
        // The repeater strongly powers the east face of the solid cube.
        // Jukeboxes, lamps and droppers must not block that power.
        for id in [21u16, 84, 91, 97, 123, 124, 158] {
            w.import_block(0, 64, 0, id << 4).unwrap();
            assert_eq!(
                w.signal_into((0, 64, 0), (1, 64, 0)),
                15,
                "opaque block {id} should relay repeater power"
            );
            assert_eq!(
                w.signal_into((0, 64, 0), (0, 65, 0)),
                15,
                "opaque block {id} should relay power upward"
            );
        }
        w.import_block(-1, 64, 0, (93 << 4) | 1).unwrap();
        assert_eq!(w.signal_into((0, 64, 0), (1, 64, 0)), 0);
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
    fn comparator_side_output_locks_repeater_only_when_facing_and_powered() {
        let mut w = World::new();
        // East-facing repeater: its south-side comparator must face north.
        w.import_block(0, 64, 0, (93 << 4) | 1).unwrap();
        w.import_block(0, 64, 1, 150 << 4).unwrap();
        w.comparator_outputs.insert((0, 64, 1), 9);
        assert!(w.repeater_locked(0, 64, 0, 1));

        // A zero-strength comparator does not lock, even if its block is on.
        w.comparator_outputs.insert((0, 64, 1), 0);
        assert!(!w.repeater_locked(0, 64, 0, 1));

        // Comparator output pointing away from the repeater cannot lock it.
        w.comparator_outputs.insert((0, 64, 1), 9);
        w.import_block(0, 64, 1, (150 << 4) | 2).unwrap();
        assert!(!w.repeater_locked(0, 64, 0, 1));

        // An unpowered comparator does not lock the repeater.
        w.import_block(0, 64, 1, 149 << 4).unwrap();
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
    fn comparator_reads_override_through_one_solid_block_only() {
        let mut w = World::new();
        let comparator = (0, 64, 0);
        let intermediate = (-1, 64, 0);
        let inventory = (-2, 64, 0);
        w.import_block(intermediate.0, intermediate.1, intermediate.2, 1 << 4)
            .unwrap();
        w.import_block(inventory.0, inventory.1, inventory.2, 23 << 4)
            .unwrap();
        w.device_comparator_inputs.insert((-2, 64, 0), 9);
        assert_eq!(w.comparator_rear_input(intermediate, comparator), 9);
        // Inventory overrides never power ordinary dust through stone.
        assert_eq!(w.signal_into(inventory, intermediate), 0);

        // A transparent block cannot bridge the comparator to an inventory.
        w.import_block(intermediate.0, intermediate.1, intermediate.2, 20 << 4)
            .unwrap();
        assert_eq!(w.comparator_rear_input(intermediate, comparator), 0);

        // A second opaque block does not extend the override lookup.
        w.import_block(intermediate.0, intermediate.1, intermediate.2, 1 << 4)
            .unwrap();
        w.import_block(inventory.0, inventory.1, inventory.2, 1 << 4)
            .unwrap();
        w.import_block(-3, 64, 0, 23 << 4).unwrap();
        w.device_comparator_inputs.insert((-3, 64, 0), 11);
        assert_eq!(w.comparator_rear_input(intermediate, comparator), 0);
    }

    #[test]
    fn comparator_direct_analog_override_takes_precedence_over_redstone() {
        let mut w = World::new();
        let comparator = (0, 64, 0);
        let rear = (-1, 64, 0);
        w.import_block(rear.0, rear.1, rear.2, (118 << 4) | 2)
            .unwrap();
        assert_eq!(w.comparator_rear_input(rear, comparator), 2);
        w.import_block(rear.0, rear.1, rear.2, 23 << 4).unwrap();
        w.device_comparator_inputs.insert((-1, 64, 0), 6);
        assert_eq!(w.comparator_rear_input(rear, comparator), 6);
    }

    #[test]
    fn comparator_reads_container_analog_snapshots_without_powering_dust() {
        // The 20TPS worker currently supplies dispenser/dropper snapshots.
        // Other container snapshots can use the same input path when wired.
        for id in [23u16, 54, 61, 62, 146, 154, 158] {
            let mut w = World::new();
            let rear = (-1, 64, 0);
            let comparator = (0, 64, 0);
            w.import_block(rear.0, rear.1, rear.2, id << 4).unwrap();
            w.device_comparator_inputs.insert((-1, 64, 0), 9);
            assert_eq!(
                w.comparator_rear_input(rear, comparator),
                9,
                "container {id} must expose its analog inventory signal"
            );
            assert_eq!(
                w.signal_into(rear, comparator),
                0,
                "container {id} must not emit ordinary redstone power"
            );
            w.device_comparator_inputs.remove(&(-1, 64, 0));
            assert_eq!(
                w.comparator_rear_input(rear, comparator),
                0,
                "container {id} must reset when its inventory becomes empty"
            );
        }
    }

    #[test]
    fn comparator_inventory_snapshots_cannot_exceed_vanilla_signal_strength() {
        let mut w = World::new();
        w.import_block(-1, 64, 0, 23 << 4).unwrap();
        let mut inputs = BTreeMap::new();
        inputs.insert((-1, 64, 0), u8::MAX);
        w.set_device_comparator_inputs(inputs);
        assert_eq!(w.device_comparator_inputs.get(&(-1, 64, 0)), Some(&15));
        assert_eq!(w.comparator_rear_input((-1, 64, 0), (0, 64, 0)), 15);
        assert_eq!(w.signal_into((-1, 64, 0), (0, 64, 0)), 0);

        // The same cap applies when reading through one opaque block.
        w.import_block(-1, 64, 0, 1 << 4).unwrap();
        w.import_block(-2, 64, 0, 158 << 4).unwrap();
        let mut inputs = BTreeMap::new();
        inputs.insert((-2, 64, 0), 200);
        w.set_device_comparator_inputs(inputs);
        assert_eq!(w.comparator_rear_input((-1, 64, 0), (0, 64, 0)), 15);

        // Valid analog values remain unchanged, including zero.
        let mut inputs = BTreeMap::new();
        inputs.insert((-2, 64, 0), 7);
        w.set_device_comparator_inputs(inputs);
        assert_eq!(w.comparator_rear_input((-1, 64, 0), (0, 64, 0)), 7);
        let mut inputs = BTreeMap::new();
        inputs.insert((-2, 64, 0), 0);
        w.set_device_comparator_inputs(inputs);
        assert_eq!(w.comparator_rear_input((-1, 64, 0), (0, 64, 0)), 0);
    }

    #[test]
    fn comparator_reads_container_analog_through_one_opaque_block() {
        for id in [54u16, 61, 62, 146, 154] {
            let mut w = World::new();
            w.import_block(-1, 64, 0, 1 << 4).unwrap();
            w.import_block(-2, 64, 0, id << 4).unwrap();
            w.device_comparator_inputs.insert((-2, 64, 0), 6);
            assert_eq!(
                w.comparator_rear_input((-1, 64, 0), (0, 64, 0)),
                6,
                "container {id} must be readable through one opaque block"
            );
            w.import_block(-1, 64, 0, 20 << 4).unwrap();
            assert_eq!(
                w.comparator_rear_input((-1, 64, 0), (0, 64, 0)),
                0,
                "glass must not bridge container {id} analog input"
            );
        }
    }

    #[test]
    fn comparator_compare_and_subtract_modes_preserve_analog_strength() {
        // Every vanilla redstone strength must be handled without wrapping.
        for rear in 0..=15u8 {
            for side in 0..=15u8 {
                assert_eq!(
                    comparator_strength(rear, side, false),
                    if rear >= side { rear } else { 0 }
                );
                assert_eq!(
                    comparator_strength(rear, side, true),
                    rear.saturating_sub(side)
                );
            }
        }

        let path = std::env::temp_dir().join(format!(
            "ir1710-comparator-modes-{}.irw",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        let mut w = World::new();
        // Facing east: rear is west, and the side input is south.
        // Use a pre-energized weighted pressure plate as a stable side input.
        // Unpowered dust decays, while daylight sensors are recalculated by
        // the day/night cycle; neither holds an arbitrary analog test value.
        w.update_and_save(&path, 0, 64, 0, (149 << 4) | 1).unwrap();
        w.update_and_save(&path, -1, 64, 0, 92 << 4).unwrap(); // Whole cake: 14.
        w.update_and_save(&path, 0, 64, 1, (147 << 4) | 5).unwrap();
        for _ in 0..3 {
            w.block_tick(&path, 64).unwrap();
        }
        assert_eq!(w.comparator_outputs.get(&(0, 64, 0)), Some(&14));

        // Interacting with the comparator switches it to subtraction mode.
        assert_eq!(
            w.interact_device(&path, 0, 64, 0).unwrap(),
            vec![(0, 64, 0)]
        );
        for _ in 0..3 {
            w.block_tick(&path, 64).unwrap();
        }
        assert_eq!(w.comparator_outputs.get(&(0, 64, 0)), Some(&9));

        // Stronger side input must turn the comparator off in subtract mode.
        w.update_and_save(&path, 0, 64, 1, (147 << 4) | 15).unwrap();
        for _ in 0..3 {
            w.block_tick(&path, 64).unwrap();
        }
        // Zero-strength outputs may be omitted from the sparse output map.
        assert_eq!(
            w.comparator_outputs.get(&(0, 64, 0)).copied().unwrap_or(0),
            0
        );
        assert_eq!(w.get(0, 64, 0) >> 4, 149);
        fs::remove_file(path).unwrap();
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
    fn piston_does_not_move_portals_or_end_portal_frames() {
        for id in [90u16, 119, 120] {
            let mut world = World::new();
            world.import_block(-2, 64, 0, (69 << 4) | 8).unwrap();
            world.import_block(-1, 64, 0, (33 << 4) | 5).unwrap();
            world.import_block(0, 64, 0, id << 4).unwrap();

            assert!(!movable(id << 4), "portal block {id} must not move");
            let mut changes = BTreeMap::new();
            world.push_piston(-1, 64, 0, (33 << 4) | 5, &mut changes);
            assert!(
                changes.is_empty(),
                "piston must not overwrite or displace portal block {id}"
            );
        }
    }

    #[test]
    fn piston_does_not_push_moving_piston_blocks() {
        // ID 36 is the transient moving-piston block, not an ordinary solid.
        // It must not be displaced or overwritten by another piston.
        for piston_id in [29u16, 33] {
            let mut world = World::new();
            world.import_block(-2, 64, 0, (69 << 4) | 8).unwrap();
            world.import_block(-1, 64, 0, (piston_id << 4) | 5).unwrap();
            world.import_block(0, 64, 0, 36 << 4).unwrap();

            assert!(!movable(36 << 4));
            let mut changes = BTreeMap::new();
            world.push_piston(-1, 64, 0, (piston_id << 4) | 5, &mut changes);
            assert!(
                changes.is_empty(),
                "piston {piston_id} must not push a moving piston block"
            );
        }
    }

    #[test]
    fn piston_retraction_does_not_erase_a_foreign_facing_head() {
        for piston_id in [29u16, 33] {
            let mut world = World::new();
            let piston = (piston_id << 4) | 13;
            world.import_block(0, 64, 0, piston).unwrap();
            // The adjacent head belongs to a different orientation.
            world.import_block(1, 64, 0, (34 << 4) | 2).unwrap();
            world.import_block(2, 64, 0, 1 << 4).unwrap();

            let mut changes = BTreeMap::new();
            world.push_piston(0, 64, 0, piston, &mut changes);
            assert_eq!(changes.len(), 1);
            assert_eq!(changes.get(&(0, 64, 0)), Some(&((piston_id << 4) | 5)));
            assert!(!changes.contains_key(&(1, 64, 0)));
            assert!(!changes.contains_key(&(2, 64, 0)));
        }
    }

    #[test]
    fn sticky_piston_retraction_still_pulls_through_matching_head() {
        let mut world = World::new();
        let piston = (29 << 4) | 13;
        world.import_block(0, 64, 0, piston).unwrap();
        world.import_block(1, 64, 0, (34 << 4) | 5).unwrap();
        world.import_block(2, 64, 0, 1 << 4).unwrap();

        let mut changes = BTreeMap::new();
        world.push_piston(0, 64, 0, piston, &mut changes);
        assert_eq!(changes.get(&(0, 64, 0)), Some(&((29 << 4) | 5)));
        assert_eq!(changes.get(&(1, 64, 0)), Some(&(1 << 4)));
        assert_eq!(changes.get(&(2, 64, 0)), Some(&0));
    }

    #[test]
    fn piston_displaces_liquids_without_moving_them_as_solid_blocks() {
        for liquid_id in [8u16, 9, 10, 11] {
            for blocks_before_liquid in [0, 1] {
                let mut world = World::new();
                let piston = (33 << 4) | 5; // Facing east.
                world.import_block(-2, 64, 0, (69 << 4) | 8).unwrap();
                world.import_block(-1, 64, 0, piston).unwrap();
                if blocks_before_liquid == 1 {
                    world.import_block(0, 64, 0, 1 << 4).unwrap();
                }
                world
                    .import_block(blocks_before_liquid, 64, 0, liquid_id << 4)
                    .unwrap();

                let mut changes = BTreeMap::new();
                world.push_piston(-1, 64, 0, piston, &mut changes);
                assert_eq!(changes.get(&(-1, 64, 0)), Some(&(piston | 8)));
                assert_eq!(changes.get(&(0, 64, 0)), Some(&((34 << 4) | 5)));
                if blocks_before_liquid == 1 {
                    assert_eq!(changes.get(&(1, 64, 0)), Some(&(1 << 4)));
                } else {
                    assert!(!changes.contains_key(&(1, 64, 0)));
                }
                assert!(
                    !changes.values().any(|block| block >> 4 == liquid_id),
                    "piston must displace liquid {liquid_id}, not push it"
                );
            }
        }
    }

    #[test]
    fn piston_replaces_torches_instead_of_pushing_them() {
        for torch_id in [50u16, 75, 76] {
            for blocks_before_torch in [0, 1] {
                let mut world = World::new();
                let piston = (33 << 4) | 5; // Facing east.
                world.import_block(-2, 64, 0, (69 << 4) | 8).unwrap();
                world.import_block(-1, 64, 0, piston).unwrap();
                if blocks_before_torch == 1 {
                    world.import_block(0, 64, 0, 1 << 4).unwrap();
                }
                world
                    .import_block(blocks_before_torch, 64, 0, torch_id << 4)
                    .unwrap();

                let mut changes = BTreeMap::new();
                world.push_piston(-1, 64, 0, piston, &mut changes);
                assert_eq!(changes.get(&(-1, 64, 0)), Some(&(piston | 8)));
                assert_eq!(changes.get(&(0, 64, 0)), Some(&((34 << 4) | 5)));
                if blocks_before_torch == 1 {
                    assert_eq!(changes.get(&(1, 64, 0)), Some(&(1 << 4)));
                } else {
                    assert!(!changes.contains_key(&(1, 64, 0)));
                }
                assert!(
                    !changes.values().any(|block| block >> 4 == torch_id),
                    "piston must not move torch {torch_id}"
                );
            }
        }
    }

    #[test]
    fn sticky_piston_does_not_pull_a_torch() {
        for torch_id in [50u16, 75, 76] {
            let mut world = World::new();
            let piston = (29 << 4) | 13;
            world.import_block(0, 64, 0, piston).unwrap();
            world.import_block(1, 64, 0, (34 << 4) | 5).unwrap();
            world.import_block(2, 64, 0, torch_id << 4).unwrap();

            let mut changes = BTreeMap::new();
            world.push_piston(0, 64, 0, piston, &mut changes);
            assert_eq!(changes.get(&(0, 64, 0)), Some(&((29 << 4) | 5)));
            assert_eq!(changes.get(&(1, 64, 0)), Some(&0));
            assert!(!changes.contains_key(&(2, 64, 0)));
        }
    }

    #[test]
    fn piston_breaks_rails_and_redstone_wire_instead_of_moving_them() {
        // Powered, detector, ordinary and activator rails, plus dust, are
        // fragile piston-destroy blocks in vanilla 1.7.10.
        for fragile_id in [27u16, 28, 55, 66, 157] {
            for blocks_before_fragile in [0, 1] {
                let mut world = World::new();
                let piston = (33 << 4) | 5; // Facing east.
                world.import_block(-2, 64, 0, (69 << 4) | 8).unwrap();
                world.import_block(-1, 64, 0, piston).unwrap();
                if blocks_before_fragile == 1 {
                    world.import_block(0, 64, 0, 1 << 4).unwrap();
                }
                world
                    .import_block(blocks_before_fragile, 64, 0, fragile_id << 4)
                    .unwrap();

                assert!(piston_breakable(fragile_id << 4));
                let mut changes = BTreeMap::new();
                world.push_piston(-1, 64, 0, piston, &mut changes);
                assert_eq!(changes.get(&(-1, 64, 0)), Some(&(piston | 8)));
                assert_eq!(changes.get(&(0, 64, 0)), Some(&((34 << 4) | 5)));
                if blocks_before_fragile == 1 {
                    assert_eq!(changes.get(&(1, 64, 0)), Some(&(1 << 4)));
                } else {
                    assert!(!changes.contains_key(&(1, 64, 0)));
                }
                assert!(
                    !changes.values().any(|block| block >> 4 == fragile_id),
                    "piston must not translate fragile block {fragile_id}"
                );
            }
        }
    }

    #[test]
    fn sticky_piston_does_not_pull_rails_or_redstone_wire() {
        for fragile_id in [27u16, 28, 55, 66, 157] {
            let mut world = World::new();
            let piston = (29 << 4) | 13;
            world.import_block(0, 64, 0, piston).unwrap();
            world.import_block(1, 64, 0, (34 << 4) | 5).unwrap();
            world.import_block(2, 64, 0, fragile_id << 4).unwrap();

            let mut changes = BTreeMap::new();
            world.push_piston(0, 64, 0, piston, &mut changes);
            assert_eq!(changes.get(&(0, 64, 0)), Some(&((29 << 4) | 5)));
            assert_eq!(changes.get(&(1, 64, 0)), Some(&0));
            assert!(
                !changes.contains_key(&(2, 64, 0)),
                "sticky piston must not pull fragile block {fragile_id}"
            );
        }
    }

    #[test]
    fn pistons_destroy_fragile_plants_instead_of_translating_them() {
        // Vanilla 1.7.10 uproots these non-solid plants and coverings when
        // a piston extends; a sticky piston must not pull them back.
        // Drops are intentionally out of scope for this regression.
        for fragile_id in [
            6u16, 31, 32, 37, 38, 39, 40, 59, 78, 83, 104, 105, 106, 115, 141, 142, 171, 175,
        ] {
            assert!(piston_breakable(fragile_id << 4));
            for solids_ahead in [0, 1] {
                let mut world = World::new();
                let piston = (33 << 4) | 5; // East-facing normal piston.
                world.import_block(-2, 64, 0, (69 << 4) | 8).unwrap();
                world.import_block(-1, 64, 0, piston).unwrap();
                if solids_ahead == 1 {
                    world.import_block(0, 64, 0, 1 << 4).unwrap();
                }
                world
                    .import_block(solids_ahead, 64, 0, fragile_id << 4)
                    .unwrap();
                let mut changes = BTreeMap::new();
                world.push_piston(-1, 64, 0, piston, &mut changes);
                assert_eq!(changes.get(&(-1, 64, 0)), Some(&(piston | 8)));
                assert_eq!(changes.get(&(0, 64, 0)), Some(&((34 << 4) | 5)));
                if solids_ahead == 1 {
                    assert_eq!(changes.get(&(1, 64, 0)), Some(&(1 << 4)));
                }
                assert!(
                    !changes.values().any(|block| block >> 4 == fragile_id),
                    "piston must not translate plant {fragile_id}"
                );
            }

            let mut world = World::new();
            let sticky = (29 << 4) | 13; // Extended sticky piston, east.
            world.import_block(0, 64, 0, sticky).unwrap();
            world.import_block(1, 64, 0, (34 << 4) | 5).unwrap();
            world.import_block(2, 64, 0, fragile_id << 4).unwrap();
            let mut changes = BTreeMap::new();
            world.push_piston(0, 64, 0, sticky, &mut changes);
            assert_eq!(changes.get(&(0, 64, 0)), Some(&((29 << 4) | 5)));
            assert_eq!(changes.get(&(1, 64, 0)), Some(&0));
            assert!(
                !changes.contains_key(&(2, 64, 0)),
                "sticky piston must not pull plant {fragile_id}"
            );
        }
    }

    #[test]
    fn pistons_destroy_surface_redstone_devices_instead_of_moving_them() {
        // These attached redstone blocks are destroyed by a piston push in
        // vanilla 1.7.10. Their item drops are handled separately.
        for fragile_id in [69u16, 70, 72, 77, 93, 94, 143, 147, 148, 149, 150] {
            assert!(piston_breakable(fragile_id << 4));
            for blocks_before_fragile in [0, 1] {
                let mut world = World::new();
                let piston = (33 << 4) | 5; // East-facing piston.
                world.import_block(-2, 64, 0, (69 << 4) | 8).unwrap();
                world.import_block(-1, 64, 0, piston).unwrap();
                if blocks_before_fragile == 1 {
                    world.import_block(0, 64, 0, 1 << 4).unwrap();
                }
                world
                    .import_block(blocks_before_fragile, 64, 0, fragile_id << 4)
                    .unwrap();

                let mut changes = BTreeMap::new();
                world.push_piston(-1, 64, 0, piston, &mut changes);
                assert_eq!(changes.get(&(-1, 64, 0)), Some(&(piston | 8)));
                assert_eq!(changes.get(&(0, 64, 0)), Some(&((34 << 4) | 5)));
                if blocks_before_fragile == 1 {
                    assert_eq!(changes.get(&(1, 64, 0)), Some(&(1 << 4)));
                } else {
                    assert!(!changes.contains_key(&(1, 64, 0)));
                }
                assert!(
                    !changes.values().any(|block| block >> 4 == fragile_id),
                    "piston must not translate redstone component {fragile_id}"
                );
            }

            let mut world = World::new();
            let piston = (29 << 4) | 13; // Extended sticky piston, east.
            world.import_block(0, 64, 0, piston).unwrap();
            world.import_block(1, 64, 0, (34 << 4) | 5).unwrap();
            world.import_block(2, 64, 0, fragile_id << 4).unwrap();
            let mut changes = BTreeMap::new();
            world.push_piston(0, 64, 0, piston, &mut changes);
            assert_eq!(changes.get(&(0, 64, 0)), Some(&((29 << 4) | 5)));
            assert_eq!(changes.get(&(1, 64, 0)), Some(&0));
            assert!(
                !changes.contains_key(&(2, 64, 0)),
                "sticky piston must not pull redstone component {fragile_id}"
            );
        }
    }

    #[test]
    fn invalid_piston_facing_never_pushes_or_retracts_blocks() {
        for piston_id in [29u16, 33] {
            for orientation in [6u16, 7] {
                for extended in [false, true] {
                    for powered in [false, true] {
                        let mut world = World::new();
                        let extended_bit = if extended { 8 } else { 0 };
                        let piston = (piston_id << 4) | orientation | extended_bit;
                        world.import_block(0, 64, 0, piston).unwrap();
                        world.import_block(1, 64, 0, 1 << 4).unwrap();
                        if powered {
                            world.import_block(-1, 64, 0, (69 << 4) | 8).unwrap();
                        }

                        let mut changes = BTreeMap::new();
                        world.push_piston(0, 64, 0, piston, &mut changes);
                        assert!(
                            changes.is_empty(),
                            "piston {piston_id} with invalid facing {orientation} must not act"
                        );
                    }
                }
            }
        }
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
