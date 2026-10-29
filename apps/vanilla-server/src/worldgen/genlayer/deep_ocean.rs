//! Isolated Minecraft Java 1.7.10 GenLayerDeepOcean.
//!
//! Deep Ocean (24) replaces Ocean (0) only when all four cardinal
//! neighbors are also Ocean. Diagonal neighbors do not count.
//! This layer is NOT connected to the persisted IRW4 generator.

/// Apply the vanilla cardinal-neighbor Deep Ocean rule to a parent layer.
///
/// The parent is queried with a one-cell halo. Java's signed-int coordinate
/// wrapping is preserved at the edge of the coordinate domain. This layer
/// consumes no RNG, so requests are independent of seed and traversal order.
pub(super) fn deep_ocean_map<F>(x: i32, z: i32, width: usize, height: usize, parent: F) -> Vec<i32>
where
    F: Fn(i32, i32, usize, usize) -> Vec<i32>,
{
    if width == 0 || height == 0 {
        return Vec::new();
    }

    let stride = width + 2;
    let parent_height = height + 2;
    let source = parent(x.wrapping_sub(1), z.wrapping_sub(1), stride, parent_height);
    assert_eq!(source.len(), stride * parent_height);

    let mut result = Vec::with_capacity(width * height);
    for dz in 0..height {
        for dx in 0..width {
            let center = (dz + 1) * stride + dx + 1;
            let value = source[center];
            result.push(
                if value == 0
                    && source[center - stride] == 0
                    && source[center + stride] == 0
                    && source[center - 1] == 0
                    && source[center + 1] == 0
                {
                    24
                } else {
                    value
                },
            );
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
                result.push(if px.rem_euclid(11) == 0 || pz.rem_euclid(7) == 0 {
                    1
                } else {
                    0
                });
            }
        }
        result
    }

    #[test]
    fn all_ocean_cells_become_deep_ocean() {
        assert_eq!(
            deep_ocean_map(-17, -17, 4, 4, |_, _, w, h| vec![0; w * h]),
            vec![24; 16]
        );
    }

    #[test]
    fn only_cardinal_land_neighbors_prevent_deep_ocean() {
        let parent = |x: i32, z: i32, w: usize, h: usize| {
            let mut values = Vec::with_capacity(w * h);
            for dz in 0..h {
                for dx in 0..w {
                    values.push(i32::from(
                        x.wrapping_add(dx as i32) == 0 && z.wrapping_add(dz as i32) == 0,
                    ));
                }
            }
            values
        };
        assert_eq!(
            deep_ocean_map(-1, -1, 3, 3, parent),
            [24, 0, 24, 0, 1, 0, 24, 0, 24]
        );
    }

    #[test]
    fn preserves_existing_deep_ocean_and_non_ocean_biomes() {
        let parent = |x: i32, z: i32, w: usize, h: usize| {
            let mut values = Vec::with_capacity(w * h);
            for dz in 0..h {
                for dx in 0..w {
                    let point = (x.wrapping_add(dx as i32), z.wrapping_add(dz as i32));
                    values.push(match point {
                        (0, 0) => 24,
                        (2, 0) => 14,
                        _ => 0,
                    });
                }
            }
            values
        };
        assert_eq!(deep_ocean_map(0, 0, 3, 1, parent), [24, 0, 14]);
    }

    #[test]
    fn split_requests_match_negative_chunk_boundaries_and_reverse_order() {
        let whole = deep_ocean_map(-17, -17, 16, 16, patterned_parent);
        assert!(whole.contains(&0));
        assert!(whole.contains(&24));
        assert!(whole.contains(&1));
        for dz in (0..16).rev() {
            for dx in (0..16).rev() {
                let one = deep_ocean_map(-17 + dx, -17 + dz, 1, 1, patterned_parent);
                assert_eq!(one[0], whole[(dz * 16 + dx) as usize]);
            }
        }
    }

    #[test]
    fn signed_coordinate_wraparound_and_empty_requests() {
        let whole = deep_ocean_map(i32::MAX, i32::MIN, 2, 2, patterned_parent);
        for dz in 0..2 {
            for dx in 0..2 {
                let one = deep_ocean_map(
                    i32::MAX.wrapping_add(dx),
                    i32::MIN.wrapping_add(dz),
                    1,
                    1,
                    patterned_parent,
                );
                assert_eq!(one[0], whole[(dz * 2 + dx) as usize]);
            }
        }
        assert!(deep_ocean_map(0, 0, 0, 8, |_, _, _, _| -> Vec<i32> {
            panic!("empty request evaluated parent")
        })
        .is_empty());
        assert!(deep_ocean_map(0, 0, 8, 0, |_, _, _, _| -> Vec<i32> {
            panic!("empty request evaluated parent")
        })
        .is_empty());
    }
}
