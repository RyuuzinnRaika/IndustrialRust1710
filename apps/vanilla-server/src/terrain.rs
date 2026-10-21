//! Deterministic, pure-Rust bounded terrain synthesis for Protocol 5.
//! This is a new generator, not the original 1.7.10 biome/noise algorithm.
//! No allocation, RNG global state, Java dependency or cross-chunk seams.
const BEDROCK: u16 = 7 << 4;
const DIRT: u16 = 3 << 4;
const GRASS: u16 = 2 << 4;
const LOG: u16 = 17 << 4;
const LEAVES: u16 = 18 << 4;

fn hash(seed: u64, x: i32, y: i32, z: i32) -> u64 {
    let mut value = seed
        ^ (x as i64 as u64).wrapping_mul(0x9e3779b185ebca87)
        ^ (y as i64 as u64).wrapping_mul(0xc2b2ae3d27d4eb4f)
        ^ (z as i64 as u64).wrapping_mul(0x165667b19e3779f9);
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58476d1ce4e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d049bb133111eb);
    value ^ (value >> 31)
}
fn sample(seed: u64, x: i32, y: i32, z: i32) -> f64 {
    // Exactly reproducible signed noise independent of build architecture.
    (hash(seed, x, y, z) >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
}
fn smooth(t: f64) -> f64 {
    t * t * (3.0 - 2.0 * t)
}
fn noise2(seed: u64, x: i32, z: i32, span: i32) -> f64 {
    let bx = x.div_euclid(span);
    let bz = z.div_euclid(span);
    let dx = smooth(f64::from(x.rem_euclid(span)) / f64::from(span));
    let dz = smooth(f64::from(z.rem_euclid(span)) / f64::from(span));
    let at = |ox, oz| sample(seed, bx + ox, 0, bz + oz);
    let a = at(0, 0) * (1.0 - dx) + at(1, 0) * dx;
    let b = at(0, 1) * (1.0 - dx) + at(1, 1) * dx;
    a * (1.0 - dz) + b * dz
}
fn noise3(seed: u64, x: i32, y: i32, z: i32, span: i32) -> f64 {
    let bx = x.div_euclid(span);
    let by = y.div_euclid(span);
    let bz = z.div_euclid(span);
    let dx = smooth(f64::from(x.rem_euclid(span)) / f64::from(span));
    let dy = smooth(f64::from(y.rem_euclid(span)) / f64::from(span));
    let dz = smooth(f64::from(z.rem_euclid(span)) / f64::from(span));
    let mut output = 0.0;
    for oz in 0..=1 {
        for oy in 0..=1 {
            for ox in 0..=1 {
                let wx = if ox == 0 { 1.0 - dx } else { dx };
                let wy = if oy == 0 { 1.0 - dy } else { dy };
                let wz = if oz == 0 { 1.0 - dz } else { dz };
                output += sample(seed, bx + ox, by + oy, bz + oz) * wx * wy * wz;
            }
        }
    }
    output
}

/// Surface grass block Y. Shared by chunks, spawn selection, and trees.
pub fn surface(seed: u64, x: i32, z: i32) -> i32 {
    let coarse = noise2(seed ^ 0x241d, x, z, 64);
    let detail = noise2(seed ^ 0x5ed3, x, z, 20);
    (70.0 + coarse * 12.0 + detail * 5.0)
        .round()
        .clamp(52.0, 90.0) as i32
}
fn tree(seed: u64, x: i32, z: i32) -> bool {
    // Reserve a safe player spawn radius; always cross-check the same world seed.
    (x.abs() > 12 || z.abs() > 12) && hash(seed ^ 0x6ac2d, x, 0, z).is_multiple_of(73)
}
fn ore(seed: u64, x: i32, y: i32, z: i32) -> u16 {
    let pick = hash(
        seed ^ 0xa037,
        x.div_euclid(3),
        y.div_euclid(3),
        z.div_euclid(3),
    ) % 1000;
    let id = match (y, pick) {
        (0..=15, 0..=7) => 56,    // diamond
        (0..=20, 8..=23) => 73,   // redstone
        (0..=31, 24..=39) => 14,  // gold
        (0..=47, 40..=58) => 21,  // lapis
        (0..=63, 59..=104) => 15, // iron
        (_, 105..=185) => 16,     // coal
        _ => 1,                   // stone
    };
    id << 4
}

/// Stateless generated block including cross-chunk tree crowns, ores and caves.
pub fn block(seed: u64, x: i32, y: i32, z: i32) -> u16 {
    if !(0..=255).contains(&y) {
        return 0;
    }
    if y == 0 {
        return BEDROCK;
    }
    let top = surface(seed, x, z);
    if y <= top {
        if y < top - 4
            && y > 6
            && noise3(seed ^ 0xcafe, x, y, z, 13) > 0.46
            && noise3(seed ^ 0x1337, x, y, z, 29) > -0.2
        {
            return 0; // winding enclosed cave voids
        }
        if y == top {
            return GRASS;
        }
        if y >= top - 3 {
            return DIRT;
        }
        if y <= 3 && hash(seed, x, y, z).is_multiple_of(5) {
            return BEDROCK;
        }
        return ore(seed, x, y, z);
    }
    // A tree on a neighboring chunk contributes its leaves to this one.
    // Avoid a 5x5 neighbor search for most non-vegetation blocks.
    if y > 99 {
        return 0;
    }
    let mut leaf = false;
    for tx in x - 2..=x + 2 {
        for tz in z - 2..=z + 2 {
            if !tree(seed, tx, tz) {
                continue;
            }
            let root = surface(seed, tx, tz);
            if tx == x && tz == z && (root + 1..=root + 4).contains(&y) {
                return LOG;
            }
            let dy = y - (root + 4);
            let radius = if dy == 2 { 1 } else { 2 };
            if (-1..=2).contains(&dy) && (tx - x).abs() <= radius && (tz - z).abs() <= radius {
                leaf = true;
            }
        }
    }
    if leaf {
        LEAVES
    } else {
        0
    }
}

/// Opt-in, versioned biomes. Legacy `block` and `surface` are unchanged,
/// so IRW2 worlds retain exactly the same terrain for the same seed.
pub fn biome(seed: u64, x: i32, z: i32) -> u8 {
    let wet = noise2(seed ^ 0x81ab, x, z, 180);
    let temperature = noise2(seed ^ 0x932c, x, z, 210);
    if wet < -0.32 {
        0
    }
    // ocean
    else if temperature > 0.32 && wet < 0.12 {
        2
    }
    // desert
    else if temperature < -0.35 {
        3
    }
    // mountains
    else if wet > 0.20 {
        4
    }
    // forest
    else {
        1
    } // plains
}

pub fn biome_surface(seed: u64, x: i32, z: i32) -> i32 {
    let base = surface(seed, x, z);
    match biome(seed, x, z) {
        0 => base.min(57),
        3 => (base + 8).min(98),
        _ => base,
    }
}

/// IRW3 generator: plains, forest, desert, mountains and sea. Only this
/// explicitly selected format changes the base terrain algorithm.
pub fn biome_block(seed: u64, x: i32, y: i32, z: i32) -> u16 {
    if !(0..=255).contains(&y) {
        return 0;
    }
    let kind = biome(seed, x, z);
    let top = biome_surface(seed, x, z);
    if y == 0 {
        return BEDROCK;
    }
    if y > top {
        if kind == 0 && y <= 62 {
            return 9 << 4;
        }
        // Existing cross-chunk trees use their legacy surface in forests.
        return if kind == 4 || kind == 1 {
            block(seed, x, y, z)
        } else {
            0
        };
    }
    if y == top {
        return if kind == 2 || kind == 0 {
            12 << 4
        } else {
            GRASS
        };
    }
    if y >= top - 3 {
        return if kind == 2 || kind == 0 {
            12 << 4
        } else {
            DIRT
        };
    }
    // Mountain uplift must be filled rather than leaving a hollow gap
    // between the old and opt-in IRW3 surfaces.
    if y > surface(seed, x, z) {
        return 1 << 4;
    }
    block(seed, x, y, z)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deterministic_negative_coordinates_and_chunk_border_trees() {
        for (x, z) in [(0, 0), (-17, -1), (15, 16), (16, 15), (1024, -512)] {
            let h = surface(42, x, z);
            assert!((52..=90).contains(&h));
            assert_eq!(block(42, x, h, z), GRASS);
            assert_eq!(block(42, x, h + 1, z), 0);
            assert_eq!(block(42, x, 0, z), BEDROCK);
            assert_eq!(block(42, x, 20, z), block(42, x, 20, z));
        }
        assert_ne!(surface(42, 300, 300), surface(43, 300, 300));
    }
    #[test]
    fn world_contains_logs_leaves_mineable_ores_and_caves() {
        let mut logs = 0;
        let mut leaves = 0;
        let mut ores = 0;
        let mut caves = 0;
        for x in -40..40 {
            for z in -40..40 {
                let y = surface(42, x, z);
                if block(42, x, y + 1, z) == LOG {
                    logs += 1;
                }
                if block(42, x, y + 4, z) == LEAVES {
                    leaves += 1;
                }
                if x % 7 == 0 && z % 7 == 0 {
                    for deep in 7..48 {
                        let id = block(42, x, deep, z) >> 4;
                        if matches!(id, 14 | 15 | 16 | 21 | 56 | 73) {
                            ores += 1;
                        }
                        if id == 0 {
                            caves += 1;
                        }
                    }
                }
            }
        }
        assert!(logs > 0 && leaves > 0 && ores > 0 && caves > 0);
    }
}
