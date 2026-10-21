//! Bounded collision and fall tracking for the flat 1.7.10 Survival prototype.
//! The client still simulates its own gravity; the server validates positions,
//! recognizes contact with solid blocks, and owns fall damage. Fluid buoyancy and server-owned horizontal velocity remain partial.
use crate::player::PlayerPosition;
use crate::world::World;

const HALF_WIDTH: f64 = 0.3;
const HEIGHT: f64 = 1.8;
const EPS: f64 = 1e-7;
const GROUND_TOLERANCE: f64 = 0.0625;

/// Block-space collision volume, including metadata-dependent partial heights.
#[derive(Clone, Copy)]
struct Shape {
    x0: f64,
    x1: f64,
    y0: f64,
    y1: f64,
    z0: f64,
    z1: f64,
}
const FULL: Shape = Shape {
    x0: 0.0,
    x1: 1.0,
    y0: 0.0,
    y1: 1.0,
    z0: 0.0,
    z1: 1.0,
};
fn shapes(id: u16, meta: u16) -> Vec<Shape> {
    match id {
        0 | 6 | 8 | 9 | 10 | 11 | 31 | 32 | 37 | 38 | 39 | 40 | 50 | 51 | 55 | 59 | 65 | 66
        | 69 | 70 | 72 | 75 | 76 | 77 | 78 | 83 | 90 | 104 | 105 | 106 | 111 | 115 | 131 | 132
        | 141 | 142 | 143 | 147 | 148 | 149 | 150 | 151 | 157 | 171 => Vec::new(),
        44 | 126 | 182 => {
            let mut box_ = FULL;
            if meta & 8 != 0 {
                box_.y0 = 0.5;
            } else {
                box_.y1 = 0.5;
            }
            vec![box_]
        }
        53 | 67 | 108 | 109 | 114 | 128 | 134 | 135 | 136 | 156 | 163 | 164 => {
            let upper = meta & 4 != 0;
            let mut slab = FULL;
            if upper {
                slab.y0 = 0.5;
            } else {
                slab.y1 = 0.5;
            }
            let mut step = FULL;
            if upper {
                step.y1 = 0.5;
            } else {
                step.y0 = 0.5;
            }
            match meta & 3 {
                0 => step.x0 = 0.5,
                1 => step.x1 = 0.5,
                2 => step.z0 = 0.5,
                _ => step.z1 = 0.5,
            }
            vec![slab, step]
        }
        85 | 113 | 139 => vec![Shape { y1: 1.5, ..FULL }],
        107 => {
            if meta & 4 != 0 {
                Vec::new() // An opened fence gate has no collision volume.
            } else if meta & 1 == 0 {
                vec![Shape {
                    z0: 0.375,
                    z1: 0.625,
                    y1: 1.5,
                    ..FULL
                }]
            } else {
                vec![Shape {
                    x0: 0.375,
                    x1: 0.625,
                    y1: 1.5,
                    ..FULL
                }]
            }
        }
        96 => {
            if meta & 4 == 0 {
                if meta & 8 != 0 {
                    vec![Shape { y0: 0.8125, ..FULL }]
                } else {
                    vec![Shape { y1: 0.1875, ..FULL }]
                }
            } else {
                let thin = match meta & 3 {
                    0 => Shape { z0: 0.8125, ..FULL },
                    1 => Shape { z1: 0.1875, ..FULL },
                    2 => Shape { x0: 0.8125, ..FULL },
                    _ => Shape { x1: 0.1875, ..FULL },
                };
                vec![thin]
            }
        }
        64 | 71 => {
            // For both halves use the lower door metadata (see shapes_at).
            let orientation = (meta + if meta & 4 != 0 { 1 } else { 0 }) & 3;
            let thin = match orientation {
                0 => Shape { z1: 0.1875, ..FULL },
                1 => Shape { x0: 0.8125, ..FULL },
                2 => Shape { z0: 0.8125, ..FULL },
                _ => Shape { x1: 0.1875, ..FULL },
            };
            vec![thin]
        }
        60 => vec![Shape { y1: 0.9375, ..FULL }],
        43 | 125 | 181 => vec![FULL],
        _ => {
            if matches!(
                id,
                1 | 2
                    | 3
                    | 4
                    | 5
                    | 7
                    | 12
                    | 13
                    | 14
                    | 15
                    | 16
                    | 17
                    | 18
                    | 19
                    | 20
                    | 21
                    | 22
                    | 23
                    | 24
                    | 25
                    | 29
                    | 33
                    | 35
                    | 41
                    | 42
                    | 45
                    | 47
                    | 48
                    | 49
                    | 54
                    | 56
                    | 57
                    | 58
                    | 61
                    | 62
                    | 73
                    | 74
                    | 80
                    | 82
                    | 84
                    | 86
                    | 87
                    | 88
                    | 89
                    | 91
                    | 98
                    | 99
                    | 100
                    | 103
                    | 110
                    | 112
                    | 121
                    | 123
                    | 124
                    | 129
                    | 130
                    | 133
                    | 137
                    | 145
                    | 152
                    | 153
                    | 155
                    | 159
                    | 162
                    | 168
                    | 169
                    | 170
                    | 172
                    | 173
                    | 174
                    | 179
            ) {
                vec![FULL]
            } else {
                Vec::new()
            }
        }
    }
}
fn shapes_at(world: &World, x: i32, y: i32, z: i32) -> Vec<Shape> {
    let state = world.get(x, y, z);
    let id = state >> 4;
    let mut metadata = state & 15;
    if matches!(id, 64 | 71) && metadata & 8 != 0 {
        let bottom = world.get(x, y - 1, z);
        if bottom >> 4 != id {
            return Vec::new();
        }
        metadata = bottom & 7;
    }
    shapes(id, metadata)
}
fn overlap(a0: f64, a1: f64, b0: f64, b1: f64) -> bool {
    a0 < b1 - EPS && a1 > b0 + EPS
}
fn horizontal_box(position: &PlayerPosition) -> (i32, i32, i32, i32) {
    (
        (position.x - HALF_WIDTH + EPS).floor() as i32,
        (position.x + HALF_WIDTH - EPS).floor() as i32,
        (position.z - HALF_WIDTH + EPS).floor() as i32,
        (position.z + HALF_WIDTH - EPS).floor() as i32,
    )
}
fn contact(
    world: &World,
    position: &PlayerPosition,
    bx: i32,
    by: i32,
    bz: i32,
    shape: Shape,
) -> bool {
    let _ = world;
    overlap(
        position.x - HALF_WIDTH,
        position.x + HALF_WIDTH,
        bx as f64 + shape.x0,
        bx as f64 + shape.x1,
    ) && overlap(
        position.y,
        position.y + HEIGHT,
        by as f64 + shape.y0,
        by as f64 + shape.y1,
    ) && overlap(
        position.z - HALF_WIDTH,
        position.z + HALF_WIDTH,
        bz as f64 + shape.z0,
        bz as f64 + shape.z1,
    )
}
/// Reject AABB intersections with solid, half-height and stair shapes.
pub fn collides(world: &World, position: &PlayerPosition) -> bool {
    let (min_x, max_x, min_z, max_z) = horizontal_box(position);
    let min_y = position.y.floor() as i32 - 1;
    let max_y = (position.y + HEIGHT).ceil() as i32;
    (min_x..=max_x).any(|x| {
        (min_z..=max_z).any(|z| {
            (min_y..=max_y).any(|y| {
                shapes_at(world, x, y, z)
                    .into_iter()
                    .any(|shape| contact(world, position, x, y, z, shape))
            })
        })
    })
}
fn support_top(world: &World, position: &PlayerPosition) -> Option<f64> {
    let (min_x, max_x, min_z, max_z) = horizontal_box(position);
    let target = position.y;
    let mut highest = None;
    for x in min_x..=max_x {
        for z in min_z..=max_z {
            for y in (target.floor() as i32 - 2)..=(target.floor() as i32) {
                for shape in shapes_at(world, x, y, z) {
                    let top = y as f64 + shape.y1;
                    if top <= target + GROUND_TOLERANCE
                        && top >= target - GROUND_TOLERANCE
                        && overlap(
                            position.x - HALF_WIDTH,
                            position.x + HALF_WIDTH,
                            x as f64 + shape.x0,
                            x as f64 + shape.x1,
                        )
                        && overlap(
                            position.z - HALF_WIDTH,
                            position.z + HALF_WIDTH,
                            z as f64 + shape.z0,
                            z as f64 + shape.z1,
                        )
                    {
                        highest = Some(highest.map_or(top, |v: f64| v.max(top)));
                    }
                }
            }
        }
    }
    highest
}
pub fn supported(world: &World, position: &PlayerPosition) -> bool {
    support_top(world, position).is_some()
}
/// Fluid surfaces are not solids; swimming cancels accumulated fall distance.
pub fn in_water(world: &World, position: &PlayerPosition) -> bool {
    let x = position.x.floor() as i32;
    let z = position.z.floor() as i32;
    [position.y, position.y + 1.0]
        .iter()
        .any(|height| matches!(world.get(x, height.floor() as i32, z) >> 4, 8 | 9))
}
pub fn on_ladder(world: &World, position: &PlayerPosition) -> bool {
    let x = position.x.floor() as i32;
    let z = position.z.floor() as i32;
    matches!(world.get(x, position.y.floor() as i32, z) >> 4, 65)
        || matches!(world.get(x, (position.y + 1.0).floor() as i32, z) >> 4, 65)
}
/// Sweep both horizontal and vertical segments to prevent endpoint tunnelling.
pub fn valid_movement(world: &World, previous: &PlayerPosition, next: &PlayerPosition) -> bool {
    if !next.is_valid() || collides(world, next) {
        return false;
    }
    let dx = next.x - previous.x;
    let dy = next.y - previous.y;
    let dz = next.z - previous.z;
    let horizontal = dx.hypot(dz);
    if horizontal > 64.0 || dy.abs() > 128.0 {
        return false;
    }
    // Client stepping onto a slab or stair first raises the feet, then moves
    // sideways. A naive linear sweep would falsely intersect its lower half.
    if dy > 0.0 && dy <= 0.6 && horizontal <= 1.5 {
        let risen = PlayerPosition {
            y: next.y,
            ..*previous
        };
        if !collides(world, &risen) {
            let steps = (horizontal / 0.2).ceil() as usize;
            if (1..steps).all(|i| {
                let t = i as f64 / steps as f64;
                !collides(
                    world,
                    &PlayerPosition {
                        x: risen.x + dx * t,
                        y: risen.y,
                        z: risen.z + dz * t,
                    },
                )
            }) {
                return true;
            }
        }
    }
    let steps = (horizontal.max(dy.abs()) / 0.2).ceil().clamp(1.0, 640.0) as usize;
    for i in 1..steps {
        let f = i as f64 / steps as f64;
        let sample = PlayerPosition {
            x: previous.x + dx * f,
            y: previous.y + dy * f,
            z: previous.z + dz * f,
        };
        if collides(world, &sample) {
            return false;
        }
    }
    true
}

