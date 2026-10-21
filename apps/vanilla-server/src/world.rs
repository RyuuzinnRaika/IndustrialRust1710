//! Bounded, sparse, deterministic flat world streamed by chunk.
//! IRW1 is temporary storage; this is NOT Anvil/NBT/Forge world storage.
use std::collections::{BTreeMap, BTreeSet};
mod devices;
mod ecology;
mod lighting;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

/// Temporary world edge (blocks). Prevents unbounded player edits or packet positions.
/// Coordinates x/z must be in [-WORLD_LIMIT, WORLD_LIMIT).
pub const WORLD_LIMIT: i32 = 16_384;
pub const VIEW_DISTANCE: i32 = 2; // Send a 5x5 chunk window per connected player.
pub const MIN_CHUNK: i32 = -WORLD_LIMIT / 16;
pub const MAX_CHUNK: i32 = WORLD_LIMIT / 16 - 1;
const MAX_EDITS: usize = 65_536;
const MAGIC: &[u8; 4] = b"IRW1";
const NATURAL_MAGIC: &[u8; 4] = b"IRW2";
const BIOME_MAGIC: &[u8; 4] = b"IRW3";
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Generation {
    #[default]
    Flat,
    Natural(u64),
    Biomes(u64),
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
    pending_devices: BTreeSet<Position>,
    pending_repeaters: BTreeMap<Position, (u64, bool)>,
    pending_buttons: BTreeMap<Position, u64>,
    pending_drops: Vec<WorldDrop>,
    // Monotonic simulation phase; no catch-up while the server is stopped.
    fluid_phase: u64,
}

