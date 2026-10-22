//! Bounded, server-authoritative Minecraft 1.7.10 boats (object type 1).
//! Player session owns movement/consumption; client steering never supplies entity IDs.
use crate::multiplayer::{self, Event};
use crate::player::PlayerPosition;
use crate::world::World;
use legacy_protocol::write_varint;
use std::collections::BTreeMap;

const FIRST_ID: i32 = 1_400_000;
const LIMIT: usize = 64;

#[derive(Clone, Debug)]
struct Boat {
    id: i32,
    position: PlayerPosition,
    velocity: [f64; 2],
    heading: f32,
    rider: Option<i32>,
    forward: f32,
    strafe: f32,
    last_tick: u64,
}
impl Boat {
    fn spawn(&self) -> Event {
        let mut body = Vec::new();
        write_varint(self.id as u32, &mut body);
        body.push(1); // Protocol 5 Spawn Object: boat
        for value in [self.position.x, self.position.y, self.position.z] {
            body.extend_from_slice(&((value * 32.0).floor() as i32).to_be_bytes());
        }
        body.extend_from_slice(&[0, 0]); // pitch / yaw
        body.extend_from_slice(&0i32.to_be_bytes()); // object data: no velocity
        Event { id: 0x0e, body }
    }
}

pub fn attachment(entity: i32, mount: i32) -> Event {
    let mut body = Vec::with_capacity(9);
    body.extend_from_slice(&entity.to_be_bytes());
    body.extend_from_slice(&mount.to_be_bytes()); // -1 = dismount
    body.push(0); // false: riding, not a leash
    Event { id: 0x1b, body }
}

#[derive(Default)]
pub struct Boats {
    next: i32,
    active: BTreeMap<i32, Boat>,
}
impl Boats {
    pub fn can_place(&self, world: &World, at: PlayerPosition) -> bool {
        self.active.len() < LIMIT
            && self.next.checked_add(FIRST_ID).is_some()
            && at.is_valid()
            && matches!(world.get(at.x.floor() as i32, (at.y - 0.25).floor() as i32, at.z.floor() as i32) >> 4, 8 | 9)
            && self.active.values().all(|b| {
                (b.position.x - at.x).hypot(b.position.z - at.z) > 1.4
            })
    }
    pub fn place(&mut self, world: &World, at: PlayerPosition, tick: u64) -> Option<Event> {
        if !self.can_place(world, at) { return None; }
        let id = FIRST_ID.checked_add(self.next)?;
        self.next = self.next.checked_add(1)?;
        let boat = Boat {
            id, position: at, velocity: [0.0, 0.0], heading: 0.0,
            rider: None, forward: 0.0, strafe: 0.0, last_tick: tick,
        };
        let spawn = boat.spawn();
        self.active.insert(id, boat);
        Some(spawn)
    }
    pub fn mount(&mut self, boat: i32, rider: i32, player: PlayerPosition) -> Option<Event> {
        if self.active.values().any(|b| b.rider == Some(rider)) { return None; }
        let entry = self.active.get_mut(&boat)?;
        if entry.rider.is_some() || (entry.position.x - player.x).hypot(entry.position.z - player.z) > 4.0
            || (entry.position.y - player.y).abs() > 3.0 { return None; }
        entry.rider = Some(rider);
        Some(attachment(rider, boat))
    }
    pub fn boat_for(&self, rider: i32) -> Option<i32> {
        self.active.values().find(|b| b.rider == Some(rider)).map(|b| b.id)
    }
    pub fn rider_position(&self, rider: i32) -> Option<PlayerPosition> {
        self.active.values().find(|b| b.rider == Some(rider)).map(|b| {
            PlayerPosition { y: (b.position.y + 0.3).min(255.9), ..b.position }
        })
    }
    pub fn control(&mut self, rider: i32, strafe: f32, forward: f32, heading: f32) -> bool {
        if !strafe.is_finite() || !forward.is_finite() || !heading.is_finite() { return false; }
        let Some(boat) = self.active.values_mut().find(|b| b.rider == Some(rider)) else { return false };
        boat.strafe = strafe.clamp(-1.0, 1.0);
        boat.forward = forward.clamp(-1.0, 1.0);
        boat.heading = heading.rem_euclid(360.0);
        true
    }
    pub fn dismount(&mut self, rider: i32) -> Option<Event> {
        let boat = self.active.values_mut().find(|b| b.rider == Some(rider))?;
        boat.rider = None;
        boat.forward = 0.0;
        boat.strafe = 0.0;
        Some(attachment(rider, -1))
    }
    pub fn break_boat(&mut self, id: i32, player: PlayerPosition) -> Option<Vec<Event>> {
        let boat = self.active.get(&id)?;
        if (boat.position.x - player.x).hypot(boat.position.z - player.z) > 5.0 { return None; }
        let boat = self.active.remove(&id)?;
        let mut events = Vec::new();
        if let Some(rider) = boat.rider { events.push(attachment(rider, -1)); }
        events.push(multiplayer::destroy(id));
        Some(events)
    }
    /// Advances each boat at most once per server tick, independent of client packet rate.
    /// Only a valid water column can propel a boat; solid obstacles stop its motion.
    pub fn tick(&mut self, world: &World, now: u64) -> Vec<(PlayerPosition, Event)> {
        let mut events = Vec::new();
        for boat in self.active.values_mut() {
            let steps = now.saturating_sub(boat.last_tick).min(20);
            if steps == 0 { continue; }
            boat.last_tick = now;
            for _ in 0..steps {
                let rad = (boat.heading as f64).to_radians();
                boat.velocity[0] = (boat.velocity[0] * 0.84
                    + (-rad.sin() * f64::from(boat.forward)
                       + rad.cos() * f64::from(boat.strafe)) * 0.035).clamp(-0.35, 0.35);
                boat.velocity[1] = (boat.velocity[1] * 0.84
                    + (rad.cos() * f64::from(boat.forward)
                       + rad.sin() * f64::from(boat.strafe)) * 0.035).clamp(-0.35, 0.35);
                let next = PlayerPosition {
                    x: boat.position.x + boat.velocity[0],
                    z: boat.position.z + boat.velocity[1],
                    ..boat.position
                };
                let sample = (next.x.floor() as i32, (next.y - 0.25).floor() as i32, next.z.floor() as i32);
                if !next.is_valid() || !matches!(world.get(sample.0, sample.1, sample.2) >> 4, 8 | 9) {
                    boat.velocity = [0.0, 0.0];
                    break;
                }
                boat.position = next;
            }
            events.push((boat.position, multiplayer::teleport(boat.id, &boat.position, boat.heading, 0.0)));
        }
        events
    }
    pub fn snapshot(&self, observer: PlayerPosition) -> Vec<Event> {
        let mut events = Vec::new();
        for boat in self.active.values().filter(|b| multiplayer::visible(&observer, &b.position)) {
            events.push(boat.spawn());
            if let Some(rider) = boat.rider { events.push(attachment(rider, boat.id)); }
        }
        events
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn boat_spawn_mount_and_bounded_controls() {
        let world = World::new();
        let at = PlayerPosition { x: 0.5, y: 65.25, z: 0.5 };
        // A flat world may not contain water: invalid boat placements remain rejected.
        assert!(!Boats::default().can_place(&world, at));
        let mut boats = Boats::default();
        assert!(boats.mount(FIRST_ID, 2, at).is_none());
        assert!(!boats.control(2, f32::NAN, 0.0, 0.0));
        assert!(world.get(0, 65, 0) != 8 << 4 || !boats.can_place(&world, at));
        assert_eq!(attachment(2, 5).body.len(), 9);
    }
}
