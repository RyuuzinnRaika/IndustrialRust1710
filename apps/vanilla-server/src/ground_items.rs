//! Bounded Minecraft 1.7.10 / Protocol 5 ground-item entities.
//!
//! Items persist separately from IRW1 in a capped JSON file. The 20TPS
//! world worker drives a server-owned simulation step (REG-151/REG-152 /
//! #856): unsupported drops fall in the fixed-point /32 grid until they
//! reach a solid support, water floats a drop up to the fluid top, lava
//! sinks it slowly without destroying it, and resting drops with the same
//! stack in the same cell merge up to the item stack limit. Every landing
//! or merge is persisted before its packets are announced.
//! Ordering prioritizes preventing duplicate pickups: remove+persist the
//! ground item before writing the receiver's inventory. A storage failure
//! between these two commits can lose an item; a multi-file transaction is
//! needed for full crash consistency.
use crate::inventory::Stack;
use crate::multiplayer::{self, Event};
use crate::world::{World, WORLD_LIMIT};
use legacy_protocol::write_varint;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const MAX_DROPS: usize = 512;
const MAX_BYTES: u64 = 128 * 1024;
/// 1.7.10 pickup delays: block-break and other world-owned drops become
/// collectable after 10 game ticks, Q / Ctrl+Q throws after 40 ticks.
const BLOCK_PICKUP_DELAY: Duration = Duration::from_millis(500);
const THROW_PICKUP_DELAY: Duration = Duration::from_millis(2000);
const VERSION: u8 = 1;
/// Fall speed in 1/32-blocks per game tick. The step accelerates by one
/// unit per tick up to the cap, mirroring vanilla's accelerating item
/// fall without floating-point drift: every accepted step lands exactly on
/// the same /32 grid the wire and sidecar formats already use, so a
/// restart resumes an equivalent fall from the last persisted position.
const FALL_ACCEL: i32 = 1;
const FALL_TERMINAL: i32 = 62;
/// Entity center height (in /32 units) above the block column floor.
const FLOOR_OFFSET: i32 = 16;
/// Simplified fluid transport (simplified 1.7.10 parity, vertical only):
/// a drop inside water rises one whole cell per tick toward the fluid
/// top; a drop inside lava sinks two /32 units per tick and is never
/// destroyed by the fluid itself.
const WATER_RISE: i32 = 32;
const LAVA_SINK: i32 = 2;

