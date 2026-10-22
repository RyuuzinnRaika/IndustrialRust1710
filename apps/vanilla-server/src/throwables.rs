//! Bounded, server-authoritative 1.7.10 thrown entities.
//! The item debit is owned by the player session; this module never trusts
//! client positions, entity identifiers, or projectile hit claims.
use crate::multiplayer::{self, Event};
use crate::player::PlayerPosition;
use crate::world::World;
use legacy_protocol::write_varint;
use std::collections::BTreeMap;

const CAP: usize = 128;
const START_ID: i32 = 1_200_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Snowball,
    Egg,
    Pearl,
}
impl Kind {
    pub fn from_item(id: i16) -> Option<Self> {
        match id {
            332 => Some(Self::Snowball),
            344 => Some(Self::Egg),
            368 => Some(Self::Pearl),
            _ => None,
        }
    }
    fn object_type(self) -> u8 {
        match self {
            Self::Snowball => 61,
            Self::Egg => 62,
            Self::Pearl => 65,
        }
    }
}
#[derive(Clone, Debug)]
struct Thrown {
    id: i32,
    owner_entity: i32,
    owner_name: String,
    kind: Kind,
    at: PlayerPosition,
    velocity: [f64; 3],
    age: u16,
}
impl Thrown {
    fn spawn(&self) -> Event {
        let mut body = Vec::new();
        write_varint(self.id as u32, &mut body);
        body.push(self.kind.object_type());
        for coordinate in [self.at.x, self.at.y, self.at.z] {
            body.extend_from_slice(&((coordinate * 32.0).floor() as i32).to_be_bytes());
        }
        body.extend_from_slice(&[0, 0]); // pitch and yaw
        body.extend_from_slice(&self.owner_entity.to_be_bytes());
        for speed in self.velocity {
            body.extend_from_slice(
                &((speed * 8000.0).clamp(-32768.0, 32767.0) as i16).to_be_bytes(),
            );
        }
        Event { id: 0x0e, body }
    }
}
#[derive(Default)]
pub struct Throwables {
    next_id: i32,
    active: BTreeMap<i32, Thrown>,
}
#[derive(Default)]
pub struct TickResult {
    pub events: Vec<(PlayerPosition, Event)>,
    pub pearl_teleports: Vec<(String, PlayerPosition)>,
    pub egg_hatches: Vec<PlayerPosition>,
}
impl Throwables {
    pub fn launch(
        &mut self,
        item_id: i16,
        owner_entity: i32,
        owner_name: &str,
        position: PlayerPosition,
        yaw: f32,
        pitch: f32,
    ) -> Option<Event> {
        let kind = Kind::from_item(item_id)?;
        if self.active.len() >= CAP
            || owner_entity <= 0
            || !crate::whitelist::valid_name(owner_name)
            || !position.is_valid()
            || !yaw.is_finite()
            || !pitch.is_finite()
        {
            return None;
        }
        let at = PlayerPosition { y: position.y + 1.3, ..position };
        if !at.is_valid() { return None; }
        let id = START_ID.checked_add(self.next_id)?;
        self.next_id = self.next_id.checked_add(1)?;
        let y = (yaw as f64).to_radians();
        let p = (pitch as f64).to_radians();
        let speed = 1.5;
        let object = Thrown {
            id,
            owner_entity,
            owner_name: owner_name.into(),
            kind,
            at,
            velocity: [
                -y.sin() * p.cos() * speed,
                -p.sin() * speed,
                y.cos() * p.cos() * speed,
            ],
            age: 0,
        };
        let packet = object.spawn();
        self.active.insert(id, object);
        Some(packet)
    }
    pub fn snapshot(&self, observer: PlayerPosition) -> Vec<Event> {
        self.active.values().filter(|item| multiplayer::visible(&observer, &item.at))
            .map(Thrown::spawn).collect()
    }
    pub fn tick(&mut self, world: &World, tick: u64) -> TickResult {
        let mut result = TickResult::default();
        let mut removed = Vec::new();
        for object in self.active.values_mut() {
            object.age = object.age.saturating_add(1);
            let previous = object.at;
            let destination = PlayerPosition {
                x: previous.x + object.velocity[0],
                y: previous.y + object.velocity[1],
                z: previous.z + object.velocity[2],
            };
            let length = (destination.x - previous.x).hypot(destination.y - previous.y)
                .hypot(destination.z - previous.z);
            let steps = (length * 4.0).ceil().clamp(1.0, 32.0) as usize;
            let mut collision = None;
            for index in 1..=steps {
                let t = index as f64 / steps as f64;
                let sample = PlayerPosition {
                    x: previous.x + (destination.x - previous.x) * t,
                    y: previous.y + (destination.y - previous.y) * t,
                    z: previous.z + (destination.z - previous.z) * t,
                };
                if !sample.is_valid() { break; }
                let x = sample.x.floor() as i32;
                let y = sample.y.floor() as i32;
                let z = sample.z.floor() as i32;
                let block = world.get(x,y,z) >> 4;
                if block != 0 && !matches!(block, 6 | 31 | 32 | 37 | 38 | 39 | 40 | 50 | 78) {
                    collision = Some(sample);
                    break;
                }
            }
            if let Some(at) = collision {
                if object.kind == Kind::Pearl {
                    // The existing session-owned teleport path validates and
                    // durably persists the player position before syncing.
                    let target = PlayerPosition { y: at.y + 0.5, ..at };
                    if target.is_valid() {
                        result.pearl_teleports.push((object.owner_name.clone(), target));
                    }
                }
                if object.kind == Kind::Egg {
                    // Deterministic 1-in-8 hatch chance; only the server decides.
                    let roll = (tick ^ (object.id as u64).wrapping_mul(0x9e3779b97f4a7c15))
                        .rotate_left(17);
                    if roll & 7 == 0 { result.egg_hatches.push(at); }
                }
                removed.push(object.id);
            } else if object.age >= 200 || !destination.is_valid() {
                removed.push(object.id);
            } else {
                object.at = destination;
                object.velocity[0] *= 0.99;
                object.velocity[1] = object.velocity[1] * 0.99 - 0.03;
                object.velocity[2] *= 0.99;
                result.events.push((object.at, multiplayer::teleport(object.id, &object.at, 0.0, 0.0)));
            }
        }
        for id in removed {
            if let Some(object) = self.active.remove(&id) {
                result.events.push((object.at, multiplayer::destroy(id)));
            }
        }
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn objects_are_bounded_and_encode_protocol5_types() {
        let mut state = Throwables::default();
        let at = PlayerPosition { x: 0.5, y: 70.0, z: 0.5 };
        for (item, obj) in [(332,61),(344,62),(368,65)] {
            let spawn = state.launch(item, 3,"Steve",at,0.0,0.0).unwrap();
            assert_eq!(spawn.id,0x0e);
            assert_eq!(spawn.body[3],obj);
        }
        assert!(state.launch(10,3,"Steve",at,0.0,0.0).is_none());
        assert_eq!(state.snapshot(at).len(),3);
        assert!(!state.tick(&World::new(),1).events.is_empty());
    }
}
