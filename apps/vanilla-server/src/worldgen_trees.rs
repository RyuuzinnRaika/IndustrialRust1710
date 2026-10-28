//! Deterministic Minecraft Java 1.7.10-inspired tree shapes for IRW4.
//! Six wood species, with their legacy log/leaf IDs and metadata.
//! This is an independently seeded approximation, not Mojang's original RNG.
use super::{biome, hash, height, state, supplementary_tree, SEA};

const REACH: i32 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    Oak,
    FancyOak,
    SwampOak,
    Birch,
    Spruce,
    Pine,
    MegaSpruce,
    MegaPine,
    Jungle,
    MegaJungle,
    Acacia,
    DarkOak,
}

#[derive(Clone, Copy, Debug)]
struct Tree {
    x: i32,
    z: i32,
    root: i32,
    height: i32,
    shape: Shape,
    variation: u64,
}

fn thick(shape: Shape) -> bool {
    matches!(
        shape,
        Shape::MegaSpruce | Shape::MegaPine | Shape::MegaJungle | Shape::DarkOak
    )
}

fn candidate(seed: u64, x: i32, z: i32) -> Option<Tree> {
    // Keep the spawn-safe area, and avoid overflow when evaluating 2x2 roots.
    if (-12..12).contains(&x) && (-12..12).contains(&z) {
        return None;
    }
    if !(i32::MIN + 8..=i32::MAX - 8).contains(&x)
        || !(i32::MIN + 8..=i32::MAX - 8).contains(&z)
    {
        return None;
    }
    // Most columns are not trees. Do this before expensive biome noise.
    let old_roll = hash(seed ^ 0x7eed, x, 0, z);
    let new_roll = hash(seed ^ 0x5ce3_77ea, x, 0, z);
    if ![37, 47, 28, 59, 44, 12, 31]
        .iter()
        .any(|&freq| old_roll.is_multiple_of(freq))
        && !new_roll.is_multiple_of(80)
        && !new_roll.is_multiple_of(144)
    {
        return None;
    }
    let biome_id = biome(seed, x, z);
    let variation = hash(seed ^ 0x7e22_170a, x, 0, z);
    let shape = match biome_id {
        1 => {
            supplementary_tree(seed, x, z)?;
            Shape::Oak
        }
        35 => {
            supplementary_tree(seed, x, z)?;
            Shape::Acacia
        }
        4 if old_roll.is_multiple_of(37) => {
            if variation.is_multiple_of(5) {
                Shape::Birch
            } else {
                Shape::Oak
            }
        }
        18 if old_roll.is_multiple_of(44) => Shape::Oak,
        27 if old_roll.is_multiple_of(37) => Shape::Birch,
        5 | 30 if old_roll.is_multiple_of(47) => {
            if variation.is_multiple_of(4) {
                Shape::Pine
            } else {
                Shape::Spruce
            }
        }
        32 if old_roll.is_multiple_of(31) => {
            if variation.is_multiple_of(3) {
                Shape::MegaPine
            } else {
                Shape::MegaSpruce
            }
        }
        21 if old_roll.is_multiple_of(28) => {
            if variation.is_multiple_of(7) {
                Shape::MegaJungle
            } else {
                Shape::Jungle
            }
        }
        6 if old_roll.is_multiple_of(59) => Shape::SwampOak,
        29 if old_roll.is_multiple_of(12) => {
            if variation.is_multiple_of(9) {
                Shape::FancyOak
            } else {
                Shape::DarkOak
            }
        }
        _ => return None,
    };
    let root = height(seed, x, z);
    if root < SEA || (matches!(biome_id, 1 | 35) && root < SEA + 2) {
        return None;
    }
    // A four-sapling/thick-trunk tree needs a level 2x2 soil foundation.
    if thick(shape)
        && (height(seed, x + 1, z) != root
            || height(seed, x, z + 1) != root
            || height(seed, x + 1, z + 1) != root)
    {
        return None;
    }
    let height = match shape {
        Shape::Oak | Shape::SwampOak => 4 + (variation % 3) as i32,
        Shape::FancyOak => 8 + (variation % 5) as i32,
        Shape::Birch => 5 + (variation % 4) as i32,
        Shape::Spruce => 6 + (variation % 5) as i32,
        Shape::Pine => 8 + (variation % 6) as i32,
        Shape::MegaSpruce | Shape::MegaPine => 18 + (variation % 11) as i32,
        Shape::Jungle => 5 + (variation % 6) as i32,
        Shape::MegaJungle => 18 + (variation % 12) as i32,
        Shape::Acacia => 5 + (variation % 4) as i32,
        Shape::DarkOak => 6 + (variation % 4) as i32,
    };
    (root + height + 3 < 255).then_some(Tree {
        x,
        z,
        root,
        height,
        shape,
        variation,
    })
}

