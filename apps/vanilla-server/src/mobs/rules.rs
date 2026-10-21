//! Shared, bounded 1.7.10 mob lighting, visibility and explosion rules.
//! These functions read the authoritative world; clients cannot choose damage.
use crate::player::PlayerPosition;
use crate::world::World;

/// A conservative opaque blocker used for ranged attacks and explosions.
fn blocks_ray(id: u16) -> bool {
    !matches!(id, 0 | 6 | 8..=11 | 31 | 32 | 37..=40 | 50 | 51 | 55 | 59
        | 65 | 66 | 69 | 70 | 72 | 75..=78 | 83 | 90 | 93 | 94 | 106
        | 111 | 141 | 142 | 143 | 157 | 171)
}

/// Sample a straight ray at quarter-block resolution, with bounded distance.
/// Supports glass/stone occlusion without using player-sized collision boxes.
pub fn visible(world: &World, from: PlayerPosition, to: PlayerPosition) -> bool {
    if !from.is_valid() || !to.is_valid() {
        return false;
    }
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let dz = to.z - from.z;
    let length = (dx * dx + dy * dy + dz * dz).sqrt();
    if length > 32.0 {
        return false;
    }
    let steps = (length * 4.0).ceil() as usize;
    for index in 1..steps {
        let fraction = index as f64 / steps as f64;
        let p = (
            (from.x + fraction * dx).floor() as i32,
            (from.y + fraction * dy).floor() as i32,
            (from.z + fraction * dz).floor() as i32,
        );
        if blocks_ray(world.get(p.0, p.1, p.2) >> 4) {
            return false;
        }
    }
    true
}

/// Block-light and sky-light use the same computed arrays sent to clients.
/// Daylight reduction is a separate game-time decision, not an edit to skylight.
pub fn can_spawn(
    world: &World,
    at: PlayerPosition,
    passive: bool,
    nighttime: bool,
) -> bool {
    if !at.is_valid() || !crate::physics::supported(world, &at)
        || crate::physics::collides(world, &at)
    {
        return false;
    }
    let x = at.x.floor() as i32;
    let y = at.y.floor() as i32;
    let z = at.z.floor() as i32;
    let (block, sky) = world.light_at(x, y, z);
    let ground = world.get(x, y - 1, z) >> 4;
    if passive {
        ground == 2 && sky.max(block) >= 9
    } else {
        // The skylight engine stores unmodified sky brightness. During dark
        // hours the engine's skylight subtraction is approximated by 11.
        block <= 7 && sky.saturating_sub(if nighttime { 11 } else { 0 }) <= 7
    }
}

/// Sample feet, body and head for bounded block shelter reduction.
pub fn blast_exposure(world: &World, center: PlayerPosition, target: PlayerPosition) -> f64 {
    let mut clear = 0u8;
    for height in [0.1, 0.9, 1.6] {
        if visible(world, center, PlayerPosition { y: target.y + height, ..target }) {
            clear += 1;
        }
    }
    f64::from(clear) / 3.0
}

/// Vanilla-inspired distance/exposure attenuation; saturates to bounded HP.
pub fn blast_damage(radius: f64, distance: f64, exposure: f64) -> u8 {
    if radius <= 0.0 || distance >= radius * 2.0 || exposure <= 0.0 {
        return 0;
    }
    let impact = (1.0 - distance / (radius * 2.0)) * exposure.clamp(0.0, 1.0);
    ((impact * impact + impact) * 7.0 * radius + 1.0)
        .floor()
        .clamp(0.0, 20.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_blocks_skeleton_arrows_and_blasts() {
        let mut world = World::new();
        world.import_block(1, 65, 0, 1 << 4).unwrap();
        world.import_block(1, 66, 0, 1 << 4).unwrap();
        world.import_block(1, 67, 0, 1 << 4).unwrap();
        let from = PlayerPosition { x: 0.5, y: 65.5, z: 0.5 };
        let to = PlayerPosition { x: 2.5, y: 65.5, z: 0.5 };
        assert!(!visible(&world, from, to));
        assert_eq!(blast_exposure(&world, from, to), 0.0);
        assert_eq!(blast_damage(3.0, 2.0, 0.0), 0);
        assert!(blast_damage(3.0, 2.0, 1.0) > 0);
    }

    #[test]
    fn transparent_air_keeps_clear_line_of_sight() {
        let world = World::new();
        let from = PlayerPosition { x: 0.5, y: 65.5, z: 0.5 };
        let to = PlayerPosition { x: 2.5, y: 65.5, z: 0.5 };
        assert!(visible(&world, from, to));
    }

    #[test]
    fn bright_surface_is_safe_by_day() {
        let world = World::new();
        let at = PlayerPosition { x: 0.5, y: 64.0, z: 0.5 };
        assert!(!can_spawn(&world, at, false, false));
        assert!(can_spawn(&world, at, true, false));
        assert!(can_spawn(&world, at, false, true));
    }
}
