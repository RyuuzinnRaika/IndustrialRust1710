//! Bounded Minecraft 1.7.10 / Protocol 5 ground-item entities.
//!
//! Items persist separately from IRW1 in a capped JSON file. The 20TPS
//! world worker drives a server-owned gravity step (REG-151 / #856):
//! unsupported drops fall in the fixed-point /32 grid until they reach a
//! solid support, and every landing is persisted before its packets are
//! announced. Liquids and stack merging are not simulated yet.
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
const PICKUP_DELAY: Duration = Duration::from_millis(750);
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
        [
            Event { id: 0x0e, body },
            Event {
                id: 0x1c,
                body: metadata,
            },
        ]
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
        let ready = Instant::now() + PICKUP_DELAY;
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
            .insert(id, Instant::now() + PICKUP_DELAY);
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
    ) -> io::Result<(Self, Vec<GroundItem>)> {
        // Preserve the single-position API's validation even for an empty
        // batch; invalid positions must never be silently accepted.
        drop_position(block)?;
        let positions = stacks
            .iter()
            .copied()
            .map(|stack| (stack, block))
            .collect::<Vec<_>>();
        self.stage_put_at_positions(&positions)
    }

    /// Stage a world-tick batch with distinct item positions. A tick may
    /// detach several attachments in different chunks, so using one block
    /// coordinate for every item would place the drops at the wrong location.
    /// The returned pool and entities stay private until a world+ground
    /// journal transaction has committed successfully.
    pub fn stage_put_at_positions(
        &self,
        entries: &[(Stack, (i32, i32, i32))],
    ) -> io::Result<(Self, Vec<GroundItem>)> {
        if !self.has_capacity_for(entries.len())
            || entries.iter().any(|(stack, _)| !stack.is_valid())
        {
            return Err(invalid("world-drop capacity or stack invalid"));
        }
        let mut candidate = self.clone();
        let mut inserted = Vec::with_capacity(entries.len());
        let ready = Instant::now() + PICKUP_DELAY;
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
        let staged = self.stage_put_many(stacks, block)?;
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

    /// One server-owned 20TPS gravity step (REG-151 / #856). An entity is
    /// supported when the block below its column is solid or its column
    /// already sits at the world floor; supported entities never move.
    /// Unsupported entities accelerate downward in the fixed-point /32
    /// grid, and a landing is committed to the sidecar file before the
    /// matching teleport packets are returned for broadcast. If that save
    /// fails, neither memory nor packets advance, and the next tick simply
    /// retries the identical step. Because every intermediate position is
    /// derived from the persisted one by deterministic integration, a
    /// restart mid-fall replays an equivalent fall into the same column:
    /// entity ids, stacks and counts are never duplicated or lost.
    pub fn advance(&mut self, world: &World, path: &Path) -> io::Result<Vec<Event>> {
        let mut candidate = self.clone();
        let mut packets = Vec::new();
        let mut landed = false;
        for (id, item) in candidate.entries.iter_mut() {
            let x = item.position[0].div_euclid(32);
            let y = item.position[1].div_euclid(32);
            let z = item.position[2].div_euclid(32);
            if y <= 0 || world.get(x, y - 1, z) != 0 {
                candidate.falls.remove(id);
                continue;
            }
            let speed = candidate
                .falls
                .get(id)
                .copied()
                .unwrap_or(0)
                .saturating_add(FALL_ACCEL)
                .min(FALL_TERMINAL);
            // Step no more than one block cell per iteration so a terminal
            // velocity fall can never tunnel through a one-block floor.
            let mut y = item.position[1];
            let mut remain = speed;
            let mut settled = false;
            while remain > 0 && !settled {
                let cell = y.div_euclid(32);
                let step = remain.min(31);
                let next = (y - step).max(FLOOR_OFFSET);
                let next_cell = next.div_euclid(32);
                if next_cell < cell {
                    if next_cell == 0 || world.get(x, next_cell - 1, z) != 0 {
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
                remain -= step;
            }
            if y != item.position[1] {
                item.position[1] = y;
                packets.push(item.teleport());
            }
            if settled {
                candidate.falls.remove(id);
                landed = true;
            } else {
                candidate.falls.insert(*id, speed);
            }
        }
        if packets.is_empty() {
            return Ok(packets);
        }
        if landed {
            candidate.save(path)?;
        }
        *self = candidate;
        Ok(packets)
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
            assert!(initial.stage_put_many(&[item], location).is_err());
            assert!(initial.entries.is_empty());
            assert_eq!(initial.next_id, 1_000_000_000);
            let mut independent = initial.clone();
            assert!(independent
                .put(
                    std::path::Path::new("invalid-drop-must-not-write.json"),
                    item,
                    location
                )
                .is_err());
            assert!(independent.entries.is_empty());
            assert_eq!(independent.next_id, 1_000_000_000);
        }
        for location in [(-WORLD_LIMIT, 0, WORLD_LIMIT - 1), (0, 255, 0)] {
            let (candidate, new_items) = initial.stage_put_many(&[item], location).unwrap();
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
        let (staged, spawned) = ground.stage_put_at_positions(&entries).unwrap();
        assert_eq!(spawned.len(), 2);
        assert_eq!(spawned[0].position, drop_position(entries[0].1).unwrap());
        assert_eq!(spawned[1].position, drop_position(entries[1].1).unwrap());
        assert_eq!(spawned[1].stack.damage, 3);
        assert_eq!(ground.items().count(), 0);
        assert_eq!(staged.items().count(), 2);
        assert!(ground
            .stage_put_at_positions(&[(one, (0, 64, 0)), (two, (WORLD_LIMIT, 64, 0))],)
            .is_err());
        assert_eq!(ground.items().count(), 0);
        assert!(ground.stage_put_many(&[], (WORLD_LIMIT, 64, 0)).is_err());
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
            .stage_put_many(&[apple, cobble], (0, 64, 0))
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
        let (initial, spawned) = original.stage_put_many(&[stack], (0, 65, 0)).unwrap();
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
        let (staged_ground, spawned) = empty_ground.stage_put_many(&[stack], (0, 65, 0)).unwrap();
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
        let item = ground.put(&path, gravity_stack(), (4, 70, 4)).unwrap();
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
        ground.put(&path, gravity_stack(), (4, 64, 4)).unwrap();
        // The world floor is a support too (bounded-world contract).
        ground.put(&path, gravity_stack(), (9, 0, 9)).unwrap();
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
        let spawned = ground.put(&path, gravity_stack(), (9, 100, 9)).unwrap();
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
}