fn palette(shape: Shape) -> (u16, u16, u16, u16) {
    match shape {
        Shape::Spruce | Shape::Pine | Shape::MegaSpruce | Shape::MegaPine => (17, 1, 18, 1),
        Shape::Birch => (17, 2, 18, 2),
        Shape::Jungle | Shape::MegaJungle => (17, 3, 18, 3),
        Shape::Acacia => (162, 0, 161, 0),
        Shape::DarkOak => (162, 1, 161, 1),
        Shape::Oak | Shape::FancyOak | Shape::SwampOak => (17, 0, 18, 0),
    }
}

// Minecraft 1.7.10 log metadata: 0x4 means X-axis, 0x8 means Z-axis.
fn log_at(t: Tree, dx: i32, dy: i32, dz: i32, log: u16, meta: u16) -> Option<u16> {
    let h = t.height;
    if !(1..=h).contains(&dy) {
        return None;
    }
    if thick(t.shape) && (0..=1).contains(&dx) && (0..=1).contains(&dz) {
        return Some(state(log, meta));
    }
    if !thick(t.shape)
        && dx == 0
        && dz == 0
        && (t.shape != Shape::Acacia || dy <= h - 3)
    {
        return Some(state(log, meta));
    }
    let dir = ((t.variation >> 9) % 4) as i32;
    let (vx, vz) = match dir {
        0 => (1, 0),
        1 => (-1, 0),
        2 => (0, 1),
        _ => (0, -1),
    };
    if t.shape == Shape::Acacia {
        // One bent main trunk and an occasional opposing secondary branch.
        if dy >= h - 2 {
            let step = (dy - h + 3).clamp(1, 3);
            if (dx, dz) == (vx * step, vz * step)
                || (dx, dz) == (vx * (step - 1), vz * (step - 1))
            {
                return Some(state(log, meta | if vx != 0 { 4 } else { 8 }));
            }
        }
        if t.variation & 1 == 0
            && (h - 3..=h - 1).contains(&dy)
            && (dx, dz) == (-vz * (dy - h + 4), vx * (dy - h + 4))
        {
            return Some(state(log, meta | if vz != 0 { 4 } else { 8 }));
        }
    }
    if matches!(t.shape, Shape::MegaJungle | Shape::FancyOak | Shape::DarkOak) {
        let start = match t.shape {
            Shape::MegaJungle => 6,
            Shape::FancyOak => 4,
            _ => 2,
        };
        for branch in 0..3 {
            let by = h - start - branch * 3;
            if by < 3 || dy != by {
                continue;
            }
            let (bx, bz) = match ((t.variation >> (branch * 2)) & 3) as i32 {
                0 => (1, 0),
                1 => (-1, 0),
                2 => (0, 1),
                _ => (0, -1),
            };
            let length = if t.shape == Shape::DarkOak { 2 } else { 3 };
            for k in 1..=length {
                let base = if thick(t.shape) && (bx > 0 || bz > 0) { 1 } else { 0 };
                if (dx, dz) == (bx * k + bx * base, bz * k + bz * base) {
                    return Some(state(log, meta | if bx != 0 { 4 } else { 8 }));
                }
            }
        }
    }
    None
}

fn disk(dx: i32, dz: i32, radius: i32, variation: u64, salt: u64) -> bool {
    if dx.abs() > radius || dz.abs() > radius {
        return false;
    }
    // Jagged, usually missing outer corners; always deterministic.
    dx.abs() != radius
        || dz.abs() != radius
        || hash(variation ^ salt, dx, 0, dz).is_multiple_of(4)
}