const GRAVITY_ACCELERATION: f64 = 0.08;
const AIR_DRAG: f64 = 0.98;
const TERMINAL_VELOCITY: f64 = -3.92;
const IDLE_GRACE_TICKS: u64 = 6;
const MAX_CATCH_UP_STEPS: u64 = 4;

/// Resolve vertical descent against solid block top faces without tunnelling.
fn resolve_descent(world: &World, position: &PlayerPosition, wanted_y: f64) -> f64 {
    let wanted = wanted_y.max(0.0);
    let (min_x, max_x, min_z, max_z) = horizontal_box(position);
    let mut best = wanted;
    for x in min_x..=max_x {
        for z in min_z..=max_z {
            for y in (wanted.floor() as i32 - 2).max(0)..=(position.y.floor() as i32).min(255) {
                for shape in shapes_at(world, x, y, z) {
                    let top = y as f64 + shape.y1;
                    if top >= best - EPS
                        && top <= position.y + EPS
                        && overlap(
                            position.x - HALF_WIDTH,
                            position.x + HALF_WIDTH,
                            x as f64 + shape.x0,
                            x as f64 + shape.x1,
                        )
                        && overlap(
                            position.z - HALF_WIDTH,
                            position.z + HALF_WIDTH,
                            z as f64 + shape.z0,
                            z as f64 + shape.z1,
                        )
                    {
                        best = best.max(top);
                    }
                }
            }
        }
    }
    best
}
/// A 20Hz, server-owned fallback for clients that stop sending position
/// packets while airborne. Active 1.7.10 clients normally predict gravity.
#[derive(Debug)]
pub struct IdleGravity {
    last_client_tick: u64,
    last_step_tick: u64,
    vertical_velocity: f64,
}
impl IdleGravity {
    pub fn new(tick: u64) -> Self {
        Self {
            last_client_tick: tick,
            last_step_tick: tick,
            vertical_velocity: 0.0,
        }
    }

