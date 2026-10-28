//! Protocol 5 (Minecraft Java 1.7.10) decorative block state.
//! These sidecar records are world-owned, bounded and atomically replaced.
//! They are not yet exported to Anvil TileEntities.
use crate::inventory::Stack;
use crate::world;
use legacy_protocol::{write_string, write_varint};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

pub type Position = (i32, i32, i32);
const VERSION: u8 = 1;
const MAX_RECORDS: usize = 2048;
const MAX_BYTES: u64 = 1024 * 1024;

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, reason)
}
fn key((x, y, z): Position) -> String {
    format!("{x},{y},{z}")
}
fn parse_key(text: &str) -> io::Result<Position> {
    let parts = text.split(',').map(str::parse::<i32>).collect::<Vec<_>>();
    if parts.len() != 3 || parts.iter().any(Result::is_err) {
        return Err(invalid("invalid decoration position"));
    }
    let position = (
        parts[0].as_ref().copied().unwrap(),
        parts[1].as_ref().copied().unwrap(),
        parts[2].as_ref().copied().unwrap(),
    );
    if !world::in_bounds(position.0, position.1, position.2) || key(position) != text {
        return Err(invalid("invalid decoration coordinates"));
    }
    Ok(position)
}
fn valid_line(line: &str) -> bool {
    line.encode_utf16().count() <= 15 && !line.chars().any(char::is_control)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decorations {
    version: u8,
    #[serde(default)]
    signs: BTreeMap<String, [String; 4]>,
    #[serde(default)]
    notes: BTreeMap<String, u8>,
    #[serde(default)]
    records: BTreeMap<String, i16>,
    #[serde(default)]
    pots: BTreeMap<String, Stack>,
}
impl Default for Decorations {
    fn default() -> Self {
        Self {
            version: VERSION,
            signs: BTreeMap::new(),
            notes: BTreeMap::new(),
            records: BTreeMap::new(),
            pots: BTreeMap::new(),
        }
    }
}
impl Decorations {
    pub fn location(world_path: &Path) -> PathBuf {
        world_path.with_extension("decorations.json")
    }
    fn validate(&self) -> io::Result<()> {
        if self.version != VERSION
            || self.signs.len() > MAX_RECORDS
            || self.notes.len() > MAX_RECORDS
            || self.records.len() > MAX_RECORDS
            || self.pots.len() > MAX_RECORDS
        {
            return Err(invalid("decoration version or capacity exceeded"));
        }
        for (pos, lines) in &self.signs {
            parse_key(pos)?;
            if !lines.iter().all(|line| valid_line(line)) {
                return Err(invalid("invalid sign text"));
            }
        }
        for (pos, pitch) in &self.notes {
            parse_key(pos)?;
            if *pitch > 24 {
                return Err(invalid("invalid note pitch"));
            }
        }
        for (pos, record) in &self.records {
            parse_key(pos)?;
            if !(2256..=2267).contains(record) {
                return Err(invalid("invalid jukebox disc"));
            }
        }
        for (pos, stack) in &self.pots {
            parse_key(pos)?;
            if !pot_compatible(*stack) {
                return Err(invalid("invalid potted item"));
            }
        }
        Ok(())
    }
    pub fn load(path: &Path) -> io::Result<Self> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(err),
        };
        if !metadata.file_type().is_file() || metadata.len() > MAX_BYTES {
            return Err(invalid("invalid decorations file"));
        }
        let data = fs::read(path)?;
        if data.len() as u64 > MAX_BYTES {
            return Err(invalid("decoration file too large"));
        }
        let store: Self =
            serde_json::from_slice(&data).map_err(|_| invalid("invalid decoration JSON"))?;
        store.validate()?;
        Ok(store)
    }
    /// Read-only referential integrity check for IRW decoration sidecars.
    ///
    /// A JSON record can be structurally valid while referencing a removed
    /// sign, note block, jukebox or flower pot. Reject that orphaned state
    /// rather than silently declaring a stopped world recoverable.
    pub fn verify_world_references(&self, world: &world::World) -> io::Result<()> {
        for position in self.signs.keys() {
            let (x, y, z) = parse_key(position)?;
            if !matches!(world.get(x, y, z) >> 4, 63 | 68) {
                return Err(invalid("orphaned sign: no matching world block"));
            }
        }
        for position in self.notes.keys() {
            let (x, y, z) = parse_key(position)?;
            if world.get(x, y, z) >> 4 != 25 {
                return Err(invalid("orphaned note block: no matching world block"));
            }
        }
        for position in self.records.keys() {
            let (x, y, z) = parse_key(position)?;
            if world.get(x, y, z) >> 4 != 84 {
                return Err(invalid("orphaned jukebox record: no matching world block"));
            }
        }
        for position in self.pots.keys() {
            let (x, y, z) = parse_key(position)?;
            if world.get(x, y, z) >> 4 != 140 {
                return Err(invalid("orphaned flower pot: no matching world block"));
            }
        }
        Ok(())
    }

    pub fn encoded(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        let data =
            serde_json::to_vec(self).map_err(|_| invalid("decoration JSON encoding failed"))?;
        if data.len() as u64 > MAX_BYTES {
            return Err(invalid("decoration file too large"));
        }
        Ok(data)
    }
    fn save(&self, path: &Path) -> io::Result<()> {
        let data = self.encoded()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("decorations.tmp");
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        let result = (|| {
            file.write_all(&data)?;
            file.sync_all()?;
            fs::rename(&tmp, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }
    pub fn set_sign(
        &mut self,
        path: &Path,
        position: Position,
        lines: [String; 4],
    ) -> io::Result<()> {
        if !world::in_bounds(position.0, position.1, position.2)
            || !lines.iter().all(|line| valid_line(line))
        {
            return Err(invalid("invalid sign update"));
        }
        let mut next = self.clone();
        next.signs.insert(key(position), lines);
        next.save(path)?;
        *self = next;
        Ok(())
    }
    pub fn note(&self, position: Position) -> u8 {
        *self.notes.get(&key(position)).unwrap_or(&0)
    }
    pub fn next_note(&mut self, path: &Path, position: Position) -> io::Result<u8> {
        let mut next = self.clone();
        let pitch = (self.note(position) + 1) % 25;
        next.notes.insert(key(position), pitch);
        next.save(path)?;
        *self = next;
        Ok(pitch)
    }
    pub fn record(&self, position: Position) -> Option<i16> {
        self.records.get(&key(position)).copied()
    }
    pub fn occupied(&self, position: Position) -> bool {
        self.records.contains_key(&key(position)) || self.pots.contains_key(&key(position))
    }
    /// Stage a jukebox disc change without making it visible. The caller
    /// journals this snapshot with the inventory debit or item return to
    /// prevent disc loss across a crash between otherwise separate saves.
    pub fn stage_record(&self, position: Position, item: Option<i16>) -> io::Result<Self> {
        if !world::in_bounds(position.0, position.1, position.2)
            || item.is_some_and(|id| !(2256..=2267).contains(&id))
        {
            return Err(invalid("invalid record or jukebox position"));
        }
        let mut next = self.clone();
        match item {
            Some(id) => {
                next.records.insert(key(position), id);
            }
            None => {
                next.records.remove(&key(position));
            }
        }
        next.encoded()?;
        Ok(next)
    }
    pub fn pot(&self, position: Position) -> Option<Stack> {
        self.pots.get(&key(position)).copied()
    }
    /// Stage a flower-pot update without publishing it. The caller journals
    /// the resulting decoration snapshot together with the inventory debit or
    /// item return, so a crash cannot duplicate or lose the plant.
    pub fn stage_pot(&self, position: Position, item: Option<Stack>) -> io::Result<Self> {
        if !world::in_bounds(position.0, position.1, position.2)
            || item.is_some_and(|stack| !pot_compatible(stack))
        {
            return Err(invalid("invalid flower pot item or position"));
        }
        let mut next = self.clone();
        match item {
            Some(stack) => {
                next.pots.insert(key(position), stack);
            }
            None => {
                next.pots.remove(&key(position));
            }
        }
        next.encoded()?;
        Ok(next)
    }
    /// Prepare removal of all tile data owned by one mined decorative block.
    /// The caller must commit this snapshot together with the world edit and
    /// any dropped disc/plant before publishing either live state.
    pub fn staged_remove(&self, position: Position) -> io::Result<Self> {
        if !world::in_bounds(position.0, position.1, position.2) {
            return Err(invalid("invalid decoration removal position"));
        }
        let mut next = self.clone();
        let position = key(position);
        next.signs.remove(&position);
        next.notes.remove(&position);
        next.records.remove(&position);
        next.pots.remove(&position);
        next.encoded()?;
        Ok(next)
    }

    pub fn remove(&mut self, path: &Path, position: Position) -> io::Result<()> {
        let mut next = self.clone();
        let key = key(position);
        let removed = next.signs.remove(&key).is_some()
            | next.notes.remove(&key).is_some()
            | next.records.remove(&key).is_some()
            | next.pots.remove(&key).is_some();
        if removed {
            next.save(path)?;
            *self = next;
        }
        Ok(())
    }
    pub fn pot_packets_in(
        &self,
        chunks: &std::collections::BTreeSet<(i32, i32)>,
    ) -> io::Result<Vec<Vec<u8>>> {
        let mut packets = Vec::new();
        for (key, stack) in &self.pots {
            let pos = parse_key(key)?;
            if chunks.contains(&(pos.0.div_euclid(16), pos.2.div_euclid(16))) {
                packets.push(pot_packet(pos, Some(*stack))?);
            }
        }
        Ok(packets)
    }
    pub fn sign_packets_in(
        &self,
        chunks: &std::collections::BTreeSet<(i32, i32)>,
    ) -> io::Result<Vec<Vec<u8>>> {
        let mut packets = Vec::new();
        for (key, lines) in &self.signs {
            let position = parse_key(key)?;
            if chunks.contains(&(position.0.div_euclid(16), position.2.div_euclid(16))) {
                packets.push(sign_packet(position, lines)?);
            }
        }
        Ok(packets)
    }
}

pub fn pot_item_supported(stack: Stack) -> bool {
    stack.is_valid()
        && matches!(stack.id, 6 | 31 | 32 | 37 | 38 | 39 | 40 | 81 | 175 | 351)
        && (stack.id != 351 || stack.damage == 3)
}
pub fn pot_compatible(stack: Stack) -> bool {
    stack.count == 1 && pot_item_supported(stack)
}
pub fn sign_packet((x, y, z): Position, lines: &[String; 4]) -> io::Result<Vec<u8>> {
    if !world::in_bounds(x, y, z) || !lines.iter().all(|line| valid_line(line)) {
        return Err(invalid("invalid sign packet"));
    }
    let mut body = Vec::new();
    body.extend_from_slice(&x.to_be_bytes());
    body.extend_from_slice(&(y as i16).to_be_bytes());
    body.extend_from_slice(&z.to_be_bytes());
    for line in lines {
        write_string(line, &mut body)?;
    }
    Ok(body)
}
pub fn sign_editor_packet((x, y, z): Position) -> Vec<u8> {
    let mut body = Vec::with_capacity(12);
    body.extend_from_slice(&x.to_be_bytes());
    body.extend_from_slice(&y.to_be_bytes());
    body.extend_from_slice(&z.to_be_bytes());
    body
}
pub fn note_action((x, y, z): Position, instrument: u8, pitch: u8) -> Vec<u8> {
    let mut body = Vec::with_capacity(13);
    body.extend_from_slice(&x.to_be_bytes());
    body.extend_from_slice(&(y as i16).to_be_bytes());
    body.extend_from_slice(&z.to_be_bytes());
    body.push(instrument);
    body.push(pitch);
    write_varint(25, &mut body);
    body
}
pub fn instrument(below: u16) -> u8 {
    match below {
        5 | 17 | 162 | 47 | 53 | 85 | 126 => 1, // wood/bass
        12 | 13 | 88 => 2,                      // sand/snare
        20 | 95 | 102 => 3,                     // glass/hat
        1 | 4 | 14 | 15 | 16 | 48 | 49 => 4,    // stone/bass drum
        _ => 0,                                 // harp/piano
    }
}
/// S35 Update Tile Entity: action 5 = FlowerPot; compressed NBT Short+GZip.
pub fn pot_packet((x, y, z): Position, stack: Option<Stack>) -> io::Result<Vec<u8>> {
    use flate2::{write::GzEncoder, Compression};
    #[derive(Serialize)]
    struct PotNbt<'a> {
        id: &'a str,
        x: i32,
        y: i32,
        z: i32,
        #[serde(rename = "Item")]
        item: i32,
        #[serde(rename = "Data")]
        data: i32,
    }
    if !world::in_bounds(x, y, z) || stack.is_some_and(|item| !pot_compatible(item)) {
        return Err(invalid("invalid pot tile entity"));
    }
    let (item, data) = stack.map_or((0, 0), |s| (i32::from(s.id), i32::from(s.damage)));
    let nbt = fastnbt::to_bytes(&PotNbt {
        id: "FlowerPot",
        x,
        y,
        z,
        item,
        data,
    })
    .map_err(|_| invalid("invalid pot NBT"))?;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&nbt)?;
    let gzip = encoder.finish()?;
    if gzip.len() > i16::MAX as usize {
        return Err(invalid("oversized pot NBT"));
    }
    let mut body = Vec::new();
    body.extend_from_slice(&x.to_be_bytes());
    body.extend_from_slice(&(y as i16).to_be_bytes());
    body.extend_from_slice(&z.to_be_bytes());
    body.push(5);
    body.extend_from_slice(&(gzip.len() as i16).to_be_bytes());
    body.extend_from_slice(&gzip);
    Ok(body)
}
pub fn disc_event((x, y, z): Position, disc: Option<i16>) -> Vec<u8> {
    let mut body = Vec::with_capacity(18);
    body.extend_from_slice(&1005i32.to_be_bytes());
    body.extend_from_slice(&x.to_be_bytes());
    body.push(y as u8);
    body.extend_from_slice(&z.to_be_bytes());
    body.extend_from_slice(&i32::from(disc.unwrap_or(0)).to_be_bytes());
    body.push(0); // not global
    body
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_wire_fields_and_limit() {
        let text = [
            "Hello".to_owned(),
            "世界".to_owned(),
            "".to_owned(),
            "!".to_owned(),
        ];
        let bytes = sign_packet((-1, 64, 2), &text).unwrap();
        assert_eq!(&bytes[..4], &(-1i32).to_be_bytes());
        assert_eq!(&bytes[4..6], &(64i16).to_be_bytes());
        assert_eq!(&bytes[6..10], &(2i32).to_be_bytes());
        assert_eq!(bytes[10], 5);
        assert!(sign_packet(
            (0, 64, 0),
            &["1234567890123456".into(), "".into(), "".into(), "".into()]
        )
        .is_err());
        assert!(sign_packet(
            (0, 64, 0),
            &["😀".repeat(8), "".into(), "".into(), "".into()]
        )
        .is_err());
    }
    #[test]
    fn persistence_round_trip_and_bad_data() {
        let path =
            std::env::temp_dir().join(format!("irw-g-decoration-{}.json", std::process::id()));
        let _ = fs::remove_file(&path);
        let mut store = Decorations::default();
        store
            .set_sign(
                &path,
                (3, 64, -2),
                ["A".into(), "".into(), "".into(), "".into()],
            )
            .unwrap();
        store.next_note(&path, (4, 64, -2)).unwrap();
        store = store.stage_record((5, 64, -2), Some(2256)).unwrap();
        store.save(&path).unwrap();
        let restored = Decorations::load(&path).unwrap();
        assert_eq!(restored.signs.get(&key((3, 64, -2))).unwrap()[0], "A");
        assert_eq!(restored.note((4, 64, -2)), 1);
        assert_eq!(restored.record((5, 64, -2)), Some(2256));
        store.remove(&path, (3, 64, -2)).unwrap();
        assert!(!Decorations::load(&path)
            .unwrap()
            .signs
            .contains_key(&key((3, 64, -2))));
        let _ = fs::remove_file(&path);
    }
    #[test]
    fn jukebox_disc_and_inventory_survive_an_atomic_round_trip() {
        let root = std::env::temp_dir().join(format!("irw-record-journal-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let decoration_path = root.join("world.decorations.json");
        let inventory_path = root.join("reader.inventory.json");
        let log_path = root.join("world.pending-txn.json");
        let disc = Stack {
            id: 2256,
            count: 1,
            damage: 0,
            nbt: None,
        };
        let mut inventory = crate::inventory::Inventory::default();
        inventory.creative_set(36, Some(disc)).unwrap();
        let mut debited = inventory.clone();
        assert!(debited.consume_held());
        let inserted = Decorations::default()
            .stage_record((1, 64, 1), Some(disc.id))
            .unwrap();
        crate::journal::commit(
            &log_path,
            &[
                (&decoration_path, inserted.encoded().unwrap()),
                (&inventory_path, debited.encoded().unwrap()),
            ],
        )
        .unwrap();
        assert!(crate::inventory::Inventory::load(&inventory_path)
            .unwrap()
            .held()
            .is_none());
        assert_eq!(
            Decorations::load(&decoration_path)
                .unwrap()
                .record((1, 64, 1)),
            Some(disc.id)
        );

        let mut returned = crate::inventory::Inventory::load(&inventory_path).unwrap();
        assert!(returned.receive_stack(disc).is_some());
        let ejected = Decorations::load(&decoration_path)
            .unwrap()
            .stage_record((1, 64, 1), None)
            .unwrap();
        crate::journal::commit(
            &log_path,
            &[
                (&decoration_path, ejected.encoded().unwrap()),
                (&inventory_path, returned.encoded().unwrap()),
            ],
        )
        .unwrap();
        assert_eq!(
            Decorations::load(&decoration_path)
                .unwrap()
                .record((1, 64, 1)),
            None
        );
        assert_eq!(
            crate::inventory::Inventory::load(&inventory_path)
                .unwrap()
                .held(),
            Some(disc)
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn pot_item_and_inventory_are_saved_as_one_recoverable_transaction() {
        let root = std::env::temp_dir().join(format!("irw-pot-journal-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let decoration_path = root.join("world.decorations.json");
        let inventory_path = root.join("reader.inventory.json");
        let log_path = root.join("world.pending-txn.json");
        let plant = Stack {
            id: 37,
            count: 1,
            damage: 0,
            nbt: None,
        };
        let staged = Decorations::default()
            .stage_pot((0, 64, 0), Some(plant))
            .unwrap();
        let mut inventory = crate::inventory::Inventory::default();
        inventory.creative_set(36, Some(plant)).unwrap();
        assert!(inventory.consume_held());
        crate::journal::commit(
            &log_path,
            &[
                (&inventory_path, inventory.encoded().unwrap()),
                (&decoration_path, staged.encoded().unwrap()),
            ],
        )
        .unwrap();
        assert!(crate::inventory::Inventory::load(&inventory_path)
            .unwrap()
            .held()
            .is_none());
        assert_eq!(
            Decorations::load(&decoration_path).unwrap().pot((0, 64, 0)),
            Some(plant)
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn pot_rejects_non_decorative_items() {
        assert!(!pot_compatible(Stack {
            id: 276,
            count: 1,
            damage: 0,
            nbt: None,
        }));
        assert!(pot_compatible(Stack {
            id: 37,
            count: 1,
            damage: 0,
            nbt: None,
        }));
    }
}
