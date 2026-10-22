//! B14: bounded, server-authoritative rideable minecart foundation.
//! Straight and sloped rails and powered acceleration are supported first;
//! junction switching, curves, minecart collisions and durable NBT remain TODO.
use crate::multiplayer::{self, Event};
use crate::player::PlayerPosition;
use crate::vehicles::attachment;
use crate::world::World;
use legacy_protocol::write_varint;
use std::collections::BTreeMap;

const FIRST_ID: i32 = 1_500_000;
const MAX_CARTS: usize = 64;

#[derive(Clone, Debug)]
struct Cart {
    id: i32,
    position: PlayerPosition,
    speed: f64,
    direction: i8,
    east_west: bool,
    rider: Option<i32>,
    last_tick: u64,
}

fn rail(world: &World, pos: PlayerPosition) -> Option<(u16, u16)> {
    let (x, y, z) = (
        pos.x.floor() as i32,
        (pos.y - 0.5).floor() as i32,
        pos.z.floor() as i32,
    );
    let state = world.get(x, y, z);
    let id = state >> 4;
    matches!(id, 27 | 28 | 66 | 157).then_some((id, state & 15))
}

fn straight_axis(id: u16, meta: u16) -> Option<bool> {
    match if id == 66 { meta } else { meta & 7 } {
        0 | 4 | 5 => Some(false), // north/south, ascending N/S
        1..=3 => Some(true),      // east/west, ascending E/W
        _ => None,                // corners and switches need an explicit turn solver
    }
}

/// Rail center height in world coordinates. Ascending rails climb one
/// block over the width of the rail cell. Player-position Y is rail
/// baseline + 0.5, consistent with normal minecart placement.
fn rail_height(y: i32, id: u16, meta: u16, x: f64, z: f64) -> f64 {
    let fraction_x = x.rem_euclid(1.0);
    let fraction_z = z.rem_euclid(1.0);
    let slope = match if id == 66 { meta } else { meta & 7 } {
        2 => fraction_x,
        3 => 1.0 - fraction_x,
        4 => 1.0 - fraction_z,
        5 => fraction_z,
        _ => 0.0,
    };
    f64::from(y) + 0.5 + slope
}

/// Find the actual rail in a column, including a high end of a slope
/// whose rail block is one Y below the cart's current feet position.
/// Refuse transitions with a vertical gap or a perpendicular rail.
fn straight_track(
    world: &World,
    at: PlayerPosition,
    axis: Option<bool>,
) -> Option<(u16, u16, f64, bool)> {
    let (x, z) = (at.x.floor() as i32, at.z.floor() as i32);
    let base = (at.y - 0.5).floor() as i32;
    let mut selected: Option<(u16, u16, f64, bool, f64)> = None;
    for y in [base, base - 1, base + 1] {
        let block = world.get(x, y, z);
        let (id, meta) = (block >> 4, block & 15);
        if !matches!(id, 27 | 28 | 66 | 157) {
            continue;
        }
        let Some(east_west) = straight_axis(id, meta) else {
            continue;
        };
        if axis.is_some_and(|wanted| wanted != east_west) {
            continue;
        }
        let height = rail_height(y, id, meta, at.x, at.z);
        let distance = (height - at.y).abs();
        if distance > 0.51 {
            continue;
        }
        if selected.is_none_or(|candidate| distance < candidate.4) {
            selected = Some((id, meta, height, east_west, distance));
        }
    }
    selected.map(|(id, meta, y, axis, _)| (id, meta, y, axis))
}

impl Cart {
    fn spawn(&self) -> Event {
        let mut body = Vec::new();
        write_varint(self.id as u32, &mut body);
        body.push(10); // Protocol 5 object type: minecart
        for value in [self.position.x, self.position.y, self.position.z] {
            body.extend_from_slice(&((value * 32.0).floor() as i32).to_be_bytes());
        }
        body.extend_from_slice(&[0, 0]); // pitch, yaw
        body.extend_from_slice(&0i32.to_be_bytes()); // rideable minecart subtype
        Event { id: 0x0e, body }
    }
}

#[derive(Default)]
pub struct Minecarts {
    next: i32,
    carts: BTreeMap<i32, Cart>,
}

