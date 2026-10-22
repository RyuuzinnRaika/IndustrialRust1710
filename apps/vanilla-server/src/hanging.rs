//! Bounded, world-owned hanging entities for Java 1.7.10 (Protocol 5).
//! Storage is an IRW JSON sidecar and is not yet Anvil Entities NBT.
use crate::inventory::Stack;
use crate::multiplayer::Event;
use crate::world::{self, World};
use legacy_protocol::{write_string, write_varint};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

type Position = (i32, i32, i32);
const VERSION: u8 = 1;
const MAX_HANGING: usize = 256;
const MAX_BYTES: u64 = 128 * 1024;
const FIRST_ENTITY_ID: i32 = 1_600_000_000;

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, reason)
}
fn facing(face: i8) -> Option<i32> {
    match face {
        2 => Some(2), // north
        3 => Some(0), // south
        4 => Some(1), // west
        5 => Some(3), // east
        _ => None,
    }
}
fn offset(face: i8) -> (i32, i32) {
    match face {
        2 => (0, -1),
        3 => (0, 1),
        4 => (-1, 0),
        _ => (1, 0),
    }
}
fn horizontal_right(face: i8) -> (i32, i32) {
    match face {
        2 => (1, 0),
        3 => (-1, 0),
        4 => (0, -1),
        _ => (0, 1),
    }
}
/// Blocks occupied in front of the supporting wall by a frame or painting.
fn occupied_cells(anchor: Position, face: i8, width: i32, height: i32) -> Vec<Position> {
    let (rx, rz) = horizontal_right(face);
    let mut cells = Vec::with_capacity((width * height) as usize);
    for column in 0..width {
        for row in 0..height {
            cells.push((
                anchor.0 + (column - (width - 1) / 2) * rx,
                anchor.1 + row - (height - 1) / 2,
                anchor.2 + (column - (width - 1) / 2) * rz,
            ));
        }
    }
    cells
}
const PAINTINGS: &[(&str, i32, i32)] = &[
    ("Pointer", 4, 4),
    ("Pigscene", 4, 4),
    ("BurningSkull", 4, 4),
    ("Skeleton", 4, 3),
    ("DonkeyKong", 4, 3),
    ("Fighters", 4, 2),
    ("Match", 2, 2),
    ("Bust", 2, 2),
    ("Stage", 2, 2),
    ("Void", 2, 2),
    ("SkullAndRoses", 2, 2),
    ("Wither", 2, 2),
    ("Pool", 2, 1),
    ("Courbet", 2, 1),
    ("Sea", 2, 1),
    ("Sunset", 2, 1),
    ("Creebet", 2, 1),
    ("Wanderer", 1, 2),
    ("Graham", 1, 2),
    ("Kebab", 1, 1),
    ("Aztec", 1, 1),
    ("Alban", 1, 1),
    ("Aztec2", 1, 1),
    ("Bomb", 1, 1),
    ("Plant", 1, 1),
    ("Wasteland", 1, 1),
];
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Kind {
    Frame {
        item: Option<Stack>,
        rotation: u8,
    },
    Painting {
        title: String,
        width: u8,
        height: u8,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hanging {
    pub id: i32,
    at: [i32; 3],
    face: i8,
    kind: Kind,
}
impl Hanging {
    fn chunk(&self) -> (i32, i32) {
        (self.at[0].div_euclid(16), self.at[2].div_euclid(16))
    }
    fn covers(&self, pos: Position) -> bool {
        let (width, height) = match self.kind {
            Kind::Frame { .. } => (1, 1),
            Kind::Painting { width, height, .. } => (i32::from(width), i32::from(height)),
        };
        occupied_cells((self.at[0], self.at[1], self.at[2]), self.face, width, height)
            .contains(&pos)
    }
    pub fn near(&self, (x, y, z): Position) -> bool {
        // Compare as i64: untrusted player coordinates may be far outside the
        // bounded world; subtraction and abs must not overflow an i32.
        [x, y, z]
            .into_iter()
            .enumerate()
            .all(|(i, coordinate)| (i64::from(self.at[i]) - i64::from(coordinate)).abs() <= 6)
    }
    pub fn is_frame(&self) -> bool {
        matches!(self.kind, Kind::Frame { .. })
    }
    pub fn drop_position(&self) -> Position {
        (self.at[0], self.at[1], self.at[2])
    }
    pub fn drops(&self) -> Vec<Stack> {
        match &self.kind {
            Kind::Painting { .. } => vec![Stack {
                id: 321,
                count: 1,
                damage: 0,
            }],
            Kind::Frame { item, .. } => {
                let mut drops = vec![Stack {
                    id: 389,
                    count: 1,
                    damage: 0,
                }];
                if let Some(stack) = item {
                    drops.push(*stack);
                }
                drops
            }
        }
    }
    pub fn destroy(&self) -> Event {
        let mut body = vec![1]; // S13 Destroy Entities: count signed byte
        body.extend_from_slice(&self.id.to_be_bytes());
        Event { id: 0x13, body }
    }
    pub fn metadata(&self) -> Option<Event> {
        let Kind::Frame { item, rotation } = &self.kind else {
            return None;
        };
        let mut body = self.id.to_be_bytes().to_vec();
        body.push(0xa8); // type 5 (Slot), index 8 for 1.7.10 Item Frame
        match item {
            Some(stack) => {
                body.extend_from_slice(&stack.id.to_be_bytes());
                body.push(stack.count);
                body.extend_from_slice(&stack.damage.to_be_bytes());
                if let Some(nbt) = crate::books::wire_nbt(*stack) {
                    body.extend_from_slice(&(nbt.len() as i16).to_be_bytes());
                    body.extend_from_slice(&nbt);
                } else {
                    body.extend_from_slice(&(-1i16).to_be_bytes());
                }
            }
            None => body.extend_from_slice(&(-1i16).to_be_bytes()),
        }
        body.push(9); // type 0 (Byte), datawatcher index 9 rotation
        body.push(*rotation);
        body.push(127); // End of metadata
        Some(Event { id: 0x1c, body })
    }
    pub fn spawn(&self) -> io::Result<Vec<Event>> {
        let mut body = Vec::new();
        write_varint(self.id as u32, &mut body);
        let direction = facing(self.face).ok_or_else(|| invalid("invalid hanging face"))?;
        match &self.kind {
            Kind::Painting { title, .. } => {
                write_string(title, &mut body)?;
                for &coordinate in &self.at {
                    body.extend_from_slice(&coordinate.to_be_bytes());
                }
                body.extend_from_slice(&direction.to_be_bytes());
                Ok(vec![Event { id: 0x10, body }])
            }
            Kind::Frame { .. } => {
                body.push(71); // object spawn entity: item frame
                for &coordinate in &self.at {
                    body.extend_from_slice(&(coordinate * 32 + 16).to_be_bytes());
                }
                body.push(0); // pitch
                body.push(0); // yaw
                body.extend_from_slice(&direction.to_be_bytes());
                if direction != 0 {
                    body.extend_from_slice(&[0; 6]); // 3 velocity shorts
                }
                let mut packets = vec![Event { id: 0x0e, body }];
                if let Some(metadata) = self.metadata() {
                    packets.push(metadata);
                }
                Ok(packets)
            }
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hangings {
    version: u8,
    next: u16,
    entities: BTreeMap<i32, Hanging>,
}
impl Default for Hangings {
    fn default() -> Self {
        Self {
            version: VERSION,
            next: 0,
            entities: BTreeMap::new(),
        }
    }
}
impl Hangings {
    pub fn location(world: &Path) -> PathBuf {
        world.with_extension("hanging.json")
    }
    fn validate(&self) -> io::Result<()> {
        if self.version != VERSION || self.entities.len() > MAX_HANGING {
            return Err(invalid("invalid hanging version/capacity"));
        }
        for (id, entity) in &self.entities {
            if *id != entity.id
                || *id < FIRST_ENTITY_ID
                || facing(entity.face).is_none()
                || !world::in_bounds(entity.at[0], entity.at[1], entity.at[2])
            {
                return Err(invalid("invalid hanging entity"));
            }
            match &entity.kind {
                Kind::Frame { item, rotation } => {
                    if *rotation > 7
                        || item.is_some_and(|stack| !stack.is_valid() || stack.count != 1)
                    {
                        return Err(invalid("invalid frame item or rotation"));
                    }
                }
                Kind::Painting {
                    title,
                    width,
                    height,
                } => {
                    if !PAINTINGS.iter().any(|&(t, w, h)| {
                        t == title && w == i32::from(*width) && h == i32::from(*height)
                    }) {
                        return Err(invalid("invalid painting motif"));
                    }
                }
            }
        }
        Ok(())
    }
    pub fn load(path: &Path) -> io::Result<Self> {
        let meta = match fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        };
        if !meta.file_type().is_file() || meta.len() > MAX_BYTES {
            return Err(invalid("invalid hanging file"));
        }
        let data = fs::read(path)?;
        if data.len() as u64 > MAX_BYTES {
            return Err(invalid("oversized hanging file"));
        }
        let result: Self =
            serde_json::from_slice(&data).map_err(|_| invalid("invalid hanging JSON"))?;
        result.validate()?;
        Ok(result)
    }
    pub fn encoded(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        let data = serde_json::to_vec(self).map_err(|_| invalid("encode hanging JSON"))?;
        if data.len() as u64 > MAX_BYTES {
            return Err(invalid("too many hanging records"));
        }
        Ok(data)
    }
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let data = self.encoded()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension("hanging.tmp");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        let result = (|| {
            file.write_all(&data)?;
            file.sync_all()?;
            fs::rename(&temp, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }
    pub fn get(&self, id: i32) -> Option<&Hanging> {
        self.entities.get(&id)
    }
    /// Spawn only entities in newly loaded chunks. Replaying every hanging
    /// entity at login or never replaying on a chunk transition is incorrect:
    /// the client forgets entities when their chunks are unloaded.
    pub fn spawn_events_in(&self, chunks: &BTreeSet<(i32, i32)>) -> io::Result<Vec<Event>> {
        let mut out = Vec::new();
        for entity in self.entities.values() {
            if chunks.contains(&entity.chunk()) {
                out.extend(entity.spawn()?);
            }
        }
        Ok(out)
    }
    pub fn destroy_events_in(&self, chunks: &BTreeSet<(i32, i32)>) -> Vec<Event> {
        self.entities
            .values()
            .filter(|entity| chunks.contains(&entity.chunk()))
            .map(Hanging::destroy)
            .collect()
    }
    fn painting_fits(&self, world: &World, anchor: Position, face: i8, width: i32, height: i32) -> bool {
        let (dx, dz) = offset(face);
        occupied_cells(anchor, face, width, height)
            .into_iter()
            .all(|(x, y, z)| {
                world::in_bounds(x, y, z)
                    && world.get(x, y, z) == 0
                    && world.get(x - dx, y, z - dz) != 0
                    && self.entities.values().all(|entity| !entity.covers((x, y, z)))
            })
    }
    pub fn stage_place(
        &self,
        world: &World,
        clicked: Position,
        face: i8,
        id: i16,
    ) -> io::Result<Option<(Self, Hanging)>> {
        if !matches!(id, 389 | 321)
            || !world::in_bounds(clicked.0, clicked.1, clicked.2)
            || facing(face).is_none()
            || self.next == u16::MAX
            || self.entities.len() >= MAX_HANGING
        {
            return Ok(None);
        }
        let (dx, dz) = offset(face);
        let at = (clicked.0 + dx, clicked.1, clicked.2 + dz);
        if !world::in_bounds(at.0, at.1, at.2)
            || world.get(clicked.0, clicked.1, clicked.2) == 0
            || world.get(at.0, at.1, at.2) != 0
            || self.entities.values().any(|e| e.covers(at))
        {
            return Ok(None);
        }
        let kind = if id == 389 {
            Kind::Frame {
                item: None,
                rotation: 0,
            }
        } else {
            let choices: Vec<_> = PAINTINGS
                .iter()
                .copied()
                .filter(|&(title, w, h)| {
                    !title.is_empty() && self.painting_fits(world, at, face, w, h)
                })
                .collect();
            if choices.is_empty() {
                return Ok(None);
            }
            // Choose among every motif that fits, rather than always selecting
            // the first 4x4 motif on a large wall.
            let index = (usize::from(self.next)
                + at.0.unsigned_abs() as usize
                + at.2.unsigned_abs() as usize)
                % choices.len();
            let (title, width, height) = choices[index];
            Kind::Painting {
                title: title.to_owned(),
                width: width as u8,
                height: height as u8,
            }
        };
        let entity = Hanging {
            id: FIRST_ENTITY_ID + i32::from(self.next),
            at: [at.0, at.1, at.2],
            face,
            kind,
        };
        let mut next = self.clone();
        next.next += 1;
        next.entities.insert(entity.id, entity.clone());
        next.encoded()?;
        Ok(Some((next, entity)))
    }
    pub fn stage_frame_use(
        &self,
        id: i32,
        item: Option<Stack>,
    ) -> io::Result<Option<(Self, bool, Event)>> {
        let Some(entity) = self.entities.get(&id) else {
            return Ok(None);
        };
        let Kind::Frame {
            item: old_item,
            rotation,
        } = &entity.kind
        else {
            return Ok(None);
        };
        let mut next = self.clone();
        let frame = next
            .entities
            .get_mut(&id)
            .ok_or_else(|| invalid("missing frame"))?;
        let (new_item, new_rotation, debit) = if old_item.is_some() {
            (*old_item, (rotation + 1) % 8, false)
        } else if let Some(stack) = item {
            if !stack.is_valid() {
                return Ok(None);
            }
            (Some(Stack { count: 1, ..stack }), 0, true)
        } else {
            return Ok(None);
        };
        frame.kind = Kind::Frame {
            item: new_item,
            rotation: new_rotation,
        };
        let packet = frame
            .metadata()
            .ok_or_else(|| invalid("invalid frame metadata"))?;
        next.encoded()?;
        Ok(Some((next, debit, packet)))
    }
    pub fn stage_remove(&self, id: i32) -> io::Result<Option<(Self, Hanging)>> {
        let mut next = self.clone();
        let Some(entity) = next.entities.remove(&id) else {
            return Ok(None);
        };
        next.encoded()?;
        Ok(Some((next, entity)))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frame_placement_rotation_and_drops() {
        let mut world = World::new();
        world.import_block(0, 64, 0, 1 << 4).unwrap();
        let original = Hangings::default();
        let (state, frame) = original
            .stage_place(&world, (0, 64, 0), 3, 389)
            .unwrap()
            .unwrap();
        assert_eq!(frame.spawn().unwrap()[0].id, 0x0e);
        let item = Stack {
            id: 260,
            count: 7,
            damage: 0,
        };
        let (state, debit, packet) = state
            .stage_frame_use(frame.id, Some(item))
            .unwrap()
            .unwrap();
        assert!(debit);
        assert_eq!(packet.id, 0x1c);
        let (state, debit, _) = state.stage_frame_use(frame.id, None).unwrap().unwrap();
        assert!(!debit);
        let (_, removed) = state.stage_remove(frame.id).unwrap().unwrap();
        assert_eq!(removed.drops().len(), 2);
        assert_eq!(removed.drops()[1].count, 1);
    }
    #[test]
    fn crossing_chunk_boundary_replays_only_newly_visible_entities() {
        let mut world = World::new();
        world.import_block(0, 64, 0, 1 << 4).unwrap();
        world.import_block(16, 64, 0, 1 << 4).unwrap();
        let (first, first_frame) = Hangings::default()
            .stage_place(&world, (0, 64, 0), 3, 389)
            .unwrap()
            .unwrap();
        let (both, second_frame) = first
            .stage_place(&world, (16, 64, 0), 3, 389)
            .unwrap()
            .unwrap();
        let west = BTreeSet::from([(0, 0)]);
        let east = BTreeSet::from([(1, 0)]);
        let west_spawns = both.spawn_events_in(&west).unwrap();
        let east_spawns = both.spawn_events_in(&east).unwrap();
        assert_eq!(west_spawns.len(), 2); // frame object + metadata
        assert_eq!(east_spawns.len(), 2);
        assert_ne!(first_frame.id, second_frame.id);
        assert_eq!(west_spawns[0].id, 0x0e);
        assert_eq!(east_spawns[0].id, 0x0e);
        assert_eq!(both.destroy_events_in(&west).len(), 1);
        assert!(both.spawn_events_in(&BTreeSet::new()).unwrap().is_empty());
        assert!(first_frame.near((0, 64, 1)));
        assert!(!first_frame.near((i32::MIN, i32::MAX, i32::MIN)));
    }
    #[test]
    fn paintings_choose_eligible_motifs_and_reserve_their_whole_footprint() {
        let mut world = World::new();
        for x in -2..=3 {
            for y in 62..=67 {
                world.import_block(x, y, 0, 1 << 4).unwrap();
            }
        }
        let (first, painting) = Hangings::default()
            .stage_place(&world, (0, 64, 0), 3, 321)
            .unwrap()
            .unwrap();
        let Kind::Painting { width, height, .. } = painting.kind else {
            panic!("expected painting");
        };
        assert!(width > 1 && height > 1);
        // A new frame must not overlap even an off-center part of an existing painting.
        assert!(first
            .stage_place(&world, (1, 64, 0), 3, 389)
            .unwrap()
            .is_none());
        assert!(first
            .stage_place(&world, (i32::MAX, 64, 0), 3, 389)
            .unwrap()
            .is_none());
    }
    #[test]
    fn rejects_invalid_attachment_and_duplicate() {
        let world = World::new();
        assert!(Hangings::default()
            .stage_place(&world, (0, 64, 0), 3, 321)
            .unwrap()
            .is_none());
        assert!(Hangings::default()
            .stage_place(&world, (0, 63, 0), 1, 389)
            .unwrap()
            .is_none());
    }
}
