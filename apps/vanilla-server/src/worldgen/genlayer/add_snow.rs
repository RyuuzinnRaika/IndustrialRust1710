//! Minecraft Java 1.7.10 GenLayerAddSnow climate-band assignment.
//!
//! This is an isolated layer, not the live IRW4 biome generator. Saved IRW4
//! worlds must not change their ungenerated chunks without versioning.
use super::GenLayerRng;

/// Convert non-ocean parent cells to vanilla 1.7.10 climate bands:
/// warm (1), cool (3), or cold (4). Ocean (0) remains ocean.
///
/// The vanilla layer consumes one nextInt(6) per land cell; ocean cells do
/// not consume a roll. Coordinate-local RNG makes query order irrelevant.
pub(super) fn add_snow_map<F>(
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
    let source = parent(x, z, width, height);
    assert_eq!(source.len(), width * height);
    let mut result = vec![0; width * height];
    let mut rng = GenLayerRng::new(world_seed, layer_salt);
    for dz in 0..height {
        for dx in 0..width {
            let index = dz * width + dx;
            if source[index] != 0 {
                // Java's getInts uses signed int coordinate arithmetic.
                let cell_x = x.wrapping_add(dx as i32);
                let cell_z = z.wrapping_add(dz as i32);
                rng.start_chunk(i64::from(cell_x), i64::from(cell_z));
                let roll = rng.next_int(6);
                result[index] = if roll == 0 {
                    4
                } else if roll <= 1 {
                    3
                } else {
                    1
                };
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patterned_parent(x: i32, z: i32, width: usize, height: usize) -> Vec<i32> {
        let mut values = Vec::with_capacity(width * height);
        for dz in 0..height {
            for dx in 0..width {
                let px = x.wrapping_add(dx as i32);
                let pz = z.wrapping_add(dz as i32);
                values.push(i32::from(((px ^ pz) & 7) != 0));
            }
        }
        values
    }

    fn climate_digits(values: &[i32]) -> String {
        values
            .iter()
            .map(|&value| char::from_digit(value as u32, 10).unwrap())
            .collect()
    }

    #[test]
    fn matches_independent_java_long_climate_oracle() {
        // Generated with tools/GenLayerAddSnowOracle.java, which independently
        // implements Java signed-long overflow and GenLayer seed propagation.
        for (seed, x, z, expected) in [
            (
                42,
                -17,
                7,
                concat!(
                    "04111431", "10111111", "33043111", "11401411", "41130131", "41411013",
                    "11114301", "41414110"
                ),
            ),
            (
                -12_345,
                -17,
                -17,
                concat!(
                    "04111111", "10113314", "11013411", "41101311", "31130143", "11131011",
                    "11131101", "11114330"
                ),
            ),
            (
                0,
                15,
                -17,
                concat!(
                    "04111131", "10111413", "11011131", "11101111", "11140411", "11133011",
                    "11411103", "43111110"
                ),
            ),
            (
                i64::MAX,
                -1,
                -1,
                concat!(
                    "01411114", "10141111", "13031411", "11303111", "43410111", "13113031",
                    "11131101", "11314110"
                ),
            ),
        ] {
            let actual = add_snow_map(seed, 2, x, z, 8, 8, patterned_parent);
            assert_eq!(
                climate_digits(&actual),
                expected,
                "seed={seed} at ({x},{z})"
            );
        }
    }

    #[test]
    fn preserves_ocean_and_only_emits_vanilla_climate_bands() {
        assert_eq!(
            add_snow_map(42, 2, -17, 7, 4, 4, |_, _, w, h| vec![0; w * h]),
            vec![0; 16]
        );
        let map = add_snow_map(42, 2, -17, 7, 16, 16, patterned_parent);
        assert!(map.iter().all(|&value| matches!(value, 0 | 1 | 3 | 4)));
        assert!(map.contains(&0));
        assert!(map.contains(&1));
        assert!(map.contains(&3));
        assert!(map.contains(&4));
    }

    #[test]
    fn stable_across_negative_chunk_boundaries_and_request_order() {
        let whole = add_snow_map(42, 2, -17, -17, 8, 8, patterned_parent);
        for dz in (0..8).rev() {
            for dx in (0..8).rev() {
                let single = add_snow_map(42, 2, -17 + dx, -17 + dz, 1, 1, patterned_parent);
                assert_eq!(single[0], whole[(dz * 8 + dx) as usize]);
            }
        }
        assert!(add_snow_map(42, 2, 0, 0, 0, 8, patterned_parent).is_empty());
        assert!(add_snow_map(42, 2, 0, 0, 8, 0, patterned_parent).is_empty());
    }
}