impl Minecarts {
    pub fn can_place(&self, world: &World, at: PlayerPosition) -> bool {
        self.carts.len() < MAX_CARTS
            && self.next.checked_add(FIRST_ID).is_some()
            && at.is_valid()
            && rail(world, at).is_some()
            && self.carts.values().all(|other| {
                (other.position.x - at.x).hypot(other.position.z - at.z) > 1.0
                    || (other.position.y - at.y).abs() > 1.0
            })
    }

    pub fn place(&mut self, world: &World, at: PlayerPosition, tick: u64) -> Option<Event> {
        if !self.can_place(world, at) {
            return None;
        }
        let id = FIRST_ID.checked_add(self.next)?;
        self.next = self.next.checked_add(1)?;
        let (x, y, z) = (
            at.x.floor() as i32,
            (at.y - 0.5).floor() as i32,
            at.z.floor() as i32,
        );
        let state = world.get(x, y, z);
        let at = PlayerPosition {
            y: rail_height(y, state >> 4, state & 15, at.x, at.z),
            ..at
        };
        let cart = Cart {
            id,
            position: at,
            speed: 0.0,
            direction: 1,
            east_west: straight_axis(state >> 4, state & 15).unwrap_or(false),
            rider: None,
            last_tick: tick,
        };
        let spawned = cart.spawn();
        self.carts.insert(id, cart);
        Some(spawned)
    }

    pub fn mount(&mut self, id: i32, rider: i32, position: PlayerPosition) -> Option<Event> {
        if self.cart_for(rider).is_some() {
            return None;
        }
        let cart = self.carts.get_mut(&id)?;
        if cart.rider.is_some()
            || (cart.position.x - position.x).hypot(cart.position.z - position.z) > 4.0
            || (cart.position.y - position.y).abs() > 3.0
        {
            return None;
        }
        cart.rider = Some(rider);
        Some(attachment(rider, id))
    }

    pub fn cart_for(&self, rider: i32) -> Option<i32> {
        self.carts
            .values()
            .find(|c| c.rider == Some(rider))
            .map(|c| c.id)
    }

    pub fn rider_position(&self, rider: i32) -> Option<PlayerPosition> {
        self.carts
            .values()
            .find(|c| c.rider == Some(rider))
            .map(|c| PlayerPosition {
                y: c.position.y + 0.1,
                ..c.position
            })
    }

    pub fn dismount(&mut self, rider: i32) -> Option<Event> {
        let cart = self.carts.values_mut().find(|c| c.rider == Some(rider))?;
        cart.rider = None;
        Some(attachment(rider, -1))
    }

    /// A bounded push along the rail axis. Never accepts a client position.
    pub fn control(&mut self, rider: i32, forward: f32, heading: f32) -> bool {
        if !forward.is_finite() || !heading.is_finite() {
            return false;
        }
        let Some(cart) = self.carts.values_mut().find(|c| c.rider == Some(rider)) else {
            return false;
        };
        // Minecraft yaw: 0=south, 90=west, 180=north,
        // 270=east. Steering direction must follow the rail axis.
        let yaw = heading.rem_euclid(360.0);
        cart.direction = if cart.east_west {
            if yaw < 180.0 {
                -1
            } else {
                1
            }
        } else if !(90.0..270.0).contains(&yaw) {
            1
        } else {
            -1
        };
        if forward.abs() > 0.01 {
            cart.speed = (cart.speed + f64::from(forward) * 0.012).clamp(-0.4, 0.4);
        }
        true
    }

