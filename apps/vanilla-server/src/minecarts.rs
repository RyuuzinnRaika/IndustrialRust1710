//! B14: bounded, server-authoritative rideable minecart foundation.
//! Flat straight rails and powered rail acceleration are supported first;
//! junction switching, slopes, minecart collisions and durable NBT remain TODO.
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
        0 => Some(false), // north/south
        1 => Some(true),  // east/west
        _ => None,        // unsupported junction / ascending track
    }
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
        let cart = Cart {
            id,
            position: at,
            speed: 0.0,
            direction: 1,
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
        let sign = if heading.rem_euclid(360.0) < 180.0 {
            1
        } else {
            -1
        };
        cart.direction = sign;
        if forward.abs() > 0.01 {
            cart.speed = (cart.speed + f64::from(forward) * 0.012).clamp(-0.4, 0.4);
        }
        true
    }

    pub fn tick(&mut self, world: &World, now: u64) -> Vec<(PlayerPosition, Event)> {
        let mut events = Vec::new();
        for cart in self.carts.values_mut() {
            let steps = now.saturating_sub(cart.last_tick).min(20);
            if steps == 0 {
                continue;
            }
            cart.last_tick = now;
            for _ in 0..steps {
                let Some((id, meta)) = rail(world, cart.position) else {
                    cart.speed = 0.0;
                    break;
                };
                let Some(east_west) = straight_axis(id, meta) else {
                    cart.speed = 0.0;
                    break;
                };
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
                if !next.is_valid() || rail(world, next).is_none() {
                    cart.speed = 0.0;
                    break;
                }
                cart.position = next;
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
