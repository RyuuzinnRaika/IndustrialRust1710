//! Bounded, sparse, deterministic flat world streamed by chunk.
//! IRW1 is temporary storage; this is NOT Anvil/NBT/Forge world storage.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
mod devices;
mod ecology;
mod lighting;
mod special;
pub use special::TntEvent;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Temporary world edge (blocks). Prevents unbounded player edits or packet positions.
/// Coordinates x/z must be in [-WORLD_LIMIT, WORLD_LIMIT).
pub const WORLD_LIMIT: i32 = 16_384;
pub const VIEW_DISTANCE: i32 = 2; // Send a 5x5 chunk window per connected player.
pub const MIN_CHUNK: i32 = -WORLD_LIMIT / 16;
pub const MAX_CHUNK: i32 = WORLD_LIMIT / 16 - 1;
const MAX_EDITS: usize = 65_536;
// A loaded 5x5 view contains at most 6_400 surface columns per player.
// Keep weather observations bounded even after prolonged exploration.
const MAX_WEATHER_COLUMNS: usize = 16_384;
const MAGIC: &[u8; 4] = b"IRW1";
const NATURAL_MAGIC: &[u8; 4] = b"IRW2";
const BIOME_MAGIC: &[u8; 4] = b"IRW3";
const EXPANDED_MAGIC: &[u8; 4] = b"IRW4";
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Generation {
    #[default]
    Flat,
    Natural(u64),
    Biomes(u64),
    Expanded(u64, crate::dimensions::Dimension),
}
type Position = (i32, u8, i32);
#[derive(Default, Clone, Debug)]
pub struct World {
    edits: BTreeMap<Position, u16>,
    generation: Generation,
    // In-memory wake-up queue only. Reconstructed from placed blocks on load.
    pending_gravity: BTreeSet<Position>,
    pending_fluid: BTreeSet<Position>,
    pending_circuit: BTreeSet<Position>,
    pending_support: BTreeSet<Position>,
    pending_ecology: BTreeSet<Position>,
    // Weather surfaces are scheduled separately to avoid disturbing crop ticks.
    pending_weather: BTreeSet<Position>,
    // FIFO eviction prevents long-running servers from retaining every
    // cold column a player has ever loaded; does not create a new scheduler.
    pending_weather_order: VecDeque<Position>,
    pending_devices: BTreeSet<Position>,
    pending_repeaters: BTreeMap<Position, (u64, bool)>,
    // Comparator analog output (0..15) is not encoded in block metadata.
    // Reconstructed by queued evaluation after loading IRW snapshots.
    comparator_outputs: BTreeMap<Position, u8>,
    // Live committed device inventories; never serialize analog cache into IRW.
    device_comparator_inputs: BTreeMap<Position, u8>,
    pending_comparators: BTreeMap<Position, (u64, u8)>,
    comparator_ready: BTreeMap<Position, u8>,
    pending_buttons: BTreeMap<Position, u64>,
    pending_drops: Vec<WorldDrop>,
    // Fired devices are published only after the block snapshot is durable.
    pending_device_activations: Vec<(i32, i32, i32)>,
    // Protocol 5 piston animation is published only after the world save.
    pending_piston_actions: Vec<PistonAction>,
    // Short-lived conservative collision of blocks swept during a push/pull.
    piston_sweep_ghosts: BTreeMap<Position, u64>,
    // Sparse pending client-facing lighting invalidations, not part of IRW files.
    pending_light_chunks: BTreeSet<(i32, i32)>,
    // Monotonic simulation phase; no catch-up while the server is stopped.
    fluid_phase: u64,
    // Set from Clock; the weather simulation has one authoritative owner.
    raining: bool,
    day_time: i64,
    primed_tnt: BTreeMap<i32, special::PrimedTnt>,
    next_tnt_id: i32,
    tnt_events: Vec<special::TntEvent>,
    // Only live worlds bind to a session; standalone conversions and tests
    // retain their portable save behavior, while clones share one OS lock.
    save_session: Option<(PathBuf, Arc<Mutex<crate::session_lock::WorldSessionLock>>)>,
}

/// A persisted world mutation can create a temporary ground-item event.
/// World state is written first, then the 20TPS worker saves item entities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorldDrop {
    pub pos: (i32, i32, i32),
    pub stack: crate::inventory::Stack,
}

/// A committed piston base transition for Protocol 5 block-action packets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PistonAction {
    pub pos: (i32, i32, i32),
    pub block_id: u16,
    pub facing: u8,
    pub extending: bool,
}

/// A gravity block moved exactly one vertical cell during one 20Hz world tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GravityMove {
    pub from: (i32, i32, i32),
    pub to: (i32, i32, i32),
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
pub fn in_bounds(x: i32, y: i32, z: i32) -> bool {
    (-WORLD_LIMIT..WORLD_LIMIT).contains(&x)
        && (0..=255).contains(&y)
        && (-WORLD_LIMIT..WORLD_LIMIT).contains(&z)
}
pub fn is_placeable(block_id: u16) -> bool {
    // Ordinary renderable 1.7.10 blocks only; limited sand/gravel gravity.
    matches!(
        block_id,
        1 | 2
            | 3
            | 8 // flowing water: simplified bounded propagation
            | 10 // flowing lava
            | 6 // saplings
            | 31 | 32 | 37 | 38 // tall grass, dead bush and flowers
            | 18 // leaves
            | 59 // wheat
            | 60 // farmland (via hoe)
            | 27 | 28 | 66 | 157 // rails (powered/detector/normal/activator)
            | 81 | 83 // cactus and sugar cane
            | 104 | 105 // pumpkin/melon stems
            | 115 // Nether wart
            | 127 // cocoa pods
            | 107 // fence gate
            | 44 | 126 // half slabs
            | 53 | 67 | 108 | 109 | 114 | 128 | 134 | 135 | 136 | 156 | 163 | 164 // stairs
            | 149 | 150 // comparator
            | 131 | 132 // tripwire and hook
            | 151 // daylight detector
            | 118 // cauldron
            | 138 // beacon
            | 120 // end portal frame
            | 46 // TNT
            | 78 | 79 // snow layer and ice
            | 70 | 72 | 147 | 148 // pressure plates
            | 77 | 143 // buttons
            | 23 | 158 // dispenser/dropper
            | 29 | 33 // pistons
            | 64 | 71 // doors
            | 96 // trapdoors
            | 93 | 94 // repeater
            | 141 | 142 // carrot/potato
            | 50 // wall/ground torch: simplified attachment
            | 55 // redstone dust (client item 331)
            | 69 // lever
            | 76 // powered redstone torch
            | 4
            | 5
            | 7
            | 12
            | 13
            | 14 // gold ore
            | 15 // iron ore
            | 16 // coal ore
            | 17
            | 20
            | 21 // lapis ore
            | 24
            | 35
            | 41
            | 42
            | 45
            | 49
            | 26 // two-block bed
            | 25 // note block
            | 63 | 68 // standing/wall signs
            | 84 // jukebox
            | 92 // cake
            | 140 // flower pot
            | 54 | 146 // normal/trapped chest (single or double)
            | 117 // brewing stand
            | 154 // hopper
            | 56 // diamond ore
            | 57
            | 47 // bookshelf: used by the enchanting table
            | 116 // enchanting table
            | 145 // anvil
            | 58 // crafting table: opens workbench window when used
            | 73 // redstone ore
            | 80
            | 82
            | 87
            | 89
            | 98
            | 129 // emerald ore
            | 155
            | 159
            | 162 // acacia / dark oak logs
            | 168
            | 169
            | 170
            | 172
            | 173
    )
}
impl World {
    /// Bind this live world's IRW saves to the startup session.lock. Any
    /// subsequent clone (e.g. an atomic two-block edit) keeps this binding.
    pub fn bind_save_session(
        &mut self,
        world_path: &Path,
        session: Arc<Mutex<crate::session_lock::WorldSessionLock>>,
    ) -> io::Result<()> {
        let mut lock = session
            .lock()
            .map_err(|_| invalid("session lock mutex poisoned"))?;
        lock.verify()?;
        drop(lock);
        self.save_session = Some((world_path.to_path_buf(), session));
        Ok(())
    }

    /// Verify the live world still owns session.lock before a journal can
    /// publish serialized IRW bytes. Full cross-file atomicity is FS12.
    fn verify_save_session(&self, path: Option<&Path>) -> io::Result<()> {
        if let Some((world_path, session)) = &self.save_session {
            if path.is_some_and(|path| path != world_path) {
                return Err(invalid("world save path differs from session-locked world"));
            }
            session
                .lock()
                .map_err(|_| invalid("session lock mutex poisoned"))?
                .verify()?;
            if world_path.with_extension("pending-txn.json").exists() {
                return Err(invalid("pending world redo requires recovery before save"));
            }
        }
        Ok(())
    }

    pub fn set_raining(&mut self, raining: bool) {
        self.raining = raining;
    }
    pub fn set_day_time(&mut self, value: i64) {
        self.day_time = value.rem_euclid(24_000);
    }
    /// Tracks which chunk columns contain changes, without allocating a full world.
    pub fn drain_drops(&mut self) -> Vec<WorldDrop> {
        std::mem::take(&mut self.pending_drops)
    }

    pub fn drain_device_activations(&mut self) -> Vec<(i32, i32, i32)> {
        std::mem::take(&mut self.pending_device_activations)
    }

    pub fn drain_piston_actions(&mut self) -> Vec<PistonAction> {
        std::mem::take(&mut self.pending_piston_actions)
    }

    pub fn piston_sweep_at(&self, x: i32, y: i32, z: i32) -> bool {
        in_bounds(x, y, z)
            && self
                .piston_sweep_ghosts
                .get(&(x, y as u8, z))
                .is_some_and(|expiry| *expiry > self.fluid_phase)
    }

    /// Bounded per-worker budget; remaining light updates survive the next tick.
    pub fn drain_light_chunks(&mut self, budget: usize) -> Vec<(i32, i32)> {
        let chunks: Vec<_> = self
            .pending_light_chunks
            .iter()
            .take(budget)
            .copied()
            .collect();
        for chunk in &chunks {
            self.pending_light_chunks.remove(chunk);
        }
        chunks
    }

    /// Only refresh when opacity or emission changes, not merely metadata.
    /// The 15-block Manhattan halo includes chunk boundaries and corners.
    fn mark_lighting_changed(&mut self, x: i32, z: i32, old: u16, new: u16) {
        if lighting::lighting_signature(old) == lighting::lighting_signature(new) {
            return;
        }
        let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
        for chunk_x in cx - 1..=cx + 1 {
            for chunk_z in cz - 1..=cz + 1 {
                if !(MIN_CHUNK..=MAX_CHUNK).contains(&chunk_x)
                    || !(MIN_CHUNK..=MAX_CHUNK).contains(&chunk_z)
                {
                    continue;
                }
                let min_x = chunk_x * 16;
                let min_z = chunk_z * 16;
                let dx = (min_x - x).max(x - (min_x + 15)).max(0);
                let dz = (min_z - z).max(z - (min_z + 15)).max(0);
                if dx + dz <= 15 {
                    self.pending_light_chunks.insert((chunk_x, chunk_z));
                }
            }
        }
    }

    pub fn edited_chunks(&self) -> BTreeSet<(i32, i32)> {
        self.edits
            .keys()
            .map(|&(x, _, z)| (x.div_euclid(16), z.div_euclid(16)))
            .collect()
    }
    pub fn new() -> Self {
        Self::default()
    }
    pub fn natural(seed: u64) -> Self {
        Self {
            generation: Generation::Natural(seed),
            ..Self::default()
        }
    }
    pub fn is_natural(&self) -> bool {
        matches!(
            self.generation,
            Generation::Natural(_) | Generation::Biomes(_) | Generation::Expanded(_, _)
        )
    }
    pub fn is_biome_world(&self) -> bool {
        matches!(self.generation, Generation::Biomes(_))
    }
    /// Opt-in IRW4 world. Older world versions retain their exact terrain.
    pub fn expanded(seed: u64, dimension: crate::dimensions::Dimension) -> Self {
        Self {
            generation: Generation::Expanded(seed, dimension),
            ..Self::default()
        }
    }
    pub fn extended_identity(&self) -> Option<(u64, crate::dimensions::Dimension)> {
        match self.generation {
            Generation::Expanded(seed, dim) => Some((seed, dim)),
            _ => None,
        }
    }
    /// Returns deterministic starting stacks only for an untouched IRW4 dungeon chest.
    pub fn generated_dungeon_loot(&self, x: i32, y: i32, z: i32) -> Option<[(i16, u8); 3]> {
        match self.generation {
            Generation::Expanded(seed, crate::dimensions::Dimension::Overworld)
                if self.get(x, y, z) >> 4 == 54 =>
            {
                crate::worldgen::dungeon_loot(seed, x, y, z)
            }
            _ => None,
        }
    }
    /// Resolve generated village doors against the authoritative live block state.
    /// Unlike static worldgen coordinates, this excludes doors removed or
    /// replaced by players. Only IRW4 Overworld villages have this layout.
    /// A future villager AI can consume this bounded two-door lookup without
    /// caching stale generated blocks.
    #[allow(dead_code)] // Villager AI integration is a separate A07 step.
    pub fn live_village_house_doors(&self, x: i32, z: i32) -> Option<[Option<[i32; 3]>; 2]> {
        let (seed, dimension) = self.extended_identity()?;
        if dimension != crate::dimensions::Dimension::Overworld {
            return None;
        }
        // A generated door is no longer a valid village goal when the block
        // beneath its lower half is removed or replaced by non-solid terrain.
        // Match the live support check used for player-built village doors.
        let doors = crate::worldgen::village_live_house_doors(seed, x, z, |dx, dy, dz| {
            self.get(dx, dy, dz)
        })?;
        Some(doors.map(|door| {
            door.filter(|&[dx, dy, dz]| self.can_support(dx, dy, dz, self.get(dx, dy, dz)))
        }))
    }
    /// Resolve intact generated and player-built wooden doors inside a generated
    /// village footprint. This is a live, bounded lookup, not a persistent
    /// village registry: only edited lower halves are scanned and each candidate
    /// must have an intact upper half. Iron doors are not villager houses.
    ///
    /// Keep generated doors and select the nearest edited doors by 3D distance
    /// to the village center. Edit-map iteration is ordered by x/y/z, which
    /// otherwise lets distant doors exhaust the 128-door budget before nearby
    /// houses are considered. Equal distances use coordinates as a stable tie-break.
    pub fn village_doors_including_player_edits(&self, x: i32, z: i32) -> Option<Vec<[i32; 3]>> {
        let (seed, dimension) = self.extended_identity()?;
        if dimension != crate::dimensions::Dimension::Overworld {
            return None;
        }
        let (cx, cy, cz) = crate::worldgen::village_center(seed, x, z)?;
        let mut doors: BTreeSet<[i32; 3]> = self
            .live_village_house_doors(x, z)?
            .into_iter()
            .flatten()
            .collect();
        let remaining = 128usize.saturating_sub(doors.len());
        let mut nearest = BTreeSet::new();
        for (&(dx, dy, dz), &lower) in self.edits.range(
            (cx.saturating_sub(31), 0u8, i32::MIN)..=(cx.saturating_add(31), u8::MAX, i32::MAX),
        ) {
            if (i64::from(dz) - i64::from(cz)).abs() > 26
                || dy == u8::MAX
                || lower >> 4 != 64
                || lower & 8 != 0
            {
                continue;
            }
            let upper = self.get(dx, i32::from(dy) + 1, dz);
            if upper >> 4 == 64 && upper & 8 != 0 && self.can_support(dx, i32::from(dy), dz, lower)
            {
                let door = [dx, i32::from(dy), dz];
                if doors.contains(&door) {
                    continue;
                }
                let x_delta = i64::from(dx) - i64::from(cx);
                let y_delta = i64::from(dy) - i64::from(cy);
                let z_delta = i64::from(dz) - i64::from(cz);
                let distance = x_delta * x_delta + y_delta * y_delta + z_delta * z_delta;
                nearest.insert((distance, door));
                if nearest.len() > remaining {
                    nearest.pop_last();
                }
            }
        }
        doors.extend(nearest.into_iter().map(|(_, door)| door));
        Some(doors.into_iter().collect())
    }
    /// A bounded, live extension of an existing generated village: accept
    /// player-built doors outside its original footprint when a chain of
    /// intact doors connects them to the generated village. Each link spans
    /// at most 16 horizontal blocks and 4 vertical blocks, and the search
    /// stays within 16 blocks of the original footprint. Removing a bridge
    /// door immediately removes
    /// disconnected annex doors from the live result.
    /// This does not create independent villages or implement vanilla
    /// sky-exposure/population-based village boundary rules.
    pub fn village_doors_with_bounded_annex(&self, x: i32, z: i32) -> Option<Vec<[i32; 3]>> {
        let (seed, dimension) = self.extended_identity()?;
        if dimension != crate::dimensions::Dimension::Overworld {
            return None;
        }
        // An annex is limited to 16 blocks beyond the generated footprint.
        // Its coordinates still fall in the same 192-block generation cell.
        let cell_x = x.div_euclid(192).checked_mul(192)?.checked_add(48)?;
        let cell_z = z.div_euclid(192).checked_mul(192)?.checked_add(48)?;
        let (cx, _, cz) = crate::worldgen::village_center(
            seed,
            x.clamp(cell_x - 31, cell_x + 31),
            z.clamp(cell_z - 26, cell_z + 26),
        )?;
        let offset_x = (i64::from(x) - i64::from(cx)).abs();
        let offset_z = (i64::from(z) - i64::from(cz)).abs();
        if offset_x > 47 || offset_z > 42 {
            return None;
        }

        let core = self.village_doors_including_player_edits(cx, cz)?;
        if core.is_empty() {
            return None;
        }
        let mut doors: BTreeSet<[i32; 3]> = core.iter().copied().collect();
        // Collect a bounded set of live player-built doors near the village.
        // A door joins the annex only through a chain of intact doors with
        // at most 16 horizontal and 4 vertical blocks between lower halves.
        // The edit map is sorted by x/y/z, not by proximity. Keeping the
        // first 512 doors can discard the only connected annex door when
        // distant edits happen to sort first. Prefer candidates closest in
        // 3D to a live core door, prioritizing doors reachable from the core
        // before distance and then a deterministic coordinate tie-break.
        let mut candidates = BTreeSet::new();
        for (&(dx, dy, dz), &lower) in self.edits.range(
            (cx.saturating_sub(47), 0u8, i32::MIN)..=(cx.saturating_add(47), u8::MAX, i32::MAX),
        ) {
            let x_distance = (i64::from(dx) - i64::from(cx)).abs();
            let z_distance = (i64::from(dz) - i64::from(cz)).abs();
            if z_distance > 42
                || (x_distance <= 31 && z_distance <= 26)
                || dy == u8::MAX
                || lower >> 4 != 64
                || lower & 8 != 0
            {
                continue;
            }
            let upper = self.get(dx, i32::from(dy) + 1, dz);
            if upper >> 4 == 64 && upper & 8 != 0 && self.can_support(dx, i32::from(dy), dz, lower)
            {
                // The 512-candidate budget must favor a door that can
                // actually join the core. A nearby door on another floor
                // (vertical gap > 4) must not evict a valid bridge door.
                let (unreachable, distance) = core
                    .iter()
                    .map(|anchor| {
                        let x_delta = i64::from(dx) - i64::from(anchor[0]);
                        let y_delta = i64::from(dy) - i64::from(anchor[1]);
                        let z_delta = i64::from(dz) - i64::from(anchor[2]);
                        (
                            x_delta * x_delta + z_delta * z_delta > 16 * 16 || y_delta.abs() > 4,
                            x_delta * x_delta + y_delta * y_delta + z_delta * z_delta,
                        )
                    })
                    .min()
                    .unwrap_or((true, i64::MAX));
                candidates.insert((unreachable, distance, [dx, i32::from(dy), dz]));
                if candidates.len() > 512 {
                    candidates.pop_last();
                }
            }
        }
        let candidates: BTreeSet<_> = candidates.into_iter().map(|(_, _, door)| door).collect();

        let mut annex = Vec::new();
        let mut frontier = core;
        while let Some([ax, ay, az]) = frontier.pop() {
            if doors.len() >= 128 {
                break;
            }
            for door in &candidates {
                if doors.len() >= 128 {
                    break;
                }
                if doors.contains(door) {
                    continue;
                }
                let x_delta = i64::from(door[0]) - i64::from(ax);
                let z_delta = i64::from(door[2]) - i64::from(az);
                let y_delta = i64::from(door[1]) - i64::from(ay);
                if x_delta * x_delta + z_delta * z_delta <= 16 * 16 && y_delta.abs() <= 4 {
                    doors.insert(*door);
                    annex.push(*door);
                    frontier.push(*door);
                }
            }
        }
        // Outside the original footprint, a nearby live annex door is
        // required; do not make empty terrain count as an expanded village.
        if (offset_x > 31 || offset_z > 26)
            && !annex.iter().any(|[ax, _, az]| {
                let x_delta = i64::from(x) - i64::from(*ax);
                let z_delta = i64::from(z) - i64::from(*az);
                x_delta * x_delta + z_delta * z_delta <= 16 * 16
            })
        {
            return None;
        }
        Some(doors.into_iter().collect())
    }
    /// Whether a column has an unobstructed path to the sky above a door.
    /// Use the same opaque-block classification as the client light arrays,
    /// but do not confuse lateral skylight with direct sky exposure.
    fn village_column_sees_sky(&self, x: i32, y: i32, z: i32) -> bool {
        if !(0..=255).contains(&y) {
            return false;
        }
        // The artificial IRW world boundary is not a solid roof. Treat
        // columns beyond it as open sky so edge doors cannot become houses
        // solely because a five-block sample crosses the world limit.
        if !(-WORLD_LIMIT..WORLD_LIMIT).contains(&x) || !(-WORLD_LIMIT..WORLD_LIMIT).contains(&z) {
            return true;
        }
        (y..=255).all(|height| lighting::lighting_signature(self.get(x, height, z)).0)
    }

