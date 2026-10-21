//! Bounded Protocol 5 natural-survival entities. No external entity registry.
use crate::inventory::Stack;
use crate::mob_rules::{self, SpawnCategory, SpawnCheck};
use crate::multiplayer::{self, Event};
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
    fn from_name(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "zombie" => Some(Self::Zombie),
            "cow" => Some(Self::Cow),
            "pig" => Some(Self::Pig),
            "sheep" => Some(Self::Sheep),
            "chicken" => Some(Self::Chicken),
            "skeleton" => Some(Self::Skeleton),
            "creeper" => Some(Self::Creeper),
            "spider" => Some(Self::Spider),
            "cavespider" => Some(Self::CaveSpider),
            "enderman" => Some(Self::Enderman),
            "slime" => Some(Self::Slime),
            "witch" => Some(Self::Witch),
            "ghast" => Some(Self::Ghast),
            "blaze" => Some(Self::Blaze),
            "zombiepigman" => Some(Self::ZombiePigman),
            "magmacube" => Some(Self::MagmaCube),
            "villager" => Some(Self::Villager),
            "irongolem" => Some(Self::IronGolem),
            "enderdragon" => Some(Self::EnderDragon),
            "wither" => Some(Self::Wither),
            _ => None,
        }
    }
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
        matches!(
            self,
            Self::Cow | Self::Pig | Self::Sheep | Self::Chicken | Self::Villager
        )
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
        if self.kind == Kind::Enderman {
            if let Some(block) = self.carried {
                body.push((16 << 3) | 1); // carried block ID (short)
                body.extend_from_slice(&(block as i16).to_be_bytes());
            }
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
    object_type: u8,
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
        body.push(self.object_type); // Protocol 5 projectile object
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
pub struct AnimalInteraction {
    pub metadata: Option<Event>,
    pub item: Option<(Stack, (i32, i32, i32))>,
    pub milk: bool,
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
    pending_hits: Vec<(i32, u8, u64)>,
    participants: Vec<(i32, PlayerPosition)>,
    pending_poison: BTreeMap<i32, u16>,
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
            split_size: if matches!(kind, Kind::Slime | Kind::MagmaCube) {
                2
            } else {
                1
            },
            carried: None,
            profession: (id.unsigned_abs() % 5) as u8,
            angry: !matches!(
                kind,
                Kind::Spider | Kind::CaveSpider | Kind::ZombiePigman | Kind::IronGolem
            ),
        };
        let event = mob.spawn();
        self.entities.insert(id, mob);
        event
    }
    /// Developer-only summon entry point. The caller is responsible for
    /// allowing this only from a trusted local client.
    pub fn summon(&mut self, kind_name: &str, world: &World, near: PlayerPosition) -> Option<Event> {
        let kind = Kind::from_name(kind_name)?;
        if self.entities.len() >= HOSTILE_CAP + PASSIVE_CAP + 40 { return None; }
        let p = PlayerPosition {x:near.x+2.0,y:near.y,z:near.z+0.5};
        if !p.is_valid() || physics::collides(world,&p) { return None; }
        if !matches!(kind,Kind::EnderDragon|Kind::Wither|Kind::Ghast|Kind::Blaze)
            && !physics::supported(world,&p) { return None; }
        Some(self.insert(kind,p,false))
    }
    /// Split only large Slimes/Magma Cubes. Child sizes are sent in Spawn
    /// Mob metadata, avoiding the original single-entity HP illusion.
    fn split_children(&mut self, dead: Mob) -> Vec<Event> {
        if !matches!(dead.kind, Kind::Slime | Kind::MagmaCube)
            || dead.split_size <= 1
            || self.entities.len() + 4 > HOSTILE_CAP + PASSIVE_CAP + 40
        {
            return Vec::new();
        }
        let mut events = Vec::new();
        for (dx, dz) in [(-0.4,-0.4), (0.4,-0.4), (-0.4,0.4), (0.4,0.4)] {
            let at = PlayerPosition {
                x: dead.position.x+dx, y:dead.position.y, z:dead.position.z+dz,
            };
            self.insert(dead.kind, at, false);
            let id = self.next_id-1;
            if let Some(child) = self.entities.get_mut(&id) {
                child.split_size = dead.split_size / 2;
                child.health = (dead.kind.health() / 2).max(1);
                events.push(child.spawn());
            }
        }
        events
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
    /// Tie all projectile hits to stable entity IDs, including overlapping
    /// player positions; called by the 20TPS worker before ticking entities.
    pub fn set_participants(&mut self, players: &[(i32, PlayerPosition)]) {
        self.participants = players.iter().copied()
            .filter(|(_, p)| p.is_valid()).take(32).collect();
    }
    pub fn take_player_effect(&mut self, player_id: i32) -> Option<(u8, u16)> {
        self.pending_poison.remove(&player_id).map(|duration| (19, duration))
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
        world: &World,
        explosions: &[Explosion],
        participants: &[(i32, PlayerPosition)],
    ) {
        for explosion in explosions {
            for &(id, player) in participants {
                let damage =
                    mob_rules::blast_damage(world, explosion.position, player, explosion.radius);
                if damage == 0 {
                    continue;
                }
                // A queue per entity, not a spatial queue, prevents the first
                // of several co-located players from consuming all hits.
                let entry = self.pending_blast_damage.entry(id).or_default();
                *entry = entry.saturating_add(damage).min(20);
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
        let mut result = Attack {
            events: if killed {
                vec![mob.status(3), multiplayer::destroy(id)]
            } else {
                vec![mob.status(2)]
            },
            drop: (killed && mob.kind != Kind::Villager
                && !(matches!(mob.kind, Kind::Slime | Kind::MagmaCube)
                    && mob.split_size > 1)).then_some((
                mob.kind.drop(),
                (
                    mob.position.x.floor() as i32,
                    mob.position.y.floor() as i32,
                    mob.position.z.floor() as i32,
                ),
            )),
            experience: if killed {
                match mob.kind {
                    Kind::EnderDragon => 12000,
                    Kind::Wither => 50,
                    _ if mob.kind.passive() => 2,
                    _ => 5,
                }
            } else {
                0
            },
        };
        if killed {
            if let Some(dead) = self.entities.remove(&id) {
                result.events.extend(self.split_children(dead));
            }
        }
        Some(result)
    }
    /// Animal interaction result; caller must persist its inventory changes and
    /// any ground drops before publishing the entity metadata packet.
    pub fn animal_interact(
        &mut self,
        id: i32,
        at: PlayerPosition,
        held: Option<Stack>,
    ) -> Option<AnimalInteraction> {
        let mob = self.entities.get_mut(&id)?;
        if !mob.nearby(at, 3.0) || mob.baby { return None; }
        let stack = held?;
        match (mob.kind, stack.id) {
            (Kind::Cow, 325) if stack.count == 1 => Some(AnimalInteraction {
                metadata: None, item: None, milk: true,
            }),
            (Kind::Sheep, 359) if !mob.sheared => {
                mob.sheared = true;
                let mut body = mob.id.to_be_bytes().to_vec();
                body.extend_from_slice(&[16 << 3, 16, 0x7f]);
                let pos = (
                    mob.position.x.floor() as i32,
                    mob.position.y.floor() as i32,
                    mob.position.z.floor() as i32,
                );
                Some(AnimalInteraction {
                    metadata: Some(Event { id: 0x1c, body }),
                    item: Some((Stack { id: 35, count: 1, damage: 0 }, pos)),
                    milk: false,
                })
            }
            _ => None,
        }
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
            object_type: 60,
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
    /// Reuses authoritative physics and shooter identity for all mob
    /// projectiles. Type 60=arrow, 63=fireball, 64=small fireball,
    /// 66=Wither skull, 73=splash potion (Protocol 5 object registry).
    fn fire_mob_projectile(
        &mut self, shooter: i32, at: PlayerPosition, target: PlayerPosition,
        object_type: u8, speed: f64, damage: u8,
    ) -> Option<Event> {
        if !matches!(object_type, 60 | 63 | 64 | 66 | 73) { return None; }
        let dx = target.x - at.x;
        let dz = target.z - at.z;
        let yaw = (-dx).atan2(dz).to_degrees() as f32;
        let pitch = -(target.y + 1.0 - at.y).atan2(dx.hypot(dz)).to_degrees() as f32;
        self.fire_arrow(Some(shooter), at, yaw, pitch, speed, damage)?;
        let arrow = self.arrows.get_mut(&(self.next_id - 1))?;
        arrow.object_type = object_type;
        Some(arrow.spawn())
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
        if !self.enabled || players.is_empty() || !now.is_multiple_of(80) {
            return;
        }
        let Some(seed_world) = world.natural_seed() else {
            return;
        };
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
            let p = PlayerPosition {
                x: x as f64 + 0.5,
                y: y as f64,
                z: z as f64 + 0.5,
            };
            if !p.is_valid() || !physics::supported(world, &p) || physics::collides(world, &p) {
                continue;
            }
            let dist = distance_to_players(p, players);
            let (sky, block) = world.light_at(x, y, z);
            let daylight = (now % 24000) < 12000 || (now % 24000) > 23500;
            let sky = if daylight {
                sky
            } else {
                sky.saturating_sub(11)
            };
            let biome = if world.is_biome_world() {
                crate::terrain::biome(seed_world, x, z)
            } else {
                1
            };
            let common = SpawnCheck {
                sky_light: sky,
                block_light: block,
                daytime: daylight,
                biome,
                distance: dist,
                nearby_players: true,
                current: passive,
                capacity: PASSIVE_CAP,
                difficulty: 1, // Existing Protocol 5 login advertises Normal.
                grass_below: world.get(x, y - 1, z) >> 4 == 2,
                has_room: true,
            };
            let kind = if mob_rules::eligible(SpawnCategory::Passive, common) {
                match seed % 4 {
                    0 => Kind::Cow,
                    1 => Kind::Pig,
                    2 => Kind::Sheep,
                    _ => Kind::Chicken,
                }
            } else if mob_rules::eligible(
                SpawnCategory::Hostile,
                SpawnCheck {
                    current: hostile,
                    capacity: HOSTILE_CAP,
                    ..common
                },
            ) {
                match (seed >> 4) % 7 {
                    0 => Kind::Zombie,
                    1 => Kind::Skeleton,
                    2 => Kind::Creeper,
                    3 => Kind::Spider,
                    4 => Kind::Enderman,
                    5 => Kind::Witch,
                    _ => Kind::Slime,
                }
            } else {
                continue;
            };
            let baby = kind == Kind::Zombie && ((seed >> 16) % 20 == 0);
            events.push(self.insert(kind, p, baby));
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
                if mob.kind == Kind::Sheep && mob.sheared && now.is_multiple_of(6000) {
                    mob.sheared = false;
                    let mut body = mob.id.to_be_bytes().to_vec();
                    body.extend_from_slice(&[16 << 3, 0, 0x7f]);
                    events.push(Event { id: 0x1c, body });
                }
                if mob.kind == Kind::Chicken && !mob.baby {
                    if mob.egg_timer > 0 {
                        mob.egg_timer -= 1;
                    }
                    if mob.egg_timer == 0 {
                        self.pending_drops.push((
                            Stack {
                                id: 344,
                                count: 1,
                                damage: 0,
                            },
                            (
                                mob.position.x.floor() as i32,
                                mob.position.y.floor() as i32,
                                mob.position.z.floor() as i32,
                            ),
                        ));
                        mob.egg_timer = 6000 + (hash(now ^ mob.id as u64) % 6000) as u32;
                    }
                }
                if mob.kind == Kind::Wither && now.is_multiple_of(20) {
                    mob.health = (mob.health + 1).min(mob.kind.health());
                }
                if now.is_multiple_of(20) {
                    if mob.kind.burns_in_sun()
                        && sky_exposed(world, mob.position)
                        && now % 24000 < 12000
                    {
                        mob.burning = 160;
                    }
                    if matches!(mob.kind, Kind::Spider | Kind::CaveSpider) {
                        let (sky, block) = world.light_at(
                            mob.position.x.floor() as i32,
                            mob.position.y.floor() as i32,
                            mob.position.z.floor() as i32,
                        );
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
                                mob.kind.drop(),
                                (
                                    mob.position.x.floor() as i32,
                                    mob.position.y.floor() as i32,
                                    mob.position.z.floor() as i32,
                                ),
                            ));
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
                if !players.is_empty()
                    && !mob.kind.passive()
                    && !mob.kind.is_boss()
                    && distance_to_players(mob.position, players) > 128.0
                {
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
                if !mob.angry && !mob.kind.passive() {
                    continue;
                }
                let d = distance(target, mob.position);
                if mob.kind == Kind::Enderman && now.is_multiple_of(100) && d < 16.0 {
                    // Bounded, collision-checked teleport near the target.
                    let offset = if mob.id % 2 == 0 { 3.0 } else { -3.0 };
                    let candidate = PlayerPosition {
                        x: target.x + offset, y: target.y, z: target.z + offset,
                    };
                    if candidate.is_valid()
                        && !physics::collides(world, &candidate)
                        && physics::supported(world, &candidate)
                    {
                        mob.position = candidate;
                        events.push(multiplayer::teleport(mob.id, &candidate, 0.0, 0.0));
                        continue;
                    }
                }
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
                let ranged = match mob.kind {
                    Kind::Skeleton if (5.0..16.0).contains(&d) =>
                        Some((60, 1.5, 3u8, 40u64)),
                    Kind::Witch if d < 12.0 =>
                        Some((73, 0.75, 3, 60)),
                    Kind::Ghast if d < 48.0 =>
                        Some((63, 0.85, 6, 55)),
                    Kind::Blaze if d < 18.0 =>
                        Some((64, 1.0, 5, 45)),
                    Kind::Wither if d < 32.0 =>
                        Some((66, 1.0, 8, 40)),
                    _ => None,
                };
                if let Some((object_type, speed, damage, delay)) = ranged {
                    if now.saturating_sub(mob.bite_tick) >= delay
                        && mob_rules::visible(
                            world,
                            PlayerPosition { y: mob.position.y + 1.5, ..mob.position },
                            PlayerPosition { y: target.y + 1.5, ..target },
                            64.0)
                    {
                        mob.bite_tick = now;
                        shots.push((mob.id, mob.position, target, object_type, speed, damage));
                    }
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
            // A zombie close enough to attack a villager may convert it.
            // Destroy + spawn is necessary because Protocol 5 mob type is
            // immutable for a single client-side entity instance.
            if now.is_multiple_of(40) {
                let zombies: Vec<PlayerPosition> = self.entities.values()
                    .filter(|mob| mob.kind == Kind::Zombie)
                    .map(|mob| mob.position).collect();
                for villager in self.entities.values_mut() {
                    if villager.kind == Kind::Villager && zombies.iter()
                        .any(|p| distance(*p, villager.position) < 1.5)
                        && hash(now ^ villager.id as u64) & 1 == 0
                    {
                        events.push(multiplayer::destroy(villager.id));
                        villager.kind = Kind::Zombie;
                        villager.health = Kind::Zombie.health();
                        villager.angry = true;
                        events.push(villager.spawn());
                    }
                }
            }
            // Iron golems target hostile server entities rather than players.
            // Cooldowns protect 20TPS when many monsters surround the golem.
            let golems: Vec<(i32, PlayerPosition, u64)> = self.entities.values()
                .filter(|m| m.kind == Kind::IronGolem)
                .map(|m| (m.id, m.position, m.bite_tick)).collect();
            for (golem_id, at, last_hit) in golems {
                if last_hit != 0 && now.saturating_sub(last_hit) < 20 { continue; }
                let foe = self.entities.iter().find(|(_,mob)| {
                    !mob.kind.passive()
                        && !matches!(mob.kind, Kind::IronGolem | Kind::ZombiePigman)
                        && distance(mob.position, at) < 2.5
                }).map(|(&id,_)| id);
                if let Some(foe) = foe {
                    if let Some(golem) = self.entities.get_mut(&golem_id) {
                        golem.bite_tick = now;
                    }
                    if let Some(enemy) = self.entities.get_mut(&foe) {
                        enemy.health -= 8;
                        let dead = *enemy;
                        events.push(enemy.status(if enemy.health <= 0 { 3 } else { 2 }));
                        if enemy.health <= 0 {
                            if dead.kind != Kind::Villager
                                && !(matches!(dead.kind, Kind::Slime | Kind::MagmaCube)
                                    && dead.split_size > 1) {
                                self.pending_drops.push((dead.kind.drop(),(
                                    dead.position.x.floor() as i32,
                                    dead.position.y.floor() as i32,
                                    dead.position.z.floor() as i32,
                                )));
                            }
                            self.entities.remove(&foe);
                            events.push(multiplayer::destroy(foe));
                            events.extend(self.split_children(dead));
                        }
                    }
                }
            }
            for id in dead {
                if self.entities.remove(&id).is_some() {
                    events.push(multiplayer::destroy(id));
                }
            }
            for (id, at, target, object_type, speed, damage) in shots {
                if let Some(packet) = self.fire_mob_projectile(
                    id, at, target, object_type, speed, damage
                ) {
                    events.push(packet);
                }
            }
            let participants = self.participants.clone();
            self.tick_arrows(world, &participants, now, &mut events);
        }
        if self.explosions.len() > 32 {
            self.explosions.truncate(32);
        }
        // Apply the same attenuated, occlusion-aware blast to other mobs.
        let mut blasted = Vec::new();
        for explosion in &self.explosions {
            for (&id, mob) in &self.entities {
                let damage = mob_rules::blast_damage(
                    world,
                    explosion.position,
                    mob.position,
                    explosion.radius,
                );
                if damage > 0 {
                    blasted.push((id, damage));
                }
            }
        }
        for (id, damage) in blasted {
            if let Some(mob) = self.entities.get_mut(&id) {
                mob.health -= i16::from(damage);
                let dead = *mob;
                events.push(mob.status(if mob.health <= 0 { 3 } else { 2 }));
                if mob.health <= 0 {
                    if dead.kind != Kind::Villager
                        && !(matches!(dead.kind, Kind::Slime | Kind::MagmaCube) && dead.split_size > 1) {
                        self.pending_drops.push((
                            dead.kind.drop(),
                            (dead.position.x.floor() as i32, dead.position.y.floor() as i32,
                             dead.position.z.floor() as i32),
                        ));
                    }
                    self.entities.remove(&id);
                    events.push(multiplayer::destroy(id));
                    events.extend(self.split_children(dead));
                }
            }
        }
        events
    }
    fn tick_arrows(
        &mut self,
        world: &World,
        players: &[(i32, PlayerPosition)],
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
            arrow.velocity[1] = arrow.velocity[1] * 0.99
                - if matches!(arrow.object_type, 60 | 73) { 0.05 } else { 0.0 };
            arrow.velocity[2] *= 0.99;
            let blocked = !next.is_valid() || physics::collides(world, &next);
            if arrow.age >= 100 || blocked {
                if blocked && matches!(arrow.object_type, 63 | 64 | 66) && next.is_valid() {
                    self.explosions.push(Explosion {
                        position: next,
                        radius: if arrow.object_type == 63 { 1.5 } else { 1.0 },
                    });
                }
                removed.push(arrow.id);
                continue;
            }
            arrow.position = next;
            if arrow.shooter.is_some_and(|id| self.entities.contains_key(&id)) {
                if let Some(&(id, _)) = players.iter().find(|(_, p)| {
                    (p.x - next.x).hypot(p.z - next.z) < 0.85
                        && (p.y + 0.9 - next.y).abs() < 1.3
                }) {
                    self.pending_hits.push((id, arrow.damage, now));
                    if arrow.object_type == 73 {
                        self.pending_poison.insert(id, 160);
                    }
                    if matches!(arrow.object_type, 63 | 64 | 66) {
                        self.explosions.push(Explosion {
                            position: next,
                            radius: if arrow.object_type == 63 { 1.5 } else { 1.0 },
                        });
                    }
                    removed.push(arrow.id);
                    continue;
                }
            } else if let Some(mob) = self.entities.values().find(|m| m.nearby(next, 0.9)) {
                arrow_hits.push((mob.id, arrow.damage));
                removed.push(arrow.id);
                continue;
            }
            events.push(multiplayer::teleport(arrow.id, &next, 0.0, 0.0));
        }
        for (id, damage) in arrow_hits {
            if let Some(mob) = self.entities.get_mut(&id) {
                mob.health -= i16::from(damage);
                let dead = *mob;
                if mob.health <= 0 {
                    if dead.kind != Kind::Villager
                        && !(matches!(dead.kind, Kind::Slime | Kind::MagmaCube) && dead.split_size > 1) {
                        self.pending_drops.push((
                            dead.kind.drop(),
                            (dead.position.x.floor() as i32, dead.position.y.floor() as i32,
                             dead.position.z.floor() as i32),
                        ));
                    }
                    events.push(mob.status(3));
                    events.push(multiplayer::destroy(id));
                    self.entities.remove(&id);
                    events.extend(self.split_children(dead));
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
        self.pending_hits.retain(|(id, damage, t)| {
            if *id == player_id && tick.saturating_sub(*t) < 30 {
                amount = amount.saturating_add(*damage);
                false
            } else {
                tick.saturating_sub(*t) < 30
            }
        });
        for mob in self.entities.values_mut() {
            if !mob.kind.passive()
                && mob.angry
                && mob.kind != Kind::Skeleton
                && !matches!(mob.kind,
                    Kind::Creeper | Kind::Witch | Kind::Ghast | Kind::Blaze | Kind::Wither)
                && mob.nearby(player, 1.6)
                && (mob.bite_tick == 0 || tick.saturating_sub(mob.bite_tick) >= ATTACK_INTERVAL)
            {
                mob.bite_tick = tick;
                amount = amount.saturating_add(if matches!(mob.kind, Kind::Spider | Kind::CaveSpider) {
                    2
                } else { 3 });
                if mob.kind == Kind::CaveSpider {
                    self.pending_poison.insert(player_id, 140);
                }
                break;
            }
        }
        amount
    }
}
fn distance_to_players(p: PlayerPosition, players: &[PlayerPosition]) -> f64 {
    players
        .iter()
        .map(|other| {
            ((p.x - other.x).powi(2) + (p.y - other.y).powi(2) + (p.z - other.z).powi(2)).sqrt()
        })
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
        mobs.queue_explosion_hits(
            &World::new(),
            &explosions,
            &[(1, nearby), (2, nearby), (3, far)],
        );
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
