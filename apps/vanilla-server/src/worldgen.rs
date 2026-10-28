//! IRW4: deterministic, coordinate-addressable 1.7.10-compatible world features.
//! Generation is a pure function of (seed, dimension, x, y, z). It never
//! depends on request order, chunk loading order, global RNG or Java code.
//! This is NOT Mojang's 1.7.10 terrain algorithm or a bit-for-bit clone.
use crate::dimensions::Dimension;

// A01: version-gated Java GenLayer RNG foundation. IRW4 generation remains unchanged.
#[allow(dead_code)]
mod genlayer;

#[path = "worldgen_trees.rs"]
mod trees;

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
        // 1.7.10 giant-tree taiga, alongside ordinary/cold taiga.
        return if wet > 0.32 { 32 } else if wet > 0.0 { 30 } else { 5 };
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
        // Roofed Forest (29) is required for naturally generated dark oak.
        return if wet > 0.37 && ridge < 0.0 { 29 }
        else if ridge > 0.10 { 27 } else { 4 };
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
/// Fixed legacy 1.7.10 biome temperature / rainfall values for the IRW4
/// generator's supported biome IDs. This is not the full GenLayer pipeline.
/// In particular, ordinary Taiga is a freezing biome, while Cold Taiga is
/// colder; their previous shared value also misclassified snowfall.
#[allow(dead_code)] // Reserved for future precipitation/weather integration.
pub fn climate(id: u8) -> (f32, f32) {
    match id {
        0 | 24 => (0.5, 0.5),
        2 | 17 | 37 => (2.0, 0.0),
        3 | 25 => (0.2, 0.3),
        4 | 18 => (0.7, 0.8),
        5 => (0.05, 0.8),
        6 => (0.8, 0.9),
        7 => (0.5, 0.5),
        10..=13 => (0.0, 0.5),
        16 => (0.8, 0.4),
        21 => (1.2, 0.9),
        26 => (0.05, 0.3),
        27 => (0.6, 0.6),
        29 => (0.7, 0.8),
        30 => (-0.5, 0.4),
        32 => (0.3, 0.8),
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
        min_y: 0,
        max_y: 30,
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
// uniform distribution; translate its Y=15 peak to our stateless cells.
fn lapis_layer_weight(y: i32) -> u64 {
    if !(0..=30).contains(&y) {
        return 0;
    }
    (16 - (y - 15).abs()) as u64
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
/// Return the deterministic center and ground level of the village whose
/// structure footprint contains (x, z). Villager AI and door detection can
/// reuse this lookup instead of guessing the center from nearby buildings.
/// This describes generated village geometry, not a live villager population.
pub fn village_center(seed: u64, x: i32, z: i32) -> Option<(i32, i32, i32)> {
    let gx = x.div_euclid(192);
    let gz = z.div_euclid(192);
    // The leftmost generation cell cannot be represented as a center in
    // i32 coordinates. Reject it instead of overflowing in debug builds.
    let cx = gx.checked_mul(192)?.checked_add(48)?;
    let cz = gz.checked_mul(192)?.checked_add(48)?;
    if !hash(seed ^ 0x711a, gx, 0, gz).is_multiple_of(4) {
        return None;
    }
    if !matches!(biome(seed, cx, cz), 1 | 2 | 35 | 4) {
        return None;
    }
    let dx = x - cx;
    let dz = z - cz;
    if dx.abs() > 31 || dz.abs() > 26 {
        return None;
    }
    Some((cx, height(seed, cx, cz), cz))
}
/// Lower-half block coordinates of both doors in a generated village.
///
/// This describes the static house layout only: callers must query live world
/// blocks before treating a door as present, open, or usable by villagers.
/// Returns None outside a generated village footprint.
pub fn village_house_doors(seed: u64, x: i32, z: i32) -> Option<[[i32; 3]; 2]> {
    let (cx, base, cz) = village_center(seed, x, z)?;
    let y = base.checked_add(1)?;
    Some([
        [cx.checked_sub(17)?, y, cz.checked_sub(5)?],
        [cx.checked_add(17)?, y, cz.checked_add(5)?],
    ])
}
/// Return the generated house doors that still exist in the live world.
/// The block lookup must read current authoritative world state, including
/// player edits, rather than the static world generator. A complete wooden
/// door needs a lower half (metadata bit 3 clear) and an upper half (bit 3 set).
/// The remaining metadata bits may change when the door opens or its hinge
/// changes. Missing or replaced halves are excluded independently.
/// This is a geometry/validation helper; villager AI integration is pending.
#[allow(dead_code)] // Live world callers are not yet wired to villager AI.
pub fn village_live_house_doors(
    seed: u64,
    x: i32,
    z: i32,
    mut block_at: impl FnMut(i32, i32, i32) -> u16,
) -> Option<[Option<[i32; 3]>; 2]> {
    let doors = village_house_doors(seed, x, z)?;
    Some(doors.map(|[dx, dy, dz]| {
        let upper_y = dy.checked_add(1)?;
        let lower = block_at(dx, dy, dz);
        let upper = block_at(dx, upper_y, dz);
        ((lower >> 4 == 64) && (lower & 8 == 0) && (upper >> 4 == 64) && (upper & 8 != 0))
            .then_some([dx, dy, dz])
    }))
}
/// Fixed-region village foundation: roads, simple houses, farms and a central
/// well. Their footprints cross chunks naturally. Villager spawning is separate.
fn village(seed: u64, x: i32, y: i32, z: i32) -> Option<u16> {
    let (cx, base, cz) = village_center(seed, x, z)?;
    let dx = x - cx;
    let dz = z - cz;
    if !(base - 4..=base + 7).contains(&y) {
        return None;
    }
    // Fill shallow gaps below village structures when the local terrain is
    // lower than the village datum. Without this, gravel paths, house floors,
    // and farmland can float above uneven terrain or shallow cave mouths.
    // Do not overwrite naturally existing terrain, and keep the support
    // bounded to the four blocks below the structure.
    if y < base {
        let road = dz.abs() <= 1 && dx.abs() <= 29;
        // The well extends two blocks beyond the road on both axes. Its
        // corner floor blocks also need support over shallow terrain gaps.
        let well = dx.abs() <= 2 && dz.abs() <= 2;
        let path = (dx == -17 && (-4..=-2).contains(&dz)) || (dx == 17 && (2..=4).contains(&dz));
        let house = [(-17, -9), (17, 9)]
            .iter()
            .any(|&(hx, hz)| (dx - hx).abs() <= 4 && (dz - hz).abs() <= 4);
        let farm = (9..=20).contains(&dx) && (-22..=-13).contains(&dz);
        if (road || well || path || house || farm) && y > height(seed, x, z) {
            return Some(if farm && !matches!(dx, 11 | 15 | 19) {
                b(3)
            } else {
                b(4)
            });
        }
    }
    // The central well must take precedence over the crossing road.
    // Otherwise the road replaces the cobblestone floor with gravel and
    // clears the water at z=-1..=1, leaving a dry, broken well.
    if dx.abs() <= 2 && dz.abs() <= 2 {
        if y == base {
            return Some(b(4));
        }
        if y == base + 1 {
            // A solid cobblestone rim keeps the 3x3 well water contained.
            // Leaving only the four corners solid allows water to flow
            // into the surrounding village road when chunks are ticked.
            return Some(if dx.abs() == 2 || dz.abs() == 2 {
                b(4)
            } else {
                b(9)
            });
        }
        // Clear the entire well footprint above the rim, including its
        // corners outside the road. Nearby hills must not seal the water
        // under dirt or leave the well inaccessible from above.
        if (base + 2..=base + 5).contains(&y) {
            return Some(AIR);
        }
    }
    // Keep the remaining road open for player movement.
    if dz.abs() <= 1 && dx.abs() <= 29 {
        return if y == base {
            Some(b(13))
        } else if (base + 1..=base + 5).contains(&y) {
            Some(AIR)
        } else {
            None
        };
    }
    // Connect each house doorway to the central east-west road. Without
    // these short paths, local terrain can leave the doors inaccessible.
    // Roads have priority above; the paths stop before the house walls.
    if (dx == -17 && (-4..=-2).contains(&dz)) || (dx == 17 && (2..=4).contains(&dz)) {
        return if y == base {
            Some(b(13))
        } else if (base + 1..=base + 4).contains(&y) {
            Some(AIR)
        } else {
            None
        };
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
                // Standing torches on the plank floor provide fixed interior
                // fixtures without obstructing the central route to the door.
                // Metadata 5 is the floor-mounted torch in Protocol 5.
                if y == base + 1 && c == 0 && a.abs() == 2 {
                    return Some(state(50, 5));
                }
                return Some(AIR);
            }
            if a == 0
                && y < base + 3
                && village_house_doors(seed, x, z)
                    .is_some_and(|doors| doors.iter().any(|door| door[0] == x && door[2] == z))
            {
                // Wooden doors use a lower half with facing metadata and an
                // upper half marked with bit 3 in Minecraft 1.7.10.
                return Some(if y == base + 1 {
                    state(64, if hz < 0 { 0 } else { 2 })
                } else {
                    state(64, 8)
                });
            }
            // Glazed windows on both side walls and the rear wall. Keep
            // corner logs, the doorway and the roof unchanged.
            if y == base + 3 && ((a.abs() == 4 && c == 0) || (c.abs() == 4 && a.abs() == 2)) {
                return Some(b(20));
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
        // Keep the roof accessible above surrounding hills. Without this
        // clearance, natural terrain and generated foliage can bury a house
        // even when its walls, door, and roof were placed correctly.
        if (base + 6..=base + 7).contains(&y) {
            return Some(AIR);
        }
        return None;
    }
    // Farm with three irrigation channels: every farmland row is within
    // four blocks of source water, as required for 1.7.10 hydration.
    // No global RNG is used.
    if (9..=20).contains(&dx) && (-22..=-13).contains(&dz) {
        if y == base {
            // Cap the irrigation trench at both ends so source water cannot
            // spill into surrounding terrain when fluid updates run.
            return Some(if matches!(dx, 11 | 15 | 19) {
                if dz == -22 || dz == -13 {
                    b(4)
                } else {
                    b(9)
                }
            } else {
                state(60, 7)
            });
        }
        if y == base + 1 {
            return Some(if matches!(dx, 11 | 15 | 19) {
                AIR
            } else {
                state(59, 7)
            });
        }
        // Clear natural hillside blocks above the wheat and irrigation
        // channels. Otherwise a farm placed below the nearby terrain
        // datum can grow wheat inside dirt or stone.
        if (base + 2..=base + 3).contains(&y) {
            return Some(AIR);
        }
    }
    None
}
/// Add *only* missing starter trees to IRW4 plains and savannas. Existing
/// forest/taiga/jungle tree positions and palettes are left untouched. The
/// extra canopy is considered after the original generator's trees so older
/// trunks and leaves always win at overlapping coordinates.
///
/// IRW4 is coordinate-addressable, not stored chunk-by-chunk: these additions
/// also appear in old, unedited areas after upgrading. Player-edited blocks
/// still override this base terrain; back up worlds before upgrading.
fn supplementary_tree(seed: u64, x: i32, z: i32) -> Option<(u16, u16)> {
    if x.abs() < 12 && z.abs() < 12 {
        return None; // keep the established safe spawn square
    }
    // Most nearby coordinates are not candidates. Hash before calling
    // expensive biome/noise functions in the 5x5 canopy lookup.
    let roll = hash(seed ^ 0x5ce3_77ea, x, 0, z);
    if !roll.is_multiple_of(144) && !roll.is_multiple_of(80) {
        return None;
    }
    let kind = biome(seed, x, z);
    let chance = match kind {
        1 => 144, // plains: scattered oak, never dense forest
        35 => 80, // savanna: scattered acacia
        _ => return None,
    };
    if !roll.is_multiple_of(chance) || height(seed, x, z) < SEA + 2 {
        return None;
    }
    // Never replace a root with the original tall-grass/flower selection.
    // This retains the old flora-before-tree priority and avoids floating logs.
    let grass_roll = hash(seed ^ 0x771e, x, 0, z);
    if grass_roll.is_multiple_of(29) || grass_roll.is_multiple_of(5) {
        return None;
    }
    if kind == 35 {
        Some((162, 161)) // 1.7.10 acacia log and leaves
    } else {
        Some((17, 18)) // oak
    }
}

fn aboveground(seed: u64, x: i32, y: i32, z: i32, top: i32, id: u8) -> u16 {
    if y <= top {
        return AIR;
    }
    let choice = hash(seed ^ 0x771e, x, 0, z);
    if desert_biome(id) && choice.is_multiple_of(121) && y <= top + 3 {
        return b(81);
    }
    // The same tree candidate order is used for point and cached reads.
    if let Some(tree) = trees::at(seed, x, y, z) {
        return tree;
    }
    if y == top + 1 {
        if matches!(id, 6 | 21) && choice.is_multiple_of(48) {
            return b(if choice & 1 == 0 { 39 } else { 40 });
        }
        if matches!(id, 1 | 4 | 27 | 35) {
            if choice.is_multiple_of(29) {
                return state(38, (choice % 9) as u16);
            }
            if choice.is_multiple_of(5) {
                return state(31, 1);
            }
        }
    }
    if top < SEA && y <= SEA {
        // Freeze the exposed water surface, not every water source below it.
        // Keeping lower levels as water preserves liquid behavior and caves.
        if matches!(id, 10 | 11) && y == SEA {
            b(79) // ice
        } else {
            b(9) // still water
        }
    } else if y == top + 1 && top >= SEA && matches!(id, 5 | 10 | 11 | 12 | 13 | 26 | 30) {
        // Snow is a separate layer above exposed ground, except where a
        // generated trunk/canopy already occupied the coordinate.
        b(78)
    } else {
        AIR
    }
}
/// Reuse column-invariant IRW4 terrain data while building one streamed chunk.
/// The ordinary point query stays stateless; this short-lived view avoids
/// recalculating biome, height and neighboring tree candidates for every Y.
pub(crate) struct OverworldColumn {
    seed: u64,
    x: i32,
    z: i32,
    kind: u8,
    top: i32,
    canopy: [Option<u16>; 256],
}

impl OverworldColumn {
    pub(crate) fn new(seed: u64, x: i32, z: i32) -> Self {
        let kind = biome(seed, x, z);
        let top = height(seed, x, z);
        let canopy = trees::column(seed, x, z);
        Self {
            seed,
            x,
            z,
            kind,
            top,
            canopy,
        }
    }

    pub(crate) fn block_at(&self, y: i32) -> u16 {
        if !(0..=255).contains(&y) {
            return AIR;
        }
        if y == 0 {
            return b(7);
        }
        if let Some(block) = village(self.seed, self.x, y, self.z) {
            return block;
        }
        if let Some(block) = dungeon(self.seed, self.x, y, self.z) {
            return block;
        }
        if y > self.top {
            return self.aboveground(y);
        }
        if y < 5 && hash(self.seed, self.x, y, self.z).is_multiple_of(4) {
            return b(7);
        }
        if y < self.top - 4 {
            if cave(self.seed, self.x, y, self.z) {
                return if y < 11 { b(10) } else { AIR };
            }
            return ore(self.seed, self.x, y, self.z, matches!(self.kind, 3 | 13));
        }
        if y == self.top {
            return if stone_biome(self.kind) {
                b(1)
            } else if sand_biome(self.kind) {
                b(12)
            } else if matches!(self.kind, 12 | 13) {
                b(80)
            } else if self.top <= SEA + 1 {
                b(13)
            } else {
                b(2)
            };
        }
        if sand_biome(self.kind) {
            b(12)
        } else {
            b(3)
        }
    }

    fn aboveground(&self, y: i32) -> u16 {
        let choice = hash(self.seed ^ 0x771e, self.x, 0, self.z);
        if desert_biome(self.kind) && choice.is_multiple_of(121) && y <= self.top + 3 {
            return b(81);
        }
        if let Some(block) = self.canopy[y as usize] {
            return block;
        }
        if y == self.top + 1 {
            if matches!(self.kind, 6 | 21) && choice.is_multiple_of(48) {
                return b(if choice & 1 == 0 { 39 } else { 40 });
            }
            if matches!(self.kind, 1 | 4 | 27 | 35) {
                if choice.is_multiple_of(29) {
                    return state(38, (choice % 9) as u16);
                }
                if choice.is_multiple_of(5) {
                    return state(31, 1);
                }
            }
        }
        if self.top < SEA && y <= SEA {
            return if matches!(self.kind, 10 | 11) && y == SEA {
                b(79)
            } else {
                b(9)
            };
        }
        if y == self.top + 1
            && self.top >= SEA
            && matches!(self.kind, 5 | 10 | 11 | 12 | 13 | 26 | 30)
        {
            return b(78);
        }
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
    fn sparse_plains_and_savanna_trees_supply_early_survival_wood() {
        let seed = 42;
        let mut oak = 0;
        let mut acacia = 0;
        for x in 16..160 {
            for z in 16..160 {
                let Some((log, leaves)) = supplementary_tree(seed, x, z) else {
                    continue;
                };
                let root = height(seed, x, z);
                // Villages and pre-existing forest canopy take precedence;
                // count only unobstructed starter trees in this sample.
                if block(seed, Dimension::Overworld, x, root + 1, z) >> 4 != log {
                    continue;
                }
                let column = OverworldColumn::new(seed, x, z);
                assert_eq!(column.block_at(root + 1) >> 4, log);
                assert_eq!(
                    column.block_at(root + 6),
                    block(seed, Dimension::Overworld, x, root + 6, z)
                );
                assert!(matches!(leaves, 18 | 161));
                if log == 17 {
                    oak += 1;
                } else {
                    acacia += 1;
                }
            }
        }
        assert!(
            oak + acacia > 0,
            "seed 42 should offer reachable starter trees"
        );
        assert!(supplementary_tree(seed, 0, 0).is_none());
    }

    #[test]
    fn supplementary_tree_canopy_matches_point_generation_across_chunk_edges() {
        let seed = 42;
        let mut checked = 0;
        for x in 16..112 {
            for z in 16..112 {
                if supplementary_tree(seed, x, z).is_none() {
                    continue;
                }
                for dx in -2..=2 {
                    for dz in -2..=2 {
                        let tx = x + dx;
                        let tz = z + dz;
                        let column = OverworldColumn::new(seed, tx, tz);
                        for y in height(seed, x, z) + 1..=height(seed, x, z) + 7 {
                            assert_eq!(
                                column.block_at(y),
                                block(seed, Dimension::Overworld, tx, y, tz),
                                "tree=({x},{z}), column=({tx},{tz}), y={y}"
                            );
                        }
                    }
                }
                checked += 1;
                if checked >= 3 {
                    return;
                }
            }
        }
        assert!(checked > 0, "no supplementary seed-42 trees to verify");
    }

    #[test]
    fn cached_overworld_columns_match_point_generation_at_every_height() {
        for seed in [42, 983] {
            for (x, z) in [(0, 0), (14, 23), (-17, 16), (48, 48), (835, 142)] {
                let column = OverworldColumn::new(seed, x, z);
                for y in 0..256 {
                    assert_eq!(
                        column.block_at(y),
                        block(seed, Dimension::Overworld, x, y, z),
                        "seed={seed}, x={x}, y={y}, z={z}"
                    );
                }
            }
        }
    }

    #[test]
    fn lapis_ores_use_a_triangular_height_distribution() {
        assert_eq!(lapis_layer_weight(-1), 0);
        assert_eq!(lapis_layer_weight(31), 0);
        assert_eq!(lapis_layer_weight(15), 16);
        assert_eq!(lapis_layer_weight(0), lapis_layer_weight(30));
        assert!(lapis_layer_weight(15) > lapis_layer_weight(8));
        assert_eq!((0..=30).map(lapis_layer_weight).sum::<u64>(), 256);
        let spec = ORES.iter().find(|spec| spec.id == 21).unwrap();
        assert_eq!((spec.min_y, spec.max_y), (0, 30));
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
    fn supported_legacy_biome_climate_values_are_distinct() {
        assert_eq!(climate(5), (0.05, 0.8));
        assert_eq!(climate(30), (-0.5, 0.4));
        assert_eq!(climate(10), (0.0, 0.5));
        assert_eq!(climate(11), (0.0, 0.5));
        assert_eq!(climate(26), (0.05, 0.3));
        assert_eq!(climate(27), (0.6, 0.6));
        assert_eq!(climate(21), (1.2, 0.9));
    }

    #[test]
    fn frozen_water_has_surface_ice_and_lower_water_layers() {
        for biome_id in [10, 11] {
            // A submerged column must never be packed full of solid ice.
            assert_eq!(aboveground(42, 0, SEA, 0, SEA - 3, biome_id), b(79));
            assert_eq!(aboveground(42, 0, SEA - 1, 0, SEA - 3, biome_id), b(9));
            assert_eq!(aboveground(42, 0, SEA - 2, 0, SEA - 3, biome_id), b(9));
        }
        assert_eq!(aboveground(42, 0, SEA, 0, SEA - 3, 7), b(9));
        // Cold beaches do not freeze their water into an ocean ice sheet.
        assert_eq!(aboveground(42, 0, SEA, 0, SEA - 3, 26), b(9));
    }

    #[test]
    fn snowy_surfaces_use_a_layer_instead_of_tall_grass() {
        for biome_id in [5, 10, 11, 12, 13, 26, 30] {
            assert_eq!(aboveground(42, 0, SEA + 1, 0, SEA, biome_id), b(78));
        }
        // Never put snow on the bed of a submerged cold river.
        assert_eq!(aboveground(42, 0, SEA - 1, 0, SEA - 3, 11), b(9));
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
    fn village_center_matches_structure_footprints_across_negative_cells() {
        let seed = 42u64;
        let mut tested_negative = false;
        let mut tested_positive = false;
        for gx in -8..=8 {
            for gz in -8..=8 {
                let cx = gx * 192 + 48;
                let cz = gz * 192 + 48;
                if !hash(seed ^ 0x711a, gx, 0, gz).is_multiple_of(4)
                    || !matches!(biome(seed, cx, cz), 1 | 2 | 35 | 4)
                {
                    continue;
                }
                let base = height(seed, cx, cz);
                for (dx, dz) in [(0, 0), (-31, -26), (-31, 26), (31, -26), (31, 26)] {
                    assert_eq!(village_center(seed, cx + dx, cz + dz), Some((cx, base, cz)));
                }
                for (dx, dz) in [(-32, 0), (32, 0), (0, -27), (0, 27)] {
                    assert_eq!(village_center(seed, cx + dx, cz + dz), None);
                }
                tested_negative |= gx < 0 && gz < 0;
                tested_positive |= gx > 0 && gz > 0;
            }
        }
        assert!(tested_negative && tested_positive);
    }
    #[test]
    fn village_center_rejects_non_villages_and_overflowing_cells() {
        let seed = 42u64;
        let mut checked_non_village = false;
        for gx in -8..=8 {
            for gz in -8..=8 {
                let cx = gx * 192 + 48;
                let cz = gz * 192 + 48;
                if hash(seed ^ 0x711a, gx, 0, gz).is_multiple_of(4)
                    && matches!(biome(seed, cx, cz), 1 | 2 | 35 | 4)
                {
                    continue;
                }
                assert_eq!(village_center(seed, cx, cz), None);
                checked_non_village = true;
            }
        }
        assert!(checked_non_village);
        assert_eq!(village_center(seed, i32::MIN, 0), None);
        assert_eq!(village_center(seed, 0, i32::MIN), None);
    }
    #[test]
    fn village_house_doors_match_generated_door_blocks_and_bounds() {
        let seed = 42u64;
        let mut tested_negative = false;
        let mut tested_positive = false;
        for gx in -8..=8 {
            for gz in -8..=8 {
                let cx = gx * 192 + 48;
                let cz = gz * 192 + 48;
                let Some((_, base, _)) = village_center(seed, cx, cz) else {
                    continue;
                };
                let doors = village_house_doors(seed, cx, cz).unwrap();
                assert_eq!(
                    doors,
                    [[cx - 17, base + 1, cz - 5], [cx + 17, base + 1, cz + 5]]
                );
                for (index, [x, y, z]) in doors.into_iter().enumerate() {
                    let facing = if index == 0 { 0 } else { 2 };
                    assert_eq!(village(seed, x, y, z), Some(state(64, facing)));
                    assert_eq!(village(seed, x, y + 1, z), Some(state(64, 8)));
                    assert_eq!(village_house_doors(seed, x, z), Some(doors));
                }
                assert_eq!(village_house_doors(seed, cx + 32, cz), None);
                assert_eq!(village_house_doors(seed, cx, cz - 27), None);
                tested_negative |= gx < 0 && gz < 0;
                tested_positive |= gx > 0 && gz > 0;
            }
        }
        assert!(tested_negative && tested_positive);
        assert_eq!(village_house_doors(seed, i32::MIN, 0), None);
    }
    #[test]
    fn village_live_doors_respect_player_edits_and_door_metadata() {
        let seed = 42u64;
        let mut tested_negative = false;
        let mut tested_positive = false;
        for gx in -8..=8 {
            for gz in -8..=8 {
                let cx = gx * 192 + 48;
                let cz = gz * 192 + 48;
                let Some(doors) = village_house_doors(seed, cx, cz) else {
                    continue;
                };
                let generated = |x, y, z| village(seed, x, y, z).unwrap_or(AIR);
                assert_eq!(
                    village_live_house_doors(seed, cx, cz, generated),
                    Some([Some(doors[0]), Some(doors[1])])
                );
                let first = doors[0];
                let second = doors[1];
                assert_eq!(
                    village_live_house_doors(seed, cx, cz, |x, y, z| {
                        if [x, y, z] == first {
                            AIR // A player broke the lower half.
                        } else if [x, y, z] == [second[0], second[1] + 1, second[2]] {
                            b(1) // A player replaced the upper half with stone.
                        } else {
                            generated(x, y, z)
                        }
                    }),
                    Some([None, None])
                );
                assert_eq!(
                    village_live_house_doors(seed, cx, cz, |x, y, z| {
                        if [x, y, z] == first {
                            state(64, 4) // Open lower half.
                        } else if [x, y, z] == [first[0], first[1] + 1, first[2]] {
                            state(64, 9) // Upper half with hinge bit set.
                        } else {
                            generated(x, y, z)
                        }
                    }),
                    Some([Some(first), Some(second)])
                );
                tested_negative |= gx < 0 && gz < 0;
                tested_positive |= gx > 0 && gz > 0;
            }
        }
        assert!(tested_negative && tested_positive);
        assert_eq!(
            village_live_house_doors(seed, i32::MIN, 0, |_, _, _| b(64)),
            None
        );
    }
    #[test]
    fn village_live_doors_recover_independently_at_chunk_edges() {
        let seed = 42u64;
        let mut saw_negative = false;
        let mut saw_positive = false;
        for gx in -8..=8 {
            for gz in -8..=8 {
                let cx = gx * 192 + 48;
                let cz = gz * 192 + 48;
                let Some([first, second]) = village_house_doors(seed, cx, cz) else {
                    continue;
                };
                // Both door positions straddle a 16-block chunk boundary.
                assert_eq!(first[0].rem_euclid(16), 15);
                assert_eq!(second[0].rem_euclid(16), 1);
                let generated = |x, y, z| village(seed, x, y, z).unwrap_or(AIR);
                assert_eq!(
                    village_live_house_doors(seed, cx, cz, |x, y, z| {
                        if [x, y, z] == first {
                            AIR // The first lower half was broken.
                        } else {
                            generated(x, y, z)
                        }
                    }),
                    Some([None, Some(second)])
                );
                let second_upper = [second[0], second[1] + 1, second[2]];
                assert_eq!(
                    village_live_house_doors(seed, cx, cz, |x, y, z| {
                        if [x, y, z] == second_upper {
                            AIR // The second upper half was broken.
                        } else {
                            generated(x, y, z)
                        }
                    }),
                    Some([Some(first), None])
                );
                // Replacing the wrong door half must not resurrect either door.
                assert_eq!(
                    village_live_house_doors(seed, cx, cz, |x, y, z| {
                        if [x, y, z] == first {
                            state(64, 8) // Upper-half metadata in the lower position.
                        } else if [x, y, z] == second_upper {
                            state(64, 0) // Lower-half metadata in the upper position.
                        } else {
                            generated(x, y, z)
                        }
                    }),
                    Some([None, None])
                );
                // Restoring both halves recovers both village goals.
                assert_eq!(
                    village_live_house_doors(seed, cx, cz, generated),
                    Some([Some(first), Some(second)])
                );
                saw_negative |= gx < 0 && gz < 0;
                saw_positive |= gx > 0 && gz > 0;
            }
        }
        assert!(saw_negative && saw_positive);
    }

    #[test]
    fn village_well_is_not_overwritten_by_crossing_road() {
        // Find valid villages in both positive and negative generation cells.
        // Use the public block lookup too: village() is only one layer of
        // the complete overworld generator.
        let seed = 42u64;
        let mut tested_negative = false;
        let mut tested_positive = false;
        for gx in -8..=8 {
            for gz in -8..=8 {
                if !hash(seed ^ 0x711a, gx, 0, gz).is_multiple_of(4) {
                    continue;
                }
                let cx = gx * 192 + 48;
                let cz = gz * 192 + 48;
                if !matches!(biome(seed, cx, cz), 1 | 2 | 35 | 4) {
                    continue;
                }
                let base = height(seed, cx, cz);
                assert_eq!(village(seed, cx, base, cz), Some(b(4)));
                assert_eq!(village(seed, cx, base + 1, cz), Some(b(9)));
                assert_eq!(block(seed, Dimension::Overworld, cx, base, cz), b(4));
                assert_eq!(block(seed, Dimension::Overworld, cx, base + 1, cz), b(9));
                // The perimeter must be solid on all four sides, while the
                // inner 3x3 remains water, including negative village cells.
                for dx in -2i32..=2 {
                    for dz in -2i32..=2 {
                        let expected = if dx.abs() == 2 || dz.abs() == 2 {
                            b(4)
                        } else {
                            b(9)
                        };
                        assert_eq!(
                            block(seed, Dimension::Overworld, cx + dx, base + 1, cz + dz),
                            expected
                        );
                        // The entire 5x5 well remains accessible above its
                        // rim even when natural terrain is higher nearby.
                        for y in base + 2..=base + 5 {
                            assert_eq!(
                                block(seed, Dimension::Overworld, cx + dx, y, cz + dz),
                                AIR,
                                "blocked well headroom at ({}, {}, {})",
                                cx + dx,
                                y,
                                cz + dz
                            );
                        }
                    }
                }
                assert_eq!(village(seed, cx + 5, base, cz), Some(b(13)));
                assert_eq!(village(seed, cx + 5, base + 1, cz), Some(AIR));
                if gx < 0 && gz < 0 {
                    tested_negative = true;
                }
                if gx > 0 && gz > 0 {
                    tested_positive = true;
                }
            }
        }
        assert!(tested_negative && tested_positive);
    }
    #[test]
    fn village_farm_irrigation_is_capped_at_both_ends() {
        let seed = 42u64;
        let mut tested_negative = false;
        let mut tested_positive = false;
        for gx in -8..=8 {
            for gz in -8..=8 {
                if !hash(seed ^ 0x711a, gx, 0, gz).is_multiple_of(4) {
                    continue;
                }
                let cx = gx * 192 + 48;
                let cz = gz * 192 + 48;
                if !matches!(biome(seed, cx, cz), 1 | 2 | 35 | 4) {
                    continue;
                }
                let base = height(seed, cx, cz);
                for dz in -22..=-13 {
                    for dx in 9..=20 {
                        let channel = matches!(dx, 11 | 15 | 19);
                        let expected_ground = if channel {
                            if dz == -22 || dz == -13 {
                                b(4)
                            } else {
                                b(9)
                            }
                        } else {
                            state(60, 7)
                        };
                        assert_eq!(
                            block(seed, Dimension::Overworld, cx + dx, base, cz + dz),
                            expected_ground
                        );
                        assert_eq!(
                            block(seed, Dimension::Overworld, cx + dx, base + 1, cz + dz),
                            if channel { AIR } else { state(59, 7) }
                        );
                        if !channel {
                            // At least one channel must hydrate every crop,
                            // including both outer edges of the farm.
                            assert!([11_i32, 15, 19]
                                .iter()
                                .any(|&water_x| (dx - water_x).abs() <= 4));
                        }
                    }
                }
                tested_negative |= gx < 0 && gz < 0;
                tested_positive |= gx > 0 && gz > 0;
            }
        }
        assert!(tested_negative && tested_positive);
    }
    #[test]
    fn village_farms_clear_headroom_above_wheat_and_water() {
        let seed = 42u64;
        let mut checked_villages = 0usize;
        let mut tested_negative = false;
        let mut tested_positive = false;
        for gx in -8..=8 {
            for gz in -8..=8 {
                if !hash(seed ^ 0x711a, gx, 0, gz).is_multiple_of(4) {
                    continue;
                }
                let cx = gx * 192 + 48;
                let cz = gz * 192 + 48;
                if !matches!(biome(seed, cx, cz), 1 | 2 | 35 | 4) {
                    continue;
                }
                checked_villages += 1;
                tested_negative |= gx < 0 && gz < 0;
                tested_positive |= gx > 0 && gz > 0;
                let base = height(seed, cx, cz);
                for dx in 9..=20 {
                    for dz in -22..=-13 {
                        let x = cx + dx;
                        let z = cz + dz;
                        for y in base + 2..=base + 3 {
                            assert_eq!(
                                block(seed, Dimension::Overworld, x, y, z),
                                AIR,
                                "farm headroom obstructed at ({x}, {y}, {z})"
                            );
                        }
                    }
                }
            }
        }
        assert!(checked_villages > 0);
        assert!(tested_negative && tested_positive);
    }
    #[test]
    fn village_foundations_support_shallow_gaps_without_replacing_terrain() {
        let seed = 42u64;
        let mut checked_villages = 0usize;
        let mut checked_supports = 0usize;
        let mut checked_well_supports = 0usize;
        let mut tested_negative = false;
        let mut tested_positive = false;
        for gx in -8..=8 {
            for gz in -8..=8 {
                if !hash(seed ^ 0x711a, gx, 0, gz).is_multiple_of(4) {
                    continue;
                }
                let cx = gx * 192 + 48;
                let cz = gz * 192 + 48;
                if !matches!(biome(seed, cx, cz), 1 | 2 | 35 | 4) {
                    continue;
                }
                checked_villages += 1;
                tested_negative |= gx < 0 && gz < 0;
                tested_positive |= gx > 0 && gz > 0;
                let base = height(seed, cx, cz);
                // Road, both door paths, both houses, the irrigation trench,
                // and farmland have different footprints and support blocks.
                for (dx, dz, support) in [
                    (5, 0, b(4)),
                    // Well corners fall outside the central road footprint.
                    (-2, -2, b(4)),
                    (2, 2, b(4)),
                    (-17, -3, b(4)),
                    (17, 3, b(4)),
                    (-17, -9, b(4)),
                    (17, 9, b(4)),
                    (15, -18, b(4)),
                    (14, -18, b(3)),
                ] {
                    let x = cx + dx;
                    let z = cz + dz;
                    let natural_top = height(seed, x, z);
                    for y in base - 4..base {
                        if y > natural_top {
                            assert_eq!(
                                block(seed, Dimension::Overworld, x, y, z),
                                support,
                                "missing support at ({x}, {y}, {z})"
                            );
                            checked_supports += 1;
                            if dx.abs() == 2 && dz.abs() == 2 {
                                checked_well_supports += 1;
                            }
                        }
                    }
                }
            }
        }
        assert!(checked_villages > 0);
        assert!(tested_negative && tested_positive);
        assert!(checked_supports > 0);
        // At least one tested well corner must actually span a shallow gap;
        // otherwise the well-specific regression would never exercise it.
        assert!(checked_well_supports > 0);
    }
    #[test]
    fn village_house_doors_have_clear_paths_to_the_road() {
        let seed = 42u64;
        let mut villages_checked = 0;
        for gx in -4..=4 {
            for gz in -4..=4 {
                if !hash(seed ^ 0x711a, gx, 0, gz).is_multiple_of(4) {
                    continue;
                }
                let cx = gx * 192 + 48;
                let cz = gz * 192 + 48;
                if !matches!(biome(seed, cx, cz), 1 | 2 | 35 | 4) {
                    continue;
                }
                villages_checked += 1;
                let base = height(seed, cx, cz);
                for (dx, start_z, end_z, doorway_z) in [(-17, -4, -2, -5), (17, 2, 4, 5)] {
                    // Each doorway has a complete two-block wooden door,
                    // followed by gravel with four blocks of open headroom.
                    let lower_door = state(64, if dx < 0 { 0 } else { 2 });
                    assert_eq!(
                        village(seed, cx + dx, base + 1, cz + doorway_z),
                        Some(lower_door)
                    );
                    assert_eq!(
                        village(seed, cx + dx, base + 2, cz + doorway_z),
                        Some(state(64, 8))
                    );
                    assert_eq!(
                        block(
                            seed,
                            Dimension::Overworld,
                            cx + dx,
                            base + 1,
                            cz + doorway_z
                        ),
                        lower_door
                    );
                    assert_eq!(
                        block(
                            seed,
                            Dimension::Overworld,
                            cx + dx,
                            base + 2,
                            cz + doorway_z
                        ),
                        state(64, 8)
                    );
                    for dz in start_z..=end_z {
                        assert_eq!(
                            block(seed, Dimension::Overworld, cx + dx, base, cz + dz),
                            b(13)
                        );
                        for dy in 1..=4 {
                            assert_eq!(
                                block(seed, Dimension::Overworld, cx + dx, base + dy, cz + dz),
                                AIR
                            );
                        }
                    }
                }
            }
        }
        assert!(villages_checked > 0);
    }
    #[test]
    fn village_houses_have_floor_torches_and_clear_doorways() {
        let seed = 42u64;
        let mut tested_negative = false;
        let mut tested_positive = false;
        for gx in -4..=4 {
            for gz in -4..=4 {
                if !hash(seed ^ 0x711a, gx, 0, gz).is_multiple_of(4) {
                    continue;
                }
                let cx = gx * 192 + 48;
                let cz = gz * 192 + 48;
                if !matches!(biome(seed, cx, cz), 1 | 2 | 35 | 4) {
                    continue;
                }
                tested_negative |= gx < 0 && gz < 0;
                tested_positive |= gx > 0 && gz > 0;
                let base = height(seed, cx, cz);
                for (hx, hz) in [(-17, -9), (17, 9)] {
                    for a in [-2, 2] {
                        let x = cx + hx + a;
                        let z = cz + hz;
                        assert_eq!(
                            block(seed, Dimension::Overworld, x, base, z),
                            b(5),
                            "torch must have a plank floor at ({x}, {z})"
                        );
                        assert_eq!(
                            block(seed, Dimension::Overworld, x, base + 1, z),
                            state(50, 5),
                            "missing floor torch at ({x}, {z})"
                        );
                        assert_eq!(block(seed, Dimension::Overworld, x, base + 2, z), AIR);
                    }
                    // Keep the central aisle clear from the entrance to the
                    // back of both houses, even after furnishing the rooms.
                    for c in -3..=3 {
                        assert_eq!(
                            block(seed, Dimension::Overworld, cx + hx, base + 1, cz + hz + c),
                            AIR
                        );
                    }
                }
            }
        }
        assert!(tested_negative && tested_positive);
    }
    #[test]
    fn village_houses_have_glass_windows_without_overwriting_doors_or_posts() {
        let seed = 42u64;
        let mut checked = 0;
        for gx in -4..=4 {
            for gz in -4..=4 {
                if !hash(seed ^ 0x711a, gx, 0, gz).is_multiple_of(4) {
                    continue;
                }
                let cx = gx * 192 + 48;
                let cz = gz * 192 + 48;
                if !matches!(biome(seed, cx, cz), 1 | 2 | 35 | 4) {
                    continue;
                }
                checked += 1;
                let base = height(seed, cx, cz);
                for (hx, hz) in [(-17, -9), (17, 9)] {
                    for (a, c) in [(-4, 0), (4, 0), (-2, -4), (2, -4), (-2, 4), (2, 4)] {
                        let x = cx + hx + a;
                        let z = cz + hz + c;
                        assert_eq!(village(seed, x, base + 3, z), Some(b(20)));
                        assert_eq!(block(seed, Dimension::Overworld, x, base + 3, z), b(20));
                    }
                    // Log corner posts remain intact, and the doorway is not
                    // replaced by glass at either half of the wooden door.
                    for (a, c) in [(-4, -4), (-4, 4), (4, -4), (4, 4)] {
                        assert_eq!(
                            block(
                                seed,
                                Dimension::Overworld,
                                cx + hx + a,
                                base + 3,
                                cz + hz + c
                            ),
                            b(17)
                        );
                    }
                    let door_z = cz + hz + if hz < 0 { 4 } else { -4 };
                    let door_x = cx + hx;
                    assert_eq!(
                        block(seed, Dimension::Overworld, door_x, base + 1, door_z),
                        state(64, if hz < 0 { 0 } else { 2 })
                    );
                    assert_eq!(
                        block(seed, Dimension::Overworld, door_x, base + 2, door_z),
                        state(64, 8)
                    );
                }
            }
        }
        assert!(checked > 0);
    }
    #[test]
    fn village_house_roofs_clear_overburden_without_changing_adjacent_terrain() {
        let seed = 42u64;
        let mut tested_negative = false;
        let mut tested_positive = false;
        for gx in -4..=4 {
            for gz in -4..=4 {
                if !hash(seed ^ 0x711a, gx, 0, gz).is_multiple_of(4) {
                    continue;
                }
                let cx = gx * 192 + 48;
                let cz = gz * 192 + 48;
                if !matches!(biome(seed, cx, cz), 1 | 2 | 35 | 4) {
                    continue;
                }
                tested_negative |= gx < 0 && gz < 0;
                tested_positive |= gx > 0 && gz > 0;
                let base = height(seed, cx, cz);
                for (hx, hz) in [(-17, -9), (17, 9)] {
                    // Sample corners, walls and the middle of each roof via
                    // the public block API, including cross-chunk villages.
                    for (a, c) in [(-4, -4), (-4, 0), (0, 0), (4, 0), (4, 4)] {
                        let x = cx + hx + a;
                        let z = cz + hz + c;
                        assert_eq!(block(seed, Dimension::Overworld, x, base + 5, z), b(5));
                        for y in base + 6..=base + 7 {
                            assert_eq!(village(seed, x, y, z), Some(AIR));
                            assert_eq!(
                                block(seed, Dimension::Overworld, x, y, z),
                                AIR,
                                "house roof obstructed at ({x}, {y}, {z})"
                            );
                        }
                    }
                    // No clearing is applied outside the house footprint.
                    assert_eq!(village(seed, cx + hx + 5, base + 6, cz + hz), None);
                }
            }
        }
        assert!(tested_negative && tested_positive);
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