fn crown_2x2(dx: i32, dz: i32, radius: i32, variation: u64, salt: u64) -> bool {
    let nx = if dx < 0 { -dx } else { (dx - 1).max(0) };
    let nz = if dz < 0 { -dz } else { (dz - 1).max(0) };
    disk(nx, nz, radius, variation, salt)
}

fn foliage_at(t: Tree, dx: i32, dy: i32, dz: i32) -> bool {
    let h = t.height;
    let v = t.variation;
    match t.shape {
        Shape::Oak | Shape::Birch | Shape::SwampOak | Shape::FancyOak => {
            if (h - 2..=h + 1).contains(&dy) {
                let r = if dy == h + 1 { 1 } else { 2 };
                if disk(dx, dz, r, v, dy as u64) {
                    return true;
                }
            }
            if t.shape == Shape::FancyOak {
                for branch in 0..3 {
                    let by = h - 4 - branch * 3;
                    if (by - 1..=by + 1).contains(&dy) {
                        let (bx, bz) = match ((v >> (branch * 2)) & 3) as i32 {
                            0 => (3, 0),
                            1 => (-3, 0),
                            2 => (0, 3),
                            _ => (0, -3),
                        };
                        if disk(dx - bx, dz - bz, 2, v, (dy ^ by) as u64) {
                            return true;
                        }
                    }
                }
            }
            false
        }
        Shape::Spruce | Shape::Pine => {
            let crown_start = if t.shape == Shape::Pine { h - 4 } else { h / 2 };
            if dy < crown_start || dy > h + 1 {
                return false;
            }
            let down = h + 1 - dy;
            let r = if t.shape == Shape::Pine {
                if down <= 1 { 1 } else { 2 }
            } else {
                ((down + 1) / 2).min(3)
            };
            disk(dx, dz, r, v, dy as u64)
        }
        Shape::MegaSpruce | Shape::MegaPine => {
            let crown_start = if t.shape == Shape::MegaPine { h - 6 } else { h - 12 };
            if dy < crown_start || dy > h + 1 {
                return false;
            }
            let down = h + 1 - dy;
            let r = if t.shape == Shape::MegaPine {
                (down + 1).min(4)
            } else {
                ((down + 2) / 2).min(5)
            };
            crown_2x2(dx, dz, r, v, dy as u64)
        }
        Shape::Jungle => {
            (h - 2..=h + 1).contains(&dy)
                && disk(dx, dz, if dy == h + 1 { 1 } else { 2 }, v, dy as u64)
        }
        Shape::MegaJungle => {
            if (h - 3..=h + 2).contains(&dy) {
                let r = if dy == h + 2 { 2 } else if dy == h - 3 { 3 } else { 4 };
                if crown_2x2(dx, dz, r, v, dy as u64) {
                    return true;
                }
            }
            for branch in 0..3 {
                let by = h - 6 - branch * 3;
                if (by - 1..=by + 1).contains(&dy) {
                    let (bx, bz) = match ((v >> (branch * 2)) & 3) as i32 {
                        0 => (4, 0),
                        1 => (-3, 0),
                        2 => (0, 4),
                        _ => (0, -3),
                    };
                    if disk(dx - bx, dz - bz, 2, v, (dy ^ by) as u64) {
                        return true;
                    }
                }
            }
            false
        }
        Shape::Acacia => {
            let dir = ((v >> 9) % 4) as i32;
            let (vx, vz) = match dir {
                0 => (1, 0),
                1 => (-1, 0),
                2 => (0, 1),
                _ => (0, -1),
            };
            let center = (vx * 3, vz * 3);
            if (h - 1..=h + 1).contains(&dy)
                && disk(
                    dx - center.0,
                    dz - center.1,
                    if dy == h + 1 { 1 } else { 2 },
                    v,
                    dy as u64,
                )
            {
                return true;
            }
            v & 1 == 0
                && (h - 3..=h - 1).contains(&dy)
                && disk(
                    dx + vz * 2,
                    dz - vx * 2,
                    if dy == h - 1 { 1 } else { 2 },
                    v,
                    dy as u64,
                )
        }
        Shape::DarkOak => {
            if (h - 2..=h + 2).contains(&dy) {
                let r = if dy == h + 2 { 1 } else if dy == h + 1 { 2 } else { 3 };
                if crown_2x2(dx, dz, r, v, dy as u64) {
                    return true;
                }
            }
            false
        }
    }
}

