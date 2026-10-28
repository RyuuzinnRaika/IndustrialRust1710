//! Sparse, budgeted vegetation, agriculture and attachment updates.
//! Executed by the server's existing 20TPS block worker; no global chunk scan.
use super::{in_bounds, Position, World};
use std::collections::BTreeMap;

fn solid(id: u16) -> bool {
    !matches!(id, 0 | 6 | 8..=11 | 18 | 20 | 161 | 31 | 37..=40 | 50 | 55 | 59
        | 63 | 66 | 69 | 70 | 72 | 75..=78 | 83 | 93 | 94 | 96
        | 104 | 105 | 106 | 107 | 115 | 127 | 141 | 142 | 143 | 149 | 150)
}
fn ecology_roll(phase: u64, x: i32, y: i32, z: i32) -> u64 {
    let mut v = phase
        ^ (x as i64 as u64).wrapping_mul(0x9e3779b185ebca87)
        ^ (y as i64 as u64).wrapping_mul(0xc2b2ae3d27d4eb4f)
        ^ (z as i64 as u64).wrapping_mul(0x165667b19e3779f9);
    v ^= v >> 30;
    v = v.wrapping_mul(0xbf58476d1ce4e5b9);
    v ^= v >> 27;
    v = v.wrapping_mul(0x94d049bb133111eb);
    v ^ (v >> 31)
}

fn chance(phase: u64, x: i32, y: i32, z: i32, divisor: u64) -> bool {
    ecology_roll(phase, x, y, z).is_multiple_of(divisor)
}

/// Java 1.7.10's default randomTickSpeed is 3 selections per 16^3
/// section per game tick. This server schedules wakeable leaves in
/// one-second batches rather than simulating every loaded section.
/// 20 ticks * 3 / 4096 approximates the same per-leaf hazard (68.27s
/// average), without 4096 empty block checks per active section.
const LEAF_RANDOM_TICK_SPEED: u64 = 3;
const SECTION_VOLUME: u64 = 16 * 16 * 16;
const GAME_TICKS_PER_SECOND: u64 = 20;

fn leaf_decay_due(phase: u64, x: i32, y: i32, z: i32) -> bool {
    phase != 0
        && phase.is_multiple_of(GAME_TICKS_PER_SECOND)
        && ecology_roll(phase / GAME_TICKS_PER_SECOND, x, y, z) % SECTION_VOLUME
            < LEAF_RANDOM_TICK_SPEED * GAME_TICKS_PER_SECOND
}

fn plantable(id: u16) -> bool {
    matches!(id, 2 | 3 | 60)
}
/// Known combustible vanilla blocks. TNT must be handled by a primed-TNT
/// entity path rather than silently replaced by ordinary fire.
fn flammable(id: u16) -> bool {
    matches!(
        id,
        5 | 6 | 17 | 18 | 31 | 35 | 37..=40 | 47 | 53 | 54 | 58 | 59
            | 63 | 64 | 68 | 72 | 84..=86 | 91 | 96 | 103..=107
            | 125..=127 | 134..=136 | 143 | 146 | 161..=164
            | 170 | 171 | 175
    )
}

impl World {
    /// Rain cannot extinguish a fire under a solid roof. Scan at most the
    /// remaining height of the column (255 blocks) and avoid light-array
    /// recomputation inside the ecology tick.
    fn rain_reaches(&self, x: i32, y: i32, z: i32) -> bool {
        self.raining
            && self.dimension() == crate::dimensions::Dimension::Overworld
            && (y + 1..256).all(|above| {
                matches!(
                    self.get(x, above, z) >> 4,
                    0 | 31 | 32 | 37..=40 | 50 | 51 | 55 | 59 | 63
                        | 65 | 66 | 69 | 70 | 72 | 75..=78 | 83
                        | 93 | 94 | 106 | 111 | 141..=143 | 157
                )
            })
    }