/// What created a drop. Java 1.7.10 gives the Item entity different
/// PickupDelay values depending on the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropSource {
    /// Block break, mob loot, death drop, fishing and dropper output:
    /// collectable after 10 game ticks.
    BlockBreak,
    /// C0B drop-held (actions 3/4) and window-click throws (C0E mode 4
    /// and outside-window cursor drops): collectable after 40 game ticks.
    PlayerThrow,
}
impl DropSource {
    fn pickup_delay(self) -> Duration {
        match self {
            DropSource::BlockBreak => BLOCK_PICKUP_DELAY,
            DropSource::PlayerThrow => THROW_PICKUP_DELAY,
        }
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

/// Convert a bounded world cell to the Protocol 5 fixed-point center.
/// Never multiply arbitrary positions supplied by a caller without a bounds
/// check: overflow would panic in debug builds or wrap in optimized builds.
fn drop_position(block: (i32, i32, i32)) -> io::Result<[i32; 3]> {
    if !crate::world::in_bounds(block.0, block.1, block.2) {
        return Err(invalid("ground item location outside world bounds"));
    }
    let coordinate = |value: i32| {
        value
            .checked_mul(32)
            .and_then(|fixed| fixed.checked_add(16))
            .ok_or_else(|| invalid("ground item fixed-point position overflow"))
    };
    Ok([
        coordinate(block.0)?,
        coordinate(block.1)?,
        coordinate(block.2)?,
    ])
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GroundItem {
    pub id: i32,
    pub position: [i32; 3], // Protocol 5 absolute coordinates, fixed point /32
    pub stack: Stack,
}
impl GroundItem {
    fn valid(&self) -> bool {
        self.id > 1_000_000_000
            && self.stack.is_valid()
            && (-WORLD_LIMIT..WORLD_LIMIT).contains(&self.position[0].div_euclid(32))
            && (0..=255).contains(&self.position[1].div_euclid(32))
            && (-WORLD_LIMIT..WORLD_LIMIT).contains(&self.position[2].div_euclid(32))
    }
    /// 1.7.10 Spawn Object: entity VarInt, type 2, fixed-point positions,
    /// pitch, yaw and zero object data. Follow with 0x1c Metadata (slot 10).
    pub fn spawn(&self) -> [Event; 2] {
        let mut body = Vec::with_capacity(22);
        write_varint(self.id as u32, &mut body);
        body.push(2); // dropped-item object type
        for coordinate in self.position {
            body.extend_from_slice(&coordinate.to_be_bytes());
        }
        body.extend_from_slice(&[0, 0]); // pitch/yaw
        body.extend_from_slice(&0i32.to_be_bytes()); // no velocity
        [
            Event { id: 0x0e, body },
            Event {
                id: 0x1c,
                body: self.metadata_body(),
            },
        ]
    }
    /// DataWatcher body carrying only the item slot (index 10). Clients
    /// refresh a stack count from this without a full entity respawn.
    fn metadata_body(&self) -> Vec<u8> {
        let mut metadata = self.id.to_be_bytes().to_vec();
        metadata.push((5 << 5) | 10); // item slot type 5, DataWatcher index 10
        metadata.extend_from_slice(&self.stack.id.to_be_bytes());
        metadata.push(self.stack.count);
        metadata.extend_from_slice(&self.stack.damage.to_be_bytes());
        if let Some(nbt) = self
            .stack
            .nbt
            .and_then(crate::item_nbt::wire)
            .or_else(|| crate::books::wire_nbt(self.stack))
        {
            metadata.extend_from_slice(&(nbt.len() as i16).to_be_bytes());
            metadata.extend_from_slice(&nbt);
        } else {
            metadata.extend_from_slice(&(-1i16).to_be_bytes());
        }
        metadata.push(0x7f); // end of metadata
        metadata
    }
    /// Re-announce only the item slot after a stack merge changed a count.
    fn metadata(&self) -> Event {
        Event {
            id: 0x1c,
            body: self.metadata_body(),
        }
    }
    pub fn in_pickup_range(&self, feet: (f64, f64, f64)) -> bool {
        let x = f64::from(self.position[0]) / 32.0 - feet.0;
        let y = f64::from(self.position[1]) / 32.0 - feet.1;
        let z = f64::from(self.position[2]) / 32.0 - feet.2;
        x * x + y * y + z * z <= 4.0
    }
    pub fn collected(&self, player: i32) -> [Event; 2] {
        let mut collect = self.id.to_be_bytes().to_vec();
        collect.extend_from_slice(&player.to_be_bytes());
        [
            Event {
                id: 0x0d,
                body: collect,
            },
            multiplayer::destroy(self.id),
        ]
    }
    /// 1.7.10 Entity Teleport for this dropped-item object: entity VarInt,
    /// absolute fixed-point /32 coordinates (the same encoding this server
    /// already uses for the spawn packet) and zero look angles.
    fn teleport(&self) -> Event {
        let mut body = Vec::with_capacity(14);
        write_varint(self.id as u32, &mut body);
        for coordinate in self.position {
            body.extend_from_slice(&coordinate.to_be_bytes());
        }
        body.extend_from_slice(&[0, 0]); // yaw/pitch
        Event { id: 0x28, body }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GroundItems {
    version: u8,
    next_id: i32,
    entries: BTreeMap<i32, GroundItem>,
    #[serde(skip)]
    eligible_after: BTreeMap<i32, Instant>,
    /// Runtime fall speed per falling entity in 1/32-blocks per tick.
    /// Never persisted: a restart re-derives the identical landing because
    /// the persisted position stays inside the same block column.
    #[serde(skip)]
    falls: BTreeMap<i32, i32>,
}
impl Default for GroundItems {
    fn default() -> Self {
        Self {
            version: VERSION,
            next_id: 1_000_000_000,
            entries: BTreeMap::new(),
            eligible_after: BTreeMap::new(),
            falls: BTreeMap::new(),
        }
    }
}
impl GroundItems {
    pub fn location(world: &Path) -> PathBuf {
        world.with_extension("drops.json")
    }
    fn validate(&self) -> io::Result<()> {
        if self.version != VERSION
            || self.next_id < 1_000_000_000
            || self.entries.len() > MAX_DROPS
            || self
                .entries
                .iter()
                .any(|(id, item)| *id != item.id || !item.valid() || item.id > self.next_id)
        {
            return Err(invalid("invalid persisted ground items"));
        }
        Ok(())
    }
    pub fn load(path: &Path) -> io::Result<Self> {
        let meta = match fs::symlink_metadata(path) {
            Ok(v) => v,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(err),
        };
        if !meta.file_type().is_file() || meta.len() > MAX_BYTES {
            return Err(invalid("ground-item save file type or size invalid"));
        }
        let data = fs::read(path)?;
        if data.len() as u64 > MAX_BYTES {
            return Err(invalid("ground-item save file grew beyond size cap"));
        }
        let mut state: Self =
            serde_json::from_slice(&data).map_err(|_| invalid("invalid ground-item JSON"))?;
        state.validate()?;
        // The sidecar does not record a drop's source, so a restart cannot
        // restore a player-throw's longer delay. Restored entities get the
        // shorter block-break delay as a floor: never earlier than any live
        // source would allow, and still long enough to close the pickup
        // transaction window before a client has rendered the entity.
        let ready = Instant::now() + BLOCK_PICKUP_DELAY;
        for id in state.entries.keys() {
            state.eligible_after.insert(*id, ready);
        }
        Ok(state)
    }
    pub fn encoded(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| invalid("encode ground-item JSON"))?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid("ground-item save exceeds cap"));
        }
        Ok(bytes)
    }
    fn save(&self, path: &Path) -> io::Result<()> {
        let data = self.encoded()?;
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("drops.tmp");
        let mut file = File::create(&temporary).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "create temporary ground-item save {}: {error}",
                    temporary.display()
                ),
            )
        })?;
        file.write_all(&data)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "replace ground-item save {} from {}: {error}",
                    path.display(),
                    temporary.display()
                ),
            )
        })
    }
    pub fn is_full(&self) -> bool {
        self.entries.len() >= MAX_DROPS || self.next_id == i32::MAX
    }
    pub fn items(&self) -> impl Iterator<Item = &GroundItem> {
        self.entries.values()
    }
    /// Add the drop after an IRW1 edit succeeds. Persist before announcing.
    pub fn put(
        &mut self,
        path: &Path,
        stack: Stack,
        block: (i32, i32, i32),
        source: DropSource,
    ) -> io::Result<GroundItem> {
        if self.is_full() || !stack.is_valid() {
            return Err(invalid("ground-item capacity or stack invalid"));
        }
        let position = drop_position(block)?;
        let id = self.next_id + 1;
        let item = GroundItem {
            id,
            position,
            stack,
        };
        if !item.valid() {
            return Err(invalid("invalid drop position"));
        }
        self.next_id = id;
        self.entries.insert(id, item.clone());
        if let Err(err) = self.save(path) {
            self.entries.remove(&id);
            self.next_id -= 1;
            return Err(err);
        }
        self.eligible_after
            .insert(id, Instant::now() + source.pickup_delay());
        Ok(item)
    }
    /// Reserve capacity before clearing an inventory. The entire death-drop
    /// batch is committed by one file replace, never by per-stack IO.
    pub fn has_capacity_for(&self, count: usize) -> bool {
        count <= MAX_DROPS.saturating_sub(self.entries.len())
            && i64::from(self.next_id) + count as i64 <= i64::from(i32::MAX)
    }

    /// Create an unsaved, validated snapshot for atomic inventory + ground
    /// transactions. This method must not publish any state before journal::commit.
    pub fn stage_put_many(
        &self,
        stacks: &[Stack],
        block: (i32, i32, i32),
        source: DropSource,
    ) -> io::Result<(Self, Vec<GroundItem>)> {
        // Preserve the single-position API's validation even for an empty
        // batch; invalid positions must never be silently accepted.
        drop_position(block)?;
        let positions = stacks
            .iter()
            .copied()
            .map(|stack| (stack, block))
            .collect::<Vec<_>>();
        self.stage_put_at_positions(&positions, source)
    }

    /// Stage a world-tick batch with distinct item positions. A tick may
    /// detach several attachments in different chunks, so using one block
    /// coordinate for every item would place the drops at the wrong location.
    /// The returned pool and entities stay private until a world+ground
    /// journal transaction has committed successfully.
    pub fn stage_put_at_positions(
        &self,
        entries: &[(Stack, (i32, i32, i32))],
        source: DropSource,
    ) -> io::Result<(Self, Vec<GroundItem>)> {
        if !self.has_capacity_for(entries.len())
            || entries.iter().any(|(stack, _)| !stack.is_valid())
        {
            return Err(invalid("world-drop capacity or stack invalid"));
        }
        let mut candidate = self.clone();
        let mut inserted = Vec::with_capacity(entries.len());
        let ready = Instant::now() + source.pickup_delay();
        for &(stack, block) in entries {
            let position = drop_position(block)?;
            candidate.next_id += 1;
            let item = GroundItem {
                id: candidate.next_id,
                position,
                stack,
            };
            if !item.valid() {
                return Err(invalid("invalid world-drop location"));
            }
            candidate.entries.insert(item.id, item.clone());
            candidate.eligible_after.insert(item.id, ready);
            inserted.push(item);
        }
        candidate.encoded()?;
        Ok((candidate, inserted))
    }

    /// Snapshot an item removal. The caller commits it together with the
    /// receiver's inventory before publishing either in-memory state.
    #[cfg(test)]
    pub fn stage_take(&self, id: i32) -> io::Result<Option<(Self, GroundItem)>> {
        let Some(item) = self.entries.get(&id).cloned() else {
            return Ok(None);
        };
        let mut candidate = self.clone();
        candidate.entries.remove(&id);
        candidate.eligible_after.remove(&id);
        candidate.encoded()?;
        Ok(Some((candidate, item)))
    }

    /// Stage a bounded partial pickup without destroying the remaining
    /// ground entity. Callers must atomically journal this snapshot together
    /// with the player's newly credited inventory. An unchanged remainder
    /// keeps its original entity ID and eligibility time.
    pub fn stage_take_amount(
        &self,
        id: i32,
        count: u8,
    ) -> io::Result<Option<(Self, GroundItem, Option<GroundItem>)>> {
        let Some(item) = self.entries.get(&id).cloned() else {
            return Ok(None);
        };
        if count == 0 || count > item.stack.count {
            return Err(invalid("invalid partial ground item pickup count"));
        }
        let mut candidate = self.clone();
        let mut collected = item.clone();
        collected.stack.count = count;
        let remainder = if count == item.stack.count {
            candidate.entries.remove(&id);
            candidate.eligible_after.remove(&id);
            None
        } else {
            let mut remaining = item;
            remaining.stack.count -= count;
            candidate.entries.insert(id, remaining.clone());
            Some(remaining)
        };
        candidate.encoded()?;
        Ok(Some((candidate, collected, remainder)))
    }

    /// Persist an already validated, staged drop batch in one file sync.
    /// The caller only publishes spawn packets after this succeeds.
    pub fn commit_staged_put(
        &mut self,
        path: &Path,
        staged: (Self, Vec<GroundItem>),
    ) -> io::Result<Vec<GroundItem>> {
        let (candidate, inserted) = staged;
        candidate.save(path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("save ground items at {}: {error}", path.display()),
            )
        })?;
        *self = candidate;
        Ok(inserted)
    }

    #[cfg(test)]
    pub fn put_many(
        &mut self,
        path: &Path,
        stacks: &[Stack],
        block: (i32, i32, i32),
    ) -> io::Result<Vec<GroundItem>> {
        let staged = self.stage_put_many(stacks, block, DropSource::BlockBreak)?;
        self.commit_staged_put(path, staged)
    }

    /// Choose the first *receivable* nearby item rather than the first item
    /// alone. A full inventory must not let an unstackable/foreign item block
    /// pickup of a later stack that can still merge into an occupied slot.
    /// Selection is bounded by MAX_DROPS; the caller journals any transfer.
    pub fn nearby_ready_with<T>(
        &self,
        feet: (f64, f64, f64),
        mut select: impl FnMut(&GroundItem) -> Option<T>,
    ) -> Option<(GroundItem, T)> {
        let now = Instant::now();
        for item in self.entries.values() {
            let eligible = self
                .eligible_after
                .get(&item.id)
                .is_some_and(|time| now >= *time);
            if eligible && item.in_pickup_range(feet) {
                if let Some(accepted) = select(item) {
                    return Some((item.clone(), accepted));
                }
            }
        }
        None
    }

    #[cfg(test)]
    pub fn nearby_ready(&self, feet: (f64, f64, f64)) -> Option<GroundItem> {
        self.nearby_ready_with(feet, |_| Some(()))
            .map(|(item, ())| item)
    }
    /// Legacy independent-file path retained for existing unit tests.
    #[cfg(test)]
    pub fn take(&mut self, path: &Path, id: i32) -> io::Result<Option<GroundItem>> {
        let Some(item) = self.entries.remove(&id) else {
            return Ok(None);
        };
        if let Err(err) = self.save(path) {
            self.entries.insert(id, item);
            return Err(err);
        }
        self.eligible_after.remove(&id);
        Ok(Some(item))
    }

    /// One server-owned 20TPS step (REG-151/REG-152 / #856): gravity,
    /// fluid buoyancy and same-cell stack merging.
    ///
    /// Support comes only from solid terrain or the bounded world floor;
    /// water floats an unsupported drop upward one cell per tick until it
    /// rests on the fluid top, and lava sinks it slowly without ever
    /// destroying the stack. Resting drops that share one cell and one
    /// (id, damage, NBT) stack identity merge into the lowest entity id up
    /// to that item's stack limit: absorbed entities emit their destroy
    /// packet and every changed count re-announces its item slot. All
    /// landings and merges are committed to the sidecar file before the
    /// matching packets are returned; if the save fails, neither memory
    /// nor packets advance, and the next tick simply retries the identical
    /// step. Because every intermediate position is derived from the
    /// persisted one by deterministic integration, a restart mid-fall
    /// replays an equivalent move into the same column: entity ids,
    /// stacks and counts are never duplicated or lost.
    pub fn advance(&mut self, world: &World, path: &Path) -> io::Result<Vec<Event>> {
        let mut candidate = self.clone();
        let mut packets = Vec::new();
        let mut persisted = false;
        for (id, item) in candidate.entries.iter_mut() {
            let x = item.position[0].div_euclid(32);
            let y = item.position[1].div_euclid(32);
            let z = item.position[2].div_euclid(32);
            let cell = world.get(x, y, z) >> 4;
            let in_water = matches!(cell, 8 | 9);
            let in_lava = matches!(cell, 10 | 11);
            // Fluids never support an entity; only solid blocks or the
            // bounded world floor do.
            let supported_at = |world: &World, level: i32| -> bool {
                level <= 0 || {
                    let below = world.get(x, level - 1, z) >> 4;
                    below != 0 && !matches!(below, 8..=11)
                }
            };
            // Fluids never support an entity; only solid blocks or the
            // bounded world floor do. A drop resting on solid ground even
            // inside lava stays where it is; only water carries on rising.
            if !in_water && supported_at(world, y) {
                candidate.falls.remove(id);
                continue;
            }
            let next_y;
            let mut settled = false;
            if in_water {
                // Buoyancy: rise one cell while the water column continues;
                // otherwise float on the fluid surface at that cell centre.
                candidate.falls.remove(id);
                if y < 255 && matches!(world.get(x, y + 1, z) >> 4, 8 | 9) {
                    next_y = item.position[1] + WATER_RISE;
                } else {
                    next_y = y * 32 + FLOOR_OFFSET;
                    settled = true; // resting on the fluid surface
                }
            } else {
                // Falling (air) or sinking (lava); one cell per substep so
                // a terminal-velocity fall can never tunnel a one-block floor.
                let speed = if in_lava {
                    LAVA_SINK
                } else {
                    candidate
                        .falls
                        .get(id)
                        .copied()
                        .unwrap_or(0)
                        .saturating_add(FALL_ACCEL)
                        .min(FALL_TERMINAL)
                };
                let mut y = item.position[1];
                let mut remain = speed;
                while remain > 0 && !settled {
                    let cell = y.div_euclid(32);
                    let step = remain.min(31);
                    remain -= step;
                    let next = (y - step).max(FLOOR_OFFSET);
                    let next_cell = next.div_euclid(32);
                    if next_cell < cell {
                        if supported_at(world, next_cell) {
                            y = next_cell * 32 + FLOOR_OFFSET;
                            settled = true;
                        } else {
                            y = next;
                        }
                    } else {
                        y = next;
                    }
                    if next == FLOOR_OFFSET {
                        y = FLOOR_OFFSET;
                        settled = true;
                    }
                }
                next_y = y;
                if settled {
                    candidate.falls.remove(id);
                } else {
                    candidate.falls.insert(*id, speed);
                }
            }
            if next_y != item.position[1] {
                item.position[1] = next_y;
                packets.push(item.teleport());
                if settled {
                    persisted = true;
                }
            } else if settled {
                // Already resting inside (or above) the fluid: nothing
                // moved, so the sidecar already matches. Never rewrite it.
                candidate.falls.remove(id);
            }
        }
        // 1.7.10 stack merging: resting entities that share a cell and a
        // stack identity merge into the lowest entity id, up to the item
        // stack limit. Deterministic order; absorbed ids emit destroy and
        // every changed stack re-announces its item slot.
        // Cell-and-identity key: two drops merge only when they rest in the
        // exact same cell and share id, damage and NBT identity.
        type MergeKey = (i32, i32, i32, i16, i16, Option<u64>);
        let mut groups: BTreeMap<MergeKey, Vec<i32>> = BTreeMap::new();
        for (id, item) in &candidate.entries {
            if candidate.falls.contains_key(id) {
                continue;
            }
            groups
                .entry((
                    item.position[0],
                    item.position[1],
                    item.position[2],
                    item.stack.id,
                    item.stack.damage,
                    item.stack.nbt,
                ))
                .or_default()
                .push(*id);
        }
        for (_stack_identity, mut ids) in groups {
            if ids.len() < 2 {
                continue;
            }
            ids.sort_unstable();
            let cap = usize::from(candidate.entries[&ids[0]].stack.max_count());
            let survivor = ids[0];
            let mut total = usize::from(candidate.entries[&survivor].stack.count);
            for absorbed in ids[1..].iter().copied() {
                let taken =
                    (cap - total).min(usize::from(candidate.entries[&absorbed].stack.count));
                if taken == 0 {
                    break;
                }
                total += taken;
                let left = usize::from(candidate.entries[&absorbed].stack.count) - taken;
                candidate.falls.remove(&absorbed);
                if left == 0 {
                    candidate.entries.remove(&absorbed);
                    candidate.eligible_after.remove(&absorbed);
                    packets.push(multiplayer::destroy(absorbed));
                } else if let Some(entry) = candidate.entries.get_mut(&absorbed) {
                    entry.stack.count = left as u8;
                    packets.push(entry.metadata());
                }
                persisted = true;
            }
            if let Some(entry) = candidate.entries.get_mut(&survivor) {
                if usize::from(entry.stack.count) != total {
                    entry.stack.count = total as u8;
                    packets.push(entry.metadata());
                }
            }
        }
        if packets.is_empty() {
            return Ok(packets);
        }
        if persisted {
            candidate.save(path)?;
        }
        *self = candidate;
        Ok(packets)
    }

    /// Test-only: remaining pickup delay of a pooled entity. Production
    /// pickup goes through [`GroundItems::nearby_ready_with`].
    #[cfg(test)]
    pub fn pickup_remaining(&self, id: i32) -> Option<Duration> {
        self.eligible_after
            .get(&id)
            .map(|ready| ready.saturating_duration_since(Instant::now()))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn s01_out_of_world_drops_cannot_overflow_or_modify_ground_state() {
        let initial = GroundItems::default();
        let item = Stack {
            id: 17,
            count: 1,
            damage: 0,
            nbt: None,
        };
        for location in [
            (i32::MAX, 64, 0),
            (i32::MIN, 64, 0),
            (WORLD_LIMIT, 64, 0),
            (-WORLD_LIMIT - 1, 64, 0),
            (0, -1, 0),
            (0, 256, 0),
            (0, 64, i32::MAX),
        ] {
            assert!(initial
                .stage_put_many(&[item], location, DropSource::BlockBreak)
                .is_err());
            assert!(initial.entries.is_empty());
            assert_eq!(initial.next_id, 1_000_000_000);
            let mut independent = initial.clone();
            assert!(independent
                .put(
                    std::path::Path::new("invalid-drop-must-not-write.json"),
                    item,
                    location,
                    DropSource::BlockBreak
                )
                .is_err());
            assert!(independent.entries.is_empty());
            assert_eq!(independent.next_id, 1_000_000_000);
        }
        for location in [(-WORLD_LIMIT, 0, WORLD_LIMIT - 1), (0, 255, 0)] {
            let (candidate, new_items) = initial
                .stage_put_many(&[item], location, DropSource::BlockBreak)
                .unwrap();
            assert_eq!(new_items.len(), 1);
            assert_eq!(candidate.entries.len(), 1);
            assert_eq!(new_items[0].position, drop_position(location).unwrap());
        }
    }

    #[test]
    fn staged_world_drop_batch_keeps_positions_and_does_not_publish_on_error() {
        let ground = GroundItems::default();
        let one = Stack {
            id: 50,
            count: 1,
            damage: 0,
            nbt: None,
        };
        let two = Stack {
            id: 171,
            count: 1,
            damage: 3,
            nbt: None,
        };
        let entries = [(one, (-3, 64, 5)), (two, (9, 65, -8))];
        let (staged, spawned) = ground
            .stage_put_at_positions(&entries, DropSource::BlockBreak)
            .unwrap();
        assert_eq!(spawned.len(), 2);
        assert_eq!(spawned[0].position, drop_position(entries[0].1).unwrap());
        assert_eq!(spawned[1].position, drop_position(entries[1].1).unwrap());
        assert_eq!(spawned[1].stack.damage, 3);
        assert_eq!(ground.items().count(), 0);
        assert_eq!(staged.items().count(), 2);
        assert!(ground
            .stage_put_at_positions(
                &[(one, (0, 64, 0)), (two, (WORLD_LIMIT, 64, 0))],
                DropSource::BlockBreak,
            )
            .is_err());
        assert_eq!(ground.items().count(), 0);
        assert!(ground
            .stage_put_many(&[], (WORLD_LIMIT, 64, 0), DropSource::BlockBreak)
            .is_err());
    }

    #[test]
    fn s01_compatible_ground_stack_is_not_blocked_by_earlier_unreceivable_item() {
        use crate::inventory::Inventory;

        let apple = Stack {
            id: 260,
            count: 1,
            damage: 0,
            nbt: None,
        };
        let cobble = Stack {
            id: 4,
            count: 2,
            damage: 0,
            nbt: None,
        };
        let (mut ground, spawned) = GroundItems::default()
            .stage_put_many(&[apple, cobble], (0, 64, 0), DropSource::BlockBreak)
            .unwrap();
        for item in &spawned {
            ground
                .eligible_after
                .insert(item.id, Instant::now() - Duration::from_secs(1));
        }

        let mut inventory = Inventory::default();
        for slot in 9..45 {
            assert!(inventory
                .creative_set(
                    slot,
                    Some(Stack {
                        id: 3,
                        count: 64,
                        damage: 0,
                        nbt: None,
                    }),
                )
                .unwrap());
        }
        // No empty slots; the earlier apple is impossible to accept. The
        // later cobble may still merge one item into the existing hotbar.
        assert!(inventory
            .creative_set(
                36,
                Some(Stack {
                    id: 4,
                    count: 63,
                    damage: 0,
                    nbt: None,
                }),
            )
            .unwrap());
        let (selected, (credited, amount, slots)) = ground
            .nearby_ready_with((0.5, 64.0, 0.5), |item| {
                let mut next = inventory.clone();
                next.receive_stack_partial(item.stack)
                    .map(|(count, changed)| (next, count, changed))
            })
            .expect("compatible later stack must not be starved");
        assert_eq!(selected.id, spawned[1].id);
        assert_eq!(amount, 1);
        assert_eq!(slots, vec![36]);
        assert_eq!(credited.held().unwrap().count, 64);
        assert_eq!(inventory.held().unwrap().count, 63);
        let (staged, picked, remaining) = ground
            .stage_take_amount(selected.id, amount)
            .unwrap()
            .unwrap();
        assert_eq!(picked.stack.count, 1);
        assert_eq!(remaining.unwrap().stack.count, 1);
        assert_eq!(staged.entries.get(&spawned[0].id).unwrap().stack, apple);
        assert_eq!(staged.entries.get(&spawned[1].id).unwrap().stack.count, 1);
    }

    #[test]
    fn s01_unready_or_distant_drops_never_reach_the_inventory_selector() {
        let (mut drops, added) = GroundItems::default()
            .stage_put_many(
                &[Stack {
                    id: 17,
                    count: 1,
                    damage: 0,
                    nbt: None,
                }],
                (0, 64, 0),
                DropSource::BlockBreak,
            )
            .unwrap();
        let id = added[0].id;
        let mut called = false;
        assert!(drops
            .nearby_ready_with((0.5, 64.0, 0.5), |_| {
                called = true;
                Some(())
            })
            .is_none());
        assert!(!called);
        drops
            .eligible_after
            .insert(id, Instant::now() - Duration::from_secs(1));
        assert!(drops
            .nearby_ready_with((10.5, 64.0, 0.5), |_| {
                called = true;
                Some(())
            })
            .is_none());
        assert!(!called);
        assert_eq!(
            drops
                .nearby_ready_with((0.5, 64.0, 0.5), |_| Some(()))
                .unwrap()
                .0
                .id,
            id
        );
    }

    #[test]
    fn partial_take_keeps_same_item_entity_and_never_loses_remainder() {
        let original = GroundItems::default();
        let stack = Stack {
            id: 260,
            count: 5,
            damage: 0,
            nbt: None,
        };
        let (initial, spawned) = original
            .stage_put_many(&[stack], (0, 65, 0), DropSource::BlockBreak)
            .unwrap();
        let id = spawned[0].id;
        assert!(initial.stage_take_amount(id, 0).is_err());
        assert!(initial.stage_take_amount(id, 6).is_err());
        let (partial, picked, remainder) = initial.stage_take_amount(id, 2).unwrap().unwrap();
        assert_eq!(picked.stack.count, 2);
        let remaining = remainder.unwrap();
        assert_eq!(remaining.id, id);
        assert_eq!(remaining.stack.count, 3);
        assert_eq!(partial.entries.get(&id), Some(&remaining));
        assert_eq!(
            partial.eligible_after.get(&id),
            initial.eligible_after.get(&id)
        );
        assert_eq!(initial.entries.get(&id).unwrap().stack.count, 5);
        let (empty, rest, no_remainder) = partial.stage_take_amount(id, 3).unwrap().unwrap();
        assert_eq!(rest.stack.count, 3);
        assert!(no_remainder.is_none());
        assert!(!empty.entries.contains_key(&id));
        assert!(!empty.eligible_after.contains_key(&id));
    }

    #[test]
    fn inventory_and_ground_moves_use_one_recoverable_journal() {
        use crate::inventory::Inventory;
        use crate::journal;

        let dir =
            std::env::temp_dir().join(format!("ir1710-ground-journal-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("players")).unwrap();
        let inventory_file = dir.join("players/rust.inventory.json");
        let ground_file = dir.join("flat.drops.json");
        let pending_file = dir.join("flat.pending-txn.json");
        let stack = Stack {
            id: 4,
            count: 7,
            damage: 0,
            nbt: None,
        };
        let mut source = Inventory::default();
        assert!(source.creative_set(36, Some(stack)).unwrap());
        source.save(&inventory_file).unwrap();

        let empty_ground = GroundItems::default();
        let (staged_ground, spawned) = empty_ground
            .stage_put_many(&[stack], (0, 65, 0), DropSource::BlockBreak)
            .unwrap();
        let mut debited = source.clone();
        debited.clear_possessions();
        journal::commit(
            &pending_file,
            &[
                (&inventory_file, debited.encoded().unwrap()),
                (&ground_file, staged_ground.encoded().unwrap()),
            ],
        )
        .unwrap();
        assert!(!pending_file.exists());
        assert!(Inventory::load(&inventory_file)
            .unwrap()
            .possessions()
            .is_empty());
        let loaded_ground = GroundItems::load(&ground_file).unwrap();
        assert_eq!(loaded_ground.items().next().unwrap().stack, stack);

        let (taken_ground, claimed) = loaded_ground.stage_take(spawned[0].id).unwrap().unwrap();
        let mut credited = debited.clone();
        assert!(credited.receive_stack(claimed.stack).is_some());
        journal::commit(
            &pending_file,
            &[
                (&ground_file, taken_ground.encoded().unwrap()),
                (&inventory_file, credited.encoded().unwrap()),
            ],
        )
        .unwrap();
        journal::recover(&pending_file).unwrap();
        let final_inventory = Inventory::load(&inventory_file).unwrap();
        assert_eq!(final_inventory.possessions(), vec![stack]);
        assert_eq!(GroundItems::load(&ground_file).unwrap().items().count(), 0);
        assert!(taken_ground.stage_take(spawned[0].id).unwrap().is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn batch_death_drop_is_atomic_and_preserves_stacks() {
        let dir = std::env::temp_dir().join(format!("ir1710-drops-batch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("drops.json");
        let mut drops = GroundItems::default();
        let stacks = [
            Stack {
                id: 260,
                count: 5,
                damage: 0,
                nbt: None,
            },
            Stack {
                id: 1,
                count: 32,
                damage: 0,
                nbt: None,
            },
        ];
        assert!(drops.has_capacity_for(2));
        let items = drops.put_many(&file, &stacks, (0, 65, 0)).unwrap();
        assert_eq!(items.len(), 2);
        let restored = GroundItems::load(&file).unwrap();
        let values: Vec<_> = restored.items().map(|item| item.stack).collect();
        assert_eq!(values, stacks);
        let missing_parent = file.join("world.json");
        assert!(drops
            .put_many(&missing_parent, &stacks, (0, 65, 0))
            .is_err());
        assert_eq!(drops.items().count(), 2);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn protocol5_item_packets_and_range() {
        let item = GroundItem {
            id: 1_000_000_001,
            position: [16, 2032, 16],
            stack: Stack {
                id: 3,
                count: 1,
                damage: 0,
                nbt: None,
            },
        };
        let [object, metadata] = item.spawn();
        assert_eq!(object.id, 0x0e);
        assert_eq!(object.body[5], 2);
        assert_eq!(object.body.len(), 24);
        assert_eq!(metadata.id, 0x1c);
        assert_eq!(&metadata.body[4..], &[0xaa, 0, 3, 1, 0, 0, 255, 255, 127]);
        let [collect, destroy] = item.collected(42);
        assert_eq!(collect.id, 0x0d);
        assert_eq!(&collect.body[4..], &42i32.to_be_bytes());
        assert_eq!(destroy.id, 0x13);
        assert!(item.in_pickup_range((0.5, 65.0, 0.5)));
        assert!(!item.in_pickup_range((4.5, 65.0, 0.5)));
    }
    #[test]
    fn persists_and_refuses_duplicate_pickup() {
        let root = std::env::temp_dir().join(format!("ir1710-ground-items-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("drops.json");
        let mut state = GroundItems::default();
        let item = state
            .put(
                &path,
                Stack {
                    id: 3,
                    count: 1,
                    damage: 0,
                    nbt: None,
                },
                (0, 63, 0),
                DropSource::BlockBreak,
            )
            .unwrap();
        assert_eq!(GroundItems::load(&path).unwrap().items().count(), 1);
        assert_eq!(state.take(&path, item.id).unwrap(), Some(item.clone()));
        assert_eq!(state.take(&path, item.id).unwrap(), None);
        assert_eq!(GroundItems::load(&path).unwrap().items().count(), 0);
        fs::write(&path, b"{corrupt").unwrap();
        assert!(GroundItems::load(&path).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    /// ADD-018 / #442: when two player sessions run their pickup transaction
    /// concurrently against one shared, locked store, a single dropped Item
    /// Entity must commit exactly once. The authoritative order (stage the
    /// removal, then publish the in-memory snapshot) is taken under one
    /// `Mutex<GroundItems>` guard, so the second session observes the already
    /// removed entity and credits nothing. This races that exact pattern and
    /// asserts the entity is credited once, removed once, and never duplicated.
    #[test]
    fn two_concurrent_pickups_commit_a_single_entity_exactly_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier, Mutex};
        let root = std::env::temp_dir().join(format!("ir1710-ground-race-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("drops.json");
        let store = Arc::new(Mutex::new(GroundItems::default()));
        let id = store
            .lock()
            .unwrap()
            .put(
                &path,
                Stack {
                    id: 3,
                    count: 8,
                    damage: 0,
                    nbt: None,
                },
                (0, 63, 0),
                DropSource::BlockBreak,
            )
            .unwrap()
            .id;

        let barrier = Arc::new(Barrier::new(2));
        let granted = Arc::new(AtomicUsize::new(0));
        let commits = Arc::new(AtomicUsize::new(0));
        let mut players = Vec::new();
        for _ in 0..2 {
            let store = Arc::clone(&store);
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            let granted = Arc::clone(&granted);
            let commits = Arc::clone(&commits);
            players.push(std::thread::spawn(move || {
                barrier.wait();
                let mut ground = store.lock().unwrap();
                if let Some((staged, collected, _remainder)) =
                    ground.stage_take_amount(id, 8).unwrap()
                {
                    staged.save(&path).unwrap();
                    *ground = staged;
                    granted.fetch_add(collected.stack.count as usize, Ordering::SeqCst);
                    commits.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }
        for player in players {
            player.join().unwrap();
        }
        // Exactly one session commits and receives the whole 8-item stack;
        // the other sees the entity already gone and credits nothing.
        assert_eq!(commits.load(Ordering::SeqCst), 1);
        assert_eq!(granted.load(Ordering::SeqCst), 8);
        // The entity is removed from both the live store and the snapshot.
        assert_eq!(store.lock().unwrap().items().count(), 0);
        assert_eq!(GroundItems::load(&path).unwrap().items().count(), 0);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn full_stack_drop_roundtrips_and_uses_protocol5_count() {
        let root = std::env::temp_dir().join(format!("ir1710-ground-stack-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let file = root.join("drops.json");
        let mut drops = GroundItems::default();
        let item = drops
            .put(
                &file,
                Stack {
                    id: 3,
                    count: 32,
                    damage: 0,
                    nbt: None,
                },
                (1, 65, -1),
                DropSource::BlockBreak,
            )
            .unwrap();
        assert_eq!(
            GroundItems::load(&file)
                .unwrap()
                .items()
                .next()
                .unwrap()
                .stack
                .count,
            32
        );
        let [_, metadata] = item.spawn();
        assert_eq!(metadata.body[7], 32);
        assert_eq!(drops.take(&file, item.id).unwrap(), Some(item));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refuses_invalid_items_and_corrupt_metadata() {
        let state = GroundItems::default();
        assert!(state.nearby_ready((0.5, 64.0, 0.5)).is_none());
        let item = GroundItem {
            id: 1000000001,
            position: [i32::MAX, 64 * 32, 16],
            stack: Stack {
                id: 3,
                count: 1,
                damage: 0,
                nbt: None,
            },
        };
        assert!(!item.valid());
    }

    fn gravity_world() -> World {
        World::new()
    }

    fn gravity_dir(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "ir1710-ground-gravity-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn gravity_stack() -> Stack {
        Stack {
            id: 17,
            count: 3,
            damage: 0,
            nbt: None,
        }
    }

    fn teleport_y(packet: &Event) -> i32 {
        assert_eq!(packet.id, 0x28);
        // VarInt id (>= 1e9 occupies 5 bytes) then absolute x, y, z i32.
        assert_eq!(packet.body.len(), 19);
        i32::from_be_bytes(packet.body[9..13].try_into().unwrap())
    }

    /// REG-151 / #856 (gravity part): a drop detached from its support
    /// falls every 20TPS step and lands, with the landing persisted before
    /// the landing packet is announced. A supported drop never moves.
    #[test]
    fn unsupported_drops_fall_and_land_with_teleport_packets() {
        let world = gravity_world();
        let path = gravity_dir("fall").join("drops.json");
        let mut ground = GroundItems::default();
        let item = ground
            .put(&path, gravity_stack(), (4, 70, 4), DropSource::BlockBreak)
            .unwrap();
        assert_eq!(item.position, [4 * 32 + 16, 70 * 32 + 16, 4 * 32 + 16]);
        // Flat terrain tops out at y=63, so a free entity rests at 64.5.
        const REST_Y: i32 = 64 * 32 + 16;
        let mut previous = item.position[1];
        let mut teleports = 0;
        for _ in 0..200 {
            let packets = ground.advance(&world, &path).unwrap();
            if packets.is_empty() {
                break;
            }
            assert_eq!(packets.len(), 1, "one entity moves one step");
            let y = teleport_y(&packets[0]);
            assert!(y < previous, "falls strictly decrease Y: {y} !< {previous}");
            previous = y;
            teleports += 1;
        }
        assert!(
            teleports > 3,
            "a six-block fall takes several announced steps"
        );
        let entries: Vec<&GroundItem> = ground.items().collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].position, [4 * 32 + 16, REST_Y, 4 * 32 + 16]);
        assert_eq!(entries[0].id, item.id, "landing never re-ids an entity");
        // The sidecar holds the landed position, and a further tick is a
        // no-op: resting items emit neither motion nor storage writes.
        let persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(persisted["entries"].as_object().unwrap().len(), 1);
        let stored_y = persisted["entries"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap()["position"][1]
            .as_i64()
            .unwrap();
        assert_eq!(stored_y, i64::from(REST_Y));
        assert!(ground.advance(&world, &path).unwrap().is_empty());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// A drop whose support survived the tick, and a drop already sitting
    /// on the column floor, must never move or rewrite the sidecar.
    #[test]
    fn supported_drops_never_move_under_gravity() {
        let world = gravity_world();
        let path = gravity_dir("rest").join("drops.json");
        let mut ground = GroundItems::default();
        // Grass at y=63 supports the drop at y=64.
        ground
            .put(&path, gravity_stack(), (4, 64, 4), DropSource::BlockBreak)
            .unwrap();
        // The world floor is a support too (bounded-world contract).
        ground
            .put(&path, gravity_stack(), (9, 0, 9), DropSource::BlockBreak)
            .unwrap();
        for _ in 0..5 {
            assert!(ground.advance(&world, &path).unwrap().is_empty());
        }
        let ys: Vec<i32> = ground.items().map(|item| item.position[1]).collect();
        assert_eq!(ys, vec![64 * 32 + 16, 16]);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// A restart while airborne must replay an equivalent fall in the same
    /// column: identical entity id, identical final height, no duplicates
    /// and no losses.
    #[test]
    fn a_restart_mid_fall_resumes_the_same_column_without_duplication_or_loss() {
        let world = gravity_world();
        let path = gravity_dir("restart").join("drops.json");
        let mut ground = GroundItems::default();
        let spawned = ground
            .put(&path, gravity_stack(), (9, 100, 9), DropSource::BlockBreak)
            .unwrap();
        let mut airborne = 0;
        for _ in 0..3 {
            if !ground.advance(&world, &path).unwrap().is_empty() {
                airborne += 1;
            }
        }
        assert_eq!(airborne, 3, "the first three steps must move");
        // Mid-fall reload: only the spawn height is on disk so far.
        let restored = GroundItems::load(&path).unwrap();
        assert_eq!(restored.items().count(), 1);
        let restored = restored.items().next().unwrap();
        assert_eq!(restored.id, spawned.id);
        assert_eq!(restored.position[1], 100 * 32 + 16);
        let mut restored = GroundItems::load(&path).unwrap();
        for _ in 0..200 {
            if restored.advance(&world, &path).unwrap().is_empty() {
                break;
            }
        }
        let entries: Vec<&GroundItem> = restored.items().collect();
        assert_eq!(entries.len(), 1, "a restart never duplicates a drop");
        assert_eq!(entries[0].id, spawned.id);
        assert_eq!(entries[0].stack, gravity_stack(), "the stack is intact");
        assert_eq!(entries[0].position[1], 64 * 32 + 16, "same landing");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// REG-152 / #856 (fluids part): water carries an unsupported drop up
    /// to the top fluid cell and lets it float; lava sinks a drop slowly
    /// and NEVER destroys the stack. Fluids are never a support: an item
    /// above a water column keeps falling into the water instead of
    /// resting on the surface.
    #[test]
    fn water_floats_a_drop_to_the_fluid_top_and_lava_sinks_it_without_destroying_it() {
        let world_file = gravity_dir("fluids").join("fluids.irw");
        let mut world = World::new();
        // A 3-block water column above the flat surface (grass at y=63).
        for level in 64..=66 {
            world
                .update_and_save(&world_file, 5, level, 5, 8 << 4)
                .unwrap();
        }
        // A 2-block lava column with solid ground under it.
        world
            .update_and_save(&world_file, 7, 65, 7, 10 << 4)
            .unwrap();
        world
            .update_and_save(&world_file, 7, 64, 7, 10 << 4)
            .unwrap();
        let path = world_file.with_extension("drops.json");
        let mut ground = GroundItems::default();
        let floater = ground
            .put(&path, gravity_stack(), (5, 64, 5), DropSource::BlockBreak)
            .unwrap();
        let sinker = ground
            .put(&path, gravity_stack(), (7, 65, 7), DropSource::BlockBreak)
            .unwrap();
        // The lava drop sinks slowly: 32 /32-units need at least 16 ticks,
        // so it must NOT have landed on the first two steps.
        for _ in 0..2 {
            let packets = ground.advance(&world, &path).unwrap();
            assert!(!packets.is_empty(), "both drops move on early ticks");
            for packet in &packets {
                assert_eq!(packet.id, 0x28, "fluid motion uses teleports");
            }
        }
        let sink_y = ground
            .items()
            .find(|item| item.id == sinker.id)
            .unwrap()
            .position[1];
        assert!(
            sink_y > 64 * 32 + 16,
            "lava sinks slowly: {sink_y} still above the floor cell"
        );
        // Run to equilibrium.
        for _ in 0..400 {
            if ground.advance(&world, &path).unwrap().is_empty() {
                break;
            }
        }
        let final_items: std::collections::BTreeMap<i32, &GroundItem> =
            ground.items().map(|item| (item.id, item)).collect();
        // Water drop floats on the TOP fluid cell centre (66.5 → 2128).
        assert_eq!(
            final_items[&floater.id].position[1],
            66 * 32 + FLOOR_OFFSET,
            "a water drop rests at the fluid top"
        );
        // Lava drop rests inside the lowest lava cell (64.5), unharmed.
        let rested = final_items[&sinker.id];
        assert_eq!(rested.position[1], 64 * 32 + FLOOR_OFFSET);
        assert_eq!(rested.stack, gravity_stack(), "lava never destroys drops");
        assert_eq!(ground.items().count(), 2, "no fluid entity leakage");
        let _ = fs::remove_dir_all(world_file.parent().unwrap());
    }

    /// A water surface is NOT a support: a drop above the column falls into
    /// the water and ends afloat at the fluid top, not on the surface. A
    /// distinct item id keeps it from merging with the other floater.
    #[test]
    fn a_drop_above_water_falls_in_and_floats_at_the_fluid_top() {
        let world_file = gravity_dir("float").join("fluid.irw");
        let mut world = World::new();
        for level in 64..=66 {
            world
                .update_and_save(&world_file, 5, level, 5, 8 << 4)
                .unwrap();
        }
        let path = world_file.with_extension("drops.json");
        let mut ground = GroundItems::default();
        let dropped = ground
            .put(
                &path,
                Stack {
                    id: 4,
                    count: 1,
                    damage: 0,
                    nbt: None,
                },
                (5, 68, 5),
                DropSource::BlockBreak,
            )
            .unwrap();
        // Immediately airborne (the cell is water, so it is unsupported).
        ground.advance(&world, &path).unwrap();
        let landed = ground.items().find(|item| item.id == dropped.id).unwrap();
        assert!(
            landed.position[1] < 68 * 32 + FLOOR_OFFSET,
            "a water surface is not a support"
        );
        for _ in 0..400 {
            if ground.advance(&world, &path).unwrap().is_empty() {
                break;
            }
        }
        assert_eq!(
            ground
                .items()
                .find(|item| item.id == dropped.id)
                .unwrap()
                .position[1],
            66 * 32 + FLOOR_OFFSET,
            "the surface drop ends afloat at the fluid top"
        );
        let _ = fs::remove_dir_all(world_file.parent().unwrap());
    }

    /// REG-152 / #856 (merging part): resting drops in the same cell with
    /// an identical (id, damage, NBT) stack merge up to the stack limit.
    /// A fully absorbed entity emits a destroy packet, every changed count
    /// is re-announced, and the sidecar is persisted before the packets.
    #[test]
    fn resting_identical_stacks_merge_up_to_the_stack_limit() {
        let world = gravity_world();
        let path = gravity_dir("merge").join("drops.json");
        let mut ground = GroundItems::default();
        // 60 + 4 exactly fills one 64-stack: the higher entity is absorbed.
        let survivor = ground
            .put(
                &path,
                Stack {
                    id: 3,
                    count: 60,
                    damage: 0,
                    nbt: None,
                },
                (2, 64, 2),
                DropSource::BlockBreak,
            )
            .unwrap();
        let absorbed = ground
            .put(
                &path,
                Stack {
                    id: 3,
                    count: 4,
                    damage: 0,
                    nbt: None,
                },
                (2, 64, 2),
                DropSource::BlockBreak,
            )
            .unwrap();
        // 40 + 50 exceeds one stack: both entities survive, split 64 + 16.
        ground
            .put(
                &path,
                Stack {
                    id: 4,
                    count: 40,
                    damage: 0,
                    nbt: None,
                },
                (3, 64, 3),
                DropSource::BlockBreak,
            )
            .unwrap();
        ground
            .put(
                &path,
                Stack {
                    id: 4,
                    count: 50,
                    damage: 0,
                    nbt: None,
                },
                (3, 64, 3),
                DropSource::BlockBreak,
            )
            .unwrap();
        let packets = ground.advance(&world, &path).unwrap();
        let destroys: Vec<&Event> = packets.iter().filter(|e| e.id == 0x13).collect();
        assert_eq!(destroys.len(), 1, "only the fully absorbed entity dies");
        assert_eq!(
            i32::from_be_bytes(destroys[0].body[1..5].try_into().unwrap()),
            absorbed.id,
            "the higher id is absorbed"
        );
        assert_eq!(
            packets.iter().filter(|e| e.id == 0x1c).count(),
            3,
            "survivor 64, split remainder 16 and split survivor 64 re-announce"
        );
        let items: Vec<&GroundItem> = ground.items().collect();
        assert_eq!(items.len(), 3, "90 dirt cannot fit in one 64-stack");
        let counts: Vec<u8> = items.iter().map(|item| item.stack.count).collect();
        assert_eq!(
            counts.iter().map(|count| *count as usize).sum::<usize>(),
            154
        );
        assert_eq!(counts.iter().filter(|count| **count == 64).count(), 2);
        assert!(
            items
                .iter()
                .any(|item| item.id == survivor.id && item.stack.count == 64),
            "the lowest id fills first"
        );
        assert!(!items.iter().any(|item| item.id == absorbed.id));
        // The sidecar matches the merged state before clients saw packets.
        let persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(persisted["entries"].as_object().unwrap().len(), 3);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// Drops that merely share a cell must NOT merge across item type,
    /// damage value, or NBT identity. Only the stack limit splits a merge
    /// into two surviving entities.
    #[test]
    fn different_items_damage_or_nbt_never_merge() {
        let world = gravity_world();
        let path = gravity_dir("merge-identity").join("drops.json");
        let mut ground = GroundItems::default();
        ground
            .put(
                &path,
                Stack {
                    id: 3,
                    count: 1,
                    damage: 0,
                    nbt: None,
                },
                (6, 64, 6),
                DropSource::BlockBreak,
            )
            .unwrap();
        ground
            .put(
                &path,
                Stack {
                    id: 12,
                    count: 1,
                    damage: 0,
                    nbt: None,
                },
                (6, 64, 6),
                DropSource::BlockBreak,
            )
            .unwrap();
        ground
            .put(
                &path,
                Stack {
                    id: 276,
                    count: 1,
                    damage: 3,
                    nbt: None,
                },
                (6, 64, 6),
                DropSource::BlockBreak,
            )
            .unwrap();
        ground
            .put(
                &path,
                Stack {
                    id: 276,
                    count: 1,
                    damage: 3,
                    nbt: None,
                },
                (6, 64, 6),
                DropSource::BlockBreak,
            )
            .unwrap();
        // Different NBT identity: two written books must never merge.
        let tag_a = fastnbt::Value::Compound(std::collections::HashMap::from([(
            "t".to_string(),
            fastnbt::Value::String("a".to_string()),
        )]));
        let tag_b = fastnbt::Value::Compound(std::collections::HashMap::from([(
            "t".to_string(),
            fastnbt::Value::String("b".to_string()),
        )]));
        let nbt_a = crate::item_nbt::from_value(&tag_a).unwrap();
        let nbt_b = crate::item_nbt::from_value(&tag_b).unwrap();
        ground
            .put(
                &path,
                Stack {
                    id: 340,
                    count: 1,
                    damage: 0,
                    nbt: Some(nbt_a),
                },
                (6, 64, 6),
                DropSource::BlockBreak,
            )
            .unwrap();
        ground
            .put(
                &path,
                Stack {
                    id: 340,
                    count: 1,
                    damage: 0,
                    nbt: Some(nbt_b),
                },
                (6, 64, 6),
                DropSource::BlockBreak,
            )
            .unwrap();
        let packets = ground.advance(&world, &path).unwrap();
        assert!(
            packets.is_empty(),
            "no merges across identities: {packets:?}"
        );
        assert_eq!(ground.items().count(), 6);
        // Identical enchanted swords (max stack 1) still never merge.
        let mut swords = GroundItems::default();
        swords
            .put(
                &path,
                Stack {
                    id: 276,
                    count: 1,
                    damage: 0,
                    nbt: None,
                },
                (7, 64, 7),
                DropSource::BlockBreak,
            )
            .unwrap();
        swords
            .put(
                &path,
                Stack {
                    id: 276,
                    count: 1,
                    damage: 0,
                    nbt: None,
                },
                (7, 64, 7),
                DropSource::BlockBreak,
            )
            .unwrap();
        assert!(
            swords.advance(&world, &path).unwrap().is_empty(),
            "cap-1 items never merge"
        );
        assert_eq!(swords.items().count(), 2);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// REG-152 / #856 (pickup delay part): 1.7.10 PickupDelay depends on
    /// the drop source. Block-break drops become collectable after 10
    /// ticks; Q / Ctrl+Q throws after 40 ticks.
    #[test]
    fn pickup_delay_follows_the_drop_source() {
        let path = gravity_dir("delay").join("drops.json");
        let mut ground = GroundItems::default();
        let mined = ground
            .put(&path, gravity_stack(), (8, 64, 8), DropSource::BlockBreak)
            .unwrap();
        let thrown = ground
            .put(&path, gravity_stack(), (8, 64, 9), DropSource::PlayerThrow)
            .unwrap();
        let mined_delay = ground.pickup_remaining(mined.id).unwrap();
        let thrown_delay = ground.pickup_remaining(thrown.id).unwrap();
        assert!(
            mined_delay < Duration::from_millis(900),
            "block breaks become collectable quickly: {mined_delay:?}"
        );
        assert!(
            thrown_delay > Duration::from_millis(1_500),
            "throws honour the 40-tick delay: {thrown_delay:?}"
        );
        assert!(thrown_delay > mined_delay, "sources are distinguished");
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
    /// REG-153 / #856: the world store holds at most MAX_DROPS entities.
    /// A full store must refuse new single or batch drops without any
    /// mutation, and collecting one entity must free exactly one slot.
    #[test]
    fn the_store_refuses_drops_beyond_the_entity_cap() {
        let path = gravity_dir("cap").join("drops.json");
        let mut ground = GroundItems::default();
        for column in 0..MAX_DROPS as i32 {
            ground
                .put(
                    &path,
                    gravity_stack(),
                    (column, 64, 0),
                    DropSource::BlockBreak,
                )
                .unwrap();
        }
        assert_eq!(ground.entries.len(), MAX_DROPS);
        assert!(!ground.has_capacity_for(1));
        assert!(ground.has_capacity_for(0));
        assert!(ground
            .stage_put_many(&[gravity_stack()], (0, 64, 1), DropSource::BlockBreak)
            .is_err());
        assert!(ground
            .stage_put_many(
                &[gravity_stack(), gravity_stack()],
                (1, 64, 1),
                DropSource::BlockBreak
            )
            .is_err());
        assert_eq!(ground.entries.len(), MAX_DROPS, "rejections do not mutate");
        assert!(ground.take(&path, 1_000_000_001).unwrap().is_some());
        assert!(ground.has_capacity_for(1));
        assert!(!ground.has_capacity_for(2));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