fn voxel(t: Tree, x: i32, y: i32, z: i32) -> Option<u16> {
    let (log, meta, leaves, leaf_meta) = palette(t.shape);
    let (dx, dy, dz) = (x - t.x, y - t.root, z - t.z);
    if dy < 1 || dy > t.height + 2 {
        return None;
    }
    if let Some(block) = log_at(t, dx, dy, dz, log, meta) {
        return Some(block);
    }
    if foliage_at(t, dx, dy, dz) {
        // Never set metadata bit 0x4: that means player-placed, non-decaying.
        return Some(state(leaves, leaf_meta));
    }
    if matches!(t.shape, Shape::Jungle | Shape::MegaJungle | Shape::SwampOak)
        && dy <= t.height - 2
        && hash(t.variation ^ 0x106, x, y, z).is_multiple_of(4)
    {
        // Vines hang on the outer faces of jungle and swamp trunks.
        let side = if thick(t.shape) { 2 } else { 1 };
        for (ox, oz, attachment) in [(-1, 0, 8), (side, 0, 2), (0, -1, 1), (0, side, 4)] {
            if (dx, dz) == (ox, oz) {
                return Some(state(106, attachment));
            }
        }
    }
    None
}

/// Build the same species-specific geometry for a sapling-grown tree.
/// The caller owns the soil, obstructions and transaction checks; this pure
/// helper never reads or edits the world and cannot replace a player's build.
/// Root y is the *sapling's* y, while Tree::root represents the soil below it.
pub(super) fn sapling_blocks(
    seed: u64,
    x: i32,
    y: i32,
    z: i32,
    species: u16,
    large: bool,
) -> Vec<(i32, i32, i32, u16)> {
    let variation = hash(seed ^ 0x5a91_1eaf, x, y, z);
    let shape = match (species, large) {
        (0, false) if variation.is_multiple_of(10) => Shape::FancyOak,
        (0, false) => Shape::Oak,
        (1, true) if variation.is_multiple_of(3) => Shape::MegaPine,
        (1, true) => Shape::MegaSpruce,
        (1, false) if variation.is_multiple_of(4) => Shape::Pine,
        (1, false) => Shape::Spruce,
        (2, false) => Shape::Birch,
        (3, true) => Shape::MegaJungle,
        (3, false) => Shape::Jungle,
        (4, false) => Shape::Acacia,
        (5, true) => Shape::DarkOak,
        _ => return Vec::new(),
    };
    let trunk_height = match shape {
        Shape::Oak => 4 + (variation % 3) as i32,
        Shape::FancyOak => 8 + (variation % 5) as i32,
        Shape::Birch => 5 + (variation % 4) as i32,
        Shape::Spruce => 6 + (variation % 5) as i32,
        Shape::Pine => 8 + (variation % 6) as i32,
        Shape::MegaSpruce | Shape::MegaPine => 18 + (variation % 11) as i32,
        Shape::Jungle => 5 + (variation % 6) as i32,
        Shape::MegaJungle => 18 + (variation % 12) as i32,
        Shape::Acacia => 5 + (variation % 4) as i32,
        Shape::DarkOak => 6 + (variation % 4) as i32,
        Shape::SwampOak => unreachable!("swamp shape is not selected by saplings"),
    };
    if !(1..=253).contains(&y) || y + trunk_height + 1 > 255 {
        return Vec::new();
    }
    let t = Tree {
        x,
        z,
        root: y - 1,
        height: trunk_height,
        shape,
        variation,
    };
    let mut blocks = Vec::new();
    // REACH is already enough for branch heads, acacia tips and 2x2 crowns.
    for px in x - REACH..=x + REACH {
        for pz in z - REACH..=z + REACH {
            for py in y..=y + trunk_height + 1 {
                if let Some(block) = voxel(t, px, py, pz) {
                    blocks.push((px, py, pz, block));
                }
            }
        }
    }
    blocks
}

