//! Bounded Minecraft 1.7.10 / Protocol 5 ground-item entities.
//!
//! Items persist separately from IRW1 in a capped JSON file. They do not
//! simulate gravity, collisions, liquids or merging yet.
//! Ordering prioritizes preventing duplicate pickups: remove+persist the
//! ground item before writing the receiver's inventory. A storage failure
//! between these two commits can lose an item; a multi-file transaction is
//! needed for full crash consistency.
use crate::inventory::Stack;
use crate::player::PlayerPosition;
use crate::multiplayer::{self, Event};
use crate::world::WORLD_LIMIT;
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

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
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
            && self.position[0].div_euclid(32).abs() <= WORLD_LIMIT
            && (0..=255 * 32).contains(&self.position[1])
            && self.position[2].div_euclid(32).abs() <= WORLD_LIMIT
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
        metadata.extend_from_slice(&(-1i16).to_be_bytes()); // no NBT
        metadata.push(0x7f); // end of metadata
        [
            Event { id: 0x0e, body },
            Event {
                id: 0x1c,
                body: metadata,
            },
        ]
    }
    /// Coordinates used by the multiplayer chunk-visibility index.
    pub fn world_position(&self) -> PlayerPosition {
        PlayerPosition {
            x: f64::from(self.position[0]) / 32.0,
            y: f64::from(self.position[1]) / 32.0,
            z: f64::from(self.position[2]) / 32.0,
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
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GroundItems {
    version: u8,
    next_id: i32,
    entries: BTreeMap<i32, GroundItem>,
    #[serde(skip)]
    eligible_after: BTreeMap<i32, Instant>,
}
impl Default for GroundItems {
    fn default() -> Self {
        Self {
            version: VERSION,
            next_id: 1_000_000_000,
            entries: BTreeMap::new(),
            eligible_after: BTreeMap::new(),
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
        let mut file = File::create(&temporary)?;
        file.write_all(&data)?;
        file.sync_all()?;
        drop(file);
        fs::rename(temporary, path)
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
        let id = self.next_id + 1;
        let position = [block.0 * 32 + 16, block.1 * 32 + 16, block.2 * 32 + 16];
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
        if !self.has_capacity_for(stacks.len()) || stacks.iter().any(|stack| !stack.is_valid()) {
            return Err(invalid("death-drop capacity or stack invalid"));
        }
        let mut candidate = self.clone();
        let mut inserted = Vec::with_capacity(stacks.len());
        let when_ready = Instant::now() + PICKUP_DELAY;
        for &stack in stacks {
            candidate.next_id += 1;
            let item = GroundItem {
                id: candidate.next_id,
                position: [block.0 * 32 + 16, block.1 * 32 + 16, block.2 * 32 + 16],
                stack,
            };
            if !item.valid() {
                return Err(invalid("invalid death-drop location"));
            }
            candidate.entries.insert(item.id, item.clone());
            candidate.eligible_after.insert(item.id, when_ready);
            inserted.push(item);
        }
        candidate.encoded()?;
        Ok((candidate, inserted))
    }

    /// Snapshot an item removal. The caller commits it together with the
    /// receiver's inventory before publishing either in-memory state.
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

    #[cfg(test)]
    pub fn put_many(
        &mut self,
        path: &Path,
        stacks: &[Stack],
        block: (i32, i32, i32),
    ) -> io::Result<Vec<GroundItem>> {
        let (candidate, inserted) = self.stage_put_many(stacks, block)?;
        candidate.save(path)?;
        *self = candidate;
        Ok(inserted)
    }

    pub fn nearby_ready(&self, feet: (f64, f64, f64)) -> Option<GroundItem> {
        self.entries
            .values()
            .find(|item| {
                self.eligible_after
                    .get(&item.id)
                    .is_some_and(|time| Instant::now() >= *time)
                    && item.in_pickup_range(feet)
            })
            .cloned()
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
}
#[cfg(test)]
mod tests {
    use super::*;
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
            },
            Stack {
                id: 1,
                count: 32,
                damage: 0,
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
            },
        };
        assert!(!item.valid());
    }
}