/// A persisted world mutation can create a temporary ground-item event.
/// World state is written first, then the 20TPS worker saves item entities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorldDrop {
    pub pos: (i32, i32, i32),
    pub stack: crate::inventory::Stack,
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
            | 18 // leaves
            | 59 // wheat
            | 60 // farmland (via hoe)
            | 66 // rails
            | 70 | 72 // pressure plates
            | 77 | 143 // buttons
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
            | 54 // chest (single only)
            | 56 // diamond ore
            | 57
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
    /// Tracks which chunk columns contain changes, without allocating a full world.
    pub fn drain_drops(&mut self) -> Vec<WorldDrop> {
        std::mem::take(&mut self.pending_drops)
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
            Generation::Natural(_) | Generation::Biomes(_)
        )
    }
    pub fn is_biome_world(&self) -> bool {
        matches!(self.generation, Generation::Biomes(_))
    }
    pub fn with_biomes(seed: u64) -> Self {
        Self {
            generation: Generation::Biomes(seed),
            ..Self::default()
        }
    }
    pub fn natural_seed(&self) -> Option<u64> {
        match self.generation {
            Generation::Natural(seed) | Generation::Biomes(seed) => Some(seed),
            Generation::Flat => None,
        }
    }
    pub fn spawn_y(&self) -> f64 {
        match self.generation {
            Generation::Natural(seed) => f64::from(crate::terrain::surface(seed, 0, 0) + 1),
            Generation::Biomes(seed) => f64::from(crate::terrain::biome_surface(seed, 0, 0) + 1),
            Generation::Flat => 65.0,
        }
    }
    fn base_block(&self, x: i32, y: u8, z: i32) -> u16 {
        match self.generation {
            Generation::Flat => Self::default_block(y),
            Generation::Natural(seed) => crate::terrain::block(seed, x, i32::from(y), z),
            Generation::Biomes(seed) => crate::terrain::biome_block(seed, x, i32::from(y), z),
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
            if matches!(self.get(x, candidate_y, z) >> 4, 12 | 13) {
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
            if height == 0 || !matches!(block >> 4, 12 | 13) {
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
            if matches!(id, 8 | 9 | 10 | 11) {
                self.pending_fluid.insert(pos);
            }
            if id == 55 {
                self.pending_circuit.insert(pos);
            }
            if matches!(id, 6 | 18 | 59 | 60 | 141 | 142) {
                self.pending_ecology.insert(pos);
            } else {
                self.pending_ecology.remove(&pos);
            }
            if matches!(
                id,
                6 | 50
                    | 55
                    | 59
                    | 66
                    | 70
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
            if matches!(id, 29 | 33 | 93 | 94) {
                self.pending_devices.insert(pos);
            } else {
                self.pending_devices.remove(&pos);
                self.pending_repeaters.remove(&pos);
            }
        }
    }
    fn wake_leaves(&mut self, x: i32, y: i32, z: i32) {
        for dx in -4..=4 {
            for dz in -4..=4 {
                for dy in -4..=4 {
                    let (nx, ny, nz) = (x + dx, y + dy, z + dz);
                    if in_bounds(nx, ny, nz) && self.get(nx, ny, nz) >> 4 == 18 {
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
        // Lava updates every fourth server tick, water every tick. Delayed
        // lava positions remain queued rather than getting silently dropped.
        let wet: Vec<Position> = self
            .pending_fluid
            .iter()
            .filter(|&&(x, y, z)| {
                !matches!(self.get(x, i32::from(y), z) >> 4,10 | 11) || self.fluid_phase.is_multiple_of(4)
            })
            .take(budget)
            .copied()
            .collect();
        let wired: Vec<Position> = self.pending_circuit.iter().take(budget).copied().collect();
        let supported: Vec<Position> = self.pending_support.iter().take(budget).copied().collect();
        let mut changes: BTreeMap<Position, u16> = self.ecology_changes(budget);
        changes.extend(self.device_changes(budget));

        for &(x, y, z) in &wet {
            let y = i32::from(y);
            let block = self.get(x, y, z);
            let id = match block >> 4 {
                8 | 9 => 8,
                10 | 11 => 10,
                _ => continue,
            };
            let mut level = block & 15;
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
                let mut next = if above_id == id || above_id == id + 1 {
                    ((above & 15) + 1).min(7)
                } else {
                    8
                };
                let sides = [(x - 1, z), (x + 1, z), (x, z - 1), (x, z + 1)];
                let mut source_neighbors = 0;
                for (nx, nz) in sides {
                    let other = self.get(nx, y, nz);
                    if other >> 4 == id || other >> 4 == id + 1 {
                        if other & 15 == 0 {
                            source_neighbors += 1;
                        }
                        next = next.min((other & 15) + 1);
                    }
                }
                let solid_below =
                    y > 0 && !matches!(self.get(x, y - 1, z) >> 4, 0 | 8 | 9 | 10 | 11);
                if id == 8 && source_neighbors >= 2 && solid_below {
                    next = 0;
                }
                if next > 7 {
                    changes.insert((x, y as u8, z), 0);
                    continue;
                }
                if next != level {
                    changes.insert((x, y as u8, z), (id << 4) | next);
                    level = next;
                }
            }
            if level >= 7 {
                continue;
            }
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
            } else {
                vec![(x + 1, y, z), (x - 1, y, z), (x, y, z + 1), (x, y, z - 1)]
            };
            for (nx, ny, nz) in targets {
                if !in_bounds(nx, ny, nz) {
                    continue;
                }
                let existing = self.get(nx, ny, nz);
                let desired = (id << 4) | (level + 1);
                if existing == 0 || (existing >> 4 == id && existing & 15 > level + 1) {
                    let target = (nx, ny as u8, nz);
                    if changes
                        .get(&target)
                        .is_none_or(|&v| v == 0 || (v >> 4 == id && v & 15 > level + 1))
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
                    let signal = match power >> 4 {
                        69 if power & 8 != 0 => 15,
                        76 => 15,
                        55 => (power & 15).saturating_sub(1),
                        94 => 15, // powered repeater
                        _ => 0,
                    };
                    strongest = strongest.max(signal);
                }
            }
            if current & 15 != strongest {
                changes.insert((x, y as u8, z), (55 << 4) | strongest);
            }
        }

        if !changes.is_empty() {
            // Decaying oak leaves may yield a sapling or apple. Stage these
            // before mutation, but expose them only after a successful save.
            let leaf_drops: Vec<WorldDrop> = changes.iter().filter_map(|(&(x,y,z),&next)| {
                let old=self.get(x,i32::from(y),z);
                if next != 0 || old >> 4 != 18 {return None;}
                let pos=(x,i32::from(y),z);
                crate::survival::additional_drop(old,pos,self.fluid_phase)
                    .map(|stack| WorldDrop {pos,stack})
            }).take(256).collect();
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
            self.pending_drops.extend(leaf_drops);
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
        let expired: Vec<_> = self
            .pending_buttons
            .iter()
            .filter(|&(_, when)| self.fluid_phase >= *when)
            .map(|(&pos, _)| pos)
            .collect();
        for pos in expired {
            self.pending_buttons.remove(&pos);
        }
        let mut changed = Vec::with_capacity(changes.len());
        for (x, y, z) in changes.keys().copied() {
            changed.push((x, i32::from(y), z));
            self.wake_block_updates(x, i32::from(y), z);
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
        self.wake_gravity(x, y, z);
        self.wake_block_updates(x, y, z);
        if matches!(previous >> 4, 17 | 162) || matches!(block >> 4, 17 | 162) {
            self.wake_leaves(x, y, z);
        }
        Ok(true)
    }
    /// Apply one parsed block to a *detached* import world. Call save_new only
    /// after every Anvil chunk has been validated; never persist partial imports.
    pub fn import_block(&mut self, x: i32, y: i32, z: i32, block: u16) -> io::Result<()> {
        if !in_bounds(x, y, z) || block >> 4 > 255 {
            return Err(invalid("imported block outside 1.7.10 world limits"));
        }
        let key = (x, y as u8, z);
        if block == self.base_block(x, y as u8, z) {
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
            .open(&staging)?;
        let written = self.write_to(&mut file).and_then(|_| file.sync_all());
        drop(file);
        if let Err(err) = written {
            let _ = fs::remove_file(&staging);
            return Err(err);
        }
        let published = fs::hard_link(&staging, path);
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
            _ => return Err(invalid("unknown IRW world format")),
        };
        let count = u32::from_be_bytes(bytes[offset - 4..offset].try_into().unwrap()) as usize;
        if count > MAX_EDITS || bytes.len() != offset + count * 11 {
            return Err(invalid("world file length/count mismatch"));
        }
        for entry in bytes[offset..].as_chunks::<11>().0 {
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
            if matches!(block >> 4, 12 | 13) {
                world.pending_gravity.insert((x, y, z));
            }
            if matches!(block >> 4, 8 | 9 | 10 | 11) {
                world.pending_fluid.insert((x, y, z));
            }
            if block >> 4 == 55 {
                world.pending_circuit.insert((x, y, z));
            }
            if matches!(block >> 4, 6 | 18 | 59 | 60 | 141 | 142) {
                world.pending_ecology.insert((x, y, z));
            }
            if matches!(
                block >> 4,
                6 | 50
                    | 55
                    | 59
                    | 66
                    | 70
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
                world.pending_support.insert((x, y, z));
            }
            if matches!(block >> 4, 29 | 33 | 93 | 94) {
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
    fn write_to(&self, f: &mut File) -> io::Result<()> {
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
        }
        f.write_all(&(self.edits.len() as u32).to_be_bytes())?;
        for (&(x, y, z), &block) in &self.edits {
            f.write_all(&x.to_be_bytes())?;
            f.write_all(&[y])?;
            f.write_all(&z.to_be_bytes())?;
            f.write_all(&block.to_be_bytes())?;
        }
        Ok(())
    }
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension("tmp");
        let mut f = File::create(&temp)?;
        self.write_to(&mut f)?;
        f.sync_all()?;
        drop(f);
        // Keep the original on filesystems that disallow atomic replacement.
        fs::rename(temp, path)
    }
    /// Serializes the pre-1.8 Section ID/metadata/light arrays in Y-major order.
    /// The first two sections are always present for the flat base ground; other
    /// sections are transmitted when edited. This avoids dropping high-altitude
    /// or underground edits on re-login or crossing a chunk boundary.
    pub fn chunk_data(&self, chunk_x: i32, chunk_z: i32) -> (u16, Vec<u8>) {
        let mut mask: u16 = if self.is_natural() {
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
        // Section-major IDs across every selected section.
        for &section in &sections {
            for y in 0..16 {
                for z in 0..16 {
                    for x in 0..16 {
                        bytes.push(
                            (self.get(chunk_x * 16 + x, section * 16 + y, chunk_z * 16 + z) >> 4)
                                as u8,
                        );
                    }
                }
            }
        }
        // Metadata is a nibble array, two X-adjacent blocks per byte.
        for &section in &sections {
            for y in 0..16 {
                for z in 0..16 {
                    for x in (0..16).step_by(2) {
                        let a = self.get(chunk_x * 16 + x, section * 16 + y, chunk_z * 16 + z)
                            as u8
                            & 15;
                        let b = self.get(chunk_x * 16 + x + 1, section * 16 + y, chunk_z * 16 + z)
                            as u8
                            & 15;
                        bytes.push(a | (b << 4));
                    }
                }
            }
        }
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
    #[test]
    fn sand_gravel_fall_one_cell_per_tick_and_restore_after_reload() {
        let path = std::env::temp_dir().join(format!(
            "ir1710-gravity-{}-{}.irw",
            std::process::id(),
            std::thread::current().name().unwrap_or("world")
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
    fn water_flow_and_circuit_propagation_are_bounded_and_persistent() {
        let path =
            std::env::temp_dir().join(format!("ir1710-fluid-circuit-{}.irw", std::process::id()));
        let _ = fs::remove_file(&path);
        let mut w = World::new();
        // Water above the grass: first tick flows down only.
        w.update_and_save(&path, 5, 66, 5, 8 << 4).unwrap();
        let first = w.block_tick(&path, 64).unwrap();
        assert!(first.contains(&(5, 65, 5)));
        assert_eq!(w.get(5, 65, 5), (8 << 4) | 1);
        assert_eq!(w.get(6, 65, 5), 0);
        // Water flows down y=64 before spreading over the grass at y=63.
        assert!(w.block_tick(&path, 64).unwrap().contains(&(5, 64, 5)));
        assert!(w.block_tick(&path, 64).unwrap().contains(&(6, 64, 5)));
        assert_eq!(w.get(6, 64, 5), (8 << 4) | 3);
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
        assert_eq!(w.get(1, 64, 0), (10 << 4) | 1);
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
