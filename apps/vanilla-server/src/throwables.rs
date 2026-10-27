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

/// Vanilla eggs hatch on 1/8 impacts; 1/32 of those hatches produce four
/// chicks instead of one. Derive both rolls from the same stable server-side
/// value so clients cannot influence the outcome or duplicate the hatch.
fn egg_hatch_count(tick: u64, entity_id: i32) -> usize {
    let roll = (tick ^ (entity_id as u64).wrapping_mul(0x9e3779b97f4a7c15)).rotate_left(17);
    if roll & 7 != 0 {
        0
    } else if (roll >> 3) & 31 == 0 {
        4
    } else {
        1
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
    embedded: bool,
    // The exact block state hit by an arrow; a later replacement releases it.
    embedded_block: Option<(i32, i32, i32, u16)>,
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
/// Check the full flight segment against a server-owned entity AABB.
/// Sampling only the end points can skip a narrow/grazing hitbox at high speed.
/// Return the first intersection parameter on a finite segment (0..=1).
/// The entry parameter lets the caller compare entity and block impacts.
fn segment_box_entry(
    from: PlayerPosition,
    to: PlayerPosition,
    min: [f64; 3],
    max: [f64; 3],
) -> Option<f64> {
    let mut entry = 0.0_f64;
    let mut exit = 1.0_f64;
    for (start, end, low, high) in [
        (from.x, to.x, min[0], max[0]),
        (from.y, to.y, min[1], max[1]),
        (from.z, to.z, min[2], max[2]),
    ] {
        if !start.is_finite()
            || !end.is_finite()
            || !low.is_finite()
            || !high.is_finite()
            || low > high
        {
            return None;
        }
        let delta = end - start;
        if delta == 0.0 {
            if start < low || start > high {
                return None;
            }
        } else {
            let t0 = (low - start) / delta;
            let t1 = (high - start) / delta;
            entry = entry.max(t0.min(t1));
            exit = exit.min(t0.max(t1));
            if entry > exit {
                return None;
            }
        }
    }
    Some(entry)
}

#[cfg(test)]
fn segment_hits_box(
    from: PlayerPosition,
    to: PlayerPosition,
    min: [f64; 3],
    max: [f64; 3],
) -> bool {
    segment_box_entry(from, to, min, max).is_some()
}

fn segment_point(from: PlayerPosition, to: PlayerPosition, t: f64) -> PlayerPosition {
    PlayerPosition {
        x: from.x + (to.x - from.x) * t,
        y: from.y + (to.y - from.y) * t,
        z: from.z + (to.z - from.z) * t,
    }
}

fn solid_projectile_block(block: u16) -> bool {
    block != 0
        && !matches!(
            block,
            6 | 8 | 9 | 10 | 11 | 31 | 32 | 37 | 38 | 39 | 40 | 50 | 78
        )
}

/// Traverse each voxel crossed by the flight segment in parametric order.
/// Sampling endpoints alone can miss a grazing wall and incorrectly shield
/// an entity that is actually in front of the wall.
fn first_solid_block_hit(
    world: &World,
    from: PlayerPosition,
    to: PlayerPosition,
) -> Option<(f64, [i32; 3])> {
    if !from.is_valid() || !to.is_valid() {
        return None;
    }
    let start = [from.x, from.y, from.z];
    let delta = [to.x - from.x, to.y - from.y, to.z - from.z];
    let mut cell = [
        from.x.floor() as i32,
        from.y.floor() as i32,
        from.z.floor() as i32,
    ];
    let end = [
        to.x.floor() as i32,
        to.y.floor() as i32,
        to.z.floor() as i32,
    ];
    let mut entry = 0.0_f64;
    // Normal projectiles travel only a few blocks per tick; bound traversal
    // even if a future caller supplies an unexpectedly large velocity.
    for _ in 0..1024 {
        let state = world.get(cell[0], cell[1], cell[2]);
        let block = state >> 4;
        if solid_projectile_block(block) {
            // Single stone/wood slabs occupy only half a voxel. Testing the
            // whole cell would incorrectly shield entities above/below them.
            // Double slabs (43/125) still use the full-block path.
            if matches!(block, 44 | 126) {
                let bottom = f64::from(cell[1]) + if state & 8 != 0 { 0.5 } else { 0.0 };
                if let Some(t) = segment_box_entry(
                    from,
                    to,
                    [f64::from(cell[0]), bottom, f64::from(cell[2])],
                    [
                        f64::from(cell[0]) + 1.0,
                        bottom + 0.5,
                        f64::from(cell[2]) + 1.0,
                    ],
                ) {
                    if t >= entry {
                        return Some((t, cell));
                    }
                }
            } else {
                return Some((entry, cell));
            }
        }
        if cell == end {
            return None;
        }
        let mut next_t = f64::INFINITY;
        let mut axis = None;
        for index in 0..3 {
            if delta[index] == 0.0 {
                continue;
            }
            let boundary = if delta[index] > 0.0 {
                f64::from(cell[index]) + 1.0
            } else {
                f64::from(cell[index])
            };
            let candidate = (boundary - start[index]) / delta[index];
            if candidate < next_t {
                next_t = candidate;
                axis = Some(index);
            }
        }
        if !next_t.is_finite() || next_t > 1.0 {
            return None;
        }
        let index = axis?;
        cell[index] += if delta[index] > 0.0 { 1 } else { -1 };
        entry = next_t.max(entry);
    }
    None
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
    pub mob_hits: Vec<(i32, u64)>,
    pub player_hits: Vec<i32>,
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
            embedded_block: None,
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
            embedded_block: None,
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
    /// Return a landed dispenser arrow in reach. Flying arrows and arrows
    /// still shaking after impact cannot be collected (1.7.10 behavior).
    /// The session checks inventory capacity before removing this entity.
    pub fn nearby_embedded_arrow(&self, feet: (f64, f64, f64)) -> Option<i32> {
        if ![feet.0, feet.1, feet.2].iter().all(|v| v.is_finite()) {
            return None;
        }
        self.active
            .values()
            .find(|arrow| {
                if arrow.kind != Kind::Arrow || !arrow.embedded || arrow.age < 7 {
                    return false;
                }
                let dx = arrow.at.x - feet.0;
                let dy = arrow.at.y - feet.1;
                let dz = arrow.at.z - feet.2;
                dx * dx + dy * dy + dz * dz <= 4.0
            })
            .map(|arrow| arrow.id)
    }

    /// Called only after the session has durably committed one arrow to the
    /// inventory. Holding the Throwables mutex across that commit prevents
    /// two sessions from collecting the same projectile.
    pub fn take_embedded_arrow(&mut self, id: i32, collector: i32) -> Option<[Event; 2]> {
        if collector <= 0
            || !self
                .active
                .get(&id)
                .is_some_and(|arrow| arrow.kind == Kind::Arrow && arrow.embedded && arrow.age >= 7)
        {
            return None;
        }
        self.active.remove(&id)?;
        let mut body = id.to_be_bytes().to_vec();
        body.extend_from_slice(&collector.to_be_bytes());
        Some([Event { id: 0x0d, body }, multiplayer::destroy(id)])
    }

    #[cfg(test)]
    pub fn tick(&mut self, world: &World, tick: u64) -> TickResult {
        self.tick_with_mob_targets(world, tick, &[])
    }

    /// Preserve the Mob-only API for existing regression tests.
    #[cfg(test)]
    pub fn tick_with_mob_targets(
        &mut self,
        world: &World,
        tick: u64,
        targets: &[(i32, PlayerPosition, f64, f64)],
    ) -> TickResult {
        self.tick_with_targets(world, tick, targets, &[])
    }

    /// Compare swept samples against server-owned Mob and player hitboxes.
    /// Solid block collision takes precedence over either entity type.
    pub fn tick_with_targets(
        &mut self,
        world: &World,
        tick: u64,
        mob_targets: &[(i32, PlayerPosition, f64, f64)],
        player_targets: &[(i32, PlayerPosition)],
    ) -> TickResult {
        let mut result = TickResult::default();
        let mut removed = Vec::new();
        for object in self.active.values_mut() {
            object.age = object.age.saturating_add(1);
            // Vanilla arrows remain in their impact block for 60 seconds.
            // They must not repeatedly collide or emit movement while embedded.
            if object.embedded {
                if object.age >= 1200 {
                    removed.push(object.id);
                    continue;
                }
                if let Some((x, y, z, original)) = object.embedded_block {
                    if world.get(x, y, z) == original {
                        continue;
                    }
                }
                // Vanilla releases an arrow when the block it hit is removed
                // or replaced. Restart flight aging so it does not expire
                // immediately after a long period embedded in the block.
                object.embedded = false;
                object.embedded_block = None;
                object.age = 0;
            }
            let previous = object.at;
            let destination = PlayerPosition {
                x: previous.x + object.velocity[0],
                y: previous.y + object.velocity[1],
                z: previous.z + object.velocity[2],
            };
            let block_hit = first_solid_block_hit(world, previous, destination);
            let block_t = block_hit.map_or(f64::INFINITY, |(t, _)| t);
            let mut mob_hit: Option<(f64, i32)> = None;
            let mut player_hit: Option<(f64, i32)> = None;
            if object.kind == Kind::Arrow {
                for (id, at, radius, height) in mob_targets {
                    if *radius < 0.0 || *height < 0.0 {
                        continue;
                    }
                    if let Some(t) = segment_box_entry(
                        previous,
                        destination,
                        [at.x - radius, at.y - 0.25, at.z - radius],
                        [at.x + radius, at.y + height + 0.25, at.z + radius],
                    ) {
                        if mob_hit.is_none_or(|(best, _)| t < best) {
                            mob_hit = Some((t, *id));
                        }
                    }
                }
                for (id, at) in player_targets {
                    if *id == object.owner_entity {
                        continue;
                    }
                    if let Some(t) = segment_box_entry(
                        previous,
                        destination,
                        [at.x - 0.3, at.y - 0.25, at.z - 0.3],
                        [at.x + 0.3, at.y + 2.05, at.z + 0.3],
                    ) {
                        if player_hit.is_none_or(|(best, _)| t < best) {
                            player_hit = Some((t, *id));
                        }
                    }
                }
            }
            let mob_t = mob_hit.map_or(f64::INFINITY, |(t, _)| t);
            if let Some((t, player_id)) = player_hit.filter(|(t, _)| *t < block_t && *t < mob_t) {
                object.at = segment_point(previous, destination, t);
                result.player_hits.push(player_id);
                removed.push(object.id);
                continue;
            }
            if let Some((t, mob_id)) = mob_hit.filter(|(t, _)| *t < block_t) {
                object.at = segment_point(previous, destination, t);
                result.mob_hits.push((mob_id, tick));
                removed.push(object.id);
                continue;
            }
            if let Some((t, block_cell)) = block_hit {
                let at = segment_point(previous, destination, t);
                // Destroy packets are broadcast to observers near this position.
                // Keep the impact point, not the previous tick's location, so
                // clients near the collision also receive the removal.
                object.at = at;
                if object.kind == Kind::Arrow {
                    object.embedded = true;
                    let [x, y, z] = block_cell;
                    object.embedded_block = Some((x, y, z, world.get(x, y, z)));
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
                    for _ in 0..egg_hatch_count(tick, object.id) {
                        result.egg_hatches.push(at);
                    }
                }
                removed.push(object.id);
            } else if object.age >= 200 || !destination.is_valid() {
                removed.push(object.id);
            } else {
                object.at = destination;
                // In vanilla 1.7.10, projectiles lose more momentum in water
                // than in air. Lava is traversable but does not use water drag.
                let block = world.get(
                    destination.x.floor() as i32,
                    destination.y.floor() as i32,
                    destination.z.floor() as i32,
                ) >> 4;
                let drag = if matches!(block, 8 | 9) { 0.8 } else { 0.99 };
                object.velocity[0] *= drag;
                object.velocity[1] = object.velocity[1] * drag - object.kind.gravity();
                object.velocity[2] *= drag;
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
    fn projectiles_slow_down_in_water_but_not_lava() {
        // Source and flowing water apply the vanilla 0.8 drag coefficient.
        // Air and lava retain the normal 0.99 coefficient.
        for (block, drag) in [(0u16, 0.99), (8, 0.8), (9, 0.8), (10, 0.99), (11, 0.99)] {
            let mut world = World::new();
            if block != 0 {
                world.import_block(2, 70, 0, block << 4).unwrap();
            }
            for item in [262, 332, 344] {
                let mut objects = Throwables::default();
                objects
                    .launch_dispenser(item, (0, 70, 0), (1, 0, 0))
                    .unwrap();
                let result = objects.tick(&world, 1);
                assert_eq!(result.events.len(), 1, "block={block} item={item}");
                let projectile = objects.active.values().next().unwrap();
                assert!((projectile.velocity[0] - 1.1 * drag).abs() < 1e-10);
                assert!((projectile.velocity[1] + projectile.kind.gravity()).abs() < 1e-10);
            }
        }
    }

    #[test]
    fn dispenser_egg_hatch_supports_zero_one_and_four_chicks_without_duplication() {
        // Frozen deterministic impact fixtures. Vanilla's rare four-chick
        // hatch is conditional on the normal 1-in-8 hatch, not a separate egg.
        let mut world = World::new();
        world.import_block(2, 70, 0, 1 << 4).unwrap();
        for (offset, expected) in [(1, 0), (0, 1), (183, 4)] {
            let mut objects = Throwables {
                next_id: offset,
                ..Throwables::default()
            };
            objects
                .launch_dispenser(344, (0, 70, 0), (1, 0, 0))
                .unwrap();
            assert_eq!(egg_hatch_count(17, START_ID + offset), expected);
            let impact = objects.tick(&world, 17);
            assert_eq!(impact.egg_hatches.len(), expected);
            assert!(impact.egg_hatches.iter().all(|at| at.x >= 2.0));
            assert_eq!(impact.events.len(), 1);
            assert_eq!(impact.events[0].1.id, 0x13);
            assert!(objects.active.is_empty());
            assert!(objects.tick(&world, 18).egg_hatches.is_empty());
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
    fn dispenser_embedded_arrow_releases_after_impact_block_is_removed() {
        let mut world = World::new();
        world.import_block(2, 70, 0, 1 << 4).unwrap();
        let mut objects = Throwables::default();
        objects
            .launch_dispenser(262, (0, 70, 0), (1, 0, 0))
            .unwrap();
        let projectile_id = *objects.active.keys().next().unwrap();

        objects.tick(&world, 1);
        let arrow = objects.active.get(&projectile_id).unwrap();
        assert!(arrow.embedded);
        assert_eq!(arrow.embedded_block, Some((2, 70, 0, 1 << 4)));
        assert!(objects.tick(&world, 2).events.is_empty());

        world.import_block(2, 70, 0, 0).unwrap();
        let released = objects.tick(&world, 3);
        let arrow = objects.active.get(&projectile_id).unwrap();
        assert!(!arrow.embedded);
        assert_eq!(arrow.embedded_block, None);
        assert_eq!(arrow.age, 0);
        assert_eq!(released.events.len(), 1);
        assert_eq!(released.events[0].1.id, 0x18);
        assert_eq!(arrow.velocity[1], -0.05);

        let y = arrow.at.y;
        objects.tick(&world, 4);
        assert!(objects.active.get(&projectile_id).unwrap().at.y < y);
    }

    #[test]
    fn dispenser_embedded_arrow_rechecks_replaced_block_state() {
        let mut world = World::new();
        world.import_block(2, 70, 0, 1 << 4).unwrap();
        let mut objects = Throwables::default();
        objects
            .launch_dispenser(262, (0, 70, 0), (1, 0, 0))
            .unwrap();
        let projectile_id = *objects.active.keys().next().unwrap();

        objects.tick(&world, 1);
        assert!(objects.active.get(&projectile_id).unwrap().embedded);
        assert!(objects.tick(&world, 2).events.is_empty());

        // Replacing stone with another solid block must invalidate the old
        // impact state without deleting the arrow or duplicating its entity.
        world.import_block(2, 70, 0, 4 << 4).unwrap();
        let replacement = objects.tick(&world, 3);
        let arrow = objects.active.get(&projectile_id).unwrap();
        assert!(arrow.embedded);
        assert_eq!(arrow.embedded_block, Some((2, 70, 0, 4 << 4)));
        assert_eq!(arrow.age, 0);
        assert_eq!(replacement.events.len(), 1);
        assert_eq!(replacement.events[0].1.id, 0x18);

        // Removing the replacement releases the same entity exactly once.
        world.import_block(2, 70, 0, 0).unwrap();
        let released = objects.tick(&world, 4);
        let arrow = objects.active.get(&projectile_id).unwrap();
        assert!(!arrow.embedded);
        assert_eq!(arrow.embedded_block, None);
        assert_eq!(arrow.age, 0);
        assert_eq!(released.events.len(), 1);
        assert_eq!(released.events[0].1.id, 0x18);
        assert_eq!(objects.active.len(), 1);
    }

    #[test]
    fn embedded_dispenser_arrow_pickup_is_delayed_and_single_use() {
        let mut world = World::new();
        world.import_block(2, 70, 0, 1 << 4).unwrap();
        let mut objects = Throwables::default();
        objects
            .launch_dispenser(262, (0, 70, 0), (1, 0, 0))
            .unwrap();
        let id = *objects.active.keys().next().unwrap();
        let feet = (2.0, 70.5, 0.5);
        assert_eq!(objects.nearby_embedded_arrow(feet), None);
        assert!(objects.take_embedded_arrow(id, 42).is_none());
        objects.tick(&world, 1);
        for tick in 2..8 {
            objects.tick(&world, tick);
            assert_eq!(objects.nearby_embedded_arrow(feet), None);
        }
        objects.tick(&world, 8);
        assert_eq!(objects.nearby_embedded_arrow(feet), Some(id));
        assert_eq!(objects.nearby_embedded_arrow((8.0, 70.5, 0.5)), None);
        assert_eq!(objects.nearby_embedded_arrow((f64::NAN, 70.5, 0.5)), None);
        assert!(objects.take_embedded_arrow(id, -1).is_none());

        let packets = objects.take_embedded_arrow(id, 42).unwrap();
        assert_eq!(packets[0].id, 0x0d);
        assert_eq!(&packets[0].body[..4], &id.to_be_bytes());
        assert_eq!(&packets[0].body[4..], &42i32.to_be_bytes());
        assert_eq!(packets[1].id, 0x13);
        assert_eq!(objects.nearby_embedded_arrow(feet), None);
        assert!(objects.take_embedded_arrow(id, 43).is_none());
        assert!(objects.tick(&world, 9).events.is_empty());
    }

    #[test]
    fn full_inventory_does_not_consume_embedded_arrow() {
        let mut world = World::new();
        world.import_block(2, 70, 0, 1 << 4).unwrap();
        let mut objects = Throwables::default();
        objects
            .launch_dispenser(262, (0, 70, 0), (1, 0, 0))
            .unwrap();
        for tick in 1..=8 {
            objects.tick(&world, tick);
        }
        let mut inventory = crate::inventory::Inventory::default();
        for slot in 9i16..45 {
            inventory
                .creative_set(
                    slot,
                    Some(crate::inventory::Stack {
                        id: 1,
                        count: 64,
                        damage: 0,
                        nbt: None,
                    }),
                )
                .unwrap();
        }
        assert!(inventory
            .receive_stack(crate::inventory::Stack {
                id: 262,
                count: 1,
                damage: 0,
                nbt: None,
            })
            .is_none());
        assert!(objects.nearby_embedded_arrow((2.0, 70.5, 0.5)).is_some());
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
    #[test]
    fn dispenser_arrow_hits_mob_once_and_never_becomes_collectible() {
        let mut objects = Throwables::default();
        let (at, _) = objects
            .launch_dispenser(262, (0, 70, 0), (1, 0, 0))
            .unwrap();
        let targets = [(
            500_001,
            PlayerPosition {
                x: 2.0,
                y: 70.0,
                z: at.z,
            },
            0.55,
            1.4,
        )];
        let world = World::new();
        let first = objects.tick_with_mob_targets(&world, 20, &targets);
        assert_eq!(first.mob_hits, vec![(500_001, 20)]);
        assert_eq!(
            first
                .events
                .iter()
                .filter(|(_, event)| event.id == 0x13)
                .count(),
            1
        );
        assert!(objects.active.is_empty());
        assert_eq!(objects.nearby_embedded_arrow((2.0, 70.0, at.z)), None);
        let second = objects.tick_with_mob_targets(&world, 20, &targets);
        assert!(second.mob_hits.is_empty());
        assert!(second.events.is_empty());
    }

    #[test]
    fn solid_block_prevents_dispenser_arrow_hitting_mob_behind_it() {
        let mut world = World::new();
        world.import_block(2, 70, 0, 1 << 4).unwrap();
        let mut objects = Throwables::default();
        objects
            .launch_dispenser(262, (0, 70, 0), (1, 0, 0))
            .unwrap();
        let targets = [(
            500_002,
            PlayerPosition {
                x: 2.5,
                y: 70.0,
                z: 0.5,
            },
            0.5,
            1.8,
        )];
        let first = objects.tick_with_mob_targets(&world, 20, &targets);
        assert!(first.mob_hits.is_empty());
        assert!(objects.active.values().next().unwrap().embedded);
    }
    #[test]
    fn dispenser_arrow_hits_only_one_of_two_players_and_is_removed_once() {
        let mut objects = Throwables::default();
        objects
            .launch_dispenser(262, (0, 70, 0), (1, 0, 0))
            .unwrap();
        let players = [
            (
                42,
                PlayerPosition {
                    x: 2.0,
                    y: 70.0,
                    z: 0.5,
                },
            ),
            (
                43,
                PlayerPosition {
                    x: 2.0,
                    y: 70.0,
                    z: 0.5,
                },
            ),
        ];
        let world = World::new();
        let hit = objects.tick_with_targets(&world, 20, &[], &players);
        assert_eq!(hit.player_hits, vec![42]);
        assert_eq!(
            hit.events
                .iter()
                .filter(|(_, event)| event.id == 0x13)
                .count(),
            1
        );
        assert!(objects.active.is_empty());
        assert_eq!(objects.nearby_embedded_arrow((2.0, 70.0, 0.5)), None);
        let replay = objects.tick_with_targets(&world, 20, &[], &players);
        assert!(replay.player_hits.is_empty());
        assert!(replay.events.is_empty());
    }

    #[test]
    fn solid_block_shields_player_from_dispenser_arrow() {
        let mut world = World::new();
        world.import_block(2, 70, 0, 1 << 4).unwrap();
        let mut objects = Throwables::default();
        objects
            .launch_dispenser(262, (0, 70, 0), (1, 0, 0))
            .unwrap();
        let players = [(
            42,
            PlayerPosition {
                x: 2.5,
                y: 70.0,
                z: 0.5,
            },
        )];
        let impact = objects.tick_with_targets(&world, 20, &[], &players);
        assert!(impact.player_hits.is_empty());
        assert!(objects.active.values().next().unwrap().embedded);
    }
    #[test]
    fn dispenser_arrow_hits_narrow_mob_between_old_discrete_samples() {
        // The first tick travels from x=1.2 to x=2.3. Old 0.22-block
        // point samples skipped a 0.04-radius entity centered at x=1.53.
        let mut objects = Throwables::default();
        objects
            .launch_dispenser(262, (0, 70, 0), (1, 0, 0))
            .unwrap();
        let targets = [(
            500_099,
            PlayerPosition {
                x: 1.53,
                y: 70.0,
                z: 0.5,
            },
            0.04,
            1.0,
        )];
        let world = World::new();
        let first = objects.tick_with_mob_targets(&world, 20, &targets);
        assert_eq!(first.mob_hits, vec![(500_099, 20)]);
        assert!(objects.active.is_empty());
        assert!(objects
            .tick_with_mob_targets(&world, 21, &targets)
            .mob_hits
            .is_empty());
    }

    #[test]
    fn swept_hitbox_rejects_parallel_misses_and_invalid_bounds() {
        let from = PlayerPosition {
            x: 0.0,
            y: 70.0,
            z: 0.0,
        };
        let to = PlayerPosition {
            x: 2.0,
            y: 70.0,
            z: 0.0,
        };
        assert!(segment_hits_box(
            from,
            to,
            [0.9, 69.9, -0.1],
            [1.1, 70.1, 0.1]
        ));
        assert!(!segment_hits_box(
            from,
            to,
            [0.9, 69.9, 0.1],
            [1.1, 70.1, 0.2]
        ));
        assert!(!segment_hits_box(
            from,
            to,
            [0.9, 69.9, -0.1],
            [1.1, 69.99, 0.1]
        ));
        assert!(!segment_hits_box(
            from,
            to,
            [f64::NAN, 69.9, -0.1],
            [1.1, 70.1, 0.1]
        ));
    }
    #[test]
    fn arrow_hits_mob_in_front_of_wall_even_within_same_old_sample() {
        let mut world = World::new();
        world.import_block(2, 70, 0, 1 << 4).unwrap();
        let mut objects = Throwables::default();
        objects
            .launch_dispenser(262, (0, 70, 0), (1, 0, 0))
            .unwrap();
        // The old 0.22-block sampling saw stone at x=2.08 before checking
        // the segment from x=1.86, skipping this mob at x=1.94.
        let targets = [(
            500_123,
            PlayerPosition {
                x: 1.94,
                y: 70.0,
                z: 0.5,
            },
            0.03,
            1.0,
        )];
        let hit = objects.tick_with_mob_targets(&world, 20, &targets);
        assert_eq!(hit.mob_hits, vec![(500_123, 20)]);
        assert!(objects.active.is_empty());
    }

    #[test]
    fn arrow_hits_player_before_wall_but_not_player_behind_it() {
        let mut world = World::new();
        world.import_block(2, 70, 0, 1 << 4).unwrap();
        for (player_x, should_hit) in [(2.2, true), (2.6, false)] {
            let mut objects = Throwables::default();
            objects
                .launch_dispenser(262, (0, 70, 0), (1, 0, 0))
                .unwrap();
            let players = [(
                42,
                PlayerPosition {
                    x: player_x,
                    y: 70.0,
                    z: 0.5,
                },
            )];
            let hit = objects.tick_with_targets(&world, 20, &[], &players);
            assert_eq!(!hit.player_hits.is_empty(), should_hit);
            if should_hit {
                assert!(objects.active.is_empty());
            } else {
                assert!(objects.active.values().next().unwrap().embedded);
            }
        }
    }

    #[test]
    fn first_wall_intersection_uses_exact_voxel_and_negative_direction() {
        let mut world = World::new();
        world.import_block(1, 70, 0, 1 << 4).unwrap();
        let from = PlayerPosition {
            x: 2.8,
            y: 70.5,
            z: 0.5,
        };
        let to = PlayerPosition {
            x: 0.8,
            y: 70.5,
            z: 0.5,
        };
        let (t, cell) = first_solid_block_hit(&world, from, to).unwrap();
        assert_eq!(cell, [1, 70, 0]);
        assert!((t - 0.4).abs() < 1e-10);
        assert_eq!(first_solid_block_hit(&World::new(), from, to), None);
    }
    #[test]
    fn projectile_ray_respects_single_slab_half_and_full_block_behind() {
        // 1.7.10 single stone/wood slabs use metadata bit 3 for the upper half.
        for slab in [44u16, 126] {
            for upper in [false, true] {
                let mut world = World::new();
                let state = (slab << 4) | if upper { 8 } else { 0 };
                world.import_block(1, 70, 0, state).unwrap();
                let from = PlayerPosition {
                    x: 0.2,
                    y: if upper { 70.25 } else { 70.75 },
                    z: 0.5,
                };
                let to = PlayerPosition { x: 3.2, ..from };
                assert_eq!(
                    first_solid_block_hit(&world, from, to),
                    None,
                    "ray through empty slab half: block {slab}, upper {upper}"
                );
                // A full cube behind the empty half must still be detected.
                world.import_block(2, 70, 0, 1 << 4).unwrap();
                let (t, cell) = first_solid_block_hit(&world, from, to).unwrap();
                assert_eq!(cell, [2, 70, 0]);
                assert!((t - 0.6).abs() < 1e-10);
                // A ray through the occupied half hits the slab first.
                let hit_from = PlayerPosition {
                    y: if upper { 70.75 } else { 70.25 },
                    ..from
                };
                let hit_to = PlayerPosition { x: 3.2, ..hit_from };
                let (t, cell) = first_solid_block_hit(&world, hit_from, hit_to).unwrap();
                assert_eq!(cell, [1, 70, 0]);
                assert!((t - (0.8 / 3.0)).abs() < 1e-10);
                // Reversing the ray also respects the half-height shape.
                let (t, cell) = first_solid_block_hit(&world, hit_to, hit_from).unwrap();
                assert_eq!(cell, [2, 70, 0]);
                assert!((t - (0.2 / 3.0)).abs() < 1e-10);
            }
        }
    }

    #[test]
    fn projectile_ray_treats_double_slabs_as_full_blocks() {
        for slab in [43u16, 125] {
            let mut world = World::new();
            world.import_block(1, 70, 0, slab << 4).unwrap();
            for y in [70.25, 70.75] {
                let from = PlayerPosition { x: 0.2, y, z: 0.5 };
                let to = PlayerPosition { x: 2.2, ..from };
                assert_eq!(
                    first_solid_block_hit(&world, from, to).unwrap().1,
                    [1, 70, 0]
                );
            }
        }
    }
}
