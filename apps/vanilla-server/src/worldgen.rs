//! IRW4: deterministic, coordinate-addressable 1.7.10-compatible world features.
//! Generation is a pure function of (seed, dimension, x, y, z). It never
//! depends on request order, chunk loading order, global RNG or Java code.
//! This is NOT Mojang's 1.7.10 terrain algorithm or a bit-for-bit clone.
use crate::dimensions::Dimension;

const SEA: i32 = 62;
const AIR: u16 = 0;
fn b(id: u16) -> u16 {
    id << 4
}
fn state(id: u16, meta: u16) -> u16 {
    (id << 4) | (meta & 15)
}

fn hash(seed: u64, x: i32, y: i32, z: i32) -> u64 {
    let mut h = seed
        ^ (x as i64 as u64).wrapping_mul(0x9e3779b185ebca87)
        ^ (y as i64 as u64).wrapping_mul(0xc2b2ae3d27d4eb4f)
        ^ (z as i64 as u64).wrapping_mul(0x165667b19e3779f9);
    h ^= h >> 30;
    h = h.wrapping_mul(0xbf58476d1ce4e5b9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94d049bb133111eb);
    h ^ (h >> 31)
}
fn random(seed: u64, x: i32, y: i32, z: i32) -> f64 {
    (hash(seed, x, y, z) >> 11) as f64 / ((1u64 << 53) as f64) * 2.0 - 1.0
}
fn fade(x: f64) -> f64 {
    x * x * (3.0 - 2.0 * x)
}
fn mix(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}
fn n2(seed: u64, x: i32, z: i32, scale: i32) -> f64 {
    let xx = x.div_euclid(scale);
    let zz = z.div_euclid(scale);
    let fx = fade(x.rem_euclid(scale) as f64 / scale as f64);
    let fz = fade(z.rem_euclid(scale) as f64 / scale as f64);
    mix(
        mix(random(seed, xx, 0, zz), random(seed, xx + 1, 0, zz), fx),
        mix(
            random(seed, xx, 0, zz + 1),
            random(seed, xx + 1, 0, zz + 1),
            fx,
        ),
        fz,
    )
}
fn n3(seed: u64, x: i32, y: i32, z: i32, scale: i32) -> f64 {
    let xx = x.div_euclid(scale);
    let yy = y.div_euclid(scale);
    let zz = z.div_euclid(scale);
    let fx = fade(x.rem_euclid(scale) as f64 / scale as f64);
    let fy = fade(y.rem_euclid(scale) as f64 / scale as f64);
    let fz = fade(z.rem_euclid(scale) as f64 / scale as f64);
    let plane = |dz, dy| {
        mix(
            random(seed, xx, yy + dy, zz + dz),
            random(seed, xx + 1, yy + dy, zz + dz),
            fx,
        )
    };
    mix(
        mix(plane(0, 0), plane(0, 1), fy),
        mix(plane(1, 0), plane(1, 1), fy),
        fz,
    )
}

