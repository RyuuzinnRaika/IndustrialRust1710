//! Isolated Minecraft Java 1.7.10 GenLayerSmooth implementation.
//!
//! Smooths cardinal-neighbor boundaries before river/biome composition.
//! This is not wired into IRW4: existing saved worlds must not change their
//! still-unexplored chunks after a server restart.
use super::GenLayerRng;

#[derive(Clone, Copy, Debug)]
pub(super) struct SmoothLayer {
    world_seed: i64,
    layer_salt: i64,
}

impl SmoothLayer {
    pub(super) fn new(world_seed: i64, layer_salt: i64) -> Self {
        Self {
            world_seed,
            layer_salt,
        }
    }
}

/// Apply the 1.7.10 cardinal-neighbor smoothing rule to a parent GenLayer.
///
/// Parent cells are requested with a one-cell halo. If both opposing pairs
/// match, the coordinate-seeded Java RNG breaks the tie. If only one pair
/// matches, that pair wins. The parent may return arbitrary biome/river IDs.
pub(super) fn smooth_map<F>(
    layer: SmoothLayer,
    x: i32,
    z: i32,
    width: usize,
    height: usize,
    parent: F,
) -> Vec<i32>
where
    F: Fn(i32, i32, usize, usize) -> Vec<i32>,
{
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let halo_width = width.checked_add(2).expect("smooth width overflow");
    let halo_height = height.checked_add(2).expect("smooth height overflow");
    let expected_len = halo_width
        .checked_mul(halo_height)
        .expect("smooth parent area overflow");
    let source = parent(
        x.wrapping_sub(1),
        z.wrapping_sub(1),
        halo_width,
        halo_height,
    );
    assert_eq!(source.len(), expected_len, "smooth parent area mismatch");

    let mut result = Vec::with_capacity(width.checked_mul(height).expect("smooth area overflow"));
    let mut rng = GenLayerRng::new(layer.world_seed, layer.layer_salt);
    for dz in 0..height {
        for dx in 0..width {
            let center_index = (dz + 1) * halo_width + dx + 1;
            let west = source[center_index - 1];
            let east = source[center_index + 1];
            let north = source[center_index - halo_width];
            let south = source[center_index + halo_width];
            let mut center = source[center_index];

            if west == east && north == south {
                rng.start_chunk(
                    i64::from(x.wrapping_add(dx as i32)),
                    i64::from(z.wrapping_add(dz as i32)),
                );
                center = if rng.next_int(2) == 0 { west } else { north };
            } else {
                if west == east {
                    center = west;
                }
                if north == south {
                    center = north;
                }
            }
            result.push(center);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patterned_parent(x: i32, z: i32, width: usize, height: usize) -> Vec<i32> {
        let mut result = Vec::with_capacity(width * height);
        for dz in 0..height {
            for dx in 0..width {
                let px = x.wrapping_add(dx as i32);
                let pz = z.wrapping_add(dz as i32);
                let bump = if ((px ^ pz) & 7) == 0 { 4 } else { 0 };
                result.push((px & 1) + ((pz & 1) << 1) + bump);
            }
        }
        result
    }

    fn as_hex(values: &[i32]) -> String {
        values
            .iter()
            .map(|value| format!("{value:02x}"))
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn matches_independent_java_1_7_10_smooth_oracle() {
        // Independently generated with a Java 21 signed-long LCG oracle;
        // the reference program used a coordinate-addressable parent pattern.
        // Each two hex digits represent one 1.7.10 parent/child map cell.
        let cases = [
            (
                42,
                -17,
                -17,
                concat!(
                    "0102020301000202", "0101010203010001",
                    "0102010202000203", "0301010201010002",
                    "0100010201020103", "0301000201010101",
                    "0200020302020102", "0101000103010101"
                ),
            ),
            (
                -12_345,
                15,
                -17,
                concat!(
                    "0102010301030202", "0102010203020301",
                    "0102020202000203", "0001010201010001",
                    "0200020202020200", "0301030201010101",
                    "0103010302020102", "0102000103010101"
                ),
            ),
            (
                i64::MAX,
                -1,
                15,
                concat!(
                    "0102020302030202", "0101010200010301",
                    "0102010201030200", "0001010101010001",
                    "0203010201020103", "0002000201020101",
                    "0100010002020102", "0101030200020102"
                ),
            ),
            (
                42,
                i32::MAX - 2,
                i32::MIN + 1,
                concat!(
                    "0200010201020203", "0301030101010102",
                    "0203010002020202", "0101030103020101",
                    "0102010301000202", "0101010103020002",
                    "0202010202030100", "0001010101010302"
                ),
            ),
        ];
        for (seed, x, z, expected) in cases {
            let actual = smooth_map(SmoothLayer::new(seed, 1000), x, z, 8, 8, patterned_parent);
            assert_eq!(as_hex(&actual), expected, "seed={seed} at ({x},{z})");
        }
    }

    #[test]
    fn respects_cardinal_ties_and_preserves_unmatched_center() {
        let layer = SmoothLayer::new(42, 1000);
        let single = |grid: [i32; 9]| {
            smooth_map(layer, 0, 0, 1, 1, |_, _, _, _| grid.to_vec())[0]
        };
        assert_eq!(single([9, 1, 9, 2, 7, 2, 9, 3, 9]), 2);
        assert_eq!(single([9, 1, 9, 2, 7, 3, 9, 1, 9]), 1);
        assert_eq!(single([9, 1, 9, 2, 7, 3, 9, 4, 9]), 7);
        assert!(matches!(single([9, 1, 9, 2, 7, 2, 9, 1, 9]), 1 | 2));
    }

    #[test]
    fn negative_chunk_boundaries_and_split_queries_are_stable() {
        let layer = SmoothLayer::new(-12_345, 1000);
        let whole = smooth_map(layer, -17, -17, 8, 8, patterned_parent);
        for dz in (0..8).rev() {
            for dx in (0..8).rev() {
                let one = smooth_map(
                    layer,
                    -17 + dx,
                    -17 + dz,
                    1,
                    1,
                    patterned_parent,
                );
                assert_eq!(one[0], whole[(dz * 8 + dx) as usize]);
            }
        }
    }

    #[test]
    fn coordinate_wraparound_and_empty_requests() {
        let layer = SmoothLayer::new(42, 1000);
        let x = i32::MAX - 2;
        let z = i32::MIN + 1;
        let whole = smooth_map(layer, x, z, 8, 8, patterned_parent);
        for dz in 0..8 {
            for dx in 0..8 {
                let one = smooth_map(
                    layer,
                    x.wrapping_add(dx),
                    z.wrapping_add(dz),
                    1,
                    1,
                    patterned_parent,
                );
                assert_eq!(one[0], whole[(dz * 8 + dx) as usize]);
            }
        }
        assert!(smooth_map(layer, 0, 0, 0, 8, |_, _, _, _| panic!("parent evaluated")).is_empty());
        assert!(smooth_map(layer, 0, 0, 8, 0, |_, _, _, _| panic!("parent evaluated")).is_empty());
    }
}
