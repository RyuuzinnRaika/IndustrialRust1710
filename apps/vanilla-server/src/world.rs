//! Small, bounded, deterministic flat world. This is NOT Anvil/NBT or Forge storage.
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

pub const MIN_CHUNK: i32 = -2;
pub const MAX_CHUNK: i32 = 2;
const MAX_EDITS: usize = 65_536;
const MAGIC: &[u8; 4] = b"IRW1";
type Position = (i32, u8, i32);
#[derive(Default, Clone, Debug)]
pub struct World {
    edits: BTreeMap<Position, u16>,
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
pub fn in_bounds(x: i32, y: i32, z: i32) -> bool {
    (MIN_CHUNK * 16..(MAX_CHUNK + 1) * 16).contains(&x)
        && (0..=255).contains(&y)
        && (MIN_CHUNK * 16..(MAX_CHUNK + 1) * 16).contains(&z)
}
pub fn is_placeable(block_id: u16) -> bool {
    // Ordinary renderable 1.7.10 blocks only; no tile entities, gravity or multi-block logic.
    matches!(
        block_id,
        1 | 2
            | 3
            | 4
            | 5
            | 7
            | 12
            | 13
            | 17
            | 20
            | 24
            | 35
            | 41
            | 42
            | 45
            | 49
            | 57
            | 80
            | 82
            | 87
            | 89
            | 98
            | 155
            | 159
            | 168
            | 169
            | 170
            | 172
            | 173
    )
}
impl World {
    pub fn new() -> Self {
        Self::default()
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
            .unwrap_or_else(|| Self::default_block(y as u8))
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
        if block == Self::default_block(y as u8) {
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
        Ok(true)
    }
    pub fn load(path: &Path) -> io::Result<Self> {
        let bytes = match fs::read(path) {
            Ok(v) => v,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::new()),
            Err(e) => return Err(e),
        };
        if bytes.len() < 8 || &bytes[..4] != MAGIC {
            return Err(invalid("bad world header"));
        }
        let count = u32::from_be_bytes(bytes[4..8].try_into().unwrap()) as usize;
        if count > MAX_EDITS || bytes.len() != 8 + count * 11 {
            return Err(invalid("world file length/count mismatch"));
        }
        let mut world = Self::new();
        for entry in bytes[8..].chunks_exact(11) {
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
        }
        Ok(world)
    }
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension("tmp");
        let mut f = File::create(&temp)?;
        f.write_all(MAGIC)?;
        f.write_all(&(self.edits.len() as u32).to_be_bytes())?;
        for (&(x, y, z), &block) in &self.edits {
            f.write_all(&x.to_be_bytes())?;
            f.write_all(&[y])?;
            f.write_all(&z.to_be_bytes())?;
            f.write_all(&block.to_be_bytes())?;
        }
        f.sync_all()?;
        drop(f);
        // On OSes that cannot atomically replace a destination file, fail safely,
        // keeping the original untouched rather than deleting it.
        fs::rename(temp, path)
    }
    pub fn chunk_data(&self, chunk_x: i32, chunk_z: i32) -> Vec<u8> {
        // 1.7.10: two 16x16x16 sections (y=48..79), ordered block IDs, metadata,
        // block light, sky light, then 256 biome bytes. No "add" arrays.
        let mut bytes = Vec::with_capacity(2 * 10240 + 256);
        for section in [3i32, 4] {
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
        for section in [3i32, 4] {
            for y in 0..16 {
                for z in 0..16 {
                    for x in (0..16).step_by(2) {
                        let a = self.get(chunk_x * 16 + x, section * 16 + y, chunk_z * 16 + z)
                            as u8
                            & 15;
                        let b = self.get(chunk_x * 16 + x + 1, section * 16 + y, chunk_z * 16 + z)
                            as u8
                            & 15;
                        bytes.push(a | b << 4);
                    }
                }
            }
        }
        bytes.extend(std::iter::repeat_n(0u8, 2 * 2048)); // block light
        bytes.extend(std::iter::repeat_n(0xffu8, 2 * 2048)); // sky light
        bytes.extend(std::iter::repeat_n(1u8, 256)); // plains biome
        bytes
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn flat_and_bounds() {
        let w = World::new();
        assert_eq!(w.get(0, 63, 0), 2 << 4);
        assert_eq!(w.get(-32, 60, -32), 7 << 4);
        assert_eq!(w.get(-33, 63, 0), 0);
        assert_eq!(w.chunk_data(-1, 0).len(), 20736);
        assert_eq!(w.chunk_data(0, 0)[15 * 256], 2);
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
        assert!(!w.update_and_save(&path, 1000, 65, 2, 4 << 4).unwrap());
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
}