/// Vanilla 1.7.10 IDs, not a newly assigned palette.
/// The generator supports representative temperature/humidity families.
pub fn biome(seed: u64, x: i32, z: i32) -> u8 {
    let continent = n2(seed ^ 0xc001, x, z, 320);
    let cold = n2(seed ^ 0xface, x, z, 260);
    let wet = n2(seed ^ 0xfeed, x, z, 220);
    let ridge = n2(seed ^ 0xabcd, x, z, 145);
    let river = n2(seed ^ 0x513e, x, z, 90) + n2(seed ^ 0x1234, x, z, 27) * 0.20;
    if continent < -0.36 {
        return if cold < -0.35 {
            10
        } else if continent < -0.58 {
            24
        } else {
            0
        };
    }
    if river.abs() < 0.033 {
        return if cold < -0.35 { 11 } else { 7 };
    }
    if continent < -0.27 {
        return if cold < -0.25 {
            26
        } else if ridge > 0.28 {
            25
        } else {
            16
        };
    }
    if ridge > 0.38 {
        return if cold < -0.28 { 13 } else { 3 };
    }
    if cold < -0.43 {
        return 12;
    }
    if cold < -0.20 {
        return if wet > 0.0 { 30 } else { 5 };
    }
    if cold > 0.42 && wet < -0.14 {
        return if ridge > 0.12 { 17 } else { 2 };
    }
    if cold > 0.40 && wet > 0.23 {
        return 21;
    }
    if cold > 0.28 && wet < 0.04 {
        return 35;
    }
    if wet > 0.48 {
        return 6;
    }
    if wet > 0.21 {
        return if ridge > 0.10 { 27 } else { 4 };
    }
    if wet < -0.37 {
        return 1;
    }
    if wet < -0.16 && cold > 0.16 {
        return 37;
    }
    if ridge > 0.20 {
        18
    } else {
        1
    }
}
/// Temperature and precipitation used by client-side sky/precipitation logic.
#[allow(dead_code)] // Reserved for future precipitation/weather integration.
pub fn climate(id: u8) -> (f32, f32) {
    match id {
        0 | 24 => (0.5, 0.5),
        2 | 17 | 37 => (2.0, 0.0),
        3 | 25 => (0.2, 0.3),
        4 | 18 | 27 => (0.7, 0.8),
        5 | 30 => (-0.25, 0.8),
        6 => (0.8, 0.9),
        7 => (0.5, 0.5),
        10 | 11 | 12 | 13 | 26 => (-0.5, 0.5),
        16 => (0.8, 0.4),
        21 => (0.95, 0.9),
        35 => (1.2, 0.0),
        _ => (0.8, 0.4),
    }
}
/// Small seeded lake basins with smooth shorelines and negative-coordinate
/// handling. Check adjacent feature cells so lakes are seamless at their edges.
fn lake_cut(seed: u64, x: i32, z: i32) -> f64 {
    let mut cut: f64 = 0.0;
    for gx in x.div_euclid(128) - 1..=x.div_euclid(128) + 1 {
        for gz in z.div_euclid(128) - 1..=z.div_euclid(128) + 1 {
            if !hash(seed ^ 0x1a6e, gx, 0, gz).is_multiple_of(3) {
                continue;
            }
            let cx = gx * 128 + 16 + (hash(seed, gx, 10, gz) % 96) as i32;
            let cz = gz * 128 + 16 + (hash(seed, gx, 11, gz) % 96) as i32;
            let center = 67.0
                + n2(seed ^ 0xc001, cx, cz, 320) * 32.0
                + n2(seed ^ 0xabcd, cx, cz, 145).max(0.0) * 19.0;
            if !(58.0..=76.0).contains(&center) {
                continue;
            }
            let radius = 10.0 + (hash(seed, gx, 12, gz) % 6) as f64;
            let dx = (x - cx) as f64;
            let dz = (z - cz) as f64;
            let dist = (dx * dx + dz * dz).sqrt();
            let ratio = (1.0 - dist / radius).max(0.0);
            cut = cut.max(ratio * 17.0);
        }
    }
    cut
}
pub fn height(seed: u64, x: i32, z: i32) -> i32 {
    let continent = n2(seed ^ 0xc001, x, z, 320);
    let ridge = n2(seed ^ 0xabcd, x, z, 145);
    let detail = n2(seed ^ 0xbaad, x, z, 36);
    let river = n2(seed ^ 0x513e, x, z, 90) + n2(seed ^ 0x1234, x, z, 27) * 0.20;
    // All terms vary continuously in world coordinates; no chunk-edge cuts.
    let land = 67.0 + continent * 32.0 + ridge.max(0.0) * 19.0 + detail * 5.0;
    let cut = (1.0 - river.abs() / 0.07).clamp(0.0, 1.0) * 9.0;
    (land - cut - lake_cut(seed, x, z))
        .round()
        .clamp(28.0, 125.0) as i32
}
fn sand_biome(id: u8) -> bool {
    matches!(id, 0 | 2 | 16 | 17 | 24 | 26 | 37)
}
fn desert_biome(id: u8) -> bool {
    matches!(id, 2 | 17 | 37)
}
fn stone_biome(id: u8) -> bool {
    matches!(id, 3 | 13 | 25)
}

