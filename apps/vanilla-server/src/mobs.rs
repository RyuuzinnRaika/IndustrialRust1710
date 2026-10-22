//! Bounded Protocol 5 natural-survival entities. No external entity registry.
use crate::inventory::Stack;
use crate::multiplayer::{self, Event};
use crate::navigation;
use crate::physics;
use crate::player::PlayerPosition;
use crate::world::World;
use legacy_protocol::write_varint;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const BASE_ID: i32 = 500_000;
const MAX_STEPS: u64 = 4;
const ATTACK_INTERVAL: u64 = 20;
const VANILLA_HOSTILE_CAP: usize = 70;
const VANILLA_PASSIVE_CAP: usize = 10;
const HOSTILE_CAP: usize = 70;
const PASSIVE_CAP: usize = 32; // Include manually bred animals without culling.
const ARROW_CAP: usize = 128;
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
enum Kind {
    Zombie,
    Cow,
    Pig,
    Sheep,
    Chicken,
    Skeleton,
    Creeper,
    Spider,
    #[allow(dead_code)] // Registered for the Protocol 5 species integration.
    CaveSpider,
    Enderman,
    Slime,
    Witch,
    #[allow(dead_code)] // Registered for the Protocol 5 species integration.
    Ghast,
    Blaze,
    ZombiePigman,
    MagmaCube,
    Villager,
    IronGolem,
    #[allow(dead_code)] // Registered for the Protocol 5 species integration.
    EnderDragon,
    #[allow(dead_code)] // Registered for the Protocol 5 species integration.
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
        matches!(
            self,
            Self::Cow | Self::Pig | Self::Sheep | Self::Chicken | Self::Villager
        )
    }
    fn peaceful_safe(self) -> bool {
        self.passive() || matches!(self, Self::IronGolem)
    }
    fn splitting(self) -> bool {
        matches!(self, Self::Slime | Self::MagmaCube)
    }
    fn hostile_to_players(self) -> bool {
        !self.peaceful_safe() && !matches!(self, Self::ZombiePigman | Self::Enderman)
    }
    fn health(self) -> i16 {
        match self {
            Self::Chicken => 4,
            Self::Sheep | Self::Pig | Self::Cow => 10,
            Self::Spider => 16,
            Self::CaveSpider => 12,
            Self::Enderman => 40,
            Self::Slime | Self::MagmaCube => 1,
            Self::Witch => 26,
            Self::Ghast => 10,
            Self::Blaze | Self::ZombiePigman => 20,
            Self::Villager => 20,
            Self::IronGolem => 100,
            Self::EnderDragon => 200,
            Self::Wither => 300,
            Self::Creeper => 20,
            Self::Skeleton => 20,
            Self::Zombie => 20,
        }
    }
    fn drop(self, size: u8) -> Option<Stack> {
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
            Self::Slime if size > 1 => return None,
            Self::Slime => 341,
            Self::Witch => 331,
            Self::Ghast => 370,
            Self::Blaze => 369,
            Self::ZombiePigman => 367,
            Self::MagmaCube if size > 1 => return None,
            Self::MagmaCube => 378,
            Self::IronGolem => 265,
            Self::Villager | Self::EnderDragon | Self::Wither => return None,
        };
        Some(Stack {
            id,
            count: 1,
            damage: 0,
        })
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
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
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
    sheared: bool,
    next_egg_tick: u64,
    profession: u8,
    size: u8,
    anger_until: u64,
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
        if self.baby {
            body.push((2 << 5) | 12); // metadata index 12, type 2 (int)
            body.extend_from_slice(&(-24000i32).to_be_bytes());
        }
        if self.kind.splitting() {
            body.push(16); // DataWatcher byte index 16: slime/magma cube size
            body.push(self.size);
        }
        if self.kind == Kind::Villager {
            body.push((2 << 5) | 16); // profession integer: farmer (0)
            body.extend_from_slice(&i32::from(self.profession).to_be_bytes());
        }
        if matches!(self.kind, Kind::EnderDragon | Kind::Wither) {
            body.push((3 << 5) | 6); // boss health as DataWatcher float
            body.extend_from_slice(&(f32::from(self.health)).to_be_bytes());
        }
        if self.kind == Kind::Sheep {
            body.push(16); // sheep colour + sheared bit (byte metadata)
            body.push(if self.sheared { 0x10 } else { 0 });
        }
        body.push(0x7f);
        Event { id: 0x0f, body }
    }
    fn status(self, value: u8) -> Event {
        let mut body = self.id.to_be_bytes().to_vec();
        body.push(value);
        Event { id: 0x1a, body }
    }
    fn death_drop(self) -> Option<(Stack, (i32, i32, i32))> {
        self.kind.drop(self.size).map(|stack| {
            (
                stack,
                (
                    self.position.x.floor() as i32,
                    self.position.y.floor() as i32,
                    self.position.z.floor() as i32,
                ),
            )
        })
    }
    fn nearby(self, position: PlayerPosition, radius: f64) -> bool {
        let dx = self.position.x - position.x;
        let dz = self.position.z - position.z;
        let dy = self.position.y - position.y;
        dx * dx + dy * dy + dz * dz <= radius * radius
    }
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
enum ProjectileKind {
    #[default]
    Arrow,
    WitchPotion,
    GhastFireball,
    BlazeFireball,
    WitherSkull,
}
impl ProjectileKind {
    fn protocol(self) -> u8 {
        match self {
            Self::Arrow => 60,
            Self::WitchPotion => 73,
            Self::GhastFireball => 63,
            Self::BlazeFireball => 64,
            Self::WitherSkull => 66,
        }
    }
    fn gravity(self) -> f64 {
        match self {
            Self::Arrow => 0.05,
            Self::WitchPotion => 0.05,
            _ => 0.0,
        }
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
struct Arrow {
    id: i32,
    position: PlayerPosition,
    velocity: [f64; 3],
    shooter: Option<i32>,
    hostile: bool,
    #[serde(default)]
    kind: ProjectileKind,
    age: u16,
    damage: u8,
}
impl Arrow {
    fn spawn(self) -> Event {
        let mut body = Vec::new();
        write_varint(self.id as u32, &mut body);
        body.push(self.kind.protocol()); // Protocol 5 spawn object
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
#[derive(Default, Debug, Deserialize, Serialize)]
pub struct Mobs {
    entities: BTreeMap<i32, Mob>,
    arrows: BTreeMap<i32, Arrow>,
    orbs: BTreeMap<i32, (PlayerPosition, u16, u64)>,
    #[serde(skip)]
    pending_hits: Vec<(PlayerPosition, u8, u64)>,
    #[serde(skip)]
    pending_blast_damage: BTreeMap<i32, u8>,
    #[serde(skip)]
    pending_drops: Vec<(Stack, (i32, i32, i32))>,
    #[serde(skip)]
    pending_effects: Vec<(PlayerPosition, u8, u16, u64)>,
    // Melee effects are keyed by the authenticated player Entity ID, never
    // by a position that multiple players can occupy simultaneously.
    #[serde(skip)]
    pending_target_effects: BTreeMap<i32, (u8, u16, u64)>,
    #[serde(skip)]
    active_effects: BTreeMap<i32, (u8, u64, u64)>,
    #[serde(skip)]
    explosions: Vec<Explosion>,
    last_tick: u64,
    next_id: i32,
    dragon_defeated: bool,
    enabled: bool,
    difficulty: Option<u8>,
    #[serde(skip)]
    storage: Option<PathBuf>,
    #[serde(skip)]
    last_saved: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MobSnapshot {
    format: u8,
    seed: Option<u64>,
    dimension: crate::dimensions::Dimension,
    state: Mobs,
}
fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
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
fn distance_3d(a: PlayerPosition, b: PlayerPosition) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)).sqrt()
}
fn can_see(world: &World, source: PlayerPosition, target: PlayerPosition) -> bool {
    let start = PlayerPosition {
        y: source.y + 1.3,
        ..source
    };
    let end = PlayerPosition {
        y: target.y + 1.2,
        ..target
    };
    let span = distance_3d(start, end);
    let steps = (span * 4.0).ceil() as usize;
    if steps == 0 {
        return true;
    }
    if steps > 128 {
        return false;
    }
    (1..steps).all(|step| {
        let t = step as f64 / steps as f64;
        let (x, y, z) = (
            (start.x + (end.x - start.x) * t).floor() as i32,
            (start.y + (end.y - start.y) * t).floor() as i32,
            (start.z + (end.z - start.z) * t).floor() as i32,
        );
        !matches!(world.get(x,y,z) >> 4, 1..=5 | 7 | 12..=17 | 20 | 24 | 35 | 41..=49 | 54 | 56 | 57 | 98 | 155)
    })
}
fn sunlight(now: u64) -> bool {
    (0..=12000).contains(&(now % 24000))
}
fn spawn_light_ok(block: u8, sky: u8, now: u64, passive: bool) -> bool {
    let effective_sky = sky.saturating_sub(if sunlight(now) { 0 } else { 11 });
    if passive {
        block.max(effective_sky) >= 9
    } else {
        block < 8 && effective_sky < 8
    }
}
fn sky_exposed(world: &World, p: PlayerPosition) -> bool {
    let x = p.x.floor() as i32;
    let z = p.z.floor() as i32;
    ((p.y as i32 + 2)..=255).all(|y| world.get(x, y, z) == 0)
}
/// Sample nine rays across a target's body. Solid blocks attenuate the
/// exposure before damage is distributed to players and mobs.
fn blast_exposure(world: &World, from: PlayerPosition, to: PlayerPosition) -> f64 {
    let mut clear = 0u8;
    for ox in [-0.3, 0.0, 0.3] {
        for height in [0.1, 0.9, 1.6] {
            let target = PlayerPosition {
                x: to.x + ox,
                y: to.y + height,
                z: to.z,
            };
            let span = distance_3d(from, target);
            let steps = (span * 3.0).ceil().max(1.0) as usize;
            let blocked = (1..steps).any(|n| {
                let t = n as f64 / steps as f64;
                let (x, y, z) = (
                    (from.x + (target.x-from.x)*t).floor() as i32,
                    (from.y + (target.y-from.y)*t).floor() as i32,
                    (from.z + (target.z-from.z)*t).floor() as i32,
                );
                !matches!(world.get(x,y,z) >> 4, 0 | 6 | 8..=11 | 18 | 30 | 31 | 37..=40 | 50 | 55 | 59 | 63 | 65 | 66 | 69 | 70 | 72 | 75 | 76 | 77 | 78 | 83 | 90 | 106 | 111)
            });
            if !blocked {
                clear += 1;
            }
        }
    }
    f64::from(clear) / 9.0
}
fn blast_damage(world: &World, blast: &Explosion, target: PlayerPosition) -> u8 {
    let dist = distance_3d(blast.position, target);
    if dist >= blast.radius * 2.0 {
        return 0;
    }
    let impact =
        (1.0 - dist / (blast.radius * 2.0)) * blast_exposure(world, blast.position, target);
    ((impact * impact + impact) * 7.0 * blast.radius + 1.0)
        .floor()
        .clamp(0.0, 255.0) as u8
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
    /// Crash-resilient, bounded entity snapshot scoped to this exact world.
    /// An absent snapshot is a fresh world; a malformed one aborts startup.
    pub fn load_or_new(
        world: &World,
        enabled: bool,
        difficulty: u8,
        path: &Path,
    ) -> io::Result<Self> {
        let mut state = if path.exists() {
            let bytes = fs::read(path)?;
            if bytes.len() > 2 * 1024 * 1024 {
                return Err(invalid("mob snapshot too large"));
            }
            let saved: MobSnapshot =
                serde_json::from_slice(&bytes).map_err(|_| invalid("invalid mob snapshot"))?;
            if saved.format != 1
                || saved.seed != world.natural_seed()
                || saved.dimension != world.dimension()
            {
                return Err(invalid("mob snapshot belongs to a different world"));
            }
            let value = saved.state;
            let max_id = value
                .entities
                .keys()
                .chain(value.arrows.keys())
                .chain(value.orbs.keys())
                .copied()
                .max()
                .unwrap_or(BASE_ID - 1);
            if value.next_id <= max_id
                || value.next_id < BASE_ID
                || value.next_id >= i32::MAX - 1024
                || value.entities.len() > 128
                || value.arrows.len() > ARROW_CAP
                || value.orbs.len() > 256
                || value.entities.iter().any(|(&id, mob)| {
                    id < BASE_ID
                        || id != mob.id
                        || !mob.position.is_valid()
                        || mob.health <= 0
                        || mob.health > 300
                        || (mob.kind.splitting() && !matches!(mob.size, 1 | 2 | 4))
                })
                || value.arrows.iter().any(|(&id, arrow)| {
                    id < BASE_ID
                        || id != arrow.id
                        || !arrow.position.is_valid()
                        || !arrow
                            .velocity
                            .iter()
                            .all(|v| v.is_finite() && v.abs() <= 8.0)
                })
                || value
                    .orbs
                    .values()
                    .any(|(pos, amount, _)| !pos.is_valid() || *amount == 0)
            {
                return Err(invalid("invalid mob identity, position or bounds"));
            }
            value
        } else {
            Self::new_with_difficulty(world, enabled, difficulty)
        };
        state.enabled = enabled && world.is_natural();
        state.difficulty = Some(difficulty.min(3));
        state.storage = Some(path.to_path_buf());
        Ok(state)
    }
    /// Atomic replacement; the old snapshot survives failed serialization or write.
    pub fn save(&self, world: &World) -> io::Result<()> {
        let Some(path) = &self.storage else {
            return Ok(());
        };
        let snapshot = serde_json::json!({
            "format": 1,
            "seed": world.natural_seed(),
            "dimension": world.dimension(),
            "state": self,
        });
        let data = serde_json::to_vec(&snapshot)
            .map_err(|_| invalid("mob snapshot serialization failed"))?;
        if data.len() > 2 * 1024 * 1024 {
            return Err(invalid("mob snapshot too large"));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("mobs.tmp");
        let mut file = File::create(&tmp)?;
        file.write_all(&data)?;
        file.sync_all()?;
        fs::rename(tmp, path)?;
        Ok(())
    }
    fn insert(&mut self, kind: Kind, position: PlayerPosition, baby: bool) -> Event {
        self.insert_sized(kind, position, baby, 1)
    }
    fn insert_sized(
        &mut self,
        kind: Kind,
        position: PlayerPosition,
        baby: bool,
        size: u8,
    ) -> Event {
        let size = if kind.splitting() {
            size.clamp(1, 4)
        } else {
            1
        };
        let id = self.next_id;
        self.next_id += 1;
        let mob = Mob {
            id,
            kind,
            position,
            health: if kind.splitting() {
                i16::from(size) * i16::from(size)
            } else {
                kind.health()
            },
            hit_tick: 0,
            bite_tick: 0,
            fuse: 0,
            breed_tick: 0,
            baby,
            age_ticks: 0,
            waypoint: None,
            sheared: false,
            next_egg_tick: self.last_tick + 6000 + u64::from(id.unsigned_abs() % 6000),
            profession: (hash(self.last_tick ^ id as u64) % 5) as u8,
            size,
            anger_until: 0,
        };
        let event = mob.spawn();
        self.entities.insert(id, mob);
        event
    }
    /// Construct a server-owned boss. External summoning rules must validate
    /// soul sand/skulls or End progression before calling this method.
    pub fn summon_boss(
        &mut self,
        world: &World,
        kind: u8,
        position: PlayerPosition,
    ) -> Option<Event> {
        let creature = match kind {
            63 => Kind::EnderDragon,
            64 => Kind::Wither,
            _ => return None,
        };
        if !position.is_valid()
            || !self.enabled
            || self.difficulty == Some(0)
            || self.entities.values().any(|entity| entity.kind == creature)
            || (creature == Kind::EnderDragon
                && (world.dimension() != crate::dimensions::Dimension::End || self.dragon_defeated))
        {
            return None;
        }
        Some(self.insert(creature, position, false))
    }
    /// A bounded, server-authoritative emerald trade (GUI protocol pending).
    /// Item transfer must be committed atomically by the calling session.
    pub fn village_offer(
        &self,
        id: i32,
        player: PlayerPosition,
        held: Option<Stack>,
    ) -> Option<Stack> {
        let villager = self.entities.get(&id)?;
        if villager.kind != Kind::Villager || !villager.nearby(player, 3.0) || held?.id != 388 {
            return None;
        }
        let product = match villager.profession {
            0 => (297, 6), // farmer: bread
            1 => (340, 1), // librarian: book
            2 => (331, 3), // priest: redstone
            3 => (265, 2), // blacksmith: iron ingot
            _ => (320, 4), // butcher: cooked pork
        };
        Some(Stack {
            id: product.0,
            count: product.1,
            damage: 0,
        })
    }
    /// Split a defeated large slime/magma cube; every child receives an
    /// independent authoritative ID and an ordinary Protocol 5 spawn packet.
    fn split(&mut self, parent: Mob) -> Vec<Event> {
        if !parent.kind.splitting() || parent.size <= 1 {
            return Vec::new();
        }
        let child_size = parent.size / 2;
        let count = 2 + (hash(parent.id as u64 ^ self.last_tick) % 3) as usize;
        let mut events = Vec::new();
        for index in 0..count {
            if self.entities.len() >= 128 {
                break;
            }
            let angle = (index as f64) * std::f64::consts::TAU / count as f64;
            let position = PlayerPosition {
                x: parent.position.x + angle.cos() * 0.4,
                y: parent.position.y,
                z: parent.position.z + angle.sin() * 0.4,
            };
            if position.is_valid() {
                events.push(self.insert_sized(parent.kind, position, false, child_size));
            }
        }
        events
    }
    /// Natural Survival only, preserving flat E2E world behaviour.
    pub fn new_with_difficulty(world: &World, enabled: bool, difficulty: u8) -> Self {
        let mut state = Self {
            next_id: BASE_ID,
            enabled: enabled && world.is_natural(),
            difficulty: Some(difficulty.min(3)),
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
                if !physics::collides(world, &p)
                    && physics::supported(world, &p)
                    && (difficulty > 0 || n == 2)
                {
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
            .chain(self.orbs.iter().map(|(&id, &(at, amount, _))| {
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
        self.orbs.insert(id, (at, amount, self.last_tick));
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
        let (&id, &(_, amount, _)) = self.orbs.iter().find(|(_, (at, _, _))| {
            let d =
                (at.x - player.x).powi(2) + (at.y - player.y).powi(2) + (at.z - player.z).powi(2);
            d <= 2.25
        })?;
        self.orbs.remove(&id);
        let mut body = Vec::new();
        write_varint(id as u32, &mut body);
        write_varint(collector as u32, &mut body);
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
    /// An explosion damages every nearby participant once, with wall
    /// occlusion, and damages server-owned mobs before clients are notified.
    pub fn queue_explosion_hits(
        &mut self,
        world: &World,
        explosions: &[Explosion],
        participants: &[(i32, PlayerPosition)],
    ) -> Vec<Event> {
        let mut events = Vec::new();
        for explosion in explosions {
            for &(id, player) in participants {
                let raw = blast_damage(world, explosion, player);
                if raw == 0 {
                    continue;
                }
                let entry = self.pending_blast_damage.entry(id).or_default();
                *entry = entry.saturating_add(raw).min(40);
            }
            let affected: Vec<_> = self
                .entities
                .values()
                .filter_map(|mob| {
                    let raw = blast_damage(world, explosion, mob.position);
                    (raw > 0).then_some((mob.id, raw))
                })
                .collect();
            let mut dead = Vec::new();
            for (id, raw) in affected {
                if let Some(mob) = self.entities.get_mut(&id) {
                    mob.health -= i16::from(raw);
                    events.push(mob.status(if mob.health <= 0 { 3 } else { 2 }));
                    if mob.health <= 0 {
                        dead.push(id);
                    }
                }
            }
            for id in dead {
                if let Some(mob) = self.entities.remove(&id) {
                    if let Some(drop) = mob.death_drop() {
                        self.pending_drops.push(drop);
                    }
                    events.push(multiplayer::destroy(id));
                    events.extend(self.split(mob));
                }
            }
        }
        events
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
            drop: if killed { mob.death_drop() } else { None },
            experience: if killed {
                match mob.kind {
                    Kind::EnderDragon => 12_000,
                    Kind::Wither => 50,
                    _ if mob.kind.passive() => 2,
                    _ => 5,
                }
            } else {
                0
            },
        };
        let mut result = result;
        if killed {
            if let Some(parent) = self.entities.remove(&id) {
                if parent.kind == Kind::EnderDragon {
                    self.dragon_defeated = true;
                }
                result.events.extend(self.split(parent));
            }
        } else if matches!(mob.kind, Kind::Enderman | Kind::ZombiePigman) {
            mob.anger_until = tick.saturating_add(1200);
        }
        Some(result)
    }
    /// Cow milking is read-only for the animal; inventory substitution and
    /// persistence belong to the authenticated player session.
    pub fn can_milk(&self, id: i32, at: PlayerPosition, held: Option<Stack>) -> bool {
        held.is_some_and(|item| item.id == 325)
            && self
                .entities
                .get(&id)
                .is_some_and(|mob| mob.kind == Kind::Cow && !mob.baby && mob.nearby(at, 3.0))
    }
    /// Sheep shearing is server-authoritative and changes the shared entity metadata.
    pub fn shear(
        &mut self,
        id: i32,
        at: PlayerPosition,
        held: Option<Stack>,
    ) -> Option<(Stack, Event)> {
        let mob = self.entities.get_mut(&id)?;
        if mob.kind != Kind::Sheep
            || mob.sheared
            || mob.baby
            || held?.id != 359
            || !mob.nearby(at, 3.0)
        {
            return None;
        }
        mob.sheared = true;
        let mut body = id.to_be_bytes().to_vec();
        body.extend_from_slice(&[16, 0x10, 0x7f]);
        Some((
            Stack {
                id: 35,
                count: 2,
                damage: 0,
            },
            Event { id: 0x1c, body },
        ))
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
        let hostile = shooter.is_some_and(|id| self.entities.contains_key(&id));
        let arrow = Arrow {
            id,
            hostile,
            position: PlayerPosition {
                y: at.y + 1.3,
                ..at
            },
            velocity: [vx, vy, vz],
            shooter,
            kind: ProjectileKind::Arrow,
            age: 0,
            damage,
        };
        let event = arrow.spawn();
        self.arrows.insert(id, arrow);
        Some(event)
    }
    fn fire_special(
        &mut self,
        shooter: i32,
        at: PlayerPosition,
        yaw: f32,
        pitch: f32,
        kind: ProjectileKind,
    ) -> Option<Event> {
        let (speed, damage) = match kind {
            ProjectileKind::WitchPotion => (0.75, 2),
            ProjectileKind::GhastFireball => (0.7, 6),
            ProjectileKind::BlazeFireball => (0.9, 5),
            ProjectileKind::WitherSkull => (0.85, 7),
            ProjectileKind::Arrow => return None,
        };
        self.fire_arrow(Some(shooter), at, yaw, pitch, speed, damage)?;
        let id = self.next_id - 1;
        let projectile = self.arrows.get_mut(&id)?;
        projectile.kind = kind;
        Some(projectile.spawn())
    }
    /// Spawn attempts use dimension, biome, true 3D player distance, support,
    /// and the world's propagated sky and block light. The modest caps are
    /// intentionally server-specific until per-chunk vanilla caps are supported.
    fn spawn_natural(
        &mut self,
        now: u64,
        world: &World,
        players: &[PlayerPosition],
        events: &mut Vec<Event>,
    ) {
        if !self.enabled || players.is_empty() || !now.is_multiple_of(20) {
            return;
        }
        let passive = self
            .entities
            .values()
            .filter(|mob| mob.kind.passive())
            .count();
        let hostile = self
            .entities
            .values()
            .filter(|m| !m.kind.peaceful_safe())
            .count();
        let hostile_cap = natural_mob_cap(players, VANILLA_HOSTILE_CAP).min(HOSTILE_CAP);
        let passive_cap = natural_mob_cap(players, VANILLA_PASSIVE_CAP).min(PASSIVE_CAP);
        let dimension = world.dimension();
        for index in 0..4u64 {
            let seed = hash(
                now ^ index.wrapping_mul(0x9e3779b97f4a7c15) ^ world.natural_seed().unwrap_or(0),
            );
            let player = players[(seed as usize) % players.len()];
            let radius = 24 + (seed % 65) as i32;
            let x = player.x.floor() as i32 + if seed & 1 == 0 { radius } else { -radius };
            let z = player.z.floor() as i32 + (((seed >> 8) % 65) as i32 - 32);
            // Overworld has a heightmap; Nether and End have ceilings/islands
            // instead, so seek a bounded standable cavern floor.
            let height = world.surface_height_at(x, z);
            let mut y = if let Some(height) = height {
                height + 1
            } else {
                let start = 24 + ((seed >> 16) % 88) as i32;
                let mut found = None;
                for offset in 0..88 {
                    let probe = 24 + (start - 24 + offset) % 88;
                    if world.get(x, probe - 1, z) >> 4 != 0
                        && world.get(x, probe, z) == 0
                        && world.get(x, probe + 1, z) == 0
                    {
                        found = Some(probe);
                        break;
                    }
                }
                let Some(ground) = found else { continue };
                ground
            };
            // Underground probes seek a two-block-tall air pocket above solid
            // ground. Never create arbitrary mobs inside rock or liquids.
            if dimension == crate::dimensions::Dimension::Overworld
                && seed & 3 == 0
                && height.is_some_and(|h| h > 18)
            {
                let height = height.unwrap();
                let mut probe = 12 + ((seed >> 17) % (height - 13) as u64) as i32;
                for _ in 0..20 {
                    if world.get(x, probe - 1, z) != 0
                        && world.get(x, probe, z) == 0
                        && world.get(x, probe + 1, z) == 0
                    {
                        y = probe;
                        break;
                    }
                    probe = if probe + 1 < height { probe + 1 } else { 12 };
                }
            }
            let position = PlayerPosition {
                x: x as f64 + 0.5,
                y: y as f64,
                z: z as f64 + 0.5,
            };
            if !position.is_valid()
                || !(24.0..=128.0).contains(&distance_to_players(position, players))
                || !physics::supported(world, &position)
                || physics::collides(world, &position)
            {
                continue;
            }
            let biome = world.biome_at(x, z);
            let grass = world.get(x, y - 1, z) >> 4 == 2;
            let open = sky_exposed(world, position);
            let (block_light, sky_light) = world.light_at(x, y, z);
            let overworld = dimension == crate::dimensions::Dimension::Overworld;
            let is_passive =
                overworld && grass && open && now.is_multiple_of(400) && (seed & 1 == 0);
            if overworld && !spawn_light_ok(block_light, sky_light, now, is_passive) {
                continue;
            }
            let kind = if is_passive {
                if passive >= passive_cap || matches!(biome, 0 | 2 | 7 | 8 | 9 | 10 | 11 | 16 | 24)
                {
                    continue;
                }
                match seed % 4 {
                    0 => Kind::Cow,
                    1 => Kind::Pig,
                    2 => Kind::Sheep,
                    _ => Kind::Chicken,
                }
            } else {
                if self.difficulty == Some(0) || hostile >= hostile_cap {
                    continue;
                }
                match dimension {
                    crate::dimensions::Dimension::Overworld => {
                        if (y < 40 && (seed >> 24).is_multiple_of(16))
                            || (biome == 6 && !sunlight(now) && (seed >> 25).is_multiple_of(12))
                        {
                            Kind::Slime
                        } else {
                            match seed % 16 {
                                0..=4 => Kind::Zombie,
                                5..=8 => Kind::Skeleton,
                                9..=11 => Kind::Creeper,
                                12..=13 => Kind::Spider,
                                14 => Kind::Enderman,
                                _ => Kind::Witch,
                            }
                        }
                    }
                    crate::dimensions::Dimension::Nether => {
                        // Blaze spawning is limited to the existing fortress substrate.
                        let fortress = world.get(x, y - 1, z) >> 4 == 112;
                        if fortress && seed.is_multiple_of(4) {
                            Kind::Blaze
                        } else if (seed >> 24).is_multiple_of(8) {
                            Kind::MagmaCube
                        } else {
                            Kind::ZombiePigman
                        }
                    }
                    crate::dimensions::Dimension::End => Kind::Enderman,
                }
            };
            let size = if kind.splitting() {
                match (seed >> 33) % 3 {
                    0 => 1,
                    1 => 2,
                    _ => 4,
                }
            } else {
                1
            };
            let baby = kind == Kind::Zombie && (seed >> 21).is_multiple_of(20);
            events.push(self.insert_sized(kind, position, baby, size));
            break; // Bound per-cycle packets and CPU.
        }
    }
    /// Tick at most four elapsed 20Hz steps; caller owns broadcast fan-out.
    pub fn tick(&mut self, now: u64, world: &World, players: &[PlayerPosition]) -> Vec<Event> {
        // Each connected session can poll the shared world in the same 20 Hz
        // tick. Execute world simulation only once, including natural spawns.
        // Never let an out-of-order clock sample rewind the Mob timeline.
        if now <= self.last_tick {
            return Vec::new();
        }
        let steps = now.saturating_sub(self.last_tick).min(MAX_STEPS);
        self.last_tick = now;
        let mut events = Vec::new();
        // Vanilla orbs expire after five minutes (6,000 server ticks); broadcast
        // destruction so clients do not retain ghosts after the server frees them.
        self.orbs.retain(|id, orb| {
            if now.saturating_sub(orb.2) >= 6000 {
                events.push(multiplayer::destroy(*id));
                false
            } else {
                true
            }
        });
        self.pending_hits
            .retain(|(_, _, t)| now.saturating_sub(*t) < 30);
        if self.difficulty == Some(0) {
            self.entities.retain(|id, mob| {
                if mob.kind.peaceful_safe() {
                    true
                } else {
                    events.push(multiplayer::destroy(*id));
                    false
                }
            });
        }
        if self.enabled
            && world.dimension() == crate::dimensions::Dimension::End
            && !self.dragon_defeated
            && !players.is_empty()
            && !self
                .entities
                .values()
                .any(|mob| mob.kind == Kind::EnderDragon)
        {
            // The End has exactly one natural dragon until its defeat is saved.
            if let Some(spawn) = self.summon_boss(
                world,
                63,
                PlayerPosition {
                    x: 0.5,
                    y: 95.0,
                    z: 0.5,
                },
            ) {
                events.push(spawn);
            }
        }
        self.spawn_natural(now, world, players, &mut events);
        for _ in 0..steps {
            let mut shots = Vec::new();
            let mut dead = Vec::new();
            for mob in self.entities.values_mut() {
                if mob.baby {
                    mob.age_ticks = mob.age_ticks.saturating_add(1);
                    if mob.age_ticks >= 24000 {
                        mob.baby = false;
                        let mut body = mob.id.to_be_bytes().to_vec();
                        body.push((2 << 5) | 12);
                        body.extend_from_slice(&0i32.to_be_bytes());
                        body.push(0x7f);
                        events.push(Event { id: 0x1c, body });
                    }
                }
                let nearest_distance = distance_to_players(mob.position, players);
                if !mob.kind.passive() && nearest_distance > 128.0 {
                    dead.push(mob.id);
                    continue;
                }
                if !mob.kind.passive()
                    && nearest_distance > 32.0
                    && now.saturating_sub(mob.hit_tick) > 600
                    && hash(now ^ mob.id as u64).is_multiple_of(800)
                {
                    dead.push(mob.id);
                    continue;
                }
                if matches!(mob.kind, Kind::Zombie | Kind::Skeleton)
                    && sunlight(now)
                    && sky_exposed(world, mob.position)
                    && now.is_multiple_of(20)
                {
                    mob.health -= 1;
                    if mob.health <= 0 {
                        if let Some(drop) = mob.death_drop() {
                            self.pending_drops.push(drop);
                        }
                        events.push(mob.status(3));
                        dead.push(mob.id);
                        continue;
                    }
                    events.push(mob.status(2));
                }
                if mob.kind == Kind::Chicken && !mob.baby && now >= mob.next_egg_tick {
                    mob.next_egg_tick = now + 6000 + hash(now ^ mob.id as u64) % 6000;
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
                if matches!(mob.kind, Kind::Spider | Kind::CaveSpider)
                    && sunlight(now)
                    && sky_exposed(world, mob.position)
                    && distance(target, mob.position) > 3.0
                {
                    continue; // Daylight spiders are neutral until provoked.
                }
                let d = distance(target, mob.position);
                if mob.kind == Kind::EnderDragon {
                    // Server-owned orbit while the fight is active.
                    let angle = now as f64 * 0.015;
                    let next = PlayerPosition {
                        x: angle.cos() * 24.0,
                        y: 95.0 + (angle * 0.5).sin() * 8.0,
                        z: angle.sin() * 24.0,
                    };
                    if next.is_valid() {
                        mob.position = next;
                        events.push(multiplayer::teleport(mob.id, &next, 0.0, 0.0));
                    }
                    continue;
                }
                if mob.kind == Kind::IronGolem {
                    continue; // Guardian targeting is handled separately below.
                }
                if matches!(mob.kind, Kind::Enderman | Kind::ZombiePigman) && now >= mob.anger_until
                {
                    continue; // Neutral until attacked; anger expires after 60 seconds.
                }
                if mob.kind == Kind::Enderman && now.is_multiple_of(40) {
                    // A bounded teleport attempt; never move through a wall or
                    // into an unloaded/invalid coordinate. Shared server event.
                    for attempt in 0..8u64 {
                        let h = hash(now ^ mob.id as u64 ^ (attempt << 24));
                        let new_pos = PlayerPosition {
                            x: mob.position.x + ((h & 15) as f64 - 7.5),
                            y: mob.position.y,
                            z: mob.position.z + (((h >> 8) & 15) as f64 - 7.5),
                        };
                        if new_pos.is_valid()
                            && !physics::collides(world, &new_pos)
                            && physics::supported(world, &new_pos)
                        {
                            mob.position = new_pos;
                            mob.waypoint = None;
                            events.push(multiplayer::teleport(mob.id, &new_pos, 0.0, 0.0));
                            break;
                        }
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
                if mob.kind == Kind::Skeleton
                    && d < 16.0
                    && can_see(world, mob.position, target)
                    && now.saturating_sub(mob.bite_tick) >= 40
                {
                    mob.bite_tick = now;
                    let yaw = (-(target.x - mob.position.x))
                        .atan2(target.z - mob.position.z)
                        .to_degrees() as f32;
                    let pitch = -(target.y + 1.0 - mob.position.y).atan2(d).to_degrees() as f32;
                    shots.push((mob.id, mob.position, yaw, pitch, ProjectileKind::Arrow));
                }
                let spell = match mob.kind {
                    Kind::Witch if d < 12.0 => Some((ProjectileKind::WitchPotion, 60)),
                    Kind::Blaze if d < 24.0 => Some((ProjectileKind::BlazeFireball, 40)),
                    Kind::Ghast if d < 64.0 => Some((ProjectileKind::GhastFireball, 60)),
                    Kind::Wither if d < 40.0 => Some((ProjectileKind::WitherSkull, 40)),
                    _ => None,
                };
                if let Some((kind, interval)) = spell {
                    if can_see(world, mob.position, target)
                        && now.saturating_sub(mob.bite_tick) >= interval
                    {
                        mob.bite_tick = now;
                        let yaw = (-(target.x - mob.position.x))
                            .atan2(target.z - mob.position.z)
                            .to_degrees() as f32;
                        let pitch = -(target.y + 1.0 - mob.position.y).atan2(d).to_degrees() as f32;
                        shots.push((mob.id, mob.position, yaw, pitch, kind));
                    }
                }
                let destination = if mob.kind == Kind::Skeleton && d < 7.0 {
                    PlayerPosition {
                        x: mob.position.x + (mob.position.x - target.x),
                        z: mob.position.z + (mob.position.z - target.z),
                        ..mob.position
                    }
                } else if mob.kind.passive() {
                    if !(now / 80 + mob.id as u64).is_multiple_of(4) {
                        continue;
                    }
                    PlayerPosition {
                        x: mob.position.x + if mob.id % 2 == 0 { 1.0 } else { -1.0 },
                        z: mob.position.z + if mob.id % 3 == 0 { 1.0 } else { -1.0 },
                        ..mob.position
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
                let speed = if mob.kind.passive() {
                    0.065
                } else if mob.kind.splitting() {
                    0.08 + f64::from(mob.size) * 0.025
                } else if mob.baby {
                    0.18
                } else {
                    0.11
                };
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
            // Iron golems target nearby hostile mobs, never players or villagers.
            // Resolve after the iteration to avoid mutable aliasing and keep
            // each hit authoritative for all connected clients.
            let guardians: Vec<Mob> = self
                .entities
                .values()
                .filter(|m| {
                    m.kind == Kind::IronGolem
                        && (m.bite_tick == 0 || now.saturating_sub(m.bite_tick) >= 20)
                })
                .copied()
                .collect();
            for guardian in guardians {
                let victim = self
                    .entities
                    .values()
                    .filter(|other| {
                        other.kind.hostile_to_players() && other.nearby(guardian.position, 3.0)
                    })
                    .min_by(|a, b| {
                        distance(a.position, guardian.position)
                            .total_cmp(&distance(b.position, guardian.position))
                    })
                    .map(|mob| mob.id);
                if let Some(victim) = victim {
                    if let Some(golem) = self.entities.get_mut(&guardian.id) {
                        golem.bite_tick = now;
                    }
                    if let Some(enemy) = self.entities.get_mut(&victim) {
                        enemy.health -= 7;
                        events.push(enemy.status(if enemy.health <= 0 { 3 } else { 2 }));
                        if enemy.health <= 0 {
                            if let Some(drop) = enemy.death_drop() {
                                self.pending_drops.push(drop);
                            }
                            dead.push(victim);
                        }
                    }
                }
            }
            dead.sort_unstable();
            dead.dedup();
            for id in dead {
                if let Some(parent) = self.entities.remove(&id) {
                    events.push(multiplayer::destroy(id));
                    events.extend(self.split(parent));
                }
            }
            for (id, at, yaw, pitch, kind) in shots {
                let event = if kind == ProjectileKind::Arrow {
                    self.fire_arrow(Some(id), at, yaw, pitch, 1.5, 3)
                } else {
                    self.fire_special(id, at, yaw, pitch, kind)
                };
                if let Some(packet) = event {
                    events.push(packet);
                }
            }
            self.tick_arrows(world, players, now, &mut events);
        }
        if self.explosions.len() > 32 {
            self.explosions.truncate(32);
        }
        if now.saturating_sub(self.last_saved) >= 100 {
            if let Err(error) = self.save(world) {
                eprintln!("Mob snapshot save failed: {error}");
            } else {
                self.last_saved = now;
            }
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
            let previous = arrow.position;
            let next = PlayerPosition {
                x: previous.x + arrow.velocity[0],
                y: previous.y + arrow.velocity[1],
                z: previous.z + arrow.velocity[2],
            };
            arrow.velocity[0] *= 0.99;
            arrow.velocity[1] = arrow.velocity[1] * 0.99 - arrow.kind.gravity();
            arrow.velocity[2] *= 0.99;
            if arrow.age >= 100 || !next.is_valid() {
                removed.push(arrow.id);
                continue;
            }
            // Segment sweep, rather than only testing the endpoint. A fast
            // skeleton arrow must not tunnel through a wall or a player.
            let length = distance_3d(previous, next);
            let steps = (length * 4.0).ceil().clamp(1.0, 32.0) as usize;
            let mut impacted = false;
            for step in 1..=steps {
                let t = step as f64 / steps as f64;
                let sample = PlayerPosition {
                    x: previous.x + (next.x - previous.x) * t,
                    y: previous.y + (next.y - previous.y) * t,
                    z: previous.z + (next.z - previous.z) * t,
                };
                if !sample.is_valid() || physics::collides(world, &sample) {
                    if matches!(
                        arrow.kind,
                        ProjectileKind::GhastFireball | ProjectileKind::WitherSkull
                    ) {
                        self.explosions.push(Explosion {
                            position: sample,
                            radius: 1.0,
                        });
                    }
                    impacted = true;
                    break;
                }
                if arrow.hostile {
                    if let Some(hit) = players.iter().find(|p| {
                        (p.x - sample.x).hypot(p.z - sample.z) < 0.85
                            && (p.y + 0.9 - sample.y).abs() < 1.3
                    }) {
                        self.pending_hits.push((*hit, arrow.damage, now));
                        match arrow.kind {
                            ProjectileKind::WitchPotion => {
                                self.pending_effects.push((*hit, 19, 160, now));
                            }
                            ProjectileKind::WitherSkull => {
                                self.pending_effects.push((*hit, 20, 100, now));
                            }
                            ProjectileKind::GhastFireball => {
                                self.explosions.push(Explosion {
                                    position: sample,
                                    radius: 1.0,
                                });
                            }
                            _ => {}
                        }
                        impacted = true;
                        break;
                    }
                } else if let Some(mob) = self
                    .entities
                    .values()
                    .find(|mob| mob.id != arrow.shooter.unwrap_or(-1) && mob.nearby(sample, 0.9))
                {
                    arrow_hits.push((mob.id, arrow.damage));
                    impacted = true;
                    break;
                }
            }
            if impacted {
                removed.push(arrow.id);
                continue;
            }
            arrow.position = next;
            events.push(multiplayer::teleport(arrow.id, &next, 0.0, 0.0));
        }
        for (id, damage) in arrow_hits {
            if let Some(mob) = self.entities.get_mut(&id) {
                mob.health -= i16::from(damage);
                if mob.health <= 0 {
                    if let Some(drop) = mob.death_drop() {
                        self.pending_drops.push(drop);
                    }
                    events.push(mob.status(3));
                    events.push(multiplayer::destroy(id));
                    if let Some(parent) = self.entities.remove(&id) {
                        events.extend(self.split(parent));
                    }
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
    /// Apply a hostile potion effect to the correct player, not all peers.
    /// Caller broadcasts the generated Protocol 5 entity-effect packet.
    pub fn take_player_effects(
        &mut self,
        player_id: i32,
        player: PlayerPosition,
        tick: u64,
    ) -> Vec<Event> {
        let mut packets = Vec::new();
        let mut remaining = Vec::new();
        for (at, effect, duration, created) in std::mem::take(&mut self.pending_effects) {
            if tick.saturating_sub(created) >= 30 {
                continue;
            }
            if distance_3d(at, player) < 0.85 {
                self.active_effects
                    .insert(player_id, (effect, tick + u64::from(duration), tick));
                let mut body = player_id.to_be_bytes().to_vec();
                body.push(effect);
                body.push(0); // amplifier 0
                body.extend_from_slice(&(duration as i16).to_be_bytes());
                packets.push(Event { id: 0x1d, body });
            } else {
                remaining.push((at, effect, duration, created));
            }
        }
        self.pending_effects = remaining;
        if let Some((effect, duration, created)) = self.pending_target_effects.remove(&player_id) {
            if tick.saturating_sub(created) < 30 {
                self.active_effects
                    .insert(player_id, (effect, tick + u64::from(duration), tick));
                let mut body = player_id.to_be_bytes().to_vec();
                body.push(effect);
                body.push(0); // amplifier 0
                body.extend_from_slice(&(duration as i16).to_be_bytes());
                packets.push(Event { id: 0x1d, body });
            }
        }
        packets
    }
    #[cfg(test)]
    pub fn hurt_player(&mut self, player_id: i32, player: PlayerPosition, tick: u64) -> u8 {
        self.hurt_player_with_health(player_id, player, tick, 20.0)
    }
    pub fn hurt_player_with_health(
        &mut self,
        player_id: i32,
        player: PlayerPosition,
        tick: u64,
        health: f32,
    ) -> u8 {
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
            if (mob.kind.hostile_to_players()
                || (matches!(mob.kind, Kind::Enderman | Kind::ZombiePigman)
                    && tick < mob.anger_until))
                && !matches!(
                    mob.kind,
                    Kind::Skeleton
                        | Kind::Creeper
                        | Kind::Ghast
                        | Kind::Blaze
                        | Kind::Witch
                        | Kind::EnderDragon
                        | Kind::Wither
                )
                && mob.nearby(player, 1.6)
                && (mob.bite_tick == 0 || tick.saturating_sub(mob.bite_tick) >= ATTACK_INTERVAL)
            {
                mob.bite_tick = tick;
                amount = amount.saturating_add(if matches!(mob.kind, Kind::Spider | Kind::CaveSpider) {
                    2
                } else {
                    3
                });
                // Cave spiders cause poison only on Normal/Hard (7s / 15s).
                // Directly target this participant; a collocated player must
                // never inherit the victim's effect.
                let poison_duration = match self.difficulty.unwrap_or(2) {
                    2 if mob.kind == Kind::CaveSpider => 140,
                    3 if mob.kind == Kind::CaveSpider => 300,
                    _ => 0,
                };
                if poison_duration > 0 {
                    self.pending_target_effects
                        .insert(player_id, (19, poison_duration, tick));
                }
                break;
            }
        }
        // Poison (19) cannot kill by itself; Wither (20) can. Both are
        // server-timed regardless of the client's displayed potion animation.
        if let Some((effect, expires, last)) = self.active_effects.get_mut(&player_id) {
            if tick >= *expires {
                self.active_effects.remove(&player_id);
            } else if tick.saturating_sub(*last) >= (if *effect == 19 { 25 } else { 40 }) {
                *last = tick;
                if *effect == 20 || health > 1.0 {
                    amount = amount.saturating_add(1);
                }
            }
        }
        match self.difficulty.unwrap_or(1) {
            0 => 0,
            1 => amount.div_ceil(2),
            3 => amount.saturating_mul(3).div_ceil(2),
            _ => amount,
        }
    }
}
/// Unique 17x17 eligible chunk union. Vanilla's mob cap scales with this
/// area, not with raw online player count or the protocol render distance.
fn natural_mob_cap(players: &[PlayerPosition], base: usize) -> usize {
    let mut chunks = std::collections::BTreeSet::new();
    for player in players {
        let cx = (player.x.floor() as i32).div_euclid(16);
        let cz = (player.z.floor() as i32).div_euclid(16);
        for x in cx - 8..=cx + 8 {
            for z in cz - 8..=cz + 8 {
                chunks.insert((x, z));
            }
        }
    }
    base.saturating_mul(chunks.len()) / 289
}
fn distance_to_players(p: PlayerPosition, players: &[PlayerPosition]) -> f64 {
    players
        .iter()
        .map(|other| distance_3d(p, *other))
        .fold(f64::INFINITY, f64::min)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeated_sessions_do_not_rewind_or_double_advance_world_ticks() {
        let world = World::new();
        let mut mobs = Mobs::default();
        let player = PlayerPosition::default();
        mobs.insert(Kind::Zombie, player, false);
        mobs.tick(100, &world, &[player]);
        assert_eq!(mobs.last_tick, 100);
        assert!(mobs.tick(100, &world, &[player]).is_empty());
        assert!(mobs.tick(99, &world, &[player]).is_empty());
        assert_eq!(mobs.last_tick, 100);
        assert!(mobs.tick(101, &world, &[player]).is_empty());
        assert_eq!(mobs.last_tick, 101);
    }

    #[test]
    fn only_nearby_adult_cows_are_milkable() {
        let at = PlayerPosition::default();
        let mut mobs = Mobs::default();
        let bucket = Some(Stack {
            id: 325,
            count: 1,
            damage: 0,
        });
        let cow = mobs.next_id;
        mobs.insert(Kind::Cow, at, false);
        let calf = mobs.next_id;
        mobs.insert(Kind::Cow, at, true);
        let pig = mobs.next_id;
        mobs.insert(Kind::Pig, at, false);
        assert!(mobs.can_milk(cow, at, bucket));
        assert!(!mobs.can_milk(calf, at, bucket));
        assert!(!mobs.can_milk(pig, at, bucket));
        assert!(!mobs.can_milk(cow, at, None));
        assert!(!mobs.can_milk(
            cow,
            PlayerPosition { x: at.x + 4.0, ..at },
            bucket
        ));
    }

    #[test]
    fn cave_spider_poison_targets_only_bitten_player_and_respects_difficulty() {
        let at = PlayerPosition::default();
        for (difficulty, expected_duration) in [(1u8, 0u16), (2, 140), (3, 300)] {
            let mut mobs = Mobs {
                difficulty: Some(difficulty),
                ..Mobs::default()
            };
            mobs.insert(Kind::CaveSpider, at, false);
            assert!(mobs.hurt_player_with_health(17, at, 40, 20.0) > 0);
            assert!(mobs.take_player_effects(18, at, 41).is_empty());
            let events = mobs.take_player_effects(17, at, 41);
            assert_eq!(events.len(), usize::from(expected_duration > 0));
            if expected_duration > 0 {
                assert_eq!(events[0].id, 0x1d);
                assert_eq!(&events[0].body[..4], &17i32.to_be_bytes());
                assert_eq!(events[0].body[4], 19);
                assert_eq!(
                    &events[0].body[6..8],
                    &(expected_duration as i16).to_be_bytes()
                );
            }
        }
    }

    #[test]
    fn mob_snapshot_roundtrips_ids_health_ages_and_world_identity() {
        let world = World::natural(98765);
        let path = std::env::temp_dir().join(format!(
            "industrialrust-mobs-{}-persist.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut mobs = Mobs::load_or_new(&world, true, 2, &path).unwrap();
        let at = PlayerPosition {
            x: 4.5,
            y: 65.0,
            z: 4.5,
        };
        let event = mobs.insert(Kind::Villager, at, false);
        let id = mobs.next_id - 1;
        assert_eq!(event.id, 0x0f);
        if let Some(mob) = mobs.entities.get_mut(&id) {
            mob.health = 13;
            mob.age_ticks = 74;
            mob.profession = 3;
        }
        mobs.insert_sized(Kind::Slime, at, false, 4);
        mobs.save(&world).unwrap();
        let loaded = Mobs::load_or_new(&world, true, 2, &path).unwrap();
        assert_eq!(loaded.entities.get(&id).unwrap().health, 13);
        assert_eq!(loaded.entities.get(&id).unwrap().age_ticks, 74);
        assert_eq!(loaded.entities.get(&id).unwrap().profession, 3);
        assert!(loaded.next_id > id);
        assert_eq!(
            loaded
                .spawn_events()
                .filter(|event| event.id == 0x0f)
                .count(),
            loaded.entities.len()
        );
        assert!(Mobs::load_or_new(&World::natural(11), true, 2, &path).is_err());
        std::fs::write(&path, b"{garbled").unwrap();
        assert!(Mobs::load_or_new(&world, true, 2, &path).is_err());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn mob_cap_uses_unique_17_by_17_chunks_not_player_count() {
        let a = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        let b = PlayerPosition { x: 16.5, ..a };
        assert_eq!(natural_mob_cap(&[a], 70), 70);
        assert_eq!(natural_mob_cap(&[a, a], 70), 70);
        assert!(natural_mob_cap(&[a, b], 70) > 70);
        assert_eq!(natural_mob_cap(&[], 70), 0);
    }

    #[test]
    fn villager_emits_authoritative_profession_offer() {
        let mut mobs = Mobs::default();
        let at = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        mobs.insert(Kind::Villager, at, false);
        mobs.entities.get_mut(&0).unwrap().profession = 0;
        let emerald = Some(Stack {
            id: 388,
            count: 1,
            damage: 0,
        });
        assert_eq!(mobs.village_offer(0, at, emerald).unwrap().id, 297);
        assert!(mobs
            .village_offer(
                0,
                at,
                Some(Stack {
                    id: 1,
                    count: 1,
                    damage: 0
                })
            )
            .is_none());
        assert!(mobs
            .village_offer(0, PlayerPosition { x: 50.0, ..at }, emerald)
            .is_none());
    }

    #[test]
    fn special_mobs_launch_bounded_distinct_projectile_objects() {
        let mut mobs = Mobs::default();
        let at = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        for kind in [Kind::Witch, Kind::Blaze, Kind::Ghast, Kind::Wither] {
            mobs.insert(kind, at, false);
        }
        let target = PlayerPosition { x: 6.5, ..at };
        let packets = mobs.tick(80, &World::new(), &[target]);
        for kind in [
            ProjectileKind::WitchPotion,
            ProjectileKind::BlazeFireball,
            ProjectileKind::GhastFireball,
            ProjectileKind::WitherSkull,
        ] {
            assert!(mobs.arrows.values().any(|a| a.kind == kind));
            assert!(packets
                .iter()
                .any(|packet| packet.id == 0x0e && packet.body.contains(&kind.protocol())));
        }
    }

    #[test]
    fn end_dragon_boss_reappears_neither_after_death_nor_after_reload() {
        let world = World::expanded(42, crate::dimensions::Dimension::End);
        let path = std::env::temp_dir().join(format!(
            "industrialrust-mobs-{}-dragon.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut mobs = Mobs::load_or_new(&world, true, 2, &path).unwrap();
        let target = PlayerPosition {
            x: 0.5,
            y: 95.0,
            z: 0.5,
        };
        mobs.tick(1, &world, &[target]);
        let (id, pos) = mobs
            .entities
            .values()
            .find(|m| m.kind == Kind::EnderDragon)
            .map(|mob| (mob.id, mob.position))
            .unwrap();
        mobs.entities.get_mut(&id).unwrap().health = 1;
        let defeated = mobs.player_attack(id, pos, None, 100).unwrap();
        assert_eq!(defeated.experience, 12_000);
        assert!(mobs.dragon_defeated);
        mobs.save(&world).unwrap();
        let mut restored = Mobs::load_or_new(&world, true, 2, &path).unwrap();
        restored.tick(101, &world, &[target]);
        assert!(!restored
            .entities
            .values()
            .any(|mob| mob.kind == Kind::EnderDragon));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn special_mob_ids_health_and_metadata_are_protocol5() {
        assert_eq!(Kind::Enderman.protocol(), 58);
        assert_eq!(Kind::Slime.protocol(), 55);
        assert_eq!(Kind::Witch.protocol(), 66);
        assert_eq!(Kind::Ghast.protocol(), 56);
        assert_eq!(Kind::Blaze.protocol(), 61);
        assert_eq!(Kind::ZombiePigman.protocol(), 57);
        assert_eq!(Kind::MagmaCube.protocol(), 62);
        assert_eq!(Kind::Villager.protocol(), 120);
        assert_eq!(Kind::IronGolem.protocol(), 99);
        assert_eq!(Kind::EnderDragon.protocol(), 63);
        assert_eq!(Kind::Wither.protocol(), 64);
        assert_eq!(Kind::IronGolem.health(), 100);
        assert_eq!(Kind::Wither.health(), 300);
        let mut mobs = Mobs::default();
        let pos = PlayerPosition::default();
        let size = mobs.insert_sized(Kind::Slime, pos, false, 4);
        assert!(size.body.windows(2).any(|bytes| bytes == [16, 4]));
        assert_eq!(mobs.entities.get(&0).unwrap().health, 16);
        let baby = mobs.insert(Kind::Zombie, pos, true);
        assert!(baby
            .body
            .windows(5)
            .any(|bytes| bytes == [76, 255, 255, 162, 64]));
        let villager = mobs.insert(Kind::Villager, pos, false);
        assert!(villager
            .body
            .windows(5)
            .any(|bytes| bytes == [80, 0, 0, 0, 0]));
    }

    #[test]
    fn defeated_large_slimes_split_with_unique_shared_ids_and_no_air_drop() {
        let mut mobs = Mobs::default();
        let pos = PlayerPosition::default();
        mobs.insert_sized(Kind::Slime, pos, false, 4);
        mobs.entities.get_mut(&0).unwrap().health = 1;
        let attack = mobs.player_attack(0, pos, None, 20).unwrap();
        assert!(attack.drop.is_none());
        assert!(attack.events.iter().any(|event| event.id == 0x13));
        let children: Vec<_> = mobs.entities.values().copied().collect();
        assert!((2..=4).contains(&children.len()));
        assert!(children.iter().all(|child| child.kind == Kind::Slime
            && child.size == 2
            && child.health == 4
            && child.id != 0));
        let id = children[0].id;
        mobs.entities.get_mut(&id).unwrap().health = 1;
        let result = mobs.player_attack(id, pos, None, 30).unwrap();
        assert!(result.drop.is_none());
        assert!(mobs.entities.values().any(|mob| mob.size == 1));
        let unique: std::collections::BTreeSet<_> = mobs.entities.keys().copied().collect();
        assert_eq!(unique.len(), mobs.entities.len());
        assert_eq!(
            mobs.spawn_events()
                .filter(|packet| packet.id == 0x0f)
                .count(),
            mobs.entities.len()
        );
    }

    #[test]
    fn neutral_pigmen_retaliate_only_when_attacked() {
        let mut mobs = Mobs::default();
        let pos = PlayerPosition::default();
        let spawn = mobs.insert(Kind::ZombiePigman, pos, false);
        assert_eq!(spawn.id, 0x0f);
        assert_eq!(mobs.hurt_player(900, pos, 10), 0);
        mobs.player_attack(0, pos, None, 20).unwrap();
        assert!(mobs.hurt_player(900, pos, 21) > 0);
        assert_eq!(mobs.hurt_player(900, pos, 1220), 0);
    }

    #[test]
    fn iron_golem_protects_villagers_and_survives_peaceful() {
        let mut mobs = Mobs::default();
        let pos = PlayerPosition::default();
        mobs.difficulty = Some(0);
        mobs.insert(Kind::Villager, pos, false);
        mobs.insert(Kind::IronGolem, pos, false);
        mobs.insert(Kind::Zombie, pos, false);
        let packets = mobs.tick(10, &World::new(), &[pos]);
        assert!(packets.iter().any(|event| event.id == 0x13));
        assert_eq!(mobs.entities.len(), 2);
        mobs.difficulty = Some(1);
        mobs.insert(Kind::Zombie, pos, false);
        mobs.tick(30, &World::new(), &[pos]);
        assert_eq!(mobs.entities.get(&3).unwrap().health, 13);
        assert_eq!(mobs.hurt_player(999, pos, 31), 2); // Easy halves melee damage
    }

    #[test]
    fn skeleton_arrow_remains_hostile_after_shooter_dies() {
        let mut mobs = Mobs::default();
        let pos = PlayerPosition::default();
        mobs.insert(Kind::Skeleton, pos, false);
        mobs.fire_arrow(Some(0), pos, 0.0, 0.0, 2.0, 3).unwrap();
        assert!(mobs.arrows.values().next().unwrap().hostile);
        mobs.entities.remove(&0);
        assert!(mobs.arrows.values().next().unwrap().hostile);
    }
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
    fn xp_orbs_expire_after_five_minutes_and_notify_clients() {
        let mut mobs = Mobs::default();
        let at = PlayerPosition::default();
        let spawned = mobs.spawn_xp(at, 5).unwrap();
        let id = *mobs.orbs.keys().next().unwrap();
        assert_eq!(spawned.id, 0x11);
        assert_eq!(mobs.tick(5999, &World::new(), &[]).len(), 0);
        assert!(mobs.orbs.contains_key(&id));
        let destroyed = mobs.tick(6000, &World::new(), &[]);
        assert_eq!(destroyed.len(), 1);
        assert_eq!(destroyed[0].id, 0x13);
        assert!(mobs.orbs.is_empty());
        assert_eq!(mobs.spawn_events().count(), 0);
    }
    #[test]
    fn natural_mobs_spawn_in_protocol5() {
        let world = World::natural(42);
        let mut mobs = Mobs::new_with_difficulty(&world, true, 1);
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
    fn natural_spawn_checks_propagated_light_and_night() {
        let mut world = World::new();
        let x = 2;
        let y = 70;
        let z = 2;
        assert_eq!(world.light_at(x, y, z).0, 0);
        assert!(!spawn_light_ok(0, 15, 6000, false));
        assert!(spawn_light_ok(0, 15, 18000, false));
        assert!(!spawn_light_ok(9, 0, 18000, false));
        world.import_block(x, y, z, 50 << 4).unwrap();
        assert_eq!(world.light_at(x + 1, y, z).0, 13);
        assert!(!spawn_light_ok(
            world.light_at(x + 1, y, z).0,
            0,
            18000,
            false
        ));
    }
    #[test]
    fn skeleton_line_of_sight_is_blocked_by_a_stone_wall() {
        let mut world = World::new();
        let a = PlayerPosition {
            x: 0.5,
            y: 66.0,
            z: 0.5,
        };
        let b = PlayerPosition {
            x: 4.5,
            y: 66.0,
            z: 0.5,
        };
        assert!(can_see(&world, a, b));
        world.import_block(2, 67, 0, 1 << 4).unwrap();
        assert!(!can_see(&world, a, b));
    }
    #[test]
    fn explosions_respect_block_occlusion() {
        let mut world = World::new();
        let source = PlayerPosition {
            x: 0.5,
            y: 66.0,
            z: 0.5,
        };
        let target = PlayerPosition {
            x: 4.5,
            y: 66.0,
            z: 0.5,
        };
        let blast = Explosion {
            position: source,
            radius: 3.0,
        };
        let clear = blast_damage(&world, &blast, target);
        assert!(clear > 0);
        for y in 66..=68 {
            for z in -1..=1 {
                world.import_block(2, y, z, 1 << 4).unwrap();
            }
        }
        assert!(blast_damage(&world, &blast, target) < clear);
    }
    #[test]
    fn sheep_shearing_is_single_use_and_shared_with_joiners() {
        let mut mobs = Mobs::default();
        let at = PlayerPosition {
            x: 0.5,
            y: 66.0,
            z: 0.5,
        };
        let spawn = mobs.insert(Kind::Sheep, at, false);
        assert_eq!(spawn.id, 0x0f);
        let held = Some(Stack {
            id: 359,
            count: 1,
            damage: 0,
        });
        let (wool, metadata) = mobs.shear(0, at, held).unwrap();
        assert_eq!(wool.id, 35);
        assert_eq!(metadata.id, 0x1c);
        assert!(mobs.shear(0, at, held).is_none());
        let snapshot = mobs.spawn_events().next().unwrap();
        assert!(snapshot.body.windows(2).any(|part| part == [16, 0x10]));
    }
    #[test]
    fn flat_world_has_no_mobs_and_bow_shot_is_bounded() {
        assert!(Mobs::new_with_difficulty(&World::new(), true, 1)
            .entities
            .is_empty());
        let mut mobs = Mobs::default();
        assert!(mobs
            .fire_arrow(None, PlayerPosition::default(), 0.0, 0.0, 3.0, 4)
            .is_some());
        assert_eq!(mobs.arrows.len(), 1);
        assert_eq!(mobs.arrows.values().next().unwrap().spawn().id, 0x0e);
    }
}
