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
    Arrow,
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
    /// Vanilla dispensers fire arrows, eggs and snowballs. Ender pearls are
    /// dropped as items and must not teleport an arbitrary player.
    fn from_dispenser_item(id: i16) -> Option<Self> {
        match id {
            262 => Some(Self::Arrow),
            332 => Some(Self::Snowball),
            344 => Some(Self::Egg),
            _ => None,
        }
    }

    fn object_type(self) -> u8 {
        match self {
            Self::Arrow => 60,
            Self::Snowball => 61,
            Self::Egg => 62,
            Self::Pearl => 65,
        }
    }
    /// Minecraft 1.7.10 arrows fall faster than thrown eggs, snowballs and pearls.
    fn gravity(self) -> f64 {
        match self {
            Self::Arrow => 0.05,
            Self::Snowball | Self::Egg | Self::Pearl => 0.03,
        }
    }
}
/// Whether an item must be fired as a projectile by a dispenser rather than
/// ejected as a ground item. Droppers never use this path.
pub(crate) fn is_dispenser_projectile(item_id: i16) -> bool {
    Kind::from_dispenser_item(item_id).is_some()
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
    embedded: bool,
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
#[derive(Clone, Default)]
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
    /// Pure validation for a device activation. No inventory or entity state
    /// is changed until the caller durably commits the selected item debit.
    pub fn can_launch_dispenser(
        &self,
        item_id: i16,
        position: (i32, i32, i32),
        direction: (i32, i32, i32),
    ) -> bool {
        let (dx, dy, dz) = direction;
        let distance = i64::from(dx).abs() + i64::from(dy).abs() + i64::from(dz).abs();
        let at = PlayerPosition {
            x: f64::from(position.0) + 0.5 + f64::from(dx) * 0.7,
            y: f64::from(position.1) + 0.5 + f64::from(dy) * 0.7,
            z: f64::from(position.2) + 0.5 + f64::from(dz) * 0.7,
        };
        Kind::from_dispenser_item(item_id).is_some()
            && distance == 1
            && crate::world::in_bounds(position.0, position.1, position.2)
            && at.is_valid()
            && self.active.len() < CAP
            && self.next_id.checked_add(START_ID).is_some()
            && self.next_id.checked_add(1).is_some()
    }

    /// Stage a dispenser projectile in an owned Throwables snapshot. The
    /// world worker replaces the live snapshot only after the inventory
    /// journal has committed, and then broadcasts the spawn packet.
    pub fn launch_dispenser(
        &mut self,
        item_id: i16,
        position: (i32, i32, i32),
        direction: (i32, i32, i32),
    ) -> Option<(PlayerPosition, Event)> {
        if !self.can_launch_dispenser(item_id, position, direction) {
            return None;
        }
        let kind = Kind::from_dispenser_item(item_id)?;
        let (dx, dy, dz) = direction;
        let at = PlayerPosition {
            x: f64::from(position.0) + 0.5 + f64::from(dx) * 0.7,
            y: f64::from(position.1) + 0.5 + f64::from(dy) * 0.7,
            z: f64::from(position.2) + 0.5 + f64::from(dz) * 0.7,
        };
        let id = START_ID.checked_add(self.next_id)?;
        self.next_id = self.next_id.checked_add(1)?;
        let object = Thrown {
            id,
            // Protocol 5 uses a nonzero data field to carry velocity.
            // -1 is a non-player projectile source.
            owner_entity: -1,
            owner_name: String::new(),
            kind,
            at,
            velocity: [
                f64::from(dx) * 1.1,
                f64::from(dy) * 1.1,
                f64::from(dz) * 1.1,
            ],
            age: 0,
            embedded: false,
        };
        let packet = object.spawn();
        self.active.insert(id, object);
        Some((at, packet))
    }

    pub fn can_launch(
        &self,
        item_id: i16,
        owner_entity: i32,
        owner_name: &str,
        position: PlayerPosition,
        yaw: f32,
        pitch: f32,
    ) -> bool {
        Kind::from_item(item_id).is_some()
            && self.active.len() < CAP
            && self.next_id.checked_add(START_ID).is_some()
            && self.next_id.checked_add(1).is_some()
            && owner_entity > 0
            && crate::whitelist::valid_name(owner_name)
            && position.is_valid()
            && position.y + 1.3 < 256.0
            && yaw.is_finite()
            && pitch.is_finite()
    }
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
        if !self.can_launch(item_id, owner_entity, owner_name, position, yaw, pitch) {
            return None;
        }
        let at = PlayerPosition {
            y: position.y + 1.3,
            ..position
        };
        if !at.is_valid() {
            return None;
        }
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
            embedded: false,
        };
        let packet = object.spawn();
        self.active.insert(id, object);
        Some(packet)
    }
    pub fn snapshot(&self, observer: PlayerPosition) -> Vec<Event> {
        self.active
            .values()
            .filter(|item| multiplayer::visible(&observer, &item.at))
            .map(Thrown::spawn)
            .collect()
    }
    pub fn tick(&mut self, world: &World, tick: u64) -> TickResult {
        let mut result = TickResult::default();
        let mut removed = Vec::new();
        for object in self.active.values_mut() {
            object.age = object.age.saturating_add(1);
            // Vanilla arrows remain in their impact block for 60 seconds.
            // They must not repeatedly collide or emit movement while embedded.
            if object.embedded {
                if object.age >= 1200 {
                    removed.push(object.id);
                }
                continue;
            }
            let previous = object.at;
            let destination = PlayerPosition {
                x: previous.x + object.velocity[0],
                y: previous.y + object.velocity[1],
                z: previous.z + object.velocity[2],
            };
            let length = (destination.x - previous.x)
                .hypot(destination.y - previous.y)
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
                if !sample.is_valid() {
                    break;
                }
                let x = sample.x.floor() as i32;
                let y = sample.y.floor() as i32;
                let z = sample.z.floor() as i32;
                let block = world.get(x, y, z) >> 4;
                // Vanilla ray tracing does not stop on water or lava source/flow blocks.
                // Treating liquids as solid here destroys arrows and thrown items early.
                if block != 0
                    && !matches!(
                        block,
                        6 | 8 | 9 | 10 | 11 | 31 | 32 | 37 | 38 | 39 | 40 | 50 | 78
                    )
                {
                    collision = Some(sample);
                    break;
                }
            }
            if let Some(at) = collision {
                // Destroy packets are broadcast to observers near this position.
                // Keep the impact point, not the previous tick's location, so
                // clients near the collision also receive the removal.
                object.at = at;
                if object.kind == Kind::Arrow {
                    object.embedded = true;
                    // Vanilla counts the 1200-tick lifetime from entering the block,
                    // not from launch. Flight time must not shorten embedded lifetime.
                    object.age = 0;
                    object.velocity = [0.0; 3];
                    result
                        .events
                        .push((at, multiplayer::teleport(object.id, &at, 0.0, 0.0)));
                    continue;
                }
                if object.kind == Kind::Pearl {
                    // The existing session-owned teleport path validates and
                    // durably persists the player position before syncing.
                    let target = PlayerPosition {
                        y: at.y + 0.5,
                        ..at
                    };
                    if target.is_valid() {
                        result
                            .pearl_teleports
                            .push((object.owner_name.clone(), target));
                    }
                }
                if object.kind == Kind::Egg {
                    // Deterministic 1-in-8 hatch chance; only the server decides.
                    let roll = (tick ^ (object.id as u64).wrapping_mul(0x9e3779b97f4a7c15))
                        .rotate_left(17);
                    if roll & 7 == 0 {
                        result.egg_hatches.push(at);
                    }
                }
                removed.push(object.id);
            } else if object.age >= 200 || !destination.is_valid() {
                removed.push(object.id);
            } else {
                object.at = destination;
                object.velocity[0] *= 0.99;
                object.velocity[1] = object.velocity[1] * 0.99 - object.kind.gravity();
                object.velocity[2] *= 0.99;
                result.events.push((
                    object.at,
                    multiplayer::teleport(object.id, &object.at, 0.0, 0.0),
                ));
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
    fn dispenser_uses_correct_object_types_and_has_no_player_owner() {
        let mut objects = Throwables::default();
        let origin = (0, 70, 0);
        for (id, kind, direction) in [
            (262, 60u8, (0, 1, 0)),
            (332, 61u8, (1, 0, 0)),
            (344, 62u8, (0, 0, -1)),
        ] {
            let (at, spawn) = objects.launch_dispenser(id, origin, direction).unwrap();
            let mut packet = spawn.body.as_slice();
            assert_eq!(spawn.id, 0x0e);
            assert!(legacy_protocol::read_varint(&mut packet).is_ok());
            assert_eq!(packet[0], kind);
            assert_eq!(i32::from_be_bytes(packet[15..19].try_into().unwrap()), -1);
            assert!(at.is_valid());
        }
        assert_eq!(
            objects
                .snapshot(PlayerPosition {
                    x: 0.5,
                    y: 70.0,
                    z: 0.5,
                })
                .len(),
            3
        );
        for item in [262, 332, 344] {
            assert!(is_dispenser_projectile(item));
        }
        for item in [1, 368, 373] {
            assert!(!is_dispenser_projectile(item));
        }
        // Arrows can be dispensed but not thrown by right-clicking them.
        assert_eq!(Kind::from_item(262), None);
        assert!(!objects.can_launch_dispenser(368, origin, (1, 0, 0)));
        assert!(!objects.can_launch_dispenser(332, origin, (1, 1, 0)));
        assert!(!objects.can_launch_dispenser(344, origin, (0, 0, 0)));
    }

    #[test]
    fn dispenser_rejects_extreme_directions_without_overflow_or_mutation() {
        let mut objects = Throwables::default();
        let origin = (0, 70, 0);
        for direction in [
            (i32::MIN, 0, 0),
            (0, i32::MIN, 0),
            (0, 0, i32::MIN),
            (i32::MAX, 0, 0),
            (i32::MIN, i32::MAX, 0),
        ] {
            assert!(!objects.can_launch_dispenser(262, origin, direction));
            assert!(objects.launch_dispenser(262, origin, direction).is_none());
        }
        assert!(objects.active.is_empty());
        assert_eq!(objects.next_id, 0);
        assert!(objects.launch_dispenser(262, origin, (-1, 0, 0)).is_some());
    }

    #[test]
    fn dispenser_arrows_have_vanilla_gravity_without_changing_thrown_items() {
        let mut objects = Throwables::default();
        for item in [262, 332, 344] {
            assert!(objects
                .launch_dispenser(item, (0, 70, 0), (1, 0, 0))
                .is_some());
        }
        let world = World::new();
        objects.tick(&world, 1);
        for (kind, gravity) in [
            (Kind::Arrow, 0.05),
            (Kind::Snowball, 0.03),
            (Kind::Egg, 0.03),
        ] {
            let object = objects
                .active
                .values()
                .find(|item| item.kind == kind)
                .unwrap();
            assert!((object.velocity[1] + gravity).abs() < 1e-10);
        }
        objects.tick(&world, 2);
        let height = |kind| {
            objects
                .active
                .values()
                .find(|item| item.kind == kind)
                .unwrap()
                .at
                .y
        };
        assert!((height(Kind::Snowball) - height(Kind::Arrow) - 0.02).abs() < 1e-10);
        assert!((height(Kind::Egg) - height(Kind::Snowball)).abs() < 1e-10);
    }

    #[test]
    fn dispenser_projectiles_pass_through_source_and_flowing_liquids() {
        // 1.7.10 projectile block ray tracing ignores both water and lava.
        // Solid-block impacts remain covered by the neighboring regression test.
        for liquid in [8u16, 9, 10, 11] {
            let mut world = World::new();
            world.import_block(2, 70, 0, liquid << 4).unwrap();
            assert_eq!(world.get(2, 70, 0) >> 4, liquid);
            let mut objects = Throwables::default();
            objects
                .launch_dispenser(262, (0, 70, 0), (1, 0, 0))
                .unwrap();

            let result = objects.tick(&world, 1);
            assert_eq!(objects.active.len(), 1, "liquid block {liquid}");
            assert_eq!(result.events.len(), 1, "liquid block {liquid}");
            assert_ne!(result.events[0].1.id, 0x13, "liquid block {liquid}");
            assert!(objects.active.values().next().unwrap().at.x > 2.0);
        }
    }

    #[test]
    fn dispenser_arrow_embeds_behind_liquid_without_duplicate_impact() {
        // Liquids are traversable, but an arrow must stay embedded when it
        // reaches the solid block rather than immediately being destroyed.
        for liquid in [8u16, 11] {
            let mut world = World::new();
            world.import_block(2, 70, 0, liquid << 4).unwrap();
            world.import_block(3, 70, 0, 1 << 4).unwrap();
            let mut objects = Throwables::default();
            objects
                .launch_dispenser(262, (0, 70, 0), (1, 0, 0))
                .unwrap();
            let projectile_id = *objects.active.keys().next().unwrap();

            let first = objects.tick(&world, 1);
            assert_eq!(objects.active.len(), 1, "liquid block {liquid}");
            assert_eq!(first.events.len(), 1);
            assert_eq!(first.events[0].1.id, 0x18);

            let impact = objects.tick(&world, 2);
            assert_eq!(impact.events.len(), 1);
            assert_eq!(impact.events[0].1.id, 0x18);
            let arrow = objects.active.get(&projectile_id).unwrap();
            assert!(arrow.embedded);
            assert_eq!(arrow.velocity, [0.0; 3]);
            assert!((3.0..3.3).contains(&arrow.at.x), "impact={:?}", arrow.at);
            assert!(objects.tick(&world, 3).events.is_empty());
            assert_eq!(objects.active.len(), 1);
        }
    }

    #[test]
    fn dispenser_arrow_stays_in_block_then_expires_at_1200_ticks() {
        let mut objects = Throwables::default();
        let mut world = World::new();
        world.import_block(2, 70, 0, 1 << 4).unwrap();
        let (origin, _) = objects
            .launch_dispenser(262, (0, 70, 0), (1, 0, 0))
            .unwrap();
        let projectile_id = *objects.active.keys().next().unwrap();
        assert!(origin.x < 2.0);

        let impact = objects.tick(&world, 1);
        assert_eq!(objects.active.len(), 1);
        assert_eq!(impact.events.len(), 1);
        let (at, packet) = &impact.events[0];
        assert_eq!(packet.id, 0x18);
        assert_eq!(
            i32::from_be_bytes(packet.body[0..4].try_into().unwrap()),
            projectile_id
        );
        assert!((2.0..2.3).contains(&at.x), "impact={at:?}");
        assert!((at.y - 70.5).abs() < 1e-10);
        assert!(objects.active.get(&projectile_id).unwrap().embedded);

        for tick in 2..1201 {
            assert!(objects.tick(&world, tick).events.is_empty());
        }
        assert_eq!(objects.active.len(), 1);
        let expired = objects.tick(&world, 1201);
        assert!(objects.active.is_empty());
        assert_eq!(expired.events.len(), 1);
        let (at, packet) = &expired.events[0];
        assert_eq!(packet.id, 0x13);
        assert_eq!(packet.body[0], 1);
        assert_eq!(
            i32::from_be_bytes(packet.body[1..5].try_into().unwrap()),
            projectile_id
        );
        assert!((2.0..2.3).contains(&at.x), "expiry={at:?}");
        assert!(objects.tick(&world, 1202).events.is_empty());
    }

    #[test]
    fn dispenser_arrow_flight_time_does_not_shorten_embedded_lifetime() {
        let mut world = World::new();
        world.import_block(2, 70, 0, 8 << 4).unwrap();
        world.import_block(3, 70, 0, 1 << 4).unwrap();
        let mut objects = Throwables::default();
        objects
            .launch_dispenser(262, (0, 70, 0), (1, 0, 0))
            .unwrap();
        let projectile_id = *objects.active.keys().next().unwrap();

        objects.tick(&world, 1);
        assert_eq!(objects.active.get(&projectile_id).unwrap().age, 1);
        let impact = objects.tick(&world, 2);
        assert_eq!(impact.events.len(), 1);
        assert_eq!(impact.events[0].1.id, 0x18);
        let arrow = objects.active.get(&projectile_id).unwrap();
        assert!(arrow.embedded);
        assert_eq!(arrow.age, 0, "embedded lifetime begins at impact");

        for tick in 3..1202 {
            assert!(objects.tick(&world, tick).events.is_empty());
        }
        assert!(objects.active.contains_key(&projectile_id));
        let expired = objects.tick(&world, 1202);
        assert_eq!(expired.events.len(), 1);
        assert_eq!(expired.events[0].1.id, 0x13);
        assert!(!objects.active.contains_key(&projectile_id));
        assert!(objects.tick(&world, 1203).events.is_empty());
    }

    #[test]
    fn dispenser_rejects_full_registry_without_mutation() {
        let mut objects = Throwables::default();
        for _ in 0..CAP {
            assert!(objects
                .launch_dispenser(332, (0, 70, 0), (1, 0, 0))
                .is_some());
        }
        let count = objects.active.len();
        assert!(objects
            .launch_dispenser(344, (0, 70, 0), (0, 1, 0))
            .is_none());
        assert_eq!(objects.active.len(), count);
    }

    #[test]
    fn objects_are_bounded_and_encode_protocol5_types() {
        let mut state = Throwables::default();
        let at = PlayerPosition {
            x: 0.5,
            y: 70.0,
            z: 0.5,
        };
        for (item, obj) in [(332, 61), (344, 62), (368, 65)] {
            let spawn = state.launch(item, 3, "Steve", at, 0.0, 0.0).unwrap();
            assert_eq!(spawn.id, 0x0e);
            assert_eq!(spawn.body[3], obj);
        }
        assert!(state.launch(10, 3, "Steve", at, 0.0, 0.0).is_none());
        assert_eq!(state.snapshot(at).len(), 3);
        assert!(!state.tick(&World::new(), 1).events.is_empty());
    }
}
