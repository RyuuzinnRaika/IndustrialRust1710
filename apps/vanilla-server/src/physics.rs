//! Bounded collision and fall tracking for the flat 1.7.10 Survival prototype.
//! The client still simulates its own gravity; the server validates positions,
//! recognizes contact with block AABBs, and owns fall damage. Vehicles and
//! full server-driven swimming motion remain future work.
use crate::player::PlayerPosition;
use crate::world::World;

const HALF_WIDTH: f64 = 0.3;
const HEIGHT: f64 = 1.8;
const EPS: f64 = 1e-7;
const GROUND_TOLERANCE: f64 = 0.0625;

/// Axis-aligned collision box in local block coordinates.
#[derive(Clone, Copy)]
struct Shape { x0: f64, y0: f64, z0: f64, x1: f64, y1: f64, z1: f64 }
const FULL: Shape = Shape { x0: 0.0, y0: 0.0, z0: 0.0, x1: 1.0, y1: 1.0, z1: 1.0 };

fn full_block(id: u16) -> bool {
    matches!(id,
        1 | 2 | 3 | 4 | 5 | 7 | 12 | 13 | 14 | 15 | 16 | 17 |
        18 | 20 | 21 | 24 | 35 | 41 | 42 | 45 | 46 | 49 | 54 |
        56 | 57 | 58 | 61 | 62 | 73 | 74 | 80 | 82 | 87 | 89 |
        98 | 129 | 155 | 159 | 162 | 168 | 169 | 170 | 172 | 173)
}
/// Slabs, stairs, snow and carpets have sub-block shapes. Ladders and fluids
/// never present an impassable full cube. No heap allocation in hot paths.
fn shapes(block: u16) -> [Option<Shape>; 2] {
    let id = block >> 4;
    let meta = block & 15;
    if full_block(id) { return [Some(FULL), None]; }
    if matches!(id, 44 | 126) {
        let (y0,y1) = if meta & 8 == 0 { (0.0,0.5) } else { (0.5,1.0) };
        return [Some(Shape { y0,y1,..FULL }),None];
    }
    if matches!(id, 43 | 125) { return [Some(FULL), None]; } // double slabs
    if matches!(id, 53 | 67 | 108 | 109 | 114 | 128 | 134 | 135 | 136 | 156 | 163 | 164 | 180) {
        let upper = meta & 4 != 0;
        let base = if upper {
            Shape { y0: 0.5, ..FULL }
        } else {
            Shape { y1: 0.5, ..FULL }
        };
        let mut tread = if upper {
            Shape { y1: 0.5, ..FULL }
        } else {
            Shape { y0: 0.5, ..FULL }
        };
        match meta & 3 {
            0 => tread.x0 = 0.5,
            1 => tread.x1 = 0.5,
            2 => tread.z0 = 0.5,
            _ => tread.z1 = 0.5,
        };
        return [Some(base), Some(tread)];
    }
    if id == 78 { // snow layers
        return [Some(Shape { y1: ((meta + 1).min(8) as f64) / 8.0,..FULL }),None];
    }
    if id == 85 || id == 113 { return [Some(Shape { y1: 1.5, ..FULL }),None]; }
    [None,None]
}
fn horizontal_box(position: &PlayerPosition) -> (i32,i32,i32,i32) {
    (
        (position.x - HALF_WIDTH + EPS).floor() as i32,
        (position.x + HALF_WIDTH - EPS).floor() as i32,
        (position.z - HALF_WIDTH + EPS).floor() as i32,
        (position.z + HALF_WIDTH - EPS).floor() as i32,
    )
}
fn overlap(a0:f64,a1:f64,b0:f64,b1:f64)->bool { a0 < b1 - EPS && a1 > b0 + EPS }
fn horizontal_overlap(pos:&PlayerPosition,x:i32,z:i32,b:Shape)->bool {
    overlap(pos.x-HALF_WIDTH,pos.x+HALF_WIDTH,x as f64+b.x0,x as f64+b.x1) &&
    overlap(pos.z-HALF_WIDTH,pos.z+HALF_WIDTH,z as f64+b.z0,z as f64+b.z1)
}
/// AABB hit test for a 0.6 wide, 1.8 high player; no client onGround trust.
pub fn collides(world:&World, position:&PlayerPosition)->bool {
    let (min_x,max_x,min_z,max_z)=horizontal_box(position);
    let min_y=(position.y+EPS).floor() as i32;
    let max_y=(position.y+HEIGHT-EPS).floor() as i32;
    for x in min_x..=max_x { for z in min_z..=max_z { for y in min_y..=max_y {
        for b in shapes(world.get(x,y,z)).into_iter().flatten() {
            if horizontal_overlap(position,x,z,b) &&
                overlap(position.y,position.y+HEIGHT,y as f64+b.y0,y as f64+b.y1) {
                return true;
            }
        }
    }}}
    false
}
pub fn supported(world:&World, position:&PlayerPosition)->bool {
    let (min_x,max_x,min_z,max_z)=horizontal_box(position);
    let foot=(position.y.floor() as i32).saturating_sub(1);
    for x in min_x..=max_x { for z in min_z..=max_z { for y in foot..=(foot+2) {
        for b in shapes(world.get(x,y,z)).into_iter().flatten() {
            let top=y as f64 + b.y1;
            if (position.y - top).abs() <= GROUND_TOLERANCE &&
                horizontal_overlap(position,x,z,b) { return true; }
        }
    }}}
    false
}
pub fn in_water(world:&World,p:&PlayerPosition)->bool {
    [0.1,0.9,1.62].into_iter().any(|dy| matches!(world.get(p.x.floor() as i32,(p.y+dy).floor() as i32,p.z.floor() as i32)>>4,8|9))
}
pub fn in_lava(world:&World,p:&PlayerPosition)->bool {
    [0.1,0.9].into_iter().any(|dy| matches!(world.get(p.x.floor() as i32,(p.y+dy).floor() as i32,p.z.floor() as i32)>>4,10|11))
}
pub fn on_ladder(world:&World,p:&PlayerPosition)->bool {
    [0.1,1.0].into_iter().any(|dy| world.get(p.x.floor() as i32,(p.y+dy).floor() as i32,p.z.floor() as i32)>>4 == 65)
}
/// Probe a level horizontal walk. The client predicts swimming and ladder
/// climbing; the server still rejects tunnelling through collidable blocks.
pub fn valid_movement(world:&World, previous:&PlayerPosition,next:&PlayerPosition)->bool {
    if collides(world,next) {return false;}
    let dx=next.x-previous.x;
    let dy=next.y-previous.y;
    let dz=next.z-previous.z;
    let horizontal=dx.hypot(dz);
    if horizontal>64.0 || dy.abs()>128.0 {return false;}
    if horizontal>0.75 && dy.abs()<0.25 {
        let steps=(horizontal/0.25).ceil() as usize;
        for i in 1..steps {
            let fraction=i as f64/steps as f64;
            let sample=PlayerPosition{x:previous.x+dx*fraction,y:previous.y+dy*fraction,z:previous.z+dz*fraction};
            if collides(world,&sample) {return false;}
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
fn resolve_descent(world:&World,position:&PlayerPosition,wanted_y:f64)->f64 {
    let wanted_y=wanted_y.max(0.0);
    let (min_x,max_x,min_z,max_z)=horizontal_box(position);
    let min_y=(wanted_y.floor() as i32-2).max(0);
    let max_y=position.y.floor() as i32;
    let mut floor=wanted_y;
    for y in min_y..=max_y {for x in min_x..=max_x {for z in min_z..=max_z {
        for b in shapes(world.get(x,y,z)).into_iter().flatten() {
            let top=y as f64+b.y1;
            if top>=wanted_y-EPS && top<=position.y+EPS &&
                horizontal_overlap(position,x,z,b) { floor=floor.max(top); }
        }
    }}}
    floor
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
            self.vertical_velocity =
                ((self.vertical_velocity - GRAVITY_ACCELERATION) * AIR_DRAG).max(TERMINAL_VELOCITY);
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
