//! Bounded Protocol 5 natural-survival entities. No external entity registry.
use crate::inventory::Stack;
use crate::multiplayer::{self, Event};
use crate::mob_rules::{self, SpawnCategory, SpawnCheck};
use crate::navigation;
use crate::physics;
use crate::player::PlayerPosition;
use crate::world::World;
use legacy_protocol::write_varint;
use std::collections::BTreeMap;

const BASE_ID: i32 = 500_000;
const MAX_STEPS: u64 = 4;
const ATTACK_INTERVAL: u64 = 20;
const HOSTILE_CAP: usize = 32;
const PASSIVE_CAP: usize = 24;
const ARROW_CAP: usize = 128;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Zombie,
    Cow,
    Pig,
    Sheep,
    Chicken,
    Skeleton,
    Creeper,
    Spider,
    CaveSpider,
    Enderman,
    Slime,
    Witch,
    Ghast,
    Blaze,
    ZombiePigman,
    MagmaCube,
    Villager,
    IronGolem,
    EnderDragon,
    Wither,
}
impl Kind {
    fn protocol(self) -> u8 {
        match self {
            Self::Zombie => 54,
            Self::Cow => 92,
            Self::Pig => 90,
            Self::Sheep => 91,
            Self::Chicken => 93,
            Self::Skeleton => 51,
            Self::Creeper => 50,
            Self::Spider => 52,
            Self::CaveSpider => 59,
            Self::Enderman => 58,
            Self::Slime => 55,
            Self::Witch => 66,
            Self::Ghast => 56,
            Self::Blaze => 61,
            Self::ZombiePigman => 57,
            Self::MagmaCube => 62,
            Self::Villager => 120,
            Self::IronGolem => 99,
            Self::EnderDragon => 63,
            Self::Wither => 64,
        }
    }
    fn passive(self) -> bool {
        matches!(self, Self::Cow | Self::Pig | Self::Sheep | Self::Chicken | Self::Villager)
    }
    fn health(self) -> i16 {
        match self {
            Self::Chicken => 4,
            Self::Sheep | Self::Pig | Self::Cow => 10,
            Self::Spider => 16,
            Self::Creeper => 20,
            Self::Skeleton => 20,
            Self::Zombie => 20,
            Self::CaveSpider => 12,
            Self::Enderman => 40,
            Self::Slime => 4,
            Self::Witch => 26,
            Self::Ghast => 10,
            Self::Blaze | Self::ZombiePigman | Self::MagmaCube => 20,
            Self::Villager => 20,
            Self::IronGolem => 100,
            Self::EnderDragon => 200,
            Self::Wither => 300,
        }
    }
    fn is_boss(self) -> bool {
        matches!(self, Self::EnderDragon | Self::Wither)
    }
    fn burns_in_sun(self) -> bool {
        matches!(self, Self::Zombie | Self::Skeleton)
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
            Self::Spider | Self::CaveSpider => 287,
            Self::Enderman => 368,
            Self::Slime | Self::MagmaCube => 341,
            Self::Witch => 331,
            Self::Ghast => 370,
            Self::Blaze => 369,
            Self::ZombiePigman => 371,
            Self::Villager => 0,
            Self::IronGolem => 265,
            Self::EnderDragon => 122,
            Self::Wither => 399,
        };
        Stack {
            id,
            count: 1,
            damage: 0,
        }
    }
    fn breeding_food(self, held: i16) -> bool {
        match self {
            Self::Cow | Self::Sheep => held == 296,
            Self::Pig => held == 391,
            Self::Chicken => matches!(held, 295 | 361 | 362),
            _ => false,
        }
    }
}
#[derive(Clone, Copy, Debug)]
struct Mob {
    id: i32,
    kind: Kind,
    position: PlayerPosition,
    health: i16,
    hit_tick: u64,
    bite_tick: u64,
    fuse: u8,
    breed_tick: u64,
    baby: bool,
    age_ticks: u32,
    waypoint: Option<PlayerPosition>,
    burning: u16,
    sheared: bool,
    egg_timer: u32,
    split_size: u8,
    carried: Option<u16>,
    profession: u8,
    angry: bool,
}
impl Mob {
    fn spawn(self) -> Event {
        let mut body = Vec::new();
        write_varint(self.id as u32, &mut body);
        body.push(self.kind.protocol());
        for p in [self.position.x, self.position.y, self.position.z] {
            body.extend_from_slice(&((p * 32.0).floor() as i32).to_be_bytes());
        }
        body.extend_from_slice(&[0, 0, 0]);
        for _ in 0..3 {
            body.extend_from_slice(&0i16.to_be_bytes());
        }
        body.extend_from_slice(&[0, 0]); // flags byte
        if matches!(self.kind, Kind::Slime | Kind::MagmaCube) {
            body.push(16 << 3); // slime size metadata (byte)
            body.push(self.split_size);
        }
        if self.kind == Kind::Villager {
            body.push((16 << 3) | 2); // profession metadata (int)
            body.extend_from_slice(&(self.profession as i32).to_be_bytes());
        }
        if self.kind == Kind::Sheep {
            body.push(16 << 3);
            body.push(if self.sheared { 16 } else { 0 });
        }
        if self.baby {
            body.push((12 << 3) | 2); // metadata index 12, type 2 (int)
            body.extend_from_slice(&(-24000i32).to_be_bytes());
        }
        body.push(0x7f);
        Event { id: 0x0f, body }
    }
    fn status(self, value: u8) -> Event {
        let mut body = self.id.to_be_bytes().to_vec();
        body.push(value);
        Event { id: 0x1a, body }
    }
    fn nearby(self, position: PlayerPosition, radius: f64) -> bool {
        let dx = self.position.x - position.x;
        let dz = self.position.z - position.z;
        let dy = self.position.y - position.y;
        dx * dx + dy * dy + dz * dz <= radius * radius
    }
}
#[derive(Clone, Copy, Debug)]
struct Arrow {
    id: i32,
    position: PlayerPosition,
    velocity: [f64; 3],
    shooter: Option<i32>,
    age: u16,
    damage: u8,
}
impl Arrow {
    fn spawn(self) -> Event {
        let mut body = Vec::new();
        write_varint(self.id as u32, &mut body);
        body.push(60); // Protocol 5 arrow object
        for p in [self.position.x, self.position.y, self.position.z] {
            body.extend_from_slice(&((p * 32.0).floor() as i32).to_be_bytes());
        }
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(&self.shooter.unwrap_or(0).to_be_bytes());
        if self.shooter.is_some() {
            for speed in self.velocity {
                body.extend_from_slice(
                    &((speed * 8000.0).clamp(-32768.0, 32767.0) as i16).to_be_bytes(),
                );
            }
        }
        Event { id: 0x0e, body }
    }
}
#[derive(Debug)]
pub struct Attack {
    pub events: Vec<Event>,
    pub drop: Option<(Stack, (i32, i32, i32))>,
    pub experience: u32,
}
#[derive(Debug)]
pub struct Explosion {
    pub position: PlayerPosition,
    pub radius: f64,
}
impl Explosion {
    pub fn packet(&self) -> Event {
        let mut body = Vec::new();
        for v in [self.position.x, self.position.y, self.position.z] {
            body.extend_from_slice(&(v as f32).to_be_bytes());
        }
        body.extend_from_slice(&(self.radius as f32).to_be_bytes());
        body.extend_from_slice(&0i32.to_be_bytes()); // block records arrive as S23 updates
        for _ in 0..3 {
            body.extend_from_slice(&0.0f32.to_be_bytes());
        }
        Event { id: 0x27, body }
    }
}
#[derive(Default, Debug)]
pub struct Mobs {
    entities: BTreeMap<i32, Mob>,
    arrows: BTreeMap<i32, Arrow>,
    orbs: BTreeMap<i32, (PlayerPosition, u16)>,
    pending_hits: Vec<(PlayerPosition, u8, u64)>,
    pending_blast_damage: BTreeMap<i32, u8>,
    pending_drops: Vec<(Stack, (i32, i32, i32))>,
    explosions: Vec<Explosion>,
    last_tick: u64,
    next_id: i32,
    enabled: bool,
}
fn hash(mut x: u64) -> u64 {
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58476d1ce4e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}
fn distance(a: PlayerPosition, b: PlayerPosition) -> f64 {
    (a.x - b.x).hypot(a.z - b.z)
}
fn sky_exposed(world: &World, p: PlayerPosition) -> bool {
    let x = p.x.floor() as i32;
    let z = p.z.floor() as i32;
    ((p.y as i32 + 2)..=255).all(|y| world.get(x, y, z) == 0)
}
fn damage_held(held: Option<Stack>) -> i16 {
    match held.map(|s| s.id) {
        Some(268) | Some(283) => 4,
        Some(272) => 5,
        Some(267) => 6,
        Some(276) => 7,
        _ => 1,
    }
}
impl Mobs {
    fn insert(&mut self, kind: Kind, position: PlayerPosition, baby: bool) -> Event {
        let id = self.next_id;
        self.next_id += 1;
        let mob = Mob {
            id,
            kind,
            position,
            health: kind.health(),
            hit_tick: 0,
            bite_tick: 0,
            fuse: 0,
            breed_tick: 0,
            baby,
            age_ticks: 0,
            waypoint: None,
            burning: 0,
            sheared: false,
            egg_timer: 6000 + (id.unsigned_abs() % 6000),
            split_size: if matches!(kind, Kind::Slime | Kind::MagmaCube) { 2 } else { 1 },
            carried: None,
            profession: (id.unsigned_abs() % 5) as u8,
            angry: !matches!(kind, Kind::Spider | Kind::CaveSpider | Kind::ZombiePigman | Kind::IronGolem),
        };
        let event = mob.spawn();
        self.entities.insert(id, mob);
        event
    }
    /// Natural Survival only, preserving flat E2E world behaviour.
    pub fn new(world: &World, enabled: bool) -> Self {
        let mut state = Self {
            next_id: BASE_ID,
            enabled: enabled && world.is_natural(),
            ..Self::default()
        };
        if state.enabled {
            for (n, (x, z)) in [(8, 9), (-8, 7), (7, -8), (-9, -7)].into_iter().enumerate() {
                let p = PlayerPosition {
                    x: x as f64 + 0.5,
                    y: world.natural_seed().map_or(65.0, |seed| {
                        crate::terrain::surface(seed, x, z) as f64 + 1.0
                    }),
                    z: z as f64 + 0.5,
                };
                if !physics::collides(world, &p) && physics::supported(world, &p) {
                    state.insert(
                        if n == 0 {
                            Kind::Zombie
                        } else if n == 1 {
                            Kind::Skeleton
                        } else if n == 2 {
                            Kind::Cow
                        } else {
                            Kind::Creeper
                        },
                        p,
                        false,
                    );
                }
            }
        }
        state
    }
    pub fn spawn_events(&self) -> impl Iterator<Item = Event> + '_ {
        self.entities
            .values()
            .copied()
            .map(Mob::spawn)
            .chain(self.arrows.values().copied().map(Arrow::spawn))
            .chain(self.orbs.iter().map(|(&id, &(at, amount))| {
                let mut body = Vec::new();
                write_varint(id as u32, &mut body);
                for value in [at.x, at.y, at.z] {
                    body.extend_from_slice(&((value * 32.0).floor() as i32).to_be_bytes());
                }
                body.extend_from_slice(&(amount as i16).to_be_bytes());
                Event { id: 0x11, body }
            }))
    }
    /// Spawn a server-owned XP orb. No immediate XP award occurs.
    pub fn spawn_xp(&mut self, at: PlayerPosition, amount: u16) -> Option<Event> {
        if amount == 0 || self.orbs.len() >= 256 {
            return None;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.orbs.insert(id, (at, amount));
        let mut body = Vec::new();
        write_varint(id as u32, &mut body);
        for value in [at.x, at.y, at.z] {
            body.extend_from_slice(&((value * 32.0).floor() as i32).to_be_bytes());
        }
        body.extend_from_slice(&(amount as i16).to_be_bytes());
        Some(Event { id: 0x11, body })
    }
    pub fn collect_xp(
        &mut self,
        player: PlayerPosition,
        collector: i32,
    ) -> Option<(u32, Vec<Event>)> {
        let (&id, &(_, amount)) = self.orbs.iter().find(|(_, (at, _))| {
            let d =
                (at.x - player.x).powi(2) + (at.y - player.y).powi(2) + (at.z - player.z).powi(2);
            d <= 2.25
        })?;
        self.orbs.remove(&id);
        let mut body = id.to_be_bytes().to_vec();
        body.extend_from_slice(&collector.to_be_bytes());
        Some((
            u32::from(amount),
            vec![Event { id: 0x0d, body }, multiplayer::destroy(id)],
        ))
    }
    pub fn take_drops(&mut self) -> Vec<(Stack, (i32, i32, i32))> {
        std::mem::take(&mut self.pending_drops)
    }
    pub fn take_explosions(&mut self) -> Vec<Explosion> {
        std::mem::take(&mut self.explosions)
    }
    /// Each explosion is a world-wide event. Queue damage for every player
    /// in range rather than applying it only in the session that drains it.
    /// Keying by entity ID allows two players at exactly the same position
    /// to receive damage independently, once each.
    pub fn queue_explosion_hits(
        &mut self,
        explosions: &[Explosion],
        participants: &[(i32, PlayerPosition)],
    ) {
        for explosion in explosions {
            for &(id, player) in participants {
                let dx = explosion.position.x - player.x;
                let dy = explosion.position.y - player.y;
                let dz = explosion.position.z - player.z;
                let dist = (dx * dx + dy * dy + dz * dz).sqrt();
                if dist >= explosion.radius * 2.0 {
                    continue;
                }
                let fraction = 1.0 - dist / (explosion.radius * 2.0);
                let raw = (fraction * fraction * 14.0 + 1.0).ceil() as u8;
                let entry = self.pending_blast_damage.entry(id).or_default();
                *entry = entry.saturating_add(raw).min(20);
            }
        }
    }
    /// Attack a mob with authoritative held equipment, reach and cooldown.
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
        mob.health -= damage_held(held);
        let killed = mob.health <= 0;
        let result = Attack {
            events: if killed {
                vec![mob.status(3), multiplayer::destroy(id)]
            } else {
                vec![mob.status(2)]
            },
            drop: killed.then_some((
                mob.kind.drop(),
                (
                    mob.position.x.floor() as i32,
                    mob.position.y.floor() as i32,
                    mob.position.z.floor() as i32,
                ),
            )),
            experience: if killed {
                if mob.kind.passive() {
                    2
                } else {
                    5
                }
            } else {
                0
            },
        };
        if killed {
            self.entities.remove(&id);
        }
        Some(result)
    }
    /// Animal breeding consumes feed only if a nearby same-kind partner exists.
    pub fn feed(
        &mut self,
        id: i32,
        player: PlayerPosition,
        held: Option<Stack>,
        tick: u64,
    ) -> Option<Event> {
        let first = *self.entities.get(&id)?;
        let food = held?.id;
        if !first.nearby(player, 3.0)
            || self.entities.values().filter(|m| m.kind.passive()).count() >= PASSIVE_CAP
            || !first.kind.passive()
            || first.baby
            || !first.kind.breeding_food(food)
            || (first.breed_tick != 0 && tick.saturating_sub(first.breed_tick) < 600)
        {
            return None;
        }
        let mate = self.entities.values().copied().find(|m| {
            m.id != id
                && m.kind == first.kind
                && !m.baby
                && (m.breed_tick == 0 || tick.saturating_sub(m.breed_tick) >= 600)
                && distance(m.position, first.position) < 7.0
        })?;
        self.entities.get_mut(&id)?.breed_tick = tick;
        self.entities.get_mut(&mate.id)?.breed_tick = tick;
        Some(self.insert(first.kind, first.position, true))
    }
    /// Charging and ammunition are managed by the player session before this call.
    pub fn fire_arrow(
        &mut self,
        shooter: Option<i32>,
        at: PlayerPosition,
        yaw: f32,
        pitch: f32,
        speed: f64,
        damage: u8,
    ) -> Option<Event> {
        if self.arrows.len() >= ARROW_CAP
            || !at.is_valid()
            || !yaw.is_finite()
            || !pitch.is_finite()
        {
            return None;
        }
        let y = (yaw as f64).to_radians();
        let p = (pitch as f64).to_radians();
        let vx = -y.sin() * p.cos() * speed;
        let vz = y.cos() * p.cos() * speed;
        let vy = -p.sin() * speed;
        let id = self.next_id;
        self.next_id += 1;
        let arrow = Arrow {
            id,
            position: PlayerPosition {
                y: at.y + 1.3,
                ..at
            },
            velocity: [vx, vy, vz],
            shooter,
            age: 0,
            damage,
        };
        let event = arrow.spawn();
        self.arrows.insert(id, arrow);
        Some(event)
    }
    /// Natural spawning is gated by authoritative world lighting, biome,
    /// collision, difficulty and category caps. Never spawn inside 24 blocks
    /// of any player, even when another player is far away.
    fn spawn_natural(
        &mut self,
        now: u64,
        world: &World,
        players: &[PlayerPosition],
        events: &mut Vec<Event>,
    ) {
        if !self.enabled || players.is_empty() || !now.is_multiple_of(80) { return; }
        let Some(seed_world) = world.natural_seed() else { return; };
        let passive = self.entities.values().filter(|m| m.kind.passive()).count();
        let hostile = self.entities.len() - passive;
        for index in 0..4u64 {
            let seed = hash(now ^ index.wrapping_mul(0x9e3779b97f4a7c15) ^ seed_world);
            let reference = players[(seed as usize) % players.len()];
            let radial = 24 + (seed % 25) as i32;
            let x = reference.x.floor() as i32 + if seed & 1 == 0 { radial } else { -radial };
            let z = reference.z.floor() as i32 + (((seed >> 8) % 49) as i32 - 24);
            let y = if world.is_biome_world() {
                crate::terrain::biome_surface(seed_world, x, z) + 1
            } else {
                crate::terrain::surface(seed_world, x, z) + 1
            };
            let p = PlayerPosition { x: x as f64 + 0.5, y: y as f64, z: z as f64 + 0.5 };
            if !p.is_valid() || !physics::supported(world, &p)
                || physics::collides(world, &p) { continue; }
            let dist = distance_to_players(p, players);
            let (sky, block) = world.light_at(x, y, z);
            let daylight = (now % 24000) < 12000 || (now % 24000) > 23500;
            let sky = if daylight { sky } else { sky.saturating_sub(11) };
            let biome = if world.is_biome_world() {
                crate::terrain::biome(seed_world, x, z)
            } else { 1 };
            let common = SpawnCheck {
                sky_light: sky, block_light: block, daytime: daylight,
                biome, distance: dist, nearby_players: true,
                current: passive, capacity: PASSIVE_CAP,
                difficulty: 1, // Existing Protocol 5 login advertises Normal.
                grass_below: world.get(x,y-1,z) >> 4 == 2,
                has_room: true,
            };
            let kind = if mob_rules::eligible(SpawnCategory::Passive, common) {
                match seed % 4 {
                    0 => Kind::Cow, 1 => Kind::Pig, 2 => Kind::Sheep, _ => Kind::Chicken,
                }
            } else if mob_rules::eligible(SpawnCategory::Hostile, SpawnCheck {
                current: hostile, capacity: HOSTILE_CAP, ..common
            }) {
                match (seed >> 4) % 7 {
                    0 => Kind::Zombie, 1 => Kind::Skeleton, 2 => Kind::Creeper,
                    3 => Kind::Spider, 4 => Kind::Enderman, 5 => Kind::Witch,
                    _ => Kind::Slime,
                }
            } else { continue; };
            events.push(self.insert(kind,p,false));
            break; // Bound network traffic to one natural spawn per cycle.
        }
    }
    /// Tick at most four elapsed 20Hz steps; caller owns broadcast fan-out.
    pub fn tick(&mut self, now: u64, world: &World, players: &[PlayerPosition]) -> Vec<Event> {
        let steps = now.saturating_sub(self.last_tick).min(MAX_STEPS);
        self.last_tick = now;
        let mut events = Vec::new();
        self.pending_hits
            .retain(|(_, _, t)| now.saturating_sub(*t) < 30);
        self.spawn_natural(now, world, players, &mut events);
        for _ in 0..steps {
            let mut shots = Vec::new();
            let mut dead = Vec::new();
            for mob in self.entities.values_mut() {
                // Passive lifecycle is simulated only on the server.
                if mob.kind == Kind::Chicken && !mob.baby {
                    if mob.egg_timer > 0 { mob.egg_timer -= 1; }
                    if mob.egg_timer == 0 {
                        self.pending_drops.push((Stack{id:344,count:1,damage:0},(
                            mob.position.x.floor() as i32,
                            mob.position.y.floor() as i32,
                            mob.position.z.floor() as i32,
                        )));
                        mob.egg_timer = 6000 + (hash(now ^ mob.id as u64) % 6000) as u32;
                    }
                }
                if mob.kind == Kind::Wither && now.is_multiple_of(20) {
                    mob.health = (mob.health + 1).min(mob.kind.health());
                }
                if now.is_multiple_of(20) {
                    if mob.kind.burns_in_sun() && sky_exposed(world, mob.position)
                        && now % 24000 < 12000 {
                        mob.burning = 160;
                    }
                    if matches!(mob.kind, Kind::Spider | Kind::CaveSpider) {
                        let (sky, block)=world.light_at(
                            mob.position.x.floor() as i32, mob.position.y.floor() as i32,
                            mob.position.z.floor() as i32);
                        mob.angry = sky.max(block) <= 11;
                    }
                }
                if mob.burning > 0 {
                    mob.burning -= 1;
                    if now.is_multiple_of(20) {
                        mob.health -= 1;
                        if mob.health <= 0 {
                            events.push(mob.status(3));
                            self.pending_drops.push((
                                mob.kind.drop(), (
                                    mob.position.x.floor() as i32,
                                    mob.position.y.floor() as i32,
                                    mob.position.z.floor() as i32,
                                )));
                            dead.push(mob.id);
                            continue;
                        }
                        events.push(mob.status(2));
                    }
                }
                if mob.baby {
                    mob.age_ticks = mob.age_ticks.saturating_add(1);
                    if mob.age_ticks >= 24000 {
                        mob.baby = false;
                        let mut body = mob.id.to_be_bytes().to_vec();
                        body.push((12 << 3) | 2);
                        body.extend_from_slice(&0i32.to_be_bytes());
                        body.push(0x7f);
                        events.push(Event { id: 0x1c, body });
                    }
                }
                if !players.is_empty() && !mob.kind.passive()
                    && !mob.kind.is_boss() && distance_to_players(mob.position, players) > 128.0 {
                    dead.push(mob.id);
                    continue;
                }
                let nearest = players
                    .iter()
                    .copied()
                    .filter(|p| mob.nearby(*p, if mob.kind.passive() { 12.0 } else { 24.0 }))
                    .min_by(|a, b| {
                        distance(*a, mob.position).total_cmp(&distance(*b, mob.position))
                    });
                let Some(target) = nearest else {
                    continue;
                };
                if !mob.angry && !mob.kind.passive() { continue; }
                let d = distance(target, mob.position);
                if mob.kind == Kind::Creeper {
                    if d < 3.0 {
                        mob.fuse = mob.fuse.saturating_add(1);
                    } else {
                        mob.fuse = mob.fuse.saturating_sub(1);
                    }
                    if mob.fuse >= 30 {
                        self.explosions.push(Explosion {
                            position: mob.position,
                            radius: 3.0,
                        });
                        dead.push(mob.id);
                        continue;
                    }
                }
                if mob.kind == Kind::Skeleton && (5.0..16.0).contains(&d)
                    && now.saturating_sub(mob.bite_tick) >= 40
                    && mob_rules::visible(
                        world,
                        PlayerPosition { y: mob.position.y+1.55, ..mob.position },
                        PlayerPosition { y: target.y+1.55, ..target }, 16.0)
                {
                    mob.bite_tick = now;
                    let yaw = (-(target.x - mob.position.x))
                        .atan2(target.z - mob.position.z)
                        .to_degrees() as f32;
                    let pitch = -(target.y + 1.0 - mob.position.y).atan2(d).to_degrees() as f32;
                    shots.push((mob.id, mob.position, yaw, pitch));
                }
                let destination = if mob.kind.passive() {
                    if !(now / 80 + mob.id as u64).is_multiple_of(4) {
                        continue;
                    }
                    PlayerPosition {
                        x: mob.position.x + if mob.id % 2 == 0 { 1.0 } else { -1.0 },
                        z: mob.position.z + if mob.id % 3 == 0 { 1.0 } else { -1.0 },
                        ..mob.position
                    }
                } else if mob.kind == Kind::Skeleton && d < 5.0 {
                    PlayerPosition {
                        x: mob.position.x + mob.position.x - target.x,
                        y: mob.position.y,
                        z: mob.position.z + mob.position.z - target.z,
                    }
                } else {
                    target
                };
                if d < if mob.kind.passive() { 1.0 } else { 1.5 } {
                    continue;
                }
                if now.is_multiple_of(20) || mob.waypoint.is_none() {
                    mob.waypoint = navigation::next_step(world, mob.position, destination, 64);
                }
                if mob
                    .waypoint
                    .is_some_and(|p| distance(p, mob.position) < 0.25)
                {
                    mob.waypoint = None;
                }
                let waypoint = mob.waypoint.unwrap_or(destination);
                let dx = waypoint.x - mob.position.x;
                let dz = waypoint.z - mob.position.z;
                let len = dx.hypot(dz);
                if len < 1e-6 {
                    continue;
                }
                let speed = if mob.kind.passive() { 0.065 } else { 0.11 };
                let candidate = PlayerPosition {
                    x: mob.position.x + dx / len * speed,
                    z: mob.position.z + dz / len * speed,
                    y: mob.position.y,
                };
                if physics::valid_movement(world, &mob.position, &candidate)
                    && physics::supported(world, &candidate)
                {
                    mob.position = candidate;
                    events.push(multiplayer::teleport(mob.id, &candidate, 0.0, 0.0));
                } else if mob.kind == Kind::Spider {
                    // Climb alongside a wall, never through it.
                    let front = PlayerPosition {
                        x: mob.position.x + dx / len * 0.6,
                        z: mob.position.z + dz / len * 0.6,
                        ..mob.position
                    };
                    let wall = world.get(
                        front.x.floor() as i32,
                        front.y.floor() as i32,
                        front.z.floor() as i32,
                    ) >> 4;
                    if wall != 0 && !matches!(wall, 8..=11) {
                        let climbed = PlayerPosition {
                            y: mob.position.y + 0.2,
                            ..mob.position
                        };
                        if climbed.is_valid() && !physics::collides(world, &climbed) {
                            mob.position = climbed;
                            events.push(multiplayer::teleport(mob.id, &climbed, 0.0, 0.0));
                        }
                    }
                }
            }
            for id in dead {
                if self.entities.remove(&id).is_some() {
                    events.push(multiplayer::destroy(id));
                }
            }
            for (id, at, yaw, pitch) in shots {
                if let Some(packet) = self.fire_arrow(Some(id), at, yaw, pitch, 1.5, 3) {
                    events.push(packet);
                }
            }
            self.tick_arrows(world, players, now, &mut events);
        }
        if self.explosions.len() > 32 {
            self.explosions.truncate(32);
        }
        events
    }
    fn tick_arrows(
        &mut self,
        world: &World,
        players: &[PlayerPosition],
        now: u64,
        events: &mut Vec<Event>,
    ) {
        let mut removed = Vec::new();
        let mut arrow_hits = Vec::new();
        for arrow in self.arrows.values_mut() {
            arrow.age += 1;
            let next = PlayerPosition {
                x: arrow.position.x + arrow.velocity[0],
                y: arrow.position.y + arrow.velocity[1],
                z: arrow.position.z + arrow.velocity[2],
            };
            arrow.velocity[0] *= 0.99;
            arrow.velocity[1] = arrow.velocity[1] * 0.99 - 0.05;
            arrow.velocity[2] *= 0.99;
            if arrow.age >= 100 || !next.is_valid() || physics::collides(world, &next) {
                removed.push(arrow.id);
                continue;
            }
            arrow.position = next;
            if arrow
                .shooter
                .is_some_and(|id| self.entities.contains_key(&id))
            {
                if let Some(hit) = players.iter().find(|p| {
                    (p.x - next.x).hypot(p.z - next.z) < 0.85 && (p.y + 0.9 - next.y).abs() < 1.3
                }) {
                    self.pending_hits.push((*hit, arrow.damage, now));
                    removed.push(arrow.id);
                    continue;
                }
            } else {
                if let Some(mob) = self.entities.values().find(|m| m.nearby(next, 0.9)) {
                    arrow_hits.push((mob.id, arrow.damage));
                    removed.push(arrow.id);
                    continue;
                }
            }
            events.push(multiplayer::teleport(arrow.id, &next, 0.0, 0.0));
        }
        for (id, damage) in arrow_hits {
            if let Some(mob) = self.entities.get_mut(&id) {
                mob.health -= i16::from(damage);
                if mob.health <= 0 {
                    self.pending_drops.push((
                        mob.kind.drop(),
                        (
                            mob.position.x.floor() as i32,
                            mob.position.y.floor() as i32,
                            mob.position.z.floor() as i32,
                        ),
                    ));
                    events.push(mob.status(3));
                    events.push(multiplayer::destroy(id));
                    self.entities.remove(&id);
                } else {
                    events.push(mob.status(2));
                }
            }
        }
        for id in removed {
            self.arrows.remove(&id);
            events.push(multiplayer::destroy(id));
        }
    }
    pub fn hurt_player(&mut self, player_id: i32, player: PlayerPosition, tick: u64) -> u8 {
        let mut amount = self.pending_blast_damage.remove(&player_id).unwrap_or(0);
        self.pending_hits.retain(|(p, d, t)| {
            if tick.saturating_sub(*t) < 30 && (p.x - player.x).hypot(p.z - player.z) < 0.75 {
                amount = amount.saturating_add(*d);
                false
            } else {
                tick.saturating_sub(*t) < 30
            }
        });
        for mob in self.entities.values_mut() {
            if !mob.kind.passive()
                && mob.angry
                && mob.kind != Kind::Skeleton
                && mob.kind != Kind::Creeper
                && mob.nearby(player, 1.6)
                && (mob.bite_tick == 0 || tick.saturating_sub(mob.bite_tick) >= ATTACK_INTERVAL)
            {
                mob.bite_tick = tick;
                amount = amount.saturating_add(if mob.kind == Kind::Spider { 2 } else { 3 });
                break;
            }
        }
        amount
    }
}
fn distance_to_players(p: PlayerPosition, players: &[PlayerPosition]) -> f64 {
    players
        .iter()
        .map(|other| ((p.x-other.x).powi(2) + (p.y-other.y).powi(2) + (p.z-other.z).powi(2)).sqrt())
        .fold(f64::INFINITY, f64::min)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explosion_damages_all_nearby_players_once_even_when_overlapping() {
        let mut mobs = Mobs::default();
        let nearby = PlayerPosition {
            x: 1.0,
            y: 65.0,
            z: 1.0,
        };
        let far = PlayerPosition {
            x: 100.0,
            y: 65.0,
            z: 100.0,
        };
        let explosions = [Explosion {
            position: nearby,
            radius: 3.0,
        }];
        mobs.queue_explosion_hits(&explosions, &[(1, nearby), (2, nearby), (3, far)]);
        let first = mobs.hurt_player(1, nearby, 10);
        let second = mobs.hurt_player(2, nearby, 10);
        assert!(first > 0);
        assert_eq!(first, second);
        assert_eq!(mobs.hurt_player(1, nearby, 10), 0);
        assert_eq!(mobs.hurt_player(2, nearby, 10), 0);
        assert_eq!(mobs.hurt_player(3, far, 10), 0);
    }

    #[test]
    fn natural_mobs_spawn_in_protocol5() {
        let world = World::natural(42);
        let mut mobs = Mobs::new(&world, true);
        assert!(!mobs.entities.is_empty());
        let zombie = *mobs.entities.values().next().unwrap();
        assert_eq!(zombie.kind, Kind::Zombie);
        let packet = zombie.spawn();
        assert_eq!(packet.id, 0x0f);
        assert_eq!(packet.body[3], 54);
        assert!(packet.body.ends_with(&[0, 0, 0x7f]));
        let position = zombie.position;
        for tick in [100, 111] {
            assert!(mobs
                .player_attack(
                    zombie.id,
                    position,
                    Some(Stack {
                        id: 276,
                        count: 1,
                        damage: 0
                    }),
                    tick
                )
                .unwrap()
                .drop
                .is_none());
        }
        assert_eq!(
            mobs.player_attack(
                zombie.id,
                position,
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
    fn passive_animals_breed_once_per_cooldown() {
        let mut mobs = Mobs::default();
        let at = PlayerPosition::default();
        mobs.insert(Kind::Cow, at, false);
        mobs.insert(
            Kind::Cow,
            PlayerPosition {
                x: at.x + 1.0,
                ..at
            },
            false,
        );
        assert!(mobs
            .feed(
                0,
                at,
                Some(Stack {
                    id: 296,
                    count: 1,
                    damage: 0
                }),
                100
            )
            .is_some());
        assert!(mobs.entities.values().any(|m| m.baby));
        assert!(mobs
            .feed(
                1,
                at,
                Some(Stack {
                    id: 296,
                    count: 1,
                    damage: 0
                }),
                101
            )
            .is_none());
    }
    #[test]
    fn flat_world_has_no_mobs_and_bow_shot_is_bounded() {
        assert!(Mobs::new(&World::new(), true).entities.is_empty());
        let mut mobs = Mobs::default();
        assert!(mobs
            .fire_arrow(None, PlayerPosition::default(), 0.0, 0.0, 3.0, 4)
            .is_some());
        assert_eq!(mobs.arrows.len(), 1);
        assert_eq!(mobs.arrows.values().next().unwrap().spawn().id, 0x0e);
    }
}