fn candidates<F: FnMut(Tree) -> bool>(seed: u64, x: i32, z: i32, mut visit: F) {
    'outer: for tx in x.saturating_sub(REACH)..=x.saturating_add(REACH) {
        for tz in z.saturating_sub(REACH)..=z.saturating_add(REACH) {
            if let Some(t) = candidate(seed, tx, tz) {
                if visit(t) {
                    break 'outer;
                }
            }
        }
    }
}

pub(super) fn at(seed: u64, x: i32, y: i32, z: i32) -> Option<u16> {
    // IRW4 height is clamped to 125; the tallest 1.7.10-style crown
    // ends below Y=159. Avoid any biome/height probes outside that band.
    if !(SEA + 1..=158).contains(&y) {
        return None;
    }
    let mut result = None;
    candidates(seed, x, z, |tree| {
        if let Some(block) = voxel(tree, x, y, z) {
            result = Some(block);
            true
        } else {
            false
        }
    });
    result
}

pub(super) fn column(seed: u64, x: i32, z: i32) -> [Option<u16>; 256] {
    let mut blocks = [None; 256];
    candidates(seed, x, z, |tree| {
        for y in (tree.root + 1).max(0)..=(tree.root + tree.height + 2).min(255) {
            let slot = &mut blocks[y as usize];
            if slot.is_none() {
                *slot = voxel(tree, x, y, z);
            }
        }
        false
    });
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(shape: Shape) -> Tree {
        Tree { x: 0, z: 0, root: 70, height: 21, shape, variation: 12 }
    }

    #[test]
    fn six_1710_wood_species_use_the_correct_log_and_leaf_metadata() {
        for (shape, expected) in [
            (Shape::Oak, (17, 0, 18, 0)),
            (Shape::Spruce, (17, 1, 18, 1)),
            (Shape::Birch, (17, 2, 18, 2)),
            (Shape::Jungle, (17, 3, 18, 3)),
            (Shape::Acacia, (162, 0, 161, 0)),
            (Shape::DarkOak, (162, 1, 161, 1)),
        ] {
            assert_eq!(palette(shape), expected);
            let t = sample(shape);
            let log = voxel(t, 0, t.root + 1, 0).unwrap();
            assert_eq!((log >> 4, log & 3), (expected.0, expected.1));
            let mut leaf_count = 0;
            for y in t.root + 1..=t.root + t.height + 2 {
                for x in -6..=6 {
                    for z in -6..=6 {
                        let Some(block) = voxel(t, x, y, z) else { continue };
                        if block >> 4 == expected.2 {
                            assert_eq!(block & 0x4, 0, "generated leaves must decay normally");
                            assert_eq!(block & 3, expected.3);
                            leaf_count += 1;
                        }
                    }
                }
            }
            assert!(leaf_count > 8, "tree has no real canopy: {:?}", shape);
        }
    }

    #[test]
    fn old_growth_and_dark_oak_have_four_trunk_columns() {
        for shape in [Shape::MegaSpruce, Shape::MegaJungle, Shape::DarkOak] {
            let t = sample(shape);
            let species = palette(shape);
            for dx in 0..=1 {
                for dz in 0..=1 {
                    assert_eq!(voxel(t, dx, t.root + 2, dz), Some(state(species.0, species.1)));
                }
            }
        }
    }

    #[test]
    fn acacia_is_bent_and_spruce_is_conical() {
        let acacia = sample(Shape::Acacia);
        assert!(matches!(voxel(acacia, 0, acacia.root + 1, 0), Some(_)));
        assert!(matches!(voxel(acacia, 3, acacia.root + acacia.height, 0), Some(_)));
        // A bent acacia must not keep an impossible straight central log above the bend.
        assert_eq!(voxel(acacia, 0, acacia.root + acacia.height, 0), None);
        let spruce = sample(Shape::Spruce);
        assert!(foliage_at(spruce, 2, spruce.height / 2, 0));
        assert!(!foliage_at(spruce, 2, spruce.height + 1, 0));
    }

    #[test]
    fn columns_and_point_reads_agree_across_negative_chunk_boundaries() {
        for &(x,z) in &[(-17,-16),(-16,-17),(-1,0),(15,16),(16,15)] {
            let canopy = column(42, x, z);
            for y in (58..=103).step_by(3) {
                assert_eq!(canopy[y], at(42, x, y as i32, z), "at {x},{y},{z}");
            }
        }
    }
}