    /// Reconcile with accepted client movement, not rejected coordinates.
    pub fn observe_client(&mut self, tick: u64) {
        self.last_client_tick = tick;
        self.last_step_tick = tick;
        self.vertical_velocity = 0.0;
    }

    /// Advance in elapsed server ticks, capped following event-loop stalls.
    pub fn advance(
        &mut self,
        world: &World,
        current: &PlayerPosition,
        tick: u64,
    ) -> Option<PlayerPosition> {
        if tick.saturating_sub(self.last_client_tick) <= IDLE_GRACE_TICKS {
            self.last_step_tick = tick;
            return None;
        }
        let steps = tick
            .saturating_sub(self.last_step_tick)
            .min(MAX_CATCH_UP_STEPS);
        self.last_step_tick = tick;
        let mut next = *current;
        for _ in 0..steps {
            if supported(world, &next) {
                self.vertical_velocity = 0.0;
                break;
            }
            if in_water(world, &next) {
                self.vertical_velocity = ((self.vertical_velocity - 0.02) * 0.8).max(-0.08);
            } else if on_ladder(world, &next) {
                self.vertical_velocity = (self.vertical_velocity - 0.08).max(-0.15);
            } else {
                self.vertical_velocity = ((self.vertical_velocity - GRAVITY_ACCELERATION)
                    * AIR_DRAG)
                    .max(TERMINAL_VELOCITY);
            }
            let y = resolve_descent(world, &next, next.y + self.vertical_velocity);
            if (next.y - y).abs() < EPS {
                self.vertical_velocity = 0.0;
                break;
            }
            let proposed = PlayerPosition { y, ..next };
            if !proposed.is_valid() || collides(world, &proposed) {
                self.vertical_velocity = 0.0;
                break;
            }
            next = proposed;
        }
        (next.y < current.y - EPS).then_some(next)
    }
}