    /// Find a small set of player-built wooden doors that could establish a
    /// village independently of generated structures. A door is a candidate
    /// only when its halves are intact, it has a supporting block, and the
    /// five sky-exposure samples on opposite sides differ (the 1.7.10 door
    /// "inside/outside" rule). This is a live, bounded local query, not a
    /// persistent village registry or a complete population/radius model.
    ///
    /// Only edited door lower halves are considered. Generated doors remain
    /// owned by the generated-village lookup, and Nether/End doors are ignored.
    #[allow(dead_code)] // Compatibility API; live villager AI uses the height-aware variant.
    pub fn independent_player_village_doors_near(&self, x: i32, z: i32) -> Vec<[i32; 3]> {
        self.independent_player_village_doors_filtered(x, z, None)
    }

    /// Height-aware door lookup for live villager AI. A house on another
    /// floor must not consume the bounded candidate budget or become a goal.
    /// This is a local heuristic, not a complete vanilla village registry.
    pub fn independent_player_village_doors_near_at(
        &self,
        x: i32,
        y: i32,
        z: i32,
    ) -> Vec<[i32; 3]> {
        if !(0..=255).contains(&y) {
            return Vec::new();
        }
        self.independent_player_village_doors_filtered(x, z, Some(y))
    }

    fn independent_player_village_doors_filtered(
        &self,
        x: i32,
        z: i32,
        at_y: Option<i32>,
    ) -> Vec<[i32; 3]> {
        if !matches!(
            self.generation,
            Generation::Expanded(_, crate::dimensions::Dimension::Overworld)
        ) || !in_bounds(x, 64, z)
        {
            return Vec::new();
        }
        // Edits are ordered by x/y/z, not by proximity. Keeping the first
        // 32 doors can hide a nearby house behind 32 distant ones.
        // Retain the 32 closest valid doors in deterministic distance order.
        let mut nearest = BTreeSet::new();
        for (&(dx, dy, dz), &lower) in self.edits.range(
            (x.saturating_sub(32), 0u8, i32::MIN)..=(x.saturating_add(32), u8::MAX, i32::MAX),
        ) {
            let x_delta = i64::from(dx) - i64::from(x);
            let z_delta = i64::from(dz) - i64::from(z);
            let distance_squared = x_delta * x_delta + z_delta * z_delta;
            // A lower half at y=253 can still have an upper half at 254
            // and an asymmetric roof at 255. At y=254 there is no room
            // for the sky-exposure sample two blocks above the lower half.
            if distance_squared > 32 * 32
                || dy == 0
                || dy > 253
                || lower >> 4 != 64
                || lower & 8 != 0
            {
                continue;
            }
            let height = i32::from(dy);
            let height_delta = at_y.map_or(0, |at_y| i64::from(height) - i64::from(at_y));
            if height_delta.abs() > 4 {
                continue;
            }
            let key = (
                distance_squared + height_delta * height_delta,
                [dx, height, dz],
            );
            // No farther candidate can displace the current farthest door.
            // Skip expensive sky checks for such candidates.
            if nearest.len() >= 32 && nearest.last().is_some_and(|last| key >= *last) {
                continue;
            }
            let upper = self.get(dx, height + 1, dz);
            // Match the same solid-support rule used when placing a door.
            // Water, torches, and other non-solid blocks cannot anchor a house.
            if upper >> 4 != 64 || upper & 8 == 0 || !self.can_support(dx, height, dz, lower) {
                continue;
            }
            // Lower-half metadata 0/2 faces along Z, 1/3 along X.
            let (axis_x, axis_z) = if lower & 1 == 0 { (0, 1) } else { (1, 0) };
            let covered = |side: i32| {
                (1..=5)
                    .filter(|step| {
                        !self.village_column_sees_sky(
                            dx + axis_x * step * side,
                            height + 2,
                            dz + axis_z * step * side,
                        )
                    })
                    .count()
            };
            if covered(1) != covered(-1) {
                nearest.insert(key);
                if nearest.len() > 32 {
                    nearest.pop_last();
                }
            }
        }
        nearest.into_iter().map(|(_, door)| door).collect()
    }
    /// Bounded lookup of intact dungeon spawners close to active players.
    /// IRW4 generated sites are indexed by their 64-block feature cells so
    /// this never walks ungenerated chunks or scans a large 3D volume.
    /// Spawners added through block edits are also included.
    pub fn nearby_spawners(
        &self,
        players: &[crate::player::PlayerPosition],
    ) -> Vec<(i32, i32, i32)> {
        let players: Vec<_> = players
            .iter()
            .copied()
            .filter(|p| p.is_valid())
            .take(32)
            .collect();
        if players.is_empty() {
            return Vec::new();
        }
        let nearby = |x: i32, y: i32, z: i32| {
            players.iter().any(|p| {
                (p.x - (f64::from(x) + 0.5)).powi(2)
                    + (p.y - (f64::from(y) + 0.5)).powi(2)
                    + (p.z - (f64::from(z) + 0.5)).powi(2)
                    <= 16.0 * 16.0
            })
        };
        let mut found = BTreeSet::new();
        if let Generation::Expanded(seed, crate::dimensions::Dimension::Overworld) = self.generation
        {
            for player in &players {
                let gx = (player.x.floor() as i32).div_euclid(64);
                let gz = (player.z.floor() as i32).div_euclid(64);
                for rx in gx - 1..=gx + 1 {
                    for rz in gz - 1..=gz + 1 {
                        let (x, y, z) = crate::worldgen::dungeon_spawner_in_cell(seed, rx, rz);
                        if nearby(x, y, z) && self.get(x, y, z) >> 4 == 52 {
                            found.insert((x, y, z));
                        }
                    }
                }
            }
        }
        for (&(x, y, z), &block) in &self.edits {
            if block >> 4 == 52 && nearby(x, i32::from(y), z) {
                found.insert((x, i32::from(y), z));
            }
        }
        found.into_iter().take(128).collect()
    }

    /// Enumerate actual spawner blocks in one chunk for lossless Anvil
    /// TileEntity projection. Edits that removed generated spawners are
    /// respected, and explicitly placed spawners are included. This lookup
    /// does not create or modify a chunk.
    pub(crate) fn spawners_in_chunk(&self, cx: i32, cz: i32) -> Vec<(i32, i32, i32)> {
        let mut found = BTreeSet::new();
        if let Generation::Expanded(seed, crate::dimensions::Dimension::Overworld) = self.generation
        {
            let gx = cx.div_euclid(4);
            let gz = cz.div_euclid(4);
            let (x, y, z) = crate::worldgen::dungeon_spawner_in_cell(seed, gx, gz);
            if x.div_euclid(16) == cx && z.div_euclid(16) == cz && self.get(x, y, z) >> 4 == 52 {
                found.insert((x, y, z));
            }
        }
        for (&(x, y, z), &block) in &self.edits {
            if block >> 4 == 52 && x.div_euclid(16) == cx && z.div_euclid(16) == cz {
                found.insert((x, i32::from(y), z));
            }
        }
        found.into_iter().collect()
    }

    pub fn with_biomes(seed: u64) -> Self {
        Self {
            generation: Generation::Biomes(seed),
            ..Self::default()
        }
    }
    pub fn natural_seed(&self) -> Option<u64> {
        match self.generation {
            Generation::Natural(seed)
            | Generation::Biomes(seed)
            | Generation::Expanded(seed, _) => Some(seed),
            Generation::Flat => None,
        }
    }
    /// Biome at a natural spawn candidate (legacy flat/natural worlds use plains).
    pub fn biome_at(&self, x: i32, z: i32) -> u8 {
        match self.generation {
            Generation::Biomes(seed) => crate::terrain::biome(seed, x, z),
            Generation::Expanded(seed, crate::dimensions::Dimension::Overworld) => {
                crate::worldgen::biome(seed, x, z)
            }
            Generation::Expanded(_, crate::dimensions::Dimension::Nether) => 8,
            Generation::Expanded(_, crate::dimensions::Dimension::End) => 9,
            _ => 1,
        }
    }
    pub fn dimension(&self) -> crate::dimensions::Dimension {
        self.extended_identity()
            .map(|(_, dimension)| dimension)
            .unwrap_or(crate::dimensions::Dimension::Overworld)
    }
    /// Uses the active generator, not the legacy terrain generator, for mob candidates.
    pub fn surface_height_at(&self, x: i32, z: i32) -> Option<i32> {
        match self.generation {
            Generation::Flat => None,
            Generation::Natural(seed) => Some(crate::terrain::surface(seed, x, z)),
            Generation::Biomes(seed) => Some(crate::terrain::biome_surface(seed, x, z)),
            Generation::Expanded(seed, crate::dimensions::Dimension::Overworld) => {
                Some(crate::worldgen::height(seed, x, z))
            }
            Generation::Expanded(_, _) => None,
        }
    }
    fn register_weather_column(&mut self, pos: Position) {
        if self.pending_weather.contains(&pos) {
            return;
        }
        if self.pending_weather.len() == MAX_WEATHER_COLUMNS {
            if let Some(oldest) = self.pending_weather_order.pop_front() {
                self.pending_weather.remove(&oldest);
            }
        }
        self.pending_weather.insert(pos);
        self.pending_weather_order.push_back(pos);
    }

    fn unregister_weather_column(&mut self, pos: Position) {
        if self.pending_weather.remove(&pos) {
            self.pending_weather_order.retain(|&stored| stored != pos);
        }
    }

    /// Keep natural, unedited weather columns near loaded client chunks
    /// eligible for snow/ice updates. The existing 20TPS world tick is the
    /// only scheduler; observing a chunk does not write or mutate its blocks.
    pub fn observe_weather_chunk(&mut self, cx: i32, cz: i32) {
        if self.dimension() != crate::dimensions::Dimension::Overworld {
            return;
        }
        let (Some(x0), Some(z0)) = (cx.checked_mul(16), cz.checked_mul(16)) else {
            return;
        };
        for dx in 0..16 {
            for dz in 0..16 {
                let (x, z) = (x0 + dx, z0 + dz);
                if !in_bounds(x, 64, z) || !self.is_cold_biome(x, z) {
                    continue;
                }
                let Some(base_y) = self.surface_height_at(x, z) else {
                    continue;
                };
                let mut top = base_y.clamp(0, 254);
                // Include shallow trees/raised surface edits and sea-level
                // water without scanning all 256 Y cells for every chunk.
                for y in top + 1..=(top + 24).min(254) {
                    if self.get(x, y, z) != 0 {
                        top = y;
                    }
                }
                if matches!(
                    self.get(x, top, z) >> 4,
                    1 | 2 | 3 | 4 | 5 | 8 | 9 | 12 | 13 | 24 | 80
                ) {
                    self.register_weather_column((x, top as u8, z));
                }
            }
        }
    }
    /// Weather slots are bounded and evicted in observation order, without
    /// affecting persistent terrain data or active redstone processing.
    #[cfg(test)]
    fn weather_tracking_len(&self) -> usize {
        self.pending_weather.len()
    }

    pub fn spawn_y(&self) -> f64 {
        match self.generation {
            Generation::Natural(seed) => f64::from(crate::terrain::surface(seed, 0, 0) + 1),
            Generation::Biomes(seed) => f64::from(crate::terrain::biome_surface(seed, 0, 0) + 1),
            Generation::Expanded(seed, crate::dimensions::Dimension::Overworld) => {
                f64::from(crate::worldgen::height(seed, 0, 0) + 1)
            }
            Generation::Expanded(_, crate::dimensions::Dimension::Nether) => 80.0,
            Generation::Expanded(_, crate::dimensions::Dimension::End) => 81.0,
            Generation::Flat => 65.0,
        }
    }
    fn base_block(&self, x: i32, y: u8, z: i32) -> u16 {
        match self.generation {
            Generation::Flat => Self::default_block(y),
            Generation::Natural(seed) => crate::terrain::block(seed, x, i32::from(y), z),
            Generation::Biomes(seed) => crate::terrain::biome_block(seed, x, i32::from(y), z),
            Generation::Expanded(seed, dim) => {
                crate::worldgen::block(seed, dim, x, i32::from(y), z)
            }
        }
    }
    pub fn default_block(y: u8) -> u16 {
        match y {
            60 => 7 << 4,
            61 | 62 => 3 << 4,
            63 => 2 << 4,
            _ => 0,
        }
    }
    pub fn get(&self, x: i32, y: i32, z: i32) -> u16 {
        if !in_bounds(x, y, z) {
            return 0;
        }
        self.edits
            .get(&(x, y as u8, z))
            .copied()
            .unwrap_or_else(|| self.base_block(x, y as u8, z))
    }
    /// A changed cell may be a new gravity block or the support of one above it.
    /// Only queue known falling blocks; never scan the whole flat world per tick.
    fn wake_gravity(&mut self, x: i32, y: i32, z: i32) {
        for candidate_y in [y, y + 1] {
            if !in_bounds(x, candidate_y, z) {
                continue;
            }
            if matches!(self.get(x, candidate_y, z) >> 4, 12 | 13 | 145) {
                self.pending_gravity.insert((x, candidate_y as u8, z));
            }
        }
    }

    /// Advance bounded falling sand/gravel exactly one vertical block per tick.
    /// All changes in one tick are saved together; disk failure restores the
    /// previous sparse world and keeps the queue intact for a safe retry.
    pub fn gravity_tick(&mut self, path: &Path, budget: usize) -> io::Result<Vec<GravityMove>> {
        let scheduled: Vec<Position> = self.pending_gravity.iter().take(budget).copied().collect();
        let mut undo: BTreeMap<Position, Option<u16>> = BTreeMap::new();
        let mut moved = Vec::new();
        for &(x, y, z) in &scheduled {
            let height = i32::from(y);
            let block = self.get(x, height, z);
            if height == 0 || !matches!(block >> 4, 12 | 13 | 145) {
                continue;
            }
            if self.get(x, height - 1, z) != 0 {
                continue;
            }
            let upper = (x, y, z);
            let lower = (x, y - 1, z);
            for cell in [upper, lower] {
                undo.entry(cell)
                    .or_insert_with(|| self.edits.get(&cell).copied());
            }
            self.set_gravity_cell(upper, 0);
            self.set_gravity_cell(lower, block);
            moved.push(GravityMove {
                from: (x, height, z),
                to: (x, height - 1, z),
            });
        }
        if !moved.is_empty() {
            let written = if self.edits.len() > MAX_EDITS {
                Err(invalid("gravity tick would exceed IRW1 edit capacity"))
            } else {
                self.save(path)
            };
            if let Err(error) = written {
                for (cell, old) in undo {
                    match old {
                        Some(value) => {
                            self.edits.insert(cell, value);
                        }
                        None => {
                            self.edits.remove(&cell);
                        }
                    }
                }
                return Err(error);
            }
        }
        for position in scheduled {
            self.pending_gravity.remove(&position);
        }
        // Wake moving blocks for the next tick, and the block above their
        // former location, which may now have become unsupported.
        for movement in &moved {
            self.mark_lighting_changed(movement.from.0, movement.from.2, 12 << 4, 0);
            self.mark_lighting_changed(movement.to.0, movement.to.2, 0, 12 << 4);
            self.wake_gravity(movement.to.0, movement.to.1, movement.to.2);
            self.wake_gravity(movement.from.0, movement.from.1, movement.from.2);
            self.wake_block_updates(movement.to.0, movement.to.1, movement.to.2);
            self.wake_block_updates(movement.from.0, movement.from.1, movement.from.2);
        }
        Ok(moved)
    }

    /// Mutate the sparse representation without writing a partial tick.
    fn set_gravity_cell(&mut self, (x, y, z): Position, block: u16) {
        let key = (x, y, z);
        if block == self.base_block(x, y, z) {
            self.edits.remove(&key);
        } else {
            self.edits.insert(key, block);
        }
    }

    /// Wake fluids and powered dust around an edited block, without scanning
    /// every generated chunk. Source types are 1.7.10 8=water and 10=lava.
    fn wake_block_updates(&mut self, x: i32, y: i32, z: i32) {
        for (nx, ny, nz) in [
            (x, y, z),
            (x + 1, y, z),
            (x - 1, y, z),
            (x, y, z + 1),
            (x, y, z - 1),
            (x, y + 1, z),
            (x, y - 1, z),
        ] {
            if !in_bounds(nx, ny, nz) {
                continue;
            }
            let id = self.get(nx, ny, nz) >> 4;
            let pos = (nx, ny as u8, nz);
            if matches!(id, 8..=11) {
                self.pending_fluid.insert(pos);
            }
            if id == 55 {
                self.pending_circuit.insert(pos);
            }
            if matches!(id, 2 | 3 | 8 | 9) {
                self.register_weather_column(pos);
            } else {
                self.unregister_weather_column(pos);
            }
            if matches!(
                id,
                6 | 18 | 51 | 59 | 60 | 78 | 79 | 81 | 83 | 104 | 105 | 115 | 127 | 141 | 142 | 161
            ) {
                self.pending_ecology.insert(pos);
            } else {
                self.pending_ecology.remove(&pos);
            }
            if matches!(
                id,
                6 | 31
                    | 32
                    | 37
                    | 38
                    | 50
                    | 81
                    | 83
                    | 104
                    | 105
                    | 115
                    | 127
                    | 55
                    | 59
                    | 66
                    | 70
                    | 131
                    | 132
                    | 147
                    | 148
                    | 151
                    | 72
                    | 75
                    | 76
                    | 77
                    | 93
                    | 94
                    | 141
                    | 142
                    | 143
                    | 64
                    | 71
                    | 96
            ) {
                self.pending_support.insert(pos);
            }
            if matches!(
                id,
                23 | 27 | 29 | 33 | 46 | 93 | 94 | 131 | 147 | 148 | 149 | 150 | 151 | 157 | 158
            ) {
                self.pending_devices.insert(pos);
            } else {
                self.pending_devices.remove(&pos);
                self.pending_repeaters.remove(&pos);
                self.pending_comparators.remove(&pos);
                self.comparator_outputs.remove(&pos);
            }
        }
        // A strong input may power a solid neighbor, then weakly feed dust
        // or a device one additional block away. Wake only redstone at this
        // distance, not fluid/ecology: O(36) fixed neighborhood probes.
        const FACES: [(i32, i32, i32); 6] = [
            (1, 0, 0),
            (-1, 0, 0),
            (0, 1, 0),
            (0, -1, 0),
            (0, 0, 1),
            (0, 0, -1),
        ];
        for (dx, dy, dz) in FACES {
            for (ex, ey, ez) in FACES {
                let (nx, ny, nz) = (x + dx + ex, y + dy + ey, z + dz + ez);
                if !in_bounds(nx, ny, nz) {
                    continue;
                }
                let pos = (nx, ny as u8, nz);
                match self.get(nx, ny, nz) >> 4 {
                    55 => {
                        self.pending_circuit.insert(pos);
                    }
                    27 | 29 | 33 | 93 | 94 | 149 | 150 | 157 => {
                        self.pending_devices.insert(pos);
                    }
                    _ => {}
                }
            }
        }
    }
    fn wake_leaves(&mut self, x: i32, y: i32, z: i32) {
        for dx in -4..=4 {
            for dz in -4..=4 {
                for dy in -4..=4 {
                    let (nx, ny, nz) = (x + dx, y + dy, z + dz);
                    if in_bounds(nx, ny, nz) && matches!(self.get(nx, ny, nz) >> 4, 18 | 161) {
                        self.pending_ecology.insert((nx, ny as u8, nz));
                    }
                }
            }
        }
    }

