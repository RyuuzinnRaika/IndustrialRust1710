//! A bounded 1.7.10 filled-map index. Map IDs live in Stack.damage.
//! Persistent 128x128 color buffers are a transitional IRW sidecar, not Anvil map_*.dat.
use crate::world::{self, World};
use legacy_protocol::write_varint;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

const VERSION: u8 = 1;
const MAX_MAPS: usize = 32;
const MAX_BYTES: u64 = 2 * 1024 * 1024;
const SIDE: usize = 128;
const PIXELS: usize = SIDE * SIDE;
const SCALE: i32 = 8; // vanilla zoom level 3, 8 blocks per map pixel.

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, reason)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MapData {
    center_x: i32,
    center_z: i32,
    colors: Vec<u8>,
}
impl MapData {
    fn centered(x: i32, z: i32) -> Self {
        const WIDTH: i32 = 128 * SCALE;
        Self {
            center_x: x.div_euclid(WIDTH) * WIDTH + WIDTH / 2,
            center_z: z.div_euclid(WIDTH) * WIDTH + WIDTH / 2,
            colors: vec![0; PIXELS],
        }
    }
    fn coordinate(&self, world_x: i32, world_z: i32) -> (i32, i32) {
        (
            (world_x - self.center_x).div_euclid(SCALE) + 64,
            (world_z - self.center_z).div_euclid(SCALE) + 64,
        )
    }
    fn packet(&self, id: i16, x: i32, z: i32, yaw: f32, col: u8, row: u8) -> Vec<u8> {
        let mut out = Vec::with_capacity(32);
        write_varint(id as u32, &mut out);
        out.push(3); // zoom=3
        write_varint(1, &mut out); // one player marker
        let (px, pz) = self.coordinate(x, z);
        let direction = ((yaw.rem_euclid(360.0) / 22.5).round() as u8) & 15;
        out.push(direction << 4); // type 0: player pointer
        out.push((px.saturating_mul(2) - 128).clamp(-128, 127) as i8 as u8);
        out.push((pz.saturating_mul(2) - 128).clamp(-128, 127) as i8 as u8);
        out.push(1); // one color column
        out.push(16); // sixteen rows
        out.push(col);
        out.push(row);
        write_varint(16, &mut out);
        for dy in 0..16 {
            out.push(self.colors[(row as usize + dy) * SIDE + col as usize]);
        }
        out
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Maps {
    version: u8,
    next_id: i16,
    entries: BTreeMap<i16, MapData>,
}
impl Default for Maps {
    fn default() -> Self {
        Self {
            version: VERSION,
            next_id: 0,
            entries: BTreeMap::new(),
        }
    }
}
impl Maps {
    pub fn location(world_file: &Path) -> PathBuf {
        world_file.with_extension("maps.json")
    }
    fn validate(&self) -> io::Result<()> {
        if self.version != VERSION || self.next_id < 0 || self.entries.len() > MAX_MAPS {
            return Err(invalid("map capacity or version invalid"));
        }
        for (id, map) in &self.entries {
            if *id < 0
                || map.colors.len() != PIXELS
                || !world::in_bounds(map.center_x, 64, map.center_z)
            {
                return Err(invalid("invalid saved map"));
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
            return Err(invalid("invalid map save"));
        }
        let data = fs::read(path)?;
        if data.len() as u64 > MAX_BYTES {
            return Err(invalid("map save too large"));
        }
        let result: Self = serde_json::from_slice(&data).map_err(|_| invalid("bad map JSON"))?;
        result.validate()?;
        Ok(result)
    }
    pub fn encoded(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        let json = serde_json::to_vec(self).map_err(|_| invalid("encode map JSON"))?;
        if json.len() as u64 > MAX_BYTES {
            return Err(invalid("too many map pixels"));
        }
        Ok(json)
    }
    fn save(&self, path: &Path) -> io::Result<()> {
        let bytes = self.encoded()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("maps.tmp");
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        let result = (|| {
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&tmp, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }
    /// Allocate a fresh ID in a detached snapshot; caller must journal it with
    /// the inventory debit before making the item visible.
    pub fn stage_new(&self, x: i32, z: i32) -> io::Result<Option<(Self, i16)>> {
        if self.entries.len() >= MAX_MAPS || self.next_id == i16::MAX {
            return Ok(None);
        }
        let id = self.next_id;
        let mut next = self.clone();
        next.next_id += 1;
        next.entries.insert(id, MapData::centered(x, z));
        next.encoded()?;
        Ok(Some((next, id)))
    }
    /// Persist before emitting an authoritative client map update.
    /// One 16-pixel stripe every four ticks bounds CPU and I/O load.
    pub fn refresh(
        &mut self,
        path: &Path,
        world: &World,
        id: i16,
        (x, z): (i32, i32),
        yaw: f32,
        sequence: u64,
    ) -> io::Result<Option<Vec<u8>>> {
        if id < 0 {
            return Ok(None);
        }
        let mut next = self.clone();
        if !next.entries.contains_key(&id) {
            if next.entries.len() >= MAX_MAPS || id == i16::MAX {
                return Ok(None);
            }
            next.entries.insert(id, MapData::centered(x, z));
            next.next_id = next.next_id.max(id + 1);
        }
        let map = next
            .entries
            .get_mut(&id)
            .ok_or_else(|| invalid("missing allocated map"))?;
        let (px, pz) = map.coordinate(x, z);
        let start_col = (px - 8).clamp(0, 112);
        let start_row = (pz - 8).clamp(0, 112);
        let col = (start_col + (sequence % 16) as i32) as u8;
        let row = start_row as u8;
        for dy in 0..16usize {
            let wx = map.center_x + (col as i32 - 64) * SCALE;
            let wz = map.center_z + (row as i32 + dy as i32 - 64) * SCALE;
            let color = terrain_color(world, wx, wz);
            map.colors[(row as usize + dy) * SIDE + col as usize] = color;
        }
        let packet = map.packet(id, x, z, yaw, col, row);
        if next.entries != self.entries {
            next.save(path)?;
            *self = next;
        }
        Ok(Some(packet))
    }
}
fn terrain_color(world: &World, x: i32, z: i32) -> u8 {
    if !world::in_bounds(x, 64, z) {
        return 0;
    }
    let base = world.surface_height_at(x, z).unwrap_or(65).clamp(0, 255);
    // Sample up to 16 blocks above the generated surface for user-placed
    // constructions, then search down to find water, grass and natural terrain.
    let mut id = 0;
    for y in (0..=(base + 16).min(255)).rev() {
        id = world.get(x, y, z) >> 4;
        if !matches!(id, 0 | 31 | 32 | 37 | 38 | 78) {
            break;
        }
    }
    let palette = match id {
        2 | 18 | 31 | 175 => 5, // grass/foliage
        3 | 60 | 110 => 13,     // dirt
        8 | 9 => 12,            // water
        12 | 24 => 2,           // sand
        17 | 5 | 162 => 13,     // wood
        79 | 174 => 9,          // ice
        80 => 8,                // snow
        35 => 3,                // wool
        49 | 87 => 11,          // dark stone
        0 => 0,
        _ => 11, // stone/manmade
    };
    if palette == 0 {
        0
    } else {
        palette * 4 + 2
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn map_ids_are_unique_and_colors_survive_restart() {
        let path = std::env::temp_dir().join(format!("irw-map-test-{}.json", std::process::id()));
        let _ = fs::remove_file(&path);
        let (mut maps, id) = Maps::default().stage_new(0, 0).unwrap().unwrap();
        assert_eq!(id, 0);
        let (_, second) = maps.stage_new(0, 0).unwrap().unwrap();
        assert_eq!(second, 1);
        let packet = maps
            .refresh(&path, &World::new(), id, (0, 0), 0.0, 0)
            .unwrap()
            .unwrap();
        assert_eq!(packet[0..3], [0, 3, 1]); // MapID, scale, marker count
        assert_eq!(
            Maps::load(&path)
                .unwrap()
                .entries
                .get(&id)
                .unwrap()
                .colors
                .len(),
            PIXELS
        );
        let _ = fs::remove_file(path);
    }
    #[test]
    fn map_data_packet_has_one_column_of_sixteen_rows() {
        let data = MapData::centered(-99, 101);
        let packet = data.packet(7, 0, 0, 90.0, 20, 10);
        assert_eq!(packet[0], 7);
        assert_eq!(packet[6], 1);
        assert_eq!(packet[7], 16);
        assert_eq!(&packet[8..11], &[20, 10, 16]);
        assert_eq!(packet.len(), 27);
    }
}
