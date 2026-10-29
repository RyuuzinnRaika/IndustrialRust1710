//! Minecraft Java 1.7.10 GenLayerRareBiome (Sunflower Plains).
//!
//! Isolated from IRW1-IRW4: changing the persisted generator would alter
//! ungenerated chunks of existing worlds. Wire this only to a new, versioned
//! world generator after the complete GenLayer chain is verified.
use super::GenLayerRng;

/// Vanilla RareBiome: one chance in 57 to replace Plains (ID 1) with
/// Sunflower Plains (ID 129). Other biomes, including ocean, are unchanged.
///
/// Vanilla asks its parent for a one-cell halo, even though this layer only
/// reads each center cell. Coordinates wrap as signed Java int values.
pub(super) fn rare_biome_map<F>(
    world_seed: i64,
    layer_salt: i64,
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
    let parent_width = width.checked_add(2).expect("RareBiome width overflow");
    let parent_height = height.checked_add(2).expect("RareBiome height overflow");
    let source = parent(
        x.wrapping_sub(1),
        z.wrapping_sub(1),
        parent_width,
        parent_height,
    );
    assert_eq!(
        source.len(),
        parent_width
            .checked_mul(parent_height)
            .expect("RareBiome parent area overflow")
    );

    let mut result = Vec::with_capacity(
        width
            .checked_mul(height)
            .expect("RareBiome output area overflow"),
    );
    let mut rng = GenLayerRng::new(world_seed, layer_salt);
    for dz in 0..height {
        for dx in 0..width {
            let biome = source[(dz + 1) * parent_width + dx + 1];
            rng.start_chunk(
                i64::from(x.wrapping_add(dx as i32)),
                i64::from(z.wrapping_add(dz as i32)),
            );
            // Consume the vanilla roll even when the parent is not Plains.
            let rare = rng.next_int(57) == 0;
            result.push(if rare && biome == 1 { 129 } else { biome });
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
                let id = match (px ^ pz.wrapping_mul(3)) & 7 {
                    0..=3 => 1,
                    4 => 0,
                    5 => 24,
                    6 => 4,
                    _ => 2,
                };
                result.push(id);
            }
        }
        result
    }

    fn java_long_checksum(values: &[i32]) -> u64 {
        values.iter().fold(0u64, |sum, &id| {
            sum.wrapping_mul(1_315_423_911).wrapping_add(id as u64)
        })
    }

    #[test]
    fn matches_independent_java_21_oracle_across_negative_and_wrapped_coordinates() {
        // Standalone Java-long reference: GenLayerRareBiome, salt 1001,
        // patterned_parent, 16x16. Check both exact rare positions and every
        // output cell via a Java-long overflow checksum.
        let cases: &[(i64, i32, i32, &[usize], u64)] = &[
            (42, -17, -17, &[], 0xb574_4e49_d73f_dd00),
            (-12_345, 15, -17, &[13, 25, 53, 231], 0x1f2b_5268_1db4_f700),
            (0, -1, -1, &[202], 0x844d_f4df_2308_0080),
            (
                i64::MAX,
                i32::MAX - 2,
                i32::MIN,
                &[12, 62, 74, 140],
                0xce78_4894_220d_eb00,
            ),
        ];
        for &(seed, x, z, expected_rare, expected_checksum) in cases {
            let result = rare_biome_map(seed, 1001, x, z, 16, 16, patterned_parent);
            let rare: Vec<_> = result
                .iter()
                .enumerate()
                .filter_map(|(index, &biome)| (biome == 129).then_some(index))
                .collect();
            assert_eq!(rare.as_slice(), expected_rare, "seed={seed}, ({x},{z})");
            assert_eq!(
                java_long_checksum(&result),
                expected_checksum,
                "seed={seed}, ({x},{z})"
            );
        }
    }

    #[test]
    fn preserves_non_plains_and_already_mutated_biomes() {
        for id in [0, 2, 4, 14, 24, 129] {
            let result = rare_biome_map(42, 1001, -32, -32, 32, 32, |_, _, w, h| vec![id; w * h]);
            assert!(result.iter().all(|&value| value == id), "biome {id}");
        }
    }

    #[test]
    fn split_queries_match_full_region_across_negative_chunk_boundaries() {
        let seed = -12_345;
        let whole = rare_biome_map(seed, 1001, -17, -17, 16, 16, patterned_parent);
        for dz in (0..16).rev() {
            for dx in (0..16).rev() {
                let single = rare_biome_map(seed, 1001, -17 + dx, -17 + dz, 1, 1, patterned_parent);
                assert_eq!(single[0], whole[(dz * 16 + dx) as usize]);
            }
        }
    }

    #[test]
    fn empty_requests_never_read_the_parent() {
        assert!(rare_biome_map(42, 1001, 0, 0, 0, 16, |_, _, _, _| {
            panic!("empty request should not query parent")
        })
        .is_empty());
        assert!(rare_biome_map(42, 1001, 0, 0, 16, 0, |_, _, _, _| {
            panic!("empty request should not query parent")
        })
        .is_empty());
    }
}
