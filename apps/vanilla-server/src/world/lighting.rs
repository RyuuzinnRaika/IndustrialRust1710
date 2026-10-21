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
/// Observable transparency and emitted light independent of block metadata.
pub(super) fn lighting_signature(block: u16) -> (bool, u8) {
    let id = block >> 4;
    (transparent(id), emission(id))
}

fn index(x: i32, y: i32, z: i32, min_x: i32, min_z: i32) -> usize {
    (y as usize * 256) + ((z - min_z) as usize * 16) + (x - min_x) as usize
}

impl World {
    /// Exact (sky, block) light at one spawn position, from the same engine
    /// used to serialize Protocol 5 chunk lighting.
    pub fn light_at(&self, x: i32, y: i32, z: i32) -> (u8, u8) {
        if !(0..=255).contains(&y) { return (0, 0); }
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let (block, sky) = self.light_arrays(cx, cz);
        let index = ((y as usize * 16 + z.rem_euclid(16) as usize) * 16
            + x.rem_euclid(16) as usize);
        (sky[index], block[index])
    }
    /// Returns separate 16x256x16 block-light and sky-light nibble values.
    /// Direct sky columns are computed top-down. Artificial block light
    /// travels through air/transparent blocks for at most 15 blocks and may
    /// cross chunk boundaries (sources are searched in a 15-block halo).
    pub(super) fn light_arrays(&self, chunk_x: i32, chunk_z: i32) -> (Vec<u8>, Vec<u8>) {
        let min_x = chunk_x * 16;
        let min_z = chunk_z * 16;
        let mut sky = vec![0u8; 16 * 256 * 16];
        let mut block = vec![0u8; 16 * 256 * 16];
        // Direct skylight needs only a one-cell halo to illuminate the
        // chunk boundary. Cache transparency while scanning each column,
        // so a later spread never resamples expensive generated terrain.
        let mut direct = vec![0u8; 18 * 18 * 256];
        let mut passable = vec![false; 16 * 256 * 16];
        let direct_index = |x: i32, y: i32, z: i32| -> usize {
            ((y as usize * 18 + (z - min_z + 1) as usize) * 18) + (x - min_x + 1) as usize
        };
        for z in min_z - 1..min_z + 17 {
            for x in min_x - 1..min_x + 17 {
                let mut sun = 15u8;
                for y in (0..256).rev() {
                    let air = transparent(self.get(x, y, z) >> 4);
                    if !air {
                        sun = 0;
                    }
                    direct[direct_index(x, y, z)] = sun;
                    if (min_x..min_x + 16).contains(&x) && (min_z..min_z + 16).contains(&z) {
                        let slot = index(x, y, z, min_x, min_z);
                        sky[slot] = sun;
                        passable[slot] = air;
                    }
                }
            }
        }
        // Seed only shadow cells touching direct sunlight; avoid queuing
        // tens of thousands of already-lit sky cells on every chunk packet.
        let mut sky_queue = VecDeque::<(i32, i32, i32, u8)>::new();
        for y in 0..256 {
            for z in min_z..min_z + 16 {
                for x in min_x..min_x + 16 {
                    let slot = index(x, y, z, min_x, min_z);
                    if sky[slot] != 0 || !passable[slot] {
                        continue;
                    }
                    if [(1, 0), (-1, 0), (0, 1), (0, -1)]
                        .iter()
                        .any(|&(dx, dz)| direct[direct_index(x + dx, y, z + dz)] == 15)
                    {
                        sky[slot] = 14;
                        sky_queue.push_back((x, y, z, 14));
                    }
                }
            }
        }
        while let Some((x, y, z, level)) = sky_queue.pop_front() {
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
                if !(min_x..min_x + 16).contains(&nx)
                    || !(min_z..min_z + 16).contains(&nz)
                    || !(0..256).contains(&ny)
                {
                    continue;
                }
                let slot = index(nx, ny, nz, min_x, min_z);
                let next = level - 1;
                if next > sky[slot] && passable[slot] {
                    sky[slot] = next;
                    sky_queue.push_back((nx, ny, nz, next));
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

#[cfg(test)]
mod regression_tests {
    use super::*;

    #[test]
    fn skylight_flows_sideways_under_a_one_block_overhang() {
        let mut world = World::new();
        world.import_block(1, 70, 1, 1 << 4).unwrap();
        let (_, sky) = world.light_arrays(0, 0);
        let under_roof = index(1, 69, 1, 0, 0);
        assert_eq!(sky[under_roof], 14);
        assert_eq!(sky[index(0, 69, 1, 0, 0)], 15);
    }

    #[test]
    fn torch_light_levels_are_recalculated_after_placement_and_removal() {
        let mut world = World::new();
        let (empty, _) = world.light_arrays(0, 0);
        assert_eq!(empty[index(1, 64, 1, 0, 0)], 0);
        world.import_block(1, 64, 1, 50 << 4).unwrap();
        let (lit, _) = world.light_arrays(0, 0);
        assert_eq!(lit[index(1, 64, 1, 0, 0)], 14);
        assert_eq!(lit[index(2, 64, 1, 0, 0)], 13);
    }
}
