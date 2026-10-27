//! Java Edition 1.7.10 GenLayer seed and coordinate RNG primitives.
//!
//! This is intentionally separate from IRW4's existing biome selection:
//! changing the seed algorithm of an existing IRW4 world would regenerate
//! untouched chunks differently after restart. A future versioned generator
//! can opt into these primitives without modifying saved IRW1–IRW4 worlds.
// Isolated A01 layers: not wired into IRW4 world creation.
#[allow(dead_code)]
mod add_island;
#[allow(dead_code)]
mod island_zoom;
#[allow(dead_code)]
mod river;

const MULTIPLIER: i64 = 6_364_136_223_846_793_005;
const ADDEND: i64 = 1_442_695_040_888_963_407;

/// Java `long` overflow semantics for GenLayer's nonlinear LCG.
fn advance(value: i64, salt: i64) -> i64 {
    value
        .wrapping_mul(value.wrapping_mul(MULTIPLIER).wrapping_add(ADDEND))
        .wrapping_add(salt)
}

/// Per-layer GenLayer RNG. The layer salt and world seed are signed Java longs.
#[derive(Clone, Copy, Debug)]
pub(super) struct GenLayerRng {
    world_seed: i64,
    chunk_seed: i64,
}

impl GenLayerRng {
    /// Matches GenLayer's three base-seed and three world-seed mixing rounds.
    pub(super) fn new(world_seed: i64, layer_salt: i64) -> Self {
        let mut base_seed = layer_salt;
        for _ in 0..3 {
            base_seed = advance(base_seed, layer_salt);
        }
        let mut mixed_world_seed = world_seed;
        for _ in 0..3 {
            mixed_world_seed = advance(mixed_world_seed, base_seed);
        }
        Self {
            world_seed: mixed_world_seed,
            chunk_seed: mixed_world_seed,
        }
    }

    /// Initialize coordinate-local state, including negative coordinates.
    /// Call for each new cell; traversal order never affects the output.
    pub(super) fn start_chunk(&mut self, x: i64, z: i64) {
        self.chunk_seed = self.world_seed;
        for salt in [x, z, x, z] {
            self.chunk_seed = advance(self.chunk_seed, salt);
        }
    }

    /// Java's signed right-shift, remainder and negative-remainder adjustment.
    pub(super) fn next_int(&mut self, bound: i32) -> i32 {
        assert!(bound > 0, "GenLayer bound must be positive");
        let mut result = ((self.chunk_seed >> 24) % i64::from(bound)) as i32;
        if result < 0 {
            result += bound;
        }
        self.chunk_seed = advance(self.chunk_seed, self.world_seed);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_java_long_oracle_for_negative_coordinates_and_overflow() {
        // Independent Java 21 oracle using GenLayer's Java-long arithmetic
        // and 1.7.10 seed-mixing order. These are RNG fixtures, NOT complete
        // Minecraft 1.7.10 biome-map golden vectors.
        let cases: [(i64, i64, i64, i64, i64, [i32; 5]); 4] = [
            (0, 1, 0, 0, 2_669_708_687_513_693_904, [3, 6, 5, 1, 6]),
            (42, 1, -1, -1, 4_791_677_271_619_916_615, [3, 3, 5, 6, 0]),
            (
                -12_345,
                2000,
                -16,
                31,
                -4_508_102_529_938_613_745,
                [1, 6, 1, 5, 4],
            ),
            (
                i64::MAX,
                -1,
                17,
                -32,
                -9_023_192_072_942_061_398,
                [2, 4, 7, 5, 4],
            ),
        ];
        for (world, salt, x, z, expected_chunk_seed, expected_rolls) in cases {
            let mut rng = GenLayerRng::new(world, salt);
            rng.start_chunk(x, z);
            assert_eq!(rng.chunk_seed, expected_chunk_seed);
            let rolls = std::array::from_fn(|_| rng.next_int(10));
            assert_eq!(rolls, expected_rolls);
        }
    }

    #[test]
    fn independent_of_cell_visit_order_and_clone() {
        let mut forward = GenLayerRng::new(42, 2000);
        let mut backward = forward;
        let cells = [(-17, -16), (-16, -17), (-1, 0), (0, -1), (15, 16), (16, 15)];
        let sample = |rng: &mut GenLayerRng, x, z| {
            rng.start_chunk(x, z);
            (rng.next_int(1024), rng.next_int(3))
        };
        let first: Vec<_> = cells
            .iter()
            .map(|&(x, z)| sample(&mut forward, x, z))
            .collect();
        for (index, &(x, z)) in cells.iter().enumerate().rev() {
            assert_eq!(sample(&mut backward, x, z), first[index]);
        }
        forward.start_chunk(-1, 0);
        let mut replay = forward;
        assert_eq!(forward.next_int(1), 0);
        assert_eq!(replay.next_int(1), 0);
        assert_eq!(forward.next_int(10), replay.next_int(10));
    }

    #[test]
    #[should_panic(expected = "GenLayer bound must be positive")]
    fn rejects_zero_bound() {
        GenLayerRng::new(0, 1).next_int(0);
    }
}