    /// Attachment validity also used to reject impossible placements.
    pub fn can_support(&self, x: i32, y: i32, z: i32, block: u16) -> bool {
        let id = block >> 4;
        let below = self.get(x, y - 1, z) >> 4;
        match id {
            6 | 31 | 37 | 38 => plantable(below),
            32 => matches!(below, 12 | 159 | 172), // dead bush on sand/terracotta
            25 | 63 | 84 | 92 | 140 => solid(below),
            68 => {
                let neighbor = match block & 7 {
                    2 => self.get(x, y, z + 1) >> 4,
                    3 => self.get(x, y, z - 1) >> 4,
                    4 => self.get(x + 1, y, z) >> 4,
                    5 => self.get(x - 1, y, z) >> 4,
                    _ => 0,
                };
                solid(neighbor)
            }
            59 | 141 | 142 | 104 | 105 => below == 60,
            115 => below == 88, // Nether wart grows on soul sand.
            81 => {
                matches!(below, 12 | 81)
                    && [(1, 0), (-1, 0), (0, 1), (0, -1)]
                        .iter()
                        .all(|&(dx, dz)| !solid(self.get(x + dx, y, z + dz) >> 4))
            }
            83 => {
                if below == 83 {
                    return true;
                }
                matches!(below, 2 | 3 | 12)
                    && [(1, 0), (-1, 0), (0, 1), (0, -1)]
                        .iter()
                        .any(|&(dx, dz)| matches!(self.get(x + dx, y - 1, z + dz) >> 4, 8 | 9))
            }
            127 => {
                let (dx, dz) = match block & 3 {
                    0 => (0, 1),
                    1 => (-1, 0),
                    2 => (0, -1),
                    _ => (1, 0),
                };
                let support = self.get(x + dx, y, z + dz);
                support >> 4 == 17 && support & 3 == 3
            }
            50 | 75 | 76 => {
                let support = match block & 7 {
                    1 => self.get(x - 1, y, z) >> 4,
                    2 => self.get(x + 1, y, z) >> 4,
                    3 => self.get(x, y, z - 1) >> 4,
                    4 => self.get(x, y, z + 1) >> 4,
                    _ => below,
                };
                solid(support)
            }
            55 | 66 | 27 | 28 | 157 | 70 | 72 | 93 | 94 | 132 | 147 | 148 | 149 | 150 | 151 => {
                solid(below)
            }
            78 => solid(below) || below == 79,
            131 => {
                // 1.7.10 hook metadata 0=S, 1=W, 2=N, 3=E.
                // The supporting wall is behind the direction it faces.
                let neighbor = match block & 3 {
                    0 => self.get(x, y, z - 1) >> 4,
                    1 => self.get(x + 1, y, z) >> 4,
                    2 => self.get(x, y, z + 1) >> 4,
                    _ => self.get(x - 1, y, z) >> 4,
                };
                solid(neighbor)
            }
            77 | 143 => {
                let neighbor = match block & 7 {
                    1 => self.get(x - 1, y, z) >> 4,
                    2 => self.get(x + 1, y, z) >> 4,
                    3 => self.get(x, y, z - 1) >> 4,
                    4 => self.get(x, y, z + 1) >> 4,
                    _ => below,
                };
                solid(neighbor)
            }
            64 | 71 => {
                if block & 8 != 0 {
                    self.get(x, y - 1, z) >> 4 == id
                } else {
                    solid(below)
                }
            }
            96 => {
                let neighbor = match block & 3 {
                    0 => self.get(x, y, z + 1) >> 4,
                    1 => self.get(x, y, z - 1) >> 4,
                    2 => self.get(x + 1, y, z) >> 4,
                    _ => self.get(x - 1, y, z) >> 4,
                };
                solid(neighbor)
            }
            _ => true,
        }
    }
    pub(super) fn is_cold_biome(&self, x: i32, z: i32) -> bool {
        matches!(self.biome_at(x, z), 5 | 10 | 11 | 12 | 13 | 26 | 30 | 31)
    }
    fn wet_farmland(&self, x: i32, y: i32, z: i32) -> bool {
        for dx in -4..=4 {
            for dz in -4..=4 {
                for dy in 0..=1 {
                    if matches!(self.get(x + dx, y + dy, z + dz) >> 4, 8 | 9) {
                        return true;
                    }
                }
            }
        }
        false
    }
    /// A leaf survives only if a log can be reached through at most four
    /// face-adjacent leaves. Nearby but disconnected logs do not prevent decay.
    fn leaf_connected(&self, x: i32, y: i32, z: i32) -> bool {
        use std::collections::{BTreeSet, VecDeque};
        let mut seen = BTreeSet::new();
        let mut queue = VecDeque::from([(x, y, z, 0u8)]);
        seen.insert((x, y, z));
        while let Some((px, py, pz, depth)) = queue.pop_front() {
            for (dx, dy, dz) in [
                (1, 0, 0),
                (-1, 0, 0),
                (0, 1, 0),
                (0, -1, 0),
                (0, 0, 1),
                (0, 0, -1),
            ] {
                let next = (px + dx, py + dy, pz + dz);
                match self.get(next.0, next.1, next.2) >> 4 {
                    17 | 162 => return true,
                    18 | 161 if depth < 3 && seen.insert(next) => {
                        queue.push_back((next.0, next.1, next.2, depth + 1));
                    }
                    _ => {}
                }
            }
        }
        false
    }
    pub(super) fn grow_tree(&self, x: i32, y: i32, z: i32, changes: &mut BTreeMap<Position, u16>) {
        if y > 249 || !plantable(self.get(x, y - 1, z) >> 4) {
            return;
        }
        // Large dark oak is only legal for a complete 2x2 group.
        // Reuse this path for both natural growth and bone meal.
        if self.get(x, y, z) & 7 == 5 {
            self.grow_dark_oak(x, y, z, changes);
            return;
        }
        // Retain legacy log and leaf species for smaller saplings.
        let (log, log_meta, leaves, leaf_meta) = match self.get(x, y, z) & 7 {
            0 => (17, 0, 18, 0),   // Oak
            1 => (17, 1, 18, 1),   // Spruce
            2 => (17, 2, 18, 2),   // Birch
            3 => (17, 3, 18, 3),   // Jungle
            4 => (162, 0, 161, 0), // Acacia
            _ => return,
        };
        // First pass checks every eventual occupied cell; tree generation
        // must never overwrite players' buildings or another sapling.
        for dy in 1..=5 {
            let radius = if dy >= 3 { 2 } else { 0 };
            for dx in -radius..=radius {
                for dz in -radius..=radius {
                    if self.get(x + dx, y + dy, z + dz) != 0 {
                        return;
                    }
                }
            }
        }
        for dy in 0..4 {
            changes.insert((x, (y + dy) as u8, z), (log << 4) | log_meta);
        }
        for dy in 3..=5 {
            let radius: i32 = if dy == 5 { 1 } else { 2 };
            for dx in -radius..=radius {
                for dz in -radius..=radius {
                    if dx == 0 && dz == 0 && dy <= 3 {
                        continue;
                    }
                    if dx.abs() == 2 && dz.abs() == 2 {
                        continue;
                    }
                    let pos = (x + dx, (y + dy) as u8, z + dz);
                    if in_bounds(pos.0, i32::from(pos.1), pos.2) {
                        changes.insert(pos, (leaves << 4) | leaf_meta);
                    }
                }
            }
        }
    }
    /// Generate a bounded dark-oak crown only after every destination has
    /// passed validation. The four saplings are replaced together, so an
    /// obstructed canopy neither destroys blocks nor consumes bone meal.
    fn grow_dark_oak(&self, x: i32, y: i32, z: i32, changes: &mut BTreeMap<Position, u16>) {
        if y > 247 {
            return;
        }
        for ox in [-1, 0] {
            for oz in [-1, 0] {
                let (ax, az) = (x + ox, z + oz);
                if !(0..2).all(|dx| {
                    (0..2).all(|dz| {
                        in_bounds(ax + dx, y, az + dz)
                            && self.get(ax + dx, y, az + dz) >> 4 == 6
                            && self.get(ax + dx, y, az + dz) & 7 == 5
                            && plantable(self.get(ax + dx, y - 1, az + dz) >> 4)
                    })
                }) {
                    continue;
                }
                let mut proposed = BTreeMap::new();
                // Six-block high 2x2 trunk (log2, dark-oak metadata 1).
                for dy in 0..=5 {
                    for dx in 0..2 {
                        for dz in 0..2 {
                            proposed.insert((ax + dx, (y + dy) as u8, az + dz), (162 << 4) | 1);
                        }
                    }
                }
                // Crown is bounded to fit the special block transaction's
                // 128-block limit; higher rings taper without overwriting logs.
                for dy in 4..=7 {
                    let radius = if matches!(dy, 4 | 7) { 1 } else { 2 };
                    for dx in -radius..=1 + radius {
                        for dz in -radius..=1 + radius {
                            if dy <= 5 && (0..2).contains(&dx) && (0..2).contains(&dz) {
                                continue;
                            }
                            proposed.insert((ax + dx, (y + dy) as u8, az + dz), (161 << 4) | 1);
                        }
                    }
                }
                if proposed.len() > 128
                    || proposed.iter().any(|(&(px, py, pz), &block)| {
                        !in_bounds(px, i32::from(py), pz)
                            || (self.get(px, i32::from(py), pz) != 0
                                && !(py == y as u8
                                    && block >> 4 == 162
                                    && self.get(px, i32::from(py), pz) >> 4 == 6))
                    })
                {
                    continue;
                }
                changes.extend(proposed);
                return;
            }
        }
    }
    /// Changes from environmental random ticks and immediate support checks.
    /// This function only stages data, so the caller can commit or roll back
    /// the entire tick as one world-file transaction.
    pub(super) fn ecology_changes(&self, budget: usize) -> BTreeMap<Position, u16> {
        let mut changes = BTreeMap::new();
        for &(x, y, z) in self.pending_support.iter().take(budget) {
            let block = self.get(x, i32::from(y), z);
            let id = block >> 4;
            if matches!(
                id,
                6 | 31
                    | 32
                    | 37
                    | 38
                    | 50
                    | 55
                    | 59
                    | 81
                    | 83
                    | 104
                    | 105
                    | 115
                    | 127
                    | 66
                    | 70
                    | 72
                    | 131
                    | 132
                    | 147
                    | 148
                    | 151
                    | 75
                    | 76
                    | 93
                    | 94
                    | 141
                    | 142
                    | 64
                    | 71
                    | 77
                    | 143
                    | 96
                    | 149
                    | 150
            ) && !self.can_support(x, i32::from(y), z, block)
            {
                changes.insert((x, y, z), 0);
            }
        }
        // Leaves are sampled independently of the crop/ecology budget. They
        // do NOT disappear immediately on log removal, nor should flowers,
        // crops or a large leaf canopy starve the whole leaf decay queue.
        //
        // The stochastic test occurs once per simulated second and has the
        // 1.7.10 randomTickSpeed=3 average (60 / 4096 per second). The
        // world's deterministic tick counter keeps repeated reads stable.
        if !self.pending_ecology.is_empty() && self.fluid_phase.is_multiple_of(GAME_TICKS_PER_SECOND) {
            let len = self.pending_ecology.len();
            let offset = ((self.fluid_phase / GAME_TICKS_PER_SECOND) as usize)
                .wrapping_mul(budget.max(1))
                % len;
            for &(x, y, z) in self.pending_ecology.iter().cycle().skip(offset).take(len) {
                if changes.len() >= budget {
                    break;
                }
                if !leaf_decay_due(self.fluid_phase, x, i32::from(y), z) {
                    continue;
                }
                let block = self.get(x, i32::from(y), z);
                if matches!(block >> 4, 18 | 161)
                    && block & 4 == 0
                    && !self.leaf_connected(x, i32::from(y), z)
                {
                    changes.insert((x, y, z), 0);
                }
            }
        }
        if !self.fluid_phase.is_multiple_of(20) || self.pending_ecology.is_empty() {
            return changes;
        }
        let len = self.pending_ecology.len();
        let start = (self.fluid_phase as usize / 20).wrapping_mul(budget) % len;
        for &(x, y, z) in self
            .pending_ecology
            .iter()
            .cycle()
            .skip(start)
            .take(budget.min(len))
        {
            let block = self.get(x, i32::from(y), z);
            let id = block >> 4;
            let height = i32::from(y);
            if !self.can_support(x, height, z, block) {
                changes.insert((x, y, z), 0);
                continue;
            }
            match id {
                78 if self.light_at(x, height, z).0 >= 12 => {
                    changes.insert((x, y, z), 0);
                }
                79 if self.light_at(x, height, z).0 >= 12 => {
                    changes.insert((x, y, z), 9 << 4);
                }
                60 => {
                    let moisture = block & 7;
                    if self.wet_farmland(x, height, z) {
                        if moisture != 7 {
                            changes.insert((x, y, z), (60 << 4) | 7);
                        }
                    } else if moisture > 0 {
                        changes.insert((x, y, z), (60 << 4) | (moisture - 1));
                    } else if !matches!(self.get(x, height + 1, z) >> 4, 59 | 141 | 142) {
                        changes.insert((x, y, z), 3 << 4);
                    }
                }
                83 | 81 => {
                    // Keep a naturally grown column at vanilla's three-block limit.
                    let mut height_above_base = 1;
                    while height_above_base < 3
                        && self.get(x, height - height_above_base, z) >> 4 == id
                    {
                        height_above_base += 1;
                    }
                    if height_above_base < 3 && self.get(x, height + 1, z) == 0 {
                        if block & 15 == 15 {
                            changes.insert((x, y, z), id << 4);
                            if in_bounds(x, height + 1, z) {
                                changes.insert((x, (height + 1) as u8, z), id << 4);
                            }
                        } else if chance(self.fluid_phase, x, height, z, 2) {
                            changes.insert((x, y, z), block + 1);
                        }
                    }
                }
                115 if block & 3 < 3 => {
                    if chance(self.fluid_phase, x, height, z, 4) {
                        changes.insert((x, y, z), block + 1);
                    }
                }
                127 if (block >> 2) & 3 < 2 => {
                    if chance(self.fluid_phase, x, height, z, 4) {
                        changes.insert((x, y, z), block + 4);
                    }
                }
                104 | 105 => {
                    let age = block & 7;
                    if age < 7 {
                        if chance(self.fluid_phase, x, height, z, 2) {
                            changes.insert((x, y, z), block + 1);
                        }
                    } else {
                        let fruit = if id == 104 { 86 } else { 103 };
                        let neighbors = [(1, 0), (-1, 0), (0, 1), (0, -1)];
                        if !neighbors
                            .iter()
                            .any(|&(dx, dz)| self.get(x + dx, height, z + dz) >> 4 == fruit)
                        {
                            let index = (self.fluid_phase / 20) as usize % neighbors.len();
                            let (dx, dz) = neighbors[index];
                            let (nx, nz) = (x + dx, z + dz);
                            if self.get(nx, height, nz) == 0
                                && plantable(self.get(nx, height - 1, nz) >> 4)
                                && in_bounds(nx, height, nz)
                                && chance(self.fluid_phase, x, height, z, 2)
                            {
                                changes.insert((nx, y, nz), fruit << 4);
                            }
                        }
                    }
                }
                51 => {
                    // Rain or directly adjacent water extinguishes fire before
                    // any spread occurs. Reuse the scheduled ecology tick.
                    let neighboring_water = [
                        (x + 1, height, z),
                        (x - 1, height, z),
                        (x, height + 1, z),
                        (x, height - 1, z),
                        (x, height, z + 1),
                        (x, height, z - 1),
                    ]
                    .into_iter()
                    .any(|(nx, ny, nz)| {
                        in_bounds(nx, ny, nz) && matches!(self.get(nx, ny, nz) >> 4, 8 | 9)
                    });
                    if self.rain_reaches(x, height, z) || neighboring_water {
                        changes.insert((x, y, z), 0);
                    } else {
                        let age = block & 15;
                        let mut fuel = false;
                        for (dx, dy, dz) in [
                            (1, 0, 0),
                            (-1, 0, 0),
                            (0, 1, 0),
                            (0, -1, 0),
                            (0, 0, 1),
                            (0, 0, -1),
                        ] {
                            let nx = x + dx;
                            let ny = height + dy;
                            let nz = z + dz;
                            if !in_bounds(nx, ny, nz) {
                                continue;
                            }
                            if flammable(self.get(nx, ny, nz) >> 4) {
                                fuel = true;
                                if chance(self.fluid_phase, nx, ny, nz, 5) {
                                    changes.insert((nx, ny as u8, nz), 51 << 4);
                                }
                            }
                        }
                        if !fuel || age == 15 {
                            changes.insert((x, y, z), 0);
                        } else {
                            changes.insert((x, y, z), (51 << 4) | (age + 1));
                        }
                    }
                }
                59 | 141 | 142 if block & 7 < 7 => {
                    let damp = self.get(x, height - 1, z) & 7;
                    if self.get(x, height + 1, z) == 0
                        && chance(self.fluid_phase, x, height, z, if damp > 0 { 2 } else { 4 })
                    {
                        changes.insert((x, y, z), (id << 4) | ((block & 7) + 1));
                    }
                }
                6 if chance(self.fluid_phase, x, height, z, 4) => {
                    if block & 8 == 0 {
                        changes.insert((x, y, z), block | 8);
                    } else {
                        self.grow_tree(x, height, z, &mut changes);
                    }
                }
                // Leaves use the independent random-tick sampling above,
                // never the crop/flower 1-second cycle.
                _ => {}
            }
        }
        changes
    }
}

