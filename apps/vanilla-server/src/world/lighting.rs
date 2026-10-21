//! Chunk-local light arrays, with a 15-block halo for block-light emitters.
//! Calculated from durable block state: reload never needs a separate light save.
use super::World;
use std::collections::{BTreeMap, VecDeque};

fn transparent(id: u16) -> bool {
    matches!(id, 0 | 6 | 8..=11 | 18 | 20 | 30 | 31 | 37..=40 | 50 | 55
        | 59 | 60 | 63 | 65 | 66 | 69 | 70 | 72 | 75..=78 | 83 | 90
        | 93 | 94 | 96 | 106 | 111 | 141 | 142 | 143 | 157)
}
fn emission(id: u16) -> u8 {
    match id {
        10 | 11 | 89 => 15,
        50 | 75 | 76 => 14,
        62 => 13,
        _ => 0,
    }
}
fn index(x: i32, y: i32, z: i32, min_x: i32, min_z: i32) -> usize {
    (y as usize * 256) + ((z - min_z) as usize * 16) + (x - min_x) as usize
}

impl World {
    /// Returns separate 16x256x16 block-light and sky-light nibble values.
    /// Direct sky columns are computed top-down. Artificial block light
    /// travels through air/transparent blocks for at most 15 blocks and may
    /// cross chunk boundaries (sources are searched in a 15-block halo).
    pub(super) fn light_arrays(&self, chunk_x: i32, chunk_z: i32) -> (Vec<u8>, Vec<u8>) {
        let min_x = chunk_x * 16;
        let min_z = chunk_z * 16;
        let mut sky = vec![0u8; 16 * 256 * 16];
        let mut block = vec![0u8; 16 * 256 * 16];
        for z in min_z..min_z + 16 {
            for x in min_x..min_x + 16 {
                let mut sun = 15u8;
                for y in (0..256).rev() {
                    if !transparent(self.get(x, y, z) >> 4) {
                        sun = 0;
                    }
                    sky[index(x, y, z, min_x, min_z)] = sun;
                }
            }
        }
        // Only edited blocks may emit light in the present generator. For
        // source positions outside the chunk, BFS traverses their halo.
        let mut queue = VecDeque::<(i32, i32, i32, u8)>::new();
        let mut levels = BTreeMap::<(i32, i32, i32), u8>::new();
        for (&(x, y, z), &value) in self
            .edits
            .range((min_x - 15, 0, i32::MIN)..(min_x + 31, 0, i32::MIN))
        {
            if z < min_z - 15 || z >= min_z + 31 {
                continue;
            }
            let strength = emission(value >> 4);
            if strength > 0 {
                queue.push_back((x, i32::from(y), z, strength));
            }
        }
        while let Some((x, y, z, level)) = queue.pop_front() {
            if level == 0
                || x < min_x - 15
                || x >= min_x + 31
                || z < min_z - 15
                || z >= min_z + 31
                || !(0..256).contains(&y)
                || levels.get(&(x, y, z)).is_some_and(|&old| old >= level)
            {
                continue;
            }
            levels.insert((x, y, z), level);
            if (min_x..min_x + 16).contains(&x) && (min_z..min_z + 16).contains(&z) {
                let slot = index(x, y, z, min_x, min_z);
                block[slot] = block[slot].max(level);
            }
            if level <= 1 {
                continue;
            }
            for (nx, ny, nz) in [
                (x + 1, y, z),
                (x - 1, y, z),
                (x, y + 1, z),
                (x, y - 1, z),
                (x, y, z + 1),
                (x, y, z - 1),
            ] {
                if (0..256).contains(&ny) && transparent(self.get(nx, ny, nz) >> 4) {
                    queue.push_back((nx, ny, nz, level - 1));
                }
            }
        }
        (block, sky)
    }
}