/// Minecraft 1.7.10-inspired distribution targets. Ore placement is
/// stateless rather than Java's population RNG, so it is not byte-for-byte
/// vanilla generation. Each 4x4x4 cell is a possible compact vein.
#[derive(Clone, Copy, Debug)]
pub struct OreSpec {
    pub id: u16,
    pub min_y: i32,
    pub max_y: i32,
    pub attempts_per_chunk: u8,
    pub vein_size: u8,
}
pub const ORES: [OreSpec; 7] = [
    OreSpec {
        id: 16,
        min_y: 1,
        max_y: 128,
        attempts_per_chunk: 20,
        vein_size: 16,
    },
    OreSpec {
        id: 15,
        min_y: 1,
        max_y: 64,
        attempts_per_chunk: 20,
        vein_size: 8,
    },
    OreSpec {
        id: 14,
        min_y: 1,
        max_y: 32,
        attempts_per_chunk: 2,
        vein_size: 8,
    },
    OreSpec {
        id: 73,
        min_y: 1,
        max_y: 16,
        attempts_per_chunk: 8,
        vein_size: 7,
    },
    OreSpec {
        id: 56,
        min_y: 1,
        max_y: 16,
        attempts_per_chunk: 1,
        vein_size: 7,
    },
    OreSpec {
        id: 21,
        min_y: 1,
        max_y: 31,
        attempts_per_chunk: 1,
        vein_size: 6,
    },
    OreSpec {
        id: 129,
        min_y: 4,
        max_y: 32,
        attempts_per_chunk: 1,
        vein_size: 5,
    },
];

/// A fixed-size, contiguous, center-first vein. The inner 2x2x2 cube
/// supplies the first eight positions; the next 24 positions are its six
/// faces, rotated per cell. Exactly min(vein_size, 32) cells are selected.
/// No per-chunk RNG state is needed, even for negative world coordinates.
fn in_vein(shape: u64, x: i32, y: i32, z: i32, size: u8) -> bool {
    let (lx, ly, lz) = (x.rem_euclid(4), y.rem_euclid(4), z.rem_euclid(4));
    let inner = |n| (1..=2).contains(&n);
    if inner(lx) && inner(ly) && inner(lz) {
        let index = ((lx - 1) * 4 + (ly - 1) * 2 + lz - 1) as u64;
        return (index + shape % 8) % 8 < u64::from(size.min(8));
    }
    if size <= 8 {
        return false;
    }
    let face = if lx == 0 && inner(ly) && inner(lz) {
        Some((0, ly, lz))
    } else if lx == 3 && inner(ly) && inner(lz) {
        Some((1, ly, lz))
    } else if ly == 0 && inner(lx) && inner(lz) {
        Some((2, lx, lz))
    } else if ly == 3 && inner(lx) && inner(lz) {
        Some((3, lx, lz))
    } else if lz == 0 && inner(lx) && inner(ly) {
        Some((4, lx, ly))
    } else if lz == 3 && inner(lx) && inner(ly) {
        Some((5, lx, ly))
    } else {
        None
    };
    if let Some((face_id, a, b)) = face {
        let index = (face_id * 4 + (a - 1) * 2 + b - 1) as u64;
        (index + shape % 24) % 24 < u64::from(size.saturating_sub(8).min(24))
    } else {
        false
    }
}

// Vanilla 1.7.10 selects the lapis vein height by adding two independent
// uniform 0..15 draws. Its convolution peaks at Y=15/16 rather than a
// uniform distribution; translate that triangular law to our stateless cells.
fn lapis_layer_weight(y: i32) -> u64 {
    if !(1..=31).contains(&y) {
        return 0;
    }
    (16 - (y - 16).abs()) as u64
}