#[cfg(test)]
mod regression_tests {
    use super::*;

    #[test]
    fn leaf_decay_requires_a_contiguous_four_block_path_to_a_log() {
        let mut connected = World::new();
        for x in 0..4 {
            connected.import_block(x, 65, 0, 18 << 4).unwrap();
        }
        connected.import_block(4, 65, 0, 17 << 4).unwrap();
        assert!(connected.leaf_connected(0, 65, 0));

        let mut disconnected = World::new();
        disconnected.import_block(0, 65, 0, 18 << 4).unwrap();
        disconnected.import_block(2, 65, 0, 17 << 4).unwrap();
        assert!(!disconnected.leaf_connected(0, 65, 0));

        let mut too_far = World::new();
        for x in 0..5 {
            too_far.import_block(x, 65, 0, 18 << 4).unwrap();
        }
        too_far.import_block(5, 65, 0, 17 << 4).unwrap();
        assert!(!too_far.leaf_connected(0, 65, 0));
    }

    #[test]
    fn new_leaves_decay_and_both_leaf_species_share_log_paths() {
        let mut world = World::new();
        world.import_block(0, 65, 0, 161 << 4).unwrap();
        world.import_block(1, 65, 0, 18 << 4).unwrap();
        world.import_block(2, 65, 0, 162 << 4).unwrap();
        assert!(world.leaf_connected(0, 65, 0));
        assert!(world.leaf_connected(1, 65, 0));
        world.import_block(2, 65, 0, 0).unwrap();
        assert!(!world.leaf_connected(0, 65, 0));
        // No immediate disappearance, even though the log was removed.
        assert_eq!(world.ecology_changes(64).get(&(0, 65, 0)), None);
        for x in 0..=1 {
            let second = (1..=1024)
                .find(|&sec| leaf_decay_due(sec * 20, x, 65, 0))
                .expect("a pending leaf must eventually receive a random tick");
            world.fluid_phase = second * 20;
            assert_eq!(world.ecology_changes(64).get(&(x, 65, 0)), Some(&0));
        }
    }

