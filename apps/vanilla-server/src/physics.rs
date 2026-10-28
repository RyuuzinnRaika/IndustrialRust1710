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
        29 | 33 if meta & 8 != 0 => {
            // The extended base's final quarter is occupied by the rod.
            // 1.7.10 BlockPistonBase#setBlockBoundsBasedOnState.
            let mut body = FULL;
            match meta & 7 {
                0 => body.y0 = 0.25,
                1 => body.y1 = 0.75,
                2 => body.z0 = 0.25,
                3 => body.z1 = 0.75,
                4 => body.x0 = 0.25,
                5 => body.x1 = 0.75,
                _ => return vec![FULL],
            }
            vec![body]
        }
        34 => {
            // 1.7.10 piston extension has two collision components: a
            // four-pixel plate and a central rod. It is not empty/full.
            let mut plate = FULL;
            let mut rod = FULL;
            match meta & 7 {
                0 | 1 => {
                    rod.x0 = 0.375;
                    rod.x1 = 0.625;
                    rod.z0 = 0.375;
                    rod.z1 = 0.625;
                    if meta & 7 == 0 {
                        plate.y1 = 0.25;
                        rod.y0 = 0.25;
                    } else {
                        plate.y0 = 0.75;
                        rod.y1 = 0.75;
                    }
                }
                2 | 3 => {
                    rod.x0 = 0.25;
                    rod.x1 = 0.75;
                    rod.y0 = 0.375;
                    rod.y1 = 0.625;
                    if meta & 7 == 2 {
                        plate.z1 = 0.25;
                        rod.z0 = 0.25;
                    } else {
                        plate.z0 = 0.75;
                        rod.z1 = 0.75;
                    }
                }
                4 | 5 => {
                    rod.z0 = 0.25;
                    rod.z1 = 0.75;
                    rod.y0 = 0.375;
                    rod.y1 = 0.625;
                    if meta & 7 == 4 {
                        plate.x1 = 0.25;
                        rod.x0 = 0.25;
                    } else {
                        plate.x0 = 0.75;
                        rod.x1 = 0.75;
                    }
                }
                _ => return Vec::new(),
            }
            vec![plate, rod]
        }
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
            // Bottom metadata carries open state; upper bit 0 carries hinge
            // direction. A right-hinged open door swings the opposite way.
            let orientation = if meta & 4 == 0 {
                meta & 3
            } else if meta & 16 != 0 {
                (meta + 3) & 3
            } else {
                (meta + 1) & 3
            };
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
    // Conservative three-tick ghost AABB while the piston sweeps a cell.
    if world.piston_sweep_at(x, y, z) {
        return vec![FULL];
    }
    let state = world.get(x, y, z);
    let id = state >> 4;
    let mut metadata = state & 15;
    if matches!(id, 64 | 71) {
        let bottom = if metadata & 8 != 0 {
            world.get(x, y - 1, z)
        } else {
            state
        };
        let upper = if metadata & 8 != 0 {
            state
        } else {
            world.get(x, y + 1, z)
        };
        if bottom >> 4 != id {
            return Vec::new();
        }
        metadata = bottom & 7;
        if upper >> 4 == id && upper & 1 != 0 {
            metadata |= 16;
        }
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
/// Reject collisions against the actual entity AABB rather than assuming
/// every mob has the player's 0.6 x 1.8 collision box. The shared block-shape
/// resolver handles slabs, stairs, doors, fences and piston sweep volumes.
pub fn collides_sized(
    world: &World,
    position: &PlayerPosition,
    half_width: f64,
    height: f64,
) -> bool {
    if !position.is_valid()
        || !half_width.is_finite()
        || half_width <= 0.0
        || !height.is_finite()
        || height <= 0.0
    {
        return true;
    }
    let min_x = (position.x - half_width + EPS).floor() as i32;
    let max_x = (position.x + half_width - EPS).floor() as i32;
    let min_z = (position.z - half_width + EPS).floor() as i32;
    let max_z = (position.z + half_width - EPS).floor() as i32;
    let min_y = position.y.floor() as i32 - 1;
    let max_y = (position.y + height).ceil() as i32;
    (min_x..=max_x).any(|x| {
        (min_z..=max_z).any(|z| {
            (min_y..=max_y).any(|y| {
                shapes_at(world, x, y, z).into_iter().any(|shape| {
                    overlap(
                        position.x - half_width,
                        position.x + half_width,
                        x as f64 + shape.x0,
                        x as f64 + shape.x1,
                    ) && overlap(
                        position.y,
                        position.y + height,
                        y as f64 + shape.y0,
                        y as f64 + shape.y1,
                    ) && overlap(
                        position.z - half_width,
                        position.z + half_width,
                        z as f64 + shape.z0,
                        z as f64 + shape.z1,
                    )
                })
            })
        })
    })
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
/// Test a projectile's centered collision box rather than the player's
/// 0.6x1.8 hitbox. The caller supplies the projectile species' half-size:
/// arrows are 0.5 wide, small fireballs/potions are narrower.
pub fn projectile_collides(world: &World, position: &PlayerPosition, half_size: f64) -> bool {
    let min_x = (position.x - half_size + EPS).floor() as i32;
    let max_x = (position.x + half_size - EPS).floor() as i32;
    let min_y = (position.y - half_size + EPS).floor() as i32;
    let max_y = (position.y + half_size - EPS).floor() as i32;
    let min_z = (position.z - half_size + EPS).floor() as i32;
    let max_z = (position.z + half_size - EPS).floor() as i32;
    (min_x..=max_x).any(|x| {
        (min_y..=max_y).any(|y| {
            (min_z..=max_z).any(|z| {
                shapes_at(world, x, y, z).into_iter().any(|shape| {
                    overlap(
                        position.x - half_size,
                        position.x + half_size,
                        x as f64 + shape.x0,
                        x as f64 + shape.x1,
                    ) && overlap(
                        position.y - half_size,
                        position.y + half_size,
                        y as f64 + shape.y0,
                        y as f64 + shape.y1,
                    ) && overlap(
                        position.z - half_size,
                        position.z + half_size,
                        z as f64 + shape.z0,
                        z as f64 + shape.z1,
                    )
                })
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
fn swept_path_clear(world: &World, previous: &PlayerPosition, next: &PlayerPosition) -> bool {
    let dx = next.x - previous.x;
    let dy = next.y - previous.y;
    let dz = next.z - previous.z;
    let horizontal = dx.hypot(dz);
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
    // A newly committed world edit can enclose a player's previous saved
    // AABB (e.g. a delayed piston or a chunk correction). The ordinary
    // swept validator rejects every exit because its starting point collides,
    // permanently rubber-banding the player. Allow only a short escape to
    // a collision-free destination, rejecting any re-entry into another
    // solid block along the way.
    if collides(world, previous) {
        if horizontal > 1.5 || dy.abs() > 1.25 {
            return false;
        }
        let steps = (horizontal.max(dy.abs()) / 0.1).ceil().max(1.0) as usize;
        let mut escaped = false;
        for i in 1..=steps {
            let fraction = i as f64 / steps as f64;
            let sample = PlayerPosition {
                x: previous.x + dx * fraction,
                y: previous.y + dy * fraction,
                z: previous.z + dz * fraction,
            };
            if collides(world, &sample) {
                if escaped {
                    return false;
                }
            } else {
                escaped = true;
            }
        }
        return escaped;
    }
    // A normal jump or step moves upward before crossing a block edge; a
    // descent can move over the edge before dropping. A straight diagonal
    // between the two reported positions incorrectly cuts through the block.
    // Bound this alternative to ordinary small player movements and sweep
    // both legs so neither can pass through a wall or ceiling.
    if dy != 0.0 && dy.abs() <= 1.25 && horizontal <= 1.5 {
        let corner = if dy > 0.0 {
            PlayerPosition {
                y: next.y,
                ..*previous
            }
        } else {
            PlayerPosition {
                x: next.x,
                z: next.z,
                ..*previous
            }
        };
        if !collides(world, &corner)
            && swept_path_clear(world, previous, &corner)
            && swept_path_clear(world, &corner, next)
        {
            return true;
        }
    }
    swept_path_clear(world, previous, next)
}

/// Reconcile a player caught by a newly committed piston extension.
/// Block motion is delivered by the world worker asynchronously from the
/// player's input loop; without this the client becomes trapped in a solid
/// block and the ordinary movement validator rubber-bands it indefinitely.
///
/// This is a bounded server-authoritative resolution, not a replacement for
/// vanilla's half-block-per-tick TileEntityPiston entity sweep.
pub fn piston_push_candidate(
    world: &World,
    player: &PlayerPosition,
    piston: (i32, i32, i32),
    facing: u8,
) -> Option<PlayerPosition> {
    let (dx, dy, dz) = match facing {
        0 => (0, -1, 0),
        1 => (0, 1, 0),
        2 => (0, 0, -1),
        3 => (0, 0, 1),
        4 => (-1, 0, 0),
        5 => (1, 0, 0),
        _ => return None,
    };
    // Only block cells actually written by this move can displace players.
    // The transient swept-cell registry expires after three simulation ticks.
    let intersects = (1..=13).any(|step| {
        let (x, y, z) = (
            piston.0 + dx * step,
            piston.1 + dy * step,
            piston.2 + dz * step,
        );
        world.piston_sweep_at(x, y, z)
            && overlap(
                player.x - HALF_WIDTH,
                player.x + HALF_WIDTH,
                x as f64,
                x as f64 + 1.0,
            )
            && overlap(player.y, player.y + HEIGHT, y as f64, y as f64 + 1.0)
            && overlap(
                player.z - HALF_WIDTH,
                player.z + HALF_WIDTH,
                z as f64,
                z as f64 + 1.0,
            )
    });
    if !intersects {
        return None;
    }
    let next = PlayerPosition {
        x: player.x + dx as f64,
        y: player.y + dy as f64,
        z: player.z + dz as f64,
    };
    // The newly persisted block layout is authoritative. Never shove a
    // player outside world limits or through an immovable wall.
    (next.is_valid() && !collides(world, &next)).then_some(next)
}

const GRAVITY_ACCELERATION: f64 = 0.08;
const AIR_DRAG: f64 = 0.98;
const TERMINAL_VELOCITY: f64 = -3.92;
const IDLE_GRACE_TICKS: u64 = 6;
const MAX_CATCH_UP_STEPS: u64 = 4;

/// Resolve vertical descent against solid block top faces without tunnelling.
fn resolve_descent(world: &World, position: &PlayerPosition, wanted_y: f64) -> f64 {
    // Blocks only exist from Y=0 to 255, but falling entities continue into
    // the void. Do not turn Y=0 into an invisible floor.
    let wanted = wanted_y.max(-256.0);
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
    last_landing: f64,
}
impl FallTracker {
    pub fn reset(&mut self, y: f64) {
        self.last_y = y;
        self.falling = 0.0;
        self.last_landing = 0.0;
    }
    pub fn new(y: f64) -> Self {
        Self {
            last_y: y,
            falling: 0.0,
            last_landing: 0.0,
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
            self.last_landing = self.falling;
            let damage = (self.falling - 3.0).ceil().clamp(0.0, 20.0) as u8;
            self.falling = 0.0;
            damage
        } else {
            self.last_landing = 0.0;
            0
        }
    }
    /// True landing distance across multiple movement packets, not just the
    /// last packet's delta. Used by farmland trampling only after validation.
    pub fn take_landing_distance(&mut self) -> f64 {
        std::mem::take(&mut self.last_landing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f64, y: f64, z: f64) -> PlayerPosition {
        PlayerPosition { x, y, z }
    }

    #[test]
    fn player_trapped_by_changed_block_can_exit_but_cannot_teleport_through_walls() {
        let mut world = World::new();
        world.import_block(0, 65, 0, 1 << 4).unwrap();
        let trapped = at(0.75, 65.0, 0.5);
        assert!(collides(&world, &trapped));
        let clear = at(1.35, 65.0, 0.5);
        assert!(!collides(&world, &clear));
        assert!(valid_movement(&world, &trapped, &clear));
        assert!(!valid_movement(&world, &trapped, &at(3.5, 65.0, 0.5)));
        world.import_block(2, 65, 0, 1 << 4).unwrap();
        assert!(!valid_movement(&world, &trapped, &at(2.5, 65.0, 0.5)));
    }

    #[test]
    fn expanded_seed_42_surface_position_accepts_small_movements() {
        let world = World::expanded(42, crate::dimensions::Dimension::Overworld);
        let start = at(8.699999988079071, 66.01763309290659, 8.300000011920929);
        assert!(!collides(&world, &start));
        assert!(supported(&world, &start));
        for (dx, dz) in [(0.1, 0.0), (-0.1, 0.0), (0.0, 0.1), (0.0, -0.1)] {
            let end = at(start.x + dx, start.y, start.z + dz);
            assert!(valid_movement(&world, &start, &end), "move=({dx},{dz})");
        }
    }

    #[test]
    fn jumping_out_of_and_dropping_into_a_one_block_hole_remains_valid() {
        let mut world = World::expanded(42, crate::dimensions::Dimension::Overworld);
        world.import_block(8, 65, 8, 0).unwrap();
        let in_hole = at(8.699999988079071, 65.0, 8.319575753062963);
        let jumping = at(8.70194596296642, 66.24918707874468, 8.459342066656731);
        let on_surface = at(8.727139172899339, 66.0, 9.049701715671159);
        assert!(!collides(&world, &in_hole));
        assert!(!collides(&world, &jumping));
        assert!(!collides(&world, &on_surface));
        assert!(valid_movement(&world, &in_hole, &jumping), "jump out");
        assert!(valid_movement(&world, &on_surface, &in_hole), "drop in");
    }

    #[test]
    fn stepped_jump_still_checks_the_ceiling() {
        let mut world = World::new();
        world.import_block(0, 66, 0, 1 << 4).unwrap();
        let start = at(0.5, 64.0, 0.5);
        let end = at(1.5, 65.0, 0.5);
        assert!(!collides(&world, &start));
        assert!(!collides(&world, &end));
        assert!(!valid_movement(&world, &start, &end));
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
    fn projectile_uses_small_hitbox_and_respects_partial_blocks() {
        let mut world = World::new();
        world.import_block(1, 64, 0, 1 << 4).unwrap();
        // The player-sized box clips the wall, but a 0.25-wide arrow does not.
        let near_wall = at(0.72, 64.5, 0.5);
        assert!(collides(&world, &near_wall));
        assert!(!projectile_collides(&world, &near_wall, 0.25));
        assert!(projectile_collides(&world, &at(1.02, 64.5, 0.5), 0.25));

        world.import_block(3, 64, 0, 44 << 4).unwrap();
        assert!(projectile_collides(&world, &at(3.5, 64.25, 0.5), 0.25));
        assert!(!projectile_collides(&world, &at(3.5, 64.75, 0.5), 0.25));

        world.import_block(5, 64, 0, 9 << 4).unwrap();
        assert!(!projectile_collides(&world, &at(5.5, 64.5, 0.5), 0.25));
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
    fn empty_world_has_no_invisible_floor_at_y_zero() {
        let world = World::new();
        let position = PlayerPosition {
            x: 0.5,
            y: 0.05,
            z: 0.5,
        };
        let below = resolve_descent(&world, &position, -0.3);
        assert!(below < 0.0);
        let underworld = PlayerPosition {
            y: -64.5,
            ..position
        };
        assert!(!collides(&world, &underworld));
        assert!(!supported(&world, &underworld));
        assert!(valid_movement(
            &world,
            &PlayerPosition { y: 0.05, ..position },
            &PlayerPosition { y: -0.3, ..position }
        ));
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
    fn piston_heads_have_legacy_plate_and_rod_collision_in_all_six_directions() {
        for facing in 0..=5 {
            let volumes = shapes(34, facing);
            assert_eq!(volumes.len(), 2, "missing head shapes for facing {facing}");
            let plate = volumes[0];
            let rod = volumes[1];
            let spans = [
                (plate.x1 - plate.x0, rod.x1 - rod.x0),
                (plate.y1 - plate.y0, rod.y1 - rod.y0),
                (plate.z1 - plate.z0, rod.z1 - rod.z0),
            ];
            let axis = match facing {
                0 | 1 => 1,
                2 | 3 => 2,
                _ => 0,
            };
            assert_eq!(spans[axis], (0.25, 0.75));
            assert_eq!(shapes(34, facing | 8).len(), 2);
        }
    }

    #[test]
    fn extended_piston_base_leaves_space_for_the_rod() {
        for facing in 0..=5 {
            for id in [29, 33] {
                assert_eq!(shapes(id, facing).len(), 1);
                let base = shapes(id, facing | 8)[0];
                let axis_length = match facing {
                    0 | 1 => base.y1 - base.y0,
                    2 | 3 => base.z1 - base.z0,
                    _ => base.x1 - base.x0,
                };
                assert_eq!(axis_length, 0.75);
            }
        }
    }

    #[test]
    fn piston_extension_displaces_overlapping_player_without_wall_clipping() {
        let path = std::env::temp_dir().join(format!(
            "ir1710-physics-piston-player-{}.irw",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut world = World::new();
        world.update_and_save(&path, -1, 64, 0, 152 << 4).unwrap();
        world
            .update_and_save(&path, 0, 64, 0, (33 << 4) | 5)
            .unwrap();
        world.update_and_save(&path, 1, 64, 0, 1 << 4).unwrap();
        world.block_tick(&path, 64).unwrap();
        let caught = PlayerPosition {
            x: 2.5,
            y: 64.0,
            z: 0.5,
        };
        assert_eq!(
            piston_push_candidate(&world, &caught, (0, 64, 0), 5),
            Some(PlayerPosition { x: 3.5, ..caught })
        );
        assert_eq!(piston_push_candidate(&world, &caught, (0, 64, 0), 6), None);
        assert_eq!(
            piston_push_candidate(&world, &PlayerPosition { x: 5.5, ..caught }, (0, 64, 0), 5),
            None
        );
        world.update_and_save(&path, 3, 64, 0, 7 << 4).unwrap();
        assert_eq!(piston_push_candidate(&world, &caught, (0, 64, 0), 5), None);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn open_fence_gate_allows_passage_and_closed_gate_blocks_passage() {
        let mut world = World::new();
        let position = PlayerPosition {
            x: 0.5,
            y: 64.0,
            z: 0.5,
        };
        world.import_block(0, 64, 0, 107 << 4).unwrap();
        assert!(collides(&world, &position));
        world.import_block(0, 64, 0, (107 << 4) | 4).unwrap();
        assert!(!collides(&world, &position));
    }

    #[test]
    fn opened_door_collision_respects_upper_hinge_bit() {
        let mut world = World::new();
        let position = PlayerPosition {
            x: 0.1,
            y: 64.0,
            z: 0.5,
        };
        world.import_block(0, 64, 0, (64 << 4) | 4).unwrap();
        world.import_block(0, 65, 0, (64 << 4) | 8).unwrap();
        assert!(!collides(&world, &position));
        world.import_block(0, 65, 0, (64 << 4) | 9).unwrap();
        assert!(collides(&world, &position));
    }

    #[test]
    fn trapdoor_uses_metadata_dependent_horizontal_volume() {
        let mut world = World::new();
        world.import_block(0, 64, 0, 96 << 4).unwrap();
        assert!(collides(
            &world,
            &PlayerPosition {
                x: 0.5,
                y: 64.0,
                z: 0.5
            }
        ));
        world.import_block(0, 64, 0, (96 << 4) | 4).unwrap();
        assert!(!collides(
            &world,
            &PlayerPosition {
                x: 0.5,
                y: 64.0,
                z: 0.5
            }
        ));
    }
}