    pub fn tick(&mut self, world: &World, now: u64) -> Vec<(PlayerPosition, Event)> {
        let mut events = Vec::new();
        // Use a bounded snapshot, updated after each cart moves, so no two
        // server-authoritative carts can occupy the same physical volume.
        let mut occupied: BTreeMap<_, _> = self
            .carts
            .iter()
            .map(|(&id, cart)| (id, cart.position))
            .collect();
        for cart in self.carts.values_mut() {
            let steps = now.saturating_sub(cart.last_tick).min(20);
            if steps == 0 {
                continue;
            }
            cart.last_tick = now;
            for _ in 0..steps {
                let Some((id, meta, height, east_west)) =
                    straight_track(world, cart.position, None)
                else {
                    cart.speed = 0.0;
                    break;
                };
                cart.position.y = height;
                cart.east_west = east_west;
                if id == 27 {
                    if meta & 8 != 0 {
                        let drive = if cart.speed < 0.0 { -1.0 } else { 1.0 };
                        cart.speed = (cart.speed + drive * 0.06).clamp(-0.4, 0.4);
                    } else {
                        cart.speed *= 0.5;
                    }
                } else {
                    cart.speed *= if cart.rider.is_some() { 0.995 } else { 0.96 };
                }
                if cart.speed.abs() < 0.002 {
                    cart.speed = 0.0;
                    break;
                }
                let advance = cart.speed * f64::from(cart.direction);
                let next = PlayerPosition {
                    x: cart.position.x + if east_west { advance } else { 0.0 },
                    z: cart.position.z + if east_west { 0.0 } else { advance },
                    ..cart.position
                };
                // Sample the destination rail at its interpolated elevation.
                // Uphill/downhill transitions must physically meet; a rail
                // that is a full block above or below cannot teleport the cart.
                let Some((_, _, next_height, _)) = straight_track(world, next, Some(east_west))
                else {
                    cart.speed = 0.0;
                    break;
                };
                if (next_height - cart.position.y).abs() > 0.51 {
                    cart.speed = 0.0;
                    break;
                }
                let next = PlayerPosition {
                    y: next_height,
                    ..next
                };
                if !next.is_valid() {
                    cart.speed = 0.0;
                    break;
                }
                let collision = occupied.iter().any(|(&id, position)| {
                    id != cart.id
                        && (position.x - next.x).abs() < 0.9
                        && (position.y - next.y).abs() < 0.75
                        && (position.z - next.z).abs() < 0.9
                });
                if collision {
                    cart.speed = 0.0;
                    break;
                }
                cart.position = next;
                occupied.insert(cart.id, next);
            }
            events.push((
                cart.position,
                multiplayer::teleport(cart.id, &cart.position, 0.0, 0.0),
            ));
        }
        events
    }