    #[test]
    fn persistent_legacy_and_new_leaves_never_decay_without_logs() {
        let mut world = World::new();
        world.import_block(0, 65, 0, (18 << 4) | 4).unwrap();
        world.import_block(1, 65, 0, (161 << 4) | 5).unwrap();
        let changes = world.ecology_changes(64);
        assert!(!changes.contains_key(&(0, 65, 0)));
        assert!(!changes.contains_key(&(1, 65, 0)));
        assert!(!solid(161), "new leaves must not support solid attachments");
    }

    #[test]
    fn adjacent_water_extinguishes_fire_before_spread() {
        let mut world = World::new();
        world.import_block(0, 65, 0, 51 << 4).unwrap();
        world.import_block(1, 65, 0, 9 << 4).unwrap();
        world.import_block(-1, 65, 0, 17 << 4).unwrap();
        let changes = world.ecology_changes(64);
        assert_eq!(changes.get(&(0, 65, 0)), Some(&0));
        assert!(!changes.contains_key(&(-1, 65, 0)));
    }

    #[test]
    fn a_solid_roof_protects_fire_from_rain() {
        let mut world = World::new();
        world.import_block(0, 65, 0, 51 << 4).unwrap();
        world.import_block(0, 66, 0, 20 << 4).unwrap(); // glass roof blocks rain
        world.import_block(1, 65, 0, 17 << 4).unwrap(); // logs keep fire fueled
        world.set_raining(true);
        assert!(!world.rain_reaches(0, 65, 0));
        let changes = world.ecology_changes(64);
        assert_ne!(changes.get(&(0, 65, 0)), Some(&0));
        world.import_block(0, 66, 0, 0).unwrap();
        assert!(world.rain_reaches(0, 65, 0));
        assert_eq!(world.ecology_changes(64).get(&(0, 65, 0)), Some(&0));
    }