    /// Bounded 20TPS flow/conduction step. IRW save occurs once per tick.
    /// Empty fluid destinations and wire strengths are staged before mutation.
    pub fn block_tick(&mut self, path: &Path, budget: usize) -> io::Result<Vec<(i32, i32, i32)>> {
        self.fluid_phase = self.fluid_phase.wrapping_add(1);
        self.piston_sweep_ghosts
            .retain(|_, expiry| *expiry > self.fluid_phase);
        // Lava updates every fourth server tick, water every tick. Delayed
        // lava positions remain queued rather than getting silently dropped.
        let wet: Vec<Position> = self
            .pending_fluid
            .iter()
            .filter(|&&(x, y, z)| {
                // Nether lava flows on every scheduled server tick; in the
                // Overworld and End we retain the slower four-tick cadence.
                !matches!(self.get(x, i32::from(y), z) >> 4, 10 | 11)
                    || self.dimension() == crate::dimensions::Dimension::Nether
                    || self.fluid_phase.is_multiple_of(4)
            })
            .take(budget)
            .copied()
            .collect();
        let wired: Vec<Position> = self.pending_circuit.iter().take(budget).copied().collect();
        let supported: Vec<Position> = self.pending_support.iter().take(budget).copied().collect();
        let mut changes: BTreeMap<Position, u16> = self.ecology_changes(budget);
        changes.extend(self.device_changes(budget));
        changes.extend(self.special_changes(budget));
        // Timed redstone components must release power on the authoritative
        // world tick, not merely disappear from the in-memory timer table.
        // Stage their reset in the same durable snapshot as other updates.
        let expired_timers: Vec<Position> = self
            .pending_buttons
            .iter()
            .filter(|&(_, deadline)| self.fluid_phase >= *deadline)
            .take(budget)
            .map(|(&pos, _)| pos)
            .collect();
        for &(x, y, z) in &expired_timers {
            let old = self.get(x, i32::from(y), z);
            let reset = match old >> 4 {
                70 | 72 => old & !1,        // Stone and wooden pressure plates.
                77 | 131 | 143 => old & !8, // Buttons and tripwire hooks.
                132 => old & !1,            // Tripwire: clear triggered, retain attachment.
                147 | 148 => old & !15,     // Weighted analog pressure plates.
                _ => old,
            };
            if reset != old {
                changes.entry((x, y, z)).or_insert(reset);
            }
        }
        // Retain the old block identity until the world snapshot has committed.
        let primed_on_commit: Vec<_> = changes
            .iter()
            .filter_map(|(&(x, y, z), &new)| {
                (new == 0 && self.get(x, i32::from(y), z) >> 4 == 46).then_some((
                    x,
                    i32::from(y),
                    z,
                ))
            })
            .collect();

        for &(x, y, z) in &wet {
            let y = i32::from(y);
            let block = self.get(x, y, z);
            let id = match block >> 4 {
                8 | 9 => 8,
                10 | 11 => 10,
                _ => continue,
            };
            // Water cannot be placed normally in the Nether. Remove any
            // imported or leftover flowing water during the next bounded
            // fluid update without allowing it to spread first.
            if id == 8 && self.dimension() == crate::dimensions::Dimension::Nether {
                changes.insert((x, y as u8, z), 0);
                continue;
            }
            let mut level = block & 15;
            // One level per horizontal step in the Nether; two for lava
            // elsewhere, and one for water in every dimension.
            let horizontal_cost: u16 =
                if id == 10 && self.dimension() != crate::dimensions::Dimension::Nether {
                    2
                } else {
                    1
                };
            let adjacent = [
                (x - 1, y, z),
                (x + 1, y, z),
                (x, y, z - 1),
                (x, y, z + 1),
                (x, y - 1, z),
                (x, y + 1, z),
            ];
            // Resolve water/lava contact before flowing, preserving the
            // source-lava -> obsidian and flowing-lava -> cobblestone rules.
            if id == 10 {
                if adjacent.iter().any(|&(nx, ny, nz)| {
                    in_bounds(nx, ny, nz) && matches!(self.get(nx, ny, nz) >> 4, 8 | 9)
                }) {
                    changes.insert((x, y as u8, z), (if level == 0 { 49 } else { 4 }) << 4);
                    continue;
                }
            } else {
                for &(nx, ny, nz) in &adjacent {
                    if !in_bounds(nx, ny, nz) {
                        continue;
                    }
                    let other = self.get(nx, ny, nz);
                    if matches!(other >> 4, 10 | 11) {
                        changes.insert(
                            (nx, ny as u8, nz),
                            (if other & 15 == 0 { 49 } else { 4 }) << 4,
                        );
                    }
                }
            }
            // A flow cell must be connected to a better upstream level.
            // This makes disconnected rivers recede after their source is
            // removed, instead of leaving permanent unsupported water.
            if level != 0 {
                let above = self.get(x, y + 1, z);
                let above_id = above >> 4;
                let fed_from_above = above_id == id || above_id == id + 1;
                // Falling fluids use metadata level 8, not level 1. They
                // remain falling while fed vertically from their source.
                let mut next = if fed_from_above { 8 } else { 9 };
                let sides = [(x - 1, z), (x + 1, z), (x, z - 1), (x, z + 1)];
                let mut source_neighbors = 0;
                for (nx, nz) in sides {
                    let other = self.get(nx, y, nz);
                    if other >> 4 == id || other >> 4 == id + 1 {
                        if other & 15 == 0 {
                            source_neighbors += 1;
                        }
                        // Falling neighbors (metadata 8) are not a horizontal
                        // upstream source; only stationary lateral flow feeds.
                        if other & 15 < 8 {
                            next = next.min((other & 15) + horizontal_cost);
                        }
                    }
                }
                let solid_below = y > 0 && !matches!(self.get(x, y - 1, z) >> 4, 0 | 8..=11);
                if id == 8 && source_neighbors >= 2 && solid_below {
                    next = 0;
                }
                if next > 8 || (next == 8 && !fed_from_above) {
                    changes.insert((x, y as u8, z), 0);
                    continue;
                }
                if next != level {
                    changes.insert((x, y as u8, z), (id << 4) | next);
                    level = next;
                }
            }
            // Level 7 cannot spread sideways, but it can still fall.
            let down = (x, y - 1, z);
            // Flow down first. Falling lava contacting water below creates
            // stone at the destination rather than replacing the water.
            if in_bounds(down.0, down.1, down.2) {
                let below = self.get(down.0, down.1, down.2);
                if id == 10 && matches!(below >> 4, 8 | 9) {
                    changes.insert((down.0, down.1 as u8, down.2), 1 << 4);
                    continue;
                }
            }
            let falling =
                in_bounds(down.0, down.1, down.2) && self.get(down.0, down.1, down.2) == 0;
            let targets: Vec<(i32, i32, i32)> = if falling {
                vec![down]
            } else if (level & 7) + horizontal_cost > 7 {
                Vec::new()
            } else {
                vec![(x + 1, y, z), (x - 1, y, z), (x, y, z + 1), (x, y, z - 1)]
            };
            for (nx, ny, nz) in targets {
                if !in_bounds(nx, ny, nz) {
                    continue;
                }
                let existing = self.get(nx, ny, nz);
                let next_level = if falling {
                    8
                } else {
                    (level & 7) + horizontal_cost
                };
                let desired = (id << 4) | next_level;
                if existing == 0 || (existing >> 4 == id && existing & 15 > next_level) {
                    let target = (nx, ny as u8, nz);
                    if changes
                        .get(&target)
                        .is_none_or(|&v| v == 0 || (v >> 4 == id && v & 15 > next_level))
                    {
                        changes.insert(target, desired);
                    }
                }
            }
        }

        for &(x, y, z) in &wired {
            let y = i32::from(y);
            let current = self.get(x, y, z);
            if current >> 4 != 55 {
                continue;
            }
            let mut strongest = 0u16;
            for (nx, nz) in [(x - 1, z), (x + 1, z), (x, z - 1), (x, z + 1)] {
                // Dust climbs one block when there is air above the
                // current dust, or descends along a block edge.
                let height = if self.get(nx, y, nz) >> 4 == 55 {
                    y
                } else if self.get(nx, y, nz) != 0 && self.get(x, y + 1, z) == 0 {
                    y + 1
                } else {
                    y - 1
                };
                for check_y in [y, height] {
                    if !in_bounds(nx, check_y, nz) {
                        continue;
                    }
                    let power = self.get(nx, check_y, nz);
                    let signal = if power >> 4 == 55 {
                        (power & 15).saturating_sub(1)
                    } else {
                        u16::from(self.signal_into((nx, check_y, nz), (x, y, z)))
                    };
                    strongest = strongest.max(signal);
                }
            }
            if current & 15 != strongest {
                changes.insert((x, y as u8, z), (55 << 4) | strongest);
            }
        }

        // Advance the primed entity state BEFORE the atomic IRW snapshot.
        // On any storage failure, undo the state and newly queued packets.
        let primed_before = self.primed_tnt.clone();
        let tnt_events_before = self.tnt_events.len();
        self.finalize_tnt(&primed_on_commit);
        let tnt_changed = self.primed_tnt != primed_before;

        if !changes.is_empty() {
            // Decaying oak leaves may yield a sapling or apple. Stage these
            // before mutation, but expose them only after a successful save.
            let detached_drops: Vec<WorldDrop> = changes
                .iter()
                .filter_map(|(&(x, y, z), &next)| {
                    let old = self.get(x, i32::from(y), z);
                    if next != 0 || old == 0 {
                        return None;
                    }
                    let pos = (x, i32::from(y), z);
                    let id = old >> 4;
                    let item_id = match id {
                        6 => 6,
                        50 => 50,
                        55 => 331,
                        59 => {
                            if old & 7 == 7 {
                                296
                            } else {
                                295
                            }
                        }
                        66 => 66,
                        37 | 38 => id as i16,
                        70 | 72 | 77 | 143 => id as i16,
                        75 | 76 => 76,
                        93 | 94 => 356,
                        141 => 391,
                        142 => 392,
                        64 if old & 8 == 0 => 324,
                        71 if old & 8 == 0 => 330,
                        96 => 96,
                        18 | 161 => {
                            return crate::survival::additional_drop(old, pos, self.fluid_phase)
                                .map(|stack| WorldDrop { pos, stack });
                        }
                        _ => return None,
                    };
                    let stack = crate::inventory::Stack {
                        id: item_id,
                        count: 1,
                        damage: if matches!(id, 6 | 38) {
                            (old & 15) as i16
                        } else {
                            0
                        },
                        nbt: None,
                    };
                    Some(WorldDrop { pos, stack })
                })
                .take(256)
                .collect();
            let dirty_light: Vec<_> = changes
                .iter()
                .filter_map(|(&(x, y, z), &new)| {
                    let old = self.get(x, i32::from(y), z);
                    (lighting::lighting_signature(old) != lighting::lighting_signature(new))
                        .then_some((x, z, old, new))
                })
                .collect();
            let piston_actions: Vec<_> = changes
                .iter()
                .filter_map(|(&(x, y, z), &new)| {
                    let old = self.get(x, i32::from(y), z);
                    (matches!(old >> 4, 29 | 33) && old >> 4 == new >> 4 && (old ^ new) & 8 != 0)
                        .then_some(PistonAction {
                            pos: (x, i32::from(y), z),
                            block_id: old >> 4,
                            facing: (old & 7) as u8,
                            extending: new & 8 != 0,
                        })
                })
                .collect();
            // Conservative swept AABB coverage over three simulation ticks.
            // This does not replace 1.7.10's moving-piston tile-entity model.
            let mut swept = BTreeSet::new();
            for action in &piston_actions {
                let (dx, dy, dz) = match action.facing {
                    0 => (0, -1, 0),
                    1 => (0, 1, 0),
                    2 => (0, 0, -1),
                    3 => (0, 0, 1),
                    4 => (-1, 0, 0),
                    _ => (1, 0, 0),
                };
                for step in 1..=13 {
                    let (x, y, z) = (
                        action.pos.0 + dx * step,
                        action.pos.1 + dy * step,
                        action.pos.2 + dz * step,
                    );
                    if !in_bounds(x, y, z) {
                        break;
                    }
                    let position = (x, y as u8, z);
                    let before = self.get(x, y, z);
                    if changes.get(&position).is_some_and(|&after| before != after) {
                        swept.insert(position);
                    }
                }
            }
            let fired_devices: Vec<_> = changes
                .iter()
                .filter_map(|(&(x, y, z), &new)| {
                    let old = self.get(x, i32::from(y), z);
                    (matches!(new >> 4, 23 | 158)
                        && new >> 4 == old >> 4
                        && old & 8 == 0
                        && new & 8 != 0)
                        .then_some((x, i32::from(y), z))
                })
                .collect();
            let undo: Vec<(Position, Option<u16>)> = changes
                .keys()
                .map(|&p| (p, self.edits.get(&p).copied()))
                .collect();
            for (&position, &block) in &changes {
                self.set_gravity_cell(position, block);
            }
            let saved = if self.edits.len() > MAX_EDITS {
                Err(invalid("fluid or redstone exceeds world edit budget"))
            } else {
                self.save(path)
            };
            if let Err(error) = saved {
                self.primed_tnt = primed_before;
                self.tnt_events.truncate(tnt_events_before);
                self.comparator_ready.clear();
                for (pos, original) in undo {
                    match original {
                        Some(block) => {
                            self.edits.insert(pos, block);
                        }
                        None => {
                            self.edits.remove(&pos);
                        }
                    }
                }
                return Err(error);
            }
            self.pending_drops.extend(detached_drops);
            self.pending_device_activations.extend(fired_devices);
            self.pending_piston_actions.extend(piston_actions);
            for position in swept {
                if self.piston_sweep_ghosts.len() >= 2048 {
                    break;
                }
                self.piston_sweep_ghosts
                    .insert(position, self.fluid_phase.saturating_add(3));
            }
            for (x, z, old, new) in dirty_light {
                self.mark_lighting_changed(x, z, old, new);
            }
        } else if !self.comparator_ready.is_empty()
            || tnt_changed
            || (!self.primed_tnt.is_empty() && self.fluid_phase.is_multiple_of(20))
        {
            // Analog-only updates need a successful save even when the
            // comparator's on/off block ID has not changed.
            if let Err(error) = self.save(path) {
                self.primed_tnt = primed_before;
                self.tnt_events.truncate(tnt_events_before);
                self.comparator_ready.clear();
                return Err(error);
            }
        }

        // Publish analog changes only after the block snapshot committed.
        // Same-output-state comparator strength changes still wake nearby dust.
        let ready = std::mem::take(&mut self.comparator_ready);
        for (pos, strength) in ready {
            if self.get(pos.0, i32::from(pos.1), pos.2) >> 4 == 150 && strength > 0 {
                self.comparator_outputs.insert(pos, strength);
            } else {
                self.comparator_outputs.remove(&pos);
            }
            self.wake_block_updates(pos.0, i32::from(pos.1), pos.2);
        }
        for pos in wet {
            self.pending_fluid.remove(&pos);
        }
        for pos in wired {
            self.pending_circuit.remove(&pos);
        }
        for pos in supported {
            self.pending_support.remove(&pos);
        }
        // Clear timers only after the world save succeeded, so a storage
        // failure cannot silently leave a permanently powered component.
        for pos in expired_timers {
            self.pending_buttons.remove(&pos);
        }
        let mut changed = Vec::with_capacity(changes.len());
        for (x, y, z) in changes.keys().copied() {
            changed.push((x, i32::from(y), z));
            self.wake_block_updates(x, i32::from(y), z);
        }
        Ok(changed)
    }

    /// Persist a small related block edit (such as the two bed halves) as
    /// one world snapshot, never exposing a half-built object to clients.
    pub fn update_many_and_save(
        &mut self,
        path: &Path,
        changes: &[(i32, i32, i32, u16)],
    ) -> io::Result<bool> {
        if changes.is_empty() || changes.len() > 2 {
            return Err(invalid("invalid batch size"));
        }
        let mut next = self.clone();
        let mut seen = BTreeSet::new();
        let mut changed = false;
        for &(x, y, z, block) in changes {
            if !in_bounds(x, y, z) || block >> 4 > 255 || !seen.insert((x, y, z)) {
                return Err(invalid("invalid batched block update"));
            }
            if next.get(x, y, z) == block {
                continue;
            }
            let key = (x, y as u8, z);
            if block == next.base_block(x, y as u8, z) {
                next.edits.remove(&key);
            } else {
                next.edits.insert(key, block);
            }
            if next.edits.len() > MAX_EDITS {
                return Err(invalid("too many edited blocks"));
            }
            next.wake_gravity(x, y, z);
            next.wake_block_updates(x, y, z);
            changed = true;
        }
        if changed {
            next.save(path)?;
            *self = next;
        }
        Ok(changed)
    }
    pub fn update_and_save(
        &mut self,
        path: &Path,
        x: i32,
        y: i32,
        z: i32,
        block: u16,
    ) -> io::Result<bool> {
        if !in_bounds(x, y, z) || block >> 4 > 255 {
            return Ok(false);
        }
        // Vanilla 1.7.10 water bucket evaporation: reject placement before
        // creating a persisted water block in the Nether.
        if self.dimension() == crate::dimensions::Dimension::Nether && matches!(block >> 4, 8 | 9) {
            return Ok(false);
        }
        let previous = self.get(x, y, z);
        if previous == block {
            return Ok(false);
        }
        let key = (x, y as u8, z);
        let old = self.edits.get(&key).copied();
        if block == self.base_block(x, y as u8, z) {
            self.edits.remove(&key);
        } else {
            if old.is_none() && self.edits.len() >= MAX_EDITS {
                return Err(invalid("too many changed blocks"));
            }
            self.edits.insert(key, block);
        }
        if let Err(e) = self.save(path) {
            match old {
                Some(value) => {
                    self.edits.insert(key, value);
                }
                None => {
                    self.edits.remove(&key);
                }
            }
            return Err(e);
        }
        self.mark_lighting_changed(x, z, previous, block);
        self.wake_gravity(x, y, z);
        self.wake_block_updates(x, y, z);
        if matches!(previous >> 4, 17 | 162) || matches!(block >> 4, 17 | 162) {
            self.wake_leaves(x, y, z);
        }
        Ok(true)
    }
    /// Opt-in E46 bridge for one existing Overworld Anvil chunk.
    /// Uses the same multi-block redo transaction as atomic related edits.
    #[allow(dead_code)] // Deliberately not enabled in the live IRW save worker.
    pub fn update_and_flush_existing_anvil(
        &mut self,
        path: &Path,
        x: i32,
        y: i32,
        z: i32,
        block: u16,
    ) -> io::Result<bool> {
        self.update_many_and_flush_existing_anvil(path, &[(x, y, z, block)])
    }

