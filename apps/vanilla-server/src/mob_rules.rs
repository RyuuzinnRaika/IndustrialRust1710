//! Server-side 1.7.10 spawn, sight, and explosion rules.
//! Kept independent of packets so these rules can be tested without clients.
use crate::{player::PlayerPosition, world::World};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnCategory {
    Passive,
    Hostile,
}

#[derive(Clone, Copy, Debug)]
pub struct SpawnCheck {
    pub sky_light: u8,
    pub block_light: u8,
    pub daytime: bool,
    pub biome: u8,
    pub distance: f64,
    pub nearby_players: bool,
    pub current: usize,
    pub capacity: usize,
    pub difficulty: u8,
    pub grass_below: bool,
    pub has_room: bool,
}
/// The cap applies to the *whole category*, not a particular monster type.
/// Hostiles require darkness at their spawn position; a naturally generated
/// cave may still spawn hostiles during daylight.
pub fn eligible(group: SpawnCategory, c: SpawnCheck) -> bool {
    if !c.has_room
        || !c.nearby_players
        || c.current >= c.capacity
        || !c.distance.is_finite()
        || !(24.0..=128.0).contains(&c.distance)
    {
        return false;
    }
    match group {
        SpawnCategory::Hostile => {
            c.difficulty != 0
                && c.block_light <= 7
                && c.sky_light <= 7
                && !(c.daytime && c.sky_light == 15)
        }
        SpawnCategory::Passive => {
            c.grass_below && c.block_light.max(c.sky_light) >= 9 && c.biome != 0 && c.biome != 2
        }
    }
}
fn opaque(id: u16) -> bool {
    let id = id >> 4;
    !matches!(id, 0 | 6 | 8..=11 | 18 | 20 | 30 | 31 | 37..=40 | 50 | 55
        | 59 | 60 | 63 | 65 | 66 | 69 | 70 | 72 | 75..=78 | 83 | 90
        | 93 | 94 | 96 | 106 | 111 | 141 | 142 | 143 | 157)
}
/// Ray-march through world blocks at <=0.25 block intervals. Bounded by
/// max_range to prevent expensive line-of-sight checks from hostile clients.
pub fn visible(world: &World, from: PlayerPosition, to: PlayerPosition, max_range: f64) -> bool {
    let delta = [to.x - from.x, to.y - from.y, to.z - from.z];
    let len = (delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2]).sqrt();
    if !len.is_finite() || len > max_range || max_range > 64.0 {
        return false;
    }
    let steps = (len * 4.0).ceil().max(1.0) as usize;
    for index in 1..steps {
        let t = index as f64 / steps as f64;
        let x = (from.x + delta[0] * t).floor() as i32;
        let y = (from.y + delta[1] * t).floor() as i32;
        let z = (from.z + delta[2] * t).floor() as i32;
        if opaque(world.get(x, y, z)) {
            return false;
        }
    }
    true
}
/// Simplified 1.7.10 explosion exposure, including solid-block shielding.
/// Every sample is verified on the server rather than supplied by a client.
pub fn blast_damage(
    world: &World,
    center: PlayerPosition,
    target: PlayerPosition,
    radius: f64,
) -> u8 {
    let dx = target.x - center.x;
    let dy = target.y + 0.9 - center.y;
    let dz = target.z - center.z;
    let d = (dx * dx + dy * dy + dz * dz).sqrt();
    if radius <= 0.0 || !d.is_finite() || d >= radius * 2.0 {
        return 0;
    }
    let exposure = if visible(
        world,
        center,
        PlayerPosition {
            y: target.y + 0.9,
            ..target
        },
        radius * 2.0,
    ) {
        1.0
    } else {
        0.15
    };
    let impact = (1.0 - d / (radius * 2.0)) * exposure;
    ((impact * impact + impact) * 7.0 * radius + 1.0)
        .floor()
        .clamp(0.0, 255.0) as u8
}
#[cfg(test)]
mod tests {
    use super::*;
    fn check() -> SpawnCheck {
        SpawnCheck {
            sky_light: 0,
            block_light: 0,
            daytime: false,
            biome: 1,
            distance: 25.0,
            nearby_players: true,
            current: 0,
            capacity: 24,
            difficulty: 1,
            grass_below: true,
            has_room: true,
        }
    }
    #[test]
    fn hostile_requires_darkness_and_difficulty() {
        let mut c = check();
        assert!(eligible(SpawnCategory::Hostile, c));
        c.block_light = 8;
        assert!(!eligible(SpawnCategory::Hostile, c));
        c.block_light = 0;
        c.sky_light = 15;
        assert!(!eligible(SpawnCategory::Hostile, c));
        c.sky_light = 0;
        c.difficulty = 0;
        assert!(!eligible(SpawnCategory::Hostile, c));
    }
    #[test]
    fn caps_distance_and_biome_apply() {
        let mut c = check();
        c.sky_light = 15;
        assert!(eligible(SpawnCategory::Passive, c));
        c.biome = 2;
        assert!(!eligible(SpawnCategory::Passive, c));
        c.biome = 1;
        c.distance = 20.0;
        assert!(!eligible(SpawnCategory::Passive, c));
        c.distance = 25.0;
        c.current = c.capacity;
        assert!(!eligible(SpawnCategory::Passive, c));
    }
    #[test]
    fn occlusion_reduces_blast_and_line_of_sight() {
        let mut world = World::new();
        let a = PlayerPosition {
            x: 0.5,
            y: 66.0,
            z: 0.5,
        };
        let b = PlayerPosition {
            x: 3.5,
            y: 66.0,
            z: 0.5,
        };
        let clear = blast_damage(&world, a, b, 3.0);
        assert!(clear > 0);
        for y in 65..=68 {
            for z in -1..=1 {
                world.import_block(2, y, z, 1 << 4).unwrap();
            }
        }
        assert!(!visible(&world, a, b, 10.0));
        assert!(blast_damage(&world, a, b, 3.0) < clear);
    }
}