    #[test]
    fn wooden_and_planted_1710_blocks_are_combustible_but_stone_is_not() {
        for id in [
            5, 18, 31, 37, 54, 64, 85, 96, 106, 127, 134, 161, 163, 170, 171, 175,
        ] {
            assert!(flammable(id), "missing combustible block {id}");
        }
        for id in [0, 1, 8, 20, 49, 71] {
            assert!(!flammable(id), "non-combustible block {id} caught fire");
        }
        // TNT ignition requires a separate, safely persisted primed entity.
        assert!(!flammable(46));
    }

    #[test]
    fn dark_oak_requires_two_by_two_and_preserves_existing_blocks() {
        let mut w = World::new();
        for dx in 0..2 {
            for dz in 0..2 {
                w.import_block(dx, 64, dz, (6 << 4) | 5).unwrap();
                w.import_block(dx, 63, dz, 3 << 4).unwrap();
            }
        }
        let mut changes = BTreeMap::new();
        w.grow_tree(0, 64, 0, &mut changes);
        assert_eq!(changes.get(&(0, 64, 0)), Some(&((162 << 4) | 1)));
        assert_eq!(changes.get(&(1, 69, 1)), Some(&((162 << 4) | 1)));
        assert_eq!(changes.get(&(0, 71, 0)), Some(&((161 << 4) | 1)));
        assert!(changes.len() <= 128);

        w.import_block(0, 70, 0, 1 << 4).unwrap();
        let mut blocked = BTreeMap::new();
        w.grow_tree(0, 64, 0, &mut blocked);
        assert!(blocked.is_empty(), "a building must never be overwritten");

        w.import_block(0, 70, 0, 0).unwrap();
        w.import_block(1, 64, 1, 0).unwrap();
        w.grow_tree(0, 64, 0, &mut blocked);
        assert!(blocked.is_empty(), "a lone dark oak does not grow");
    }