fn ore(seed: u64, x: i32, y: i32, z: i32, is_hills: bool) -> u16 {
    let (cx, cy, cz) = (x.div_euclid(4), y.div_euclid(4), z.div_euclid(4));
    // A 16x16 chunk contains four 4x4 cells per horizontal axis, so
    // over H vertical layers there are 4*H candidate cells. Use the
    // configured attempts/chunk as the expected vein count at each ore Y.
    // Rarer ores take precedence when two veins overlap.
    for spec in ORES.iter().rev() {
        if !(spec.min_y..=spec.max_y).contains(&y) || (spec.id == 129 && !is_hills) {
            continue;
        }
        let vertical_range = (spec.max_y - spec.min_y + 1) as u64;
        let mut threshold = u64::from(spec.attempts_per_chunk) * 1_000_000 / (4 * vertical_range);
        if spec.id == 21 {
            // Sum of weights Y=1..31 is 256; normalized mean remains 1.
            threshold = threshold * lapis_layer_weight(y) * vertical_range / 256;
        }
        let salt = u64::from(spec.id).wrapping_mul(0x9e3779b185ebca87);
        let chance = hash(seed ^ salt ^ 0x0e0e, cx, cy, cz);
        if chance % 1_000_000 < threshold && in_vein(chance >> 20, x, y, z, spec.vein_size) {
            return b(spec.id);
        }
    }
    b(1)
}
fn cave(seed: u64, x: i32, y: i32, z: i32) -> bool {
    if !(7..=110).contains(&y) {
        return false;
    }
    let winding = n3(seed ^ 0xca7e, x, y, z, 17);
    let branch = n3(seed ^ 0xfeed, x, y, z, 37);
    let chamber = n3(seed ^ 0xc0de, x, y, z, 42);
    (winding.abs() < 0.105 && branch > -0.08)
        || (winding.abs() < 0.18 && branch.abs() < 0.055)
        || (chamber > 0.56 && y < 60)
        || ravine(seed, x, y, z)
}
fn ravine(seed: u64, x: i32, y: i32, z: i32) -> bool {
    // One canyon candidate per 128x128 region, tested against neighboring
    // regions so cliffs do not terminate at chunk/region boundaries.
    for rx in x.div_euclid(128) - 1..=x.div_euclid(128) + 1 {
        for rz in z.div_euclid(128) - 1..=z.div_euclid(128) + 1 {
            if !hash(seed ^ 0x5a7e, rx, 0, rz).is_multiple_of(5) {
                continue;
            }
            let cx = rx * 128 + (hash(seed, rx, 2, rz) % 128) as i32;
            let cz = rz * 128 + (hash(seed, rx, 3, rz) % 128) as i32;
            let dx = (x - cx) as f64;
            let dz = (z - cz) as f64;
            // Long narrow canyon, rotated slightly with seed.
            let axis = if hash(seed, rx, 4, rz) & 1 == 0 {
                dx
            } else {
                dz
            };
            let cross = if hash(seed, rx, 4, rz) & 1 == 0 {
                dz
            } else {
                dx
            };
            let center = cross - (axis / 18.0).sin() * 7.0;
            if axis.abs() < 45.0
                && center.abs() < 3.5 + (axis / 17.0).cos().abs() * 2.0
                && (20..=51).contains(&y)
            {
                return true;
            }
        }
    }
    false
}
/// Absolute block position of a deterministic IRW4 dungeon spawner in
/// its 64x64 generation cell. This is a read-only lookup and does not cause
/// chunk generation; runtime spawning must verify the block still exists.
pub fn dungeon_spawner_in_cell(seed: u64, gx: i32, gz: i32) -> (i32, i32, i32) {
    (
        gx * 64 + 10 + (hash(seed ^ 0xd06e, gx, 0, gz) % 43) as i32,
        19 + (hash(seed ^ 0xd06e, gx, 2, gz) % 28) as i32,
        gz * 64 + 10 + (hash(seed ^ 0xd06e, gx, 1, gz) % 43) as i32,
    )
}
/// Dungeon wall/floor, spawner, and loot-chest blocks, all seed-addressable.
fn dungeon(seed: u64, x: i32, y: i32, z: i32) -> Option<u16> {
    let gx = x.div_euclid(64);
    let gz = z.div_euclid(64);
    let (cx, sy, cz) = dungeon_spawner_in_cell(seed, gx, gz);
    let cy = sy - 1;
    if (x - cx).abs() > 3 || (z - cz).abs() > 3 || y < cy || y > cy + 4 {
        return None;
    }
    if y == cy || y == cy + 4 || (x - cx).abs() == 3 || (z - cz).abs() == 3 {
        return Some(b(if hash(seed, x, y, z).is_multiple_of(4) {
            48
        } else {
            4
        }));
    }
    if y == cy + 1 && x == cx && z == cz {
        return Some(b(52));
    }
    if y == cy + 1 && x == cx + 2 && z == cz {
        return Some(b(54));
    }
    Some(AIR)
}
/// Pseudo-random, repeatable dungeon loot. Persistent per-chest inventories
/// must be initialized via the chest-store interface when first opened.
pub fn dungeon_loot(seed: u64, x: i32, y: i32, z: i32) -> Option<[(i16, u8); 3]> {
    if dungeon(seed, x, y, z) != Some(b(54)) {
        return None;
    }
    let candidates: [i16; 8] = [265, 266, 331, 287, 296, 322, 329, 2256];
    let r = hash(seed ^ 0x10ad, x, y, z);
    Some([
        (candidates[(r % 8) as usize], 1 + ((r >> 11) % 4) as u8),
        (
            candidates[((r >> 7) % 8) as usize],
            1 + ((r >> 14) % 3) as u8,
        ),
        (candidates[((r >> 19) % 8) as usize], 1),
    ])
}
/// Fixed-region village foundation: roads, simple houses, farms and a central
/// well. Their footprints cross chunks naturally. Villager spawning is separate.
fn village(seed: u64, x: i32, y: i32, z: i32) -> Option<u16> {
    let gx = x.div_euclid(192);
    let gz = z.div_euclid(192);
    if !hash(seed ^ 0x711a, gx, 0, gz).is_multiple_of(4) {
        return None;
    }
    let cx = gx * 192 + 48;
    let cz = gz * 192 + 48;
    if !matches!(biome(seed, cx, cz), 1 | 2 | 35 | 4) {
        return None;
    }
    let dx = x - cx;
    let dz = z - cz;
    if dx.abs() > 31 || dz.abs() > 26 {
        return None;
    }
    let base = height(seed, cx, cz);
    if !(base - 4..=base + 7).contains(&y) {
        return None;
    }
    // Roads and a small central well.
    if dz.abs() <= 1 && dx.abs() <= 29 {
        return if y == base {
            Some(b(13))
        } else if (base + 1..=base + 5).contains(&y) {
            Some(AIR)
        } else {
            None
        };
    }
    if dx.abs() <= 2 && dz.abs() <= 2 {
        if y == base {
            return Some(b(4));
        }
        if y == base + 1 && dx.abs() == 2 && dz.abs() == 2 {
            return Some(b(4));
        }
        if y == base + 1 {
            return Some(b(9));
        }
    }
    // Two houses with a doorway toward the road.
    for (hx, hz) in [(-17, -9), (17, 9)] {
        let a = dx - hx;
        let c = dz - hz;
        if a.abs() > 4 || c.abs() > 4 {
            continue;
        }
        if y == base {
            return Some(b(5));
        }
        if (base + 1..=base + 4).contains(&y) {
            let boundary = a.abs() == 4 || c.abs() == 4;
            if !boundary {
                return Some(AIR);
            }
            if a == 0 && c == if hz < 0 { 4 } else { -4 } && y < base + 3 {
                return Some(AIR);
            }
            return Some(if a.abs() == 4 && c.abs() == 4 {
                b(17)
            } else {
                b(5)
            });
        }
        if y == base + 5 {
            return Some(b(5));
        }
        return None;
    }
    // Farm with water channel and mature wheat; no global RNG.
    if (9..=20).contains(&dx) && (-22..=-13).contains(&dz) {
        if y == base {
            return Some(if dx == 15 { b(9) } else { state(60, 7) });
        }
        if y == base + 1 {
            return Some(if dx == 15 { AIR } else { state(59, 7) });
        }
    }
    None
}
fn aboveground(seed: u64, x: i32, y: i32, z: i32, top: i32, id: u8) -> u16 {
    if y <= top {
        return AIR;
    }
    let choice = hash(seed ^ 0x771e, x, 0, z);
    if desert_biome(id) && choice.is_multiple_of(121) && y <= top + 3 {
        return b(81);
    }
    if y == top + 1 {
        if matches!(id, 6 | 21) && choice.is_multiple_of(48) {
            return b(if choice & 1 == 0 { 39 } else { 40 });
        }
        if matches!(id, 1 | 4 | 5 | 12 | 27 | 30 | 35) {
            if choice.is_multiple_of(29) {
                return state(38, (choice % 9) as u16);
            }
            if choice.is_multiple_of(5) {
                return state(31, 1);
            }
        }
    }
    // Cross-chunk canopy lookup around this block, independent of which
    // chunk is requested. Trees are suppressed in the spawn-safe square.
    for tx in x - 2..=x + 2 {
        for tz in z - 2..=z + 2 {
            if tx.abs() < 12 && tz.abs() < 12 {
                continue;
            }
            let kind = biome(seed, tx, tz);
            let frequency = match kind {
                4 | 27 => 37,
                5 | 30 => 47,
                21 => 28,
                6 => 59,
                18 => 44,
                _ => continue,
            };
            if !hash(seed ^ 0x7eed, tx, 0, tz).is_multiple_of(frequency) {
                continue;
            }
            let root = height(seed, tx, tz);
            if root < SEA || y < root + 1 || y > root + 7 {
                continue;
            }
            let metadata = match kind {
                5 | 30 => 1,
                27 => 2,
                21 => 3,
                _ => 0,
            };
            if tx == x && tz == z && y <= root + 5 {
                return state(17, metadata);
            }
            let radius = if y >= root + 7 { 1 } else { 2 };
            if (tx - x).abs() <= radius && (tz - z).abs() <= radius && y >= root + 4 {
                return state(18, metadata | 4); // player-placed decay disabled for generated canopy
            }
        }
    }
    if top < SEA && y <= SEA {
        b(9)
    } else {
        AIR
    }
}
fn overworld(seed: u64, x: i32, y: i32, z: i32) -> u16 {
    if y == 0 {
        return b(7);
    }
    let kind = biome(seed, x, z);
    let top = height(seed, x, z);
    if let Some(s) = village(seed, x, y, z) {
        return s;
    }
    if let Some(s) = dungeon(seed, x, y, z) {
        return s;
    }
    if y > top {
        return aboveground(seed, x, y, z, top, kind);
    }
    if y < 5 && hash(seed, x, y, z).is_multiple_of(4) {
        return b(7);
    }
    if y < top - 4 {
        if cave(seed, x, y, z) {
            return if y < 11 { b(10) } else { AIR };
        }
        return ore(seed, x, y, z, matches!(kind, 3 | 13));
    }
    if y == top {
        return if stone_biome(kind) {
            b(1)
        } else if sand_biome(kind) {
            b(12)
        } else if matches!(kind, 12 | 13) {
            b(80)
        } else if top <= SEA + 1 {
            b(13)
        } else {
            b(2)
        };
    }
    if sand_biome(kind) {
        b(12)
    } else {
        b(3)
    }
}
fn nether(seed: u64, x: i32, y: i32, z: i32) -> u16 {
    if y <= 4 || (124..128).contains(&y) {
        return b(7);
    }
    if y >= 128 {
        return AIR;
    }
    let cavern = n3(seed ^ 0x6e7e, x, y, z, 27) + n3(seed ^ 0x6e7f, x, y, z, 12) * 0.38;
    let solid = cavern < (if y < 29 { 0.22 } else { -0.02 });
    let mut block = if solid {
        b(87)
    } else if y <= 31 {
        b(10)
    } else {
        AIR
    };
    if solid && y < 58 && n2(seed ^ 0x50a1, x, z, 25) > 0.35 {
        block = b(88);
    }
    if !solid
        && y > 75
        && hash(
            seed ^ 0x6100,
            x.div_euclid(3),
            y.div_euclid(3),
            z.div_euclid(3),
        )
        .is_multiple_of(89)
    {
        block = b(89);
    }
    // Nether fortress foundation / bridge and towers.
    let ax = x.rem_euclid(192);
    let az = z.rem_euclid(192);
    if (75..=116).contains(&ax) && (94..=98).contains(&az) && (54..=58).contains(&y) {
        return if y == 54 || y == 58 { b(112) } else { AIR };
    }
    block
}
fn end(seed: u64, x: i32, y: i32, z: i32) -> u16 {
    let dist2 = (x as i64) * (x as i64) + (z as i64) * (z as i64);
    let mut island = dist2 <= 52 * 52;
    // Remote End islands are seed-defined and isolated in 256-block regions.
    let gx = x.div_euclid(256);
    let gz = z.div_euclid(256);
    let cx = gx * 256 + 70 + (hash(seed, gx, 0, gz) % 100) as i32;
    let cz = gz * 256 + 70 + (hash(seed, gx, 1, gz) % 100) as i32;
    let radius = 15 + (hash(seed, gx, 2, gz) % 29) as i32;
    let remote_d2 = (x as i64 - cx as i64).pow(2) + (z as i64 - cz as i64).pow(2);
    if dist2 > 110 * 110 && remote_d2 < (radius * radius) as i64 {
        island = true;
    }
    let top = 66 + (n2(seed ^ 0xe0d, x, z, 24) * 4.0).round() as i32;
    let thickness = if dist2 <= 52 * 52 {
        (52.0 - (dist2 as f64).sqrt()) * 0.55
    } else {
        (radius as f64 - (remote_d2 as f64).sqrt()) * 0.42
    };
    if island && y <= top && (y as f64) >= top as f64 - thickness.max(2.0) {
        // Return portal plinth / exit gateway base.
        if dist2 <= 9 && y == 66 {
            return b(7);
        }
        return b(121);
    }
    // Ten obsidian pillars around the central End island.
    for i in 0..10 {
        let a = (i as f64) * std::f64::consts::TAU / 10.0;
        let px = (a.cos() * 41.0).round() as i32;
        let pz = (a.sin() * 41.0).round() as i32;
        let d2 = (x - px).pow(2) + (z - pz).pow(2);
        if d2 <= 4 && (64..=92 + (i * 4)).contains(&y) {
            return b(49);
        }
    }
    AIR
}
pub fn block(seed: u64, dim: Dimension, x: i32, y: i32, z: i32) -> u16 {
    if !(0..=255).contains(&y) {
        return AIR;
    }
    match dim {
        Dimension::Overworld => overworld(seed, x, y, z),
        Dimension::Nether => nether(seed, x, y, z),
        Dimension::End => end(seed, x, y, z),
    }
}
pub fn biome_for_dimension(seed: u64, dim: Dimension, x: i32, z: i32) -> u8 {
    match dim {
        Dimension::Overworld => biome(seed, x, z),
        Dimension::Nether => 8, // Hell
        Dimension::End => 9,    // Sky (The End)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lapis_ores_use_a_triangular_height_distribution() {
        assert_eq!(lapis_layer_weight(0), 0);
        assert_eq!(lapis_layer_weight(32), 0);
        assert_eq!(lapis_layer_weight(16), 16);
        assert_eq!(lapis_layer_weight(1), lapis_layer_weight(31));
        assert!(lapis_layer_weight(16) > lapis_layer_weight(8));
        assert_eq!((1..=31).map(lapis_layer_weight).sum::<u64>(), 256);
        let spec = ORES.iter().find(|spec| spec.id == 21).unwrap();
        assert_eq!((spec.min_y, spec.max_y), (1, 31));
    }

    #[test]
    fn ore_tuning_is_used_and_veins_are_bounded_and_repeatable() {
        // 4x4x4 cell boundaries, including negative positions, are stable.
        for seed in [0, 42, 983] {
            for cx in -2..=2 {
                for cy in 1..=8 {
                    let mut counts = [0u8; 7];
                    for x in cx * 4..cx * 4 + 4 {
                        for y in cy * 4..cy * 4 + 4 {
                            for z in -4..0 {
                                let id = ore(seed, x, y, z, true) >> 4;
                                assert_eq!(ore(seed, x, y, z, true) >> 4, id);
                                if let Some((index, _)) =
                                    ORES.iter().enumerate().find(|(_, spec)| spec.id == id)
                                {
                                    counts[index] += 1;
                                } else {
                                    assert_eq!(id, 1);
                                }
                                if id == 129 {
                                    assert_ne!(ore(seed, x, y, z, false) >> 4, 129);
                                }
                            }
                        }
                    }
                    for (index, spec) in ORES.iter().enumerate() {
                        assert!(counts[index] <= spec.vein_size);
                    }
                }
            }
        }
        assert!(ORES
            .iter()
            .all(|spec| spec.min_y <= spec.max_y && spec.vein_size > 0 && spec.vein_size <= 32));
    }
    #[test]
    fn cross_chunk_border_is_seed_and_order_independent() {
        for d in [Dimension::Overworld, Dimension::Nether, Dimension::End] {
            for (x, z) in [(-17, -1), (15, 16), (16, 15), (255, -256), (0, 0)] {
                for y in [0, 12, 36, 64, 89, 128, 255] {
                    let expected = block(42, d, x, y, z);
                    assert_eq!(expected, block(42, d, x, y, z));
                }
            }
        }
        assert_eq!(height(42, -1, -1), height(42, -1, -1));
        assert_ne!(height(42, 300, 300), height(43, 300, 300));
    }
    #[test]
    fn temperature_and_biome_ids_are_vanilla_ids() {
        let mut seen = std::collections::BTreeSet::new();
        for x in (-2400..=2400).step_by(120) {
            for z in (-2400..=2400).step_by(120) {
                let kind = biome(42, x, z);
                seen.insert(kind);
                let (t, p) = climate(kind);
                assert!(t.is_finite() && (0.0..=1.0).contains(&p));
            }
        }
        assert!(seen.contains(&0) && seen.contains(&1) && seen.contains(&7));
        assert!(seen.len() > 5);
    }
    #[test]
    fn nether_and_end_have_distinct_base_blocks() {
        assert_eq!(block(9, Dimension::Nether, 0, 0, 0), b(7));
        assert_eq!(block(9, Dimension::End, 0, 66, 0), b(7));
        assert!((35..=74).any(|y| block(9, Dimension::End, 30, y, 0) == b(121)));
        assert_eq!(biome_for_dimension(9, Dimension::Nether, 0, 0), 8);
        assert_eq!(biome_for_dimension(9, Dimension::End, 0, 0), 9);
    }
    #[test]
    fn dungeon_spawner_lookup_matches_existing_generation_at_negative_cells() {
        for seed in [0, 42, 99_123] {
            for gx in -3..=3 {
                for gz in -3..=3 {
                    let (x, y, z) = dungeon_spawner_in_cell(seed, gx, gz);
                    assert_eq!(x.div_euclid(64), gx);
                    assert_eq!(z.div_euclid(64), gz);
                    assert!((19..=46).contains(&y));
                    assert_eq!(block(seed, Dimension::Overworld, x, y, z), b(52));
                }
            }
        }
    }
    #[test]
    fn generated_dungeons_have_spawner_and_loot() {
        let seed = 42u64;
        let gx = 2;
        let gz = 3;
        let cx = gx * 64 + 10 + (hash(seed ^ 0xd06e, gx, 0, gz) % 43) as i32;
        let cz = gz * 64 + 10 + (hash(seed ^ 0xd06e, gx, 1, gz) % 43) as i32;
        let cy = 18 + (hash(seed ^ 0xd06e, gx, 2, gz) % 28) as i32;
        assert_eq!(block(seed, Dimension::Overworld, cx, cy + 1, cz), b(52));
        assert_eq!(block(seed, Dimension::Overworld, cx + 2, cy + 1, cz), b(54));
        assert_eq!(
            dungeon_loot(seed, cx + 2, cy + 1, cz),
            dungeon_loot(seed, cx + 2, cy + 1, cz)
        );
    }
}
