//! Isolated Minecraft Java 1.7.10 GenLayerBiome climate-to-biome mapping.
//!
//! This layer is deliberately NOT connected to live IRW4 generation: existing
//! saved worlds must retain their original ungenerated-chunk results. The
//! input is the climate/special-bit map produced by preceding GenLayers.
use super::GenLayerRng;

/// Vanilla 1.7.10 default-world biome pools. Repeated IDs are weights.
/// These are *not* the biome height or terrain-noise parameters.
const HOT: [i32; 6] = [2, 2, 2, 35, 35, 1];
const WARM: [i32; 6] = [4, 29, 3, 1, 27, 6];
const COOL: [i32; 4] = [4, 3, 5, 1];
const ICY: [i32; 4] = [12, 12, 12, 30];
const DEFAULT_1_1_HOT: [i32; 6] = [2, 4, 3, 6, 1, 5];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BiomeWorldType {
    Default,
    Default11,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct BiomeLayer {
    world_seed: i64,
    layer_salt: i64,
    world_type: BiomeWorldType,
}

impl BiomeLayer {
    pub(super) fn new(world_seed: i64, layer_salt: i64, world_type: BiomeWorldType) -> Self {
        Self { world_seed, layer_salt, world_type }
    }
}

/// Translate parent climate IDs and special flags to vanilla biome IDs.
///
/// The upper 0xF00 bits carry the special-biome marker from GenLayerEdge.
/// Ocean (0), Deep Ocean (24), and Mushroom Island (14) bypass selection.
/// Each cell seeds its own Java-long RNG, so split/overlapping requests are
/// deterministic even for negative coordinates and signed-int wraparound.
pub(super) fn biome_map<F>(
    layer: BiomeLayer,
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
    let mut result = Vec::with_capacity(source.len());
    let mut rng = GenLayerRng::new(layer.world_seed, layer.layer_salt);

    for dz in 0..height {
        for dx in 0..width {
            let raw = source[dz * width + dx];
            let special = (raw & 0xF00) >> 8;
            let climate = raw & !0xF00;
            let cell_x = x.wrapping_add(dx as i32);
            let cell_z = z.wrapping_add(dz as i32);
            rng.start_chunk(i64::from(cell_x), i64::from(cell_z));

            let id = match climate {
                0 | 24 | 14 => climate,
                1 if special > 0 => {
                    if rng.next_int(3) == 0 {
                        39 // Mesa Plateau
                    } else {
                        38 // Mesa Plateau F
                    }
                }
                2 if special > 0 => 21, // Jungle
                3 if special > 0 => 32, // Mega Taiga
                1..=4 => {
                    let pool: &[i32] = match climate {
                        1 if layer.world_type == BiomeWorldType::Default11 => &DEFAULT_1_1_HOT,
                        1 => &HOT,
                        2 => &WARM,
                        3 => &COOL,
                        _ => &ICY,
                    };
                    pool[rng.next_int(pool.len() as i32) as usize]
                }
                _ => 14, // Vanilla's fallback for unexpected climate values
            };
            result.push(id);
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
                let raw = match (px ^ pz.wrapping_mul(3)) & 15 {
                    0 => 0,
                    1 => 24,
                    2 => 14,
                    3 => 1,
                    4 => 2,
                    5 => 3,
                    6 => 4,
                    7 => 1 | 0x100,
                    8 => 2 | 0x100,
                    9 => 3 | 0x100,
                    10 => 4 | 0x100,
                    11 => 1,
                    12 => 2,
                    13 => 3,
                    14 => 4,
                    _ => 0,
                };
                values.push(raw);
            }
        }
        values
    }

    fn biome_hex(values: &[i32]) -> String {
        values
            .iter()
            .map(|value| format!("{value:02x}"))
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn matches_independent_java_long_biome_fixtures() {
        // Produced by a separate Java 21 implementation of the signed-long
        // GenLayer RNG and 1.7.10 climate pools, using patterned_parent.
        // Each pair of hex digits is one biome ID, in row-major order.
        let cases = [
            (
                42,
                -17,
                -17,
                BiomeWorldType::Default,
                concat!(
                    "0e011b000c201502", "0000180e0206010c",
                    "1b230e1800270c01", "200c2606040e0100",
                    "0c2015021e030100", "2303030c0015200c",
                    "00000c031b020c20", "010e2300180c2601"
                ),
            ),
            (
                -12_345,
                15,
                -17,
                BiomeWorldType::Default,
                concat!(
                    "0e0101000c201523", "0000180e021b031e",
                    "01230e1800260c05", "200c2604040e2300",
                    "0c2015020c010100", "0201030c0015200c",
                    "00000c0403010c20", "030e0200181e2704"
                ),
            ),
            (
                i64::MAX,
                -1,
                -1,
                BiomeWorldType::Default,
                concat!(
                    "0e0101000c201502", "0000180e0206010c",
                    "01020e1800260c03", "200c271b040e2300",
                    "0c2015021e030600", "0203031e0015200c",
                    "00001e0304230c20", "030e0200181e2606"
                ),
            ),
            (
                42,
                -17,
                15,
                BiomeWorldType::Default11,
                concat!(
                    "0e0404000c201502", "0000180e0506030c",
                    "1b050e1800260c03", "201e2706030e0600",
                    "0c2015050c040100", "0303040c0015200c",
                    "00000c0503020c20", "050e0100181e261b"
                ),
            ),
        ];
        for (seed, x, z, world_type, expected) in cases {
            let actual = biome_map(BiomeLayer::new(seed, 200, world_type), x, z, 8, 8, patterned_parent);
            assert_eq!(biome_hex(&actual), expected, "seed={seed} at ({x},{z})");
        }
    }

    #[test]
    fn preserves_ocean_mushroom_and_special_biome_rules() {
        let source = [0, 24, 14, 1 | 0x100, 2 | 0x100, 3 | 0x100, 4 | 0x100, 5];
        let result = biome_map(BiomeLayer::new(42, 200, BiomeWorldType::Default), -16, 15, 8, 1, |_, _, _, _| {
            source.to_vec()
        });
        assert_eq!(&result[..3], &[0, 24, 14]);
        assert!(matches!(result[3], 38 | 39));
        assert_eq!(result[4], 21);
        assert_eq!(result[5], 32);
        assert!(matches!(result[6], 12 | 30));
        assert_eq!(result[7], 14);
    }

    #[test]
    fn split_queries_match_across_negative_chunk_boundaries() {
        for world_type in [BiomeWorldType::Default, BiomeWorldType::Default11] {
            let whole = biome_map(BiomeLayer::new(42, 200, world_type), -17, -17, 8, 8, patterned_parent);
            for dz in (0..8).rev() {
                for dx in (0..8).rev() {
                    let single = biome_map(
                        BiomeLayer::new(42, 200, world_type),
                        -17 + dx,
                        -17 + dz,
                        1,
                        1,
                        patterned_parent,
                    );
                    assert_eq!(single[0], whole[(dz * 8 + dx) as usize]);
                }
            }
        }
    }

    #[test]
    fn signed_coordinate_wraparound_and_empty_requests() {
        let whole = biome_map(
            BiomeLayer::new(42, 200, BiomeWorldType::Default),
            i32::MAX - 1,
            i32::MIN,
            4,
            2,
            patterned_parent,
        );
        for dz in 0..2 {
            for dx in 0..4 {
                let single = biome_map(
                    BiomeLayer::new(42, 200, BiomeWorldType::Default),
                    (i32::MAX - 1).wrapping_add(dx),
                    i32::MIN.wrapping_add(dz),
                    1,
                    1,
                    patterned_parent,
                );
                assert_eq!(single[0], whole[(dz * 4 + dx) as usize]);
            }
        }
        assert!(biome_map(
            BiomeLayer::new(42, 200, BiomeWorldType::Default), 0, 0, 0, 8,
            |_, _, _, _| panic!("empty request evaluated parent")
        ).is_empty());
        assert!(biome_map(
            BiomeLayer::new(42, 200, BiomeWorldType::Default), 0, 0, 8, 0,
            |_, _, _, _| panic!("empty request evaluated parent")
        ).is_empty());
    }
}