    #[test]
    fn farmland_hydration_uses_water_within_four_horizontal_blocks() {
        let mut w = World::new();
        w.import_block(4, 64, 0, 8 << 4).unwrap();
        assert!(w.wet_farmland(0, 64, 0));
        assert!(!w.wet_farmland(-1, 64, 0));
    }
    #[test]
    fn leaf_random_ticks_match_vanilla_default_average() {
        // 3 attempts/4096 blocks/tick x 20 TPS gives 60/4096 per second.
        assert_eq!(LEAF_RANDOM_TICK_SPEED, 3);
        assert_eq!(SECTION_VOLUME, 4096);
        assert!(!leaf_decay_due(0, 0, 65, 0));
        assert!(!leaf_decay_due(19, 0, 65, 0));
        assert!(!leaf_decay_due(21, 0, 65, 0));
        let selected = (1..=4096)
            .filter(|&sec| leaf_decay_due(sec * 20, 0, 65, 0))
            .count();
        assert!(
            (30..=95).contains(&selected),
            "expected ~60 random leaf checks over 4096 seconds, got {selected}"
        );
    }

    #[test]
    fn connected_and_persistent_leaves_survive_even_when_selected() {
        let mut world = World::new();
        world.import_block(0, 65, 0, 18 << 4).unwrap();
        world.import_block(1, 65, 0, 17 << 4).unwrap();
        world.import_block(3, 65, 0, (161 << 4) | 4).unwrap();
        let sec = (1..=1024)
            .find(|&sec| leaf_decay_due(sec * 20, 0, 65, 0))
            .unwrap();
        world.fluid_phase = sec * 20;
        assert!(!world.ecology_changes(64).contains_key(&(0, 65, 0)));
        let sec = (1..=1024)
            .find(|&sec| leaf_decay_due(sec * 20, 3, 65, 0))
            .unwrap();
        world.fluid_phase = sec * 20;
        assert!(!world.ecology_changes(64).contains_key(&(3, 65, 0)));
    }

    #[test]
    fn many_unrelated_ecology_entries_do_not_prevent_leaf_decay() {
        let mut world = World::new();
        for x in 0..40 {
            world.import_block(x, 65, 2, 18 << 4).unwrap();
        }
        world.import_block(0, 65, 0, 161 << 4).unwrap();
        let sec = (1..=1024)
            .find(|&sec| leaf_decay_due(sec * 20, 0, 65, 0))
            .unwrap();
        world.fluid_phase = sec * 20;
        assert_eq!(world.ecology_changes(2).get(&(0, 65, 0)), Some(&0));
    }

}
