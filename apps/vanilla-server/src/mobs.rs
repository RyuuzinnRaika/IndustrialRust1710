//! Server-owned 1.7.10 living entities, capped natural spawning and combat.
//! Lighting is an explicit sky-visibility approximation until full light maps exist.
use crate::{
    inventory::Stack,
    multiplayer::{self, Event},
    navigation, physics,
    player::PlayerPosition,
    terrain,
    world::World,
};
use legacy_protocol::write_varint;
use std::collections::BTreeMap;

const BASE_ID: i32 = 500_000;
const MAX_STEPS: u64 = 4;
const MAX_HOSTILE: usize = 32;
const MAX_PASSIVE: usize = 24;
const MAX_ARROWS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Species {
    Zombie,
    Cow,
    Pig,
    Sheep,
    Chicken,
    Skeleton,
    Creeper,
    Spider,
}
impl Species {
    fn kind(self) -> u8 {
        match self {
            Self::Zombie => 54,
            Self::Cow => 92,
            Self::Pig => 90,
            Self::Sheep => 91,
            Self::Chicken => 93,
            Self::Skeleton => 51,
            Self::Creeper => 50,
            Self::Spider => 52,
        }
    }
    fn hostile(self) -> bool {
        matches!(
            self,
            Self::Zombie | Self::Skeleton | Self::Creeper | Self::Spider
        )
    }
    fn max_health(self) -> i16 {
        match self {
            Self::Cow | Self::Pig | Self::Sheep => 10,
            Self::Chicken => 4,
            Self::Spider => 16,
            _ => 20,
        }
    }
    fn drop(self) -> Stack {
        let id = match self {
            Self::Zombie => 367,
            Self::Cow => 363,
            Self::Pig => 319,
            Self::Sheep => 35,
            Self::Chicken => 365,
            Self::Skeleton => 352,
            Self::Creeper => 289,
            Self::Spider => 287,
        };
        Stack {
            id,
            count: 1,
            damage: 0,
        }
    }
    fn breeding_food(self) -> i16 {
        match self {
            Self::Cow | Self::Sheep => 296,
            Self::Pig => 391,
            Self::Chicken => 295,
            _ => -1,
        }
    }
}
#[derive(Clone, Copy, Debug)]
struct Mob {
    id: i32,
    species: Species,
    position: PlayerPosition,
    health: i16,
    hit_tick: u64,
    bite_tick: u64,
    born: u64,
    fuse_start: Option<u64>,
    love_until: u64,
    breeding_cooldown: u64,
}
impl Mob {
    fn new(id: i32, species: Species, position: PlayerPosition, tick: u64) -> Self {
        Self {
            id,
            species,
            position,
            health: species.max_health(),
            hit_tick: 0,
            bite_tick: 0,
            born: tick,
            fuse_start: None,
            love_until: 0,
            breeding_cooldown: 0,
        }
    }
    fn spawn(self) -> Event {
        let mut body = Vec::with_capacity(32);
        write_varint(self.id as u32, &mut body);
        body.push(self.species.kind());
        for value in [self.position.x, self.position.y, self.position.z] {
            body.extend_from_slice(&((value * 32.0).floor() as i32).to_be_bytes());
        }
        body.extend_from_slice(&[0, 0, 0]); // yaw/pitch/head pitch
        for _ in 0..3 {
            body.extend_from_slice(&0i16.to_be_bytes());
        }
        body.extend_from_slice(&[0, 0, 0x7f]); // flags byte, metadata sentinel
        Event { id: 0x0f, body }
    }
    fn status(self, value: u8) -> Event {
        let mut body = self.id.to_be_bytes().to_vec();
        body.push(value);
        Event { id: 0x1a, body }
    }
    fn nearby(self, position: PlayerPosition, radius: f64) -> bool {
        distance_squared(self.position, position) <= radius * radius
    }
    fn at(self) -> (i32, i32, i32) {
        (
            self.position.x.floor() as i32,
            self.position.y.floor() as i32,
            self.position.z.floor() as i32,
        )
    }
}
fn distance_squared(a: PlayerPosition, b: PlayerPosition) -> f64 {
    (a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)
}
#[derive(Debug)]
pub struct Attack {
    pub events: Vec<Event>,
    pub drop: Option<(Stack, (i32, i32, i32))>,
    pub xp: u16,
}
#[derive(Clone, Copy, Debug)]
struct Arrow {
    id: i32,
    origin: i32,
    from_player: bool,
    position: PlayerPosition,
    velocity: (f64, f64, f64),
    damage: i16,
    lifetime: u16,
}
impl Arrow {
    fn spawn(self) -> Event {
        let mut body = Vec::with_capacity(33);
        write_varint(self.id as u32, &mut body);
        body.push(60); // Arrow object in Protocol 5
        for value in [self.position.x, self.position.y, self.position.z] {
            body.extend_from_slice(&((value * 32.0).floor() as i32).to_be_bytes());
        }
        body.extend_from_slice(&[0, 0]); // pitch, yaw
        body.extend_from_slice(&self.origin.to_be_bytes());
        for velocity in [self.velocity.0, self.velocity.1, self.velocity.2] {
            body.extend_from_slice(
                &((velocity * 8000.0).clamp(-32768.0, 32767.0) as i16).to_be_bytes(),
            );
        }
        Event { id: 0x0e, body }
    }
}
#[derive(Clone, Debug)]
struct PendingHit {
    center: PlayerPosition,
    radius: f64,
    damage: u8,
    expires: u64,
    victims: Vec<i32>,
}
#[derive(Debug)]
pub struct Mobs {
    entities: BTreeMap<i32, Mob>,
    arrows: BTreeMap<i32, Arrow>,
    hits: Vec<PendingHit>,
    drops: Vec<(Stack, (i32, i32, i32), u16)>,
    melee_cooldowns: BTreeMap<(i32, i32), u64>,
    explosions: Vec<(i32, i32, i32)>,
    next_id: i32,
    random: u64,
    last_tick: u64,
    enabled: bool,
}
impl Default for Mobs {
    fn default() -> Self {
        Self {
            entities: BTreeMap::new(),
            arrows: BTreeMap::new(),
            hits: Vec::new(),
            drops: Vec::new(),
            melee_cooldowns: BTreeMap::new(),
            explosions: Vec::new(),
            next_id: BASE_ID,
            random: 1,
            last_tick: 0,
            enabled: false,
        }
    }
}
impl Mobs {
    pub fn new(world: &World, enabled: bool) -> Self {
        let mut state = Self {
            enabled: enabled && world.is_natural(),
            random: world.natural_seed().unwrap_or(1) | 1,
            ..Self::default()
        };
        // Retain legacy starter zombies for existing protocol regression tests.
        if state.enabled {
            for (x, z) in [(8, 9), (-8, 7), (7, -8), (-9, -7)] {
                let y = terrain::surface(world.natural_seed().unwrap_or(1), x, z) + 1;
                let at = PlayerPosition {
                    x: x as f64 + 0.5,
                    y: y as f64,
                    z: z as f64 + 0.5,
                };
                if !physics::collides(world, &at) && physics::supported(world, &at) {
                    state.add(Species::Zombie, at, 0);
                }
            }
        }
        state
    }
    fn add(&mut self, species: Species, position: PlayerPosition, tick: u64) -> Event {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let mob = Mob::new(id, species, position, tick);
        self.entities.insert(id, mob);
        mob.spawn()
    }
    fn rng(&mut self) -> u64 {
        let mut x = self.random;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.random = x;
        x
    }
    pub fn spawn_events(&self) -> impl Iterator<Item = Event> + '_ {
        self.entities
            .values()
            .copied()
            .map(Mob::spawn)
            .chain(self.arrows.values().copied().map(Arrow::spawn))
    }
    /// Surface skylight proxy, with an actual unbroken vertical column.
    /// Does not replace vanilla propagated skylight through translucent blocks.
    fn sky_open(world: &World, x: i32, y: i32, z: i32) -> bool {
        ((y + 1)..=255)
            .all(|height| matches!(world.get(x, height, z) >> 4, 0 | 8 | 9 | 18 | 20 | 65 | 78))
    }
    /// Bounded block-light proxy near common 1.7.10 light emitters.
    /// Conservative: reduces undesired spawning around torches, furnaces and lava.
    fn lit_by_block(world: &World, x: i32, y: i32, z: i32) -> bool {
        for dx in -7_i32..=7 {
            for dz in -7_i32..=7 {
                for dy in -3_i32..=3 {
                    let distance = dx.abs() + dz.abs() + dy.abs();
                    if distance >= 14 {
                        continue;
                    }
                    let light = match world.get(x + dx, y + dy, z + dz) >> 4 {
                        50 | 76 | 62 => 14,
                        89 | 91 | 10 | 11 => 15,
                        51 | 90 => 11,
                        74 => 9,
                        _ => 0,
                    };
                    if light > distance + 7 {
                        return true;
                    }
                }
            }
        }
        false
    }
    fn spawn_near(
        &mut self,
        tick: u64,
        day_time: i64,
        world: &World,
        players: &[PlayerPosition],
        events: &mut Vec<Event>,
    ) {
        if !self.enabled || players.is_empty() || !tick.is_multiple_of(20) {
            return;
        }
        let hostile_count = self
            .entities
            .values()
            .filter(|m| m.species.hostile())
            .count();
        let passive_count = self.entities.len() - hostile_count;
        if hostile_count >= MAX_HOSTILE && passive_count >= MAX_PASSIVE {
            return;
        }
        // 12 candidates per second, never brute-force chunks or per-player worlds.
        for attempt in 0..12 {
            let base = players[(self.rng() as usize) % players.len()];
            let angle = (self.rng() % 6283) as f64 / 1000.0;
            let distance = 24.0 + (self.rng() % 41) as f64;
            let x = (base.x + angle.cos() * distance).floor() as i32;
            let z = (base.z + angle.sin() * distance).floor() as i32;
            if !crate::world::in_bounds(x, 64, z) {
                continue;
            }
            let Some(seed) = world.natural_seed() else {
                continue;
            };
            let surface = terrain::surface(seed, x, z) + 1;
            // Roughly half of the hostile attempts probe caves below the surface.
            // All attempts still enforce AABB, floor, proximity and light checks.
            let y = if attempt % 3 == 1 {
                surface - 2 - (self.rng() % 30) as i32
            } else {
                surface
            };
            if !(2..254).contains(&y) {
                continue;
            }
            let at = PlayerPosition {
                x: x as f64 + 0.5,
                y: y as f64,
                z: z as f64 + 0.5,
            };
            if !physics::supported(world, &at) || physics::collides(world, &at) {
                continue;
            }
            if players
                .iter()
                .any(|p| distance_squared(*p, at) < 24.0_f64.powi(2))
            {
                continue;
            }
            if self.entities.values().any(|m| m.nearby(at, 3.0)) {
                continue;
            }
            let open = Self::sky_open(world, x, y, z);
            let daytime = (0..12000).contains(&(day_time.rem_euclid(24000)));
            let passive = attempt % 3 == 0;
            if passive {
                if self
                    .entities
                    .values()
                    .filter(|m| !m.species.hostile())
                    .count()
                    >= MAX_PASSIVE
                    || y != surface
                    || world.get(x, y - 1, z) >> 4 != 2
                    || !open
                    || !daytime
                {
                    continue;
                }
                let species = match self.rng() % 4 {
                    0 => Species::Cow,
                    1 => Species::Pig,
                    2 => Species::Sheep,
                    _ => Species::Chicken,
                };
                events.push(self.add(species, at, tick));
            } else {
                if self
                    .entities
                    .values()
                    .filter(|m| m.species.hostile())
                    .count()
                    >= MAX_HOSTILE
                    || (open && daytime)
                    || Self::lit_by_block(world, x, y, z)
                {
                    continue;
                }
                let species = match self.rng() % 4 {
                    0 => Species::Zombie,
                    1 => Species::Skeleton,
                    2 => Species::Creeper,
                    _ => Species::Spider,
                };
                events.push(self.add(species, at, tick));
            }
        }
    }
    pub fn tick_with_day(
        &mut self,
        now: u64,
        day_time: i64,
        world: &World,
        players: &[PlayerPosition],
    ) -> Vec<Event> {
        let steps = now.saturating_sub(self.last_tick).min(MAX_STEPS);
        self.last_tick = now;
        let mut events = Vec::new();
        for tick in (now - steps + 1)..=now {
            self.hits.retain(|hit| tick <= hit.expires);
            self.spawn_near(tick, day_time, world, players, &mut events);
            let mut remove = Vec::new();
            let mut shots = Vec::new();
            let mut navigation_budget = 8;
            let mut blasts = Vec::new();
            for mob in self.entities.values_mut() {
                if players.is_empty() {
                    continue;
                }
                let near = players
                    .iter()
                    .copied()
                    .min_by(|a, b| {
                        distance_squared(*a, mob.position)
                            .total_cmp(&distance_squared(*b, mob.position))
                    })
                    .expect("checked nonempty");
                let sq = distance_squared(near, mob.position);
                if sq > 128.0 * 128.0
                    || (sq > 32.0 * 32.0
                        && tick.saturating_sub(mob.born) > 600
                        && (tick + mob.id as u64).is_multiple_of(400))
                {
                    remove.push(mob.id);
                    continue;
                }
                if mob.species == Species::Creeper {
                    if sq <= 3.0 * 3.0 {
                        let began = *mob.fuse_start.get_or_insert(tick);
                        if tick.saturating_sub(began) >= 30 {
                            blasts.push(mob.position);
                            remove.push(mob.id);
                            continue;
                        }
                    } else {
                        mob.fuse_start = None;
                    }
                }
                if mob.species == Species::Skeleton
                    && (3.0 * 3.0..=15.0 * 15.0).contains(&sq)
                    && tick.saturating_sub(mob.bite_tick) >= 40
                {
                    mob.bite_tick = tick;
                    shots.push((mob.id, mob.position, near));
                }
                let target = if mob.species.hostile() {
                    Some(near)
                } else if tick % 80 < 40 {
                    // gentle deterministic animal wandering
                    let offset = if (mob.id as u64 / 3 + tick / 80).is_multiple_of(2) {
                        1.5
                    } else {
                        -1.5
                    };
                    Some(PlayerPosition {
                        x: mob.position.x + offset,
                        y: mob.position.y,
                        z: mob.position.z,
                    })
                } else {
                    None
                };
                let Some(target) = target else {
                    continue;
                };
                if mob.species.hostile() && sq > 16.0 * 16.0 {
                    continue;
                }
                if mob.species == Species::Skeleton && sq < 8.0 * 8.0 {
                    continue;
                }
                let dx = target.x - mob.position.x;
                let dz = target.z - mob.position.z;
                let length = dx.hypot(dz);
                if length < 1.5 {
                    continue;
                }
                let speed = match mob.species {
                    Species::Spider => 0.16,
                    Species::Chicken => 0.06,
                    _ => 0.11,
                };
                let mut candidate = PlayerPosition {
                    x: mob.position.x + dx / length * speed,
                    y: mob.position.y,
                    z: mob.position.z + dz / length * speed,
                };
                let mut valid = physics::valid_movement(world, &mob.position, &candidate)
                    && physics::supported(world, &candidate);
                if !valid && navigation_budget > 0 && tick % 5 == 0 {
                    navigation_budget -= 1;
                    if let Some(waypoint) = navigation::next_step(
                        world,
                        mob.position,
                        target,
                        mob.species == Species::Spider,
                    ) {
                        let dx = waypoint.x - mob.position.x;
                        let dz = waypoint.z - mob.position.z;
                        let len = dx.hypot(dz);
                        if len > 0.01 {
                            candidate = PlayerPosition {
                                x: mob.position.x + dx / len * speed,
                                y: if (waypoint.y - mob.position.y).abs() > 0.1 {
                                    waypoint.y
                                } else {
                                    mob.position.y
                                },
                                z: mob.position.z + dz / len * speed,
                            };
                            valid = physics::valid_movement(world, &mob.position, &candidate)
                                && (physics::supported(world, &candidate)
                                    || mob.species == Species::Spider);
                        }
                    }
                }
                if valid {
                    mob.position = candidate;
                    events.push(multiplayer::teleport(mob.id, &candidate, 0.0, 0.0));
                }
            }
            for id in remove {
                self.melee_cooldowns.retain(|(entity, _), _| *entity != id);
                if self.entities.remove(&id).is_some() {
                    events.push(multiplayer::destroy(id));
                }
            }
            for center in blasts {
                self.explosions.push((
                    center.x.floor() as i32,
                    center.y.floor() as i32,
                    center.z.floor() as i32,
                ));
                self.hits.push(PendingHit {
                    center,
                    radius: 3.0,
                    damage: 8,
                    expires: tick + 4,
                    victims: Vec::new(),
                });
            }
            for (origin, from, target) in shots {
                if self.arrows.len() < MAX_ARROWS {
                    events.push(self.launch(origin, from, target, false, 3, 1.0));
                }
            }
            self.step_arrows(tick, world, players, &mut events);
        }
        events
    }
    fn launch(
        &mut self,
        origin: i32,
        from: PlayerPosition,
        target: PlayerPosition,
        from_player: bool,
        damage: i16,
        speed: f64,
    ) -> Event {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let aim = (
            target.x - from.x,
            target.y + 1.0 - (from.y + 1.2),
            target.z - from.z,
        );
        let length = (aim.0 * aim.0 + aim.1 * aim.1 + aim.2 * aim.2)
            .sqrt()
            .max(0.001);
        let arrow = Arrow {
            id,
            origin,
            from_player,
            position: PlayerPosition {
                y: from.y + 1.2,
                ..from
            },
            velocity: (
                aim.0 / length * speed,
                aim.1 / length * speed,
                aim.2 / length * speed,
            ),
            damage,
            lifetime: 0,
        };
        self.arrows.insert(id, arrow);
        arrow.spawn()
    }
    /// Only the server computes arrow direction, velocity and charge.
    pub fn player_shoot(
        &mut self,
        player_id: i32,
        from: PlayerPosition,
        yaw: f32,
        pitch: f32,
        charge_ticks: u64,
    ) -> Option<Event> {
        if self.arrows.len() >= MAX_ARROWS
            || !yaw.is_finite()
            || !pitch.is_finite()
            || charge_ticks < 3
        {
            return None;
        }
        let strength = (charge_ticks.min(20) as f64 / 20.0).powf(0.75);
        let a = (yaw as f64).to_radians();
        let p = (pitch as f64).to_radians();
        let speed = 0.25 + strength * 1.2;
        let direction = (-a.sin() * p.cos(), -p.sin(), a.cos() * p.cos());
        let target = PlayerPosition {
            x: from.x + direction.0 * 10.0,
            y: from.y + direction.1 * 10.0 + 0.2,
            z: from.z + direction.2 * 10.0,
        };
        Some(self.launch(
            player_id,
            from,
            target,
            true,
            (1.0 + 4.0 * strength).ceil() as i16,
            speed,
        ))
    }
    fn step_arrows(
        &mut self,
        tick: u64,
        world: &World,
        players: &[PlayerPosition],
        events: &mut Vec<Event>,
    ) {
        let mut remove = Vec::new();
        let mut killed = Vec::new();
        for (&id, arrow) in self.arrows.iter_mut() {
            arrow.lifetime = arrow.lifetime.saturating_add(1);
            if arrow.lifetime > 200 {
                remove.push(id);
                continue;
            }
            let previous = arrow.position;
            arrow.position.x += arrow.velocity.0;
            arrow.position.y += arrow.velocity.1;
            arrow.position.z += arrow.velocity.2;
            arrow.velocity.0 *= 0.99;
            arrow.velocity.1 = arrow.velocity.1 * 0.99 - 0.03;
            arrow.velocity.2 *= 0.99;
            let mut collided = false;
            for sample in 1..=4 {
                let part = sample as f64 / 4.0;
                let at = PlayerPosition {
                    x: previous.x + (arrow.position.x - previous.x) * part,
                    y: previous.y + (arrow.position.y - previous.y) * part,
                    z: previous.z + (arrow.position.z - previous.z) * part,
                };
                if at.y < 0.0 || at.y > 254.0 || physics::projectile_collides(world, &at) {
                    collided = true;
                    break;
                }
                if arrow.from_player {
                    for mob in self.entities.values_mut() {
                        if mob.id != arrow.origin && distance_squared(at, mob.position) < 1.5 {
                            mob.health -= arrow.damage;
                            events.push(mob.status(if mob.health <= 0 { 3 } else { 2 }));
                            if mob.health <= 0 {
                                killed.push(mob.id);
                            }
                            collided = true;
                            break;
                        }
                    }
                } else if players.iter().any(|pos| {
                    (at.x - pos.x).abs() < 0.55
                        && (at.z - pos.z).abs() < 0.55
                        && at.y >= pos.y
                        && at.y <= pos.y + 1.8
                }) {
                    self.hits.push(PendingHit {
                        center: at,
                        radius: 0.9,
                        damage: arrow.damage.max(1) as u8,
                        expires: tick + 4,
                        victims: Vec::new(),
                    });
                    collided = true;
                }
                if collided {
                    break;
                }
            }
            if collided {
                remove.push(id);
            } else {
                events.push(multiplayer::teleport(id, &arrow.position, 0.0, 0.0));
            }
        }
        for id in remove {
            self.arrows.remove(&id);
            events.push(multiplayer::destroy(id));
        }
        for id in killed {
            if let Some(mob) = self.entities.remove(&id) {
                events.push(multiplayer::destroy(id));
                self.drops.push((mob.species.drop(), mob.at(), 5));
            }
        }
    }
    pub fn player_attack(
        &mut self,
        id: i32,
        at: PlayerPosition,
        held: Option<Stack>,
        tick: u64,
    ) -> Option<Attack> {
        let mob = self.entities.get_mut(&id)?;
        if !mob.nearby(at, 3.0) || (mob.hit_tick != 0 && tick.saturating_sub(mob.hit_tick) < 10) {
            return None;
        }
        mob.hit_tick = tick;
        let damage = match held.map(|item| item.id) {
            Some(268) | Some(283) => 4,
            Some(272) => 5,
            Some(267) => 6,
            Some(276) => 7,
            _ => 1,
        };
        mob.health -= damage;
        let killed = mob.health <= 0;
        let outcome = Attack {
            events: if killed {
                vec![mob.status(3), multiplayer::destroy(id)]
            } else {
                vec![mob.status(2)]
            },
            drop: killed.then_some((mob.species.drop(), mob.at())),
            xp: if killed { 5 } else { 0 },
        };
        if killed {
            self.entities.remove(&id);
        }
        Some(outcome)
    }
    pub fn damage_player(&mut self, player_id: i32, player: PlayerPosition, tick: u64) -> u8 {
        let mut amount = 0_u8;
        for hit in &mut self.hits {
            if tick <= hit.expires
                && distance_squared(player, hit.center) <= hit.radius * hit.radius
                && !hit.victims.contains(&player_id)
            {
                hit.victims.push(player_id);
                amount = amount.saturating_add(hit.damage);
            }
        }
        for mob in self.entities.values_mut() {
            if mob.species != Species::Zombie && mob.species != Species::Spider {
                continue;
            }
            let key = (mob.id, player_id);
            if mob.nearby(player, 1.6)
                && self
                    .melee_cooldowns
                    .get(&key)
                    .is_none_or(|last| tick.saturating_sub(*last) >= 20)
            {
                self.melee_cooldowns.insert(key, tick);
                amount = amount.saturating_add(if mob.species == Species::Spider { 2 } else { 3 });
            }
        }
        amount.min(20)
    }
    pub fn take_explosions(&mut self) -> Vec<(i32, i32, i32)> {
        std::mem::take(&mut self.explosions)
    }
    pub fn take_drops(&mut self) -> Vec<(Stack, (i32, i32, i32), u16)> {
        std::mem::take(&mut self.drops)
    }
    /// Right click with wheat, carrot or seeds; matching nearby parents breed.
    pub fn feed_animal(&mut self, id: i32, item: i16, tick: u64) -> Option<Vec<Event>> {
        let first = *self.entities.get(&id)?;
        if first.species.breeding_food() != item || tick < first.breeding_cooldown {
            return None;
        }
        let mate = self
            .entities
            .values()
            .find(|m| {
                m.id != id
                    && m.species == first.species
                    && m.love_until > tick
                    && tick >= m.breeding_cooldown
                    && m.nearby(first.position, 8.0)
            })
            .map(|m| m.id);
        if let Some(mob) = self.entities.get_mut(&id) {
            mob.love_until = tick + 600;
        }
        let Some(mate_id) = mate else {
            return Some(Vec::new());
        };
        if let Some(mob) = self.entities.get_mut(&id) {
            mob.love_until = 0;
            mob.breeding_cooldown = tick + 6000;
        }
        if let Some(mob) = self.entities.get_mut(&mate_id) {
            mob.love_until = 0;
            mob.breeding_cooldown = tick + 6000;
        }
        let baby = PlayerPosition {
            x: first.position.x + 0.4,
            ..first.position
        };
        Some(vec![self.add(first.species, baby, tick)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn natural() -> World {
        World::natural(42)
    }
    #[test]
    fn protocol5_zombie_spawn_attack_and_rotten_flesh() {
        let world = natural();
        let mut mobs = Mobs::new(&world, true);
        assert!(!mobs.entities.is_empty());
        let mob = *mobs.entities.values().next().unwrap();
        let pkt = mob.spawn();
        assert_eq!(pkt.id, 0x0f);
        assert_eq!(pkt.body[3], 54);
        assert!(pkt.body.ends_with(&[0, 0, 0x7f]));
        assert!(mobs
            .player_attack(mob.id, PlayerPosition::default(), None, 100)
            .is_none());
        for t in [100, 111] {
            let hit = mobs
                .player_attack(
                    mob.id,
                    mob.position,
                    Some(Stack {
                        id: 276,
                        count: 1,
                        damage: 0,
                    }),
                    t,
                )
                .unwrap();
            assert!(hit.drop.is_none());
        }
        assert_eq!(
            mobs.player_attack(
                mob.id,
                mob.position,
                Some(Stack {
                    id: 276,
                    count: 1,
                    damage: 0
                }),
                122
            )
            .unwrap()
            .drop
            .unwrap()
            .0
            .id,
            367
        );
    }
    #[test]
    fn natural_spawns_respect_torches_and_real_sky_visibility() {
        let mut world = World::new();
        assert!(Mobs::sky_open(&world, 2, 65, 2));
        world.import_block(2, 100, 2, 1 << 4).unwrap();
        assert!(!Mobs::sky_open(&world, 2, 65, 2));
        assert!(!Mobs::lit_by_block(&world, 2, 65, 2));
        world.import_block(3, 66, 2, 50 << 4).unwrap();
        assert!(Mobs::lit_by_block(&world, 2, 65, 2));
    }
    #[test]
    fn each_player_gets_independent_hit_tracking() {
        let mut mobs = Mobs::default();
        let pos = PlayerPosition::default();
        mobs.hits.push(PendingHit {
            center: pos,
            radius: 3.0,
            damage: 4,
            expires: 100,
            victims: Vec::new(),
        });
        assert_eq!(mobs.damage_player(1, pos, 95), 4);
        assert_eq!(mobs.damage_player(2, pos, 95), 4);
        assert_eq!(mobs.damage_player(1, pos, 95), 0);
    }
    #[test]
    fn cap_safeguards_and_flat_worlds() {
        assert!(Mobs::new(&World::new(), true).entities.is_empty());
        assert!(Mobs::new(&natural(), false).entities.is_empty());
        let mut mobs = Mobs::new(&natural(), true);
        let pos = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        for tick in 1..=1200 {
            mobs.tick_with_day(tick, 18000, &natural(), &[pos]);
        }
        assert!(
            mobs.entities
                .values()
                .filter(|m| m.species.hostile())
                .count()
                <= MAX_HOSTILE
        );
        assert!(
            mobs.entities
                .values()
                .filter(|m| !m.species.hostile())
                .count()
                <= MAX_PASSIVE
        );
    }
    #[test]
    fn arrows_limit_and_charge_are_authoritative() {
        let mut mobs = Mobs::default();
        let from = PlayerPosition::default();
        assert!(mobs.player_shoot(4, from, 0.0, 0.0, 1).is_none());
        assert_eq!(mobs.player_shoot(4, from, 0.0, 0.0, 20).unwrap().id, 0x0e);
    }
    #[test]
    fn passive_animals_accept_only_correct_breeding_food() {
        let mut mobs = Mobs::default();
        let p = PlayerPosition::default();
        let first = mobs.add(Species::Cow, p, 1);
        let first_id = mobs.next_id - 1;
        let _ = first;
        mobs.add(Species::Cow, PlayerPosition { x: p.x + 2.0, ..p }, 1);
        let mate_id = mobs.next_id - 1;
        assert!(mobs.feed_animal(first_id, 391, 100).is_none());
        assert!(mobs.feed_animal(first_id, 296, 100).unwrap().is_empty());
        assert_eq!(mobs.feed_animal(mate_id, 296, 100).unwrap().len(), 1);
    }
}
