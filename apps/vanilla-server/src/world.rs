//! Bounded, sparse, deterministic flat world streamed by chunk.
//! IRW1 is temporary storage; this is NOT Anvil/NBT/Forge world storage.
use std::collections::{BTreeMap, BTreeSet};
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
type Position = (i32, u8, i32);
#[derive(Default, Clone, Debug)]
pub struct World {
    edits: BTreeMap<Position, u16>,
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
    pub fn edited_chunks(&self) -> BTreeSet<(i32, i32)> {
        self.edits
            .keys()
            .map(|&(x, _, z)| (x.div_euclid(16), z.div_euclid(16)))
            .collect()
    }
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
    /// Apply one parsed block to a *detached* import world. Call save_new only
    /// after every Anvil chunk has been validated; never persist partial imports.
    pub fn import_block(&mut self, x: i32, y: i32, z: i32, block: u16) -> io::Result<()> {
        if !in_bounds(x, y, z) || block >> 4 > 255 {
            return Err(invalid("imported block outside 1.7.10 world limits"));
        }
        let key = (x, y as u8, z);
        if block == Self::default_block(y as u8) {
            return Ok(());
        }
        if !self.edits.contains_key(&key) && self.edits.len() >= MAX_EDITS {
            return Err(invalid("Anvil import exceeds IRW1 edit limit"));
        }
        self.edits.insert(key, block);
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
        if bytes.len() < 8 || &bytes[..4] != MAGIC {
            return Err(invalid("bad world header"));
        }
        let count = u32::from_be_bytes(bytes[4..8].try_into().unwrap()) as usize;
        if count > MAX_EDITS || bytes.len() != 8 + count * 11 {
            return Err(invalid("world file length/count mismatch"));
        }
        let mut world = Self::new();
        for entry in bytes[8..].as_chunks::<11>().0 {
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
    fn write_to(&self, f: &mut File) -> io::Result<()> {
        f.write_all(MAGIC)?;
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
        let mut mask: u16 = (1 << 3) | (1 << 4);
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
        bytes.extend(std::iter::repeat_n(0u8, sections.len() * 2048)); // block light
        bytes.extend(std::iter::repeat_n(0xffu8, sections.len() * 2048)); // sky light
        bytes.extend(std::iter::repeat_n(1u8, 256)); // plains biome
        (mask, bytes)
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
}
