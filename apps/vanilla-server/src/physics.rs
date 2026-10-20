//! Bounded collision and fall tracking for the flat 1.7.10 Survival prototype.
//! The client still simulates its own gravity; the server validates positions,
//! recognizes contact with solid blocks, and owns fall damage. Fluids, slabs,
//! stairs, ladders, vehicles and full per-tick physics remain unsupported.
use crate::player::PlayerPosition;
use crate::world::World;

const HALF_WIDTH: f64 = 0.3;
const HEIGHT: f64 = 1.8;
const EPS: f64 = 1e-7;
const GROUND_TOLERANCE: f64 = 0.0625;

fn solid(id: u16) -> bool {
    // Only full blocks supported by the current flat-world prototype.
    // Chests are conservatively approximated as full cubes until proper AABBs.
    matches!(
        id,
        1 | 2 | 3 | 4 | 5 | 7 | 12 | 13 | 14 | 15 | 16 | 17 | 20
            | 21 | 24 | 35 | 41 | 42 | 45 | 49 | 54 | 56 | 57 | 58
            | 61 | 62 | 73 | 74 | 80 | 82 | 87 | 89 | 98 | 129
            | 155 | 159 | 162 | 168 | 169 | 170 | 172 | 173
    )
}

fn horizontal_box(position: &PlayerPosition) -> (i32, i32, i32, i32) {
    (
        (position.x - HALF_WIDTH + EPS).floor() as i32,
        (position.x + HALF_WIDTH - EPS).floor() as i32,
        (position.z - HALF_WIDTH + EPS).floor() as i32,
        (position.z + HALF_WIDTH - EPS).floor() as i32,
    )
}

/// Reject positions that overlap a known, fully solid block.
pub fn collides(world: &World, position: &PlayerPosition) -> bool {
    let (min_x, max_x, min_z, max_z) = horizontal_box(position);
    let min_y = (position.y + EPS).floor() as i32;
    let max_y = (position.y + HEIGHT - EPS).floor() as i32;
    (min_x..=max_x).any(|x| {
        (min_z..=max_z).any(|z| {
            (min_y..=max_y).any(|y| solid(world.get(x, y, z) >> 4))
        })
    })
}

/// A client's onGround flag alone is not evidence of contact with a block.
pub fn supported(world: &World, position: &PlayerPosition) -> bool {
    let foot_block = (position.y - GROUND_TOLERANCE).floor() as i32;
    if ((foot_block + 1) as f64 - position.y).abs() > GROUND_TOLERANCE {
        return false;
    }
    let (min_x, max_x, min_z, max_z) = horizontal_box(position);
    (min_x..=max_x).any(|x| {
        (min_z..=max_z).any(|z| solid(world.get(x, foot_block, z) >> 4))
    })
}

/// Check the destination and fixed-height, long horizontal movements.
/// A true swept-collision solver and server-owned velocity are future work.
pub fn valid_movement(world: &World, previous: &PlayerPosition, next: &PlayerPosition) -> bool {
    if collides(world, next) {
        return false;
    }
    let dx = next.x - previous.x;
    let dz = next.z - previous.z;
    let horizontal = dx.hypot(dz);
    if horizontal > 64.0 || (next.y - previous.y).abs() > 128.0 {
        return false;
    }
    // Long, level movements cannot tunnel through a wall between endpoints.
    // Do not use a straight-line sweep while jumping: the actual arc differs.
    if horizontal > 0.75 && (next.y - previous.y).abs() < 0.25 {
        let steps = (horizontal / 0.25).ceil() as usize;
        for i in 1..steps {
            let fraction = i as f64 / steps as f64;
            let sample = PlayerPosition {
                x: previous.x + dx * fraction,
                y: previous.y + (next.y - previous.y) * fraction,
                z: previous.z + dz * fraction,
            };
            if collides(world, &sample) {
                return false;
            }
        }
    }
    true
}

/// Tracks actual downward displacement across accepted position packets.
/// A spoofed client onGround bit cannot reset accumulated fall distance.
#[derive(Debug)]
pub struct FallTracker {
    last_y: f64,
    falling: f64,
}
impl FallTracker {
    pub fn new(y: f64) -> Self {
        Self {
            last_y: y,
            falling: 0.0,
        }
    }
    /// Return Minecraft-style integer health damage on an authenticated landing.
    pub fn update(&mut self, y: f64, on_solid_ground: bool) -> u8 {
        if y + EPS < self.last_y {
            self.falling += self.last_y - y;
        } else if y > self.last_y + EPS {
            self.falling = 0.0;
        }
        self.last_y = y;
        if on_solid_ground {
            let damage = (self.falling - 3.0).ceil().clamp(0.0, 20.0) as u8;
            self.falling = 0.0;
            damage
        } else {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f64, y: f64, z: f64) -> PlayerPosition {
        PlayerPosition { x, y, z }
    }

    #[test]
    fn rejects_full_block_intersections_and_tunnelling() {
        let mut world = World::new();
        world.import_block(1, 64, 0, 1 << 4).unwrap();
        let start = at(0.5, 64.0, 0.5);
        assert!(!collides(&world, &start));
        assert!(collides(&world, &at(1.5, 64.0, 0.5)));
        assert!(!valid_movement(&world, &start, &at(1.5, 64.0, 0.5)));
        assert!(!valid_movement(&world, &start, &at(2.5, 64.0, 0.5)));
        assert!(valid_movement(&world, &start, &at(0.5, 66.0, 0.5)));
        assert!(!valid_movement(&world, &start, &at(100.5, 64.0, 0.5)));
    }

    #[test]
    fn landing_requires_a_real_solid_floor_not_client_claims() {
        let mut world = World::new();
        assert!(supported(&world, &at(0.5, 64.0, 0.5)));
        assert!(!supported(&world, &at(0.5, 65.0, 0.5)));
        assert!(!supported(&world, &at(0.5, 64.5, 0.5)));
        world.import_block(0, 63, 0, 0).unwrap();
        assert!(!supported(&world, &at(0.5, 64.0, 0.5)));
    }

    #[test]
    fn fall_damage_is_based_on_height_and_cannot_be_reset_in_midair() {
        let mut tracker = FallTracker::new(72.0);
        assert_eq!(tracker.update(69.0, false), 0);
        assert_eq!(tracker.update(66.0, false), 0);
        assert_eq!(tracker.update(64.0, true), 5);
        assert_eq!(tracker.update(64.0, true), 0);
        let mut short = FallTracker::new(66.0);
        assert_eq!(short.update(64.0, true), 0);
        let mut jumped = FallTracker::new(65.0);
        assert_eq!(jumped.update(62.0, false), 0);
        assert_eq!(jumped.update(67.0, false), 0);
        assert_eq!(jumped.update(64.0, true), 0);
    }
}