    /// E46 opt-in bridge for up to 32 explicit block changes across at most
    /// seven EXISTING Anvil regions (the eighth redo participant is IRW).
    ///
    /// Check that the old IRW disk image matches this exact live generation
    /// and that EVERY touched Anvil block matches the IRW source before
    /// committing ANY changes. The existing redo journal then publishes the
    /// next IRW snapshot and all region images together, not one at a time.
    /// This does NOT activate Anvil as the live server's primary store.
    #[allow(dead_code)] // Requires explicit migration validation before use.
    pub fn update_many_and_flush_existing_anvil(
        &mut self,
        path: &Path,
        changes: &[(i32, i32, i32, u16)],
    ) -> io::Result<bool> {
        if changes.is_empty() || changes.len() > 32 {
            return Err(invalid("E46 Anvil batch must contain 1..32 blocks"));
        }
        if self.dimension() != crate::dimensions::Dimension::Overworld {
            return Err(invalid("E46 Anvil block bridge supports Overworld only"));
        }
        let (bound_path, session) = self
            .save_session
            .as_ref()
            .ok_or_else(|| invalid("E46 Anvil block write requires session.lock"))?;
        if bound_path != path {
            return Err(invalid("Anvil block write path differs from bound world"));
        }

        // Validate the whole request before even considering an early no-op.
        // A duplicate is not a legitimate multi-block transaction: it could
        // hide an inconsistent final state in a last-writer-wins map.
        let mut seen = BTreeSet::new();
        let mut different = Vec::new();
        for &(x, y, z, block) in changes {
            if !in_bounds(x, y, z) || block >> 4 > 255 || !seen.insert((x, y, z)) {
                return Err(invalid("E46 invalid or duplicate Anvil block change"));
            }
            let previous = self.get(x, y, z);
            if previous != block {
                different.push((x, y, z, previous, block));
            }
        }
        if different.is_empty() {
            return Ok(false);
        }

        // Construct but do NOT publish the next world generation. All checks,
        // disk writes and journal replay gates precede the memory transition.
        let old_irw = self.encoded()?;
        if fs::read(path)? != old_irw {
            return Err(invalid("E46 IRW memory and disk differ before Anvil save"));
        }
        let mut next = self.clone();
        let mut regions = BTreeSet::new();
        for &(x, y, z, _, block) in &different {
            let key = (x, y as u8, z);
            if block == next.base_block(x, y as u8, z) {
                next.edits.remove(&key);
            } else {
                if !next.edits.contains_key(&key) && next.edits.len() >= MAX_EDITS {
                    return Err(invalid("too many changed blocks for E46 Anvil save"));
                }
                next.edits.insert(key, block);
            }
            regions.insert((x.div_euclid(512), z.div_euclid(512)));
        }
        if regions.len() > 7 {
            return Err(invalid("E46 Anvil batch exceeds seven region participants"));
        }
        let next_irw = next.encoded()?;
        // The old and new byte images are now immutable while we hold the OS
        // session lock. Other Rust saves must obtain this same session lock.
        let session = Arc::clone(session);
        let mut lock = session
            .lock()
            .map_err(|_| invalid("E46 session lock mutex poisoned"))?;
        lock.verify()?;
        if path.parent() != Some(lock.world_dir()) {
            return Err(invalid("E46 Anvil world must be in locked root directory"));
        }
        let mut source_images = BTreeMap::new();
        for &(rx, rz) in &regions {
            let region_path = lock
                .world_dir()
                .join("region")
                .join(format!("r.{rx}.{rz}.mca"));
            let metadata = fs::symlink_metadata(&region_path)?;
            if !metadata.file_type().is_file()
                || metadata.len() < 8192
                || metadata.len() > 8 * 1024 * 1024
            {
                return Err(invalid("E46 invalid or oversized Anvil source region"));
            }
            source_images.insert((rx, rz), fs::read(&region_path)?);
        }
        // Reuse a decoded chunk per (cx, cz) to keep the preflight bounded.
        // Never trust just the updated NBT: compare every old block state.
        let mut original_chunks = BTreeMap::new();
        for &(x, y, z, previous, _) in &different {
            let chunk_pos = (x.div_euclid(16), z.div_euclid(16));
            if let std::collections::btree_map::Entry::Vacant(entry) =
                original_chunks.entry(chunk_pos)
            {
                let (cx, cz) = chunk_pos;
                let snapshot = &source_images[&(cx.div_euclid(32), cz.div_euclid(32))];
                let mut reader = fastanvil::Region::from_stream(std::io::Cursor::new(snapshot))
                    .map_err(|error| io::Error::other(error.to_string()))?;
                let bytes = reader
                    .read_chunk(cx.rem_euclid(32) as usize, cz.rem_euclid(32) as usize)
                    .map_err(|error| io::Error::other(error.to_string()))?
                    .ok_or_else(|| invalid("E46 dirty save requires existing Anvil chunks"))?;
                entry.insert(crate::legacy_nbt::parse_chunk(&bytes, cx, cz)?);
            }
            let original = &original_chunks[&chunk_pos];
            let index = (y as usize % 16) * 256
                + z.rem_euclid(16) as usize * 16
                + x.rem_euclid(16) as usize;
            let (old_id, old_meta) = original
                .level
                .sections
                .iter()
                .find(|section| section.y == (y / 16) as i8)
                .and_then(|section| section.block_at(index))
                .unwrap_or((0, 0));
            if (old_id << 4) | u16::from(old_meta) != previous {
                return Err(invalid("E46 Anvil block disagrees with authoritative IRW"));
            }
        }
        let dirty: Vec<_> = different
            .iter()
            .map(|&(x, y, z, _, block)| (x, y, z, block))
            .collect();
        crate::legacy_region_rewrite::flush_irw_block_edits_under_lock_expected(
            &mut lock,
            crate::dimensions::Dimension::Overworld,
            &dirty,
            &source_images,
            path,
            &old_irw,
            &next_irw,
        )?;
        drop(lock);
        // Block wake queues, lighting and leaves become visible only after
        // every participant of the redo transaction was published.
        for (x, y, z, previous, block) in different {
            next.mark_lighting_changed(x, z, previous, block);
            next.wake_gravity(x, y, z);
            next.wake_block_updates(x, y, z);
            if matches!(previous >> 4, 17 | 162) || matches!(block >> 4, 17 | 162) {
                next.wake_leaves(x, y, z);
            }
        }
        *self = next;
        Ok(true)
    }

