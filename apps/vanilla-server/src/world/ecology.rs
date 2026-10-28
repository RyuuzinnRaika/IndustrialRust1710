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

/// 1.7.10 saplings attempt a growth-stage update on only one in seven
/// randomly selected block ticks. At 20 TPS/randomTickSpeed=3 this
/// yields approximately 478 seconds per stage (two stages per tree).
fn sapling_growth_due(phase: u64, x: i32, y: i32, z: i32) -> bool {
    leaf_decay_due(phase, x, y, z)
        && ecology_roll(
            (phase / GAME_TICKS_PER_SECOND) ^ 0x5a91_1eaf,
            x,
            y,
            z,
        )
            .is_multiple_of(7)
}

/// Vines use legacy pre-flattening wall attachment bits:
/// south=1, west=2, north=4, east=8.
const VINE_FACES: [(u16, i32, i32); 4] =
    [(1, 0, 1), (2, -1, 0), (4, 0, -1), (8, 1, 0)];

fn vine_random_tick_due(phase: u64, x: i32, y: i32, z: i32) -> bool {
    leaf_decay_due(phase, x, y, z)
        && ecology_roll(phase / GAME_TICKS_PER_SECOND ^ 0x1060_1710, x, y, z)
            .is_multiple_of(4)
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
            106 => {
                self.supported_vine_mask(x, y, z, block & 15) != 0
                    || self.vine_has_ceiling(x, y, z)
            }
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
    /// The vine mask that still has a valid wall or a matching vine above.
    /// The ceiling itself is handled separately because Java 1.7.10 uses
    /// metadata zero for vines hanging only from their top face.
    fn supported_vine_mask(&self, x: i32, y: i32, z: i32, mask: u16) -> u16 {
        let above = self.get(x, y + 1, z);
        let inherited = if above >> 4 == 106 { above & 15 } else { 0 };
        let mut valid = 0;
        for (face, dx, dz) in VINE_FACES {
            if mask & face != 0
                && (solid(self.get(x + dx, y, z + dz) >> 4)
                    || inherited & face != 0)
            {
                valid |= face;
            }
        }
        valid
    }

    fn vine_has_ceiling(&self, x: i32, y: i32, z: i32) -> bool {
        solid(self.get(x, y + 1, z) >> 4)
    }

    /// One vanilla-style random vine tick. Respect the global 1/4 growth
    /// chance, four attachment bits, ceiling support and a local cap on
    /// upward/horizontal spreading. Downward hanging vines remain possible
    /// regardless of local density.
    fn grow_vine(&self, x: i32, y: i32, z: i32, block: u16, changes: &mut BTreeMap<Position, u16>) {
        let tick = self.fluid_phase / GAME_TICKS_PER_SECOND;
        let roll = ecology_roll(tick ^ 0x7669_6e65, x, y, z);
        let facing = block & 15;
        let dir = roll % 6;
        if dir == 0 && y > 0 {
            let below = self.get(x, y - 1, z);
            let mut candidate = 0u16;
            for (bit, _, _) in VINE_FACES {
                if facing & bit != 0
                    && ecology_roll(tick ^ (0x6000 + u64::from(bit)), x, y, z) & 1 == 0
                {
                    candidate |= bit;
                }
            }
            if below == 0 && candidate != 0 {
                let supported = self.supported_vine_mask(x, y - 1, z, candidate);
                if supported != 0 || self.vine_has_ceiling(x, y - 1, z) {
                    changes.insert((x, (y - 1) as u8, z), (106 << 4) | supported);
                }
            } else if below >> 4 == 106 && candidate != 0 {
                let extended = self.supported_vine_mask(x, y - 1, z, (below & 15) | candidate);
                if extended != below & 15 {
                    changes.insert((x, (y - 1) as u8, z), (106 << 4) | extended);
                }
            }
            return;
        }
        if y >= 255 || dir == 0 {
            return;
        }
        // Java's vine spread is limited in dense patches. A bounded local
        // count prevents infinite vines from flooding one loaded chunk.
        let mut nearby = 0;
        for dx in -4..=4 {
            for dz in -4..=4 {
                for dy in -1..=1 {
                    if self.get(x + dx, y + dy, z + dz) >> 4 == 106 {
                        nearby += 1;
                        if nearby >= 5 {
                            return;
                        }
                    }
                }
            }
        }
        let (tx, ty, tz) = match dir {
            1 => (x, y + 1, z),
            2 => (x - 1, y, z),
            3 => (x + 1, y, z),
            4 => (x, y, z - 1),
            _ => (x, y, z + 1),
        };
        if !in_bounds(tx, ty, tz) || self.get(tx, ty, tz) != 0 {
            return;
        }
        // A newly spread vine must immediately have a real attachment;
        // copying bits without a wall would create unsupported blocks.
        let supported = self.supported_vine_mask(tx, ty, tz, facing);
        if supported != 0 {
            changes.insert((tx, ty as u8, tz), (106 << 4) | supported);
        }
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
    /// A grown tree uses the SAME shapes and 1.7.10 block metadata as natural
    /// terrain trees. Stage all changes first, and reject a blocked crown or
    /// incomplete 2x2 set without removing any saplings or nearby structures.
    /// The caller commits the map atomically (random tick or bone meal).
    pub(super) fn grow_tree(&self, x: i32, y: i32, z: i32, changes: &mut BTreeMap<Position, u16>) {
        if !(1..=253).contains(&y) || !in_bounds(x, y, z) {
            return;
        }
        let sapling = self.get(x, y, z);
        if sapling >> 4 != 6 {
            return;
        }
        let species = sapling & 7;
        if species > 5 || !plantable(self.get(x, y - 1, z) >> 4) {
            return;
        }

        // In Java 1.7.10, only spruce/jungle/dark oak support 2x2 growth.
        // Dark oak REQUIRES the square; the other two use the giant variant
        // whenever the full 2x2 is present, even if its canopy is obstructed.
        let mut origin = (x, z);
        let mut large = false;
        if matches!(species, 1 | 3 | 5) {
            'squares: for dx in [-1, 0] {
                for dz in [-1, 0] {
                    let (ax, az) = (x + dx, z + dz);
                    if (0..=1).all(|ox| {
                        (0..=1).all(|oz| {
                            let px = ax + ox;
                            let pz = az + oz;
                            in_bounds(px, y, pz)
                                && self.get(px, y, pz) >> 4 == 6
                                && self.get(px, y, pz) & 7 == species
                                && plantable(self.get(px, y - 1, pz) >> 4)
                        })
                    }) {
                        origin = (ax, az);
                        large = true;
                        break 'squares;
                    }
                }
            }
        }
        if species == 5 && !large {
            return;
        }

        let seed = match self.generation {
            super::Generation::Flat => 0,
            super::Generation::Natural(seed)
            | super::Generation::Biomes(seed)
            | super::Generation::Expanded(seed, _) => seed,
        };
        let blocks = crate::worldgen::sapling_tree_blocks(
            seed ^ self.fluid_phase,
            origin.0,
            y,
            origin.1,
            species,
            large,
        );
        if blocks.is_empty() || blocks.len() > 2048 {
            return;
        }

        let is_sapling_root = |px: i32, pz: i32| {
            if large {
                (origin.0..=origin.0 + 1).contains(&px)
                    && (origin.1..=origin.1 + 1).contains(&pz)
            } else {
                px == x && pz == z
            }
        };
        let mut proposed = BTreeMap::new();
        for (px, py, pz, block) in blocks {
            if !in_bounds(px, py, pz) {
                return;
            }
            let existing = self.get(px, py, pz);
            let id = existing >> 4;
            let root_sapling = py == y
                && is_sapling_root(px, pz)
                && id == 6
                && existing & 7 == species;
            // Vanilla trees can grow through ordinary natural foliage.
            // Never erase logs, another sapling, buildings, or deliberately
            // placed/permanent leaves (metadata 0x4).
            let replaceable = matches!(id, 0 | 31 | 32 | 37 | 38 | 78 | 106)
                || (matches!(id, 18 | 161) && existing & 4 == 0);
            if !root_sapling && !replaceable {
                return;
            }
            proposed.insert((px, py as u8, pz), block);
        }
        // Every sapling in a complete square must become a trunk. Reject
        // pathological shapes instead of destroying three of four saplings.
        if large && (0..=1).any(|dx| {
            (0..=1).any(|dz| {
                !proposed.contains_key(&(origin.0 + dx, y as u8, origin.1 + dz))
            })
        }) {
            return;
        }
        // Growing saplings turn grass (and cultivated farmland) directly
        // beneath their trunk roots into dirt in the same atomic transition.
        let side = if large { 2 } else { 1 };
        for dx in 0..side {
            for dz in 0..side {
                let px = origin.0 + dx;
                let pz = origin.1 + dz;
                if matches!(self.get(px, y - 1, pz) >> 4, 2 | 60) {
                    proposed.insert((px, (y - 1) as u8, pz), 3 << 4);
                }
            }
        }
        changes.extend(proposed);
    }
    /// An IRW snapshot records removed *generated* logs as sparse overrides,
    /// but does not serialize the in-memory random-tick wake set. Rebuild it
    /// for nearby generated leaves on restart so chopped trees can finish
    /// decaying, even if the server was stopped right after the last log broke.
    pub(super) fn restore_generated_leaf_checks(&mut self) {
        if !matches!(
            self.generation,
            super::Generation::Expanded(_, crate::dimensions::Dimension::Overworld)
        ) {
            return;
        }
        let removed_logs: Vec<(i32, i32, i32)> = self
            .edits
            .iter()
            .filter_map(|(&(x, y, z), &replacement)| {
                if !(63..=160).contains(&y) || matches!(replacement >> 4, 17 | 162) {
                    return None;
                }
                matches!(self.base_block(x, y, z) >> 4, 17 | 162)
                    .then_some((x, i32::from(y), z))
            })
            .collect();
        for (x, y, z) in removed_logs {
            self.wake_leaves(x, y, z);
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
            if id == 106 {
                let height = i32::from(y);
                let supported = self.supported_vine_mask(x, height, z, block & 15);
                if supported == 0 && !self.vine_has_ceiling(x, height, z) {
                    changes.insert((x, y, z), 0);
                } else if supported != block & 15 {
                    changes.insert((x, y, z), (106 << 4) | supported);
                }
                continue;
            }
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
        if !self.pending_ecology.is_empty()
            && self.fluid_phase.is_multiple_of(GAME_TICKS_PER_SECOND)
            // Ground-item storage has a bounded 512-entity registry.
            // When it fills, keep the surviving leaves in the world
            // rather than spawning an unbounded unsaved item backlog.
            && self.pending_drops.len() < super::LEAF_DROP_BACKPRESSURE
        {
            let len = self.pending_ecology.len();
            let offset = ((self.fluid_phase / GAME_TICKS_PER_SECOND) as usize)
                .wrapping_mul(budget.max(1))
                % len;
            for &(x, y, z) in self
                .pending_ecology
                .iter()
                .cycle()
                .skip(offset)
                .take(len)
            {
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
            // Never grow or spread a plant which was already scheduled
            // to detach or lose its attachment metadata in this tick.
            // Otherwise an unsupported vine could produce a new dangling
            // vine below it in the same atomic snapshot.
            if changes.contains_key(&(x, y, z)) {
                continue;
            }
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
                6 if sapling_growth_due(self.fluid_phase, x, height, z) => {
                    // Vanilla saplings advance naturally only when the
                    // light above them is at least 9. Bone meal directly
                    // calls grow_tree and remains usable under dim light.
                    let (block_light, sky_light) = self.light_at(x, height + 1, z);
                    if block_light.max(sky_light) >= 9 {
                        if block & 8 == 0 {
                            changes.insert((x, y, z), block | 8);
                        } else {
                            self.grow_tree(x, height, z, &mut changes);
                        }
                    }
                }
                106 if vine_random_tick_due(self.fluid_phase, x, height, z) => {
                    self.grow_vine(x, height, z, block, &mut changes);
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
    fn vines_drop_unsupported_faces_and_detach_when_last_wall_breaks() {
        let mut world = World::new();
        world.import_block(1, 65, 0, 17 << 4).unwrap();
        world.import_block(0, 65, 0, (106 << 4) | 8 | 1).unwrap();
        assert!(world.can_support(0, 65, 0, (106 << 4) | 8));
        assert_eq!(
            world.ecology_changes(64).get(&(0, 65, 0)),
            Some(&((106 << 4) | 8)),
            "south attachment should clear but east-facing wall survives"
        );
        world.import_block(1, 65, 0, 0).unwrap();
        assert!(!world.can_support(0, 65, 0, (106 << 4) | 8));
        assert_eq!(world.ecology_changes(64).get(&(0, 65, 0)), Some(&0));
    }

    #[test]
    fn lower_vines_survive_when_they_hang_from_an_attached_vine_above() {
        let mut world = World::new();
        world.import_block(1, 66, 0, 17 << 4).unwrap();
        world.import_block(0, 66, 0, (106 << 4) | 8).unwrap();
        world.import_block(0, 65, 0, (106 << 4) | 8).unwrap();
        assert!(
            !world.ecology_changes(64).contains_key(&(0, 65, 0)),
            "the matching upper vine should keep the hanging vine attached"
        );
        world.import_block(0, 66, 0, 0).unwrap();
        assert_eq!(world.ecology_changes(64).get(&(0, 65, 0)), Some(&0));
    }

    #[test]
    fn vines_grow_downward_only_when_selected_by_random_ticks() {
        let mut world = World::new();
        world.import_block(1, 65, 0, 17 << 4).unwrap();
        world.import_block(0, 65, 0, (106 << 4) | 8).unwrap();
        assert!(world.pending_ecology.contains(&(0, 65, 0)));
        assert!(world.ecology_changes(64).get(&(0, 64, 0)).is_none());
        let second = (1..=20_000u64)
            .find(|&second| {
                let tick = second * GAME_TICKS_PER_SECOND;
                vine_random_tick_due(tick, 0, 65, 0)
                    && ecology_roll(second ^ 0x7669_6e65, 0, 65, 0) % 6 == 0
                    && ecology_roll(second ^ (0x6000 + 8), 0, 65, 0) & 1 == 0
            })
            .expect("the vine must eventually receive a downward growth tick");
        world.fluid_phase = second * GAME_TICKS_PER_SECOND;
        assert_eq!(
            world.ecology_changes(64).get(&(0, 64, 0)),
            Some(&((106 << 4) | 8)),
            "a hanging vine must inherit its supported wall orientation"
        );
    }

    #[test]
    fn saved_vines_rebuild_both_support_and_ecology_queues() {
        let path = std::env::temp_dir().join(format!(
            "ir1710-vines-reload-{}.irw",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut world = World::new();
        world.import_block(0, 65, 0, (106 << 4) | 8).unwrap();
        world.save(&path).unwrap();
        let loaded = World::load(&path).unwrap();
        assert!(loaded.pending_support.contains(&(0, 65, 0)));
        assert!(loaded.pending_ecology.contains(&(0, 65, 0)));
        assert_eq!(loaded.ecology_changes(64).get(&(0, 65, 0)), Some(&0));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn unsupported_vines_do_not_spread_during_the_detachment_tick() {
        let mut world = World::new();
        world.import_block(0, 65, 0, (106 << 4) | 8).unwrap();
        // Without a backing wall or attached vine above, its support
        // check stages removal. Even on a lucky growth tick, it must
        // not create a fresh vine underneath.
        let lucky = (1..=20_000u64)
            .find(|&second| {
                let phase = second * GAME_TICKS_PER_SECOND;
                vine_random_tick_due(phase, 0, 65, 0)
                    && ecology_roll(second ^ 0x7669_6e65, 0, 65, 0) % 6 == 0
                    && ecology_roll(second ^ (0x6000 + 8), 0, 65, 0) & 1 == 0
            })
            .unwrap();
        world.fluid_phase = lucky * GAME_TICKS_PER_SECOND;
        let staged = world.ecology_changes(64);
        assert_eq!(staged.get(&(0, 65, 0)), Some(&0));
        assert!(!staged.contains_key(&(0, 64, 0)));
    }

    #[test]
    fn saturated_drop_backlog_defers_leaf_decay_without_destroying_the_leaf() {
        let mut world = World::new();
        world.import_block(0, 65, 0, 18 << 4).unwrap();
        world.pending_drops.resize(
            super::super::LEAF_DROP_BACKPRESSURE,
            super::super::WorldDrop {
                pos: (10, 80, 10),
                stack: crate::inventory::Stack {
                    id: 6,
                    count: 1,
                    damage: 0,
                    nbt: None,
                },
            },
        );
        let second = (1..=10_000u64)
            .find(|&sec| leaf_decay_due(sec * GAME_TICKS_PER_SECOND, 0, 65, 0))
            .unwrap();
        world.fluid_phase = second * GAME_TICKS_PER_SECOND;
        assert!(
            !world.ecology_changes(64).contains_key(&(0, 65, 0)),
            "natural leaf decay must pause while unpersisted drops accumulate"
        );
        world.pending_drops.clear();
        assert_eq!(world.ecology_changes(64).get(&(0, 65, 0)), Some(&0));
    }

    #[test]
    fn naturally_decaying_leaves_persist_drops_after_the_commit() {
        let path = std::env::temp_dir().join(format!(
            "ir1710-leaf-drop-{}.irw",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let leaf = 18 << 4;
        let pos = (0, 65, 0);
        let second = (1..=50_000u64)
            .find(|&sec| {
                leaf_decay_due(sec * GAME_TICKS_PER_SECOND, pos.0, pos.1, pos.2)
                    && crate::survival::leaf_decay_drops(
                        leaf,
                        pos,
                        sec * GAME_TICKS_PER_SECOND,
                    )
                    .iter()
                    .any(|drop| drop.id == 6)
            })
            .expect("a decaying oak leaf should eventually drop a sapling");
        let mut world = World::new();
        world.import_block(pos.0, pos.1, pos.2, leaf).unwrap();
        world.fluid_phase = second * GAME_TICKS_PER_SECOND - 1;
        let changed = world.block_tick(&path, 64).unwrap();
        assert!(changed.contains(&pos));
        assert_eq!(world.get(pos.0, pos.1, pos.2), 0);
        assert!(world.drain_drops().iter().any(|drop| {
            drop.pos == pos && drop.stack.id == 6 && drop.stack.damage == 0
        }));
        assert_eq!(World::load(&path).unwrap().get(pos.0, pos.1, pos.2), 0);
        std::fs::remove_file(path).unwrap();
    }

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
        assert!(changes.values().any(|&block| block >> 4 == 161));
        assert!(changes.len() <= 2048);

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
    fn sapling_growth_uses_one_in_seven_random_ticks_not_every_four_seconds() {
        assert!(!sapling_growth_due(0, 0, 64, 0));
        assert!(!sapling_growth_due(19, 0, 64, 0));
        assert!(!sapling_growth_due(21, 0, 64, 0));
        let mut checks = 0;
        let mut growths = 0;
        for second in 1..=50_000 {
            let phase = second * 20;
            if leaf_decay_due(phase, 0, 64, 0) {
                checks += 1;
                if sapling_growth_due(phase, 0, 64, 0) {
                    growths += 1;
                }
            }
        }
        assert!((600..=850).contains(&checks), "random ticks={checks}");
        assert!((60..=145).contains(&growths), "growth attempts={growths}");
        assert!(growths < checks / 4, "saplings must not grow every fourth second");
    }

    #[test]
    fn six_sapling_species_grow_matching_logs_and_leaves() {
        for (species, expected_log, expected_leaf, expected_meta) in [
            (0u16, 17, 18, 0),
            (1, 17, 18, 1),
            (2, 17, 18, 2),
            (3, 17, 18, 3),
            (4, 162, 161, 0),
        ] {
            let mut world = World::new();
            world
                .import_block(0, 64, 0, (6 << 4) | species)
                .unwrap();
            let mut changes = BTreeMap::new();
            world.grow_tree(0, 64, 0, &mut changes);
            assert!(
                changes.values().any(|&b| b >> 4 == expected_log && b & 3 == expected_meta),
                "sapling {species} must generate its own logs"
            );
            assert!(
                changes.values().any(|&b| b >> 4 == expected_leaf && b & 3 == expected_meta),
                "sapling {species} must generate its own leaves"
            );
            assert_eq!(changes.get(&(0, 64, 0)).unwrap() >> 4, expected_log);
            assert!(changes.len() < 2048);
        }
    }

    #[test]
    fn tree_growth_turns_grass_to_dirt_and_replaces_only_natural_foliage() {
        let mut world = World::new();
        world.import_block(0, 63, 0, 2 << 4).unwrap();
        world.import_block(0, 64, 0, 6 << 4).unwrap();
        world.import_block(0, 65, 0, 31 << 4).unwrap();
        let mut grown = BTreeMap::new();
        world.grow_tree(0, 64, 0, &mut grown);
        assert_eq!(grown.get(&(0, 63, 0)), Some(&(3 << 4)));
        assert_eq!(grown.get(&(0, 64, 0)).map(|b| b >> 4), Some(17));
        assert_eq!(grown.get(&(0, 65, 0)).map(|b| b >> 4), Some(17));
        // A player-placed (persistent) leaf is part of a build and must
        // not be overwritten by a later tree growth attempt.
        world.import_block(0, 65, 0, (18 << 4) | 4).unwrap();
        grown.clear();
        world.grow_tree(0, 64, 0, &mut grown);
        assert!(grown.is_empty());
    }

    #[test]
    fn spruce_and_jungle_two_by_two_grow_giant_trunks() {
        for species in [1u16, 3] {
            let mut world = World::new();
            for dx in 0..2 {
                for dz in 0..2 {
                    world
                        .import_block(dx, 64, dz, (6 << 4) | species | 8)
                        .unwrap();
                }
            }
            // Call on the south-east sapling; growth must still use the
            // north-west root and replace all four saplings atomically.
            let mut changes = BTreeMap::new();
            world.grow_tree(1, 64, 1, &mut changes);
            for dx in 0..2 {
                for dz in 0..2 {
                    assert_eq!(
                        changes.get(&(dx, 70, dz)).map(|state| state >> 4),
                        Some(17),
                        "2x2 species {species} must have a tall thick trunk"
                    );
                    assert_eq!(
                        changes.get(&(dx, 64, dz)).map(|state| state & 3),
                        Some(species),
                        "all four matching saplings are consumed"
                    );
                }
            }
            assert!(changes.values().any(|state| state >> 4 == 18));
            assert!(changes.len() <= 2048);
        }
    }

    #[test]
    fn big_sapling_trees_do_not_overwrite_builds_or_consume_saplings() {
        let mut world = World::new();
        for dx in 0..2 {
            for dz in 0..2 {
                world.import_block(dx, 64, dz, (6 << 4) | 1).unwrap();
            }
        }
        world.import_block(0, 70, 0, 1 << 4).unwrap();
        let mut blocked = BTreeMap::new();
        world.grow_tree(1, 64, 1, &mut blocked);
        assert!(blocked.is_empty(), "giant spruce cannot overwrite stone");
        world.import_block(0, 70, 0, 0).unwrap();
        world.import_block(1, 64, 1, (6 << 4) | 3).unwrap();
        let mut unpaired = BTreeMap::new();
        world.grow_tree(0, 64, 0, &mut unpaired);
        // Without a complete 2x2 spruce set, the remaining tree is 1x1.
        assert_eq!(unpaired.get(&(0, 64, 0)).map(|b| b >> 4), Some(17));
        assert!(unpaired.len() <= 2048);
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
    fn batched_trunk_cut_schedules_natural_leaves_until_the_random_tick() {
        let path = std::env::temp_dir().join(format!(
            "ir1710-leaf-batch-{}.irw",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut world = World::new();
        world.import_block(0, 65, 0, 17 << 4).unwrap();
        world.import_block(1, 65, 0, 18 << 4).unwrap();
        assert!(world
            .update_many_and_save(&path, &[(0, 65, 0, 0)])
            .unwrap());
        assert!(world.pending_ecology.contains(&(1, 65, 0)));
        assert_eq!(world.get(1, 65, 0), 18 << 4);
        // Move to the game tick immediately before this leaf's first
        // deterministic random sample; no forced deletion on cutting.
        let first_sec = (1..=1024)
            .find(|&sec| leaf_decay_due(sec * 20, 1, 65, 0))
            .unwrap();
        world.fluid_phase = first_sec * 20 - 1;
        let changed = world.block_tick(&path, 64).unwrap();
        assert!(changed.contains(&(1, 65, 0)));
        assert_eq!(world.get(1, 65, 0), 0);
        assert_eq!(World::load(&path).unwrap().get(1, 65, 0), 0);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn removing_a_leaf_wakes_neighbors_after_the_save() {
        let path = std::env::temp_dir().join(format!(
            "ir1710-leaf-neighbor-{}.irw",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut world = World::new();
        for x in 0..3 {
            world.import_block(x, 65, 0, 18 << 4).unwrap();
        }
        world.pending_ecology.clear();
        assert!(world.update_and_save(&path, 1, 65, 0, 0).unwrap());
        assert!(world.pending_ecology.contains(&(0, 65, 0)));
        assert!(world.pending_ecology.contains(&(2, 65, 0)));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn failed_atomic_batch_does_not_publish_new_leaf_checks() {
        let path = std::env::temp_dir().join(format!(
            "ir1710-leaf-rollback-{}.irw",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut world = World::new();
        world.import_block(0, 65, 0, 17 << 4).unwrap();
        world.import_block(1, 65, 0, 18 << 4).unwrap();
        world.pending_ecology.clear();
        assert!(world
            .update_many_and_save(&path, &[(0, 65, 0, 0), (0, 65, 0, 0)])
            .is_err());
        assert_eq!(world.get(0, 65, 0), 17 << 4);
        assert!(world.pending_ecology.is_empty());
        assert!(!path.exists());
    }

    #[test]
    fn many_unrelated_ecology_entries_do_not_prevent_leaf_decay() {
        let mut world = World::new();
        for x in 0..40 {
            // These permanent leaves fill the ecology set but can never
            // consume the live-leaf decay budget.
            world.import_block(x, 65, 2, (18 << 4) | 4).unwrap();
        }
        world.import_block(0, 65, 0, 161 << 4).unwrap();
        let sec = (1..=1024)
            .find(|&sec| leaf_decay_due(sec * 20, 0, 65, 0))
            .unwrap();
        world.fluid_phase = sec * 20;
        assert_eq!(world.ecology_changes(1).get(&(0, 65, 0)), Some(&0));
    }

}
