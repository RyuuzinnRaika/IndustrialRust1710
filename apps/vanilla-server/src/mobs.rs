//! Bounded Protocol 5 natural-survival entities. No external entity registry.
use crate::inventory::Stack;
use crate::multiplayer::{self, Event};
use crate::navigation;
use crate::physics;
use crate::player::PlayerPosition;
use crate::world::{LightSampler, World};
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
// A bounded resource guard, not a per-server cap of 70: the natural mob cap
// scales with the *union* of player-centred 17x17 chunk areas.
const MAX_NATURAL_PER_CATEGORY: usize = 512;
const MAX_LOADED_MOBS: usize = 1024;
const ARROW_CAP: usize = 128;
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
enum Kind {
    Zombie,
    Cow,
    Pig,
    Horse,
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
            Self::Horse => 100,
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
            Self::Cow | Self::Pig | Self::Horse | Self::Sheep | Self::Chicken | Self::Villager
        )
    }
    /// Vanilla natural-spawn caps count animals and monsters, not villagers,
    /// golems or constructed/summoned bosses. Keeping this distinct from
    /// peaceful-safe/despawnable prevents persistent entities consuming a
    /// player's natural spawn budget.
    fn counts_toward_natural_cap(self, passive: bool) -> bool {
        if passive {
            matches!(
                self,
                Self::Cow | Self::Pig | Self::Horse | Self::Sheep | Self::Chicken
            )
        } else {
            matches!(
                self,
                Self::Zombie
                    | Self::Skeleton
                    | Self::Creeper
                    | Self::Spider
                    | Self::CaveSpider
                    | Self::Enderman
                    | Self::Slime
                    | Self::Witch
                    | Self::Ghast
                    | Self::Blaze
                    | Self::ZombiePigman
                    | Self::MagmaCube
            )
        }
    }
    fn peaceful_safe(self) -> bool {
        self.passive() || matches!(self, Self::IronGolem)
    }
    /// Bosses and constructed guardians are persistent; they must never be
    /// culled by the ordinary hostile 32/128-block distance despawn rules.
    fn despawnable(self) -> bool {
        !self.peaceful_safe() && !matches!(self, Self::EnderDragon | Self::Wither)
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
            Self::Horse => 30,
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
            Self::Horse => 334,
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
            // Vanilla 1.7.10 Wither boss always drops one Nether Star.
            Self::Wither => 399,
            Self::Villager | Self::EnderDragon => return None,
        };
        Some(Stack {
            id,
            count: 1,
            damage: 0,
            nbt: None,
        })
    }
    fn breeding_food(self, held: i16) -> bool {
        match self {
            Self::Cow | Self::Sheep => held == 296,
            Self::Pig => held == 391,
            // Java 1.7.10 horses enter love mode only with golden apples
            // or golden carrots; ordinary apples, wheat and sugar heal/tame.
            Self::Horse => matches!(held, 322 | 396),
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
    /// Consecutive simulated ticks outside every player's 32-block safe zone.
    /// Missing in older snapshots, so default to a fresh despawn grace period.
    #[serde(default)]
    far_from_player_ticks: u32,
    bite_tick: u64,
    fuse: u8,
    breed_tick: u64,
    /// The 600-tick vanilla love window; absent in older snapshots.
    #[serde(default)]
    love_until: u64,
    baby: bool,
    age_ticks: u32,
    waypoint: Option<PlayerPosition>,
    sheared: bool,
    next_egg_tick: u64,
    profession: u8,
    size: u8,
    anger_until: u64,
    // Transient local light decision. Recomputed from the actual world each
    // server tick; do not persist a stale day/night aggression state.
    #[serde(skip)]
    bright_spider: bool,
    #[serde(default)]
    zombie_villager: bool,
    #[serde(default)]
    can_break_wooden_doors: bool,
    #[serde(default)]
    door_break_target: Option<[i32; 3]>,
    #[serde(default)]
    door_break_started: u64,
    #[serde(default)]
    saddled: bool,
    #[serde(default)]
    tamed: bool,
    #[serde(default)]
    tame_progress: u8,
    #[serde(default)]
    fence_anchor: Option<[i32; 3]>,
    #[serde(skip)]
    rider: Option<i32>,
    #[serde(skip)]
    leash_holder: Option<i32>,
    #[serde(skip)]
    last_ride_tick: u64,
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
            if matches!(self.kind, Kind::Zombie | Kind::ZombiePigman) {
                // Zombie and pig-zombie use a byte child flag at index 12.
                // Sending the ageable-mob integer here makes 1.7.10's
                // EntityZombie cast the watcher value to Byte and crash.
                body.extend_from_slice(&[12, 1]);
            } else {
                // Other ageable mobs encode their growing age as an integer.
                body.push((2 << 5) | 12); // metadata index 12, type 2 (int)
                body.extend_from_slice(&(-24000i32).to_be_bytes());
            }
        }
        if self.kind == Kind::Zombie && self.zombie_villager {
            // Protocol 5 / Java 1.7.10: zombie DataWatcher byte 13
            // distinguishes infected villagers from ordinary zombies.
            body.extend_from_slice(&[13, 1]);
        }
        if self.kind.splitting() {
            body.push(16); // DataWatcher byte index 16: slime/magma cube size
            body.push(self.size);
        }
        if self.kind == Kind::Villager {
            body.push((2 << 5) | 16); // profession integer: farmer (0)
            body.extend_from_slice(&i32::from(self.profession).to_be_bytes());
        }
        if self.kind == Kind::Sheep {
            body.push(16); // sheep colour + sheared bit (byte metadata)
            body.push(if self.sheared { 0x10 } else { 0 });
        }
        if self.kind == Kind::Pig {
            body.push(16); // pig saddle metadata byte
            body.push(u8::from(self.saddled));
        }
        if self.kind == Kind::Horse {
            body.push((2 << 5) | 16); // horse flags int
            let flags: i32 = (if self.tamed { 2 } else { 0 }) | (if self.saddled { 4 } else { 0 });
            body.extend_from_slice(&flags.to_be_bytes());
        }
        // All living mobs expose health as a float at DataWatcher index 6,
        // not only bosses. New joiners must observe the current shared HP.
        body.push((3 << 5) | 6);
        body.extend_from_slice(&f32::from(self.health).to_be_bytes());
        body.push(0x7f);
        Event { id: 0x0f, body }
    }
    fn mount_metadata(self) -> Event {
        let mut body = self.id.to_be_bytes().to_vec();
        if self.kind == Kind::Horse {
            body.push((2 << 5) | 16);
            let flags: i32 = (if self.tamed { 2 } else { 0 }) | (if self.saddled { 4 } else { 0 });
            body.extend_from_slice(&flags.to_be_bytes());
        } else {
            body.push(16);
            body.push(u8::from(self.saddled));
        }
        body.push(0x7f);
        Event { id: 0x1c, body }
    }
    /// Protocol 5 Entity Metadata (0x1C): server-authoritative current HP.
    fn health_metadata(self) -> Event {
        let mut body = self.id.to_be_bytes().to_vec();
        body.push((3 << 5) | 6); // DataWatcher index 6, float
        body.extend_from_slice(&f32::from(self.health.max(0)).to_be_bytes());
        body.push(0x7f);
        Event { id: 0x1c, body }
    }
    fn status(self, value: u8) -> Event {
        let mut body = self.id.to_be_bytes().to_vec();
        body.push(value);
        Event { id: 0x1a, body }
    }
    fn death_drop(self) -> Option<(Stack, (i32, i32, i32))> {
        // Baby farm animals produce neither adult meat nor wool. Sheared
        // sheep cannot drop a second wool block on death.
        if (self.baby && self.kind.passive()) || (self.kind == Kind::Sheep && self.sheared) {
            return None;
        }
        let mut stack = self.kind.drop(self.size)?;
        // Vanilla 1.7.10 drops are not a guaranteed one item for every mob.
        // Use a reproducible roll from the server-owned entity and death tick.
        // Looting, rare drops and cooked loot are tracked separately in C26.
        let roll = hash(self.id as u64 ^ self.hit_tick.rotate_left(17) ^ 0x2f56a163);
        stack.count = match self.kind {
            Kind::Cow | Kind::Pig => (1 + roll % 3) as u8,
            Kind::Horse => ((roll >> 4) % 3) as u8, // 0-2 leather, not guaranteed
            Kind::Zombie | Kind::Skeleton | Kind::Creeper | Kind::Spider | Kind::CaveSpider => {
                ((roll >> 4) % 3) as u8
            }
            Kind::Enderman | Kind::Ghast | Kind::Blaze => ((roll >> 6) % 2) as u8,
            _ => stack.count,
        };
        (stack.count > 0).then_some(self.drop_stack(stack))
    }
    fn drop_stack(self, stack: Stack) -> (Stack, (i32, i32, i32)) {
        (
            stack,
            (
                self.position.x.floor() as i32,
                self.position.y.floor() as i32,
                self.position.z.floor() as i32,
            ),
        )
    }
    /// Secondary common drops are separate server-owned stacks, never an
    /// overwrite of the primary drop. Babies cannot produce adult loot.
    fn extra_death_drops(self) -> Vec<(Stack, (i32, i32, i32))> {
        if self.baby && self.kind.passive() {
            return Vec::new();
        }
        // Saddles are persistent equipment, not random creature loot. Drop
        // exactly one on death for both rideable species, never at despawn.
        if self.saddled && matches!(self.kind, Kind::Pig | Kind::Horse) {
            return vec![self.drop_stack(Stack {
                id: 329,
                count: 1,
                damage: 0,
                nbt: None,
            })];
        }
        let roll = hash(self.id as u64 ^ self.hit_tick.rotate_left(17) ^ 0xb52c8c91);
        let (id, count) = match self.kind {
            Kind::Cow => (334, (roll % 3) as u8),      // 0–2 leather
            Kind::Chicken => (288, (roll % 3) as u8),  // 0–2 feathers
            Kind::Skeleton => (262, (roll % 3) as u8), // 0–2 arrows
            _ => return Vec::new(),
        };
        if count == 0 {
            Vec::new()
        } else {
            vec![self.drop_stack(Stack {
                id,
                count,
                damage: 0,
                nbt: None,
            })]
        }
    }
    fn experience_on_player_kill(self) -> u16 {
        if self.baby && self.kind.passive() {
            0
        } else {
            match self.kind {
                Kind::EnderDragon => 12_000,
                Kind::Wither => 50,
                _ if self.kind.passive() => 2,
                _ => 5,
            }
        }
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
/// Optional Entity Metadata together with a changed grass block coordinate.
pub type GrazingUpdate = (Option<Event>, (i32, i32, i32));

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
    #[serde(default)]
    last_graze_tick: u64,
    /// Next eligible spawn tick per spawner coordinate. Persist across restarts
    /// so reconnecting players cannot reset a dungeon's spawn cooldown.
    #[serde(default)]
    spawner_next_tick: BTreeMap<String, u64>,
    #[serde(skip)]
    last_leash_tick: u64,
    // Runtime-only flag. Default false (not paused) on new/load.
    #[serde(skip)]
    natural_spawning_paused: bool,
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

/// The stable mob selected for an IRW4 dungeon spawner. Share this lookup
/// with the Anvil NBT exporter so EntityId never disagrees with live spawning.
pub(crate) fn dungeon_spawner_entity_id(x: i32, y: i32, z: i32) -> &'static str {
    let salt = (x as i64 as u64).wrapping_mul(0x9e3779b185ebca87)
        ^ (y as i64 as u64).wrapping_mul(0xc2b2ae3d27d4eb4f)
        ^ (z as i64 as u64).wrapping_mul(0x165667b19e3779f9);
    match hash(salt) % 4 {
        0 | 1 => "Zombie",
        2 => "Skeleton",
        _ => "Spider",
    }
}
fn distance(a: PlayerPosition, b: PlayerPosition) -> f64 {
    (a.x - b.x).hypot(a.z - b.z)
}
fn distance_3d(a: PlayerPosition, b: PlayerPosition) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)).sqrt()
}
/// Choose a live wooden door in a generated village, bounded annex, or a
/// nearby player-built house with asymmetric sky exposure. The independent
/// door fallback is deliberately bounded; it does not implement a complete
/// vanilla village registry, population cap, or door lifetime.
/// Re-read the authoritative world each tick so removed doors are not AI goals.
fn villager_door_destination(world: &World, at: PlayerPosition) -> Option<PlayerPosition> {
    if !at.is_valid() {
        return None;
    }
    let (x, z) = (at.x.floor() as i32, at.z.floor() as i32);
    // A generated village can have live doors on a different floor.
    // Still consider nearby player-built houses instead of suppressing the
    // fallback whenever any generated door happens to exist.
    let doors = world
        .village_doors_with_bounded_annex(x, z)
        .unwrap_or_default()
        .into_iter()
        .chain(world.independent_player_village_doors_near_at(x, at.y.floor() as i32, z));
    doors
        .into_iter()
        .filter_map(|[x, y, z]| {
            // Keep generated-village and independent-house goals on the same
            // reachable floor. A 3D radius alone lets a villager on an upper
            // floor walk toward an inaccessible ground-level door.
            if (f64::from(y) - at.y.floor()).abs() > 4.0 {
                return None;
            }
            // 1.7.10 lower-half metadata bits 0..1 encode door facing.
            // Approach from the exterior side rather than the closed door.
            let (tx, tz) = match world.get(x, y, z) & 3 {
                0 => (f64::from(x) + 0.5, f64::from(z) + 1.5),
                1 => (f64::from(x) - 0.5, f64::from(z) + 0.5),
                2 => (f64::from(x) + 0.5, f64::from(z) - 0.5),
                _ => (f64::from(x) + 1.5, f64::from(z) + 0.5),
            };
            let target = PlayerPosition {
                x: tx,
                y: f64::from(y),
                z: tz,
            };
            (distance_3d(at, target) <= 32.0).then_some(target)
        })
        .min_by(|a, b| distance_3d(at, *a).total_cmp(&distance_3d(at, *b)))
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
/// In Java 1.7.10, both spider variants acquire targets in light below 12.
/// Night reduces the effective skylight, while block light remains unchanged.
fn spider_bright_light(block: u8, sky: u8, now: u64) -> bool {
    let sky = sky.saturating_sub(if sunlight(now) { 0 } else { 11 });
    block.max(sky) >= 12
}
fn spawn_light_ok(block: u8, sky: u8, now: u64, passive: bool) -> bool {
    let effective_sky = sky.saturating_sub(if sunlight(now) { 0 } else { 11 });
    if passive {
        block.max(effective_sky) >= 9
    } else {
        block < 8 && effective_sky < 8
    }
}

/// Vanilla animals need grass and sufficient *combined* light, not direct
/// access to the sky. A roofed, torch-lit grass enclosure remains eligible.
fn natural_passive_habitat_ok(ground: u16, block: u8, sky: u8, now: u64) -> bool {
    ground == 2 && spawn_light_ok(block, sky, now, true)
}

/// Vanilla 1.7.10 slime chunks allow underground slimes at Y < 40
/// regardless of light level. Swamp surface slimes and every other species
/// still obey the ordinary spawn-light check.
fn natural_spawn_species_light_ok(
    kind: Kind,
    world_seed: u64,
    at: (i32, i32, i32),
    light: (u8, u8),
    now: u64,
    passive: bool,
) -> bool {
    (kind == Kind::Slime && is_slime_cave_location(world_seed, at.0, at.1, at.2))
        || spawn_light_ok(light.0, light.1, now, passive)
}
/// True only when a block's unit interval has positive overlap with the
/// entity's interval. Widen before adding one: `block + 1` overflows at
/// i32::MAX even though the floating-point AABB is still representable.
fn block_overlaps_axis(block: i32, min: f64, max: f64) -> bool {
    let start = f64::from(block);
    start < max && start + 1.0 > min
}

/// Ghasts store their position at the centre of a 4x4x4 AABB. Use
/// the same precise solid-block collision shapes as other natural spawns,
/// while rejecting fluids separately (fluids have no collision shapes).
/// Merely touching the outer face of a block is not an intersection.
fn ghast_has_clearance(world: &World, p: PlayerPosition) -> bool {
    if !p.is_valid() {
        return false;
    }
    let feet = PlayerPosition { y: p.y - 2.0, ..p };
    if !feet.is_valid() || physics::collides_sized(world, &feet, 2.0, 4.0) {
        return false;
    }
    let (min_x, max_x) = (p.x - 2.0, p.x + 2.0);
    let (min_y, max_y) = (p.y - 2.0, p.y + 2.0);
    let (min_z, max_z) = (p.z - 2.0, p.z + 2.0);
    ((min_x.floor() as i32)..=(max_x.floor() as i32)).all(|x| {
        ((min_y.floor() as i32)..=(max_y.floor() as i32)).all(|y| {
            ((min_z.floor() as i32)..=(max_z.floor() as i32)).all(|z| {
                if !block_overlaps_axis(x, min_x, max_x)
                    || !block_overlaps_axis(y, min_y, max_y)
                    || !block_overlaps_axis(z, min_z, max_z)
                {
                    return true;
                }
                !matches!(world.get(x, y, z) >> 4, 8..=11)
            })
        })
    })
}

/// Return the horizontal half-width and vertical height used by natural
/// spawning. Keep entity-to-entity and terrain clearance on the same hull.
fn natural_spawn_body_dimensions(kind: Kind, size: u8) -> (f64, f64) {
    if kind.splitting() {
        let edge = 0.6 * f64::from(size);
        return (edge / 2.0, edge);
    }
    let half_width = match kind {
        Kind::Ghast => 2.0,
        Kind::Spider | Kind::Horse | Kind::IronGolem => 0.7,
        Kind::Cow | Kind::Pig | Kind::Sheep => 0.45,
        Kind::CaveSpider => 0.35,
        Kind::Chicken => 0.2,
        _ => 0.3,
    };
    // Use each species' 1.7.10 Entity#setSize height. A blanket two-block
    // hull incorrectly rejects pigs/chickens/spiders below low ceilings and
    // changes the vertical entity-overlap test for natural spawn packs.
    let height = match kind {
        Kind::Ghast => 4.0,
        Kind::Enderman | Kind::IronGolem => 2.9,
        Kind::Horse => 1.6,
        Kind::Creeper => 1.7,
        Kind::Zombie | Kind::ZombiePigman | Kind::Blaze | Kind::Villager => 1.8,
        Kind::Witch => 1.95,
        // Java 1.7.10 skeletons were 1.95 tall; 1.99 was introduced in 1.9.
        Kind::Skeleton => 1.95,
        Kind::Cow | Kind::Sheep => 1.3,
        Kind::Spider | Kind::Pig => 0.9,
        Kind::Chicken => 0.7,
        Kind::CaveSpider => 0.5,
        _ => 2.0,
    };
    (half_width, height)
}

/// Vanilla natural spawns must not overlap already loaded living entities.
/// Check all three AABB axes, including the chosen slime size and floating
/// ghasts (whose stored position is the center rather than the feet).
fn natural_spawn_entity_clearance(
    entities: &BTreeMap<i32, Mob>,
    kind: Kind,
    at: PlayerPosition,
    size: u8,
) -> bool {
    let (half_width, height) = natural_spawn_body_dimensions(kind, size);
    let bottom = if kind == Kind::Ghast {
        at.y - height / 2.0
    } else {
        at.y
    };
    entities.values().all(|mob| {
        let (other_half, other_height) = natural_spawn_body_dimensions(mob.kind, mob.size);
        let other_bottom = if mob.kind == Kind::Ghast {
            mob.position.y - other_height / 2.0
        } else {
            mob.position.y
        };
        (at.x - mob.position.x).abs() >= half_width + other_half
            || (at.z - mob.position.z).abs() >= half_width + other_half
            || bottom >= other_bottom + other_height
            || other_bottom >= bottom + height
    })
}

/// Flying ghasts must be able to spawn in air. A generic ground-support
/// test here would discard every valid floating ghast candidate.
fn natural_spawn_body_ok(world: &World, kind: Kind, at: PlayerPosition, size: u8) -> bool {
    if kind == Kind::Ghast {
        return ghast_has_clearance(world, at);
    }
    if !physics::supported(world, &at) {
        return false;
    }
    // Fluids have no solid collision boxes. Test the horizontal extent of
    // the species' body, not just the center column: a spider/horse can
    // overlap liquid in the next block while its feet remain on dry ground.
    // EntitySlime#setSlimeSize uses a 0.6 * size cube in Java 1.7.10.
    // The selected natural-spawn size must be used before inserting it.
    let (half_width, body_height) = natural_spawn_body_dimensions(kind, size);
    let y = at.y.floor() as i32;
    // The player-sized physics collision check misses solid blocks intersecting
    // wide spiders/horses or the upper part of a large slime/enderman.
    if physics::collides_sized(world, &at, half_width, body_height) {
        return false;
    }
    let max_y = (at.y + body_height).ceil() as i32;
    let min_x = (at.x - half_width).floor() as i32;
    let max_x = (at.x + half_width).floor() as i32;
    let min_z = (at.z - half_width).floor() as i32;
    let max_z = (at.z + half_width).floor() as i32;
    (min_x..=max_x).all(|x| {
        (min_z..=max_z).all(|z| {
            // A face-touching block has zero overlap and is not inside the
            // entity's AABB (important at exact integer boundaries).
            if !block_overlaps_axis(x, at.x - half_width, at.x + half_width)
                || !block_overlaps_axis(z, at.z - half_width, at.z + half_width)
            {
                return true;
            }
            (y..max_y).all(|by| !matches!(world.get(x, by, z) >> 4, 8..=11))
        })
    })
}

/// A bounded, collision-swept aerial step. Ghasts keep their distance and
/// circle their target while blazes try to gain altitude for ranged attacks.
/// Flight never requires a supporting ground block.
fn flying_step(
    world: &World,
    kind: Kind,
    from: PlayerPosition,
    target: PlayerPosition,
    tick: u64,
) -> Option<PlayerPosition> {
    if !matches!(kind, Kind::Ghast | Kind::Blaze) || !from.is_valid() || !target.is_valid() {
        return None;
    }
    let dx = target.x - from.x;
    let dz = target.z - from.z;
    let horizontal = dx.hypot(dz);
    let (step_x, step_z) = if horizontal > 0.01 {
        let (ux, uz) = (dx / horizontal, dz / horizontal);
        if kind == Kind::Ghast {
            if horizontal < 16.0 {
                (-ux * 0.10, -uz * 0.10)
            } else if horizontal > 28.0 {
                (ux * 0.10, uz * 0.10)
            } else {
                let sign = if (tick / 80).is_multiple_of(2) {
                    1.0
                } else {
                    -1.0
                };
                (-uz * 0.10 * sign, ux * 0.10 * sign)
            }
        } else if horizontal < 5.0 {
            (-ux * 0.10, -uz * 0.10)
        } else if horizontal > 10.0 {
            (ux * 0.10, uz * 0.10)
        } else {
            (0.0, 0.0)
        }
    } else {
        (0.0, 0.0)
    };
    let desired_y = target.y + if kind == Kind::Ghast { 8.0 } else { 3.0 };
    let step_y = (desired_y - from.y).clamp(-0.08, 0.08);
    let next = PlayerPosition {
        x: from.x + step_x,
        y: from.y + step_y,
        z: from.z + step_z,
    };
    if next.is_valid()
        && physics::valid_movement(world, &from, &next)
        && (kind != Kind::Ghast || ghast_has_clearance(world, next))
        && distance_3d(from, next) > 0.0001
    {
        Some(next)
    } else {
        None
    }
}

/// Endermen are almost three blocks tall. A player-sized (1.8 block)
/// collision check alone would teleport the head into a solid block, water
/// or lava. Require a dry three-block column and a real supporting floor.
fn enderman_teleport_safe(world: &World, position: PlayerPosition) -> bool {
    if !position.is_valid() || !(1.0..=252.0).contains(&position.y) {
        return false;
    }
    let x = position.x.floor() as i32;
    let y = position.y.floor() as i32;
    let z = position.z.floor() as i32;
    (0..3).all(|dy| world.get(x, y + dy, z) >> 4 == 0)
        && physics::supported(world, &position)
        && !physics::collides(world, &position)
}

fn sky_exposed(world: &World, p: PlayerPosition) -> bool {
    let x = p.x.floor() as i32;
    let z = p.z.floor() as i32;
    ((p.y as i32 + 2)..=255).all(|y| world.get(x, y, z) == 0)
}
/// Sample nine rays across a target's body. Solid blocks attenuate the
/// exposure before damage is distributed to players and mobs.
fn blast_exposure(world: &World, from: PlayerPosition, to: PlayerPosition) -> f64 {
    // Sample all three axes of a standing entity, not a single X/Y plane.
    // A shielding wall to the north/south must attenuate damage just like
    // a corresponding east/west wall. Bound the work to 27 short rays.
    let mut clear = 0u8;
    for ox in [-0.3, 0.0, 0.3] {
        for oz in [-0.3, 0.0, 0.3] {
            for height in [0.1, 0.9, 1.6] {
                let target = PlayerPosition {
                    x: to.x + ox,
                    y: to.y + height,
                    z: to.z + oz,
                };
                let span = distance_3d(from, target);
                let steps = (span * 3.0).ceil().max(1.0) as usize;
                let blocked = (1..steps).any(|n| {
                    let t = n as f64 / steps as f64;
                    let (x, y, z) = (
                        (from.x + (target.x - from.x) * t).floor() as i32,
                        (from.y + (target.y - from.y) * t).floor() as i32,
                        (from.z + (target.z - from.z) * t).floor() as i32,
                    );
                    !matches!(
                        world.get(x, y, z) >> 4,
                        0 | 6
                            | 8..=11
                            | 18
                            | 30
                            | 31
                            | 37..=40
                            | 50
                            | 55
                            | 59
                            | 63
                            | 65
                            | 66
                            | 69
                            | 70
                            | 72
                            | 75
                            | 76
                            | 77
                            | 78
                            | 83
                            | 90
                            | 106
                            | 111
                    )
                });
                if !blocked {
                    clear += 1;
                }
            }
        }
    }
    f64::from(clear) / 27.0
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
/// Shared 1.7.10 Mob collision dimensions for arrows and their tick snapshots.
fn projectile_hitbox(mob: &Mob) -> (f64, f64) {
    match mob.kind {
        Kind::Ghast => (2.0, 4.0),
        Kind::EnderDragon => (4.0, 8.0),
        Kind::Wither => (0.9, 3.5),
        Kind::IronGolem => (0.7, 2.7),
        Kind::Enderman => (0.45, 2.9),
        Kind::Slime | Kind::MagmaCube => {
            let edge = f64::from(mob.size) * 0.51;
            (edge, edge * 2.0)
        }
        Kind::Spider | Kind::CaveSpider => (0.7, 0.9),
        Kind::Chicken => (0.3, 0.7),
        Kind::Cow | Kind::Pig | Kind::Sheep => (0.55, 1.4),
        _ => (0.45, if mob.baby { 1.0 } else { 1.95 }),
    }
}
/// Projectile collisions must intersect the mob's body, not its feet.
fn projectile_hits_mob(mob: &Mob, point: PlayerPosition) -> bool {
    let (radius, height) = projectile_hitbox(mob);
    (point.x - mob.position.x).hypot(point.z - mob.position.z) <= radius
        && point.y >= mob.position.y - 0.25
        && point.y <= mob.position.y + height + 0.25
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
    /// Called only after the console has durably recorded the new difficulty.
    /// The next world tick despawns hostile mobs on Peaceful, while damage and
    /// natural spawning immediately consult the updated setting.
    pub fn set_difficulty(&mut self, difficulty: u8) {
        self.difficulty = Some(difficulty.min(3));
    }

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
                || value.next_id > 1_000_000
                || value.entities.len() > MAX_LOADED_MOBS
                // All three entity categories share a single Protocol 5 ID
                // space. Reject corrupt snapshots that reuse an ID.
                || value.entities.keys().any(|id| {
                    value.arrows.contains_key(id) || value.orbs.contains_key(id)
                })
                || value.arrows.keys().any(|id| value.orbs.contains_key(id))
                || value.arrows.len() > ARROW_CAP
                || value.orbs.len() > 256
                || value.spawner_next_tick.len() > 512
                || value.spawner_next_tick.keys().any(|key| {
                    let coords: Vec<_> = key.split(',').map(str::parse::<i32>).collect();
                    coords.len() != 3
                        || coords.iter().any(Result::is_err)
                        || !crate::world::in_bounds(
                            coords[0].as_ref().copied().unwrap_or(0),
                            coords[1].as_ref().copied().unwrap_or(-1),
                            coords[2].as_ref().copied().unwrap_or(0),
                        )
                })
                || value.entities.iter().any(|(&id, mob)| {
                    id < BASE_ID
                        || mob.tame_progress > 20
                        || (mob.saddled && !matches!(mob.kind, Kind::Pig | Kind::Horse))
                        || (mob.fence_anchor.is_some() && !mob.kind.passive())
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
                        || arrow.age >= 100
                        || !arrow
                            .velocity
                            .iter()
                            .all(|v| v.is_finite() && v.abs() <= 8.0)
                })
                || value
                    .orbs
                    .iter()
                    .any(|(&id, (pos, amount, _))| {
                        id < BASE_ID || !pos.is_valid() || *amount == 0
                    })
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
            far_from_player_ticks: 0,
            bite_tick: 0,
            fuse: 0,
            breed_tick: 0,
            love_until: 0,
            baby,
            age_ticks: 0,
            waypoint: None,
            sheared: false,
            next_egg_tick: self.last_tick + 6000 + u64::from(id.unsigned_abs() % 6000),
            profession: (hash(self.last_tick ^ id as u64) % 5) as u8,
            size,
            anger_until: 0,
            bright_spider: false,
            zombie_villager: false,
            // Java's uncommon break-door capability is assigned on spawn,
            // not enabled for every zombie in Hard mode.
            can_break_wooden_doors: kind == Kind::Zombie
                && hash(self.last_tick ^ id as u64 ^ 0x2a3814f7).is_multiple_of(20),
            door_break_target: None,
            door_break_started: 0,
            saddled: false,
            tamed: false,
            tame_progress: 0,
            fence_anchor: None,
            rider: None,
            leash_holder: None,
            last_ride_tick: 0,
        };
        let event = mob.spawn();
        self.entities.insert(id, mob);
        event
    }
    /// Persist a bounded 1.7.10 Mob summon *before* publishing the spawn
    /// packet. Reuse the same entity IDs, AI and save format as natural spawns.
    /// NBT and bosses require their own validated command implementation.
    pub fn summon_command(
        &mut self,
        world: &World,
        entity: &str,
        at: PlayerPosition,
    ) -> io::Result<Event> {
        let kind = match entity {
            "Zombie" => Kind::Zombie,
            "Skeleton" => Kind::Skeleton,
            "Creeper" => Kind::Creeper,
            "Spider" => Kind::Spider,
            "CaveSpider" => Kind::CaveSpider,
            "Enderman" => Kind::Enderman,
            "Slime" => Kind::Slime,
            "Witch" => Kind::Witch,
            "Cow" => Kind::Cow,
            "Pig" => Kind::Pig,
            "Sheep" => Kind::Sheep,
            "Chicken" => Kind::Chicken,
            "Horse" | "EntityHorse" => Kind::Horse,
            "Villager" => Kind::Villager,
            "VillagerGolem" => Kind::IronGolem,
            "PigZombie" => Kind::ZombiePigman,
            "LavaSlime" => Kind::MagmaCube,
            "Blaze" => Kind::Blaze,
            "Ghast" => Kind::Ghast,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unsupported 1.7.10 Mob name",
                ))
            }
        };
        // Explicit /summon is distinct from *natural* spawning. Flat worlds
        // must still run AI for summoned mobs even with natural spawning off.
        if !at.is_valid()
            || physics::collides(world, &at)
            || self.entities.len() >= MAX_LOADED_MOBS
            || self.next_id >= 1_000_000
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Mob spawn position/cap invalid",
            ));
        }
        if self.difficulty == Some(0) && !kind.peaceful_safe() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "hostile Mob cannot remain in Peaceful",
            ));
        }
        let previous_id = self.next_id;
        let packet = self.insert(kind, at, false);
        if let Err(error) = self.save(world) {
            self.entities.remove(&previous_id);
            self.next_id = previous_id;
            return Err(error);
        }
        Ok(packet)
    }

    /// An egg impact may spawn one baby chicken, under the existing Mob cap.
    pub fn hatch_egg(&mut self, world: &World, at: PlayerPosition) -> Option<Event> {
        if self.entities.len() >= MAX_LOADED_MOBS
            || self.next_id >= 1_000_000
            || !at.is_valid()
            || physics::collides(world, &at)
        {
            return None;
        }
        Some(self.insert(Kind::Chicken, at, true))
    }
    /// Set zombie-villager metadata before sending the authoritative spawn.
    /// The former villager's age and profession survive for future curing.
    fn insert_zombie_villager(
        &mut self,
        position: PlayerPosition,
        baby: bool,
        profession: u8,
    ) -> Event {
        let id = self.next_id;
        let _ = self.insert(Kind::Zombie, position, baby);
        let mob = self.entities.get_mut(&id).expect("newly inserted zombie");
        mob.zombie_villager = true;
        mob.profession = profession;
        mob.spawn()
    }

    /// Resolve zombie-on-villager melee on the server. The existing mob tick
    /// calls this once; each zombie's bite cooldown prevents duplicated hits.
    fn zombies_attack_villagers(
        &mut self,
        now: u64,
        world: &World,
        players: &[PlayerPosition],
        events: &mut Vec<Event>,
    ) {
        if self.difficulty == Some(0) {
            return;
        }
        let attackers: Vec<_> = self
            .entities
            .values()
            .filter(|mob| {
                mob.kind == Kind::Zombie
                    && mob.health > 0
                    && (mob.bite_tick == 0 || now.saturating_sub(mob.bite_tick) >= ATTACK_INTERVAL)
            })
            .map(|mob| (mob.id, mob.position))
            .collect();
        for (zombie_id, origin) in attackers {
            let victim_id = self
                .entities
                .values()
                .filter(|mob| {
                    mob.kind == Kind::Villager
                        && mob.nearby(origin, 1.6)
                        // A player closer than the villager takes target
                        // priority; the zombie must not burn its bite cooldown
                        // by attacking a collocated villager instead.
                        && !players.iter().any(|player| {
                            distance_3d(origin, *player)
                                <= distance_3d(origin, mob.position)
                        })
                        && can_see(world, origin, mob.position)
                })
                .min_by(|a, b| {
                    distance_3d(origin, a.position).total_cmp(&distance_3d(origin, b.position))
                })
                .map(|mob| mob.id);
            let Some(victim_id) = victim_id else { continue };
            if let Some(zombie) = self.entities.get_mut(&zombie_id) {
                zombie.bite_tick = now;
            }
            let Some(villager) = self.entities.get_mut(&victim_id) else {
                continue;
            };
            villager.health -= 3;
            events.push(villager.status(if villager.health <= 0 { 3 } else { 2 }));
            if villager.health > 0 {
                events.push(villager.health_metadata());
                continue;
            }
            // Old entity ID must be destroyed before any replacement is sent
            // to multiple clients; only the shared next_id allocates new IDs.
            let victim = self.entities.remove(&victim_id).expect("living victim");
            events.push(multiplayer::destroy(victim_id));
            let roll = hash(now ^ zombie_id as u64 ^ victim_id as u64);
            if zombie_infects_villager(self.difficulty.unwrap_or(2), roll)
                && self.entities.len() < MAX_LOADED_MOBS
                && self.next_id < 1_000_000
            {
                events.push(self.insert_zombie_villager(
                    victim.position,
                    victim.baby,
                    victim.profession,
                ));
            }
        }
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
            || self.entities.len() >= MAX_LOADED_MOBS
            || self.next_id >= 1_000_000
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
            nbt: None,
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
            if self.entities.len() >= MAX_LOADED_MOBS || self.next_id >= 1_000_000 {
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
        // Do not place fixed zombies/skeletons near (0,0) at world creation:
        // that bypassed all biome, light, player-radius and world-spawn checks.
        // Only the validated natural-spawn tick may create initial monsters.
        Self {
            next_id: BASE_ID,
            enabled: enabled && world.is_natural(),
            difficulty: Some(difficulty.min(3)),
            ..Self::default()
        }
    }
    /// Snapshot only living server-authoritative Mob positions for block
    /// contact sensors. Does not expose client-provided movement or mutate AI.
    pub fn contact_positions(&self) -> Vec<PlayerPosition> {
        self.entities.values().map(|mob| mob.position).collect()
    }

    /// Read-only, bounded hitboxes for dispenser arrows. No Mob state is
    /// modified while the projectile worker advances its physics.
    pub fn projectile_targets(&self) -> Vec<(i32, PlayerPosition, f64, f64)> {
        self.entities
            .values()
            .filter(|mob| mob.health > 0)
            .map(|mob| {
                let (radius, height) = projectile_hitbox(mob);
                (mob.id, mob.position, radius, height)
            })
            .collect()
    }

    /// Apply one server-confirmed dispenser arrow hit. Damage and death drops
    /// are authoritative; the normal invulnerability window prevents duplicate
    /// damage when multiple projectiles strike in the same tick.
    pub fn damage_from_dispenser_arrow(
        &mut self,
        id: i32,
        tick: u64,
    ) -> Option<(PlayerPosition, Vec<Event>)> {
        let mob = self.entities.get_mut(&id)?;
        if mob.hit_tick != 0 && tick.saturating_sub(mob.hit_tick) < 10 {
            return None;
        }
        mob.hit_tick = tick;
        mob.health -= 2;
        let killed = mob.health <= 0;
        let at = mob.position;
        let mut events = if killed {
            vec![mob.status(3), multiplayer::destroy(id)]
        } else {
            vec![mob.status(2), mob.health_metadata()]
        };
        if killed {
            let mob = self.entities.remove(&id)?;
            if let Some(drop) = mob.death_drop() {
                self.pending_drops.push(drop);
            }
            self.pending_drops.extend(mob.extra_death_drops());
            if mob.kind == Kind::EnderDragon {
                self.dragon_defeated = true;
            }
            events.extend(self.split(mob));
        }
        Some((at, events))
    }

    /// Push server-owned living entities displaced by an extending piston.
    /// The caller must durably save Mobs before publishing these teleports.
    pub fn push_by_piston(
        &mut self,
        world: &World,
        piston: &crate::world::PistonAction,
    ) -> Vec<Event> {
        if !piston.extending {
            return Vec::new();
        }
        let mut events = Vec::new();
        for mob in self.entities.values_mut() {
            if let Some(next) =
                physics::piston_push_candidate(world, &mob.position, piston.pos, piston.facing)
            {
                mob.position = next;
                events.push(multiplayer::teleport(mob.id, &next, 0.0, 0.0));
            }
        }
        events
    }

    pub fn spawn_events(&self) -> impl Iterator<Item = Event> + '_ {
        self.entities
            .values()
            .copied()
            .map(Mob::spawn)
            .chain(self.tether_snapshots())
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
        if amount == 0 || self.orbs.len() >= 256 || self.next_id >= 1_000_000 {
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
                    if mob.health > 0 {
                        events.push(mob.health_metadata());
                    }
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
                    self.pending_drops.extend(mob.extra_death_drops());
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
                vec![mob.status(2), mob.health_metadata()]
            },
            drop: if killed { mob.death_drop() } else { None },
            experience: if killed {
                u32::from(mob.experience_on_player_kill())
            } else {
                0
            },
        };
        let mut result = result;
        if killed {
            self.pending_drops.extend(mob.extra_death_drops());
            if let Some(parent) = self.entities.remove(&id) {
                if parent.kind == Kind::EnderDragon {
                    self.dragon_defeated = true;
                }
                result.events.extend(self.split(parent));
            }
        } else if mob.kind == Kind::ZombiePigman {
            // Pigmen communicate retaliation to their nearby group; an
            // unrelated Enderman never receives this anger state.
            let position = mob.position;
            self.alert_nearby_pigmen(position, tick);
        } else if matches!(mob.kind, Kind::Enderman | Kind::Spider | Kind::CaveSpider) {
            // A spider attacked in bright light must retaliate even though
            // naturally neutral spiders do not start fights on their own.
            mob.anger_until = tick.saturating_add(1200);
        }
        Some(result)
    }

    /// A single attacked pigman warns its local herd, including itself.
    /// Limits are bounded to an already loaded set of Mob entities.
    fn alert_nearby_pigmen(&mut self, attacked: PlayerPosition, tick: u64) {
        let until = tick.saturating_add(1200);
        for peer in self.entities.values_mut() {
            if peer.kind != Kind::ZombiePigman {
                continue;
            }
            let dx = peer.position.x - attacked.x;
            let dz = peer.position.z - attacked.z;
            let dy = (peer.position.y - attacked.y).abs();
            if dx * dx + dz * dz <= 32.0 * 32.0 && dy <= 16.0 {
                peer.anger_until = peer.anger_until.max(until);
            }
        }
    }

    /// Hard-only wooden-door breaking (240 ticks, server authority).
    /// The caller holds the world and mob locks and broadcasts BOTH changed
    /// block coordinates after this method returns. Existing doors are never
    /// edited on Easy/Normal or by zombies without the rare capability.
    pub fn break_wooden_doors(
        &mut self,
        world: &mut World,
        world_path: &Path,
        now: u64,
        players: &[PlayerPosition],
    ) -> io::Result<Vec<(i32, i32, i32)>> {
        if self.difficulty != Some(3) {
            for mob in self.entities.values_mut() {
                mob.door_break_target = None;
            }
            return Ok(Vec::new());
        }
        let mut targets = players.to_vec();
        targets.extend(
            self.entities
                .values()
                .filter(|mob| mob.kind == Kind::Villager)
                .map(|mob| mob.position),
        );
        let mut broken = Vec::new();
        for mob in self.entities.values_mut() {
            if mob.kind != Kind::Zombie || !mob.can_break_wooden_doors || mob.health <= 0 {
                continue;
            }
            let current = targets
                .iter()
                .copied()
                .filter(|target| distance_3d(*target, mob.position) <= 16.0)
                .filter_map(|target| {
                    closed_wooden_door_on_path(world, mob.position, target)
                        .map(|door| (distance_3d(target, mob.position), door))
                })
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .map(|(_, door)| door);
            if current != mob.door_break_target {
                mob.door_break_target = current;
                mob.door_break_started = now;
                continue;
            }
            let Some([x, y, z]) = current else { continue };
            if now.saturating_sub(mob.door_break_started) < 240 {
                continue;
            }
            // A paired world transaction never publishes half a door.
            if world.update_many_and_save(world_path, &[(x, y, z, 0), (x, y + 1, z, 0)])? {
                broken.extend([(x, y, z), (x, y + 1, z)]);
                self.pending_drops.push((
                    Stack {
                        id: 324, // wooden door item
                        count: 1,
                        damage: 0,
                        nbt: None,
                    },
                    (x, y, z),
                ));
            }
            mob.door_break_target = None;
            mob.door_break_started = 0;
            if broken.len() >= 8 {
                break; // maximum four paired door edits per caller pass
            }
        }
        if !broken.is_empty() {
            self.save(world)?;
        }
        Ok(broken)
    }

    /// Grazing is server-owned and persisted before clients see sheep wool
    /// regrowth. Each sheep has a bounded chance every other game tick:
    /// adults 1/500, babies 1/25. Grass blocks turn into dirt on success.
    /// At most four grazing edits run in a single global tick.
    pub fn graze(
        &mut self,
        world: &mut World,
        world_path: &Path,
        tick: u64,
    ) -> io::Result<Vec<GrazingUpdate>> {
        if tick <= self.last_graze_tick || !tick.is_multiple_of(2) {
            return Ok(Vec::new());
        }
        self.last_graze_tick = tick;
        let mut events = Vec::new();
        for mob in self.entities.values_mut() {
            if events.len() >= 4 {
                break;
            }
            if mob.kind != Kind::Sheep {
                continue;
            }
            let chance = if mob.baby { 25 } else { 500 };
            if !hash(tick ^ (mob.id as u64).wrapping_mul(0x9e3779b97f4a7c15)).is_multiple_of(chance)
            {
                continue;
            }
            let (x, y, z) = (
                mob.position.x.floor() as i32,
                mob.position.y.floor() as i32 - 1,
                mob.position.z.floor() as i32,
            );
            if !(0..255).contains(&y) || world.get(x, y, z) >> 4 != 2 {
                continue;
            }
            // Do not mutate the Mob before the world snapshot commits.
            if world.update_and_save(world_path, x, y, z, 3 << 4)? {
                if mob.sheared {
                    mob.sheared = false;
                    let mut body = mob.id.to_be_bytes().to_vec();
                    body.extend_from_slice(&[16, 0, 0x7f]);
                    events.push((Some(Event { id: 0x1c, body }), (x, y, z)));
                } else {
                    // A grass change still needs a block packet, even when
                    // the sheep already has its wool.
                    events.push((None, (x, y, z)));
                }
            }
        }
        if !events.is_empty() {
            self.save(world)?;
        }
        Ok(events)
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
                nbt: None,
            },
            Event { id: 0x1c, body },
        ))
    }

    /// Only a nearby, eligible animal can receive a saddle (item debit is caller-owned).
    pub fn can_saddle(&self, id: i32, player: PlayerPosition) -> bool {
        self.entities.get(&id).is_some_and(|mob| {
            matches!(mob.kind, Kind::Pig | Kind::Horse)
                && (mob.kind == Kind::Pig || mob.tamed)
                && !mob.baby
                && !mob.saddled
                && mob.nearby(player, 3.0)
        })
    }
    pub fn saddle(&mut self, id: i32, player: PlayerPosition) -> Option<Event> {
        if !self.can_saddle(id, player) {
            return None;
        }
        let mob = self.entities.get_mut(&id)?;
        mob.saddled = true;
        Some(mob.mount_metadata())
    }
    pub fn use_mount(&mut self, id: i32, rider: i32, player: PlayerPosition) -> Option<Vec<Event>> {
        if self.entities.values().any(|mob| mob.rider == Some(rider)) {
            return None;
        }
        let mob = self.entities.get_mut(&id)?;
        if !matches!(mob.kind, Kind::Pig | Kind::Horse)
            || mob.baby
            || mob.rider.is_some()
            || !mob.nearby(player, 4.0)
        {
            return None;
        }
        let mut result = Vec::new();
        if mob.kind == Kind::Horse && !mob.tamed {
            mob.tame_progress = mob.tame_progress.saturating_add(1);
            if mob.tame_progress >= 3 {
                mob.tamed = true;
                result.push(mob.mount_metadata());
                result.push(mob.status(7)); // taming hearts
            }
        } else if !mob.saddled {
            return None;
        }
        mob.rider = Some(rider);
        result.push(crate::vehicles::attachment(rider, id));
        Some(result)
    }
    pub fn rider_position(&self, rider: i32) -> Option<PlayerPosition> {
        self.entities
            .values()
            .find(|mob| mob.rider == Some(rider))
            .map(|mob| PlayerPosition {
                y: (mob.position.y + 0.8).min(255.8),
                ..mob.position
            })
    }
    pub fn is_riding(&self, rider: i32) -> bool {
        self.rider_position(rider).is_some()
    }
    pub fn dismount(&mut self, rider: i32) -> Option<Event> {
        let mob = self
            .entities
            .values_mut()
            .find(|mob| mob.rider == Some(rider))?;
        mob.rider = None;
        Some(crate::vehicles::attachment(rider, -1))
    }
    pub fn steer_rider(
        &mut self,
        rider: i32,
        world: &World,
        forward: f32,
        yaw: f32,
        holding_carrot: bool,
        now: u64,
    ) -> Vec<Event> {
        if !forward.is_finite() || !yaw.is_finite() {
            return Vec::new();
        }
        let Some(mob) = self
            .entities
            .values_mut()
            .find(|mob| mob.rider == Some(rider))
        else {
            return Vec::new();
        };
        if now <= mob.last_ride_tick {
            return Vec::new();
        }
        mob.last_ride_tick = now;
        if !mob.saddled || (mob.kind == Kind::Pig && !holding_carrot) {
            return Vec::new();
        }
        let radians = (yaw as f64).to_radians();
        let speed = if mob.kind == Kind::Horse { 0.22 } else { 0.11 };
        let step = f64::from(forward.clamp(-1.0, 1.0)) * speed;
        let next = PlayerPosition {
            x: mob.position.x - radians.sin() * step,
            z: mob.position.z + radians.cos() * step,
            ..mob.position
        };
        if next.is_valid()
            && physics::valid_movement(world, &mob.position, &next)
            && physics::supported(world, &next)
        {
            mob.position = next;
            return vec![multiplayer::teleport(mob.id, &next, yaw, 0.0)];
        }
        Vec::new()
    }
    /// Leash packet 0x1B is server-owned; a fence tie remains in the saved Mob.
    pub fn can_leash(&self, id: i32, holder: i32, player: PlayerPosition) -> bool {
        holder > 0
            && self.entities.get(&id).is_some_and(|mob| {
                mob.kind.passive()
                    && !mob.baby
                    && mob.rider.is_none()
                    && mob.leash_holder.is_none()
                    && mob.fence_anchor.is_none()
                    && mob.nearby(player, 5.0)
            })
    }
    pub fn leash(&mut self, id: i32, holder: i32, player: PlayerPosition) -> Option<Event> {
        if !self.can_leash(id, holder, player) {
            return None;
        }
        self.entities.get_mut(&id)?.leash_holder = Some(holder);
        Some(Self::leash_packet(id, holder))
    }
    fn leash_packet(id: i32, holder: i32) -> Event {
        let mut body = Vec::with_capacity(9);
        body.extend_from_slice(&id.to_be_bytes());
        body.extend_from_slice(&holder.to_be_bytes());
        body.push(1);
        Event { id: 0x1b, body }
    }
    fn knot_id(mob_id: i32) -> i32 {
        2_000_000 + mob_id
    }
    fn knot_spawn(mob_id: i32, anchor: [i32; 3]) -> Event {
        let mut body = Vec::new();
        write_varint(Self::knot_id(mob_id) as u32, &mut body);
        body.push(77);
        for value in anchor {
            let fixed = ((f64::from(value) + 0.5) * 32.0) as i32;
            body.extend_from_slice(&fixed.to_be_bytes());
        }
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(&0i32.to_be_bytes());
        Event { id: 0x0e, body }
    }
    pub fn tie_fence(&mut self, holder: i32, anchor: [i32; 3]) -> Vec<Event> {
        let mut events = Vec::new();
        for mob in self.entities.values_mut() {
            let dx = mob.position.x - f64::from(anchor[0]);
            let dz = mob.position.z - f64::from(anchor[2]);
            if mob.leash_holder == Some(holder) && dx.hypot(dz) <= 8.0 && events.len() < 32 {
                mob.leash_holder = None;
                mob.fence_anchor = Some(anchor);
                events.push(Self::knot_spawn(mob.id, anchor));
                events.push(Self::leash_packet(mob.id, Self::knot_id(mob.id)));
            }
        }
        events
    }
    pub fn untie_fence(&mut self, anchor: [i32; 3]) -> Vec<Event> {
        let mut result = Vec::new();
        for mob in self.entities.values_mut() {
            if mob.fence_anchor == Some(anchor) {
                mob.fence_anchor = None;
                result.push(Self::leash_packet(mob.id, -1));
                result.push(multiplayer::destroy(Self::knot_id(mob.id)));
            }
        }
        result
    }
    pub fn release_player(&mut self, holder: i32) -> Vec<Event> {
        let mut result = Vec::new();
        if let Some(detach) = self.dismount(holder) {
            result.push(detach);
        }
        for mob in self.entities.values_mut() {
            if mob.leash_holder == Some(holder) {
                mob.leash_holder = None;
                result.push(Self::leash_packet(mob.id, -1));
            }
        }
        result
    }
    pub fn tether_snapshots(&self) -> Vec<Event> {
        let mut result = Vec::new();
        for mob in self.entities.values() {
            if let Some(anchor) = mob.fence_anchor {
                result.push(Self::knot_spawn(mob.id, anchor));
                result.push(Self::leash_packet(mob.id, Self::knot_id(mob.id)));
            } else if let Some(holder) = mob.leash_holder {
                result.push(Self::leash_packet(mob.id, holder));
            }
            if let Some(rider) = mob.rider {
                result.push(crate::vehicles::attachment(rider, mob.id));
            }
        }
        result
    }
    /// Follow a verified player entity or a persisted fence anchor once per tick.
    pub fn advance_leashes(
        &mut self,
        world: &World,
        now: u64,
        players: &[(i32, PlayerPosition)],
    ) -> Vec<Event> {
        if now <= self.last_leash_tick {
            return Vec::new();
        }
        self.last_leash_tick = now;
        let mut events = Vec::new();
        for mob in self.entities.values_mut() {
            let destination = if let Some(anchor) = mob.fence_anchor {
                Some(PlayerPosition {
                    x: f64::from(anchor[0]) + 0.5,
                    y: f64::from(anchor[1]),
                    z: f64::from(anchor[2]) + 0.5,
                })
            } else {
                mob.leash_holder.and_then(|id| {
                    players
                        .iter()
                        .find(|(owner, _)| *owner == id)
                        .map(|(_, p)| *p)
                })
            };
            let Some(target) = destination else { continue };
            let dx = target.x - mob.position.x;
            let dz = target.z - mob.position.z;
            let gap = dx.hypot(dz);
            if gap > 10.0 {
                mob.leash_holder = None;
                if mob.fence_anchor.take().is_some() {
                    events.push(multiplayer::destroy(Self::knot_id(mob.id)));
                }
                events.push(Self::leash_packet(mob.id, -1));
                continue;
            }
            if gap > 2.5 && gap.is_finite() && now.is_multiple_of(2) {
                let step = 0.18;
                let next = PlayerPosition {
                    x: mob.position.x + dx / gap * step,
                    z: mob.position.z + dz / gap * step,
                    ..mob.position
                };
                if next.is_valid()
                    && physics::valid_movement(world, &mob.position, &next)
                    && physics::supported(world, &next)
                {
                    mob.position = next;
                    events.push(multiplayer::teleport(mob.id, &next, 0.0, 0.0));
                }
            }
        }
        events
    }
    /// Animal breeding consumes feed only if a nearby same-kind partner exists.
    /// Each parent must be fed independently within a 600-tick love window.
    /// A single feeding produces hearts, NOT a calf; after breeding, both
    /// parents have the vanilla 6,000-tick cooldown. The caller debits only
    /// successful feed operations and broadcasts all authoritative events.
    pub fn feed(
        &mut self,
        id: i32,
        player: PlayerPosition,
        held: Option<Stack>,
        tick: u64,
    ) -> Option<Vec<Event>> {
        let first = *self.entities.get(&id)?;
        let food = held?.id;
        if !first.nearby(player, 3.0)
            || self.entities.len() >= MAX_LOADED_MOBS
            || self.next_id >= 1_000_000
            || !first.kind.passive()
            || first.baby
            || !first.kind.breeding_food(food)
            || (first.kind == Kind::Horse && !first.tamed)
            || (first.breed_tick != 0 && tick.saturating_sub(first.breed_tick) < 6000)
            || tick < first.love_until
        {
            return None;
        }

        // Vanilla love lasts 600 ticks, even when no mate is present.
        self.entities.get_mut(&id)?.love_until = tick.saturating_add(600);
        let mut events = vec![first.status(18)];
        let mate_id = self
            .entities
            .values()
            .find(|mate| {
                mate.id != id
                    && mate.kind == first.kind
                    && (mate.kind != Kind::Horse || mate.tamed)
                    && !mate.baby
                    && mate.love_until > tick
                    && (mate.breed_tick == 0 || tick.saturating_sub(mate.breed_tick) >= 6000)
                    && distance_3d(mate.position, first.position) < 7.0
            })
            .map(|mate| mate.id);
        if let Some(mate_id) = mate_id {
            for parent_id in [id, mate_id] {
                if let Some(parent) = self.entities.get_mut(&parent_id) {
                    parent.love_until = 0;
                    parent.breed_tick = tick;
                }
            }
            events.push(self.insert(first.kind, first.position, true));
        }
        Some(events)
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
            || self.next_id >= 1_000_000
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
    /// and the world's propagated sky and block light. Runtime safety limits
    /// are separate from the vanilla per-eligible-chunk cap calculation.
    fn spawn_natural(
        &mut self,
        now: u64,
        world: &World,
        lighting: &mut LightSampler<'_>,
        players: &[PlayerPosition],
        events: &mut Vec<Event>,
    ) {
        if !self.enabled
            || self.natural_spawning_paused
            || players.is_empty()
            || self.entities.len() >= MAX_LOADED_MOBS
            || !now.is_multiple_of(20)
        {
            return;
        }
        // Mob-cap accounting includes finite player heights outside build
        // limits, but these snapshots must not anchor a spawn attempt. Only
        // in-world players may consume one of the four bounded origin rolls.
        let valid_players: Vec<_> = players
            .iter()
            .copied()
            .filter(PlayerPosition::is_valid)
            .collect();
        if valid_players.is_empty() {
            return;
        }
        let passive = self
            .entities
            .values()
            .filter(|mob| mob.kind.counts_toward_natural_cap(true))
            .count();
        let hostile = self
            .entities
            .values()
            .filter(|m| m.kind.counts_toward_natural_cap(false))
            .count();
        let hostile_cap = natural_spawn_capacity(players, VANILLA_HOSTILE_CAP);
        let passive_cap = natural_spawn_capacity(players, VANILLA_PASSIVE_CAP);
        let dimension = world.dimension();
        for index in 0..4u64 {
            let seed = hash(
                now ^ index.wrapping_mul(0x9e3779b97f4a7c15) ^ world.natural_seed().unwrap_or(0),
            );
            let player = valid_players[(seed as usize) % valid_players.len()];
            let (offset_x, offset_z) = natural_spawn_horizontal_offset(seed);
            // A world-border coordinate plus a bounded spawn offset can
            // exceed i32. Never wrap or clamp it into a different chunk.
            let Some(x) = natural_spawn_offset_block(player.x, offset_x) else {
                continue;
            };
            let Some(z) = natural_spawn_offset_block(player.z, offset_z) else {
                continue;
            };
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
            // Species has not been chosen yet: demanding ground support here
            // silently excludes every otherwise valid flying Ghast. Validate
            // the selected species' body below for each attempted pack member.
            if !natural_spawn_origin_ok(world, position, &valid_players) {
                continue;
            }
            let biome = world.biome_at(x, z);
            // Mushroom islands have no ordinary animal/monster spawn entries
            // in Java 1.7.10. Mooshrooms need their own species implementation.
            if dimension == crate::dimensions::Dimension::Overworld
                && !overworld_biome_allows_natural_spawn(biome)
            {
                continue;
            }
            let overworld = dimension == crate::dimensions::Dimension::Overworld;
            let (block, sky) = lighting.light_at(x, y, z);
            let is_passive = overworld
                && now.is_multiple_of(400)
                && natural_spawn_passive_roll(seed)
                && natural_passive_habitat_ok(world.get(x, y - 1, z) >> 4, block, sky, now);
            // Select the species before applying light rules: slime-chunk
            // cave spawns are legal even at light level 15.
            let (kind, swamp_bonus) = if is_passive {
                if passive >= passive_cap || !overworld_biome_allows_passive_spawn(biome) {
                    continue;
                }
                (
                    overworld_passive_kind(biome, hash(seed ^ 0x31acb4105ff327ab)),
                    false,
                )
            } else {
                if self.difficulty == Some(0) || hostile >= hostile_cap {
                    continue;
                }
                match dimension {
                    crate::dimensions::Dimension::Overworld => {
                        overworld_monster_entry(biome, hash(seed ^ 0xd1a7c4b500137e6d))
                    }
                    crate::dimensions::Dimension::Nether => {
                        // Blaze spawning is limited to the existing fortress substrate.
                        let fortress = world.get(x, y - 1, z) >> 4 == 112;
                        let kind = if fortress && seed.is_multiple_of(4) {
                            Kind::Blaze
                        } else if (seed >> 24).is_multiple_of(12) {
                            Kind::Ghast
                        } else if (seed >> 24).is_multiple_of(8) {
                            Kind::MagmaCube
                        } else {
                            Kind::ZombiePigman
                        };
                        (kind, false)
                    }
                    crate::dimensions::Dimension::End => (Kind::Enderman, false),
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
            let spawn_position = if kind == Kind::Ghast {
                PlayerPosition {
                    y: position.y + 4.0,
                    ..position
                }
            } else {
                position
            };
            if kind == Kind::Ghast
                && (!spawn_position.is_valid() || !ghast_has_clearance(world, spawn_position))
            {
                continue;
            }
            // Once a species is selected, attempt a bounded same-species
            // pack. Recheck every proposed position: one safe center does not
            // make nearby blocks/biomes/light levels safe. This preserves the
            // older server's CPU and snapshot guards (at most six per cycle).
            let mobs_before_attempt = self.entities.len();
            let pack_attempts = natural_pack_attempts_for_entry(kind, seed, swamp_bonus);
            for member in 0..pack_attempts {
                if self.entities.len() >= MAX_LOADED_MOBS || self.next_id >= 1_000_000 {
                    break;
                }
                let candidate = if member == 0 {
                    spawn_position
                } else {
                    let h = hash(seed ^ (member as u64).wrapping_mul(0x517cc1b727220a95));
                    let dx = (h % 6) as i32 - ((h >> 8) % 6) as i32;
                    let dz = ((h >> 16) % 6) as i32 - ((h >> 24) % 6) as i32;
                    let Some(nx) = x.checked_add(dx) else {
                        continue;
                    };
                    let Some(nz) = z.checked_add(dz) else {
                        continue;
                    };
                    let ny = if overworld && height.is_some_and(|surface| y > surface) {
                        world
                            .surface_height_at(nx, nz)
                            .map_or(y, |surface| surface + 1)
                    } else {
                        y
                    };
                    PlayerPosition {
                        x: f64::from(nx) + 0.5,
                        y: f64::from(ny) + if kind == Kind::Ghast { 4.0 } else { 0.0 },
                        z: f64::from(nz) + 0.5,
                    }
                };
                if !candidate.is_valid()
                    || !natural_spawn_location_ok(world, candidate, &valid_players)
                    || !natural_spawn_body_ok(world, kind, candidate, size)
                    || !natural_spawn_entity_clearance(&self.entities, kind, candidate, size)
                {
                    continue;
                }
                let (cx, cy, cz) = (
                    candidate.x.floor() as i32,
                    candidate.y.floor() as i32,
                    candidate.z.floor() as i32,
                );
                // Every pack member rechecks its own biome and ground,
                // including species-specific entries at biome boundaries.
                if !natural_pack_member_biome_ok(world, kind, (cx, cy, cz), is_passive) {
                    continue;
                }
                let candidate_light = if overworld {
                    let light = if (cx, cy, cz) == (x, y, z) {
                        (block, sky)
                    } else {
                        lighting.light_at(cx, cy, cz)
                    };
                    if !natural_spawn_species_light_ok(
                        kind,
                        world.natural_seed().unwrap_or(0),
                        (cx, cy, cz),
                        light,
                        now,
                        is_passive,
                    ) {
                        continue;
                    }
                    Some(light)
                } else {
                    None
                };
                if kind == Kind::Blaze && world.get(cx, cy - 1, cz) >> 4 != 112 {
                    continue;
                }
                if kind == Kind::Enderman && world.get(cx, cy + 2, cz) >> 4 != 0 {
                    continue;
                }
                if kind == Kind::Slime
                    && !slime_cave_spawn_ok(
                        world.natural_seed().unwrap_or(0),
                        cx,
                        cy,
                        cz,
                        hash(seed ^ (member as u64).wrapping_mul(0x8cb9_2baa) ^ 0x69b7_c2d3),
                    )
                    && !swamp_slime_spawn_ok(
                        world.biome_at(cx, cz),
                        cy,
                        now,
                        hash(seed ^ (member as u64).wrapping_mul(0x65d2_1b0c_91fa_3e47)),
                        candidate_light.unwrap_or_else(|| lighting.light_at(cx, cy, cz)),
                    )
                {
                    continue;
                }
                let current_category = self
                    .entities
                    .values()
                    .filter(|mob| mob.kind.counts_toward_natural_cap(is_passive))
                    .count();
                if current_category >= if is_passive { passive_cap } else { hostile_cap } {
                    break;
                }
                let roll = hash(seed ^ (member as u64).wrapping_mul(0x9e3779b97f4a7c15));
                let baby = kind == Kind::Zombie && (roll >> 21).is_multiple_of(20);
                if kind == Kind::Zombie && (roll >> 26).is_multiple_of(20) {
                    events.push(self.insert_zombie_villager(candidate, baby, 0));
                } else {
                    events.push(self.insert_sized(kind, candidate, baby, size));
                }
            }
            // A rejected pack must not consume the whole spawn cycle: try
            // the remaining bounded origins when no member was inserted.
            if self.entities.len() > mobs_before_attempt {
                break; // Bound per-cycle packets and CPU after a successful pack.
            }
        }
    }
    /// Minecraft 1.7.10-style dungeon spawner activation. Check at most once
    /// per second: the block must still exist, a player must be nearby, no
    /// more than six matching mobs may inhabit the local spawner area and no
    /// more than the global entity cap may exist. A failed attempt still
    /// advances the persisted cooldown; time is the restored server tick.
    fn spawn_from_spawners(
        &mut self,
        now: u64,
        world: &World,
        players: &[PlayerPosition],
        events: &mut Vec<Event>,
    ) {
        if !self.enabled || self.difficulty == Some(0) || !now.is_multiple_of(20) {
            return;
        }
        for (x, y, z) in world.nearby_spawners(players) {
            if self.entities.len() >= MAX_LOADED_MOBS || self.next_id >= 1_000_000 {
                break;
            }
            let key = format!("{x},{y},{z}");
            if self
                .spawner_next_tick
                .get(&key)
                .is_some_and(|tick| *tick > now)
            {
                continue;
            }
            // Keep this persisted table bounded even when players explore
            // more than 512 distinct dungeons. Evict an old cooldown rather
            // than permanently disabling every newly discovered spawner.
            if self.spawner_next_tick.len() >= 512 && !self.spawner_next_tick.contains_key(&key) {
                if let Some(oldest) = self
                    .spawner_next_tick
                    .iter()
                    .min_by_key(|(_, tick)| *tick)
                    .map(|(key, _)| key.clone())
                {
                    self.spawner_next_tick.remove(&oldest);
                }
            }
            let salt = (x as i64 as u64).wrapping_mul(0x9e3779b185ebca87)
                ^ (y as i64 as u64).wrapping_mul(0xc2b2ae3d27d4eb4f)
                ^ (z as i64 as u64).wrapping_mul(0x165667b19e3779f9);
            // Use the same stable entity ID as offline Anvil TileEntity NBT.
            let kind = match dungeon_spawner_entity_id(x, y, z) {
                "Zombie" => Kind::Zombie,
                "Skeleton" => Kind::Skeleton,
                _ => Kind::Spider,
            };
            let nearby_mobs = self
                .entities
                .values()
                .filter(|mob| {
                    mob.kind == kind
                        && (mob.position.x - (f64::from(x) + 0.5)).abs() <= 8.0
                        && (mob.position.y - f64::from(y)).abs() <= 4.0
                        && (mob.position.z - (f64::from(z) + 0.5)).abs() <= 8.0
                })
                .count();
            if nearby_mobs < 6 {
                for attempt in 0..4 {
                    let roll = hash(salt ^ now ^ (attempt as u64).wrapping_mul(0x94d049bb133111eb));
                    let sx = x + (roll % 9) as i32 - 4;
                    let sz = z + ((roll >> 8) % 9) as i32 - 4;
                    let sy = y + ((roll >> 16) % 3) as i32 - 1;
                    let position = PlayerPosition {
                        x: f64::from(sx) + 0.5,
                        y: f64::from(sy),
                        z: f64::from(sz) + 0.5,
                    };
                    if !position.is_valid()
                        || !matches!(
                            world.get(sx, sy - 1, sz) >> 4,
                            1..=5 | 7 | 12 | 13 | 24 | 48 | 49 | 87 | 88 | 89 | 112 | 121
                        )
                        || world.get(sx, sy, sz) != 0
                        || world.get(sx, sy + 1, sz) != 0
                        || physics::collides(world, &position)
                    {
                        continue;
                    }
                    events.push(self.insert(kind, position, false));
                    break; // One new mob per spawner/second, bounded packet fan-out.
                }
            }
            self.spawner_next_tick.insert(
                key,
                now.saturating_add(200 + hash(salt ^ now ^ 0x5a0f) % 600),
            );
        }
    }
    /// The world GameRule controls only natural spawning, never existing mob AI.
    pub fn set_natural_spawning(&mut self, allowed: bool) {
        self.natural_spawning_paused = !allowed;
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
        let mut lighting = world.lighting_sampler();
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
        // Despawn and natural-spawn chunk selection must agree about valid
        // player coordinates. Finite X/Z outside the i32 block range are
        // corrupt snapshots, not distant players; finite Y outside build
        // height still counts as a valid reference.
        let has_valid_online_player = players
            .iter()
            .any(|&player| natural_spawn_player_chunk(player).is_some());
        for step in 0..steps {
            // A catch-up call simulates up to four distinct 20 Hz ticks.
            // Reusing `now` for every step would repeat one 1/800 despawn
            // roll rather than giving each elapsed tick its own chance.
            let simulated_tick = now - (steps - 1 - step);
            // A delayed poll may cross a 20-tick spawner or natural-spawn
            // boundary without landing on it. Run both against the simulated
            // tick so neither attempt is lost; persisted spawner cooldowns
            // still prevent duplicate activation on subsequent ticks.
            self.spawn_from_spawners(simulated_tick, world, players, &mut events);
            self.spawn_natural(simulated_tick, world, &mut lighting, players, &mut events);
            let mut shots = Vec::new();
            let mut dead = Vec::new();
            let villagers: Vec<_> = self
                .entities
                .values()
                .filter(|mob| mob.kind == Kind::Villager)
                .map(|mob| mob.position)
                .collect();
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
                // Egg laying is a timed biological action, not wandering AI.
                // Leashed or fence-anchored adult chickens must keep laying.
                if mob.kind == Kind::Chicken && !mob.baby && now >= mob.next_egg_tick {
                    mob.next_egg_tick = now + 6000 + hash(now ^ mob.id as u64) % 6000;
                    self.pending_drops.push((
                        Stack {
                            id: 344,
                            count: 1,
                            damage: 0,
                            nbt: None,
                        },
                        (
                            mob.position.x.floor() as i32,
                            mob.position.y.floor() as i32,
                            mob.position.z.floor() as i32,
                        ),
                    ));
                }
                // Restraints suppress independent wandering and ordinary despawn,
                // but must not make undead immune to daylight damage.
                // Catch-up simulates up to four distinct ticks. Checking the
                // final clock value for each step burns undead up to four
                // times on a single 20-tick boundary (or misses a boundary).
                if matches!(mob.kind, Kind::Zombie | Kind::Skeleton)
                    && sunlight(simulated_tick)
                    && sky_exposed(world, mob.position)
                    && simulated_tick.is_multiple_of(20)
                {
                    mob.health -= 1;
                    if mob.health <= 0 {
                        if let Some(drop) = mob.death_drop() {
                            self.pending_drops.push(drop);
                        }
                        self.pending_drops.extend(mob.extra_death_drops());
                        events.push(mob.status(3));
                        dead.push(mob.id);
                        continue;
                    }
                    events.push(mob.status(2));
                    events.push(mob.health_metadata());
                }
                if mob.rider.is_some() || mob.leash_holder.is_some() || mob.fence_anchor.is_some() {
                    continue; // Mounted or leashed animals never wander independently.
                }
                let nearest_distance = distance_to_players(mob.position, players);
                // With no online players, chunks have no active despawn
                // reference point. Infinity must not be treated as a player
                // farther than 128 blocks, or every hostile vanishes on logout.
                if mob.kind.despawnable() && has_valid_online_player && nearest_distance > 128.0 {
                    dead.push(mob.id);
                    continue;
                }
                // The 1/800 random despawn roll starts only after this mob
                // has spent 600 simulated ticks outside the 32-block zone.
                // Server uptime or the last combat hit is not the mob's age.
                // Vanilla's near-player reset is strictly < 32 blocks.
                // At exactly 32 the age advances, but the random cull is
                // strictly > 32 blocks (distance squared > 1024).
                if nearest_distance < 32.0 {
                    mob.far_from_player_ticks = 0;
                } else if mob.kind.despawnable() && has_valid_online_player {
                    mob.far_from_player_ticks = mob.far_from_player_ticks.saturating_add(1);
                    if nearest_distance > 32.0
                        && mob.far_from_player_ticks > 600
                        && hash(simulated_tick ^ mob.id as u64).is_multiple_of(800)
                    {
                        dead.push(mob.id);
                        continue;
                    }
                }
                if matches!(mob.kind, Kind::Spider | Kind::CaveSpider) {
                    let p = mob.position;
                    let (block, sky) = lighting.light_at(
                        p.x.floor() as i32,
                        p.y.floor() as i32,
                        p.z.floor() as i32,
                    );
                    // Evaluate light at each simulated tick, not the final catch-up
                    // time: sunset can fall between the bounded steps.
                    mob.bright_spider = spider_bright_light(block, sky, simulated_tick);
                }
                let nearest = players
                    .iter()
                    .copied()
                    .filter(|p| {
                        mob.nearby(
                            *p,
                            match mob.kind {
                                Kind::Ghast => 64.0,
                                Kind::Blaze | Kind::Wither => 48.0,
                                _ if mob.kind.passive() => 12.0,
                                _ => 24.0,
                            },
                        )
                    })
                    .min_by(|a, b| {
                        distance(*a, mob.position).total_cmp(&distance(*b, mob.position))
                    });
                let village_target = if mob.kind == Kind::Villager {
                    villager_door_destination(world, mob.position)
                } else {
                    None
                };
                let prey = (mob.kind == Kind::Zombie)
                    .then(|| {
                        villagers
                            .iter()
                            .copied()
                            .filter(|at| {
                                distance_3d(mob.position, *at) <= 16.0
                                    && can_see(world, mob.position, *at)
                            })
                            .min_by(|a, b| {
                                distance_3d(mob.position, *a)
                                    .total_cmp(&distance_3d(mob.position, *b))
                            })
                    })
                    .flatten();
                let target = match (nearest, prey) {
                    (Some(player), Some(villager))
                        if distance_3d(mob.position, villager)
                            < distance_3d(mob.position, player) =>
                    {
                        villager
                    }
                    (None, Some(villager)) => villager,
                    (Some(player), _) => village_target.unwrap_or(player),
                    (None, None) => match village_target {
                        Some(door) => door,
                        None => continue,
                    },
                };
                if matches!(mob.kind, Kind::Spider | Kind::CaveSpider)
                    && mob.bright_spider
                    && simulated_tick >= mob.anger_until
                {
                    continue; // Light >= 12: neutral, unless attacked first.
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
                        if enderman_teleport_safe(world, new_pos) {
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
                if matches!(mob.kind, Kind::Ghast | Kind::Blaze) {
                    if let Some(next) = flying_step(world, mob.kind, mob.position, target, now) {
                        mob.position = next;
                        events.push(multiplayer::teleport(mob.id, &next, 0.0, 0.0));
                    }
                    continue;
                }
                let destination = if mob.kind == Kind::Villager && village_target.is_some() {
                    target
                } else if mob.kind == Kind::Skeleton && d < 7.0 {
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
                // Without an A* waypoint retain the old flat pursuit
                // fallback, rather than demanding a multi-block vertical jump
                // directly towards a player standing high above the mob.
                let waypoint = mob.waypoint.unwrap_or(PlayerPosition {
                    y: mob.position.y,
                    ..destination
                });
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
                if let Some(candidate) =
                    navigation::advance_step(world, mob.position, waypoint, speed)
                {
                    mob.position = candidate;
                    events.push(multiplayer::teleport(mob.id, &candidate, 0.0, 0.0));
                } else if matches!(mob.kind, Kind::Spider | Kind::CaveSpider) {
                    // Both spider variants climb alongside a wall, never through it.
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
            self.zombies_attack_villagers(now, world, players, &mut events);
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
                        if enemy.health > 0 {
                            events.push(enemy.health_metadata());
                        }
                        if enemy.health <= 0 {
                            if let Some(drop) = enemy.death_drop() {
                                self.pending_drops.push(drop);
                            }
                            self.pending_drops.extend(enemy.extra_death_drops());
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
            arrow.age = arrow.age.saturating_add(1);
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
                let half_size = match arrow.kind {
                    ProjectileKind::Arrow => 0.25,
                    ProjectileKind::GhastFireball => 0.5,
                    ProjectileKind::BlazeFireball | ProjectileKind::WitherSkull => 0.15625,
                    ProjectileKind::WitchPotion => 0.125,
                };
                if !sample.is_valid() || physics::projectile_collides(world, &sample, half_size) {
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
                }
                // Skeleton arrows can hit a different mob. Preserve
                // existing special-projectile splash handling separately,
                // and never collide with the firing entity.
                if let Some(mob) = self.entities.values().find(|mob| {
                    arrow.kind == ProjectileKind::Arrow
                        && mob.id != arrow.shooter.unwrap_or(-1)
                        && projectile_hits_mob(mob, sample)
                }) {
                    arrow_hits.push((
                        mob.id,
                        arrow.damage,
                        !arrow.hostile && arrow.shooter.is_some(),
                    ));
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
        let mut earned_xp = Vec::new();
        for (id, damage, player_shot) in arrow_hits {
            if let Some(mob) = self.entities.get_mut(&id) {
                mob.health -= i16::from(damage);
                if mob.health <= 0 {
                    if let Some(drop) = mob.death_drop() {
                        self.pending_drops.push(drop);
                    }
                    self.pending_drops.extend(mob.extra_death_drops());
                    if player_shot {
                        earned_xp.push((mob.position, mob.experience_on_player_kill()));
                    }
                    events.push(mob.status(3));
                    events.push(multiplayer::destroy(id));
                    if let Some(parent) = self.entities.remove(&id) {
                        if parent.kind == Kind::EnderDragon {
                            self.dragon_defeated = true;
                        }
                        events.extend(self.split(parent));
                    }
                } else {
                    events.push(mob.status(2));
                    events.push(mob.health_metadata());
                }
            }
        }
        for (at, amount) in earned_xp {
            if let Some(event) = self.spawn_xp(at, amount) {
                events.push(event);
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
                && (!matches!(mob.kind, Kind::Spider | Kind::CaveSpider)
                    || !mob.bright_spider
                    || tick < mob.anger_until)
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
                amount =
                    amount.saturating_add(if matches!(mob.kind, Kind::Spider | Kind::CaveSpider) {
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
/// Minecraft 1.7.10 BiomeGenBase spawn entries: sheep/pig/chicken/cow
/// weights 12/10/10/8. Plains add horses (weight 5); savannas (weight 1).
/// The roll is supplied by the server, never by the client.
fn horse_spawn_weight(biome: u8) -> u64 {
    match biome {
        1 | 129 => 5,             // Plains / sunflower plains
        35 | 36 | 163 | 164 => 1, // Savanna and variants
        _ => 0,
    }
}

/// Mushroom Island and Mushroom Island Shore (IDs 14/15) clear the vanilla
/// ordinary creature and monster lists. The special Mooshroom entry is not
/// represented by the current Kind enum; never substitute a cow or hostile mob.
fn overworld_biome_allows_natural_spawn(biome: u8) -> bool {
    !matches!(biome, 14 | 15)
}

/// Vanilla 1.7.10 Desert, Snow, and Beach biome constructors clear the
/// inherited passive creature list. Stone Beach (25) and Cold Beach (26)
/// must not silently inherit ordinary farm-animal entries. Apply this filter
/// at both the pack origin and each member: an adjacent plains pack must not
/// spawn cows on a beach, in Ice Plains or Desert Hills. Mutated Desert (130)
/// and Ice Plains Spikes (140) inherit their parent's empty passive list.
/// Mesa (37-39) explicitly clears that list in 1.7.10, and mutated Mesa
/// (165-167) copies the empty list. A grass patch does not override it.
fn overworld_biome_allows_passive_spawn(biome: u8) -> bool {
    !matches!(
        biome,
        0 | 2 | 7 | 8 | 9 | 10 | 11 | 12 | 13 | 14 | 15 | 16 | 17 | 24 | 25 | 26 | 130 | 140
    ) && !matches!(biome, 37..=39 | 165..=167)
}

/// Every member of a natural passive pack must have its species in the
/// destination biome's spawn list. In 1.7.10 horses are listed only in
/// Plains/Savanna variants, even if an adjacent Forest has grass and light.
fn overworld_biome_allows_passive_species(biome: u8, kind: Kind) -> bool {
    overworld_biome_allows_passive_spawn(biome)
        && (kind != Kind::Horse || horse_spawn_weight(biome) > 0)
}

/// Apply the destination biome's spawn list to every pack member, not just
/// the first successful origin. Ground support alone does not make a river,
/// ocean, beach, or mesa grass patch eligible for ordinary farm animals.
fn natural_pack_member_biome_ok(
    world: &World,
    kind: Kind,
    at: (i32, i32, i32),
    passive: bool,
) -> bool {
    if world.dimension() != crate::dimensions::Dimension::Overworld {
        return true;
    }
    let (x, y, z) = at;
    let biome = world.biome_at(x, z);
    overworld_biome_allows_natural_spawn(biome)
        && (!passive
            || (world.get(x, y - 1, z) >> 4 == 2
                && overworld_biome_allows_passive_species(biome, kind)))
}

fn overworld_passive_kind(biome: u8, roll: u64) -> Kind {
    let weight = horse_spawn_weight(biome);
    match roll % (40 + weight) {
        0..=11 => Kind::Sheep,
        12..=21 => Kind::Pig,
        22..=31 => Kind::Chicken,
        32..=39 => Kind::Cow,
        _ => Kind::Horse,
    }
}

/// Swamp slimes are subject to the lunar phase in Java 1.7.10, unlike
/// underground slime-chunk spawns. Day zero is a full moon; day four is
/// a new moon. Use independent deterministic rolls for the 50% gate,
/// lunar brightness and the 0..7 light threshold.
fn swamp_slime_spawn_ok(biome: u8, y: i32, now: u64, seed: u64, light: (u8, u8)) -> bool {
    if !matches!(biome, 6 | 134) || !(51..70).contains(&y) {
        return false;
    }
    let moon_quarters = [4_u64, 3, 2, 1, 0, 1, 2, 3][((now / 24_000) % 8) as usize];
    if moon_quarters == 0 {
        return false;
    }
    let effective_sky = light.1.saturating_sub(if sunlight(now) { 0 } else { 11 });
    let brightness = light.0.max(effective_sky);
    let light_roll = (hash(seed ^ 0x40d5_3a2b_2d10_28f1) % 8) as u8;
    let moon_roll = hash(seed ^ 0x9f71_428c_40e2_a503) % 65_536;
    let half_roll = hash(seed ^ 0x65d2_1b0c_91fa_3e47) & 1 == 0;
    half_roll && brightness <= light_roll && moon_roll < moon_quarters * 16_384
}

/// Minecraft 1.7.10 default hostile list: four common monsters and Slime
/// each weight 100, Enderman weight 10, Witch weight 5 (total 515).
/// A selected slime must still pass its chunk/swamp and light checks; an
/// invalid species is not silently replaced with another kind.
fn overworld_monster_kind(roll: u64) -> Kind {
    match roll % 515 {
        0..=99 => Kind::Spider,
        100..=199 => Kind::Zombie,
        200..=299 => Kind::Skeleton,
        300..=399 => Kind::Creeper,
        400..=499 => Kind::Slime,
        500..=509 => Kind::Enderman,
        _ => Kind::Witch,
    }
}

/// Swampland (6) adds a second Slime entry with weight 1 and group size 1
/// to the inherited 515-weight hostile list. Its mutated biome (134) copies
/// the parent's spawn entries. Preserve the distinct entry rather
/// than silently using the inherited Slime pack size of four.
fn overworld_monster_entry(biome: u8, roll: u64) -> (Kind, bool) {
    let total_weight = if matches!(biome, 6 | 134) { 516 } else { 515 };
    let selected = roll % total_weight;
    if selected == 515 {
        (Kind::Slime, true)
    } else {
        (overworld_monster_kind(selected), false)
    }
}

/// Attempt the species-specific 1.7.10 pack size. Every attempt independently
/// checks collision, biome, distance, light and both resource limits.
fn natural_pack_attempts(kind: Kind, seed: u64) -> usize {
    match kind {
        Kind::Witch | Kind::Ghast => 1,
        Kind::Enderman => 1 + ((seed >> 40) % 4) as usize,
        Kind::Horse => 2 + ((seed >> 40) % 5) as usize,
        _ => 4,
    }
}

/// The extra Swampland Slime entry requests exactly one mob, unlike the
/// inherited four-member Slime entry. Keep this rule independently testable.
fn natural_pack_attempts_for_entry(kind: Kind, seed: u64, swamp_bonus: bool) -> usize {
    if swamp_bonus {
        1
    } else {
        natural_pack_attempts(kind, seed)
    }
}

/// Use separate random bits for the horizontal spawn side and the passive
/// spawn attempt. Reusing bit zero for both makes passive packs spawn only
/// east of players, leaving the western half of eligible terrain empty.
fn natural_spawn_x_sign(seed: u64) -> i32 {
    if seed & 1 == 0 {
        1
    } else {
        -1
    }
}

/// Convert a player coordinate and a bounded spawn offset without the
/// saturating float-to-int cast or signed overflow changing the spawn chunk.
fn natural_spawn_offset_block(origin: f64, offset: i32) -> Option<i32> {
    let block = origin.floor();
    if !block.is_finite() || block < f64::from(i32::MIN) || block > f64::from(i32::MAX) {
        return None;
    }
    (block as i32).checked_add(offset)
}

/// Keep the bounded origin search symmetric across all four cardinal
/// directions. Previously the long offset was always on X, so candidates
/// could never be far north or south of a player. Low bits choose direction
/// independently of the passive/hostile attempt; the radius uses higher bits.
fn natural_spawn_horizontal_offset(seed: u64) -> (i32, i32) {
    // Reach the outer eligible chunks too: 24..=88 never sampled the
    // 89..=120-block band even though the 17x17 chunk cap includes it.
    // A 120-block long offset and a 32-block lateral offset remain inside
    // the 128-block 3D player-distance limit before vertical checks.
    let radius = 24 + ((seed >> 3) % 97) as i32;
    let lateral = ((seed >> 8) % 65) as i32 - 32;
    let long_offset = natural_spawn_x_sign(seed) * radius;
    if seed & 4 == 0 {
        (long_offset, lateral)
    } else {
        (lateral, long_offset)
    }
}

fn natural_spawn_passive_roll(seed: u64) -> bool {
    seed & 2 == 0
}

/// Unique 17x17 eligible chunk union. Java 1.7.10 counts the entire
/// 289-chunk square, but divides by 256 (not 289) when scaling the mob cap.
/// Modern Java versions use 289; retaining the legacy divisor is important
/// for a single player's 79-monster/11-animal caps.
fn natural_spawn_capacity(players: &[PlayerPosition], base: usize) -> usize {
    natural_mob_cap(players, base).min(MAX_NATURAL_PER_CATEGORY)
}

/// Only in-world X/Z coordinates may contribute to the eligible-chunk union.
/// Java 1.7.10 counts online players above build height or in the void, so
/// height must not affect the cap. But corrupt positions beyond our world
/// border must not inflate caps or act as distance/despawn references.
fn natural_spawn_player_chunk(player: PlayerPosition) -> Option<(i64, i64)> {
    let limit = f64::from(crate::world::WORLD_LIMIT);
    if !player.y.is_finite()
        || !(-limit..limit).contains(&player.x)
        || !(-limit..limit).contains(&player.z)
    {
        return None;
    }
    let x = natural_spawn_offset_block(player.x, 0)?;
    let z = natural_spawn_offset_block(player.z, 0)?;
    Some((i64::from(x.div_euclid(16)), i64::from(z.div_euclid(16))))
}

fn natural_mob_cap(players: &[PlayerPosition], base: usize) -> usize {
    let mut chunks = std::collections::BTreeSet::new();
    for &player in players {
        // Widen before extending the 17x17 square at signed block limits.
        let Some((cx, cz)) = natural_spawn_player_chunk(player) else {
            continue;
        };
        for x in cx - 8..=cx + 8 {
            for z in cz - 8..=cz + 8 {
                chunks.insert((x, z));
            }
        }
    }
    // Java Edition 1.7.10 SpawnerAnimals: cap * eligibleChunks / 256.
    // The 17x17 border chunks count toward the cap even when not eligible
    // as actual pack-spawn origins.
    base.saturating_mul(chunks.len()) / 256
}
/// Java 1.7.10 counts the 17x17 square for the mob cap, but its outer
/// one-chunk border is ineligible for natural spawn attempts. A chunk is
/// eligible if it lies in the interior 15x15 square of *any* online player.
/// Use signed chunk coordinates and widened subtraction for negative worlds.
fn natural_spawn_chunk_eligible(at: PlayerPosition, players: &[PlayerPosition]) -> bool {
    let Some((cx, cz)) = natural_spawn_player_chunk(at) else {
        return false;
    };
    players
        .iter()
        .filter_map(|&player| natural_spawn_player_chunk(player))
        .any(|(px, pz)| (cx - px).abs() <= 7 && (cz - pz).abs() <= 7)
}

fn distance_to_players(p: PlayerPosition, players: &[PlayerPosition]) -> f64 {
    players
        .iter()
        .filter(|&&player| natural_spawn_player_chunk(player).is_some())
        .map(|other| distance_3d(p, *other))
        .fold(f64::INFINITY, f64::min)
}
/// Java Edition 1.7.10 slime-chunk selection; compatible with java.util.Random.
/// Chunk arithmetic is **signed 32-bit** (Java int), then widened to Java long
/// with wrapping signed 64-bit addition. The RNG consumes nextInt(10).
/// Search only the 3x3 blocks in front of a zombie's feet. A candidate
/// must be a CLOSED wooden door with a complete upper/lower half, stand
/// between hunter and target, and lie within the narrow pursuit corridor.
fn closed_wooden_door_on_path(
    world: &World,
    from: PlayerPosition,
    target: PlayerPosition,
) -> Option<[i32; 3]> {
    let dx = target.x - from.x;
    let dz = target.z - from.z;
    let horizontal_sq = dx * dx + dz * dz;
    if horizontal_sq < 4.0 || (target.y - from.y).abs() > 2.0 {
        return None;
    }
    let start_x = from.x.floor() as i32;
    let start_y = from.y.floor() as i32;
    let start_z = from.z.floor() as i32;
    for x in start_x - 1..=start_x + 1 {
        for z in start_z - 1..=start_z + 1 {
            let lower = world.get(x, start_y, z);
            let upper = world.get(x, start_y + 1, z);
            if lower >> 4 != 64
                || lower & 8 != 0
                || lower & 4 != 0
                || upper >> 4 != 64
                || upper & 8 == 0
            {
                continue;
            }
            let door_x = f64::from(x) + 0.5 - from.x;
            let door_z = f64::from(z) + 0.5 - from.z;
            let along = (door_x * dx + door_z * dz) / horizontal_sq;
            let cross = (door_x * dz - door_z * dx).abs() / horizontal_sq.sqrt();
            if (0.0..1.0).contains(&along)
                && cross <= 0.75
                && door_x * door_x + door_z * door_z <= 2.25
            {
                return Some([x, start_y, z]);
            }
        }
    }
    None
}

fn is_slime_chunk(world_seed: u64, chunk_x: i32, chunk_z: i32) -> bool {
    let x_square = chunk_x.wrapping_mul(chunk_x).wrapping_mul(0x4c1906) as i64;
    let x_linear = chunk_x.wrapping_mul(0x5ac0db) as i64;
    let z_square = (chunk_z.wrapping_mul(chunk_z) as i64).wrapping_mul(0x4307a7);
    let z_linear = chunk_z.wrapping_mul(0x5f24f) as i64;
    let mixed = (world_seed as i64)
        .wrapping_add(x_square)
        .wrapping_add(x_linear)
        .wrapping_add(z_square)
        .wrapping_add(z_linear)
        ^ 0x3ad8025f_i64;
    const JAVA_MASK: u64 = (1 << 48) - 1;
    let mut rng = ((mixed as u64) ^ 0x5deece66d) & JAVA_MASK;
    loop {
        rng = rng.wrapping_mul(0x5deece66d).wrapping_add(0xb) & JAVA_MASK;
        let bits = (rng >> 17) as u32; // java.util.Random.next(31)
        let candidate = bits % 10;
        // Java's signed-int overflow rejection path for nextInt(non-power-of-2).
        if i64::from(bits) - i64::from(candidate) + 9 <= i64::from(i32::MAX) {
            return candidate == 0;
        }
    }
}

/// Block-to-chunk coordinates must floor toward negative infinity, not zero.
fn is_slime_cave_location(world_seed: u64, x: i32, y: i32, z: i32) -> bool {
    y < 40 && is_slime_chunk(world_seed, x.div_euclid(16), z.div_euclid(16))
}

/// Java 1.7.10's EntitySlime::getCanSpawnHere applies a separate 1-in-10
/// random gate even after the world-seed slime-chunk and Y<40 checks pass.
/// Do not conflate the chunk's 10% eligibility with the per-attempt 10% roll.
fn slime_cave_spawn_ok(world_seed: u64, x: i32, y: i32, z: i32, roll: u64) -> bool {
    is_slime_cave_location(world_seed, x, y, z) && roll.is_multiple_of(10)
}

/// Vanilla 1.7.10 villager infection chance by world difficulty.
fn zombie_infects_villager(difficulty: u8, roll: u64) -> bool {
    match difficulty {
        3 => true,
        2 => roll & 1 == 0,
        _ => false,
    }
}
/// Natural-spawn origin checks are independent of entity shape: a Ghast may
/// be selected while floating above a valid Nether candidate floor.
fn natural_spawn_origin_ok(world: &World, at: PlayerPosition, players: &[PlayerPosition]) -> bool {
    at.is_valid() && natural_spawn_location_ok(world, at, players)
}
/// Respect both the closest *online player* and Overworld world-spawn
/// exclusion zones. This guard runs for every natural spawn candidate.
fn natural_spawn_location_ok(
    world: &World,
    at: PlayerPosition,
    players: &[PlayerPosition],
) -> bool {
    // Java 1.7.10 excludes candidates at *or within* 24 blocks of the
    // nearest player; the valid inner boundary is strictly greater than 24.
    let nearest = distance_to_players(at, players);
    if nearest <= 24.0 || nearest > 128.0 || !natural_spawn_chunk_eligible(at, players) {
        return false;
    }
    if world.dimension() == crate::dimensions::Dimension::Overworld {
        let origin = PlayerPosition {
            x: 0.5,
            y: world.spawn_y(),
            z: 0.5,
        };
        // The world-spawn exclusion uses the same inclusive 24-block sphere.
        if distance_3d(at, origin) <= 24.0 {
            return false;
        }
    }
    true
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_summon_in_flat_world_survives_restart_without_enabling_natural_spawns() {
        let world = World::new();
        let path = std::env::temp_dir().join(format!(
            "ir1710-summon-{}-{:?}.mobs.json",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut mobs = Mobs::load_or_new(&world, true, 2, &path).unwrap();
        assert!(!mobs.enabled);
        let at = PlayerPosition { x: 0.5, y: 90.0, z: 0.5 };
        let packet = mobs.summon_command(&world, "Cow", at).unwrap();
        assert_eq!(packet.id, 0x0f);
        assert_eq!(mobs.entities.len(), 1);
        assert!(mobs.summon_command(&world, "minecraft:netherite", at).is_err());
        let loaded = Mobs::load_or_new(&world, true, 2, &path).unwrap();
        assert_eq!(loaded.entities.len(), 1);
        assert!(!loaded.enabled);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn natural_spawn_fluid_overlap_is_strict_and_safe_at_i32_limits() {
        // Touching either face of a water/lava block is not a collision.
        assert!(!block_overlaps_axis(4, 5.0, 7.0));
        assert!(!block_overlaps_axis(7, 5.0, 7.0));
        assert!(block_overlaps_axis(6, 5.0, 7.0));

        // Integer addition in the old fluid check panicked in debug builds
        // when a candidate hull reached the signed world-coordinate limit.
        let max = f64::from(i32::MAX);
        assert!(block_overlaps_axis(i32::MAX, max + 0.5, max + 1.5));
        assert!(!block_overlaps_axis(i32::MAX, max + 1.0, max + 2.0));
        let min = f64::from(i32::MIN);
        assert!(block_overlaps_axis(i32::MIN, min - 0.5, min + 0.5));
    }

    #[test]
    fn natural_spawn_hulls_use_species_heights() {
        for (kind, half_width, height) in [
            (Kind::Spider, 0.7, 0.9),
            (Kind::CaveSpider, 0.35, 0.5),
            (Kind::Chicken, 0.2, 0.7),
            (Kind::Pig, 0.45, 0.9),
            (Kind::Cow, 0.45, 1.3),
            (Kind::Sheep, 0.45, 1.3),
            (Kind::Horse, 0.7, 1.6),
            (Kind::Creeper, 0.3, 1.7),
            (Kind::Zombie, 0.3, 1.8),
            (Kind::ZombiePigman, 0.3, 1.8),
            (Kind::Blaze, 0.3, 1.8),
            (Kind::Villager, 0.3, 1.8),
            (Kind::Witch, 0.3, 1.95),
            (Kind::Skeleton, 0.3, 1.95),
            (Kind::Enderman, 0.3, 2.9),
            (Kind::IronGolem, 0.7, 2.9),
        ] {
            assert_eq!(
                natural_spawn_body_dimensions(kind, 1),
                (half_width, height),
                "{kind:?}"
            );
        }
        // Splitting mobs retain their size-dependent cubic hitboxes.
        assert_eq!(natural_spawn_body_dimensions(Kind::Slime, 4), (1.2, 2.4));
    }

    #[test]
    fn legacy_skeleton_height_prevents_false_pack_overlap() {
        let mut mobs = Mobs::default();
        let feet = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        mobs.insert(Kind::Skeleton, feet, false);
        // The 1.7.10 skeleton AABB ends at Y=66.95, not 67.0.
        let above = PlayerPosition { y: 66.96, ..feet };
        assert!(natural_spawn_entity_clearance(
            &mobs.entities,
            Kind::Chicken,
            above,
            1
        ));
        let overlapping = PlayerPosition { y: 66.94, ..feet };
        assert!(!natural_spawn_entity_clearance(
            &mobs.entities,
            Kind::Chicken,
            overlapping,
            1
        ));
    }

    #[test]
    fn natural_spawn_low_ceiling_allows_short_species_but_not_tall_animals() {
        let mut world = World::new();
        world.import_block(0, 64, 0, 2 << 4).unwrap();
        world.import_block(0, 66, 0, 1 << 4).unwrap();
        let feet = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        assert!(natural_spawn_body_ok(&world, Kind::Pig, feet, 1));
        assert!(natural_spawn_body_ok(&world, Kind::Chicken, feet, 1));
        assert!(natural_spawn_body_ok(&world, Kind::Spider, feet, 1));
        assert!(!natural_spawn_body_ok(&world, Kind::Cow, feet, 1));
        assert!(!natural_spawn_body_ok(&world, Kind::Horse, feet, 1));
    }

    #[test]
    fn natural_ghast_candidates_can_float_but_cannot_overlap_walls() {
        let mut world = World::new();
        let air = PlayerPosition {
            x: 0.5,
            y: 80.0,
            z: 0.5,
        };
        assert!(!physics::supported(&world, &air));
        assert!(natural_spawn_body_ok(&world, Kind::Ghast, air, 1));
        assert!(!natural_spawn_body_ok(&world, Kind::Skeleton, air, 1));
        world.import_block(2, 80, 0, 1 << 4).unwrap();
        assert!(!natural_spawn_body_ok(&world, Kind::Ghast, air, 1));
    }

    #[test]
    fn ghast_clearance_distinguishes_face_contact_from_volume_overlap() {
        let at = PlayerPosition {
            x: 0.0,
            y: 80.0,
            z: 0.0,
        };
        // A centered four-block-wide Ghast occupies [-2,2] on X and Z
        // and [78,82] on Y. The adjacent faces must not reject spawning.
        for (x, y, z) in [(2, 80, 0), (-3, 80, 0), (0, 82, 0), (0, 77, 0), (0, 80, 2)] {
            let mut world = World::new();
            world.import_block(x, y, z, 1 << 4).unwrap();
            assert!(
                ghast_has_clearance(&world, at),
                "face contact at ({x},{y},{z}) is not overlap"
            );
        }
        // Solid blocks in the actual hull, including its negative faces,
        // must still be rejected.
        for (x, y, z) in [(1, 80, 0), (-2, 80, 0), (0, 81, 0), (0, 78, 0), (0, 80, -2)] {
            let mut world = World::new();
            world.import_block(x, y, z, 1 << 4).unwrap();
            assert!(
                !ghast_has_clearance(&world, at),
                "solid block at ({x},{y},{z}) overlaps the Ghast"
            );
        }
        for (x, y, z, blocked) in [
            (2, 80, 0, false),
            (1, 80, 0, true),
            (0, 82, 0, false),
            (0, 81, 0, true),
        ] {
            let mut world = World::new();
            world.import_block(x, y, z, 8 << 4).unwrap();
            assert_eq!(
                ghast_has_clearance(&world, at),
                !blocked,
                "fluid at ({x},{y},{z})"
            );
        }
    }

    #[test]
    fn ghast_spawn_clearance_rejects_water_and_lava() {
        let at = PlayerPosition {
            x: 0.5,
            y: 80.0,
            z: 0.5,
        };
        for fluid in [8u16, 9, 10, 11] {
            let mut world = World::new();
            assert!(ghast_has_clearance(&world, at));
            world.import_block(1, 80, 0, fluid << 4).unwrap();
            assert!(
                !ghast_has_clearance(&world, at),
                "fluid {fluid} must block ghast spawning"
            );
            assert!(!natural_spawn_body_ok(&world, Kind::Ghast, at, 1));
        }
    }

    #[test]
    fn natural_land_mobs_reject_water_and_lava_in_their_body() {
        let at = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        for fluid in [8u16, 9, 10, 11] {
            for body_y in [65, 66] {
                let mut world = World::new();
                world.import_block(0, 64, 0, 1 << 4).unwrap();
                assert!(natural_spawn_body_ok(&world, Kind::Zombie, at, 1));
                assert!(natural_spawn_body_ok(&world, Kind::Cow, at, 1));
                world.import_block(0, body_y, 0, fluid << 4).unwrap();
                for kind in [Kind::Zombie, Kind::Cow, Kind::Slime] {
                    let size = if kind == Kind::Slime && body_y == 66 {
                        4
                    } else {
                        1
                    };
                    assert!(
                        !natural_spawn_body_ok(&world, kind, at, size),
                        "{kind:?} must not spawn in fluid {fluid} at y={body_y}"
                    );
                }
            }
            // The Enderman's third block is part of its spawn body too.
            let mut world = World::new();
            world.import_block(0, 64, 0, 1 << 4).unwrap();
            assert!(natural_spawn_body_ok(&world, Kind::Enderman, at, 1));
            world.import_block(0, 67, 0, fluid << 4).unwrap();
            assert!(!natural_spawn_body_ok(&world, Kind::Enderman, at, 1));
        }
    }

    #[test]
    fn wide_natural_mobs_reject_fluid_in_adjacent_body_columns() {
        let center = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        for fluid in [8u16, 9, 10, 11] {
            let mut world = World::new();
            world.import_block(0, 64, 0, 1 << 4).unwrap();
            for kind in [Kind::Spider, Kind::Horse, Kind::IronGolem] {
                assert!(natural_spawn_body_ok(&world, kind, center, 1));
            }
            world.import_block(1, 65, 0, fluid << 4).unwrap();
            // Wide mobs overlap x=1 even though their center is in x=0.
            for kind in [Kind::Spider, Kind::Horse, Kind::IronGolem] {
                assert!(!natural_spawn_body_ok(&world, kind, center, 1));
            }
            // A 0.6-wide zombie at the center does not overlap x=1.
            assert!(natural_spawn_body_ok(&world, Kind::Zombie, center, 1));
            let edge = PlayerPosition { x: 0.85, ..center };
            assert!(!natural_spawn_body_ok(&world, Kind::Cow, edge, 1));
        }

        // Negative coordinates and the Z axis use the same AABB logic.
        let mut world = World::new();
        world.import_block(-1, 64, -1, 1 << 4).unwrap();
        let negative = PlayerPosition {
            x: -0.5,
            y: 65.0,
            z: -0.5,
        };
        assert!(natural_spawn_body_ok(&world, Kind::Spider, negative, 1));
        world.import_block(-1, 65, 0, 9 << 4).unwrap();
        assert!(!natural_spawn_body_ok(&world, Kind::Spider, negative, 1));
    }

    #[test]
    fn natural_spawns_reject_overlapping_mobs_with_species_sized_aabbs() {
        let mut mobs = Mobs::default();
        let at = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        assert!(natural_spawn_entity_clearance(
            &mobs.entities,
            Kind::Zombie,
            at,
            1
        ));
        mobs.insert(Kind::Zombie, at, false);
        assert!(!natural_spawn_entity_clearance(
            &mobs.entities,
            Kind::Zombie,
            at,
            1
        ));
        // Touching faces are not overlapping bodies.
        assert!(natural_spawn_entity_clearance(
            &mobs.entities,
            Kind::Zombie,
            PlayerPosition { x: 1.5, ..at },
            1
        ));
        assert!(natural_spawn_entity_clearance(
            &mobs.entities,
            Kind::Zombie,
            PlayerPosition { y: 67.0, ..at },
            1
        ));
        assert!(!natural_spawn_entity_clearance(
            &mobs.entities,
            Kind::Zombie,
            PlayerPosition { y: 66.5, ..at },
            1
        ));
        // A zombie is 1.8 blocks tall, not 2.0. A candidate whose feet
        // exactly touch its head must not be rejected as overlapping.
        assert!(natural_spawn_entity_clearance(
            &mobs.entities,
            Kind::Creeper,
            PlayerPosition { y: 66.8, ..at },
            1
        ));
        assert!(!natural_spawn_entity_clearance(
            &mobs.entities,
            Kind::Creeper,
            PlayerPosition { y: 66.75, ..at },
            1
        ));

        mobs.insert_sized(Kind::Slime, PlayerPosition { x: 5.5, ..at }, false, 4);
        assert!(!natural_spawn_entity_clearance(
            &mobs.entities,
            Kind::Zombie,
            PlayerPosition { x: 6.5, ..at },
            1
        ));
        assert!(natural_spawn_entity_clearance(
            &mobs.entities,
            Kind::Zombie,
            PlayerPosition { x: 7.5, ..at },
            1
        ));

        // A ghast's stored Y is its center: Y=70 spans [68,72].
        mobs.insert(
            Kind::Ghast,
            PlayerPosition {
                x: 12.5,
                y: 70.0,
                ..at
            },
            false,
        );
        assert!(!natural_spawn_entity_clearance(
            &mobs.entities,
            Kind::Zombie,
            PlayerPosition {
                x: 12.5,
                y: 67.0,
                ..at
            },
            1
        ));
        assert!(natural_spawn_entity_clearance(
            &mobs.entities,
            Kind::Zombie,
            PlayerPosition {
                x: 12.5,
                y: 65.0,
                ..at
            },
            1
        ));
    }

    #[test]
    fn natural_mob_solid_clearance_uses_species_width_and_height() {
        let at = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        let mut world = World::new();
        world.import_block(0, 64, 0, 1 << 4).unwrap();
        for kind in [Kind::Zombie, Kind::Spider, Kind::Horse, Kind::Enderman] {
            assert!(natural_spawn_body_ok(&world, kind, at, 1));
        }
        // An adjacent solid wall misses a player's AABB but clips wide mobs.
        world.import_block(1, 65, 0, 1 << 4).unwrap();
        assert!(natural_spawn_body_ok(&world, Kind::Zombie, at, 1));
        for kind in [Kind::Spider, Kind::Horse, Kind::IronGolem] {
            assert!(!natural_spawn_body_ok(&world, kind, at, 1));
        }
        for kind in [Kind::Slime, Kind::MagmaCube] {
            assert!(natural_spawn_body_ok(&world, kind, at, 1));
            assert!(!natural_spawn_body_ok(&world, kind, at, 2));
            assert!(!natural_spawn_body_ok(&world, kind, at, 4));
        }

        // Tall Endermen must also reject a ceiling above a zombie's head.
        world.import_block(1, 65, 0, 0).unwrap();
        world.import_block(0, 67, 0, 1 << 4).unwrap();
        assert!(natural_spawn_body_ok(&world, Kind::Zombie, at, 1));
        assert!(!natural_spawn_body_ok(&world, Kind::Enderman, at, 1));

        // A half slab only collides when its shape intersects the actual body.
        world.import_block(0, 67, 0, 0).unwrap();
        world.import_block(1, 65, 0, 44 << 4).unwrap();
        assert!(!natural_spawn_body_ok(&world, Kind::Spider, at, 1));
        assert!(natural_spawn_body_ok(&world, Kind::Zombie, at, 1));
    }

    #[test]
    fn natural_slimes_and_magma_cubes_use_selected_size_for_fluid_clearance() {
        let at = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        for kind in [Kind::Slime, Kind::MagmaCube] {
            for fluid in [8u16, 9, 10, 11] {
                let mut world = World::new();
                world.import_block(0, 64, 0, 1 << 4).unwrap();
                for size in [1, 2, 4] {
                    assert!(natural_spawn_body_ok(&world, kind, at, size));
                }
                // Size 1 is 0.6 wide, while size 2 already reaches x=1.
                world.import_block(1, 65, 0, fluid << 4).unwrap();
                assert!(natural_spawn_body_ok(&world, kind, at, 1));
                assert!(!natural_spawn_body_ok(&world, kind, at, 2));
                assert!(!natural_spawn_body_ok(&world, kind, at, 4));

                let mut world = World::new();
                world.import_block(0, 64, 0, 1 << 4).unwrap();
                // Size 4 is 2.4 tall; size 1 and 2 do not reach y=67.
                world.import_block(0, 67, 0, fluid << 4).unwrap();
                assert!(natural_spawn_body_ok(&world, kind, at, 1));
                assert!(natural_spawn_body_ok(&world, kind, at, 2));
                assert!(!natural_spawn_body_ok(&world, kind, at, 4));
            }
        }
    }

    #[test]
    fn flying_mobs_move_without_a_ground_support_block() {
        let world = World::new();
        let from = PlayerPosition {
            x: 0.5,
            y: 80.0,
            z: 0.5,
        };
        let player = PlayerPosition {
            x: 36.5,
            y: 75.0,
            z: 0.5,
        };
        assert!(!physics::supported(&world, &from));
        let ghast = flying_step(&world, Kind::Ghast, from, player, 100).unwrap();
        assert!(ghast.x > from.x);
        assert!(ghast.y > from.y);
        let blaze = flying_step(&world, Kind::Blaze, from, player, 100).unwrap();
        assert!(blaze.x > from.x);
        assert!(blaze.y < from.y);
        assert!(flying_step(&world, Kind::Zombie, from, player, 100).is_none());
    }

    #[test]
    fn ghast_large_body_cannot_clip_through_a_block_or_ceiling() {
        let mut world = World::new();
        let from = PlayerPosition {
            x: 0.5,
            y: 80.0,
            z: 0.5,
        };
        let target = PlayerPosition {
            x: 36.5,
            y: 80.0,
            z: 0.5,
        };
        assert!(ghast_has_clearance(&world, from));
        world.import_block(2, 80, 0, 1 << 4).unwrap();
        assert!(!ghast_has_clearance(&world, from));
        assert!(flying_step(&world, Kind::Ghast, from, target, 20).is_none());
        // A blaze has a smaller collision envelope and still moves.
        assert!(flying_step(&world, Kind::Blaze, from, target, 20).is_some());
    }

    #[test]
    fn sheared_sheep_regrows_only_after_persisted_grass_consumption() {
        let mut world = World::new();
        world.import_block(0, 64, 0, 2 << 4).unwrap();
        let path = std::env::temp_dir().join(format!("ir1710-grazing-{}.irw", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut mobs = Mobs::default();
        let id = mobs.next_id;
        mobs.insert(
            Kind::Sheep,
            PlayerPosition {
                x: 0.5,
                y: 65.0,
                z: 0.5,
            },
            false,
        );
        mobs.entities.get_mut(&id).unwrap().sheared = true;
        let successful_tick = (2..50000u64)
            .step_by(2)
            .find(|tick| {
                hash(*tick ^ (id as u64).wrapping_mul(0x9e3779b97f4a7c15)).is_multiple_of(500)
            })
            .unwrap();
        // Before grazing, the wool stays sheared.
        assert!(mobs.entities.get(&id).unwrap().sheared);
        let changes = mobs.graze(&mut world, &path, successful_tick).unwrap();
        assert_eq!(changes.len(), 1);
        let metadata = changes[0].0.as_ref().unwrap();
        assert_eq!(metadata.id, 0x1c);
        assert_eq!(&metadata.body[..4], &id.to_be_bytes());
        assert_eq!(&metadata.body[4..], &[16, 0, 0x7f]);
        assert!(!mobs.entities.get(&id).unwrap().sheared);
        assert_eq!(world.get(0, 64, 0) >> 4, 3);
        assert!(mobs
            .graze(&mut world, &path, successful_tick)
            .unwrap()
            .is_empty());
        assert_eq!(World::load(&path).unwrap().get(0, 64, 0) >> 4, 3);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn mounted_pig_requires_saddle_and_horse_tames_on_server() {
        let mut mobs = Mobs {
            next_id: BASE_ID,
            ..Mobs::default()
        };
        let at = PlayerPosition::default();
        let pig = mobs.next_id;
        mobs.insert(Kind::Pig, at, false);
        assert!(mobs.use_mount(pig, 2, at).is_none());
        assert!(mobs.can_saddle(pig, at));
        assert_eq!(mobs.saddle(pig, at).unwrap().id, 0x1c);
        assert!(mobs.use_mount(pig, 2, at).is_some());
        assert_eq!(mobs.dismount(2).unwrap().id, 0x1b);
        let horse = mobs.next_id;
        mobs.insert(Kind::Horse, at, false);
        for _ in 0..3 {
            assert!(mobs.use_mount(horse, 3, at).is_some());
            mobs.dismount(3).unwrap();
        }
        assert!(mobs.can_saddle(horse, at));
    }

    #[test]
    fn leash_cannot_attach_to_hostile_or_duplicate_animal() {
        let mut mobs = Mobs {
            next_id: BASE_ID,
            ..Mobs::default()
        };
        let at = PlayerPosition::default();
        let cow = mobs.next_id;
        mobs.insert(Kind::Cow, at, false);
        let zombie = mobs.next_id;
        mobs.insert(Kind::Zombie, at, false);
        assert!(!mobs.can_leash(zombie, 1, at));
        assert_eq!(mobs.leash(cow, 1, at).unwrap().id, 0x1b);
        assert!(!mobs.can_leash(cow, 1, at));
        let tied = mobs.tie_fence(1, [0, 64, 0]);
        assert_eq!(tied.len(), 2);
        assert_eq!(mobs.untie_fence([0, 64, 0]).len(), 2);
    }

    #[test]
    fn dungeon_spawner_creates_bounded_mobs_and_remembers_cooldown() {
        let seed = 42;
        let mut world = World::expanded(seed, crate::dimensions::Dimension::Overworld);
        let (x, y, z) = crate::worldgen::dungeon_spawner_in_cell(seed, 0, 0);
        // A controlled platform supplies four valid spawn attempts without
        // changing the generated spawner itself.
        for dx in -4..=4 {
            for dz in -4..=4 {
                for (height, block) in [(y - 1, 1 << 4), (y, 0), (y + 1, 0)] {
                    world.import_block(x + dx, height, z + dz, block).unwrap();
                }
            }
        }
        world.import_block(x, y, z, 52 << 4).unwrap();
        let player = PlayerPosition {
            x: f64::from(x) + 0.5,
            y: f64::from(y) + 2.0,
            z: f64::from(z) + 0.5,
        };
        let mut mobs = Mobs::new_with_difficulty(&world, true, 2);
        // Simulate a heavily explored server: old cooldown slots must not
        // permanently suppress active spawners after reaching the size cap.
        for i in 0..512 {
            mobs.spawner_next_tick.insert(format!("{i},64,{i}"), 0);
        }
        let mut events = Vec::new();
        mobs.spawn_from_spawners(20, &world, &[player], &mut events);
        assert_eq!(events.len(), 1);
        assert_eq!(mobs.spawner_next_tick.len(), 512);
        assert!(mobs.spawner_next_tick.contains_key(&format!("{x},{y},{z}")));
        let mut second = Vec::new();
        mobs.spawn_from_spawners(40, &world, &[player], &mut second);
        assert!(second.is_empty());
        let restored: Mobs = serde_json::from_slice(&serde_json::to_vec(&mobs).unwrap()).unwrap();
        assert_eq!(restored.spawner_next_tick, mobs.spawner_next_tick);
        world.import_block(x, y, z, 0).unwrap();
        mobs.spawn_from_spawners(10_000, &world, &[player], &mut second);
        assert!(second.is_empty());
    }
    #[test]
    fn dungeon_spawner_catches_up_twenty_tick_boundary_without_duplicate_spawn() {
        let seed = 42;
        let mut world = World::expanded(seed, crate::dimensions::Dimension::Overworld);
        let (x, y, z) = crate::worldgen::dungeon_spawner_in_cell(seed, 0, 0);
        for dx in -4..=4 {
            for dz in -4..=4 {
                for (height, block) in [(y - 1, 1 << 4), (y, 0), (y + 1, 0)] {
                    world.import_block(x + dx, height, z + dz, block).unwrap();
                }
            }
        }
        world.import_block(x, y, z, 52 << 4).unwrap();
        let player = PlayerPosition {
            x: f64::from(x) + 0.5,
            y: f64::from(y) + 2.0,
            z: f64::from(z) + 0.5,
        };
        let mut mobs = Mobs::new_with_difficulty(&world, true, 2);
        mobs.set_natural_spawning(false);
        let key = format!("{x},{y},{z}");

        // The server is polled at 19 and 21, never at the actual 20-tick
        // spawner boundary. The bounded catch-up must still activate it.
        assert!(mobs.tick(19, &world, &[player]).is_empty());
        assert!(!mobs.spawner_next_tick.contains_key(&key));
        let events = mobs.tick(21, &world, &[player]);
        assert_eq!(events.iter().filter(|event| event.id == 0x0f).count(), 1);
        assert_eq!(mobs.entities.len(), 1);
        let cooldown = mobs.spawner_next_tick[&key];
        assert!(cooldown > 21);

        // Repeated session polling at the same tick must not rerun a spawner.
        assert!(mobs.tick(21, &world, &[player]).is_empty());
        assert_eq!(mobs.entities.len(), 1);
        assert_eq!(mobs.spawner_next_tick[&key], cooldown);
    }

    #[test]
    fn living_health_metadata_is_shared_on_spawn_and_after_damage() {
        let mut mobs = Mobs::default();
        let at = PlayerPosition::default();
        let spawn = mobs.insert(Kind::Cow, at, false);
        let id = mobs.next_id - 1;
        assert_eq!(spawn.id, 0x0f);
        assert!(spawn.body.ends_with(&[0x66, 0x41, 0x20, 0, 0, 0x7f]));
        let attack = mobs.player_attack(id, at, None, 20).unwrap();
        assert_eq!(attack.events.len(), 2);
        let health_packet = &attack.events[1];
        assert_eq!(health_packet.id, 0x1c);
        assert_eq!(&health_packet.body[..4], &id.to_be_bytes());
        assert_eq!(&health_packet.body[4..], &[0x66, 0x41, 0x10, 0, 0, 0x7f]);
        // Rejoining clients obtain the same current health value.
        let snapshot = mobs.spawn_events().next().unwrap();
        assert!(snapshot.body.ends_with(&[0x66, 0x41, 0x10, 0, 0, 0x7f]));
    }

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
            nbt: None,
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
            PlayerPosition {
                x: at.x + 4.0,
                ..at
            },
            bucket
        ));
    }

    #[test]
    fn daylight_spiders_are_neutral_in_both_ai_and_melee_until_attacked() {
        // A bright, unprovoked spider cannot inflict a close-range hit. The
        // same mob retaliates when attacked and calms when anger expires.
        let at = PlayerPosition::default();
        for kind in [Kind::Spider, Kind::CaveSpider] {
            let mut mobs = Mobs {
                difficulty: Some(2),
                ..Mobs::default()
            };
            let id = mobs.next_id;
            mobs.insert(kind, at, false);
            mobs.entities.get_mut(&id).unwrap().bright_spider = true;
            assert_eq!(mobs.hurt_player(777, at, 20), 0);
            assert!(mobs.player_attack(id, at, None, 21).is_some());
            assert_eq!(mobs.hurt_player(777, at, 42), 2);
            mobs.entities.get_mut(&id).unwrap().anger_until = 0;
            assert_eq!(mobs.hurt_player(777, at, 100), 0);
        }
    }

    #[test]
    fn spider_brightness_accounts_for_night_and_artificial_light() {
        assert!(spider_bright_light(0, 15, 6000));
        assert!(!spider_bright_light(0, 15, 18000));
        assert!(spider_bright_light(12, 0, 18000));
        assert!(!spider_bright_light(11, 0, 6000));
    }

    #[test]
    fn spider_sunset_catch_up_matches_split_simulation() {
        // Sunset is between ticks 12_000 and 12_001. A catch-up of four
        // simulated ticks must not retroactively treat the daylight ticks
        // as night (for either spider variant).
        let world = World::new();
        let from = PlayerPosition {
            x: 0.5,
            y: 64.0,
            z: 0.5,
        };
        let player = PlayerPosition { x: 8.5, ..from };
        assert!(world.light_at(0, 64, 0).1 >= 12);
        for kind in [Kind::Spider, Kind::CaveSpider] {
            let make_mobs = || {
                let mut mobs = Mobs::default();
                mobs.insert(kind, from, false);
                mobs.last_tick = 11_998;
                mobs
            };
            let mut catch_up = make_mobs();
            let mut split = make_mobs();
            catch_up.tick(12_002, &world, &[player]);
            split.tick(12_000, &world, &[player]);
            split.tick(12_002, &world, &[player]);
            let actual = catch_up.entities.get(&0).unwrap();
            let expected = split.entities.get(&0).unwrap();
            assert_eq!(actual.position, expected.position, "{kind:?}");
            assert!(actual.position.x > from.x, "{kind:?} must pursue at night");
            assert!(!actual.bright_spider);
        }
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
    fn snapshots_reject_duplicate_entity_ids_across_mobs_and_xp() {
        let world = World::natural(9981);
        let path = std::env::temp_dir().join(format!(
            "industrialrust-mobs-{}-duplicate-entity.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut mobs = Mobs::load_or_new(&world, true, 2, &path).unwrap();
        let at = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        mobs.insert(Kind::Cow, at, false);
        let id = mobs.next_id - 1;
        mobs.orbs.insert(id, (at, 5, 0));
        mobs.save(&world).unwrap();
        assert!(Mobs::load_or_new(&world, true, 2, &path).is_err());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn natural_pack_attempts_are_bounded_and_species_specific() {
        for seed in [0, 1u64 << 40, 2u64 << 40, 3u64 << 40, u64::MAX] {
            assert_eq!(natural_pack_attempts(Kind::Zombie, seed), 4);
            assert_eq!(natural_pack_attempts(Kind::Sheep, seed), 4);
            assert!((1..=4).contains(&natural_pack_attempts(Kind::Enderman, seed)));
            assert!((2..=6).contains(&natural_pack_attempts(Kind::Horse, seed)));
            assert_eq!(natural_pack_attempts(Kind::Ghast, seed), 1);
            assert_eq!(natural_pack_attempts(Kind::Witch, seed), 1);
        }
        assert_eq!(natural_pack_attempts(Kind::Horse, 0), 2);
        assert_eq!(natural_pack_attempts(Kind::Horse, 4u64 << 40), 6);
    }

    #[test]
    fn overworld_spawn_entries_use_species_weights_not_uniform_selection() {
        for (kind, expected) in [
            (Kind::Spider, 100),
            (Kind::Zombie, 100),
            (Kind::Skeleton, 100),
            (Kind::Creeper, 100),
            (Kind::Slime, 100),
            (Kind::Enderman, 10),
            (Kind::Witch, 5),
        ] {
            assert_eq!(
                (0..515)
                    .filter(|roll| overworld_monster_kind(*roll) == kind)
                    .count(),
                expected,
                "{kind:?}"
            );
        }
        for (kind, expected) in [
            (Kind::Sheep, 12),
            (Kind::Pig, 10),
            (Kind::Chicken, 10),
            (Kind::Cow, 8),
        ] {
            assert_eq!(
                (0..40)
                    .filter(|roll| overworld_passive_kind(4, *roll) == kind)
                    .count(),
                expected,
                "{kind:?}"
            );
        }
        assert_eq!(
            (0..45)
                .filter(|roll| overworld_passive_kind(1, *roll) == Kind::Horse)
                .count(),
            5
        );
        assert_eq!(
            (0..41)
                .filter(|roll| overworld_passive_kind(35, *roll) == Kind::Horse)
                .count(),
            1
        );
        for biome in [2, 3, 4, 18, 21] {
            assert_eq!(horse_spawn_weight(biome), 0);
            assert!((0..256).all(|roll| overworld_passive_kind(biome, roll) != Kind::Horse));
        }
    }

    #[test]
    fn swamp_slime_bonus_spawn_entry_uses_its_own_weight_and_single_member_pack() {
        // The default 515-weight list is unchanged outside Swampland.
        for biome in [1, 4] {
            assert_eq!(
                (0..515)
                    .filter(|&roll| overworld_monster_entry(biome, roll).0 == Kind::Slime)
                    .count(),
                100
            );
            assert!((0..515).all(|roll| !overworld_monster_entry(biome, roll).1));
        }
        // In Swampland, the inherited 100-weight Slime entry and the
        // separate weight-1 single-member entry both remain selectable.
        let mut slimes = 0;
        let mut bonus = 0;
        for roll in 0..516 {
            let (kind, single_member) = overworld_monster_entry(6, roll);
            if kind == Kind::Slime {
                slimes += 1;
            }
            if single_member {
                bonus += 1;
                assert_eq!(kind, Kind::Slime);
            }
        }
        assert_eq!(slimes, 101);
        assert_eq!(bonus, 1);
        assert_eq!(overworld_monster_entry(6, 515), (Kind::Slime, true));
        assert_eq!(overworld_monster_entry(134, 515), (Kind::Slime, true));
        assert_eq!(overworld_monster_entry(6, 1031), (Kind::Slime, true));
        assert_eq!(overworld_monster_entry(6, 514), (Kind::Witch, false));
        assert_eq!(natural_pack_attempts_for_entry(Kind::Slime, 0, false), 4);
        assert_eq!(natural_pack_attempts_for_entry(Kind::Slime, 0, true), 1);
        assert_eq!(natural_pack_attempts_for_entry(Kind::Witch, 0, false), 1);
    }

    #[test]
    fn tethered_undead_still_take_daylight_damage() {
        let mut world = World::new();
        // A roof above the head must still protect a restrained zombie.
        world.import_block(10, 67, 0, 1 << 4).unwrap();
        let mut mobs = Mobs::default();
        let at = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        mobs.insert(Kind::Zombie, at, false);
        let leashed = mobs.next_id - 1;
        mobs.entities.get_mut(&leashed).unwrap().leash_holder = Some(42);
        mobs.insert(Kind::Skeleton, PlayerPosition { x: 5.5, ..at }, false);
        let anchored = mobs.next_id - 1;
        mobs.entities.get_mut(&anchored).unwrap().fence_anchor = Some([5, 65, 0]);
        mobs.insert(Kind::Zombie, PlayerPosition { x: 10.5, ..at }, false);
        let sheltered = mobs.next_id - 1;
        mobs.entities.get_mut(&sheltered).unwrap().leash_holder = Some(42);

        mobs.tick(20, &world, &[]);
        assert!(mobs.entities[&leashed].health < Kind::Zombie.health());
        assert!(mobs.entities[&anchored].health < Kind::Skeleton.health());
        assert_eq!(mobs.entities[&sheltered].health, Kind::Zombie.health());
        // Restraints remain attached; sunlight must not detach or move them.
        assert_eq!(mobs.entities[&leashed].leash_holder, Some(42));
        assert_eq!(mobs.entities[&anchored].fence_anchor, Some([5, 65, 0]));
    }

    #[test]
    fn daylight_damage_uses_each_simulated_tick_once_during_catch_up() {
        let world = World::new();
        let mut mobs = Mobs::default();
        let at = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        mobs.insert(Kind::Zombie, at, false);
        let zombie = mobs.next_id - 1;
        mobs.insert(Kind::Skeleton, PlayerPosition { x: 5.5, ..at }, false);
        let skeleton = mobs.next_id - 1;
        // Keep both mobs stationary so only daylight damage affects HP.
        mobs.entities.get_mut(&zombie).unwrap().leash_holder = Some(42);
        mobs.entities.get_mut(&skeleton).unwrap().fence_anchor = Some([5, 65, 0]);

        // The first call simulates ticks 17..=20, not tick 20 four times.
        mobs.tick(20, &world, &[]);
        assert_eq!(mobs.entities[&zombie].health, Kind::Zombie.health() - 1);
        assert_eq!(mobs.entities[&skeleton].health, Kind::Skeleton.health() - 1);

        // Polling the same tick or advancing fewer than 20 ticks adds no burn.
        mobs.tick(20, &world, &[]);
        mobs.tick(23, &world, &[]);
        assert_eq!(mobs.entities[&zombie].health, Kind::Zombie.health() - 1);

        // The second catch-up window includes tick 40 exactly once.
        mobs.tick(40, &world, &[]);
        assert_eq!(mobs.entities[&zombie].health, Kind::Zombie.health() - 2);
        assert_eq!(mobs.entities[&skeleton].health, Kind::Skeleton.health() - 2);
    }

    #[test]
    fn tethered_adult_chickens_lay_eggs_without_duplicate_ticks() {
        let world = World::new();
        let at = PlayerPosition {
            x: 10.5,
            y: 65.0,
            z: 12.5,
        };
        let mut mobs = Mobs::default();
        mobs.insert(Kind::Chicken, at, false);
        let adult_id = mobs.next_id - 1;
        mobs.insert(Kind::Chicken, at, true);
        let baby_id = mobs.next_id - 1;

        // Leashing suppresses wandering but not an adult's egg timer.
        let adult = mobs.entities.get_mut(&adult_id).unwrap();
        adult.leash_holder = Some(42);
        adult.next_egg_tick = 1;
        let baby = mobs.entities.get_mut(&baby_id).unwrap();
        baby.fence_anchor = Some([10, 65, 12]);
        baby.next_egg_tick = 1;

        mobs.tick(1, &world, &[]);
        let drops = mobs.take_drops();
        assert_eq!(drops.len(), 1);
        assert_eq!(drops[0].0.id, 344);
        assert_eq!(drops[0].0.count, 1);
        assert_eq!(drops[0].1, (10, 65, 12));

        // Repeated polls and later ticks before the cooldown never duplicate.
        mobs.tick(1, &world, &[]);
        mobs.tick(2, &world, &[]);
        assert!(mobs.take_drops().is_empty());

        // A fence anchor must also leave egg laying enabled.
        let adult = mobs.entities.get_mut(&adult_id).unwrap();
        adult.leash_holder = None;
        adult.fence_anchor = Some([10, 65, 12]);
        adult.next_egg_tick = 3;
        mobs.tick(3, &world, &[]);
        assert_eq!(mobs.take_drops().len(), 1);
        assert!(mobs.entities[&baby_id].baby);
    }

    #[test]
    fn horse_breeding_requires_taming_and_golden_food() {
        let at = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        let mut mobs = Mobs::default();
        mobs.insert(Kind::Horse, at, false);
        let first = mobs.next_id - 1;
        mobs.insert(Kind::Horse, PlayerPosition { x: 2.5, ..at }, false);
        let second = mobs.next_id - 1;
        let held = |id| {
            Some(Stack {
                id,
                count: 1,
                damage: 0,
                nbt: None,
            })
        };

        // Golden food cannot breed an untamed horse.
        assert!(mobs.feed(first, at, held(396), 100).is_none());
        mobs.entities.get_mut(&first).unwrap().tamed = true;
        for id in [260, 296, 353] {
            assert!(mobs.feed(first, at, held(id), 100).is_none());
        }
        assert!(mobs.feed(first, at, held(396), 100).is_some());
        // The untamed second horse cannot be put into love mode.
        assert!(mobs.feed(second, at, held(322), 101).is_none());
        assert_eq!(mobs.entities.len(), 2);

        mobs.entities.get_mut(&second).unwrap().tamed = true;
        assert!(mobs.feed(second, at, held(322), 102).is_some());
        assert_eq!(mobs.entities.len(), 3);
        assert!(mobs
            .entities
            .values()
            .any(|mob| mob.kind == Kind::Horse && mob.baby));
        assert_eq!(mobs.entities[&first].breed_tick, 102);
        assert_eq!(mobs.entities[&second].breed_tick, 102);
    }

    #[test]
    fn adult_horses_drop_zero_to_two_leather_not_always_one() {
        let at = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        let mut observed = [false; 3];
        for id in BASE_ID..BASE_ID + 256 {
            let mut state = Mobs {
                next_id: id,
                ..Mobs::default()
            };
            state.insert(Kind::Horse, at, false);
            let horse = state.entities[&id];
            if let Some((leather, _)) = horse.death_drop() {
                assert_eq!(leather.id, 334);
                assert!((1..=2).contains(&leather.count));
                observed[leather.count as usize] = true;
            } else {
                observed[0] = true;
            }
        }
        assert!(observed.iter().all(|seen| *seen));
    }

    #[test]
    fn passive_spawn_roll_is_independent_of_all_four_spawn_directions() {
        // Bit zero chooses positive/negative; bit two chooses X/Z as the
        // long axis; bit one chooses the passive attempt independently.
        for seed in 0_u64..8 {
            let sign = if seed & 1 == 0 { 1 } else { -1 };
            let (dx, dz) = natural_spawn_horizontal_offset(seed);
            if seed & 4 == 0 {
                assert_eq!((dx, dz), (24 * sign, -32));
            } else {
                assert_eq!((dx, dz), (-32, 24 * sign));
            }
            assert_eq!(natural_spawn_passive_roll(seed), seed & 2 == 0);
        }
    }

    #[test]
    fn slime_chunk_cave_spawns_ignore_light_without_weakening_other_spawn_rules() {
        let seed = 0;
        let slime_chunk = (0..128)
            .find(|&chunk| is_slime_chunk(seed, chunk, 0))
            .expect("find a slime chunk");
        let other_chunk = (0..128)
            .find(|&chunk| !is_slime_chunk(seed, chunk, 0))
            .expect("find a non-slime chunk");
        let sx = slime_chunk * 16 + 8;
        let ox = other_chunk * 16 + 8;
        let now = 6000; // Daytime, so ordinary hostile light rules reject 15.
        assert!(!spawn_light_ok(15, 15, now, false));
        assert!(natural_spawn_species_light_ok(
            Kind::Slime,
            seed,
            (sx, 30, 8),
            (15, 15),
            now,
            false
        ));
        assert!(!natural_spawn_species_light_ok(
            Kind::Slime,
            seed,
            (ox, 30, 8),
            (15, 15),
            now,
            false
        ));
        assert!(!natural_spawn_species_light_ok(
            Kind::Slime,
            seed,
            (sx, 40, 8),
            (15, 15),
            now,
            false
        ));
        assert!(!natural_spawn_species_light_ok(
            Kind::Zombie,
            seed,
            (sx, 30, 8),
            (15, 15),
            now,
            false
        ));
        assert!(natural_spawn_species_light_ok(
            Kind::Cow,
            seed,
            (sx, 65, 8),
            (15, 15),
            now,
            true
        ));
    }

    #[test]
    fn torchlit_roofed_grass_remains_eligible_for_passive_spawning() {
        let mut world = World::new();
        let at = PlayerPosition {
            x: 2.5,
            y: 70.0,
            z: 2.5,
        };
        world.import_block(2, 69, 2, 2 << 4).unwrap(); // grass
        world.import_block(2, 72, 2, 1 << 4).unwrap(); // roof
        world.import_block(3, 70, 2, 50 << 4).unwrap(); // torch
        assert!(!sky_exposed(&world, at));
        let (block, sky) = world.light_at(2, 70, 2);
        assert!(
            block >= 9,
            "torch must illuminate the roofed spawn location"
        );
        assert!(natural_passive_habitat_ok(2, block, sky, 6000));
        assert!(natural_passive_habitat_ok(2, block, sky, 18000));
        assert!(!natural_passive_habitat_ok(1, block, sky, 6000));
        assert!(!natural_passive_habitat_ok(2, 8, 0, 6000));
        assert!(natural_passive_habitat_ok(2, 0, 15, 6000));
    }

    #[test]
    fn mushroom_islands_exclude_ordinary_natural_mobs_and_pack_members() {
        // Both the origin and each proposed pack member use this predicate.
        for biome in [14, 15] {
            assert!(!overworld_biome_allows_natural_spawn(biome));
        }
        for biome in [0, 1, 6, 35, 129] {
            assert!(overworld_biome_allows_natural_spawn(biome));
        }
    }

    #[test]
    fn snow_and_desert_variants_reject_passive_spawns_at_origin_and_pack_edges() {
        // Both origin selection and pack-member validation use this filter.
        // These biomes clear the ordinary passive spawn list in Java 1.7.10.
        for biome in [2, 12, 13, 17, 130, 140] {
            assert!(
                !overworld_biome_allows_passive_spawn(biome),
                "biome {biome} must not spawn ordinary passive mobs"
            );
        }
        // Plains, sunflower plains, taiga and savanna retain passive entries.
        for biome in [1, 4, 5, 6, 35, 129] {
            assert!(overworld_biome_allows_passive_spawn(biome));
        }
        for biome in [14, 15] {
            assert!(!overworld_biome_allows_passive_spawn(biome));
            assert!(!overworld_biome_allows_natural_spawn(biome));
        }
    }

    #[test]
    fn all_three_legacy_beach_biomes_reject_passive_spawns() {
        // The ordinary Beach, Stone Beach and Cold Beach constructors
        // clear the farm-animal list in Java 1.7.10. Both origin and pack
        // member checks share this predicate, even beside a grassy biome.
        for biome in [16, 25, 26] {
            assert!(
                !overworld_biome_allows_passive_spawn(biome),
                "beach biome {biome} must reject passive packs"
            );
            // Their ordinary hostile list is not empty.
            assert!(overworld_biome_allows_natural_spawn(biome));
        }
        for biome in [1, 4, 5, 6] {
            assert!(overworld_biome_allows_passive_spawn(biome));
        }
    }

    #[test]
    fn horse_pack_members_cannot_cross_into_biomes_without_horse_entries() {
        // A pack may start in Plains or Savanna, but its later members
        // must not appear in an adjacent Forest, Taiga or Swamp on grass.
        for biome in [1, 129, 35, 36, 163, 164] {
            assert!(overworld_biome_allows_passive_species(biome, Kind::Horse));
            assert!(overworld_biome_allows_passive_species(biome, Kind::Cow));
        }
        for biome in [4, 5, 6, 18, 27] {
            assert!(!overworld_biome_allows_passive_species(biome, Kind::Horse));
            assert!(overworld_biome_allows_passive_species(biome, Kind::Cow));
        }
        for biome in [0, 14, 16, 25, 37, 165] {
            assert!(!overworld_biome_allows_passive_species(biome, Kind::Horse));
            assert!(!overworld_biome_allows_passive_species(biome, Kind::Cow));
        }
    }

    #[test]
    fn mesa_biomes_reject_ordinary_passive_packs_even_on_grass() {
        // Java 1.7.10 BiomeGenMesa clears spawnableCreatureList for all
        // three variants. BiomeGenMutated copies that empty list.
        // The same predicate guards both the pack origin and its members.
        for biome in [37, 38, 39, 165, 166, 167] {
            assert!(!overworld_biome_allows_passive_spawn(biome));
            // Mesa still permits the ordinary hostile spawn list.
            assert!(overworld_biome_allows_natural_spawn(biome));
        }
        // A nearby pack may still spawn in an eligible grassy biome.
        for biome in [1, 4, 5, 6, 35] {
            assert!(overworld_biome_allows_passive_spawn(biome));
        }
    }

    #[test]
    fn ocean_biomes_reject_passive_packs_even_at_grassy_boundary_blocks() {
        // BiomeGenOcean, BiomeGenOcean (frozen), and Deep Ocean clear
        // spawnableCreatureList in 1.7.10. A neighboring plains pack must
        // not admit a member on a grass block in any of these biomes.
        // The same predicate guards both pack origin and each member.
        for (biome, name) in [(0, "Ocean"), (10, "Frozen Ocean"), (24, "Deep Ocean")] {
            assert!(
                !overworld_biome_allows_passive_spawn(biome),
                "{name} must reject ordinary passive mobs at origin and pack edges"
            );
            // These biomes retain the ordinary hostile-spawn list.
            assert!(overworld_biome_allows_natural_spawn(biome));
        }
        // A pack may still originate or continue in an eligible neighboring
        // biome; rejecting ocean members must not reject the entire pack.
        for biome in [1, 4, 5, 6, 35] {
            assert!(overworld_biome_allows_passive_spawn(biome));
        }
    }

    #[test]
    fn irw4_plains_pack_members_recheck_river_biome_on_real_world_columns() {
        // Fixed IRW4 seed and two nearby columns on opposite sides of a
        // Plains (1) -> River (7) boundary. Both receive identical grass
        // support and air clearance; biome alone must reject river animals.
        let mut world = World::expanded(42, crate::dimensions::Dimension::Overworld);
        let plains = (835, 142);
        let river = (837, 142);
        assert_eq!(world.biome_at(plains.0, plains.1), 1);
        assert_eq!(world.biome_at(river.0, river.1), 7);
        let mut positions = Vec::new();
        for (x, z) in [plains, river] {
            let y = world.surface_height_at(x, z).unwrap() + 1;
            world.import_block(x, y - 1, z, 2 << 4).unwrap();
            world.import_block(x, y, z, 0).unwrap();
            world.import_block(x, y + 1, z, 0).unwrap();
            positions.push((x, y, z));
        }
        let plains_at = positions[0];
        let river_at = positions[1];
        for kind in [Kind::Cow, Kind::Sheep, Kind::Horse] {
            assert!(natural_pack_member_biome_ok(&world, kind, plains_at, true));
            assert!(
                !natural_pack_member_biome_ok(&world, kind, river_at, true),
                "{kind:?} crossed into River despite valid grass"
            );
        }
        // River's ordinary hostile list remains available. Do not reject
        // monster packs just because the passive spawn list is empty.
        assert!(natural_pack_member_biome_ok(
            &world,
            Kind::Zombie,
            river_at,
            false
        ));
        // The per-member light gate remains independent of biome/ground.
        assert!(natural_spawn_species_light_ok(
            Kind::Cow,
            42,
            plains_at,
            (9, 0),
            6000,
            true
        ));
        assert!(!natural_spawn_species_light_ok(
            Kind::Cow,
            42,
            plains_at,
            (8, 0),
            6000,
            true
        ));
    }

    #[test]
    fn irw4_natural_pack_loop_does_not_spawn_sheep_across_plains_river_edge() {
        // Exercise spawn_natural itself, not only its per-member predicate.
        // Seed 42 at tick 58_400 selects a passive Sheep pack with origin
        // (835, 142); member 2 probes (837, 142) in the adjacent River.
        let mut world = World::expanded(42, crate::dimensions::Dimension::Overworld);
        let now = 58_400_u64;
        let seed = hash(now ^ 42);
        assert_eq!(natural_spawn_horizontal_offset(seed), (-68, 13));
        assert!(natural_spawn_passive_roll(seed));
        assert_eq!(
            overworld_passive_kind(1, hash(seed ^ 0x31acb4105ff327ab)),
            Kind::Sheep
        );
        let river_member_roll = hash(seed ^ 2_u64.wrapping_mul(0x517cc1b727220a95));
        let river_dx = (river_member_roll % 6) as i32 - ((river_member_roll >> 8) % 6) as i32;
        let river_dz =
            ((river_member_roll >> 16) % 6) as i32 - ((river_member_roll >> 24) % 6) as i32;
        assert_eq!((river_dx, river_dz), (2, 0));
        assert_eq!(world.biome_at(835, 142), 1);
        assert_eq!(world.biome_at(837, 142), 7);

        // Make all four deterministic pack columns equally standable.
        // River rejection must come from its spawn list, not missing grass,
        // collision, water, or insufficient light.
        for (dx, dz) in [(0, 0), (3, -1), (2, 0), (2, 1)] {
            let (x, z) = (835 + dx, 142 + dz);
            let y = world.surface_height_at(x, z).unwrap() + 1;
            world.import_block(x, y - 1, z, 2 << 4).unwrap();
            world.import_block(x, y, z, 0).unwrap();
            world.import_block(x, y + 1, z, 0).unwrap();
        }
        let player = PlayerPosition {
            x: 903.5,
            y: world.spawn_y(),
            z: 129.5,
        };
        let river_y = world.surface_height_at(837, 142).unwrap() + 1;
        let river_at = PlayerPosition {
            x: 837.5,
            y: f64::from(river_y),
            z: 142.5,
        };
        assert!(natural_spawn_location_ok(&world, river_at, &[player]));
        assert!(natural_spawn_body_ok(&world, Kind::Sheep, river_at, 1));
        let mut lighting = world.lighting_sampler();
        let light = lighting.light_at(837, river_y, 142);
        assert!(natural_spawn_species_light_ok(
            Kind::Sheep,
            42,
            (837, river_y, 142),
            light,
            now,
            true
        ));
        assert!(!natural_pack_member_biome_ok(
            &world,
            Kind::Sheep,
            (837, river_y, 142),
            true
        ));

        let mut mobs = Mobs::new_with_difficulty(&world, true, 2);
        let mut events = Vec::new();
        mobs.spawn_natural(now, &world, &mut lighting, &[player], &mut events);
        assert!(
            !mobs.entities.is_empty(),
            "the Plains origin must produce a Sheep"
        );
        assert_eq!(events.len(), mobs.entities.len());
        for mob in mobs.entities.values() {
            assert_eq!(mob.kind, Kind::Sheep);
            let x = mob.position.x.floor() as i32;
            let z = mob.position.z.floor() as i32;
            assert!(
                overworld_biome_allows_passive_species(world.biome_at(x, z), mob.kind),
                "pack member spawned in forbidden biome at ({x}, {z})"
            );
            assert_ne!((x, z), (837, 142), "River member bypassed its spawn list");
        }

        // C21: The same live pack loop must honor the *animal* cap, not only
        // the monster cap tested elsewhere. In Java 1.7.10 one player's
        // 17x17 eligible-chunk union gives floor(10 * 289 / 256) = 11.
        // Place existing cows away from the origin so body collisions cannot
        // mask an accidental cap overshoot.
        assert_eq!(natural_spawn_capacity(&[player], VANILLA_PASSIVE_CAP), 11);
        let filler = PlayerPosition {
            x: 4096.5,
            y: 65.0,
            z: 4096.5,
        };
        let spawn_with_animals = |preexisting: usize| {
            let mut capped = Mobs::new_with_difficulty(&world, true, 2);
            for _ in 0..preexisting {
                capped.insert(Kind::Cow, filler, false);
            }
            let mut packets = Vec::new();
            let mut sampler = world.lighting_sampler();
            capped.spawn_natural(now, &world, &mut sampler, &[player], &mut packets);
            assert_eq!(
                packets.iter().filter(|event| event.id == 0x0f).count(),
                capped.entities.len() - preexisting,
                "each live spawn must have exactly one Protocol 5 notification"
            );
            capped
                .entities
                .values()
                .filter(|mob| mob.kind.counts_toward_natural_cap(true))
                .count()
        };
        assert_eq!(
            spawn_with_animals(11),
            11,
            "a full animal cap must block new sheep"
        );
        assert_eq!(
            spawn_with_animals(10),
            11,
            "one free animal slot must admit exactly one sheep, not a full pack"
        );
    }

    #[test]
    fn swamp_slimes_obey_moon_phase_light_and_height_without_affecting_cave_slimes() {
        let full_moon = 18_000_u64;
        let new_moon = full_moon + 4 * 24_000;
        let mut full_moon_successes = 0;
        let mut half_moon_successes = 0;
        for seed in 0..8_192_u64 {
            if swamp_slime_spawn_ok(6, 60, full_moon, seed, (0, 0)) {
                full_moon_successes += 1;
            }
            // Mutated Swampland inherits the extra Slime spawn entry and
            // must pass the same moon, light and height gates as Swampland.
            assert_eq!(
                swamp_slime_spawn_ok(134, 60, full_moon, seed, (0, 0)),
                swamp_slime_spawn_ok(6, 60, full_moon, seed, (0, 0))
            );
            if swamp_slime_spawn_ok(6, 60, full_moon + 2 * 24_000, seed, (0, 0)) {
                half_moon_successes += 1;
            }
            assert!(!swamp_slime_spawn_ok(6, 60, new_moon, seed, (0, 0)));
            assert!(!swamp_slime_spawn_ok(134, 60, new_moon, seed, (0, 0)));
            assert!(!swamp_slime_spawn_ok(6, 60, full_moon, seed, (8, 0)));
            assert!(!swamp_slime_spawn_ok(6, 50, full_moon, seed, (0, 0)));
            assert!(!swamp_slime_spawn_ok(6, 70, full_moon, seed, (0, 0)));
            assert!(!swamp_slime_spawn_ok(134, 70, full_moon, seed, (0, 0)));
            assert!(!swamp_slime_spawn_ok(1, 60, full_moon, seed, (0, 0)));
        }
        assert!(full_moon_successes > 0);
        assert!(half_moon_successes > 0);
        assert!(half_moon_successes < full_moon_successes);
        // Underground slime chunks remain valid at Y=39 independently of
        // the lunar phase; Y=40 is outside the cave-only exemption.
        let chunk_x = (0..64).find(|&x| is_slime_chunk(0, x, 0)).unwrap();
        assert!(is_slime_cave_location(0, chunk_x * 16, 39, 0));
        assert!(!is_slime_cave_location(0, chunk_x * 16, 40, 0));
    }

    #[test]
    fn cave_slimes_require_per_attempt_one_in_ten_roll_after_chunk_selection() {
        let slime_chunk = (0..64).find(|&x| is_slime_chunk(0, x, 0)).unwrap();
        let non_slime_chunk = (0..64).find(|&x| !is_slime_chunk(0, x, 0)).unwrap();
        let x = slime_chunk * 16;
        // A slime chunk is not a guaranteed spawn: only one in ten
        // independently sampled candidate rolls can pass at Y < 40.
        assert_eq!(
            (0..100_u64)
                .filter(|&roll| slime_cave_spawn_ok(0, x, 39, 0, roll))
                .count(),
            10
        );
        assert!((0..100_u64).all(|roll| !slime_cave_spawn_ok(0, x, 40, 0, roll)));
        assert!(
            (0..100_u64).all(|roll| { !slime_cave_spawn_ok(0, non_slime_chunk * 16, 39, 0, roll) })
        );
        // Block-to-chunk conversion must continue to use floor division.
        assert_eq!(
            slime_cave_spawn_ok(0, -1, 39, 0, 0),
            is_slime_chunk(0, -1, 0)
        );
    }

    #[test]
    fn natural_spawn_coordinate_offsets_reject_overflow_and_invalid_positions() {
        assert_eq!(natural_spawn_offset_block(0.5, 24), Some(24));
        assert_eq!(natural_spawn_offset_block(-0.5, -24), Some(-25));
        assert_eq!(natural_spawn_offset_block(f64::from(i32::MAX), 1), None);
        assert_eq!(natural_spawn_offset_block(f64::from(i32::MIN), -1), None);
        assert_eq!(
            natural_spawn_offset_block(f64::from(i32::MAX) + 1.0, -1),
            None
        );
        assert_eq!(
            natural_spawn_offset_block(f64::from(i32::MAX) - 1.0, 1),
            Some(i32::MAX)
        );
        assert_eq!(
            natural_spawn_offset_block(f64::from(i32::MIN) + 1.0, -1),
            Some(i32::MIN)
        );
        assert_eq!(natural_spawn_offset_block(f64::NAN, 0), None);
        assert_eq!(natural_spawn_offset_block(f64::INFINITY, 0), None);
    }

    #[test]
    fn natural_spawn_offsets_stay_inside_the_bounded_search_radius() {
        let mut reached_outer_band = [false; 4];
        for seed in 0_u64..8192 {
            let (dx, dz) = natural_spawn_horizontal_offset(seed);
            let horizontal_distance = f64::from(dx).hypot(f64::from(dz));
            assert!((24.0..=128.0).contains(&horizontal_distance));
            // The outer eligible band must be reachable in every direction;
            // a previous 88-block maximum silently excluded it entirely.
            if dx >= 110 {
                reached_outer_band[0] = true;
            }
            if dx <= -110 {
                reached_outer_band[1] = true;
            }
            if dz >= 110 {
                reached_outer_band[2] = true;
            }
            if dz <= -110 {
                reached_outer_band[3] = true;
            }
        }
        assert!(
            reached_outer_band.iter().all(|reached| *reached),
            "outer eligible spawn band must be sampled in all four directions"
        );
    }

    #[test]
    fn natural_spawn_skips_outer_border_chunks_but_counts_them_for_caps() {
        let player = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        let candidate = |x: f64, z: f64| PlayerPosition { x, z, ..player };
        // The cap includes 17x17 chunks; spawning only uses the 15x15
        // interior. Negative coordinates must use floor division.
        assert_eq!(natural_mob_cap(&[player], 70), 79);
        assert!(natural_spawn_chunk_eligible(
            candidate(127.5, 0.5),
            &[player]
        ));
        assert!(natural_spawn_chunk_eligible(
            candidate(-111.5, 0.5),
            &[player]
        ));
        assert!(!natural_spawn_chunk_eligible(
            candidate(128.5, 0.5),
            &[player]
        ));
        assert!(!natural_spawn_chunk_eligible(
            candidate(-112.5, 0.5),
            &[player]
        ));
        assert!(!natural_spawn_chunk_eligible(
            candidate(0.5, -112.5),
            &[player]
        ));
        // A chunk on one player's border may be interior to another.
        let second = candidate(-128.5, 0.5);
        assert!(natural_spawn_chunk_eligible(
            candidate(-112.5, 0.5),
            &[player, second]
        ));
        assert!(!natural_spawn_chunk_eligible(candidate(0.5, 0.5), &[]));
    }

    #[test]
    fn natural_spawn_24_block_exclusion_is_strict_for_players_and_world_spawn() {
        let world = World::natural(42);
        let y = world.spawn_y();
        let at = |x: f64| PlayerPosition { x, y, z: 0.5 };
        let player = at(400.5);
        // The 1.7.10 nearest-player check excludes exactly 24 blocks, not
        // just distances strictly smaller than 24.
        assert!(!natural_spawn_location_ok(&world, at(423.5), &[player]));
        assert!(!natural_spawn_location_ok(&world, at(424.5), &[player]));
        assert!(natural_spawn_location_ok(&world, at(424.501), &[player]));
        // A more distant second player must not override the closest one.
        assert!(!natural_spawn_location_ok(
            &world,
            at(424.5),
            &[at(500.5), player]
        ));

        // The world-spawn point at (0.5, spawn_y, 0.5) has its own 24-block
        // exclusion, even when the nearest online player is farther away.
        let far_player = at(64.5);
        assert!(!natural_spawn_location_ok(&world, at(23.5), &[far_player]));
        assert!(!natural_spawn_location_ok(&world, at(24.5), &[far_player]));
        assert!(natural_spawn_location_ok(&world, at(24.501), &[far_player]));
    }

    #[test]
    fn natural_spawn_128_block_outer_radius_is_three_dimensional() {
        // The outer limit is a 3D sphere, not a horizontal radius.
        // Keep X/Z in the same eligible chunk while varying only height.
        let world = World::natural(42);
        let player = PlayerPosition {
            x: 400.5,
            y: 200.0,
            z: 400.5,
        };
        let candidate = |y: f64| PlayerPosition { y, ..player };
        assert_eq!(distance_to_players(candidate(72.0), &[player]), 128.0);
        assert!(natural_spawn_location_ok(
            &world,
            candidate(72.0),
            &[player]
        ));
        assert!(!natural_spawn_location_ok(
            &world,
            candidate(71.999),
            &[player]
        ));
        assert!(natural_spawn_location_ok(
            &world,
            candidate(72.001),
            &[player]
        ));

        // A second, closer player can exclude an otherwise eligible position.
        let near = PlayerPosition { y: 96.0, ..player };
        assert!(!natural_spawn_location_ok(
            &world,
            candidate(72.0),
            &[player, near]
        ));
        let outside_inner = PlayerPosition { y: 97.0, ..player };
        assert!(natural_spawn_location_ok(
            &world,
            candidate(72.0),
            &[player, outside_inner]
        ));
    }

    #[test]
    fn natural_spawn_location_rejects_border_even_within_distance_limit() {
        let world = World::natural(42);
        let y = world.spawn_y();
        let player = PlayerPosition {
            x: 400.5,
            y,
            z: 0.5,
        };
        // Player chunk 25, candidate chunk 17: exactly eight chunks west.
        // Its 113-block distance passes the radius check, but not the
        // 1.7.10 interior-chunk eligibility check.
        let border = PlayerPosition { x: 287.5, ..player };
        let interior = PlayerPosition { x: 288.5, ..player };
        assert!((24.0..=128.0).contains(&distance_to_players(border, &[player])));
        assert!(!natural_spawn_location_ok(&world, border, &[player]));
        assert!(natural_spawn_location_ok(&world, interior, &[player]));
    }

    #[test]
    fn mob_cap_uses_unique_17_by_17_chunks_not_player_count() {
        let a = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        let b = PlayerPosition { x: 16.5, ..a };
        // 1.7.10 uses 17x17 counted chunks but a 256 divisor.
        assert_eq!(natural_mob_cap(&[a], 70), 79);
        assert_eq!(natural_mob_cap(&[a], 10), 11);
        assert_eq!(natural_mob_cap(&[a, a], 70), 79);
        // One-chunk X offset adds 17 previously uncounted chunks.
        assert_eq!(natural_mob_cap(&[a, b], 70), 83);
        assert_eq!(natural_mob_cap(&[], 70), 0);
        let distant = PlayerPosition { x: 512.5, ..a };
        // Two disjoint 17x17 areas must count all 578 chunks once.
        assert_eq!(natural_spawn_capacity(&[a, distant], 70), 158);
        assert_eq!(natural_spawn_capacity(&[a, distant], 10), 22);
        assert_eq!(natural_spawn_capacity(&[a, a], 70), 79);
        // Negative block coordinates must floor to the correct chunk.
        let west = PlayerPosition { x: -0.5, ..a };
        assert_eq!(natural_mob_cap(&[a, west], 70), 83);
    }

    #[test]
    fn natural_spawn_live_pack_respects_multiplayer_chunk_union_cap() {
        // The live spawn loop must use the 17x17 chunk UNION, not a
        // per-player count or the cap of just the first player. Keep all
        // pre-existing mobs far from both spawn origins so their collision
        // boxes do not suppress an otherwise eligible pack.
        let world = World::natural(0);
        let near = PlayerPosition {
            x: 512.5,
            y: world.spawn_y(),
            z: 512.5,
        };
        let distant = PlayerPosition { x: 1024.5, ..near };
        assert_eq!(natural_spawn_capacity(&[near], VANILLA_HOSTILE_CAP), 79);
        assert_eq!(
            natural_spawn_capacity(&[near, distant], VANILLA_HOSTILE_CAP),
            158
        );

        let filler = PlayerPosition {
            x: 4096.5,
            y: 65.0,
            z: 4096.5,
        };
        let seeded_mobs = |count| {
            let mut mobs = Mobs::new_with_difficulty(&world, true, 2);
            for _ in 0..count {
                mobs.insert(Kind::Zombie, filler, false);
            }
            mobs
        };
        let spawn_once = |mobs: &mut Mobs, players: &[PlayerPosition]| {
            let mut events = Vec::new();
            let mut lighting = world.lighting_sampler();
            mobs.spawn_natural(18_040, &world, &mut lighting, players, &mut events);
            events
        };

        // At the single-player limit, the same deterministic tick cannot
        // create a mob or send a Protocol 5 spawn packet.
        let mut single = seeded_mobs(79);
        assert!(spawn_once(&mut single, &[near]).is_empty());
        assert_eq!(single.entities.len(), 79);

        // The distant second player contributes a disjoint 17x17 area.
        // With 79 existing hostiles, the natural pack must still be eligible.
        let mut expanded = seeded_mobs(79);
        let expanded_events = spawn_once(&mut expanded, &[near, distant]);
        assert!(
            expanded.entities.len() > 79,
            "a disjoint second player must increase the live spawn budget"
        );
        assert!(expanded_events.iter().any(|event| event.id == 0x0f));

        // Near the expanded limit, a multi-member pack may create exactly
        // one more mob, even when the selected species permits a larger pack.
        let mut nearly_full = seeded_mobs(157);
        let limited_events = spawn_once(&mut nearly_full, &[near, distant]);
        assert_eq!(nearly_full.entities.len(), 158);
        assert_eq!(
            limited_events
                .iter()
                .filter(|event| event.id == 0x0f)
                .count(),
            1
        );
    }

    #[test]
    fn natural_spawn_live_pack_respects_overlapping_player_chunk_caps() {
        // Two players one chunk apart share 16x17 chunks. Their union is
        // 306 chunks, not 578: the 1.7.10 hostile cap is 83, not 158.
        // Exercise the real spawn loop rather than only natural_mob_cap.
        let world = World::natural(0);
        let first = PlayerPosition {
            x: 512.5,
            y: world.spawn_y(),
            z: 512.5,
        };
        let adjacent = PlayerPosition { x: 528.5, ..first };
        let diagonal = PlayerPosition {
            x: 640.5,
            z: 640.5,
            ..first
        };
        assert_eq!(
            natural_spawn_capacity(&[first, adjacent], VANILLA_HOSTILE_CAP),
            83
        );
        assert_eq!(
            natural_spawn_capacity(&[first, diagonal], VANILLA_HOSTILE_CAP),
            135
        );

        let filler = PlayerPosition {
            x: 4096.5,
            y: 65.0,
            z: 4096.5,
        };
        let seeded_mobs = |count| {
            let mut mobs = Mobs::new_with_difficulty(&world, true, 2);
            for _ in 0..count {
                mobs.insert(Kind::Zombie, filler, false);
            }
            mobs
        };
        let spawn_once = |mobs: &mut Mobs, players: &[PlayerPosition]| {
            let mut events = Vec::new();
            let mut lighting = world.lighting_sampler();
            mobs.spawn_natural(18_040, &world, &mut lighting, players, &mut events);
            events
        };

        // At the exact union cap, no pack may create a mob or a Protocol 5
        // spawn packet, even though two players are online.
        for (players, cap) in [(vec![first, adjacent], 83), (vec![first, diagonal], 135)] {
            let mut full = seeded_mobs(cap);
            assert!(spawn_once(&mut full, &players).is_empty());
            assert_eq!(full.entities.len(), cap);

            let mut almost_full = seeded_mobs(cap - 1);
            let events = spawn_once(&mut almost_full, &players);
            let spawned = almost_full.entities.len() - (cap - 1);
            assert!(
                spawned <= 1,
                "a pack must not exceed the overlapping union cap"
            );
            assert_eq!(
                events.iter().filter(|event| event.id == 0x0f).count(),
                spawned,
                "each newly spawned mob needs one Protocol 5 spawn event"
            );
        }
    }

    #[test]
    fn natural_mob_cap_diagonal_overlap_and_border_membership() {
        let a = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        // An eight-chunk diagonal offset overlaps the two 17x17 squares
        // in exactly 9x9 chunks. Count the union, not 2 * 289 chunks.
        let b = PlayerPosition {
            x: 128.5,
            z: 128.5,
            ..a
        };
        assert_eq!(natural_mob_cap(&[a, b], 70), 135);
        assert_eq!(natural_mob_cap(&[b, a, a], 70), 135);
        assert_eq!(natural_mob_cap(&[a, b], 10), 19);
        assert_eq!(natural_spawn_capacity(&[a, b], 70), 135);

        let at = |x: f64, z: f64| PlayerPosition { x, z, ..a };
        // Chunk (8, 0) is on both players' outer border and cannot spawn.
        assert!(!natural_spawn_chunk_eligible(at(128.5, 0.5), &[a, b]));
        // Chunk (8, 1) is on A's border but inside B's 15x15 area.
        assert!(natural_spawn_chunk_eligible(at(128.5, 16.5), &[a, b]));
        // Chunk (7, 0) remains eligible through A's interior.
        assert!(natural_spawn_chunk_eligible(at(112.5, 0.5), &[a, b]));
    }

    #[test]
    fn natural_mob_cap_handles_world_border_signed_coordinates() {
        // The 17x17 union extends beyond a player's chunk at the world edge.
        // Both signs must remain valid without integer overflow or saturation.
        let edge = |x: f64, z: f64| PlayerPosition { x, y: 65.0, z };
        let limit = f64::from(crate::world::WORLD_LIMIT);
        let max = edge(limit - 0.5, limit - 0.5);
        let min = edge(-limit, -limit);
        assert_eq!(natural_mob_cap(&[max], 70), 79);
        assert_eq!(natural_mob_cap(&[min], 10), 11);
        assert_eq!(natural_mob_cap(&[max, max], 70), 79);
        assert_eq!(natural_mob_cap(&[min, max], 70), 158);
        assert!(natural_spawn_chunk_eligible(max, &[max]));
        assert!(natural_spawn_chunk_eligible(min, &[min]));

        // The positive border is exclusive, but the negative border is inclusive.
        // Out-of-world coordinates must not protect mobs from despawning,
        // or make a candidate chunk eligible.
        for outside in [
            edge(limit, 0.5),
            edge(-limit - 0.5, 0.5),
            edge(0.5, limit),
            edge(0.5, -limit - 0.5),
            edge(f64::from(i32::MAX), 0.5),
            edge(f64::from(i32::MIN), 0.5),
        ] {
            assert_eq!(natural_mob_cap(&[outside], 70), 0);
            assert_eq!(natural_mob_cap(&[max, outside], 70), 79);
            assert!(!natural_spawn_chunk_eligible(outside, &[outside]));
            assert!(!natural_spawn_chunk_eligible(outside, &[max]));
            assert_eq!(distance_to_players(max, &[outside, max]), 0.0);
            assert_eq!(distance_to_players(max, &[outside]), f64::INFINITY);
        }
        // Out-of-height players at valid X/Z still count for vanilla caps.
        let above = PlayerPosition { y: 300.0, ..min };
        assert_eq!(natural_mob_cap(&[above], 70), 79);
    }

    #[test]
    fn natural_spawn_ignores_malformed_player_chunks() {
        let valid = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        let invalid = [
            PlayerPosition {
                x: f64::NAN,
                ..valid
            },
            PlayerPosition {
                z: f64::INFINITY,
                ..valid
            },
            PlayerPosition {
                y: f64::NEG_INFINITY,
                ..valid
            },
            PlayerPosition {
                x: f64::from(i32::MAX) + 1.0,
                ..valid
            },
            PlayerPosition {
                z: f64::from(i32::MIN) - 1.0,
                ..valid
            },
        ];
        for malformed in invalid {
            assert_eq!(natural_mob_cap(&[malformed], 70), 0);
            assert_eq!(natural_mob_cap(&[valid, malformed], 70), 79);
            assert!(!natural_spawn_chunk_eligible(malformed, &[valid]));
            assert!(!natural_spawn_chunk_eligible(valid, &[malformed]));
            assert!(natural_spawn_chunk_eligible(valid, &[valid, malformed]));
        }
        // Java 1.7.10 derives the eligible chunk union from player X/Z,
        // including players below the world or above build height.
        for y in [-1.0, 256.0, 300.0] {
            let out_of_height = PlayerPosition { y, ..valid };
            assert_eq!(natural_mob_cap(&[out_of_height], 70), 79);
            assert!(natural_spawn_chunk_eligible(valid, &[out_of_height]));
        }
    }

    #[test]
    fn adult_loot_varies_with_entity_but_respects_vanilla_common_ranges() {
        let at = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        let mut observed_cow_leather = false;
        let mut observed_chicken_feathers = false;
        for id in 500_000..500_080 {
            for (kind, item_id, low, high) in [
                (Kind::Cow, 363, 1, 3),
                (Kind::Pig, 319, 1, 3),
                (Kind::Chicken, 365, 1, 1),
                (Kind::Sheep, 35, 1, 1),
                (Kind::Wither, 399, 1, 1),
            ] {
                let mut mobs = Mobs {
                    next_id: id,
                    ..Mobs::default()
                };
                mobs.insert(kind, at, false);
                let mob = mobs.entities[&id];
                let (stack, _) = mob.death_drop().expect("adult primary drop");
                assert_eq!(stack.id, item_id);
                assert!((low..=high).contains(&stack.count));
                for (extra, _) in mob.extra_death_drops() {
                    assert!(extra.count >= 1 && extra.count <= 2);
                    if kind == Kind::Cow {
                        assert_eq!(extra.id, 334);
                        observed_cow_leather = true;
                    }
                    if kind == Kind::Chicken {
                        assert_eq!(extra.id, 288);
                        observed_chicken_feathers = true;
                    }
                }
            }
        }
        assert!(observed_cow_leather);
        assert!(observed_chicken_feathers);
    }

    #[test]
    fn a_player_kill_enqueues_secondary_loot_exactly_once() {
        let at = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        let mut mobs = Mobs {
            next_id: BASE_ID,
            ..Mobs::default()
        };
        mobs.insert(Kind::Cow, at, false);
        let id = mobs.next_id - 1;
        mobs.entities.get_mut(&id).unwrap().health = 1;
        let expected = mobs.entities[&id].extra_death_drops().len();
        let hit = mobs.player_attack(id, at, None, 10).unwrap();
        assert!(hit.drop.is_some());
        assert_eq!(mobs.take_drops().len(), expected);
        assert!(mobs.player_attack(id, at, None, 20).is_none());
        assert!(mobs.take_drops().is_empty());
    }

    #[test]
    fn eggs_and_slime_splits_share_the_current_loaded_mob_limit() {
        let world = World::new();
        let at = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        let mut mobs = Mobs {
            next_id: BASE_ID,
            ..Mobs::default()
        };
        for _ in 0..129 {
            mobs.insert(Kind::Cow, at, false);
        }
        assert!(mobs.hatch_egg(&world, at).is_some());
        mobs.insert_sized(Kind::Slime, at, false, 4);
        let parent = mobs.entities.remove(&(mobs.next_id - 1)).unwrap();
        assert!(!mobs.split(parent).is_empty());
        mobs.enabled = true;
        mobs.next_id = 1_000_000;
        assert!(mobs.hatch_egg(&world, at).is_none());
        assert!(mobs.spawn_xp(at, 5).is_none());
        assert!(mobs.fire_arrow(None, at, 0.0, 0.0, 1.0, 1).is_none());
        assert!(mobs.summon_boss(&world, 64, at).is_none());
        assert!(mobs.split(parent).is_empty());
    }

    #[test]
    fn saddled_horses_and_pigs_return_one_saddle_on_death() {
        let at = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        for kind in [Kind::Horse, Kind::Pig] {
            let mut mobs = Mobs {
                next_id: BASE_ID,
                ..Mobs::default()
            };
            mobs.insert(kind, at, false);
            let id = mobs.next_id - 1;
            let animal = mobs.entities.get_mut(&id).unwrap();
            animal.saddled = true;
            animal.health = 1;
            assert!(mobs.player_attack(id, at, None, 20).is_some());
            let loot = mobs.take_drops();
            assert_eq!(loot.len(), 1);
            assert_eq!(loot[0].0.id, 329);
            assert_eq!(loot[0].0.count, 1);
            assert!(mobs.player_attack(id, at, None, 40).is_none());
            assert!(mobs.take_drops().is_empty());
        }
    }

    #[test]
    fn baby_animals_and_sheared_sheep_do_not_drop_adult_loot_or_xp() {
        let at = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        let mut mobs = Mobs::default();
        mobs.insert(Kind::Cow, at, true);
        let cow = mobs.next_id - 1;
        mobs.entities.get_mut(&cow).unwrap().health = 1;
        let killed = mobs.player_attack(cow, at, None, 10).unwrap();
        assert!(killed.drop.is_none());
        assert_eq!(killed.experience, 0);

        mobs.insert(Kind::Sheep, at, false);
        let sheep = mobs.next_id - 1;
        mobs.entities.get_mut(&sheep).unwrap().sheared = true;
        assert!(mobs.entities[&sheep].death_drop().is_none());
    }

    #[test]
    fn out_of_range_player_positions_are_not_despawn_references() {
        let world = World::new();
        let at = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        // These are finite numbers, but no signed 32-bit block coordinate
        // represents them. They must not trigger a global hostile cull.
        let invalid = [
            PlayerPosition {
                x: f64::from(i32::MAX) + 1.0,
                ..at
            },
            PlayerPosition {
                z: f64::from(i32::MIN) - 1.0,
                ..at
            },
        ];
        assert_eq!(distance_to_players(at, &invalid), f64::INFINITY);
        let mut mobs = Mobs::default();
        mobs.insert(Kind::Zombie, at, false);
        let id = mobs.next_id - 1;
        mobs.entities.get_mut(&id).unwrap().far_from_player_ticks = 600;
        mobs.tick(1, &world, &invalid);
        assert!(mobs.entities.contains_key(&id));
        assert_eq!(mobs.entities[&id].far_from_player_ticks, 600);

        // Near the world border, an invalid coordinate can look closer than
        // a real distant player. Ignore it for the nearest-player test.
        let limit = f64::from(crate::world::WORLD_LIMIT);
        let edge = PlayerPosition {
            x: limit - 2.5,
            ..at
        };
        let outside = PlayerPosition {
            x: limit + 1.0,
            ..at
        };
        assert!(distance_to_players(edge, &[outside, at]) > 128.0);
        let near = PlayerPosition {
            x: edge.x - 10.0,
            ..at
        };
        assert!(distance_to_players(edge, &[outside, near]) < 32.0);
    }

    #[test]
    fn malformed_online_player_positions_do_not_force_hostile_despawn() {
        let world = World::new();
        let at = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        let mut mobs = Mobs::default();
        mobs.insert(Kind::Zombie, at, false);
        let id = mobs.next_id - 1;
        mobs.entities.get_mut(&id).unwrap().far_from_player_ticks = 600;

        // A corrupted connection position is not a 128-block-away player.
        let invalid = [
            PlayerPosition { x: f64::NAN, ..at },
            PlayerPosition {
                z: f64::INFINITY,
                ..at
            },
        ];
        mobs.tick(1, &world, &invalid);
        assert!(mobs.entities.contains_key(&id));
        assert_eq!(mobs.entities[&id].far_from_player_ticks, 600);

        // A valid nearby player still resets the timer, even when a bad
        // position is present in the same online snapshot.
        let near = PlayerPosition { x: 1.5, ..at };
        mobs.tick(2, &world, &[invalid[0], near]);
        assert!(mobs.entities.contains_key(&id));
        assert_eq!(mobs.entities[&id].far_from_player_ticks, 0);

        // Valid players outside 128 blocks must still despawn the hostile.
        let far = PlayerPosition { x: 200.5, ..at };
        mobs.tick(3, &world, &[invalid[0], far]);
        assert!(!mobs.entities.contains_key(&id));
    }

    #[test]
    fn no_online_players_do_not_despawn_or_age_hostiles() {
        let world = World::new();
        let at = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        let mut mobs = Mobs::default();
        mobs.insert(Kind::Zombie, at, false);
        let id = mobs.next_id - 1;
        // A pre-existing mob can have accumulated time outside the 32-block
        // safe zone before the final player logs out. Offline time must not
        // advance its random despawn timer or immediately cull the entity.
        mobs.entities.get_mut(&id).unwrap().far_from_player_ticks = 600;
        mobs.tick(20, &world, &[]);
        mobs.tick(40, &world, &[]);
        assert!(mobs.entities.contains_key(&id));
        assert_eq!(mobs.entities[&id].far_from_player_ticks, 600);

        // Once a player reconnects beyond 128 blocks, ordinary despawning
        // resumes rather than leaving the mob permanently exempt.
        let far = PlayerPosition { x: 512.5, ..at };
        mobs.tick(60, &world, &[far]);
        assert!(!mobs.entities.contains_key(&id));
    }

    #[test]
    fn newly_spawned_hostiles_get_a_full_far_player_despawn_grace_period() {
        let world = World::new();
        let at = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        let far = PlayerPosition { x: 64.5, ..at };
        let mut mobs = Mobs::default();
        mobs.insert(Kind::Zombie, at, false);
        let id = mobs.next_id - 1;
        // At this uptime the old hit_tick=0 logic immediately despawned
        // a newly inserted mob on its first eligible 1/800 random roll.
        let first_roll = (1000_u64..100_000)
            .find(|tick| hash(*tick ^ id as u64).is_multiple_of(800))
            .unwrap();
        mobs.tick(first_roll, &world, &[far]);
        assert!(mobs.entities.contains_key(&id));
        assert_eq!(mobs.entities[&id].far_from_player_ticks, 4);

        // Re-entering the safe radius clears the accumulated far-away age.
        mobs.tick(first_roll + 1, &world, &[at]);
        assert_eq!(mobs.entities[&id].far_from_player_ticks, 0);

        // Once 600 distant ticks have elapsed, a matching roll can despawn.
        mobs.entities.get_mut(&id).unwrap().far_from_player_ticks = 600;
        let eligible_roll = (first_roll + 2..first_roll + 100_000)
            .find(|tick| hash(*tick ^ id as u64).is_multiple_of(800))
            .unwrap();
        mobs.tick(eligible_roll, &world, &[far]);
        assert!(!mobs.entities.contains_key(&id));
    }

    #[test]
    fn hostile_despawn_rolls_once_per_simulated_catch_up_tick() {
        let world = World::new();
        let at = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        let far = PlayerPosition { x: 64.5, ..at };
        let mut mobs = Mobs::default();
        mobs.insert(Kind::Creeper, at, false);
        let id = mobs.next_id - 1;
        mobs.entities.get_mut(&id).unwrap().far_from_player_ticks = 600;

        // Four elapsed simulation steps include now-3, now-2, now-1,
        // and now. The old implementation reused the final roll four
        // times and missed an eligible roll at now-1.
        let catch_up_tick = (1000_u64..100_000)
            .find(|tick| {
                hash((*tick - 1) ^ id as u64).is_multiple_of(800)
                    && !hash(*tick ^ id as u64).is_multiple_of(800)
            })
            .expect("an earlier catch-up tick with an independent despawn roll");
        mobs.tick(catch_up_tick, &world, &[far]);
        assert!(
            !mobs.entities.contains_key(&id),
            "the now-1 despawn roll must run during four-step catch-up"
        );
    }

    #[test]
    fn hostile_despawn_uses_strict_32_block_random_cull_boundary() {
        let world = World::new();
        let at = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        let mut exact = Mobs::default();
        exact.insert(Kind::Creeper, at, false);
        let id = exact.next_id - 1;
        let roll_tick = (1000_u64..100_000)
            .find(|tick| hash(*tick ^ id as u64).is_multiple_of(800))
            .unwrap();

        // At exactly 32 blocks, vanilla advances the far-player age but
        // does not reset it or execute the 1/800 random despawn roll.
        exact.entities.get_mut(&id).unwrap().far_from_player_ticks = 600;
        let boundary = PlayerPosition {
            x: at.x + 32.0,
            ..at
        };
        exact.tick(roll_tick, &world, &[boundary]);
        assert!(exact.entities.contains_key(&id));
        assert_eq!(exact.entities[&id].far_from_player_ticks, 604);

        // Moving even slightly farther permits the same eligible roll.
        let mut outside = Mobs::default();
        outside.insert(Kind::Creeper, at, false);
        outside.entities.get_mut(&id).unwrap().far_from_player_ticks = 600;
        outside.tick(
            roll_tick,
            &world,
            &[PlayerPosition {
                x: at.x + 32.001,
                ..at
            }],
        );
        assert!(!outside.entities.contains_key(&id));

        // A player just inside the boundary resets the far-player age.
        let mut inside = Mobs::default();
        inside.insert(Kind::Creeper, at, false);
        inside.entities.get_mut(&id).unwrap().far_from_player_ticks = 600;
        inside.tick(
            roll_tick,
            &world,
            &[PlayerPosition {
                x: at.x + 31.999,
                ..at
            }],
        );
        assert_eq!(inside.entities[&id].far_from_player_ticks, 0);
    }

    #[test]
    fn hostile_despawn_uses_closest_valid_player_at_128_block_boundary() {
        let world = World::new();
        let at = PlayerPosition {
            x: -128.5,
            y: 65.0,
            z: -32.5,
        };
        let farther = PlayerPosition {
            x: at.x + 192.0,
            ..at
        };
        let invalid = PlayerPosition { z: f64::NAN, ..at };
        // A farther player and a malformed snapshot must not override the
        // nearest valid player, including across negative world coordinates.
        for (label, nearest, survives, expected_age) in [
            (
                "inside 32 blocks",
                PlayerPosition {
                    x: at.x - 31.0,
                    ..at
                },
                true,
                0,
            ),
            (
                "exactly 128 blocks",
                PlayerPosition {
                    x: at.x - 128.0,
                    ..at
                },
                true,
                1,
            ),
            (
                "just outside 128 blocks",
                PlayerPosition {
                    x: at.x - 128.001,
                    ..at
                },
                false,
                0,
            ),
        ] {
            let mut mobs = Mobs::default();
            mobs.insert(Kind::Creeper, at, false);
            let id = mobs.next_id - 1;
            mobs.tick(1, &world, &[farther, invalid, nearest]);
            assert_eq!(mobs.entities.contains_key(&id), survives, "{label}");
            if survives {
                assert_eq!(
                    mobs.entities[&id].far_from_player_ticks, expected_age,
                    "{label}"
                );
            }
        }
    }

    #[test]
    fn legacy_mob_snapshots_default_missing_far_player_age_to_zero() {
        let mut mobs = Mobs::default();
        let at = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        mobs.insert(Kind::Zombie, at, false);
        let id = mobs.next_id - 1;
        let mut saved = serde_json::to_value(mobs.entities[&id]).unwrap();
        saved
            .as_object_mut()
            .unwrap()
            .remove("far_from_player_ticks");
        let restored: Mob = serde_json::from_value(saved).unwrap();
        assert_eq!(restored.far_from_player_ticks, 0);
    }

    #[test]
    fn distant_players_do_not_despawn_a_wither_or_golem() {
        let world = World::new();
        let at = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        let far = PlayerPosition { x: 512.5, ..at };
        let mut mobs = Mobs::default();
        for kind in [Kind::Wither, Kind::IronGolem, Kind::Zombie] {
            mobs.insert(kind, at, false);
        }
        mobs.tick(1, &world, &[far]);
        assert!(mobs.entities.values().any(|m| m.kind == Kind::Wither));
        assert!(mobs.entities.values().any(|m| m.kind == Kind::IronGolem));
        assert!(!mobs.entities.values().any(|m| m.kind == Kind::Zombie));
    }

    #[test]
    fn player_bow_kill_awards_orb_once_and_despawns_the_arrow() {
        let world = World::new();
        let at = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        let mut mobs = Mobs::default();
        mobs.insert(Kind::Cow, PlayerPosition { z: 2.5, ..at }, false);
        let target = mobs.next_id - 1;
        mobs.entities.get_mut(&target).unwrap().health = 1;
        mobs.fire_arrow(Some(9_999), at, 0.0, 30.0, 2.0, 3).unwrap();
        let mut events = Vec::new();
        mobs.tick_arrows(&world, &[], 1, &mut events);
        assert!(!mobs.entities.contains_key(&target));
        assert!(mobs.arrows.is_empty());
        assert_eq!(mobs.orbs.len(), 1);
        assert_eq!(mobs.orbs.values().next().unwrap().1, 2);
        assert_eq!(events.iter().filter(|event| event.id == 0x11).count(), 1);
    }

    #[test]
    fn villager_ai_tracks_live_village_doors_without_a_player_target() {
        let mut world = World::expanded(42, crate::dimensions::Dimension::Overworld);
        let mut village = None;
        'search: for gx in -8..=8 {
            for gz in -8..=8 {
                let x = gx * 192 + 48;
                let z = gz * 192 + 48;
                if let Some([Some(first), Some(second)]) = world.live_village_house_doors(x, z) {
                    village = Some((x, z, first, second));
                    break 'search;
                }
            }
        }
        let (cx, cz, first, second) = village.expect("seed 42 has generated villages");
        let middle = PlayerPosition {
            x: f64::from(cx) + 0.5,
            y: f64::from(first[1]),
            z: f64::from(cz) + 0.5,
        };
        let first_goal = PlayerPosition {
            x: f64::from(first[0]) + 0.5,
            y: f64::from(first[1]),
            z: f64::from(first[2]) + 1.5,
        };
        let second_goal = PlayerPosition {
            x: f64::from(second[0]) + 0.5,
            y: f64::from(second[1]),
            z: f64::from(second[2]) - 0.5,
        };
        assert_eq!(villager_door_destination(&world, middle), Some(first_goal));
        // Generated doors must obey the same four-block vertical reach as
        // independent player-built houses, even when horizontally nearby.
        assert_eq!(
            villager_door_destination(
                &world,
                PlayerPosition {
                    y: middle.y + 8.0,
                    ..middle
                }
            ),
            None
        );
        assert_eq!(
            villager_door_destination(
                &world,
                PlayerPosition {
                    y: middle.y - 8.0,
                    ..middle
                }
            ),
            None
        );
        assert_eq!(
            villager_door_destination(
                &world,
                PlayerPosition {
                    y: middle.y + 4.0,
                    ..middle
                }
            ),
            Some(first_goal)
        );

        // The floor of the villager's feet coordinate defines its block Y;
        // fractional positions on the fourth block still count as reachable.
        assert_eq!(
            villager_door_destination(
                &world,
                PlayerPosition {
                    y: middle.y + 4.75,
                    ..middle
                }
            ),
            Some(first_goal)
        );

        // The mob tick must use the live door even without a nearby player.
        let outside = PlayerPosition {
            z: f64::from(first[2]) + 3.5,
            ..first_goal
        };
        let mut mobs = Mobs::new_with_difficulty(&world, false, 2);
        let id = mobs.next_id;
        mobs.insert(Kind::Villager, outside, false);
        mobs.tick(1, &world, &[]);
        let villager = mobs.entities.get(&id).unwrap();
        assert!(
            villager.position != outside || villager.waypoint.is_some(),
            "villager must plan or move toward an intact generated door"
        );

        let original_lower = world.get(first[0], first[1], first[2]);
        world.import_block(first[0], first[1], first[2], 0).unwrap();
        assert_eq!(villager_door_destination(&world, middle), Some(second_goal));
        world
            .import_block(second[0], second[1], second[2], 1 << 4)
            .unwrap();
        assert_eq!(villager_door_destination(&world, middle), None);
        let before = mobs.entities.get(&id).unwrap().position;
        mobs.tick(2, &world, &[]);
        assert_eq!(mobs.entities.get(&id).unwrap().position, before);
        world
            .import_block(first[0], first[1], first[2], original_lower)
            .unwrap();
        assert_eq!(villager_door_destination(&world, middle), Some(first_goal));

        // A newly placed wooden door becomes a live village goal. The same
        // lower/upper-half validation rejects it after either half is removed.
        world.import_block(first[0], first[1], first[2], 0).unwrap();
        let added = [cx, first[1], cz + 8];
        world
            .import_block(added[0], added[1] - 1, added[2], 1 << 4)
            .unwrap();
        world
            .import_block(added[0], added[1], added[2], 64 << 4)
            .unwrap();
        world
            .import_block(added[0], added[1] + 1, added[2], 64 << 4 | 8)
            .unwrap();
        assert_eq!(
            villager_door_destination(&world, middle),
            Some(PlayerPosition {
                x: f64::from(cx) + 0.5,
                y: f64::from(first[1]),
                z: f64::from(cz) + 9.5,
            })
        );
        world
            .import_block(added[0], added[1] + 1, added[2], 0)
            .unwrap();
        assert_eq!(villager_door_destination(&world, middle), None);
        // A player-built door one block outside the static footprint can
        // extend a village only when connected to a live core door.
        world
            .import_block(second[0], second[1], second[2], 64 << 4 | 2)
            .unwrap();
        let annex = [second[0] + 15, second[1], second[2]];
        let outside = PlayerPosition {
            x: f64::from(annex[0]) + 0.5,
            y: f64::from(annex[1]),
            z: f64::from(annex[2]) + 0.5,
        };
        assert_eq!(villager_door_destination(&world, outside), None);
        world
            .import_block(annex[0], annex[1] - 1, annex[2], 1 << 4)
            .unwrap();
        world
            .import_block(annex[0], annex[1], annex[2], 64 << 4 | 2)
            .unwrap();
        world
            .import_block(annex[0], annex[1] + 1, annex[2], 64 << 4 | 8)
            .unwrap();
        assert_eq!(
            villager_door_destination(&world, outside),
            Some(PlayerPosition {
                x: f64::from(annex[0]) + 0.5,
                y: f64::from(annex[1]),
                z: f64::from(annex[2]) - 0.5,
            })
        );
        world.import_block(annex[0], annex[1], annex[2], 0).unwrap();
        assert_eq!(villager_door_destination(&world, outside), None);
        assert_eq!(
            villager_door_destination(
                &World::expanded(42, crate::dimensions::Dimension::Nether),
                middle
            ),
            None
        );
    }

    #[test]
    fn villager_can_target_independent_player_built_house() {
        let mut world = World::expanded(42, crate::dimensions::Dimension::Overworld);
        let (x, z) = (-8..=8)
            .flat_map(|gx| (-8..=8).map(move |gz| (gx * 192 + 48, gz * 192 + 48)))
            .find(|&(x, z)| world.village_doors_with_bounded_annex(x, z).is_none())
            .expect("seed 42 has a location without a generated village");
        let y = 200;
        let at = PlayerPosition {
            x: f64::from(x) + 0.5,
            y: f64::from(y),
            z: f64::from(z) + 3.5,
        };
        world.import_block(x, y - 1, z, 1 << 4).unwrap();
        world.import_block(x, y, z, 64 << 4).unwrap();
        world.import_block(x, y + 1, z, 64 << 4 | 8).unwrap();
        assert_eq!(villager_door_destination(&world, at), None);
        world.import_block(x, y + 3, z + 1, 1 << 4).unwrap();
        assert_eq!(
            villager_door_destination(&world, at),
            Some(PlayerPosition {
                x: f64::from(x) + 0.5,
                y: f64::from(y),
                z: f64::from(z) + 1.5,
            })
        );
        // A door on a different floor must not attract this villager.
        let upper_floor = PlayerPosition {
            y: f64::from(y + 16),
            ..at
        };
        assert_eq!(villager_door_destination(&world, upper_floor), None);
        world.import_block(x, y + 3, z + 1, 0).unwrap();
        assert_eq!(villager_door_destination(&world, at), None);
        assert_eq!(
            villager_door_destination(
                &World::expanded(42, crate::dimensions::Dimension::Nether),
                at
            ),
            None
        );
    }

    #[test]
    fn villager_can_target_roofed_player_house_above_generated_village() {
        let mut world = World::expanded(42, crate::dimensions::Dimension::Overworld);
        let (x, z) = (-8..=8)
            .flat_map(|gx| (-8..=8).map(move |gz| (gx * 192 + 48, gz * 192 + 48)))
            .find(|&(x, z)| world.live_village_house_doors(x, z).is_some())
            .expect("seed 42 has a generated village");
        let y = 200;
        // The house is outside the generated footprint and cannot connect
        // to its ground-level doors, but is within 32 blocks of this villager.
        let house_x = x + 40;
        let at = PlayerPosition {
            x: f64::from(x + 9) + 0.5,
            y: f64::from(y),
            z: f64::from(z) + 3.5,
        };
        assert!(world.village_doors_with_bounded_annex(x + 9, z).is_some());
        assert_eq!(
            villager_door_destination(&world, at),
            None,
            "generated ground-level doors must not attract an upper-floor villager"
        );

        // The independent house must remain eligible even while the generated
        // village still has intact doors. Previously Some(generated doors)
        // suppressed the independent-house lookup altogether.
        for (bx, by, bz, block) in [
            (house_x, y - 1, z, 1 << 4),
            (house_x, y, z, 64 << 4),
            (house_x, y + 1, z, 64 << 4 | 8),
            (house_x, y + 3, z + 1, 1 << 4),
        ] {
            world.import_block(bx, by, bz, block).unwrap();
        }
        let goal = PlayerPosition {
            x: f64::from(house_x) + 0.5,
            y: f64::from(y),
            z: f64::from(z) + 1.5,
        };
        assert_eq!(villager_door_destination(&world, at), Some(goal));
        world.import_block(house_x, y + 3, z + 1, 0).unwrap();
        assert_eq!(villager_door_destination(&world, at), None);
        world.import_block(house_x, y + 3, z + 1, 1 << 4).unwrap();
        assert_eq!(villager_door_destination(&world, at), Some(goal));
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
            nbt: None,
        });
        assert_eq!(mobs.village_offer(0, at, emerald).unwrap().id, 297);
        assert!(mobs
            .village_offer(
                0,
                at,
                Some(Stack {
                    id: 1,
                    count: 1,
                    damage: 0,
                    nbt: None,
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
    fn wither_death_drops_exactly_one_nether_star() {
        let mut mobs = Mobs::default();
        let at = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        mobs.insert(Kind::Wither, at, false);
        let wither = *mobs.entities.values().next().unwrap();
        let (star, position) = wither.death_drop().expect("Nether Star reward");
        assert_eq!(star.id, 399);
        assert_eq!(star.count, 1);
        assert_eq!(position, (0, 65, 0));
        assert_eq!(wither.experience_on_player_kill(), 50);
        assert!(Kind::EnderDragon.drop(1).is_none());
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
    fn enderman_teleport_rejects_low_ceiling_liquid_and_missing_floor() {
        let mut world = World::new();
        let standing = PlayerPosition {
            x: 0.5,
            y: 64.0,
            z: 0.5,
        };
        assert!(enderman_teleport_safe(&world, standing));
        world.import_block(0, 66, 0, 1 << 4).unwrap();
        assert!(!enderman_teleport_safe(&world, standing));
        world.import_block(0, 66, 0, 0).unwrap();
        world.import_block(0, 64, 0, 9 << 4).unwrap();
        assert!(!enderman_teleport_safe(&world, standing));
        world.import_block(0, 64, 0, 0).unwrap();
        world.import_block(0, 63, 0, 0).unwrap();
        assert!(!enderman_teleport_safe(&world, standing));
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
        assert!(baby.body.windows(2).any(|bytes| bytes == [12, 1]));
        assert!(!baby
            .body
            .windows(5)
            .any(|bytes| bytes == [76, 255, 255, 162, 64]));
        let villager = mobs.insert(Kind::Villager, pos, false);
        assert!(villager
            .body
            .windows(5)
            .any(|bytes| bytes == [80, 0, 0, 0, 0]));
        let baby_pigman = mobs.insert(Kind::ZombiePigman, pos, true);
        assert!(baby_pigman.body.windows(2).any(|bytes| bytes == [12, 1]));
        let baby_cow = mobs.insert(Kind::Cow, pos, true);
        assert!(baby_cow
            .body
            .windows(5)
            .any(|bytes| bytes == [76, 255, 255, 162, 64]));
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
    fn pigman_retaliation_spreads_to_nearby_pigmen_but_not_distant_or_other_mobs() {
        let mut mobs = Mobs::default();
        let at = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        let attacked = mobs.next_id;
        mobs.insert(Kind::ZombiePigman, at, false);
        let nearby = mobs.next_id;
        mobs.insert(Kind::ZombiePigman, PlayerPosition { x: 31.5, ..at }, false);
        let above = mobs.next_id;
        mobs.insert(
            Kind::ZombiePigman,
            PlayerPosition {
                y: at.y + 17.0,
                ..at
            },
            false,
        );
        let distant = mobs.next_id;
        mobs.insert(Kind::ZombiePigman, PlayerPosition { x: 33.5, ..at }, false);
        let other_kind = mobs.next_id;
        mobs.insert(Kind::Cow, at, false);

        assert!(mobs.entities.values().all(|mob| mob.anger_until == 0));
        assert!(mobs.player_attack(attacked, at, None, 20).is_some());
        assert_eq!(mobs.entities[&attacked].anger_until, 1220);
        assert_eq!(mobs.entities[&nearby].anger_until, 1220);
        assert_eq!(mobs.entities[&above].anger_until, 0);
        assert_eq!(mobs.entities[&distant].anger_until, 0);
        assert_eq!(mobs.entities[&other_kind].anger_until, 0);
        // Another attack extends the shared timer instead of shortening it.
        assert!(mobs.player_attack(attacked, at, None, 40).is_some());
        assert_eq!(mobs.entities[&nearby].anger_until, 1240);
        assert!(mobs.hurt_player(900, at, 1240) == 0);
    }

    #[test]
    fn hard_zombie_breaks_only_closed_wooden_door_after_240_ticks() {
        let path = std::env::temp_dir().join(format!(
            "industrialrust-hard-zombie-door-{}.irw",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut world = World::new();
        assert!(world.place_door(&path, 1, 64, 0, 64, 0).unwrap());
        let from = PlayerPosition {
            x: 0.5,
            y: 64.0,
            z: 0.5,
        };
        let target = PlayerPosition { x: 4.5, ..from };
        assert_eq!(
            closed_wooden_door_on_path(&world, from, target),
            Some([1, 64, 0])
        );
        let mut mobs = Mobs {
            difficulty: Some(3),
            ..Mobs::default()
        };
        mobs.insert(Kind::Zombie, from, false);
        mobs.entities.get_mut(&0).unwrap().can_break_wooden_doors = true;
        assert!(mobs
            .break_wooden_doors(&mut world, &path, 20, &[target])
            .unwrap()
            .is_empty());
        assert!(mobs
            .break_wooden_doors(&mut world, &path, 259, &[target])
            .unwrap()
            .is_empty());
        assert_eq!(world.get(1, 64, 0) >> 4, 64);
        let changed = mobs
            .break_wooden_doors(&mut world, &path, 260, &[target])
            .unwrap();
        assert_eq!(changed, vec![(1, 64, 0), (1, 65, 0)]);
        assert_eq!(world.get(1, 64, 0), 0);
        assert_eq!(world.get(1, 65, 0), 0);
        let drops = mobs.take_drops();
        assert_eq!(drops.len(), 1);
        assert_eq!(drops[0].0.id, 324);
        assert!(mobs
            .break_wooden_doors(&mut world, &path, 261, &[target])
            .unwrap()
            .is_empty());
        assert!(mobs.take_drops().is_empty());
        let restored = World::load(&path).unwrap();
        assert_eq!(restored.get(1, 64, 0), 0);
        assert_eq!(restored.get(1, 65, 0), 0);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn zombies_cannot_break_on_nonhard_or_through_open_and_iron_doors() {
        let path = std::env::temp_dir().join(format!(
            "industrialrust-door-guard-{}.irw",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut world = World::new();
        assert!(world.place_door(&path, 1, 64, 0, 64, 0).unwrap());
        let from = PlayerPosition {
            x: 0.5,
            y: 64.0,
            z: 0.5,
        };
        let target = PlayerPosition { x: 4.5, ..from };
        let mut mobs = Mobs {
            difficulty: Some(2),
            ..Mobs::default()
        };
        mobs.insert(Kind::Zombie, from, false);
        mobs.entities.get_mut(&0).unwrap().can_break_wooden_doors = true;
        assert!(mobs
            .break_wooden_doors(&mut world, &path, 20, &[target])
            .unwrap()
            .is_empty());
        mobs.difficulty = Some(3);
        // Entering Hard does not retroactively accrue Normal-mode progress.
        assert!(mobs
            .break_wooden_doors(&mut world, &path, 260, &[target])
            .unwrap()
            .is_empty());
        assert_eq!(world.get(1, 64, 0) >> 4, 64);
        // An opened door is not a valid breaking target.
        world
            .update_and_save(&path, 1, 64, 0, (64 << 4) | 4)
            .unwrap();
        assert_eq!(closed_wooden_door_on_path(&world, from, target), None);
        mobs.break_wooden_doors(&mut world, &path, 600, &[target])
            .unwrap();
        assert_eq!(world.get(1, 64, 0) >> 4, 64);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn slime_chunks_match_java_1710_seed_and_negative_chunk_coordinates() {
        // Golden answers computed from java.util.Random.nextInt(10), not the
        // mob tick's hash-based spawn-attempt RNG.
        for (seed, cx, cz, expected) in [
            (0_u64, 0, 0, false),
            (1, 12, -3, true),
            (1, 100, 100, true),
            (1, -1, -1, false),
            (12345, -100, 300, true),
            (12345, 12, -3, false),
            (u64::MAX, 0, 0, false),
        ] {
            assert_eq!(
                is_slime_chunk(seed, cx, cz),
                expected,
                "seed={seed}, chunk=({cx},{cz})"
            );
        }
        // A block coordinate of -1 belongs to chunk -1, not chunk 0.
        assert_eq!(
            is_slime_cave_location(1, -1, 20, -1),
            is_slime_chunk(1, -1, -1)
        );
        assert!(is_slime_cave_location(1, 12 * 16, 39, -3 * 16));
        assert!(!is_slime_cave_location(1, 12 * 16, 40, -3 * 16));
        assert!(!is_slime_cave_location(1, 12 * 16 + 16, 39, -3 * 16));
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
    fn world_spawn_and_player_exclusion_apply_to_all_natural_candidates() {
        let world = World::natural(42);
        let y = world.spawn_y();
        let player = PlayerPosition { x: 60.5, y, z: 0.5 };
        let candidate = |x| PlayerPosition { x, y, z: 0.5 };
        assert!(!natural_spawn_location_ok(
            &world,
            candidate(24.4),
            &[player]
        ));
        assert!(!natural_spawn_location_ok(
            &world,
            candidate(24.5),
            &[player]
        ));
        assert!(natural_spawn_location_ok(
            &world,
            candidate(24.501),
            &[player]
        ));
        assert!(!natural_spawn_location_ok(
            &world,
            candidate(36.6),
            &[candidate(40.0)]
        ));
        assert!(!natural_spawn_location_ok(
            &world,
            candidate(200.5),
            &[player]
        ));
        assert!(!natural_spawn_location_ok(&world, candidate(45.5), &[]));
        assert!(Mobs::new_with_difficulty(&world, true, 0)
            .entities
            .is_empty());
        assert!(Mobs::new_with_difficulty(&world, true, 3)
            .entities
            .is_empty());
    }
    #[test]
    fn zombie_attacks_player_before_equally_close_villager() {
        let at = PlayerPosition::default();
        let world = World::new();
        let mut mobs = Mobs {
            difficulty: Some(2),
            ..Mobs::default()
        };
        mobs.insert(Kind::Zombie, at, false);
        mobs.insert(Kind::Villager, at, false);
        let mut packets = Vec::new();
        mobs.zombies_attack_villagers(20, &world, &[at], &mut packets);
        assert!(packets.is_empty());
        assert_eq!(mobs.entities.get(&1).unwrap().health, 20);
        assert_eq!(mobs.hurt_player(900, at, 21), 3);
    }
    #[test]
    fn zombie_villager_infection_survives_restart_and_new_clients() {
        let world = World::natural(2468);
        let path = std::env::temp_dir().join(format!(
            "industrialrust-mobs-{}-infected.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut mobs = Mobs::load_or_new(&world, true, 3, &path).unwrap();
        let at = PlayerPosition {
            x: 40.5,
            y: 200.0,
            z: 40.5,
        };
        let attacker_id = mobs.next_id;
        mobs.insert(Kind::Zombie, at, false);
        let original_villager_id = mobs.next_id;
        mobs.insert(
            Kind::Villager,
            PlayerPosition {
                x: at.x + 1.0,
                ..at
            },
            true,
        );
        mobs.entities
            .get_mut(&original_villager_id)
            .unwrap()
            .profession = 4;
        let mut events = Vec::new();
        for tick in (20..=140).step_by(20) {
            mobs.zombies_attack_villagers(tick, &world, &[], &mut events);
        }

        let infected = *mobs
            .entities
            .values()
            .find(|mob| mob.zombie_villager)
            .expect("hard difficulty should infect the villager");
        assert_eq!(events.iter().filter(|event| event.id == 0x13).count(), 1);
        assert_eq!(events.iter().filter(|event| event.id == 0x0f).count(), 1);
        assert!(!mobs.entities.contains_key(&original_villager_id));
        assert!(infected.id > original_villager_id);
        assert!(infected.baby);
        assert_eq!(infected.profession, 4);
        mobs.save(&world).unwrap();

        let restored = Mobs::load_or_new(&world, true, 3, &path).unwrap();
        assert_eq!(restored.entities.len(), 2);
        assert!(restored.entities.contains_key(&attacker_id));
        assert!(!restored.entities.contains_key(&original_villager_id));
        let rejoined = *restored.entities.get(&infected.id).unwrap();
        assert_eq!(rejoined.kind, Kind::Zombie);
        assert!(rejoined.zombie_villager);
        assert_eq!(rejoined.profession, 4);
        assert!(rejoined.baby);
        assert_eq!(rejoined.health, infected.health);
        assert_eq!(restored.next_id, infected.id + 1);
        // An observer connecting only after restart sees the infected flag
        // and the same unique ID through the authoritative spawn snapshot.
        let snapshot = restored
            .spawn_events()
            .find(|event| event.id == 0x0f && event.body.windows(2).any(|w| w == [13, 1]))
            .expect("new client must receive zombie-villager metadata");
        let mut expected_id = Vec::new();
        write_varint(infected.id as u32, &mut expected_id);
        assert!(snapshot.body.starts_with(&expected_id));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn zombie_villager_metadata_and_infection_follow_difficulty() {
        let at = PlayerPosition {
            x: 0.5,
            y: 64.0,
            z: 0.5,
        };
        let world = World::new();
        for (difficulty, convert) in [(1, false), (3, true)] {
            let mut mobs = Mobs {
                difficulty: Some(difficulty),
                ..Mobs::default()
            };
            let zombie_id = mobs.next_id;
            mobs.insert(Kind::Zombie, at, false);
            let villager_id = mobs.next_id;
            mobs.insert(
                Kind::Villager,
                PlayerPosition {
                    x: at.x + 1.0,
                    ..at
                },
                false,
            );
            mobs.entities.get_mut(&villager_id).unwrap().profession = 2;
            let mut all_events = Vec::new();
            for tick in (20..=140).step_by(20) {
                mobs.zombies_attack_villagers(tick, &world, &[], &mut all_events);
            }
            assert!(!mobs.entities.contains_key(&villager_id));
            assert!(all_events.iter().any(|event| event.id == 0x13));
            let infected: Vec<_> = mobs
                .entities
                .values()
                .filter(|mob| mob.zombie_villager)
                .collect();
            assert_eq!(infected.len(), usize::from(convert));
            assert!(mobs.entities.contains_key(&zombie_id));
            if convert {
                let infected = infected[0];
                assert!(infected.id != villager_id && infected.id != zombie_id);
                assert_eq!(infected.profession, 2);
                assert!(infected
                    .spawn()
                    .body
                    .windows(2)
                    .any(|entry| entry == [13, 1]));
            }
        }
        let mut previous = Mobs::default();
        previous.insert(Kind::Zombie, at, false);
        let mut legacy = serde_json::to_value(previous.entities.get(&0).unwrap()).unwrap();
        let _ = legacy.as_object_mut().unwrap().remove("zombie_villager");
        let old: Mob = serde_json::from_value(legacy).unwrap();
        assert!(!old.zombie_villager);
        assert!(!zombie_infects_villager(1, 0));
        assert!(zombie_infects_villager(2, 0));
        assert!(!zombie_infects_villager(2, 1));
        assert!(zombie_infects_villager(3, 1));
    }
    #[test]
    fn natural_spawning_skips_malformed_players_without_losing_valid_origins() {
        // The first origin can select an invalid player when the online
        // snapshot mixes malformed coordinates with a legitimate player.
        // Such entries must not consume the bounded four origin attempts.
        let world = World::natural(0);
        let valid = PlayerPosition {
            x: 512.5,
            y: world.spawn_y(),
            z: 512.5,
        };
        let invalid = [
            PlayerPosition {
                x: f64::NAN,
                ..valid
            },
            PlayerPosition {
                z: f64::INFINITY,
                ..valid
            },
            PlayerPosition {
                x: f64::from(i32::MAX) + 1.0,
                ..valid
            },
            // These are valid for chunk-union mob-cap accounting, but not
            // for choosing an in-world natural-spawn origin.
            PlayerPosition { y: -1.0, ..valid },
            PlayerPosition { y: 256.0, ..valid },
            PlayerPosition { y: 300.0, ..valid },
            PlayerPosition {
                x: f64::from(crate::world::WORLD_LIMIT),
                ..valid
            },
            PlayerPosition {
                z: -f64::from(crate::world::WORLD_LIMIT) - 1.0,
                ..valid
            },
        ];
        let now = 18_040;
        let mut baseline = Mobs::new_with_difficulty(&world, true, 2);
        let mut baseline_events = Vec::new();
        let mut baseline_lighting = world.lighting_sampler();
        baseline.spawn_natural(
            now,
            &world,
            &mut baseline_lighting,
            &[valid],
            &mut baseline_events,
        );
        assert!(!baseline.entities.is_empty());

        let snapshot = |mobs: &Mobs| {
            mobs.entities
                .values()
                .map(|mob| {
                    (
                        mob.kind,
                        mob.position.x.to_bits(),
                        mob.position.y.to_bits(),
                        mob.position.z.to_bits(),
                    )
                })
                .collect::<Vec<_>>()
        };
        for bad in invalid {
            for online in [[bad, valid], [valid, bad]] {
                let mut mixed = Mobs::new_with_difficulty(&world, true, 2);
                let mut mixed_events = Vec::new();
                let mut mixed_lighting = world.lighting_sampler();
                mixed.spawn_natural(now, &world, &mut mixed_lighting, &online, &mut mixed_events);
                assert_eq!(snapshot(&mixed), snapshot(&baseline));
                assert_eq!(mixed_events.len(), baseline_events.len());
            }
        }
        let mut invalid_only = Mobs::new_with_difficulty(&world, true, 2);
        let mut invalid_events = Vec::new();
        let mut invalid_lighting = world.lighting_sampler();
        invalid_only.spawn_natural(
            now,
            &world,
            &mut invalid_lighting,
            &invalid,
            &mut invalid_events,
        );
        assert!(invalid_only.entities.is_empty());
        assert!(invalid_events.is_empty());
    }

    #[test]
    fn natural_spawning_catch_up_does_not_skip_a_crossed_twenty_tick_boundary() {
        // The poll ends at 18_041, but the two simulated ticks include
        // 18_040. The old now-only gate skipped the entire spawn attempt.
        let world = World::natural(0);
        let player = PlayerPosition {
            x: 512.5,
            y: world.spawn_y(),
            z: 512.5,
        };
        let mut mobs = Mobs::new_with_difficulty(&world, true, 2);
        mobs.last_tick = 18_039;
        let events = mobs.tick(18_041, &world, &[player]);
        assert!(
            mobs.entities.values().any(|mob| mob.kind == Kind::Zombie),
            "the crossed 20-tick boundary must trigger the eligible pack"
        );
        assert!(
            events.iter().any(|event| event.id == 0x0f),
            "the spawned mob must be announced to Protocol 5 clients"
        );
        let entity_count = mobs.entities.len();
        assert!(mobs.tick(18_041, &world, &[player]).is_empty());
        assert_eq!(mobs.entities.len(), entity_count);
    }

    #[test]
    fn natural_spawning_retries_after_a_pack_with_no_eligible_members() {
        // At this tick, the first origin selects a surface slime in plains
        // (ineligible), while the second selects a zombie. A failed first
        // pack must not stop the remaining three bounded origin attempts.
        let world = World::natural(0);
        let now = 18_040_u64;
        let first_seed = hash(now);
        let second_seed = hash(now ^ 0x9e3779b97f4a7c15);
        assert_eq!(
            overworld_monster_kind(hash(first_seed ^ 0xd1a7c4b500137e6d)),
            Kind::Slime
        );
        assert_eq!(
            overworld_monster_kind(hash(second_seed ^ 0xd1a7c4b500137e6d)),
            Kind::Zombie
        );
        let player = PlayerPosition {
            x: 512.5,
            y: world.spawn_y(),
            z: 512.5,
        };
        let (dx, dz) = natural_spawn_horizontal_offset(first_seed);
        let first_y = world.surface_height_at(512 + dx, 512 + dz).unwrap() + 1;
        assert!(first_y >= 40);
        assert_eq!(world.biome_at(512 + dx, 512 + dz), 1);

        let mut mobs = Mobs::new_with_difficulty(&world, true, 2);
        let mut events = Vec::new();
        let mut lighting = world.lighting_sampler();
        mobs.spawn_natural(now, &world, &mut lighting, &[player], &mut events);
        assert!(
            mobs.entities.values().any(|mob| mob.kind == Kind::Zombie),
            "second origin must be attempted after the rejected slime pack"
        );
        assert!(!events.is_empty());
    }

    #[test]
    fn natural_mobs_spawn_in_protocol5() {
        let world = World::natural(42);
        let mut mobs = Mobs::new_with_difficulty(&world, true, 1);
        // Starting a new world never sidesteps the spawn eligibility checks.
        assert!(mobs.entities.is_empty());
        assert_eq!(mobs.next_id, BASE_ID);
        let position = PlayerPosition {
            x: 32.5,
            y: world.spawn_y(),
            z: 32.5,
        };
        mobs.insert(Kind::Zombie, position, false);
        let zombie = *mobs.entities.values().next().unwrap();
        assert_eq!(zombie.kind, Kind::Zombie);
        let packet = zombie.spawn();
        assert_eq!(packet.id, 0x0f);
        assert_eq!(packet.body[3], 54);
        assert!(packet.body.ends_with(&[0x66, 0x41, 0xa0, 0, 0, 0x7f]));
        let position = zombie.position;
        for tick in [100, 111] {
            assert!(mobs
                .player_attack(
                    zombie.id,
                    position,
                    Some(Stack {
                        id: 276,
                        count: 1,
                        damage: 0,
                        nbt: None,
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
                    damage: 0,
                    nbt: None,
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
    fn passive_animals_require_two_feedings_and_obey_breeding_cooldown() {
        let mut mobs = Mobs::default();
        let at = PlayerPosition::default();
        let wheat = Some(Stack {
            id: 296,
            count: 1,
            damage: 0,
            nbt: None,
        });
        mobs.insert(Kind::Cow, at, false);
        mobs.insert(
            Kind::Cow,
            PlayerPosition {
                x: at.x + 1.0,
                ..at
            },
            false,
        );
        let first = mobs.feed(0, at, wheat, 100).unwrap();
        assert_eq!(first.len(), 1); // heart effect only
        assert!(!mobs.entities.values().any(|m| m.baby));
        assert!(mobs.feed(0, at, wheat, 101).is_none()); // no repeated payment
        let second = mobs.feed(1, at, wheat, 102).unwrap();
        assert_eq!(second.len(), 2); // hearts plus authoritative calf
        assert_eq!(mobs.entities.values().filter(|m| m.baby).count(), 1);
        assert!(mobs.feed(0, at, wheat, 103).is_none());
        assert!(mobs.feed(1, at, wheat, 6101).is_none());
        assert!(mobs.feed(0, at, wheat, 6102).is_some());
    }

    #[test]
    fn natural_cap_excludes_persistent_villagers_guardians_and_bosses() {
        for species in [
            Kind::Villager,
            Kind::IronGolem,
            Kind::Wither,
            Kind::EnderDragon,
        ] {
            assert!(!species.counts_toward_natural_cap(true));
            assert!(!species.counts_toward_natural_cap(false));
        }
        assert!(Kind::Cow.counts_toward_natural_cap(true));
        assert!(Kind::Ghast.counts_toward_natural_cap(false));
        assert!(!Kind::Ghast.counts_toward_natural_cap(true));
    }

    #[test]
    fn natural_ghast_origin_does_not_require_ground_support() {
        let world = World::new();
        let floating = PlayerPosition {
            x: 64.5,
            y: 80.0,
            z: 0.5,
        };
        let player = PlayerPosition { x: 0.5, ..floating };
        assert!(!physics::supported(&world, &floating));
        assert!(natural_spawn_origin_ok(&world, floating, &[player]));
        assert!(natural_spawn_body_ok(&world, Kind::Ghast, floating, 1));
        assert!(!natural_spawn_body_ok(&world, Kind::Skeleton, floating, 1));
    }

    #[test]
    fn hostile_arrows_can_hit_other_mobs_without_hitting_the_shooter() {
        let mut mobs = Mobs::default();
        let at = PlayerPosition {
            x: 0.5,
            y: 70.0,
            z: 0.5,
        };
        mobs.insert(Kind::Skeleton, at, false);
        mobs.insert(Kind::Cow, PlayerPosition { x: 2.5, ..at }, false);
        mobs.fire_arrow(Some(0), at, 0.0, 0.0, 1.5, 3).unwrap();
        let projectile = mobs.arrows.values_mut().next().unwrap();
        projectile.position = PlayerPosition {
            x: 1.6,
            y: 71.3,
            z: 0.5,
        };
        projectile.velocity = [1.0, 0.0, 0.0];
        let mut events = Vec::new();
        mobs.tick_arrows(&World::new(), &[], 10, &mut events);
        assert_eq!(mobs.entities.get(&0).unwrap().health, 20);
        assert_eq!(mobs.entities.get(&1).unwrap().health, 7);
        assert!(mobs.arrows.is_empty());
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
    fn explosion_exposure_samples_both_horizontal_axes_symmetrically() {
        let from = PlayerPosition {
            x: 0.5,
            y: 66.0,
            z: 0.5,
        };
        let east = PlayerPosition { x: 4.5, ..from };
        let south = PlayerPosition { z: 4.5, ..from };
        let clear = World::new();
        assert_eq!(blast_exposure(&clear, from, east), 1.0);
        assert_eq!(blast_exposure(&clear, from, south), 1.0);

        let mut east_wall = World::new();
        let mut south_wall = World::new();
        for y in 66..=68 {
            for offset in -1..=1 {
                east_wall.import_block(2, y, offset, 1 << 4).unwrap();
                south_wall.import_block(offset, y, 2, 1 << 4).unwrap();
            }
        }
        let east_exposure = blast_exposure(&east_wall, from, east);
        let south_exposure = blast_exposure(&south_wall, from, south);
        assert!(east_exposure < 1.0);
        assert_eq!(east_exposure, south_exposure);
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
            nbt: None,
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
    #[test]
    fn dispenser_arrow_mob_damage_and_death_are_not_duplicated() {
        let mut mobs = Mobs::default();
        let at = PlayerPosition {
            x: 2.0,
            y: 70.0,
            z: 0.5,
        };
        let id = mobs.next_id;
        mobs.insert(Kind::Cow, at, false);
        let initial_health = mobs.entities[&id].health;
        assert!(mobs
            .projectile_targets()
            .iter()
            .any(|target| target.0 == id));
        let (hit_at, events) = mobs.damage_from_dispenser_arrow(id, 20).unwrap();
        assert_eq!(hit_at.x, at.x);
        assert_eq!(events.len(), 2);
        assert_eq!(mobs.entities[&id].health, initial_health - 2);
        assert!(mobs.damage_from_dispenser_arrow(id, 20).is_none());
        assert_eq!(mobs.entities[&id].health, initial_health - 2);
        mobs.entities.get_mut(&id).unwrap().health = 2;
        let (_, events) = mobs.damage_from_dispenser_arrow(id, 30).unwrap();
        assert!(events.iter().any(|event| event.id == 0x13));
        assert!(!mobs.entities.contains_key(&id));
        assert!(mobs.damage_from_dispenser_arrow(id, 40).is_none());
        assert!(mobs
            .projectile_targets()
            .iter()
            .all(|target| target.0 != id));
    }
}