    /// E46 opt-in: atomically add ONE previously absent Anvil chunk together
    /// with the next IRW image after 1..32 explicit block edits inside it.
    ///
    /// Restricted to pristine flat Overworld chunks: until native container
    /// state is transactionally migrated, we cannot safely synthesize NBT
    /// for a chunk that has already been modified or generated elsewhere.
    /// Existing chunks must use update_many_and_flush_existing_anvil instead.
    #[allow(dead_code)] // Not wired to live block/tick saves.
    pub fn update_many_and_create_missing_anvil_chunk(
        &mut self,
        path: &Path,
        changes: &[(i32, i32, i32, u16)],
    ) -> io::Result<()> {
        if self.generation != Generation::Flat
            || self.dimension() != crate::dimensions::Dimension::Overworld
        {
            return Err(invalid("E46 new Anvil chunks require flat Overworld"));
        }
        if changes.is_empty() || changes.len() > 32 {
            return Err(invalid("E46 new-chunk batch must contain 1..32 blocks"));
        }
        let (bound, session) = self
            .save_session
            .as_ref()
            .ok_or_else(|| invalid("E46 new chunk requires bound session.lock"))?;
        if bound != path {
            return Err(invalid("E46 new chunk IRW path differs from bound session"));
        }
        let mut seen = BTreeSet::new();
        let mut target_chunk = None;
        for &(x, y, z, block) in changes {
            let chunk = (x.div_euclid(16), z.div_euclid(16));
            if !in_bounds(x, y, z)
                || block >> 4 > 255
                || block == self.get(x, y, z)
                || !seen.insert((x, y, z))
                || target_chunk.is_some_and(|existing| existing != chunk)
            {
                return Err(invalid("E46 invalid or cross-chunk block insertion"));
            }
            target_chunk = Some(chunk);
        }
        let (cx, cz) = target_chunk.ok_or_else(|| invalid("missing target chunk"))?;
        if self
            .edits
            .keys()
            .any(|&(x, _, z)| x.div_euclid(16) == cx && z.div_euclid(16) == cz)
        {
            return Err(invalid("E46 will not synthesize a previously edited chunk"));
        }
        let old_irw = self.encoded()?;
        if fs::read(path)? != old_irw {
            return Err(invalid("E46 new chunk IRW memory/disk image mismatch"));
        }
        let mut next = self.clone();
        for &(x, y, z, block) in changes {
            let key = (x, y as u8, z);
            if block == next.base_block(x, y as u8, z) {
                next.edits.remove(&key);
            } else {
                if !next.edits.contains_key(&key) && next.edits.len() >= MAX_EDITS {
                    return Err(invalid("E46 new chunk exceeds IRW edit budget"));
                }
                next.edits.insert(key, block);
            }
        }
        let next_irw = next.encoded()?;
        // Reuse the same 1.7.10 section/heightmap encoder as the offline
        // exporter. Flat pristine chunks contain no existing native tiles.
        let encoded_chunk = crate::anvil_export::nbt_for_chunk(&next, cx, cz, &[])?;
        let mut chunk = crate::legacy_nbt::parse_chunk(&encoded_chunk, cx, cz)?;
        // Export's constant lighting placeholders are NOT a live relight.
        chunk
            .level
            .extra
            .insert("LightPopulated".to_owned(), fastnbt::Value::Byte(0));
        let encoded_chunk = fastnbt::to_bytes(&chunk)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        crate::legacy_nbt::parse_chunk(&encoded_chunk, cx, cz)?;

        let session = Arc::clone(session);
        let mut lock = session
            .lock()
            .map_err(|_| invalid("E46 new chunk session mutex poisoned"))?;
        lock.verify()?;
        if path.parent() != Some(lock.world_dir()) {
            return Err(invalid("E46 new chunk world path outside locked root"));
        }
        let (rx, rz) = (cx.div_euclid(32), cz.div_euclid(32));
        let region_path = lock
            .world_dir()
            .join("region")
            .join(format!("r.{rx}.{rz}.mca"));
        let source = match fs::symlink_metadata(&region_path) {
            Ok(meta) if meta.file_type().is_file() => {
                if meta.len() < 8192 || meta.len() > 8 * 1024 * 1024 {
                    return Err(invalid("E46 new chunk source region size invalid"));
                }
                Some(fs::read(&region_path)?)
            }
            Ok(_) => return Err(invalid("E46 new chunk source region must be regular file")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        crate::legacy_region_rewrite::insert_irw_chunks_under_lock_expected(
            &mut lock,
            crate::dimensions::Dimension::Overworld,
            &[((cx, cz), encoded_chunk.as_slice())],
            &BTreeMap::from([((rx, rz), source)]),
            path,
            &old_irw,
            &next_irw,
        )?;
        drop(lock);
        for &(x, y, z, block) in changes {
            let previous = self.get(x, y, z);
            next.mark_lighting_changed(x, z, previous, block);
            next.wake_gravity(x, y, z);
            next.wake_block_updates(x, y, z);
            if matches!(previous >> 4, 17 | 162) || matches!(block >> 4, 17 | 162) {
                next.wake_leaves(x, y, z);
            }
        }
        *self = next;
        Ok(())
    }

    /// Apply one parsed block to a *detached* import world. Call save_new only
    /// after every Anvil chunk has been validated; never persist partial imports.
    pub fn import_block(&mut self, x: i32, y: i32, z: i32, block: u16) -> io::Result<()> {
        if !in_bounds(x, y, z) || block >> 4 > 255 {
            return Err(invalid("imported block outside 1.7.10 world limits"));
        }
        let key = (x, y as u8, z);
        if block == self.base_block(x, y as u8, z) {
            if self.edits.remove(&key).is_some() {
                self.wake_gravity(x, y, z);
                self.wake_block_updates(x, y, z);
            }
            return Ok(());
        }
        if !self.edits.contains_key(&key) && self.edits.len() >= MAX_EDITS {
            return Err(invalid("Anvil import exceeds IRW1 edit limit"));
        }
        self.edits.insert(key, block);
        self.wake_gravity(x, y, z);
        self.wake_block_updates(x, y, z);
        Ok(())
    }

    /// Publish a new IRW1 file only after all bytes are durable.
    /// hard_link is atomic and fails if the destination already exists.
    /// An interrupted import may leave the .importing staging file, but never
    /// replaces a source world or an existing target.
    pub fn save_new(&self, path: &Path) -> io::Result<()> {
        if path.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "IRW1 target exists",
            ));
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let staging = path.with_extension("importing");
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)
            .map_err(|err| {
                io::Error::new(
                    err.kind(),
                    format!(
                        "failed to create new world staging file {}: {err}",
                        staging.display()
                    ),
                )
            })?;
        let written = self.write_to(&mut file).and_then(|_| file.sync_all());
        drop(file);
        if let Err(err) = written {
            let _ = fs::remove_file(&staging);
            return Err(io::Error::new(
                err.kind(),
                format!(
                    "failed to write new world staging file {}: {err}",
                    staging.display()
                ),
            ));
        }
        let published = fs::hard_link(&staging, path).map_err(|err| {
            io::Error::new(
                err.kind(),
                format!(
                    "failed to publish new world file {} from {}: {err}",
                    path.display(),
                    staging.display()
                ),
            )
        });
        let _ = fs::remove_file(staging);
        published
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        let bytes = match fs::read(path) {
            Ok(v) => v,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::new()),
            Err(e) => return Err(e),
        };
        if bytes.len() < 8 {
            return Err(invalid("bad world header"));
        }
        let (mut world, offset) = match &bytes[..4] {
            value if value == MAGIC => (Self::new(), 8),
            value if value == NATURAL_MAGIC && bytes.len() >= 16 => {
                let seed = u64::from_be_bytes(bytes[4..12].try_into().unwrap());
                (Self::natural(seed), 16)
            }
            value if value == BIOME_MAGIC && bytes.len() >= 16 => {
                let seed = u64::from_be_bytes(bytes[4..12].try_into().unwrap());
                (Self::with_biomes(seed), 16)
            }
            value if value == EXPANDED_MAGIC && bytes.len() >= 17 => {
                let seed = u64::from_be_bytes(bytes[4..12].try_into().unwrap());
                let dimension = crate::dimensions::Dimension::from_protocol(bytes[12] as i8)
                    .ok_or_else(|| invalid("invalid IRW4 dimension id"))?;
                (Self::expanded(seed, dimension), 17)
            }
            _ => return Err(invalid("unknown IRW world format")),
        };
        let count = u32::from_be_bytes(bytes[offset - 4..offset].try_into().unwrap()) as usize;
        if count > MAX_EDITS {
            return Err(invalid("world edit count exceeds limit"));
        }
        let blocks_end = offset + count * 11;
        // Optional IRW TNT1 extension: older snapshots have no trailer.
        // Every stored fuse is relative to the next loaded world tick,
        // preventing accidental detonation after a server restart.
        if bytes.len() < blocks_end {
            return Err(invalid("world file length/count mismatch"));
        }
        let trailer = &bytes[blocks_end..];
        if !trailer.is_empty() {
            if trailer.len() < 6 || &trailer[..4] != b"TNT1" {
                return Err(invalid("unrecognized world entity trailer"));
            }
            let primed_count = u16::from_be_bytes(trailer[4..6].try_into().unwrap()) as usize;
            if primed_count > 4096 || trailer.len() != 6 + primed_count * 20 {
                return Err(invalid("invalid primed TNT trailer length"));
            }
            for entry in trailer[6..].as_chunks::<20>().0 {
                let entity_id = i32::from_be_bytes(entry[..4].try_into().unwrap());
                let x = i32::from_be_bytes(entry[4..8].try_into().unwrap());
                let y = i32::from_be_bytes(entry[8..12].try_into().unwrap());
                let z = i32::from_be_bytes(entry[12..16].try_into().unwrap());
                let remaining = u32::from_be_bytes(entry[16..20].try_into().unwrap());
                if entity_id < 10_000_001 || !in_bounds(x, y, z) || remaining == 0 {
                    return Err(invalid("invalid primed TNT entity"));
                }
                if world
                    .primed_tnt
                    .insert(
                        entity_id,
                        special::PrimedTnt {
                            entity_id,
                            pos: (x, y, z),
                            explode_at: u64::from(remaining),
                        },
                    )
                    .is_some()
                {
                    return Err(invalid("duplicate primed TNT entity"));
                }
                world.next_tnt_id = world.next_tnt_id.max(entity_id);
                // A reconnected 1.7.10 client needs to see surviving TNT.
                world.tnt_events.push(TntEvent::Spawn {
                    entity_id,
                    pos: (x, y, z),
                });
            }
        }
        for entry in bytes[offset..blocks_end].as_chunks::<11>().0 {
            let x = i32::from_be_bytes(entry[0..4].try_into().unwrap());
            let y = entry[4];
            let z = i32::from_be_bytes(entry[5..9].try_into().unwrap());
            let block = u16::from_be_bytes(entry[9..11].try_into().unwrap());
            if !in_bounds(x, y as i32, z) || block >> 4 > 255 {
                return Err(invalid("world has out-of-range block"));
            }
            if world.edits.insert((x, y, z), block).is_some() {
                return Err(invalid("duplicate world block"));
            }
            if matches!(block >> 4, 12 | 13 | 145) {
                world.pending_gravity.insert((x, y, z));
            }
            if matches!(block >> 4, 8..=11) {
                world.pending_fluid.insert((x, y, z));
            }
            if block >> 4 == 55 {
                world.pending_circuit.insert((x, y, z));
            }
            if matches!(block >> 4, 2 | 3 | 8 | 9) {
                world.register_weather_column((x, y, z));
            }
            if matches!(
                block >> 4,
                6 | 18 | 51 | 59 | 60 | 78 | 79 | 81 | 83 | 104 | 105 | 115 | 127 | 141 | 142 | 161
            ) {
                world.pending_ecology.insert((x, y, z));
            }
            if matches!(
                block >> 4,
                6 | 50
                    | 55
                    | 59
                    | 66
                    | 70
                    | 131
                    | 132
                    | 147
                    | 148
                    | 151
                    | 72
                    | 75
                    | 76
                    | 77
                    | 93
                    | 94
                    | 141
                    | 142
                    | 143
                    | 64
                    | 71
                    | 96
                    | 27
                    | 28
                    | 157
                    | 149
                    | 150
            ) {
                world.pending_support.insert((x, y, z));
            }
            if matches!(
                block >> 4,
                23 | 27 | 29 | 33 | 46 | 93 | 94 | 131 | 147 | 148 | 149 | 150 | 151 | 157 | 158
            ) {
                world.pending_devices.insert((x, y, z));
            }
            if (matches!(block >> 4, 70 | 72) && block & 1 != 0)
                || (matches!(block >> 4, 77 | 143) && block & 8 != 0)
            {
                world.pending_buttons.insert((x, y, z), 20);
            }
        }
        Ok(world)
    }
    fn write_to(&self, f: &mut impl Write) -> io::Result<()> {
        match self.generation {
            Generation::Flat => f.write_all(MAGIC)?,
            Generation::Natural(seed) => {
                f.write_all(NATURAL_MAGIC)?;
                f.write_all(&seed.to_be_bytes())?;
            }
            Generation::Biomes(seed) => {
                f.write_all(BIOME_MAGIC)?;
                f.write_all(&seed.to_be_bytes())?;
            }
            Generation::Expanded(seed, dim) => {
                f.write_all(EXPANDED_MAGIC)?;
                f.write_all(&seed.to_be_bytes())?;
                f.write_all(&[dim.protocol_id() as u8])?;
            }
        }
        f.write_all(&(self.edits.len() as u32).to_be_bytes())?;
        for (&(x, y, z), &block) in &self.edits {
            f.write_all(&x.to_be_bytes())?;
            f.write_all(&[y])?;
            f.write_all(&z.to_be_bytes())?;
            f.write_all(&block.to_be_bytes())?;
        }
        if !self.primed_tnt.is_empty() {
            if self.primed_tnt.len() > 4096 {
                return Err(invalid("primed TNT entity capacity exceeded"));
            }
            f.write_all(b"TNT1")?;
            f.write_all(&(self.primed_tnt.len() as u16).to_be_bytes())?;
            for tnt in self.primed_tnt.values() {
                let remaining = tnt.explode_at.saturating_sub(self.fluid_phase);
                if remaining > u64::from(u32::MAX) {
                    return Err(invalid("invalid primed TNT fuse"));
                }
                f.write_all(&tnt.entity_id.to_be_bytes())?;
                f.write_all(&tnt.pos.0.to_be_bytes())?;
                f.write_all(&tnt.pos.1.to_be_bytes())?;
                f.write_all(&tnt.pos.2.to_be_bytes())?;
                // A tick processes at most four detonations. If more than
                // four TNT share a deadline, the remaining entities are
                // overdue but still valid. Persist those as due next tick:
                // a zero fuse is invalid on disk and previously aborted
                // both world-tick writes and the final shutdown save.
                f.write_all(&(remaining.max(1) as u32).to_be_bytes())?;
            }
        }
        Ok(())
    }
    /// Encode the same bounded IRW snapshot used by save(), for journaled
    /// inventory/world transactions without an intermediate live disk write.
    pub fn encoded(&self) -> io::Result<Vec<u8>> {
        self.verify_save_session(None)?;
        if self.edits.len() > MAX_EDITS {
            return Err(invalid("world edits exceed IRW capacity"));
        }
        let mut bytes = Vec::new();
        self.write_to(&mut bytes)?;
        Ok(bytes)
    }
    pub fn save(&self, path: &Path) -> io::Result<()> {
        // Keep the session mutex held across staging and publication, so
        // another Rust write on the same world cannot interleave with us.
        // Re-verify immediately before renaming the finished IRW snapshot.
        let mut session_guard = if let Some((bound_path, session)) = &self.save_session {
            if bound_path != path {
                return Err(invalid("world save path differs from session-locked world"));
            }
            Some(
                session
                    .lock()
                    .map_err(|_| invalid("session lock mutex poisoned"))?,
            )
        } else {
            None
        };
        if let Some(guard) = session_guard.as_mut() {
            guard.verify()?;
            if path.with_extension("pending-txn.json").exists() {
                return Err(invalid(
                    "pending world redo requires recovery before IRW save",
                ));
            }
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension("tmp");
        let mut f = File::create(&temp)?;
        self.write_to(&mut f)?;
        f.sync_all()?;
        drop(f);
        if let Some(guard) = session_guard.as_mut() {
            guard.verify()?;
        }
        // Keep the original on filesystems that disallow atomic replacement.
        fs::rename(temp, path)
    }
    /// Serializes the pre-1.8 Section ID/metadata/light arrays in Y-major order.
    /// The first two sections are always present for the flat base ground; other
    /// sections are transmitted when edited. This avoids dropping high-altitude
    /// or underground edits on re-login or crossing a chunk boundary.
    pub fn chunk_data(&self, chunk_x: i32, chunk_z: i32) -> (u16, Vec<u8>) {
        let mut mask: u16 = if self.extended_identity().is_some() {
            0x01ff
        } else if self.is_natural() {
            0x00ff
        } else {
            (1 << 3) | (1 << 4)
        };
        let min_x = chunk_x * 16;
        let min_z = chunk_z * 16;
        // An x-range BTreeMap scan avoids walking every edit for every chunk.
        for (&(_, y, z), _) in self
            .edits
            .range((min_x, 0, i32::MIN)..(min_x + 16, 0, i32::MIN))
        {
            if (min_z..min_z + 16).contains(&z) {
                mask |= 1u16 << (y / 16);
            }
        }
        let sections: Vec<i32> = (0..16).filter(|y| (mask & (1 << y)) != 0).collect();
        let mut bytes = Vec::with_capacity(sections.len() * 10240 + 256);
        // Emit IDs and collect metadata in one pass. Generated blocks can
        // evaluate expensive biome/noise functions, so reading them a second
        // time for the nibble array doubles that part of chunk snapshot work.
        // Keep Protocol 5's wire layout unchanged: all IDs, then all metadata.
        let mut metadata = Vec::with_capacity(sections.len() * 2048);
        for &section in &sections {
            for y in 0..16 {
                for z in 0..16 {
                    for x in (0..16).step_by(2) {
                        let a = self.get(chunk_x * 16 + x, section * 16 + y, chunk_z * 16 + z);
                        let b = self.get(chunk_x * 16 + x + 1, section * 16 + y, chunk_z * 16 + z);
                        bytes.push((a >> 4) as u8);
                        bytes.push((b >> 4) as u8);
                        metadata.push((a as u8 & 15) | ((b as u8 & 15) << 4));
                    }
                }
            }
        }
        bytes.extend_from_slice(&metadata);
        let (block_light, sky_light) = self.light_arrays(chunk_x, chunk_z);
        for channel in [&block_light, &sky_light] {
            for &section in &sections {
                for y in 0..16 {
                    for z in 0..16 {
                        for x in (0..16).step_by(2) {
                            let offset = ((section * 16 + y) * 256 + z * 16 + x) as usize;
                            bytes.push(channel[offset] | (channel[offset + 1] << 4));
                        }
                    }
                }
            }
        }
        for z in 0..16 {
            for x in 0..16 {
                let kind = match self.generation {
                    Generation::Biomes(seed) => {
                        crate::terrain::biome(seed, chunk_x * 16 + x, chunk_z * 16 + z)
                    }
                    Generation::Expanded(seed, dim) => crate::worldgen::biome_for_dimension(
                        seed,
                        dim,
                        chunk_x * 16 + x,
                        chunk_z * 16 + z,
                    ),
                    _ => 1,
                };
                bytes.push(kind);
            }
        }
        (mask, bytes)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn e46_fixture(label: &str, divergent_anvil: bool) -> (PathBuf, World) {
        let base = std::env::temp_dir().join(format!(
            "industrial-e46-world-{}-{}-{:?}",
            label,
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let root = base.join("anvil");
        let mut exported = World::new();
        if divergent_anvil {
            exported.import_block(2, 90, 3, 5 << 4).unwrap();
        }
        crate::anvil_export::export(&exported, &root).unwrap();
        let path = root.join("flat.irw");
        let mut world = World::new();
        world.save_new(&path).unwrap();
        let lock = Arc::new(Mutex::new(
            crate::session_lock::WorldSessionLock::acquire(&root).unwrap(),
        ));
        world.bind_save_session(&path, lock).unwrap();
        (base, world)
    }

    #[test]
    fn e46_bound_world_block_write_updates_anvil_and_irw_together() {
        let (base, mut world) = e46_fixture("durable", false);
        let root = base.join("anvil");
        let path = root.join("flat.irw");
        assert!(world
            .update_and_flush_existing_anvil(&path, 2, 90, 3, (5 << 4) | 7)
            .unwrap());
        assert!(!world
            .update_and_flush_existing_anvil(&path, 2, 90, 3, (5 << 4) | 7)
            .unwrap());
        assert!(world
            .update_and_flush_existing_anvil(&path, 2, 90, 3, (5 << 4) | 3)
            .unwrap());
        assert_eq!(world.get(2, 90, 3), (5 << 4) | 3);
        assert_eq!(World::load(&path).unwrap().get(2, 90, 3), (5 << 4) | 3);
        let mut region =
            fastanvil::Region::from_stream(File::open(root.join("region/r.0.0.mca")).unwrap())
                .unwrap();
        let chunk =
            crate::legacy_nbt::parse_chunk(&region.read_chunk(0, 0).unwrap().unwrap(), 0, 0)
                .unwrap();
        assert_eq!(
            chunk
                .level
                .sections
                .iter()
                .find(|s| s.y == 5)
                .unwrap()
                .block_at((90 % 16) * 256 + 3 * 16 + 2),
            Some((5, 3))
        );
        assert!(!path.with_extension("pending-txn.json").exists());
        drop(region);
        drop(world);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn e46_refuses_divergent_anvil_without_modifying_world() {
        let (base, mut world) = e46_fixture("anvil-drift", true);
        let root = base.join("anvil");
        let irw_path = root.join("flat.irw");
        let region_path = root.join("region/r.0.0.mca");
        let irw_before = fs::read(&irw_path).unwrap();
        let region_before = fs::read(&region_path).unwrap();
        assert!(world
            .update_and_flush_existing_anvil(&irw_path, 2, 90, 3, (1 << 4) | 2)
            .is_err());
        assert_eq!(world.get(2, 90, 3), 0);
        assert_eq!(fs::read(&irw_path).unwrap(), irw_before);
        assert_eq!(fs::read(&region_path).unwrap(), region_before);
        drop(world);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn e46_refuses_stale_irw_and_pending_redo_without_publishing_anvil() {
        let (base, mut world) = e46_fixture("failclosed", false);
        let root = base.join("anvil");
        let irw_path = root.join("flat.irw");
        let region_path = root.join("region/r.0.0.mca");
        let irw_before = fs::read(&irw_path).unwrap();
        let region_before = fs::read(&region_path).unwrap();
        fs::write(&irw_path, b"stale file").unwrap();
        assert!(world
            .update_and_flush_existing_anvil(&irw_path, 2, 90, 3, 5 << 4)
            .is_err());
        assert_eq!(fs::read(&region_path).unwrap(), region_before);
        fs::write(&irw_path, &irw_before).unwrap();
        fs::write(irw_path.with_extension("pending-txn.json"), b"pending").unwrap();
        assert!(world.save(&irw_path).is_err());
        assert!(world.encoded().is_err());
        assert!(world
            .update_and_flush_existing_anvil(&irw_path, 2, 90, 3, 5 << 4)
            .is_err());
        assert_eq!(fs::read(&irw_path).unwrap(), irw_before);
        assert_eq!(fs::read(&region_path).unwrap(), region_before);
        assert_eq!(world.get(2, 90, 3), 0);
        drop(world);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn e46_multi_block_flush_crosses_chunks_and_region_boundaries_atomically() {
        let (base, mut world) = e46_fixture("multi-region", false);
        let root = base.join("anvil");
        let path = root.join("flat.irw");
        let changes = [
            (0, 90, 0, (5 << 4) | 7),
            (1, 90, 0, (5 << 4) | 3),
            (16, 90, 0, 1 << 4),
            (-1, 90, 0, (17 << 4) | 2),
            (-1, 90, -1, 4 << 4),
        ];
        assert!(world
            .update_many_and_flush_existing_anvil(&path, &changes)
            .unwrap());
        for &(x, y, z, block) in &changes {
            assert_eq!(world.get(x, y, z), block);
            assert_eq!(World::load(&path).unwrap().get(x, y, z), block);
            let region_path = root.join("region").join(format!(
                "r.{}.{}.mca",
                x.div_euclid(512),
                z.div_euclid(512)
            ));
            let mut region =
                fastanvil::Region::from_stream(File::open(&region_path).unwrap()).unwrap();
            let nbt = region
                .read_chunk(
                    x.div_euclid(16).rem_euclid(32) as usize,
                    z.div_euclid(16).rem_euclid(32) as usize,
                )
                .unwrap()
                .unwrap();
            let chunk =
                crate::legacy_nbt::parse_chunk(&nbt, x.div_euclid(16), z.div_euclid(16)).unwrap();
            let index = (y as usize % 16) * 256
                + z.rem_euclid(16) as usize * 16
                + x.rem_euclid(16) as usize;
            let (id, metadata) = chunk
                .level
                .sections
                .iter()
                .find(|section| section.y == (y / 16) as i8)
                .unwrap()
                .block_at(index)
                .unwrap();
            assert_eq!((id << 4) | u16::from(metadata), block);
        }
        assert!(!path.with_extension("pending-txn.json").exists());
        // All no-op edits remain a no-op, not a redundant redo write.
        assert!(!world
            .update_many_and_flush_existing_anvil(&path, &changes)
            .unwrap());
        drop(world);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn e46_multi_block_divergent_later_chunk_rolls_back_all_participants() {
        let (base, mut world) = e46_fixture("multi-stale", true);
        let root = base.join("anvil");
        let irw = root.join("flat.irw");
        let regions = [
            root.join("region/r.0.0.mca"),
            root.join("region/r.-1.0.mca"),
        ];
        let original_irw = fs::read(&irw).unwrap();
        let before: Vec<_> = regions.iter().map(|p| fs::read(p).unwrap()).collect();
        assert!(world
            .update_many_and_flush_existing_anvil(&irw, &[(-1, 90, 0, 1 << 4), (2, 90, 3, 4 << 4)],)
            .is_err());
        for &(x, y, z) in &[(-1, 90, 0), (2, 90, 3)] {
            assert_eq!(world.get(x, y, z), 0);
        }
        assert_eq!(fs::read(&irw).unwrap(), original_irw);
        for (path, expected) in regions.iter().zip(before.iter()) {
            assert_eq!(fs::read(path).unwrap(), *expected);
        }
        assert!(!irw.with_extension("pending-txn.json").exists());
        drop(world);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn e46_multi_block_rejects_duplicates_invalid_and_missing_chunks() {
        let (base, mut world) = e46_fixture("multi-invalid", false);
        let root = base.join("anvil");
        let irw = root.join("flat.irw");
        let region = root.join("region/r.0.0.mca");
        let before_irw = fs::read(&irw).unwrap();
        let before_region = fs::read(&region).unwrap();
        assert!(world
            .update_many_and_flush_existing_anvil(&irw, &[])
            .is_err());
        assert!(world
            .update_many_and_flush_existing_anvil(&irw, &[(0, 90, 0, 1 << 4), (0, 90, 0, 2 << 4)])
            .is_err());
        assert!(world
            .update_many_and_flush_existing_anvil(&irw, &[(0, 256, 0, 1 << 4)])
            .is_err());
        assert!(world
            .update_many_and_flush_existing_anvil(&irw, &[(0, 90, 0, 4096)])
            .is_err());
        // Anvil snapshot exports spawn chunks only; x=512 is absent.
        assert!(
            world
                .update_many_and_flush_existing_anvil(
                    &irw,
                    &[(0, 90, 0, 1 << 4), (512, 90, 0, 1 << 4)],
                )
                .is_err()
        );
        assert_eq!(fs::read(&irw).unwrap(), before_irw);
        assert_eq!(fs::read(&region).unwrap(), before_region);
        assert_eq!(world.get(0, 90, 0), 0);
        drop(world);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn e46_multi_block_changes_in_same_chunk_preserve_other_blocks() {
        let (base, mut world) = e46_fixture("same-chunk", false);
        let root = base.join("anvil");
        let irw = root.join("flat.irw");
        let patches = [(2, 90, 3, (5 << 4) | 1), (3, 90, 3, (5 << 4) | 2)];
        assert!(world
            .update_many_and_flush_existing_anvil(&irw, &patches)
            .unwrap());
        assert_eq!(world.get(2, 90, 3), (5 << 4) | 1);
        assert_eq!(world.get(3, 90, 3), (5 << 4) | 2);
        assert_eq!(World::load(&irw).unwrap().get(3, 90, 3), (5 << 4) | 2);
        drop(world);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn e46_creates_missing_chunk_inside_existing_region_and_keeps_original() {
        let (base, mut world) = e46_fixture("missing-in-region", false);
        let root = base.join("anvil");
        let path = root.join("flat.irw");
        let region_path = root.join("region/r.0.0.mca");
        let before_chunk = {
            let mut region =
                fastanvil::Region::from_stream(File::open(&region_path).unwrap()).unwrap();
            assert!(region.read_chunk(4, 0).unwrap().is_none());
            region.read_chunk(0, 0).unwrap().unwrap()
        };
        world
            .update_many_and_create_missing_anvil_chunk(
                &path,
                &[(64, 90, 2, (5 << 4) | 7), (65, 90, 2, 1 << 4)],
            )
            .unwrap();
        assert_eq!(World::load(&path).unwrap().get(64, 90, 2), (5 << 4) | 7);
        assert_eq!(world.get(65, 90, 2), 1 << 4);
        let mut region = fastanvil::Region::from_stream(File::open(&region_path).unwrap()).unwrap();
        assert_eq!(region.read_chunk(0, 0).unwrap().unwrap(), before_chunk);
        let chunk =
            crate::legacy_nbt::parse_chunk(&region.read_chunk(4, 0).unwrap().unwrap(), 4, 0)
                .unwrap();
        let section = chunk
            .level
            .sections
            .iter()
            .find(|section| section.y == 5)
            .unwrap();
        assert_eq!(section.block_at((90 % 16) * 256 + 2 * 16), Some((5, 7)));
        assert_eq!(section.block_at((90 % 16) * 256 + 2 * 16 + 1), Some((1, 0)));
        assert_eq!(
            chunk.level.extra.get("LightPopulated"),
            Some(&fastnbt::Value::Byte(0))
        );
        assert!(!path.with_extension("pending-txn.json").exists());
        drop(region);
        drop(world);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn e46_creates_whole_missing_region_with_irw_same_generation() {
        let (base, mut world) = e46_fixture("new-region", false);
        let root = base.join("anvil");
        let path = root.join("flat.irw");
        let new_region = root.join("region/r.1.-1.mca");
        assert!(!new_region.exists());
        world
            .update_many_and_create_missing_anvil_chunk(&path, &[(512, 90, -512, 4 << 4)])
            .unwrap();
        assert_eq!(World::load(&path).unwrap().get(512, 90, -512), 4 << 4);
        let mut region = fastanvil::Region::from_stream(File::open(&new_region).unwrap()).unwrap();
        let chunk =
            crate::legacy_nbt::parse_chunk(&region.read_chunk(0, 0).unwrap().unwrap(), 32, -32)
                .unwrap();
        let block = chunk
            .level
            .sections
            .iter()
            .find(|section| section.y == 5)
            .unwrap();
        assert_eq!(block.block_at((90 % 16) * 256), Some((4, 0)));
        assert!(region.read_chunk(1, 0).unwrap().is_none());
        drop(region);
        drop(world);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn e46_missing_chunk_rejects_collisions_and_bad_inputs_without_writes() {
        let (base, mut world) = e46_fixture("new-guard", false);
        let root = base.join("anvil");
        let path = root.join("flat.irw");
        let region_path = root.join("region/r.0.0.mca");
        let old_irw = fs::read(&path).unwrap();
        let old_region = fs::read(&region_path).unwrap();
        // Already present in the exported spawn region: must NEVER replace.
        assert!(world
            .update_many_and_create_missing_anvil_chunk(&path, &[(0, 90, 0, 1 << 4)])
            .is_err());
        assert!(world
            .update_many_and_create_missing_anvil_chunk(
                &path,
                &[(64, 90, 2, 5 << 4), (64, 90, 2, 4 << 4)],
            )
            .is_err());
        assert!(world
            .update_many_and_create_missing_anvil_chunk(
                &path,
                &[(64, 90, 2, 5 << 4), (80, 90, 2, 4 << 4)],
            )
            .is_err());
        assert!(world
            .update_many_and_create_missing_anvil_chunk(&path, &[(64, 256, 2, 5 << 4)])
            .is_err());
        assert!(world
            .update_many_and_create_missing_anvil_chunk(&path, &[(64, 90, 2, 4096)])
            .is_err());
        assert_eq!(fs::read(&path).unwrap(), old_irw);
        assert_eq!(fs::read(&region_path).unwrap(), old_region);
        assert_eq!(world.get(64, 90, 2), 0);
        drop(world);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn e46_new_chunk_rejects_stale_irw_and_previously_edited_column() {
        let (base, mut world) = e46_fixture("missing-stale", false);
        let root = base.join("anvil");
        let path = root.join("flat.irw");
        let old_irw = fs::read(&path).unwrap();
        fs::write(&path, b"not the bound IRW generation").unwrap();
        assert!(world
            .update_many_and_create_missing_anvil_chunk(&path, &[(64, 90, 2, 4 << 4)])
            .is_err());
        assert!(!root.join("region/r.1.0.mca").exists());
        fs::write(&path, &old_irw).unwrap();
        // This IRW already has edits outside any corresponding Anvil chunk.
        world.import_block(64, 90, 2, 1 << 4).unwrap();
        assert!(world
            .update_many_and_create_missing_anvil_chunk(&path, &[(65, 90, 2, 4 << 4)])
            .is_err());
        assert_eq!(fs::read(&path).unwrap(), old_irw);
        drop(world);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn e46_does_not_implicitly_bind_or_create_anvil_storage() {
        let mut detached = World::new();
        assert!(detached
            .update_and_flush_existing_anvil(Path::new("nonexistent-e46.irw"), 0, 64, 0, 1 << 4,)
            .is_err());
        assert_eq!(detached.get(0, 64, 0), 0);
    }

    #[test]
    fn chunk_ids_and_metadata_match_direct_world_samples() {
        // The optimized single-pass packing must retain section order and
        // low/high nibble order, including sparse high sections and negatives.
        let mut world = World::new();
        for (x, y, z, block) in [
            (-16, 70, -1, (35 << 4) | 14),
            (-15, 200, -1, (1 << 4) | 3),
            (-1, 64, -16, (35 << 4) | 5),
            (0, 150, 0, (35 << 4) | 7),
        ] {
            world.import_block(x, y, z, block).unwrap();
        }
        for (cx, cz) in [(-1, -1), (0, 0), (1, -1)] {
            let (mask, packet) = world.chunk_data(cx, cz);
            let sections: Vec<i32> = (0..16).filter(|y| mask & (1 << y) != 0).collect();
            let mut expected_ids = Vec::new();
            let mut expected_metadata = Vec::new();
            for &section in &sections {
                for y in 0..16 {
                    for z in 0..16 {
                        for x in 0..16 {
                            let state = world.get(cx * 16 + x, section * 16 + y, cz * 16 + z);
                            expected_ids.push((state >> 4) as u8);
                        }
                    }
                }
            }
            for &section in &sections {
                for y in 0..16 {
                    for z in 0..16 {
                        for x in (0..16).step_by(2) {
                            let a = world.get(cx * 16 + x, section * 16 + y, cz * 16 + z);
                            let b = world.get(cx * 16 + x + 1, section * 16 + y, cz * 16 + z);
                            expected_metadata.push((a as u8 & 15) | ((b as u8 & 15) << 4));
                        }
                    }
                }
            }
            let ids = expected_ids.len();
            let metadata = expected_metadata.len();
            assert_eq!(&packet[..ids], expected_ids.as_slice());
            assert_eq!(&packet[ids..ids + metadata], expected_metadata.as_slice());
            assert_eq!(packet.len(), sections.len() * 10240 + 256);
        }
    }

    #[test]
    fn live_village_doors_follow_world_edits_and_ignore_other_dimensions() {
        let seed = 42;
        let mut world = World::expanded(seed, crate::dimensions::Dimension::Overworld);
        let mut negative_checked = false;
        let mut positive_checked = false;
        for gx in -8..=8 {
            for gz in -8..=8 {
                let x = gx * 192 + 48;
                let z = gz * 192 + 48;
                let negative = gx < 0 && gz < 0 && !negative_checked;
                let positive = gx > 0 && gz > 0 && !positive_checked;
                if !negative && !positive {
                    continue;
                }
                let Some(doors) = world.live_village_house_doors(x, z) else {
                    continue;
                };
                let [first, second] = doors;
                let [dx, dy, dz] = first.expect("generated first house door");
                assert!(second.is_some(), "generated second house door");
                let lower = world.get(dx, dy, dz);
                let upper = world.get(dx, dy + 1, dz);

                world.import_block(dx, dy, dz, 0).unwrap();
                assert_eq!(world.live_village_house_doors(x, z), Some([None, second]));
                world.import_block(dx, dy, dz, lower).unwrap();

                // Opening a door and changing its upper-half hinge metadata
                // must not make an otherwise intact wooden door disappear.
                world.import_block(dx, dy, dz, lower ^ 4).unwrap();
                world.import_block(dx, dy + 1, dz, upper ^ 1).unwrap();
                assert_eq!(world.live_village_house_doors(x, z), Some(doors));

                world.import_block(dx, dy + 1, dz, 71 << 4 | 8).unwrap();
                assert_eq!(world.live_village_house_doors(x, z), Some([None, second]));
                world.import_block(dx, dy, dz, lower).unwrap();
                world.import_block(dx, dy + 1, dz, upper).unwrap();
                assert_eq!(world.live_village_house_doors(x, z), Some(doors));

                // Both halves may survive a terrain edit even though a door
                // without a solid foundation must cease being a village goal.
                let support = world.get(dx, dy - 1, dz);
                for unsupported in [0, 9 << 4, 50 << 4] {
                    world.import_block(dx, dy - 1, dz, unsupported).unwrap();
                    assert_eq!(world.live_village_house_doors(x, z), Some([None, second]));
                }
                world.import_block(dx, dy - 1, dz, support).unwrap();
                assert_eq!(world.live_village_house_doors(x, z), Some(doors));
                if negative {
                    negative_checked = true;
                } else {
                    positive_checked = true;
                }
            }
        }
        assert!(negative_checked && positive_checked);
        assert_eq!(world.live_village_house_doors(i32::MIN, 0), None);
        assert_eq!(World::new().live_village_house_doors(0, 0), None);
        assert_eq!(
            World::expanded(seed, crate::dimensions::Dimension::Nether)
                .live_village_house_doors(0, 0),
            None
        );
    }

    #[test]
    fn player_built_village_doors_follow_edits_and_survive_restart() {
        let seed = 42;
        let mut world = World::expanded(seed, crate::dimensions::Dimension::Overworld);
        let (cx, base, cz) = (-8..=8)
            .flat_map(|gx| (-8..=8).map(move |gz| (gx * 192 + 48, gz * 192 + 48)))
            .find_map(|(x, z)| crate::worldgen::village_center(seed, x, z))
            .expect("seed 42 has a generated village");
        let generated = world.live_village_house_doors(cx, cz).unwrap();
        let placed = [cx, base + 1, cz + 8];
        let outside = [cx + 32, base + 1, cz];
        let mut expected: BTreeSet<[i32; 3]> = generated.into_iter().flatten().collect();
        world
            .import_block(placed[0], placed[1] - 1, placed[2], 1 << 4)
            .unwrap();
        assert_eq!(
            world.village_doors_including_player_edits(cx, cz),
            Some(expected.iter().copied().collect())
        );
        // A lone lower half is never a valid door.
        world
            .import_block(placed[0], placed[1], placed[2], 64 << 4)
            .unwrap();
        assert!(!world
            .village_doors_including_player_edits(cx, cz)
            .unwrap()
            .contains(&placed));
        world
            .import_block(placed[0], placed[1] + 1, placed[2], 64 << 4 | 8)
            .unwrap();
        expected.insert(placed);
        assert_eq!(
            world.village_doors_including_player_edits(cx, cz),
            Some(expected.iter().copied().collect())
        );
        // Upper-half replacement, including an iron door, invalidates it.
        world
            .import_block(placed[0], placed[1] + 1, placed[2], 71 << 4 | 8)
            .unwrap();
        assert!(!world
            .village_doors_including_player_edits(cx, cz)
            .unwrap()
            .contains(&placed));
        world
            .import_block(placed[0], placed[1] + 1, placed[2], 64 << 4 | 9)
            .unwrap();
        world
            .import_block(outside[0], outside[1], outside[2], 64 << 4)
            .unwrap();
        world
            .import_block(outside[0], outside[1] + 1, outside[2], 64 << 4 | 8)
            .unwrap();
        assert!(!world
            .village_doors_including_player_edits(cx, cz)
            .unwrap()
            .contains(&outside));
        // Removing a generated door must also remove it from the combined set.
        let removed = generated[0].unwrap();
        world
            .import_block(removed[0], removed[1], removed[2], 0)
            .unwrap();
        expected.remove(&removed);
        assert_eq!(
            world.village_doors_including_player_edits(cx, cz),
            Some(expected.iter().copied().collect())
        );
        let path = std::env::temp_dir().join(format!(
            "ir1710-player-village-doors-{}.irw",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        world.save(&path).unwrap();
        let restored = World::load(&path).unwrap();
        assert_eq!(
            restored.village_doors_including_player_edits(cx, cz),
            Some(expected.into_iter().collect())
        );
        assert_eq!(
            restored.village_doors_including_player_edits(cx + 32, cz),
            None
        );
        assert_eq!(
            restored.village_doors_including_player_edits(i32::MIN, 0),
            None
        );
        assert_eq!(
            World::expanded(seed, crate::dimensions::Dimension::Nether)
                .village_doors_including_player_edits(cx, cz),
            None
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn village_core_prioritizes_nearby_door_after_many_distant_edits() {
        let seed = 42;
        let mut world = World::expanded(seed, crate::dimensions::Dimension::Overworld);
        let (cx, base, cz) = (-8..=8)
            .flat_map(|gx| (-8..=8).map(move |gz| (gx * 192 + 48, gz * 192 + 48)))
            .find_map(|(x, z)| crate::worldgen::village_center(seed, x, z))
            .expect("seed 42 has a generated village");
        let generated = world.live_village_house_doors(cx, cz).unwrap();

        // More than 128 valid but distant doors sort before the nearby door
        // in the edit map. They must not consume the entire live door budget.
        for x_offset in -30..=-25 {
            for z_offset in -20..=20 {
                let x = cx + x_offset;
                let z = cz + z_offset;
                world.import_block(x, 199, z, 1 << 4).unwrap();
                world.import_block(x, 200, z, 64 << 4).unwrap();
                world.import_block(x, 201, z, 64 << 4 | 8).unwrap();
            }
        }
        let near = [cx + 2, base + 1, cz + 10];
        world
            .import_block(near[0], near[1] - 1, near[2], 1 << 4)
            .unwrap();
        world
            .import_block(near[0], near[1], near[2], 64 << 4)
            .unwrap();
        world
            .import_block(near[0], near[1] + 1, near[2], 64 << 4 | 8)
            .unwrap();

        let doors = world.village_doors_including_player_edits(cx, cz).unwrap();
        assert_eq!(doors.len(), 128);
        assert!(doors.contains(&near), "nearby door must not be crowded out");
        for door in generated.into_iter().flatten() {
            assert!(
                doors.contains(&door),
                "generated village door must be retained"
            );
        }
        assert!(!doors.contains(&[cx - 30, 200, cz - 20]));

        world
            .import_block(near[0], near[1] + 1, near[2], 0)
            .unwrap();
        assert!(!world
            .village_doors_including_player_edits(cx, cz)
            .unwrap()
            .contains(&near));
        world
            .import_block(near[0], near[1] + 1, near[2], 64 << 4 | 8)
            .unwrap();
        assert!(world
            .village_doors_including_player_edits(cx, cz)
            .unwrap()
            .contains(&near));
    }

    #[test]
    fn village_annex_uses_only_connected_live_doors_and_survives_restart() {
        let seed = 42;
        let mut world = World::expanded(seed, crate::dimensions::Dimension::Overworld);
        let (cx, _, cz) = (-8..=8)
            .flat_map(|gx| (-8..=8).map(move |gz| (gx * 192 + 48, gz * 192 + 48)))
            .find_map(|(x, z)| crate::worldgen::village_center(seed, x, z))
            .expect("seed 42 has a generated village");
        let anchor = world.live_village_house_doors(cx, cz).unwrap()[1].unwrap();
        let annex = [anchor[0] + 15, anchor[1], anchor[2]];
        let chained = [anchor[0] + 30, anchor[1], anchor[2]];
        let disconnected = [cx - 45, anchor[1], cz + 40];
        // An upper-floor door is horizontally adjacent to the annex but
        // cannot form a village link across a sixteen-block vertical gap.
        let elevated = [annex[0], annex[1] + 16, annex[2] + 1];
        assert_eq!(
            world.village_doors_with_bounded_annex(chained[0], chained[2]),
            None
        );
        for door in [annex, chained, disconnected, elevated] {
            world
                .import_block(door[0], door[1] - 1, door[2], 1 << 4)
                .unwrap();
            world
                .import_block(door[0], door[1], door[2], 64 << 4 | 2)
                .unwrap();
            world
                .import_block(door[0], door[1] + 1, door[2], 64 << 4 | 8)
                .unwrap();
        }
        let active = world
            .village_doors_with_bounded_annex(chained[0], chained[2])
            .unwrap();
        assert!(active.contains(&annex));
        assert!(active.contains(&chained));
        assert!(!active.contains(&disconnected));
        assert!(!active.contains(&elevated));
        assert!(!world
            .village_doors_including_player_edits(cx, cz)
            .unwrap()
            .contains(&annex));
        assert_eq!(world.village_doors_with_bounded_annex(cx + 48, cz), None);

        let path =
            std::env::temp_dir().join(format!("ir1710-village-annex-{}.irw", std::process::id()));
        let _ = fs::remove_file(&path);
        world.save(&path).unwrap();
        let mut restored = World::load(&path).unwrap();
        let restored_doors = restored
            .village_doors_with_bounded_annex(chained[0], chained[2])
            .unwrap();
        assert!(restored_doors.contains(&chained));
        assert!(!restored_doors.contains(&elevated));
        // The distant annex door must disappear when its only bridge breaks,
        // even though its two halves remain intact in the live world.
        restored
            .import_block(annex[0], annex[1] + 1, annex[2], 0)
            .unwrap();
        assert_eq!(
            restored.village_doors_with_bounded_annex(chained[0], chained[2]),
            None
        );
        assert_eq!(
            restored.village_doors_with_bounded_annex(annex[0], annex[2]),
            None
        );
        assert_eq!(restored.village_doors_with_bounded_annex(i32::MIN, 0), None);
        assert_eq!(
            World::expanded(seed, crate::dimensions::Dimension::Nether)
                .village_doors_with_bounded_annex(cx, cz),
            None
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn village_annex_across_chunk_boundary_expires_and_recovers_after_restart() {
        let seed = 42;
        for negative in [true, false] {
            let mut world = World::expanded(seed, crate::dimensions::Dimension::Overworld);
            let (cx, _, cz) = (-8..=8)
                .flat_map(|gx| (-8..=8).map(move |gz| (gx * 192 + 48, gz * 192 + 48)))
                .filter(|&(x, z)| (x < 0 && z < 0) == negative)
                .find_map(|(x, z)| crate::worldgen::village_center(seed, x, z))
                .expect("seed 42 has villages in both signed quadrants");
            let anchor = world.live_village_house_doors(cx, cz).unwrap()[1].unwrap();
            // The bridge is in the core footprint, but the next door is in
            // the adjacent chunk and outside the generated village footprint.
            let bridge = [anchor[0] + 14, anchor[1], anchor[2]];
            let remote = [anchor[0] + 30, anchor[1], anchor[2]];
            assert_eq!(bridge[0].rem_euclid(16), 15);
            assert_eq!(remote[0].rem_euclid(16), 15);
            assert_eq!(remote[0].div_euclid(16), bridge[0].div_euclid(16) + 1);

            for door in [bridge, remote] {
                world
                    .import_block(door[0], door[1] - 1, door[2], 1 << 4)
                    .unwrap();
                world
                    .import_block(door[0], door[1], door[2], 64 << 4)
                    .unwrap();
                world
                    .import_block(door[0], door[1] + 1, door[2], 64 << 4 | 8)
                    .unwrap();
            }
            let active = world
                .village_doors_with_bounded_annex(remote[0], remote[2])
                .expect("annex should cross the chunk boundary");
            assert!(active.contains(&bridge));
            assert!(active.contains(&remote));

            // The remote door is intact, but losing its only supporting
            // bridge must immediately disconnect it from the generated village.
            world
                .import_block(bridge[0], bridge[1] - 1, bridge[2], 9 << 4)
                .unwrap();
            assert_eq!(
                world.village_doors_with_bounded_annex(remote[0], remote[2]),
                None
            );
            world
                .import_block(bridge[0], bridge[1] - 1, bridge[2], 1 << 4)
                .unwrap();
            assert!(world
                .village_doors_with_bounded_annex(remote[0], remote[2])
                .unwrap()
                .contains(&remote));

            let path = std::env::temp_dir().join(format!(
                "ir1710-annex-chunk-{}-{}.irw",
                std::process::id(),
                if negative { "negative" } else { "positive" }
            ));
            let _ = fs::remove_file(&path);
            world.save(&path).unwrap();
            let mut restored = World::load(&path).unwrap();
            assert!(restored
                .village_doors_with_bounded_annex(remote[0], remote[2])
                .unwrap()
                .contains(&remote));
            restored
                .import_block(remote[0], remote[1] + 1, remote[2], 0)
                .unwrap();
            assert_eq!(
                restored.village_doors_with_bounded_annex(remote[0], remote[2]),
                None
            );
            restored
                .import_block(remote[0], remote[1] + 1, remote[2], 64 << 4 | 8)
                .unwrap();
            assert!(restored
                .village_doors_with_bounded_annex(remote[0], remote[2])
                .unwrap()
                .contains(&remote));
            fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn village_annex_prioritizes_connected_door_after_many_distant_edits() {
        let seed = 42;
        let mut world = World::expanded(seed, crate::dimensions::Dimension::Overworld);
        let (cx, _, cz) = (-8..=8)
            .flat_map(|gx| (-8..=8).map(move |gz| (gx * 192 + 48, gz * 192 + 48)))
            .find_map(|(x, z)| crate::worldgen::village_center(seed, x, z))
            .expect("seed 42 has a generated village");
        let anchor = world.live_village_house_doors(cx, cz).unwrap()[1].unwrap();
        let annex = [anchor[0] + 15, anchor[1], anchor[2]];

        // These 512 intact but vertically disconnected doors sort before
        // the annex in the edit map. They used to exhaust its candidate cap.
        for x in -47..=-32 {
            for z in -42..=-27 {
                for y in [180, 200] {
                    world.import_block(cx + x, y - 1, cz + z, 1 << 4).unwrap();
                    world.import_block(cx + x, y, cz + z, 64 << 4).unwrap();
                    world
                        .import_block(cx + x, y + 1, cz + z, 64 << 4 | 8)
                        .unwrap();
                }
            }
        }
        world
            .import_block(annex[0], annex[1] - 1, annex[2], 1 << 4)
            .unwrap();
        world
            .import_block(annex[0], annex[1], annex[2], 64 << 4 | 2)
            .unwrap();
        world
            .import_block(annex[0], annex[1] + 1, annex[2], 64 << 4 | 8)
            .unwrap();
        let doors = world
            .village_doors_with_bounded_annex(annex[0], annex[2])
            .expect("connected annex remains reachable despite distant edits");
        assert!(doors.contains(&annex));
        assert!(!doors.contains(&[cx - 47, 180, cz - 42]));
    }

    #[test]
    fn village_annex_prioritizes_reachable_bridge_over_nearby_upper_floor_doors() {
        let seed = 42;
        let mut world = World::expanded(seed, crate::dimensions::Dimension::Overworld);
        let (cx, _, cz) = (-8..=8)
            .flat_map(|gx| (-8..=8).map(move |gz| (gx * 192 + 48, gz * 192 + 48)))
            .find_map(|(x, z)| crate::worldgen::village_center(seed, x, z))
            .expect("seed 42 has a generated village");
        // A player-built core door can bridge into the bounded annex.
        let core = [cx + 30, 200, cz];
        let bridge = [cx + 46, 200, cz];
        for door in [core, bridge] {
            world
                .import_block(door[0], door[1] - 1, door[2], 1 << 4)
                .unwrap();
            world
                .import_block(door[0], door[1], door[2], 64 << 4)
                .unwrap();
            world
                .import_block(door[0], door[1] + 1, door[2], 64 << 4 | 8)
                .unwrap();
        }
        // More than 512 doors are closer in 3D to the core than the
        // bridge, but every one is at least five blocks higher.
        // Their foundations and upper halves are all intact.
        let mut distractors = 0;
        'fill: for y in [205, 208] {
            for dx in 32..=45 {
                for dz in -15..=15 {
                    let x_distance = i64::from(dx - 30);
                    let z_distance = i64::from(dz);
                    let y_distance = i64::from(y - 200);
                    if x_distance * x_distance + y_distance * y_distance + z_distance * z_distance
                        >= 16 * 16
                    {
                        continue;
                    }
                    let door = [cx + dx, y, cz + dz];
                    world
                        .import_block(door[0], door[1] - 1, door[2], 1 << 4)
                        .unwrap();
                    world
                        .import_block(door[0], door[1], door[2], 64 << 4)
                        .unwrap();
                    world
                        .import_block(door[0], door[1] + 1, door[2], 64 << 4 | 8)
                        .unwrap();
                    distractors += 1;
                    if distractors == 520 {
                        break 'fill;
                    }
                }
            }
        }
        assert_eq!(distractors, 520);
        let doors = world
            .village_doors_with_bounded_annex(bridge[0], bridge[2])
            .expect("a reachable bridge must survive the candidate budget");
        assert!(doors.contains(&core));
        assert!(doors.contains(&bridge));
        assert!(!doors.contains(&[cx + 32, 205, cz]));
    }

    #[test]
    fn player_built_village_doors_expire_when_support_is_removed() {
        let seed = 42;
        let mut world = World::expanded(seed, crate::dimensions::Dimension::Overworld);
        let (cx, base, cz) = (-8..=8)
            .flat_map(|gx| (-8..=8).map(move |gz| (gx * 192 + 48, gz * 192 + 48)))
            .find_map(|(x, z)| crate::worldgen::village_center(seed, x, z))
            .expect("seed 42 has a generated village");
        let core = [cx, base + 1, cz + 8];
        let anchor = world.live_village_house_doors(cx, cz).unwrap()[1].unwrap();
        let annex = [anchor[0] + 15, anchor[1], anchor[2]];

        for door in [core, annex] {
            world
                .import_block(door[0], door[1] - 1, door[2], 1 << 4)
                .unwrap();
            world
                .import_block(door[0], door[1], door[2], 64 << 4)
                .unwrap();
            world
                .import_block(door[0], door[1] + 1, door[2], 64 << 4 | 8)
                .unwrap();
        }
        assert!(world
            .village_doors_including_player_edits(cx, cz)
            .unwrap()
            .contains(&core));
        assert!(world
            .village_doors_with_bounded_annex(annex[0], annex[2])
            .unwrap()
            .contains(&annex));

        // A door whose lower half is still present must nevertheless expire
        // immediately when its supporting block is no longer solid.
        world
            .import_block(core[0], core[1] - 1, core[2], 9 << 4)
            .unwrap();
        assert!(!world
            .village_doors_including_player_edits(cx, cz)
            .unwrap()
            .contains(&core));
        world
            .import_block(annex[0], annex[1] - 1, annex[2], 50 << 4)
            .unwrap();
        assert_eq!(
            world.village_doors_with_bounded_annex(annex[0], annex[2]),
            None
        );

        let path = std::env::temp_dir().join(format!(
            "ir1710-village-door-support-{}.irw",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        world.save(&path).unwrap();
        let mut restored = World::load(&path).unwrap();
        assert!(!restored
            .village_doors_including_player_edits(cx, cz)
            .unwrap()
            .contains(&core));
        assert_eq!(
            restored.village_doors_with_bounded_annex(annex[0], annex[2]),
            None
        );
        for door in [core, annex] {
            restored
                .import_block(door[0], door[1] - 1, door[2], 1 << 4)
                .unwrap();
        }
        assert!(restored
            .village_doors_including_player_edits(cx, cz)
            .unwrap()
            .contains(&core));
        assert!(restored
            .village_doors_with_bounded_annex(annex[0], annex[2])
            .unwrap()
            .contains(&annex));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn independent_player_village_requires_roof_asymmetry_and_survives_restart() {
        let mut world = World::expanded(42, crate::dimensions::Dimension::Overworld);
        let door = [-1000, 200, -1000];
        let support = 1 << 4;
        world
            .import_block(door[0], door[1] - 1, door[2], support)
            .unwrap();
        world
            .import_block(door[0], door[1], door[2], 64 << 4)
            .unwrap();
        world
            .import_block(door[0], door[1] + 1, door[2], 64 << 4 | 8)
            .unwrap();
        let doors = |world: &World| world.independent_player_village_doors_near(door[0], door[2]);
        assert!(
            doors(&world).is_empty(),
            "a bare outdoor door is not a village"
        );
        world
            .import_block(door[0], door[1] + 3, door[2] + 1, support)
            .unwrap();
        assert_eq!(doors(&world), vec![door]);
        // A door must stop qualifying as a house as soon as its support
        // disappears or becomes a non-solid block. Restoring stone makes
        // the same live door eligible again without rebuilding its halves.
        for unsupported in [0, 9 << 4, 50 << 4] {
            world
                .import_block(door[0], door[1] - 1, door[2], unsupported)
                .unwrap();
            assert!(
                doors(&world).is_empty(),
                "unsupported block {unsupported} must not anchor a village door"
            );
        }
        world
            .import_block(door[0], door[1] - 1, door[2], support)
            .unwrap();
        assert_eq!(doors(&world), vec![door]);
        world
            .import_block(door[0], door[1] + 3, door[2] - 1, support)
            .unwrap();
        assert!(doors(&world).is_empty(), "symmetric roofs are not a house");
        world
            .import_block(door[0], door[1] + 3, door[2] - 1, 0)
            .unwrap();
        assert_eq!(doors(&world), vec![door]);
        world
            .import_block(door[0], door[1] + 1, door[2], 0)
            .unwrap();
        assert!(
            doors(&world).is_empty(),
            "a broken upper half invalidates the door"
        );
        world
            .import_block(door[0], door[1] + 1, door[2], 64 << 4 | 8)
            .unwrap();
        assert_eq!(doors(&world), vec![door]);
        assert!(world
            .independent_player_village_doors_near(door[0] + 100, door[2])
            .is_empty());
        let path = std::env::temp_dir().join(format!(
            "ir1710-independent-village-{}.irw",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        world.save(&path).unwrap();
        let restored = World::load(&path).unwrap();
        assert_eq!(
            restored.independent_player_village_doors_near(door[0], door[2]),
            vec![door]
        );
        assert!(World::expanded(42, crate::dimensions::Dimension::Nether)
            .independent_player_village_doors_near(door[0], door[2])
            .is_empty());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn independent_player_village_recognizes_roof_at_world_ceiling() {
        let mut world = World::expanded(42, crate::dimensions::Dimension::Overworld);
        // A wooden door can have its lower half at y=253, upper half at
        // y=254, and an asymmetric roof at the last valid block height.
        let door = [-1000, 253, -1000];
        let stone = 1 << 4;
        for (x, y, z, block) in [
            (door[0], door[1] - 1, door[2], stone),
            (door[0], door[1], door[2], 64 << 4),
            (door[0], door[1] + 1, door[2], 64 << 4 | 8),
            (door[0], 255, door[2] + 1, stone),
        ] {
            world.import_block(x, y, z, block).unwrap();
        }
        let at_door = |world: &World| {
            world.independent_player_village_doors_near_at(door[0], door[1], door[2])
        };
        assert_eq!(at_door(&world), vec![door]);
        assert_eq!(
            world.independent_player_village_doors_near(door[0], door[2]),
            vec![door]
        );
        assert!(world
            .independent_player_village_doors_near_at(door[0], 248, door[2])
            .is_empty());

        // Roof removal must invalidate the house immediately; restoring the
        // same block must work without rebuilding the door or its support.
        world.import_block(door[0], 255, door[2] + 1, 0).unwrap();
        assert!(at_door(&world).is_empty());
        world
            .import_block(door[0], 255, door[2] + 1, stone)
            .unwrap();
        assert_eq!(at_door(&world), vec![door]);

        let path = std::env::temp_dir().join(format!(
            "ir1710-village-ceiling-door-{}.irw",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        world.save(&path).unwrap();
        let restored = World::load(&path).unwrap();
        assert_eq!(at_door(&restored), vec![door]);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn independent_player_village_prioritizes_nearby_doors_over_edit_order() {
        fn add_roofed_door(world: &mut World, x: i32, z: i32) {
            let y = 200;
            world.import_block(x, y - 1, z, 1 << 4).unwrap();
            world.import_block(x, y, z, 64 << 4).unwrap();
            world.import_block(x, y + 1, z, 64 << 4 | 8).unwrap();
            world.import_block(x, y + 3, z + 1, 1 << 4).unwrap();
        }

        let mut world = World::expanded(42, crate::dimensions::Dimension::Overworld);
        // All 32 distant doors precede x=0 in the edit map. The former
        // first-32 strategy never returned the much closer door at (0, 0).
        for x in -25..=-18 {
            for z in [-12, -4, 4, 12] {
                add_roofed_door(&mut world, x, z);
            }
        }
        add_roofed_door(&mut world, 0, 0);
        let doors = world.independent_player_village_doors_near(0, 0);
        assert_eq!(doors.len(), 32);
        assert_eq!(doors[0], [0, 200, 0]);
        assert!(doors.contains(&[0, 200, 0]));
        assert_eq!(
            world.independent_player_village_doors_near(100, 100),
            Vec::<[i32; 3]>::new()
        );
    }

    #[test]
    fn independent_player_village_does_not_target_different_floors() {
        fn add_roofed_door(world: &mut World, x: i32, y: i32, z: i32) {
            world.import_block(x, y - 1, z, 1 << 4).unwrap();
            world.import_block(x, y, z, 64 << 4).unwrap();
            world.import_block(x, y + 1, z, 64 << 4 | 8).unwrap();
            world.import_block(x, y + 3, z + 1, 1 << 4).unwrap();
        }

        let mut world = World::expanded(42, crate::dimensions::Dimension::Overworld);
        add_roofed_door(&mut world, 0, 200, 0);
        add_roofed_door(&mut world, 2, 220, 0);
        assert_eq!(
            world.independent_player_village_doors_near_at(0, 200, 0),
            vec![[0, 200, 0]]
        );
        assert_eq!(
            world.independent_player_village_doors_near_at(0, 220, 0),
            vec![[2, 220, 0]]
        );
        assert!(world
            .independent_player_village_doors_near_at(0, 190, 0)
            .is_empty());
        assert!(world
            .independent_player_village_doors_near_at(0, -1, 0)
            .is_empty());
        // Preserve the old horizontal-only API for existing consumers.
        assert_eq!(world.independent_player_village_doors_near(0, 0).len(), 2);
    }

    #[test]
    fn independent_village_world_border_is_not_a_roof() {
        let mut world = World::expanded(42, crate::dimensions::Dimension::Overworld);
        let y = 200;
        let z = 0;
        // A door facing the artificial X world edge must not acquire a
        // fake roof merely because its five-block sample leaves the map.
        // A real roof on the interior side must still qualify as a house.
        for (x, inside) in [(WORLD_LIMIT - 2, -1), (-WORLD_LIMIT + 1, 1)] {
            world.import_block(x, y - 1, z, 1 << 4).unwrap();
            world.import_block(x, y, z, 64 << 4 | 1).unwrap();
            world.import_block(x, y + 1, z, 64 << 4 | 8).unwrap();
            assert!(
                world
                    .independent_player_village_doors_near_at(x, y, z)
                    .is_empty(),
                "world border must not count as a roof at x={x}"
            );
            world.import_block(x + inside, y + 3, z, 1 << 4).unwrap();
            assert_eq!(
                world.independent_player_village_doors_near_at(x, y, z),
                vec![[x, y, z]],
                "an interior roof should qualify the door at x={x}"
            );
        }
    }

    #[test]
    fn generated_spawner_lookup_tracks_removal_and_placement() {
        let seed = 42;
        let mut overworld = World::expanded(seed, crate::dimensions::Dimension::Overworld);
        let (x, y, z) = crate::worldgen::dungeon_spawner_in_cell(seed, 0, 0);
        let player = crate::player::PlayerPosition {
            x: f64::from(x) + 0.5,
            y: f64::from(y) + 1.0,
            z: f64::from(z) + 0.5,
        };
        assert!(overworld.nearby_spawners(&[player]).contains(&(x, y, z)));
        overworld.import_block(x, y, z, 0).unwrap();
        assert!(!overworld.nearby_spawners(&[player]).contains(&(x, y, z)));
        overworld.import_block(x, y, z, 52 << 4).unwrap();
        assert!(overworld.nearby_spawners(&[player]).contains(&(x, y, z)));
        let nether = World::expanded(seed, crate::dimensions::Dimension::Nether);
        assert!(!nether.nearby_spawners(&[player]).contains(&(x, y, z)));
    }

    #[test]
    fn flower_support_break_detaches_original_variant_after_commit() {
        let path = std::env::temp_dir().join(format!("ir1710-flower-{}.irw", std::process::id()));
        let _ = fs::remove_file(&path);
        let mut world = World::new();
        let flower = (38 << 4) | 5;
        assert!(world.can_support(1, 64, 1, flower));
        world.update_and_save(&path, 1, 64, 1, flower).unwrap();
        world.update_and_save(&path, 1, 63, 1, 0).unwrap();
        assert!(world.block_tick(&path, 64).unwrap().contains(&(1, 64, 1)));
        assert_eq!(world.get(1, 64, 1), 0);
        assert!(world
            .drain_drops()
            .iter()
            .any(|drop| drop.stack.id == 38 && drop.stack.damage == 5));
        assert_eq!(World::load(&path).unwrap().get(1, 64, 1), 0);
        assert!(!world.can_support(1, 64, 1, 31 << 4));
        assert!(!world.can_support(1, 64, 1, 32 << 4));
        world.import_block(1, 63, 1, 12 << 4).unwrap();
        assert!(world.can_support(1, 64, 1, 32 << 4));
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn unsupported_torch_detaches_into_an_item_after_commit() {
        let path =
            std::env::temp_dir().join(format!("ir1710-attachment-{}.irw", std::process::id()));
        let _ = fs::remove_file(&path);
        let mut w = World::new();
        w.update_and_save(&path, 1, 64, 1, (50 << 4) | 5).unwrap();
        w.update_and_save(&path, 1, 63, 1, 0).unwrap();
        let changed = w.block_tick(&path, 64).unwrap();
        assert!(changed.contains(&(1, 64, 1)));
        assert_eq!(w.get(1, 64, 1), 0);
        assert!(w.drain_drops().iter().any(|drop| drop.stack.id == 50));
        let restored = World::load(&path).unwrap();
        assert_eq!(restored.get(1, 64, 1), 0);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn light_refreshes_are_bounded_to_chunks_and_only_follow_committed_edits() {
        let path =
            std::env::temp_dir().join(format!("ir1710-light-dirty-{}.irw", std::process::id()));
        let _ = fs::remove_file(&path);
        let mut w = World::new();
        w.update_and_save(&path, 15, 64, 15, 50 << 4).unwrap();
        let first = w.drain_light_chunks(1);
        assert_eq!(first, vec![(0, 0)]);
        let remaining = w.drain_light_chunks(32);
        assert!(remaining.contains(&(1, 1)));
        assert!(!remaining.contains(&(2, 2)));
        assert!(w.drain_light_chunks(1).is_empty());
        // Metadata-only updates cannot change the light field.
        w.update_and_save(&path, 15, 64, 15, (50 << 4) | 5).unwrap();
        assert!(w.drain_light_chunks(32).is_empty());
        // A failed save neither mutates the block nor queues a relight.
        let failure_path = path.join("invalid.irw");
        assert!(w.update_and_save(&failure_path, 15, 64, 15, 0).is_err());
        assert!(w.drain_light_chunks(32).is_empty());
        w.update_and_save(&path, 15, 64, 15, 0).unwrap();
        assert!(!w.drain_light_chunks(32).is_empty());
        let restored = World::load(&path).unwrap();
        assert_eq!(restored.get(15, 64, 15), 0);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn sand_gravel_fall_one_cell_per_tick_and_restore_after_reload() {
        let path = std::env::temp_dir().join(format!(
            "ir1710-gravity-{}-{:?}.irw",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_file(&path);
        let mut world = World::new();
        world
            .update_and_save(&path, 2, 67, -2, (12 << 4) | 1)
            .unwrap();
        world.update_and_save(&path, 3, 66, -2, 13 << 4).unwrap();
        let first = world.gravity_tick(&path, 64).unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(world.get(2, 66, -2), (12 << 4) | 1);
        assert_eq!(world.get(3, 65, -2), 13 << 4);
        // In-flight queue state is reconstructed from durable sparse edits.
        let mut restored = World::load(&path).unwrap();
        assert_eq!(restored.gravity_tick(&path, 64).unwrap().len(), 2);
        for _ in 0..8 {
            restored.gravity_tick(&path, 64).unwrap();
        }
        assert_eq!(restored.get(2, 64, -2), (12 << 4) | 1);
        assert_eq!(restored.get(3, 64, -2), 13 << 4);
        assert!(restored.gravity_tick(&path, 64).unwrap().is_empty());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn breaking_support_wakes_gravity_and_saves_undo_on_failure() {
        let path =
            std::env::temp_dir().join(format!("ir1710-gravity-support-{}.irw", std::process::id()));
        let _ = fs::remove_file(&path);
        let mut world = World::new();
        world.update_and_save(&path, 0, 64, 0, 12 << 4).unwrap();
        assert!(world.gravity_tick(&path, 64).unwrap().is_empty());
        world.update_and_save(&path, 0, 63, 0, 0).unwrap();
        // A regular file cannot be used as the parent directory of a save.
        let blocked = path.join("world.irw");
        let err = world.gravity_tick(&blocked, 64);
        assert!(err.is_err());
        assert_eq!(world.get(0, 64, 0), 12 << 4);
        assert_eq!(world.get(0, 63, 0), 0);
        assert_eq!(world.gravity_tick(&path, 64).unwrap().len(), 1);
        assert_eq!(world.get(0, 63, 0), 12 << 4);
        assert_eq!(world.get(0, 64, 0), 0);
        assert!(world.gravity_tick(&path, 64).unwrap().is_empty());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn seeded_natural_world_roundtrip_isolated_from_irw1() {
        let path = std::env::temp_dir().join(format!("ir1710-natural-{}.irw", std::process::id()));
        let _ = fs::remove_file(&path);
        let mut generated = World::natural(42);
        assert!(generated.is_natural());
        assert_eq!(generated.natural_seed(), Some(42));
        assert!(generated.spawn_y() > 50.0);
        let surface = crate::terrain::surface(42, -17, 16);
        assert_eq!(generated.get(-17, surface, 16), 2 << 4);
        generated
            .update_and_save(&path, -17, surface, 16, 0)
            .unwrap();
        assert_eq!(&fs::read(&path).unwrap()[..4], b"IRW2");
        let again = World::load(&path).unwrap();
        assert_eq!(again.natural_seed(), Some(42));
        assert_eq!(again.get(-17, surface, 16), 0);
        assert_eq!(again.get(-17, surface - 1, 16), 3 << 4);
        let (mask, packet) = again.chunk_data(-2, 1);
        assert_eq!(mask, 0xff);
        assert_eq!(packet.len(), 8 * 10240 + 256);
        fs::remove_file(path).unwrap();
        assert_eq!(World::new().chunk_data(0, 0).0, 0x18);
    }
    #[test]
    fn nether_lava_spreads_every_tick_with_one_level_per_horizontal_step() {
        let path =
            std::env::temp_dir().join(format!("ir1710-nether-lava-{}.irw", std::process::id()));
        let _ = fs::remove_file(&path);
        let mut world = World::expanded(42, crate::dimensions::Dimension::Nether);
        // Carve a deterministic floor and open channel independent of the
        // Nether generator's native terrain.
        for x in -1..=4 {
            for z in -1..=1 {
                world.import_block(x, 63, z, 87 << 4).unwrap();
                world.import_block(x, 64, z, 0).unwrap();
            }
        }
        world.update_and_save(&path, 0, 64, 0, 10 << 4).unwrap();
        world.block_tick(&path, 128).unwrap();
        // The first tick must flow (Overworld lava waits four ticks).
        assert_eq!(world.get(1, 64, 0), (10 << 4) | 1);
        for _ in 0..24 {
            world.block_tick(&path, 256).unwrap();
        }
        assert_eq!(world.get(2, 64, 0), (10 << 4) | 2);
        assert_eq!(World::load(&path).unwrap().get(2, 64, 0), (10 << 4) | 2);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn overworld_lava_attenuates_twice_as_fast_as_water() {
        let path = std::env::temp_dir().join(format!(
            "ir1710-lava-attenuation-{}.irw",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        let mut world = World::new();
        world.update_and_save(&path, 0, 64, 0, 10 << 4).unwrap();
        for _ in 0..48 {
            world.block_tick(&path, 128).unwrap();
        }
        assert_eq!(world.get(1, 64, 0), (10 << 4) | 2);
        assert_eq!(world.get(2, 64, 0), (10 << 4) | 4);
        assert_eq!(world.get(3, 64, 0), (10 << 4) | 6);
        assert_eq!(world.get(4, 64, 0), 0);
        let loaded = World::load(&path).unwrap();
        assert_eq!(loaded.get(3, 64, 0), (10 << 4) | 6);
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn water_flow_and_circuit_propagation_are_bounded_and_persistent() {
        let path =
            std::env::temp_dir().join(format!("ir1710-fluid-circuit-{}.irw", std::process::id()));
        let _ = fs::remove_file(&path);
        let mut w = World::new();
        // Water above the grass: first tick flows down only.
        w.update_and_save(&path, 5, 66, 5, 8 << 4).unwrap();
        let first = w.block_tick(&path, 64).unwrap();
        assert!(first.contains(&(5, 65, 5)));
        assert_eq!(w.get(5, 65, 5), (8 << 4) | 8);
        assert_eq!(w.get(6, 65, 5), 0);
        // Falling level-8 water reaches y=64 before spreading at level 1.
        assert!(w.block_tick(&path, 64).unwrap().contains(&(5, 64, 5)));
        assert!(w.block_tick(&path, 64).unwrap().contains(&(6, 64, 5)));
        assert_eq!(w.get(6, 64, 5), (8 << 4) | 1);
        // Lever -> one wire -> adjacent wire, with 15 and then 14.
        w.update_and_save(&path, 0, 64, 0, (69 << 4) | 8).unwrap();
        w.update_and_save(&path, 1, 64, 0, 55 << 4).unwrap();
        w.update_and_save(&path, 2, 64, 0, 55 << 4).unwrap();
        w.block_tick(&path, 64).unwrap();
        assert_eq!(w.get(1, 64, 0), (55 << 4) | 15);
        w.block_tick(&path, 64).unwrap();
        assert_eq!(w.get(2, 64, 0), (55 << 4) | 14);
        // Turning off the lever decays a wire chain to zero.
        w.update_and_save(&path, 0, 64, 0, 69 << 4).unwrap();
        for _ in 0..35 {
            w.block_tick(&path, 64).unwrap();
        }
        assert_eq!(w.get(1, 64, 0), 55 << 4);
        assert_eq!(w.get(2, 64, 0), 55 << 4);
        let saved = World::load(&path).unwrap();
        assert_eq!(saved.get(5, 65, 5), (8 << 4) | 1);
        assert_eq!(saved.get(1, 64, 0), 55 << 4);
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn water_lava_contacts_solidify_sources_and_flowing_lava_atomically() {
        let path =
            std::env::temp_dir().join(format!("ir1710-lava-contact-{}.irw", std::process::id()));
        let _ = fs::remove_file(&path);
        let mut world = World::new();
        // On the flat world's solid grass, each fluid is confined to y=64.
        world.update_and_save(&path, 0, 64, 0, 8 << 4).unwrap();
        world.update_and_save(&path, 1, 64, 0, 10 << 4).unwrap();
        world.update_and_save(&path, -3, 64, 0, 8 << 4).unwrap();
        world
            .update_and_save(&path, -2, 64, 0, (10 << 4) | 3)
            .unwrap();

        // A failed batch save must not leave a solid block in memory or
        // consume the queued contacts. The following retry must work.
        assert!(world.block_tick(&path.join("invalid"), 64).is_err());
        assert_eq!(world.get(1, 64, 0), 10 << 4);
        assert_eq!(world.get(-2, 64, 0), (10 << 4) | 3);
        let changed = world.block_tick(&path, 64).unwrap();
        assert!(changed.contains(&(1, 64, 0)));
        assert!(changed.contains(&(-2, 64, 0)));
        assert_eq!(world.get(1, 64, 0), 49 << 4); // obsidian
        assert_eq!(world.get(-2, 64, 0), 4 << 4); // cobblestone
        assert_eq!(World::load(&path).unwrap().get(1, 64, 0), 49 << 4);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn source_flow_respects_edit_budget_and_legacy_flat() {
        let mut w = World::new();
        assert!(w
            .block_tick(Path::new("/no-write-when-empty"), 64)
            .unwrap()
            .is_empty());
        assert!(is_placeable(55) && is_placeable(69) && is_placeable(76));
        assert!(is_placeable(8) && is_placeable(10));
        w.import_block(0, 67, 0, 10 << 4).unwrap();
        assert_eq!(w.get(0, 67, 0) >> 4, 10);
    }
    #[test]
    fn flat_and_bounds() {
        let w = World::new();
        assert_eq!(w.get(0, 63, 0), 2 << 4);
        assert_eq!(w.get(-32, 60, -32), 7 << 4);
        assert_eq!(w.get(-33, 63, 0), 2 << 4);
        assert_eq!(w.get(WORLD_LIMIT, 63, 0), 0);
        assert_eq!(w.get(-WORLD_LIMIT - 1, 63, 0), 0);
        assert_eq!(w.chunk_data(-1, 0).0, 0x18);
        assert_eq!(w.chunk_data(-1, 0).1.len(), 20736);
        assert_eq!(w.chunk_data(0, 0).1[15 * 256], 2);
    }
    #[test]
    fn indexed_chunk_sections_respect_both_negative_and_positive_boundaries() {
        let mut world = World::new();
        world.import_block(-1, 145, -1, 4 << 4).unwrap();
        world.import_block(0, 210, -1, 4 << 4).unwrap();
        world.import_block(-1, 226, 0, 4 << 4).unwrap();
        world.import_block(15, 241, 15, 4 << 4).unwrap();
        assert_eq!(world.chunk_data(-1, -1).0, 0x18 | (1 << 9));
        assert_eq!(world.chunk_data(0, -1).0, 0x18 | (1 << 13));
        assert_eq!(world.chunk_data(-1, 0).0, 0x18 | (1 << 14));
        assert_eq!(world.chunk_data(0, 0).0, 0x18 | (1 << 15));
    }

    #[test]
    fn live_world_refuses_irw_writes_after_session_lock_is_stolen() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-guarded-irw-save-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root).unwrap();
        let world_path = root.join("flat.irw");
        let session = Arc::new(Mutex::new(
            crate::session_lock::WorldSessionLock::acquire(&root).unwrap(),
        ));
        let mut world = World::new();
        world
            .bind_save_session(&world_path, Arc::clone(&session))
            .unwrap();
        world.save(&world_path).unwrap();
        let before = fs::read(&world_path).unwrap();
        // A cloned editor must carry the same ownership requirement.
        let cloned = world.clone();
        assert!(cloned.save(&root.join("wrong.irw")).is_err());
        session
            .lock()
            .unwrap()
            .replace_timestamp_for_test(0)
            .unwrap();
        assert!(cloned.save(&world_path).is_err());
        assert!(cloned.encoded().is_err());
        assert_eq!(fs::read(&world_path).unwrap(), before);
        assert!(!world_path.with_extension("tmp").exists());
        drop(cloned);
        drop(world);
        drop(session);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persist_and_rollback() {
        let path = std::env::temp_dir().join(format!(
            "industrial-world-test-{}-{}.bin",
            std::process::id(),
            196710u32
        ));
        let _ = fs::remove_file(&path);
        let mut w = World::new();
        assert!(w.update_and_save(&path, -1, 65, 2, 4 << 4).unwrap());
        let loaded = World::load(&path).unwrap();
        assert_eq!(loaded.get(-1, 65, 2), 4 << 4);
        assert_eq!(loaded.get(-1, 63, 2), 2 << 4);
        fs::remove_file(&path).unwrap();
        assert!(!w
            .update_and_save(&path, WORLD_LIMIT, 65, 2, 4 << 4)
            .unwrap());
    }
    #[test]
    fn never_replaces_an_existing_import_target() {
        let base = std::env::temp_dir().join(format!("ir1710-world-import-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let target = base.join("imported.irw");
        let mut world = World::new();
        world.import_block(-49, 145, 19, (4 << 4) | 2).unwrap();
        world.import_block(1, 63, 1, 0).unwrap();
        world.save_new(&target).unwrap();
        let first = fs::read(&target).unwrap();
        assert_eq!(
            World::load(&target).unwrap().get(-49, 145, 19),
            (4 << 4) | 2
        );
        assert_eq!(World::load(&target).unwrap().get(1, 63, 1), 0);
        assert!(world.save_new(&target).is_err());
        assert_eq!(fs::read(&target).unwrap(), first);
        fs::remove_dir_all(base).unwrap();
    }
    #[test]
    fn corrupt_save_rejected() {
        let path = std::env::temp_dir().join(format!(
            "industrial-corrupt-test-{}.bin",
            std::process::id()
        ));
        fs::write(&path, b"IRW1\x00\x00\x00\x01").unwrap();
        assert!(World::load(&path).is_err());
        fs::remove_file(&path).unwrap();
    }
    #[test]
    fn sparse_edits_survive_cross_chunk_and_section_boundaries() {
        let path =
            std::env::temp_dir().join(format!("industrial-chunks-{}.irw", std::process::id()));
        let _ = fs::remove_file(&path);
        let mut world = World::new();
        for (x, y, z, block) in [
            (-49, 2, 19, 4 << 4),
            (56, 145, -33, 5 << 4),
            (63, 64, 0, 1 << 4),
        ] {
            assert!(world.update_and_save(&path, x, y, z, block).unwrap());
        }
        let loaded = World::load(&path).unwrap();
        assert_eq!(loaded.get(56, 145, -33), 5 << 4);
        assert_eq!(loaded.get(-49, 2, 19), 4 << 4);
        let (mask, binary) = loaded.chunk_data(3, -3);
        assert_eq!(mask, 0x18 | (1 << 9));
        assert_eq!(binary.len(), 3 * 10240 + 256);
        // Section 9 (y=144..159) follows section 3 and 4 in ascending Y order.
        let index_in_section = 256 + 15 * 16 + 8;
        assert_eq!(binary[2 * 4096 + index_in_section as usize], 5);
        assert_eq!(loaded.chunk_data(-4, 1).0, 0x19);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn water_sources_regenerate_and_disconnected_flows_recede() {
        let path =
            std::env::temp_dir().join(format!("ir1710-water-recede-{}.irw", std::process::id()));
        let _ = fs::remove_file(&path);
        let mut w = World::new();
        w.update_and_save(&path, -1, 64, 0, 8 << 4).unwrap();
        w.update_and_save(&path, 1, 64, 0, 8 << 4).unwrap();
        w.update_and_save(&path, 0, 64, 0, (8 << 4) | 1).unwrap();
        w.block_tick(&path, 64).unwrap();
        assert_eq!(w.get(0, 64, 0), 8 << 4);
        w.update_and_save(&path, -1, 64, 0, 0).unwrap();
        w.update_and_save(&path, 1, 64, 0, 0).unwrap();
        w.update_and_save(&path, 0, 64, 0, 0).unwrap();
        for _ in 0..90 {
            w.block_tick(&path, 64).unwrap();
        }
        assert_eq!(w.get(0, 64, 0), 0);
        assert_eq!(World::load(&path).unwrap().get(0, 64, 0), 0);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn lava_delay_and_cross_chunk_block_light() {
        let path =
            std::env::temp_dir().join(format!("ir1710-lava-delay-{}.irw", std::process::id()));
        let _ = fs::remove_file(&path);
        let mut w = World::new();
        w.update_and_save(&path, 0, 64, 0, 10 << 4).unwrap();
        for _ in 0..3 {
            w.block_tick(&path, 64).unwrap();
        }
        assert_eq!(w.get(1, 64, 0), 0);
        w.block_tick(&path, 64).unwrap();
        assert_eq!(w.get(1, 64, 0), (10 << 4) | 2);
        w.update_and_save(&path, 15, 64, 0, 50 << 4).unwrap();
        let (block, sky) = w.light_arrays(1, 0);
        assert!(block[64 * 256] > 0);
        assert_eq!(sky[80 * 256], 15);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn crops_hydrate_and_grow_saplings_become_trees_and_attachments_fall() {
        let path = std::env::temp_dir().join(format!("ir1710-ecology-{}.irw", std::process::id()));
        let _ = fs::remove_file(&path);
        let mut w = World::new();
        w.update_and_save(&path, 0, 63, 0, 60 << 4).unwrap();
        w.update_and_save(&path, 2, 64, 0, 8 << 4).unwrap();
        w.update_and_save(&path, 0, 64, 0, 59 << 4).unwrap();
        w.update_and_save(&path, 8, 64, 0, 6 << 4).unwrap();
        for _ in 0..400 {
            w.block_tick(&path, 64).unwrap();
        }
        assert_eq!(w.get(0, 63, 0), (60 << 4) | 7);
        assert_eq!(w.get(0, 64, 0), (59 << 4) | 7);
        assert_eq!(w.get(8, 64, 0) >> 4, 17);
        // A broken support causes a torch to detach via the next tick.
        w.update_and_save(&path, 12, 64, 0, 50 << 4).unwrap();
        w.update_and_save(&path, 12, 63, 0, 0).unwrap();
        w.block_tick(&path, 64).unwrap();
        assert_eq!(w.get(12, 64, 0), 0);
        assert_eq!(World::load(&path).unwrap().get(0, 64, 0), (59 << 4) | 7);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn nether_rejects_placed_water_and_cleans_imported_water_on_tick() {
        let path =
            std::env::temp_dir().join(format!("ir1710-nether-water-{}.irw", std::process::id()));
        let _ = fs::remove_file(&path);
        let mut world = World::expanded(42, crate::dimensions::Dimension::Nether);
        assert!(!world.update_and_save(&path, 0, 110, 0, 8 << 4).unwrap());
        assert!(!path.exists(), "rejected water must not cause a disk write");

        world.import_block(0, 110, 0, 9 << 4).unwrap();
        let changed = world.block_tick(&path, 64).unwrap();
        assert!(changed.contains(&(0, 110, 0)));
        assert_eq!(world.get(0, 110, 0), 0);
        assert_eq!(World::load(&path).unwrap().get(0, 110, 0), 0);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn device_fires_once_per_rising_edge_only_after_world_save() {
        let path =
            std::env::temp_dir().join(format!("ir1710-device-trigger-{}.irw", std::process::id()));
        let _ = fs::remove_file(&path);
        let mut world = World::new();
        world
            .update_and_save(&path, 0, 65, 0, (23 << 4) | 5)
            .unwrap();
        world.update_and_save(&path, -1, 65, 0, 152 << 4).unwrap();
        world.block_tick(&path, 64).unwrap();
        assert_eq!(world.drain_device_activations(), vec![(0, 65, 0)]);
        for _ in 0..4 {
            world.block_tick(&path, 64).unwrap();
        }
        assert!(world.drain_device_activations().is_empty());
        assert_eq!(World::load(&path).unwrap().get(0, 65, 0) & 8, 8);
        world.update_and_save(&path, -1, 65, 0, 0).unwrap();
        world.block_tick(&path, 64).unwrap();
        assert_eq!(world.get(0, 65, 0) & 8, 0);
        world.update_and_save(&path, -1, 65, 0, 152 << 4).unwrap();
        world.block_tick(&path, 64).unwrap();
        assert_eq!(world.drain_device_activations(), vec![(0, 65, 0)]);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn piston_action_is_published_once_after_durable_world_commit() {
        let path =
            std::env::temp_dir().join(format!("ir1710-piston-action-{}.irw", std::process::id()));
        let _ = fs::remove_file(&path);
        let mut world = World::new();
        world
            .update_and_save(&path, 1, 64, 0, (33 << 4) | 5)
            .unwrap();
        world.update_and_save(&path, 0, 64, 0, 152 << 4).unwrap();
        world.block_tick(&path, 64).unwrap();
        assert_eq!(
            world.drain_piston_actions(),
            vec![PistonAction {
                pos: (1, 64, 0),
                block_id: 33,
                facing: 5,
                extending: true,
            }]
        );
        world.block_tick(&path, 64).unwrap();
        assert!(world.drain_piston_actions().is_empty());
        world.update_and_save(&path, 0, 64, 0, 0).unwrap();
        world.block_tick(&path, 64).unwrap();
        assert_eq!(
            world.drain_piston_actions(),
            vec![PistonAction {
                pos: (1, 64, 0),
                block_id: 33,
                facing: 5,
                extending: false,
            }]
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn piston_collision_sweep_expires_after_three_ticks() {
        let path =
            std::env::temp_dir().join(format!("ir1710-piston-sweep-{}.irw", std::process::id()));
        let _ = fs::remove_file(&path);
        let mut w = World::new();
        w.update_and_save(&path, 0, 65, 0, (33 << 4) | 5).unwrap();
        w.update_and_save(&path, 1, 65, 0, 1 << 4).unwrap();
        w.update_and_save(&path, -1, 65, 0, 152 << 4).unwrap();
        w.block_tick(&path, 64).unwrap();
        assert!(w.piston_sweep_at(1, 65, 0));
        assert!(w.piston_sweep_at(2, 65, 0));
        for _ in 0..3 {
            w.block_tick(&path, 64).unwrap();
        }
        assert!(!w.piston_sweep_at(1, 65, 0));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn piston_pushes_dust_powered_block_and_doors_have_both_halves() {
        let path = std::env::temp_dir().join(format!("ir1710-device-{}.irw", std::process::id()));
        let _ = fs::remove_file(&path);
        let mut w = World::new();
        w.update_and_save(&path, -1, 64, 0, (69 << 4) | 8).unwrap();
        w.update_and_save(&path, 0, 64, 0, 55 << 4).unwrap();
        w.update_and_save(&path, 1, 64, 0, (33 << 4) | 5).unwrap();
        w.update_and_save(&path, 2, 64, 0, 1 << 4).unwrap();
        for _ in 0..6 {
            w.block_tick(&path, 64).unwrap();
        }
        assert_eq!(w.get(1, 64, 0), (33 << 4) | 13);
        assert_eq!(w.get(2, 64, 0), (34 << 4) | 5);
        assert_eq!(w.get(3, 64, 0), 1 << 4);
        w.update_and_save(&path, -1, 64, 0, 69 << 4).unwrap();
        for _ in 0..35 {
            w.block_tick(&path, 64).unwrap();
        }
        assert_eq!(w.get(1, 64, 0), (33 << 4) | 5);
        assert_eq!(w.get(2, 64, 0), 0);
        assert_eq!(w.get(3, 64, 0), 1 << 4);
        assert!(w.place_door(&path, 5, 64, 0, 64, 2).unwrap());
        assert_eq!(w.get(5, 65, 0), (64 << 4) | 8);
        assert_eq!(
            w.interact_device(&path, 5, 65, 0).unwrap(),
            vec![(5, 64, 0)]
        );
        assert_ne!(w.get(5, 64, 0) & 4, 0);
        w.update_and_save(&path, 5, 64, 0, 0).unwrap();
        w.block_tick(&path, 64).unwrap();
        assert_eq!(w.get(5, 65, 0), 0);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn irw3_biomes_are_opt_in_and_old_generation_is_unchanged() {
        let path = std::env::temp_dir().join(format!("ir1710-biomes-{}.irw", std::process::id()));
        let _ = fs::remove_file(&path);
        let old = World::natural(42);
        let new = World::with_biomes(42);
        assert!(!old.is_biome_world() && new.is_biome_world());
        assert_eq!(old.get(0, crate::terrain::surface(42, 0, 0), 0), 2 << 4);
        let mut seen = std::collections::BTreeSet::new();
        for x in (-1500..=1500).step_by(100) {
            for z in (-1500..=1500).step_by(100) {
                seen.insert(crate::terrain::biome(42, x, z));
            }
        }
        assert_eq!(seen, [0u8, 1, 2, 3, 4].into_iter().collect());
        new.save_new(&path).unwrap();
        assert_eq!(&fs::read(&path).unwrap()[..4], b"IRW3");
        let reloaded = World::load(&path).unwrap();
        assert!(reloaded.is_biome_world());
        assert_eq!(reloaded.natural_seed(), Some(42));
        assert_eq!(old.get(20, 60, 20), World::natural(42).get(20, 60, 20));
        fs::remove_file(path).unwrap();
    }
}

#[cfg(test)]
mod group_b_regression_tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("ir1710-b-{name}-{}.irw", std::process::id()))
    }

    #[test]
    fn falling_water_uses_legacy_level_eight_and_survives_reload() {
        let path = temp("flow-level8");
        let _ = std::fs::remove_file(&path);
        let mut world = World::new();
        world.update_and_save(&path, 0, 75, 0, 8 << 4).unwrap();
        world.block_tick(&path, 64).unwrap();
        assert_eq!(world.get(0, 74, 0), (8 << 4) | 8);
        let mut reloaded = World::load(&path).unwrap();
        reloaded.block_tick(&path, 64).unwrap();
        assert_eq!(reloaded.get(0, 74, 0), (8 << 4) | 8);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn comparator_uses_delayed_analog_output_and_survives_world_reload() {
        let path = temp("comparator-delay");
        let _ = std::fs::remove_file(&path);
        let mut world = World::new();
        // Comparator requires a floor; world reload checks support validity.
        world.update_and_save(&path, 0, 64, 0, 1 << 4).unwrap();
        world
            .update_and_save(&path, -1, 65, 0, (69 << 4) | 8)
            .unwrap();
        world
            .update_and_save(&path, 0, 65, 0, (149 << 4) | 1)
            .unwrap();
        world.block_tick(&path, 64).unwrap();
        assert_eq!(world.get(0, 65, 0) >> 4, 149);
        world.block_tick(&path, 64).unwrap();
        world.block_tick(&path, 64).unwrap();
        assert_eq!(world.get(0, 65, 0) >> 4, 150);
        assert!(world.power_into((0, 65, 0), (1, 65, 0)));
        let mut restored = World::load(&path).unwrap();
        for _ in 0..3 {
            restored.block_tick(&path, 64).unwrap();
        }
        assert!(restored.power_into((0, 65, 0), (1, 65, 0)));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn cactus_and_netther_wart_support_is_validated() {
        let mut world = World::new();
        world.import_block(0, 64, 0, 12 << 4).unwrap();
        world.import_block(1, 64, 0, 88 << 4).unwrap();
        assert!(world.can_support(0, 65, 0, 81 << 4));
        assert!(world.can_support(1, 65, 0, 115 << 4));
        assert!(!world.can_support(0, 65, 0, 115 << 4));
        assert!(!world.can_support(1, 65, 0, 83 << 4));
    }

    #[test]
    fn comparator_strength_is_not_rounded_up_to_fifteen_by_dust() {
        let path = temp("comparator-strength");
        let _ = std::fs::remove_file(&path);
        let mut world = World::new();
        world.import_block(0, 64, 0, 1 << 4).unwrap();
        world.import_block(0, 65, 0, (150 << 4) | 1).unwrap();
        world.comparator_outputs.insert((0, 65, 0), 5);
        world.import_block(1, 65, 0, 55 << 4).unwrap();
        assert_eq!(world.signal_into((0, 65, 0), (1, 65, 0)), 5);
        assert_eq!(world.signal_into((0, 65, 0), (0, 65, 1)), 0);
        world.block_tick(&path, 64).unwrap();
        assert_eq!(world.get(1, 65, 0), (55 << 4) | 5);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn repeater_strongly_powers_cube_and_not_its_own_input() {
        let path = temp("conductor-direction");
        let _ = std::fs::remove_file(&path);
        let mut world = World::new();
        world.import_block(0, 65, 0, (94 << 4) | 1).unwrap();
        world.import_block(1, 65, 0, 1 << 4).unwrap();
        world.import_block(2, 65, 0, 55 << 4).unwrap();
        assert_eq!(world.signal_into((1, 65, 0), (2, 65, 0)), 15);
        assert_eq!(world.signal_into((1, 65, 0), (0, 65, 0)), 0);
        world.block_tick(&path, 64).unwrap();
        assert_eq!(world.get(2, 65, 0), (55 << 4) | 15);
        // A deleted power source must invalidate the distant dust, too.
        world.update_and_save(&path, 0, 65, 0, 0).unwrap();
        world.block_tick(&path, 64).unwrap();
        assert_eq!(world.get(2, 65, 0), 55 << 4);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn raining_extinguishes_fire_through_existing_tick_pipeline() {
        let path = temp("rain-fire");
        let _ = std::fs::remove_file(&path);
        let mut world = World::new();
        world.update_and_save(&path, 0, 65, 0, 51 << 4).unwrap();
        world.set_raining(true);
        for _ in 0..20 {
            world.block_tick(&path, 64).unwrap();
        }
        assert_eq!(world.get(0, 65, 0), 0);
        assert_eq!(World::load(&path).unwrap().get(0, 65, 0), 0);
        std::fs::remove_file(path).unwrap();
    }
}

#[cfg(test)]
mod weather_observation_limits_tests {
    use super::*;

    #[test]
    fn old_weather_columns_are_evicted_and_revisited_columns_are_recorded() {
        let mut world = World::new();
        // Exercise the actual legal coordinate range around zero.
        let first_x = -(MAX_WEATHER_COLUMNS as i32) / 2;
        for offset in 0..(MAX_WEATHER_COLUMNS as i32 + 20) {
            world.register_weather_column((first_x + offset, 64, 0));
        }
        assert_eq!(world.weather_tracking_len(), MAX_WEATHER_COLUMNS);
        assert_eq!(world.pending_weather_order.len(), MAX_WEATHER_COLUMNS);
        assert!(!world.pending_weather.contains(&(first_x, 64, 0)));
        assert!(world.pending_weather.contains(&(
            first_x + MAX_WEATHER_COLUMNS as i32 + 19,
            64,
            0
        )));
        world.register_weather_column((first_x, 64, 0));
        assert!(world.pending_weather.contains(&(first_x, 64, 0)));
        assert!(!world.pending_weather.contains(&(first_x + 20, 64, 0)));
    }

    #[test]
    fn block_replacement_removes_weather_registration_and_order_entry() {
        let mut world = World::new();
        world.register_weather_column((0, 64, 0));
        world.register_weather_column((1, 64, 0));
        world.unregister_weather_column((0, 64, 0));
        assert_eq!(world.weather_tracking_len(), 1);
        assert_eq!(world.pending_weather_order.len(), 1);
        world.register_weather_column((0, 64, 0));
        assert_eq!(world.pending_weather_order.len(), 2);
        assert_eq!(world.pending_weather_order[0], (1, 64, 0));
    }
}