    pub fn snapshot(&self, observer: PlayerPosition) -> Vec<Event> {
        let mut events = Vec::new();
        for cart in self
            .carts
            .values()
            .filter(|c| multiplayer::visible(&observer, &c.position))
        {
            events.push(cart.spawn());
            if let Some(rider) = cart.rider {
                events.push(attachment(rider, cart.id));
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ascending_east_connects_lower_and_upper_tracks() {
        let mut world = World::new();
        world.import_block(0, 64, 0, (27 << 4) | 9).unwrap();
        world.import_block(1, 64, 0, (27 << 4) | 10).unwrap();
        world.import_block(2, 65, 0, (27 << 4) | 9).unwrap();
        let at = PlayerPosition {
            x: 0.8,
            y: 64.5,
            z: 0.5,
        };
        let mut carts = Minecarts::default();
        carts.place(&world, at, 0).unwrap();
        let cart = carts.carts.get_mut(&FIRST_ID).unwrap();
        cart.speed = 0.34;
        cart.direction = 1;
        for tick in 1..7 {
            carts.tick(&world, tick);
        }
        let at = carts.carts[&FIRST_ID].position;
        assert!(at.x > 2.0, "cart failed to climb: {}", at.x);
        assert!(
            (at.y - 65.5).abs() < 0.05,
            "incorrect slope height: {}",
            at.y
        );
    }

    #[test]
    fn slope_rejects_unconnected_one_block_jump() {
        let mut world = World::new();
        world.import_block(0, 64, 0, (27 << 4) | 9).unwrap();
        world.import_block(1, 65, 0, (27 << 4) | 9).unwrap();
        let at = PlayerPosition {
            x: 0.85,
            y: 64.5,
            z: 0.5,
        };
        let mut carts = Minecarts::default();
        carts.place(&world, at, 0).unwrap();
        let cart = carts.carts.get_mut(&FIRST_ID).unwrap();
        cart.speed = 0.35;
        carts.tick(&world, 1);
        assert!(carts.carts[&FIRST_ID].position.x < 1.0);
        assert_eq!(carts.carts[&FIRST_ID].speed, 0.0);
    }

    #[test]
    fn minecart_stops_before_perpendicular_unconnected_track() {
        let mut world = World::new();
        world.import_block(0, 65, 0, (27 << 4) | 9).unwrap();
        world.import_block(1, 65, 0, (27 << 4) | 8).unwrap();
        let origin = PlayerPosition {
            x: 0.85,
            y: 65.5,
            z: 0.5,
        };
        let mut carts = Minecarts::default();
        carts.place(&world, origin, 0).unwrap();
        let cart = carts.carts.get_mut(&FIRST_ID).unwrap();
        cart.speed = 0.35;
        cart.direction = 1;
        carts.tick(&world, 1);
        assert!((carts.carts[&FIRST_ID].position.x - origin.x).abs() < f64::EPSILON);
        assert_eq!(carts.carts[&FIRST_ID].speed, 0.0);
    }

    #[test]
    fn colliding_minecarts_stop_before_overlapping() {
        let mut world = World::new();
        for x in 0..4 {
            // Keep the target cart parked on an ordinary rail; powered
            // rails accelerate even an unridden cart in this simulation.
            let rail = if x == 2 { (66 << 4) | 1 } else { (27 << 4) | 9 };
            world.import_block(x, 65, 0, rail).unwrap();
        }
        let mut carts = Minecarts::default();
        let left = PlayerPosition {
            x: 0.5,
            y: 65.5,
            z: 0.5,
        };
        let right = PlayerPosition {
            x: 2.5,
            y: 65.5,
            z: 0.5,
        };
        carts.place(&world, left, 0).unwrap();
        carts.place(&world, right, 0).unwrap();
        carts.carts.get_mut(&FIRST_ID).unwrap().speed = 0.4;
        // Powered rails may re-accelerate a blocked cart on later ticks;
        // the safety property is that the vehicles never overlap.
        for tick in 1..40 {
            carts.tick(&world, tick);
            let first = &carts.carts[&FIRST_ID];
            let other = &carts.carts[&(FIRST_ID + 1)];
            assert!(
                other.position.x - first.position.x >= 0.9,
                "overlapping carts after tick {tick}"
            );
        }
    }

    #[test]
    fn player_yaw_respects_east_west_and_north_south_tracks() {
        let mut world = World::new();
        world.import_block(0, 65, 0, (27 << 4) | 9).unwrap();
        world.import_block(0, 65, 1, (27 << 4) | 8).unwrap();
        let mut east_west = Minecarts::default();
        let at = PlayerPosition {
            x: 0.5,
            y: 65.5,
            z: 0.5,
        };
        east_west.place(&world, at, 0).unwrap();
        east_west.mount(FIRST_ID, 22, at).unwrap();
        assert!(east_west.control(22, 1.0, 90.0));
        assert_eq!(east_west.carts[&FIRST_ID].direction, -1);
        assert!(east_west.control(22, 1.0, 270.0));
        assert_eq!(east_west.carts[&FIRST_ID].direction, 1);

        let mut north_south = Minecarts::default();
        let at = PlayerPosition {
            x: 0.5,
            y: 65.5,
            z: 1.5,
        };
        north_south.place(&world, at, 0).unwrap();
        north_south.mount(FIRST_ID, 23, at).unwrap();
        assert!(north_south.control(23, 1.0, 0.0));
        assert_eq!(north_south.carts[&FIRST_ID].direction, 1);
        assert!(north_south.control(23, 1.0, 180.0));
        assert_eq!(north_south.carts[&FIRST_ID].direction, -1);
    }

    #[test]
    fn minecart_mount_and_rail_bounds() {
        let mut world = World::new();
        world.import_block(0, 65, 0, 27 << 4 | 9).unwrap();
        world.import_block(1, 65, 0, 27 << 4 | 9).unwrap();
        let at = PlayerPosition {
            x: 0.5,
            y: 65.5,
            z: 0.5,
        };
        let mut carts = Minecarts::default();
        let packet = carts.place(&world, at, 0).unwrap();
        assert_eq!(packet.id, 0x0e);
        let mut data = packet.body.as_slice();
        assert_eq!(
            legacy_protocol::read_varint(&mut data).unwrap(),
            FIRST_ID as u32
        );
        assert_eq!(data[0], 10);
        let id = FIRST_ID;
        assert_eq!(carts.mount(id, 11, at).unwrap().id, 0x1b);
        assert!(carts.mount(id, 12, at).is_none());
        assert!(carts.control(11, 1.0, 90.0));
        for tick in 1..10 {
            carts.tick(&world, tick);
        }
        assert!(carts.rider_position(11).is_some());
        assert_eq!(carts.dismount(11).unwrap().id, 0x1b);
        assert!(carts.cart_for(11).is_none());
    }
}