/// Tracks actual downward displacement across accepted position packets.
/// A spoofed client onGround bit cannot reset accumulated fall distance.
#[derive(Debug)]
pub struct FallTracker {
    last_y: f64,
    falling: f64,
}
impl FallTracker {
    pub fn reset(&mut self, y: f64) {
        self.last_y = y;
        self.falling = 0.0;
    }
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
    fn slab_supports_partial_player_height_and_water_cancels_fall() {
        let mut world = World::new();
        world.import_block(2, 64, 0, 44 << 4).unwrap();
        let on_slab = at(2.5, 64.5, 0.5);
        assert!(supported(&world, &on_slab));
        assert!(!collides(&world, &on_slab));
        assert!(collides(&world, &at(2.5, 64.0, 0.5)));
        world.import_block(0, 64, 1, 9 << 4).unwrap();
        assert!(in_water(&world, &at(0.5, 64.0, 1.5)));
        let mut tracker = FallTracker::new(70.0);
        tracker.reset(64.0);
        assert_eq!(tracker.update(64.0, true), 0);
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
    fn idle_gravity_uses_server_ticks_and_sweeps_to_real_floor() {
        let world = World::new();
        let start = at(0.5, 70.0, 0.5);
        let mut fall = FallTracker::new(start.y);
        let mut gravity = IdleGravity::new(0);
        assert_eq!(gravity.advance(&world, &start, 5), None);
        assert_eq!(gravity.advance(&world, &start, 6), None);
        let mut current = start;
        let mut total_damage = 0;
        for tick in 7..80 {
            if let Some(next) = gravity.advance(&world, &current, tick) {
                assert!(next.y <= current.y);
                assert!(!collides(&world, &next));
                total_damage += u32::from(fall.update(next.y, supported(&world, &next)));
                current = next;
            }
        }
        assert_eq!(current.y, 64.0);
        assert_eq!(total_damage, 3);
        assert_eq!(gravity.advance(&world, &current, 81), None);
    }

    #[test]
    fn client_movement_reconciles_idle_gravity_without_rubberbanding() {
        let world = World::new();
        let start = at(0.5, 69.0, 0.5);
        let mut gravity = IdleGravity::new(0);
        assert!(gravity.advance(&world, &start, 7).is_some());
        gravity.observe_client(7);
        assert_eq!(gravity.advance(&world, &start, 12), None);
        assert!(gravity.advance(&world, &start, 14).is_some());
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

#[cfg(test)]
mod group_b_collision_tests {
    use super::*;

    #[test]
    fn open_fence_gate_allows_passage_and_closed_gate_blocks_passage() {
        let mut world = World::new();
        let position = PlayerPosition { x: 0.5, y: 64.0, z: 0.5 };
        world.import_block(0, 64, 0, 107 << 4).unwrap();
        assert!(collides(&world, &position));
        world.import_block(0, 64, 0, (107 << 4) | 4).unwrap();
        assert!(!collides(&world, &position));
    }

    #[test]
    fn trapdoor_uses_metadata_dependent_horizontal_volume() {
        let mut world = World::new();
        world.import_block(0, 64, 0, 96 << 4).unwrap();
        assert!(collides(&world, &PlayerPosition { x: 0.5, y: 64.0, z: 0.5 }));
        world.import_block(0, 64, 0, (96 << 4) | 4).unwrap();
        assert!(!collides(&world, &PlayerPosition { x: 0.5, y: 64.0, z: 0.5 }));
    }
}
