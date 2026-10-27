//! Minecraft Java 1.7.10 GenLayerRemoveTooMuchOcean primitive.
//!
//! This layer is deliberately isolated from the persisted IRW1–IRW4
//! generators. A new generator version must opt in explicitly so that
//! unexplored chunks of existing worlds do not change after an update.

use super::GenLayerRng;

/// Occasionally turn the middle of a five-cell ocean cross into land.
///
/// Vanilla checks the four cardinal neighbors, not the diagonals. The layer
/// consumes a `nextInt(2)` roll only for all-ocean crosses. Coordinates are
/// Java signed 32-bit layer cells, including wraparound at the domain edge.
pub(super) fn remove_too_much_ocean_map<F>(
    world_seed: i64,
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
    let stride = width.checked_add(2).expect("GenLayer width overflow");
    let parent_height = height.checked_add(2).expect("GenLayer height overflow");
    let parent_cells = stride
        .checked_mul(parent_height)
        .expect("GenLayer parent area overflow");
    let output_cells = width
        .checked_mul(height)
        .expect("GenLayer output area overflow");
    let source = parent(
        x.wrapping_sub(1),
        z.wrapping_sub(1),
        stride,
        parent_height,
    );
    assert_eq!(source.len(), parent_cells);

    let mut result = Vec::with_capacity(output_cells);
    let mut rng = GenLayerRng::new(world_seed, 2);
    for dz in 0..height {
        for dx in 0..width {
            let center = (dz + 1) * stride + dx + 1;
            let value = source[center];
            // Java evaluates the i32 coordinate addition before widening to
            // long in initChunkSeed; do not use a non-wrapping i64 sum here.
            rng.start_chunk(
                i64::from(x.wrapping_add(dx as i32)),
                i64::from(z.wrapping_add(dz as i32)),
            );
            let all_ocean = value == 0
                && source[center - stride] == 0
                && source[center + stride] == 0
                && source[center - 1] == 0
                && source[center + 1] == 0;
            result.push(if all_ocean && rng.next_int(2) == 0 {
                1
            } else {
                value
            });
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
                result.push(i32::from(px % 11 == 0 || pz % 7 == 0));
            }
        }
        result
    }

    #[test]
    fn matches_independent_java_long_oracle_for_ocean_and_coast() {
        // Produced with an independent Java long-overflow implementation of
        // the 1.7.10 layer's salt=2 RNG and five-cell cardinal rule.
        let cases: [(i64, i32, i32, [i32; 16], [i32; 16]); 4] = [
            (
                0, 0, 0,
                [0, 0, 1, 0, 0, 1, 1, 0, 0, 0, 1, 1, 1, 0, 1, 0],
                [1, 1, 1, 1, 1, 0, 0, 0, 1, 0, 1, 1, 1, 0, 1, 0],
            ),
            (
                42, -17, 31,
                [0, 1, 0, 0, 0, 1, 0, 0, 1, 0, 1, 0, 0, 1, 0, 1],
                [0, 1, 0, 0, 0, 1, 0, 0, 1, 0, 1, 0, 0, 0, 0, 0],
            ),
            (
                -12_345, 16, -17,
                [0, 1, 1, 1, 0, 0, 1, 0, 1, 1, 1, 1, 0, 1, 1, 0],
                [0, 1, 1, 1, 0, 0, 1, 0, 0, 0, 0, 0, 1, 1, 1, 1],
            ),
            (
                i64::MAX, i32::MAX, i32::MIN,
                [0, 1, 1, 1, 1, 0, 0, 0, 1, 1, 0, 0, 1, 0, 1, 0],
                [0, 1, 0, 1, 0, 0, 0, 1, 1, 1, 1, 1, 0, 0, 0, 1],
            ),
        ];
        for (seed, x, z, ocean_expected, coast_expected) in cases {
            assert_eq!(
                remove_too_much_ocean_map(seed, x, z, 4, 4, |_, _, w, h| vec![0; w * h]),
                ocean_expected,
                "all ocean seed={seed} x={x} z={z}"
            );
            assert_eq!(
                remove_too_much_ocean_map(seed, x, z, 4, 4, patterned_parent),
                coast_expected,
                "coast seed={seed} x={x} z={z}"
            );
        }
    }

    #[test]
    fn never_changes_existing_land_or_nonzero_biomes() {
        let result = remove_too_much_ocean_map(42, -1, -1, 3, 3, |_, _, w, h| {
            vec![24; w * h]
        });
        assert_eq!(result, vec![24; 9]);
        let result = remove_too_much_ocean_map(42, -1, -1, 3, 3, |_, _, w, h| {
            vec![1; w * h]
        });
        assert_eq!(result, vec![1; 9]);
    }

    #[test]
    fn chunk_split_and_reverse_query_order_are_identical() {
        let whole = remove_too_much_ocean_map(42, -17, -17, 16, 16, patterned_parent);
        assert!(whole.contains(&0));
        assert!(whole.contains(&1));
        for dz in (0..16).rev() {
            for dx in (0..16).rev() {
                let one = remove_too_much_ocean_map(
                    42,
                    -17 + dx,
                    -17 + dz,
                    1,
                    1,
                    patterned_parent,
                );
                assert_eq!(one[0], whole[(dz * 16 + dx) as usize]);
            }
        }
    }

    #[test]
    fn empty_requests_never_call_parent() {
        let no_parent = |_, _, _, _| -> Vec<i32> { panic!("unexpected parent request") };
        assert!(remove_too_much_ocean_map(42, 0, 0, 0, 8, no_parent).is_empty());
        assert!(remove_too_much_ocean_map(42, 0, 0, 8, 0, no_parent).is_empty());
    }
}
