//! Vanilla-only Minecraft 1.7.10 / Protocol 5 server proof of concept.
//! Offline-mode by design: bind to loopback unless you trust every client on your LAN.
//! This is NOT a Forge server, public-server auth, survival engine or Anvil implementation.
mod achievements;
mod admin;
mod anvil_export;
mod anvil_import;
mod backup_world;
mod brewing;
mod chests;
mod chunk_pipeline;
mod clock;
mod crafting;
mod dimensions;
mod dispensers;
mod experience;
mod fishing;
mod furnaces;
mod game_rules;
mod ground_items;
mod hoppers;
mod inventory;
mod items_1710;
mod journal;
mod legacy_chunk_store;
mod legacy_entity_nbt;
mod legacy_inventory_nbt;
mod legacy_nbt;
mod legacy_region;
mod legacy_world_inspect;
mod level_dat;
mod minecarts;
mod mobs;
mod multiplayer;
mod native_tile_export;
mod navigation;
mod online_auth;
mod physics;
mod player;
mod scoreboard;
mod security_limits;
mod server_config;
mod session_lock;
mod survival;
mod survival_damage;
mod telemetry;
mod terrain;
mod throwables;
mod vehicles;
mod vitals;
mod whitelist;
mod world;
mod worldgen;

use achievements::Achievements;
use admin::Administration;
use brewing::{Brewer, Brewers, Position as BrewerPosition};
use chests::{Chests, Position as ChestPosition};
use clock::Clock;
use dispensers::{Devices, Position as DevicePosition};
use experience::Experience;
use fishing::Fishing;
use furnaces::{Furnace, Furnaces, Position as FurnacePosition};
use game_rules::GameRules;
use ground_items::GroundItems;
use hoppers::{Hoppers, Position as HopperPosition};
use inventory::{Inventory, Stack};
use legacy_protocol::{
    read_frame, read_varint, write_frame, write_string, write_varint, BufferedFrameReader,
    PROTOCOL_VERSION,
};
use minecarts::Minecarts;
use mobs::Mobs;
use multiplayer::{
    Event, Hub, JoinError, ADMIN_KICK_EVENT, ADMIN_TELEPORT_EVENT, MAX_PLAYERS,
    PEARL_TELEPORT_EVENT,
};
use online_auth::{Connection, ServerIdentity};
use physics::{FallTracker, IdleGravity};
use player::PlayerPosition;
use scoreboard::Scoreboard;
use security_limits::{ChatLimiter, ConnectionLimiter};
use serde_json::json;
use std::collections::BTreeSet;
use std::io::{self, BufRead, ErrorKind};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use survival::MiningProgress;
use survival_damage::Hazards;
use throwables::Throwables;
use vehicles::Boats;
use vitals::Vitals;
use whitelist::Whitelist;
use world::{World, MAX_CHUNK, MIN_CHUNK, VIEW_DISTANCE};

const MAX_CONNECTIONS: usize = 32;
// Internal to Hub; no such packet ID is ever sent on the Protocol 5 wire.
const CHUNK_RELIGHT_EVENT: u32 = u32::MAX;
fn bad(reason: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, reason)
}
struct Input<'a>(&'a [u8]);
impl<'a> Input<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let bytes = self.0.get(..n).ok_or_else(|| bad("short packet"))?;
        self.0 = &self.0[n..];
        Ok(bytes)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn i8(&mut self) -> io::Result<i8> {
        Ok(self.u8()? as i8)
    }
    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn i16(&mut self) -> io::Result<i16> {
        Ok(self.u16()? as i16)
    }
    fn i32(&mut self) -> io::Result<i32> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> io::Result<f32> {
        Ok(f32::from_bits(self.i32()? as u32))
    }
    fn f64(&mut self) -> io::Result<f64> {
        Ok(f64::from_bits(u64::from_be_bytes(
            self.take(8)?.try_into().unwrap(),
        )))
    }
    fn varint(&mut self) -> io::Result<i32> {
        Ok(read_varint(&mut self.0)? as i32)
    }
    fn string(&mut self, limit: usize) -> io::Result<String> {
        let size = self.varint()?;
        if size < 0 || size as usize > limit.saturating_mul(4) {
            return Err(bad("invalid string size"));
        }
        let s = std::str::from_utf8(self.take(size as usize)?).map_err(|_| bad("invalid utf8"))?;
        if s.chars().count() > limit {
            return Err(bad("too many characters"));
        }
        Ok(s.to_owned())
    }
    fn slot(&mut self) -> io::Result<Option<Stack>> {
        let id = self.i16()?;
        if id == -1 {
            return Ok(None);
        }
        let count = self.u8()?;
        let damage = self.i16()?;
        let nbt_length = self.i16()?;
        if nbt_length != -1 {
            return Err(bad("NBT item stacks are not supported yet"));
        }
        let stack = Stack { id, count, damage };
        if !stack.is_valid() {
            return Err(bad("invalid item ID, count or damage"));
        }
        Ok(Some(stack))
    }
    /// Protocol 5 Click Window carries a *client-reported* slot snapshot.
    /// The snapshot is never used to authorize a mutation, and may contain
    /// valid NBT even when the server-side Stack model cannot store tags yet.
    /// Consume its exact length without converting or losing an item.
    fn skip_untrusted_slot(&mut self) -> io::Result<()> {
        let id = self.i16()?;
        if id == -1 {
            return Ok(());
        }
        self.take(3)?; // count + damage
        let nbt_len = self.i16()?;
        if nbt_len < -1 {
            return Err(bad("invalid claimed item NBT length"));
        }
        if nbt_len >= 0 {
            self.take(nbt_len as usize)?;
        }
        Ok(())
    }
    fn finish(&self) -> io::Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(bad("unexpected packet bytes"))
        }
    }
}
fn receive(s: &mut Connection) -> io::Result<(i32, Vec<u8>)> {
    let data = read_frame(s)?;
    let mut r = Input(&data);
    let id = r.varint()?;
    Ok((id, r.0.to_vec()))
}
fn send(s: &mut Connection, id: u32, body: &[u8]) -> io::Result<()> {
    let mut packet = Vec::with_capacity(5 + body.len());
    write_varint(id, &mut packet);
    packet.extend_from_slice(body);
    write_frame(s, &packet)
}
fn as_chat(s: &mut Connection, message: &str) -> io::Result<()> {
    let mut bytes = Vec::new();
    write_string(&json!({"text": message}).to_string(), &mut bytes)?;
    send(s, 0x02, &bytes)
}
fn login_disconnect(s: &mut Connection, reason: &str) -> io::Result<()> {
    let mut payload = Vec::new();
    write_string(&json!({"text":reason}).to_string(), &mut payload)?;
    send(s, 0x00, &payload)
}
fn offline_uuid(name: &str) -> String {
    let mut digest = md5::compute(format!("OfflinePlayer:{name}").as_bytes()).0;
    digest[6] = digest[6] & 0x0f | 0x30;
    digest[8] = digest[8] & 0x3f | 0x80;
    let hex: String = digest.iter().map(|v| format!("{v:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}
fn status(s: &mut Connection, hub: &Hub) -> io::Result<()> {
    let (id, body) = receive(s)?;
    if id != 0 || !body.is_empty() {
        return Err(bad("invalid status request"));
    }
    let status = json!({
        "version":{"name":"1.7.10","protocol":PROTOCOL_VERSION},
        "players":{"max":MAX_PLAYERS,"online":hub.count()?,"sample":[]},
        "description":{"text":"IndustrialRust1710 vanilla-only prototype"}
    });
    let mut bytes = Vec::new();
    write_string(&status.to_string(), &mut bytes)?;
    send(s, 0, &bytes)?;
    match receive(s) {
        Ok((1, data)) if data.len() == 8 => send(s, 1, &data),
        Err(e)
            if matches!(
                e.kind(),
                ErrorKind::UnexpectedEof | ErrorKind::WouldBlock | ErrorKind::TimedOut
            ) =>
        {
            Ok(())
        }
        _ => Err(bad("bad status ping")),
    }
}
struct InitialPlayer<'a> {
    position: &'a PlayerPosition,
    inventory: &'a Inventory,
    vitals: Vitals,
    survival: bool,
    hardcore: bool,
    difficulty: u8,
}
fn join(
    s: &mut Connection,
    world: &Mutex<World>,
    clock: &Clock,
    entity_id: i32,
    initial: InitialPlayer<'_>,
) -> io::Result<()> {
    let InitialPlayer {
        position,
        inventory,
        vitals,
        survival,
        hardcore,
        difficulty,
    } = initial;
    // S01 Join Game: entity i32, gamemode u8 (1=creative), dimension i8,
    // difficulty u8, maxPlayers u8, levelType string.
    let mut join = Vec::new();
    join.extend_from_slice(&entity_id.to_be_bytes());
    let dimension_id = world
        .lock()
        .map_err(|_| bad("world mutex poisoned"))?
        .extended_identity()
        .map_or(0, |(_, dim)| dim.protocol_id());
    join.extend_from_slice(&[
        u8::from(!survival) | if hardcore { 0x08 } else { 0 },
        dimension_id as u8,
        difficulty, // explicitly configured difficulty
        MAX_PLAYERS as u8,
    ]);
    write_string("default", &mut join)?;
    send(s, 0x01, &join)?;

    let mut abilities = vec![if survival { 0 } else { 0x0d }]; // Survival has no fly
    abilities.extend_from_slice(&0.05f32.to_be_bytes());
    abilities.extend_from_slice(&0.1f32.to_be_bytes());
    send(s, 0x39, &abilities)?;

    let mut spawn = Vec::new();
    let surface = world
        .lock()
        .map_err(|_| bad("world mutex poisoned"))?
        .spawn_y() as i32
        - 1;
    for coordinate in [0i32, surface, 0] {
        spawn.extend_from_slice(&coordinate.to_be_bytes());
    }
    send(s, 0x05, &spawn)?;
    send(s, 0x06, &health_packet(vitals))?;
    let mut time = Vec::new();
    time.extend_from_slice(&(clock.ticks() as i64).to_be_bytes());
    time.extend_from_slice(&clock.day_time().to_be_bytes());
    send(s, 0x03, &time)?;
    for weather in clock.weather_packets() {
        send(s, 0x2b, &weather)?;
    }

    // Bound per-session outstanding compression jobs, including simultaneous
    // logins. Encoding is done off the world tick mutex by shared workers.
    send_chunk_batch(s, world, view_chunks(chunk_center(position)))?;

    // Clientbound 0x30 / 0x09: authoritative Creative inventory and hotbar.
    send(s, 0x30, &inventory.window_items())?;
    send(s, 0x2f, &inventory.cursor_packet())?;
    send(s, 0x09, &[inventory.selected()])?;
    // Player starts at their last safely persisted position.
    send_player_position(s, position)?;
    as_chat(
        s,
        if survival {
            "Rust survival: mobs, ranged combat, XP, hazards, crafting, collision."
        } else {
            "Rust vanilla prototype: creative flat world. /help for commands."
        },
    )?;
    Ok(())
}

fn safe_spawn(world: &Mutex<World>) -> io::Result<PlayerPosition> {
    let y = world
        .lock()
        .map_err(|_| bad("world mutex poisoned"))?
        .spawn_y();
    Ok(PlayerPosition { x: 0.5, y, z: 0.5 })
}
fn chunk_center(player: &PlayerPosition) -> (i32, i32) {
    (
        (player.x.floor() as i32).div_euclid(16),
        (player.z.floor() as i32).div_euclid(16),
    )
}
fn view_chunks(center: (i32, i32)) -> BTreeSet<(i32, i32)> {
    let mut chunks = BTreeSet::new();
    for cz in center.1 - VIEW_DISTANCE..=center.1 + VIEW_DISTANCE {
        for cx in center.0 - VIEW_DISTANCE..=center.0 + VIEW_DISTANCE {
            if (MIN_CHUNK..=MAX_CHUNK).contains(&cx) && (MIN_CHUNK..=MAX_CHUNK).contains(&cz) {
                chunks.insert((cx, cz));
            }
        }
    }
    chunks
}
fn queue_chunk(world: &Mutex<World>, cx: i32, cz: i32) -> io::Result<chunk_pipeline::PendingChunk> {
    let (mask, data) = {
        let mut w = world.lock().map_err(|_| bad("world mutex poisoned"))?;
        w.observe_weather_chunk(cx, cz);
        w.chunk_data(cx, cz)
    };
    chunk_pipeline::submit(cx, cz, mask, data)
}
/// Submit at most eight chunks per session before draining them in wire order.
/// This avoids a large per-client raw-chunk backlog when players join together.
fn send_chunk_batch(
    s: &mut Connection,
    world: &Mutex<World>,
    coords: impl IntoIterator<Item = (i32, i32)>,
) -> io::Result<()> {
    let mut pending = Vec::with_capacity(chunk_pipeline::SESSION_BATCH_LIMIT);
    let mut deadline = Instant::now() + chunk_pipeline::BATCH_DEADLINE;
    for (cx, cz) in coords {
        pending.push(queue_chunk(world, cx, cz)?);
        if pending.len() == chunk_pipeline::SESSION_BATCH_LIMIT {
            for chunk in pending.drain(..) {
                send(s, 0x21, &chunk.finish_before(deadline)?)?;
            }
            // Reset only after the bounded batch has drained, not for each chunk.
            deadline = Instant::now() + chunk_pipeline::BATCH_DEADLINE;
        }
    }
    for chunk in pending {
        send(s, 0x21, &chunk.finish_before(deadline)?)?;
    }
    Ok(())
}

fn send_chunk(s: &mut Connection, world: &Mutex<World>, cx: i32, cz: i32) -> io::Result<()> {
    send(s, 0x21, &queue_chunk(world, cx, cz)?.finish()?)
}
fn unload_chunk(s: &mut Connection, cx: i32, cz: i32) -> io::Result<()> {
    // Protocol 5: full column with zero section bitmap and no data.
    let mut packet = Vec::with_capacity(17);
    packet.extend_from_slice(&cx.to_be_bytes());
    packet.extend_from_slice(&cz.to_be_bytes());
    packet.push(1);
    packet.extend_from_slice(&0u16.to_be_bytes());
    packet.extend_from_slice(&0u16.to_be_bytes());
    packet.extend_from_slice(&0i32.to_be_bytes());
    send(s, 0x21, &packet)
}
fn sync_chunks(
    s: &mut Connection,
    world: &Mutex<World>,
    loaded: &mut BTreeSet<(i32, i32)>,
    center: (i32, i32),
) -> io::Result<()> {
    let desired = view_chunks(center);
    for &(cx, cz) in loaded.difference(&desired) {
        unload_chunk(s, cx, cz)?;
    }
    send_chunk_batch(s, world, desired.difference(loaded).copied())?;
    *loaded = desired;
    Ok(())
}

fn health_packet(vitals: Vitals) -> Vec<u8> {
    let mut packet = Vec::with_capacity(10);
    packet.extend_from_slice(&vitals.health.to_be_bytes());
    packet.extend_from_slice(&(vitals.food as i16).to_be_bytes());
    packet.extend_from_slice(&vitals.saturation.to_be_bytes());
    packet
}
fn send_player_position(s: &mut Connection, location: &PlayerPosition) -> io::Result<()> {
    // Protocol 5 clientbound position/look: x, feet y, z, yaw, pitch, on_ground.
    let mut bytes = Vec::with_capacity(33);
    bytes.extend_from_slice(&location.x.to_be_bytes());
    bytes.extend_from_slice(&location.y.to_be_bytes());
    bytes.extend_from_slice(&location.z.to_be_bytes());
    bytes.extend_from_slice(&0f32.to_be_bytes());
    bytes.extend_from_slice(&0f32.to_be_bytes());
    bytes.push(0);
    send(s, 0x08, &bytes)
}
fn chunk_relight_event(cx: i32, cz: i32) -> Event {
    let mut body = Vec::with_capacity(8);
    body.extend_from_slice(&cx.to_be_bytes());
    body.extend_from_slice(&cz.to_be_bytes());
    Event {
        id: CHUNK_RELIGHT_EVENT,
        body,
    }
}

/// Legacy 1.7.10 S24 Block Action: X:Int, Y:Short, Z:Int,
/// action (0=extend, 1=retract), facing, and block-id VarInt.
fn piston_action_event(action: world::PistonAction) -> Event {
    let mut body = Vec::with_capacity(16);
    body.extend_from_slice(&action.pos.0.to_be_bytes());
    body.extend_from_slice(&(action.pos.1 as i16).to_be_bytes());
    body.extend_from_slice(&action.pos.2.to_be_bytes());
    body.push(u8::from(!action.extending));
    body.push(action.facing);
    write_varint(u32::from(action.block_id), &mut body);
    Event { id: 0x24, body }
}

fn block_change_event(world: &World, x: i32, y: i32, z: i32) -> Event {
    let block = world.get(x, y, z);
    let mut data = Vec::new();
    data.extend_from_slice(&x.to_be_bytes());
    data.push(y as u8);
    data.extend_from_slice(&z.to_be_bytes());
    write_varint((block >> 4) as u32, &mut data);
    data.push((block & 15) as u8);
    Event {
        id: 0x23,
        body: data,
    }
}
fn near(player: (f64, f64, f64), x: i32, y: i32, z: i32) -> bool {
    let dx = player.0 - (x as f64 + 0.5);
    let dy = player.1 - (y as f64 + 0.5);
    let dz = player.2 - (z as f64 + 0.5);
    dx * dx + dy * dy + dz * dz <= 64.0
}
/// Double chests are two adjacent, independently persisted 27-slot halves.
/// Canonical ordering is shared by all viewers regardless of clicked side.
fn chest_neighbor(world: &World, pos: ChestPosition) -> Option<ChestPosition> {
    let (x, y, z) = pos;
    [(x - 1, y, z), (x + 1, y, z), (x, y, z - 1), (x, y, z + 1)]
        .into_iter()
        .find(|&(x, y, z)| world.get(x, y, z) >> 4 == 54)
}
fn can_place_chest(world: &World, pos: ChestPosition) -> bool {
    let (x, y, z) = pos;
    let neighbors: Vec<_> = [(x - 1, y, z), (x + 1, y, z), (x, y, z - 1), (x, y, z + 1)]
        .into_iter()
        .filter(|&(x, y, z)| world.get(x, y, z) >> 4 == 54)
        .collect();
    if neighbors.len() > 1 {
        return false;
    }
    neighbors
        .first()
        .is_none_or(|&neighbor| chest_neighbor(world, neighbor).is_none())
}
fn chest_window_state(
    storage: &Chests,
    pos: ChestPosition,
    partner: Option<ChestPosition>,
) -> Vec<Option<Stack>> {
    let mut data = Vec::with_capacity(if partner.is_some() { 54 } else { 27 });
    if let Some(other) = partner {
        let (first, second) = if pos < other {
            (pos, other)
        } else {
            (other, pos)
        };
        data.extend_from_slice(&storage.slots(first));
        data.extend_from_slice(&storage.slots(second));
    } else {
        data.extend_from_slice(&storage.slots(pos));
    }
    data
}
fn stage_chest_window(
    storage: &Chests,
    pos: ChestPosition,
    partner: Option<ChestPosition>,
    slots: &[Option<Stack>],
) -> io::Result<Chests> {
    if slots.len() != if partner.is_some() { 54 } else { 27 } {
        return Err(bad("wrong chest slot count"));
    }
    let mut first = [None; 27];
    first.copy_from_slice(&slots[..27]);
    if let Some(other) = partner {
        let mut second = [None; 27];
        second.copy_from_slice(&slots[27..]);
        let (left, right) = if pos < other {
            (pos, other)
        } else {
            (other, pos)
        };
        storage
            .staged_update(left, first)?
            .staged_update(right, second)
    } else {
        storage.staged_update(pos, first)
    }
}

/// The client receives all potion-effect packets; health-bearing effects are
/// also enforced server-side rather than trusting client-reported HP.
#[derive(Clone, Copy)]
struct ActiveEffect {
    kind: u8,
    amplifier: u8,
    expires_at: u64,
    next_periodic: u64,
}
/// Legacy status effects tick faster at higher amplifiers. A Potion II must
/// never heal or poison at the level-I interval.
/// Non-cryptographic server-side roll for probabilistic food effects.
/// Clients never provide this number or choose the inflicted effects.
fn food_effect_roll(now_tick: u64, entity_id: i32) -> u8 {
    let mut x = now_tick ^ (entity_id as u32 as u64).wrapping_mul(0x9e3779b97f4a7c15);
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58476d1ce4e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d049bb133111eb);
    x ^= x >> 31;
    (x % 100) as u8
}

fn periodic_effect_interval(kind: u8, amplifier: u8) -> u64 {
    let base = match kind {
        10 => 50_u64, // regeneration
        19 => 25_u64, // poison
        _ => return u64::MAX,
    };
    (base.checked_shr(u32::from(amplifier)).unwrap_or(0)).max(1)
}

/// Protocol 5 Entity Action 3/4 starts and stops sprinting. Other actions
/// (sneaking, waking up, horse controls) never alter the sprint flag.
fn sprint_action(current: bool, action: i32, food: i32) -> bool {
    match action {
        3 => food > 6,
        4 => false,
        _ => current,
    }
}

fn potion_effect_packet(entity: i32, kind: u8, amplifier: u8, duration: u16) -> Vec<u8> {
    let mut packet = entity.to_be_bytes().to_vec();
    packet.push(kind);
    packet.push(amplifier);
    packet.extend_from_slice(&(duration as i16).to_be_bytes());
    packet
}
struct PlayerSession<'a> {
    position: &'a mut PlayerPosition,
    vitals: &'a mut Vitals,
    experience: &'a mut Experience,
    experience_path: &'a Path,
    achievements: &'a mut Achievements,
    achievements_path: &'a Path,
    save_path: &'a Path,
    inventory: &'a mut Inventory,
    inventory_path: &'a Path,
    ground_items: &'a Mutex<GroundItems>,
    ground_path: &'a Path,
    mobs: &'a Mutex<Mobs>,
    throwables: &'a Mutex<Throwables>,
    boats: &'a Mutex<Boats>,
    minecarts: &'a Mutex<Minecarts>,
    fishing: &'a Mutex<Fishing>,
    chests: &'a Mutex<Chests>,
    chest_path: &'a Path,
    furnaces: &'a Mutex<Furnaces>,
    furnace_path: &'a Path,
    hoppers: &'a Mutex<Hoppers>,
    hopper_path: &'a Path,
    devices: &'a Mutex<Devices>,
    device_path: &'a Path,
    brewers: &'a Mutex<Brewers>,
    brewer_path: &'a Path,
    survival: bool,
    hardcore: bool,
    difficulty: u8,
    game_rules: &'a Mutex<GameRules>,
    scoreboard: &'a Mutex<Scoreboard>,
    scoreboard_path: &'a Path,
    hub: &'a Hub,
    entity_id: i32,
    events: &'a Receiver<Event>,
    administration: &'a Administration,
    peer_ip: IpAddr,
    verified_uuid: Option<&'a str>,
}
/// Apply server-authoritative special item uses; return None for a normal
/// right-click, Some(changes) when this interaction was handled (including no-op).
fn use_special_item(
    w: &mut World,
    world_path: &Path,
    inventory: &mut Inventory,
    inventory_path: &Path,
    survival: bool,
    pos: (i32, i32, i32),
) -> io::Result<Option<Vec<(i32, i32, i32)>>> {
    let (x, y, z) = pos;
    let Some(held) = inventory.held() else {
        return Ok(None);
    };
    let Some((staged_world, changed, replacement, wears_tool)) =
        w.stage_special_item(x, y, z, held)?
    else {
        return Ok(None);
    };
    // Invalid uses and blocked tree generation never debit or damage an item.
    if changed.is_empty() {
        return Ok(Some(Vec::new()));
    }
    if survival {
        let mut staged_inventory = inventory.clone();
        let spent = if let Some(result) = replacement {
            staged_inventory.exchange_one_held(result)
        } else if wears_tool {
            staged_inventory.wear_flint_steel()
        } else {
            staged_inventory.consume_held()
        };
        if !spent {
            return Ok(Some(Vec::new()));
        }
        // Existing E45 redo journal atomically publishes both durable files.
        // Do not swap either live state until the journal completes.
        journal::commit(
            &world_path.with_extension("pending-txn.json"),
            &[
                (world_path, staged_world.encoded()?),
                (inventory_path, staged_inventory.encoded()?),
            ],
        )?;
        *inventory = staged_inventory;
    } else {
        staged_world.save(world_path)?;
    }
    *w = staged_world;
    Ok(Some(changed))
}
fn play(
    s: &mut Connection,
    world: &Mutex<World>,
    world_path: &Path,
    name: &str,
    whitelist_path: &Path,
    clock: &Clock,
    session: PlayerSession<'_>,
) -> io::Result<()> {
    let PlayerSession {
        position: player,
        vitals,
        experience,
        experience_path,
        achievements,
        achievements_path,
        save_path: player_path,
        inventory,
        inventory_path,
        ground_items,
        ground_path,
        mobs,
        throwables,
        boats,
        minecarts,
        fishing,
        chests,
        chest_path,
        furnaces,
        furnace_path,
        hoppers,
        hopper_path,
        devices,
        device_path,
        brewers,
        brewer_path,
        survival,
        hardcore,
        difficulty,
        game_rules,
        scoreboard,
        scoreboard_path,
        hub,
        entity_id,
        events,
        administration,
        peer_ip,
        verified_uuid,
    } = session;
    s.set_read_timeout(Some(Duration::from_millis(50)))?;
    let mut location = (player.x, player.y, player.z);
    let mut fall = FallTracker::new(player.y);
    let mut idle_gravity = IdleGravity::new(clock.ticks());
    let mut loaded_chunks = view_chunks(chunk_center(player));
    let mut pending_chunk_relights = BTreeSet::new();
    let mut last_chunk_relight = Instant::now() - Duration::from_millis(200);
    let mut yaw: f32 = 0.0;
    let mut pitch: f32 = 0.0;
    let mut last_movement_broadcast = Instant::now() - Duration::from_millis(50);
    let started = Instant::now();
    let mut ping_time = started;
    let mut pong_time = started;
    let mut ping_id: i32 = 0;
    let mut last_time_tick: u64 = clock.ticks();
    let mut last_vitals_tick = clock.ticks();
    let mut admission_check = started;
    let mut permissions = administration.permission(name)?;
    let mut chat_limiter = ChatLimiter::default();
    let mut saved_at = started;
    // Keep fragmented TCP frames across short receive timeouts.
    let mut packet_reader = BufferedFrameReader::new();
    let mut mining: Option<MiningProgress> = None;
    let mut active_window: u8 = 0;
    let mut active_chest: Option<ChestPosition> = None;
    let mut active_chest_partner: Option<ChestPosition> = None;
    let mut active_furnace: Option<FurnacePosition> = None;
    let mut active_hopper: Option<HopperPosition> = None;
    let mut active_device: Option<DevicePosition> = None;
    let mut last_device_view = [None; dispensers::DEVICE_SLOTS];
    let mut active_brewer: Option<BrewerPosition> = None;
    let mut last_brewer_view = Brewer::default();
    let mut last_furnace_view = Furnace::default();
    let mut observed_chest_revision: u64 = 0;
    let mut observed_hopper_revision: u64 = 0;
    let mut next_window: u8 = 1;
    let mut last_pickup = Instant::now();
    let mut eating: Option<(u64, Stack)> = None;
    let mut bow_start: Option<u64> = None;
    let mut sprinting = false;
    let mut hazards = Hazards::new();
    let mut last_hazards_tick = clock.ticks();
    let mut active_effects: Vec<ActiveEffect> = Vec::new();
    let mut last_effect_tick = clock.ticks();
    let mut death_recorded = vitals.health <= 0.0;
    let mut last_carrot_wear = 0u64;
    let mut last_beacon_tick = clock.ticks();
    let mut last_plate_refresh_tick = clock.ticks();
    loop {
        if !hub.is_healthy(entity_id) {
            return Err(io::Error::other(
                "outbound packet overflow or session removed",
            ));
        }
        while let Ok(event) = events.try_recv() {
            if event.id == ADMIN_KICK_EVENT {
                let reason = std::str::from_utf8(&event.body)
                    .map_err(|_| bad("invalid administrator kick event"))?;
                let mut payload = Vec::new();
                write_string(&json!({"text":reason}).to_string(), &mut payload)?;
                send(s, 0x40, &payload)?;
                return Ok(());
            }
            if matches!(event.id, ADMIN_TELEPORT_EVENT | PEARL_TELEPORT_EVENT) {
                if event.body.len() != 24 {
                    return Err(bad("invalid internal teleport payload"));
                }
                let target = PlayerPosition {
                    x: f64::from_be_bytes(event.body[0..8].try_into().unwrap()),
                    y: f64::from_be_bytes(event.body[8..16].try_into().unwrap()),
                    z: f64::from_be_bytes(event.body[16..24].try_into().unwrap()),
                };
                if !target.is_valid() {
                    return Err(bad("invalid internal teleport coordinates"));
                }
                // Distinguish command teleport from a paid ender-pearl impact:
                // the latter costs 5 health (2.5 hearts), exactly once, in
                // the owning player session before its new location is saved.
                let pearl_impact = event.id == PEARL_TELEPORT_EVENT;
                if pearl_impact && vitals.health <= 0.0 {
                    continue;
                }
                let mut next_vitals = *vitals;
                if pearl_impact {
                    next_vitals.apply_ender_pearl_damage();
                }
                target.save_state(player_path, next_vitals)?;
                *vitals = next_vitals;
                *player = target;
                location = (target.x, target.y, target.z);
                fall = FallTracker::new(target.y);
                idle_gravity = IdleGravity::new(clock.ticks());
                mining = None;
                eating = None;
                bow_start = None;
                yaw = 0.0;
                pitch = 0.0;
                sync_chunks(s, world, &mut loaded_chunks, chunk_center(player))?;
                send_player_position(s, player)?;
                if pearl_impact {
                    send(s, 0x06, &health_packet(*vitals))?;
                }
                hub.move_player(entity_id, *player, yaw, pitch);
                last_movement_broadcast = Instant::now();
                continue;
            }
            if event.id == CHUNK_RELIGHT_EVENT {
                if event.body.len() == 8 {
                    let cx = i32::from_be_bytes(event.body[0..4].try_into().unwrap());
                    let cz = i32::from_be_bytes(event.body[4..8].try_into().unwrap());
                    if loaded_chunks.contains(&(cx, cz)) {
                        pending_chunk_relights.insert((cx, cz));
                    }
                }
                continue;
            }
            send(s, event.id, &event.body)?;
        }
        // Full chunk resend re-synchronizes both sky and block light; limit
        // each peer to one per 200 ms and drop refreshes for unloaded chunks.
        if last_chunk_relight.elapsed() >= Duration::from_millis(200) {
            if let Some(chunk) = pending_chunk_relights.pop_first() {
                if loaded_chunks.contains(&chunk) {
                    send_chunk(s, world, chunk.0, chunk.1)?;
                }
                last_chunk_relight = Instant::now();
            }
        }
        // Poll the single shared chest revision to synchronize viewers.
        // Do not send a chest's window-id to unrelated players.
        if let Some(pos) = active_chest {
            let guard = chests.lock().map_err(|_| bad("chest lock poisoned"))?;
            if guard.revision() != observed_chest_revision {
                let data = inventory.chest_window_items(
                    active_window,
                    &chest_window_state(&guard, pos, active_chest_partner),
                );
                observed_chest_revision = guard.revision();
                drop(guard);
                send(s, 0x30, &data)?;
            }
        }
        if let Some(pos) = active_brewer {
            let guard = brewers.lock().map_err(|_| bad("brewer lock poisoned"))?;
            let current = guard.get(pos);
            if current != last_brewer_view {
                let changed_slots = current.slots != last_brewer_view.slots;
                last_brewer_view = current;
                drop(guard);
                if changed_slots {
                    send(
                        s,
                        0x30,
                        &inventory.chest_window_items(active_window, &current.slots),
                    )?;
                }
                send(s, 0x31, &current.property(active_window))?;
            }
        }
        if let Some(pos) = active_device {
            let guard = devices.lock().map_err(|_| bad("device lock poisoned"))?;
            let current = guard.slots(pos);
            if current != last_device_view {
                last_device_view = current;
                drop(guard);
                send(
                    s,
                    0x30,
                    &inventory.chest_window_items(active_window, &current),
                )?;
            }
        }
        if let Some(pos) = active_hopper {
            let guard = hoppers.lock().map_err(|_| bad("hopper lock poisoned"))?;
            if guard.revision() != observed_hopper_revision {
                let data = inventory.chest_window_items(active_window, &guard.slots(pos));
                observed_hopper_revision = guard.revision();
                drop(guard);
                send(s, 0x30, &data)?;
            }
        }
        if let Some(pos) = active_furnace {
            let guard = furnaces.lock().map_err(|_| bad("furnace lock poisoned"))?;
            let current = guard.get(pos);
            if current != last_furnace_view {
                let contents = (current.slots != last_furnace_view.slots)
                    .then(|| inventory.furnace_window_items(active_window, &current));
                let properties = current.properties(active_window);
                last_furnace_view = current;
                drop(guard);
                if let Some(contents) = contents {
                    send(s, 0x30, &contents)?;
                }
                for property in properties {
                    send(s, 0x31, &property)?;
                }
            }
        }
        if saved_at.elapsed() >= Duration::from_secs(5) {
            player.save_state(player_path, *vitals)?;
            inventory.save(inventory_path)?;
            saved_at = Instant::now();
        }
        if survival && vitals.health > 0.0 && last_pickup.elapsed() >= Duration::from_millis(200) {
            let mut ground = ground_items
                .lock()
                .map_err(|_| io::Error::other("ground items lock poisoned"))?;
            if let Some(item) = ground.nearby_ready(location) {
                let mut candidate = inventory.clone();
                if let Some(slots) = candidate.receive_stack(item.stack) {
                    if let Some((staged_ground, collected)) = ground.stage_take(item.id)? {
                        journal::commit(
                            &world_path.with_extension("pending-txn.json"),
                            &[
                                (ground_path, staged_ground.encoded()?),
                                (inventory_path, candidate.encoded()?),
                            ],
                        )?;
                        *ground = staged_ground;
                        *inventory = candidate;
                        for packet in collected.collected(entity_id) {
                            hub.broadcast(packet);
                        }
                        for slot in slots {
                            send(s, 0x2f, &inventory.set_slot(slot)?)?;
                        }
                        // A grant is sent only after the inventory and ground
                        // item transaction commits successfully.
                        if let Some(key) = Achievements::collected_item_trigger(item.stack.id) {
                            let mut next_achievements = achievements.clone();
                            if let Some(packet) = next_achievements.award(key)? {
                                next_achievements.save(achievements_path)?;
                                *achievements = next_achievements;
                                send(s, packet.id, &packet.body)?;
                            }
                        }
                    }
                }
            }
            last_pickup = Instant::now();
        }
        if admission_check.elapsed() >= Duration::from_secs(2) {
            // Reload on a bounded interval: file removal, corruption or revocation
            // causes active sessions to be disconnected (fail closed).
            let allowed = Whitelist::load(whitelist_path).is_ok_and(|list| list.allows(name))
                && administration
                    .banned_identity(name, verified_uuid, peer_ip)
                    .is_ok_and(|reason| reason.is_none());
            if !allowed {
                let mut reason = Vec::new();
                write_string(
                    &json!({"text":"Whitelist access revoked or unavailable"}).to_string(),
                    &mut reason,
                )?;
                send(s, 0x40, &reason)?;
                return Ok(());
            }
            permissions = administration.permission(name)?;
            admission_check = Instant::now();
        }
        let now_tick = clock.ticks();
        let boat_mount = boats
            .lock()
            .map_err(|_| bad("boats mutex poisoned"))?
            .rider_position(entity_id);
        let minecart_mount = minecarts
            .lock()
            .map_err(|_| bad("minecarts mutex poisoned"))?
            .rider_position(entity_id);
        let animal_mount = mobs
            .lock()
            .map_err(|_| bad("mob mutex poisoned"))?
            .rider_position(entity_id);
        if let Some(mounted) = minecart_mount.or(boat_mount).or(animal_mount) {
            if mounted.is_valid() && *player != mounted {
                *player = mounted;
                location = (mounted.x, mounted.y, mounted.z);
                fall.reset(mounted.y);
                idle_gravity.observe_client(now_tick);
                sync_chunks(s, world, &mut loaded_chunks, chunk_center(player))?;
                hub.move_player(entity_id, mounted, yaw, pitch);
            }
        }
        // XP orbs belong to the world and are collected by proximity, not by client inventory claims.
        if survival && vitals.health > 0.0 {
            let collected = mobs
                .lock()
                .map_err(|_| bad("mob mutex poisoned"))?
                .collect_xp(*player, entity_id);
            if let Some((amount, packets)) = collected {
                if experience.grant(amount) {
                    experience.save(experience_path)?;
                    send(s, 0x1f, &experience.packet())?;
                }
                for packet in packets {
                    hub.broadcast(packet);
                }
            }
        }
        // The first active session advances the shared mob simulation.
        if survival && vitals.health > 0.0 {
            let participants = hub.participants()?;
            let players = hub.positions()?;
            // Read the rule before taking world/mob locks to preserve lock order.
            let allow_natural_spawning = game_rules
                .lock()
                .map_err(|_| bad("game rules mutex poisoned"))?
                .mob_spawning();
            let mut w = world.lock().map_err(|_| bad("world mutex poisoned"))?;
            let mut creatures = mobs.lock().map_err(|_| bad("mob mutex poisoned"))?;
            creatures.set_natural_spawning(allow_natural_spawning);
            let mut updates = creatures.tick(now_tick, &w, &players);
            // Preserve main's grazing and Group I's leash simulation.
            let mut grazed_blocks = Vec::new();
            for (metadata, at) in creatures.graze(&mut w, world_path, now_tick)? {
                if let Some(packet) = metadata {
                    updates.push(packet);
                }
                grazed_blocks.push(at);
            }
            updates.extend(creatures.advance_leashes(&w, now_tick, &participants));
            // Publish both door halves only after the paired world write
            // commits; all clients observe the same server-owned destruction.
            let broken_doors =
                creatures.break_wooden_doors(&mut w, world_path, now_tick, &players)?;
            let explosions = creatures.take_explosions();
            updates.extend(creatures.queue_explosion_hits(&w, &explosions, &participants));
            updates.extend(creatures.take_player_effects(entity_id, *player, now_tick));
            let hurt =
                creatures.hurt_player_with_health(entity_id, *player, now_tick, vitals.health);
            let drops = creatures.take_drops();
            drop(creatures);
            for (x, y, z) in grazed_blocks.into_iter().chain(broken_doors) {
                hub.broadcast(block_change_event(&w, x, y, z));
            }
            let damage = f32::from(hurt) * (25.0 - f32::from(inventory.armor_points())) / 25.0;
            for explosion in explosions {
                hub.broadcast_in_view(explosion.position, explosion.packet());
                // One blast removes at most 64 non-tile blocks, protecting persisted containers.
                let mut destroyed = 0;
                let (cx, cy, cz) = (
                    explosion.position.x.floor() as i32,
                    explosion.position.y.floor() as i32,
                    explosion.position.z.floor() as i32,
                );
                'blast: for y in (cy - 3).max(1)..=(cy + 3).min(254) {
                    for x in cx - 3..=cx + 3 {
                        for z in cz - 3..=cz + 3 {
                            let d = ((x - cx).pow(2) + (y - cy).pow(2) + (z - cz).pow(2)) as f64;
                            if d > explosion.radius * explosion.radius {
                                continue;
                            }
                            let block = w.get(x, y, z) >> 4;
                            if matches!(block, 0 | 7 | 49 | 54 | 61 | 62) {
                                continue;
                            }
                            if w.update_and_save(world_path, x, y, z, 0)? {
                                hub.broadcast(block_change_event(&w, x, y, z));
                                destroyed += 1;
                                if destroyed >= 64 {
                                    break 'blast;
                                }
                            }
                        }
                    }
                }
            }
            drop(w);
            for update in updates {
                hub.broadcast(update);
            }
            if !drops.is_empty() {
                let mut ground = ground_items
                    .lock()
                    .map_err(|_| bad("ground items mutex poisoned"))?;
                for (stack, at) in drops {
                    if ground.is_full() {
                        break;
                    }
                    let item = ground.put(ground_path, stack, at)?;
                    for event in item.spawn() {
                        hub.broadcast(event);
                    }
                }
            }
            if damage > 0.0 {
                // Only armor-blockable Mob/explosion hits wear equipment. Do
                // not charge armor for hunger, falling, or poison. Persist
                // wear before advertising the hit to prevent an equipment
                // rollback from minting durability on reconnect.
                let mut worn = inventory.clone();
                let armor_changed = worn.wear_armor_on_hit(hurt);
                if armor_changed {
                    worn.save(inventory_path)?;
                    *inventory = worn;
                }
                vitals.health = (vitals.health - damage.ceil()).max(0.0);
                player.save_state(player_path, *vitals)?;
                if armor_changed {
                    send(s, 0x30, &inventory.window_items())?;
                }
                send(s, 0x06, &health_packet(*vitals))?;
            }
        }
        // A real 1.7.10 client predicts gravity while sending movement.
        // After six silent server ticks we integrate vertical gravity here.
        if survival && vitals.health > 0.0 {
            let gravity_world = world.lock().map_err(|_| bad("world mutex poisoned"))?;
            let simulated = idle_gravity.advance(&gravity_world, player, now_tick);
            let grounded = simulated
                .as_ref()
                .is_some_and(|candidate| physics::supported(&gravity_world, candidate));
            let wet = simulated.as_ref().is_some_and(|candidate| {
                physics::in_water(&gravity_world, candidate)
                    || physics::on_ladder(&gravity_world, candidate)
            });
            drop(gravity_world);
            if let Some(candidate) = simulated {
                let damage = if wet {
                    fall.reset(candidate.y);
                    0
                } else {
                    fall.update(candidate.y, grounded)
                };
                let landed_distance = fall.take_landing_distance();
                let mut next_vitals = *vitals;
                next_vitals.health = (next_vitals.health - f32::from(damage)).max(0.0);
                // Save accepted position and HP before sending an update.
                candidate.save_state(player_path, next_vitals)?;
                *player = candidate;
                *vitals = next_vitals;
                location = (candidate.x, candidate.y, candidate.z);
                if survival && landed_distance >= 1.0 {
                    let (fx, fy, fz) = (
                        candidate.x.floor() as i32,
                        candidate.y.floor() as i32 - 1,
                        candidate.z.floor() as i32,
                    );
                    let mut soil = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                    if soil.get(fx, fy, fz) >> 4 == 60
                        && soil.update_and_save(world_path, fx, fy, fz, 3 << 4)?
                    {
                        hub.broadcast(block_change_event(&soil, fx, fy, fz));
                    }
                }
                send_player_position(s, player)?;
                if damage != 0 {
                    send(s, 0x06, &health_packet(*vitals))?;
                }
                if last_movement_broadcast.elapsed() >= Duration::from_millis(50) {
                    hub.move_player(entity_id, *player, yaw, pitch);
                    last_movement_broadcast = Instant::now();
                }
            }
        }
        if survival && now_tick > last_vitals_tick {
            let steps = (now_tick - last_vitals_tick).min(20);
            last_vitals_tick = now_tick;
            let mut next = *vitals;
            if next.tick(steps) {
                player.save_state(player_path, next)?;
                *vitals = next;
                send(s, 0x06, &health_packet(*vitals))?;
            } else {
                *vitals = next;
            }
        }
        if survival && vitals.health > 0.0 && now_tick > last_hazards_tick {
            let mut total = 0.0f32;
            let w = world.lock().map_err(|_| bad("world mutex poisoned"))?;
            let steps = (now_tick - last_hazards_tick).min(4);
            for t in last_hazards_tick + 1..=last_hazards_tick + steps {
                total += hazards.tick(&w, *player, t, inventory);
            }
            last_hazards_tick = now_tick;
            drop(w);
            if total > 0.0 {
                vitals.health = (vitals.health - total).max(0.0);
                player.save_state(player_path, *vitals)?;
                send(s, 0x06, &health_packet(*vitals))?;
            }
        }
        // A stationary player continues pressing a plate. Refresh its timer
        // at most every five existing server ticks; do not resave an unchanged
        // analog value or create a second physics/tick scheduler.
        if vitals.health > 0.0 && now_tick.saturating_sub(last_plate_refresh_tick) >= 5 {
            last_plate_refresh_tick = now_tick;
            let (bx, by, bz) = (
                player.x.floor() as i32,
                player.y.floor() as i32 - 1,
                player.z.floor() as i32,
            );
            if world::in_bounds(bx, by, bz) {
                let plate_id = world
                    .lock()
                    .map_err(|_| bad("world mutex poisoned"))?
                    .get(bx, by, bz)
                    >> 4;
                if matches!(plate_id, 70 | 72 | 147 | 148) {
                    let count = 1 + hub
                        .participants()?
                        .into_iter()
                        .filter(|(other_id, at)| {
                            *other_id != entity_id
                                && at.x.floor() as i32 == bx
                                && at.y.floor() as i32 - 1 == by
                                && at.z.floor() as i32 == bz
                        })
                        .count();
                    let mut w = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                    let changed = if matches!(w.get(bx, by, bz) >> 4, 147 | 148) {
                        w.press_weighted_plate(world_path, bx, by, bz, count)?
                    } else {
                        w.press_plate(world_path, bx, by, bz)?
                    };
                    if changed {
                        hub.broadcast(block_change_event(&w, bx, by, bz));
                    }
                }
            }
        }
        // Active beacons sample their pyramid on the existing 20TPS clock.
        if survival && vitals.health > 0.0 && now_tick.saturating_sub(last_beacon_tick) >= 80 {
            last_beacon_tick = now_tick;
            let affected = world
                .lock()
                .map_err(|_| bad("world mutex poisoned"))?
                .beacon_in_range(player.x, player.y, player.z);
            if affected
                && !active_effects.iter().any(|effect| {
                    effect.kind == 1 && effect.amplifier > 0 && effect.expires_at > now_tick
                })
            {
                active_effects.retain(|effect| effect.kind != 1);
                active_effects.push(ActiveEffect {
                    kind: 1,
                    amplifier: 0,
                    expires_at: now_tick + 180,
                    next_periodic: now_tick + 180,
                });
                send(s, 0x1d, &potion_effect_packet(entity_id, 1, 0, 180))?;
            }
        }
        // A food right-click starts a 32 tick use animation. The server owns
        // completion, selection, consumption, and the resulting food values.
        if survival && vitals.health > 0.0 && now_tick > last_effect_tick {
            last_effect_tick = now_tick;
            let old_health = vitals.health;
            let old_food = vitals.food;
            let old_saturation = vitals.saturation;
            let old_exhaustion = vitals.exhaustion;
            for effect in &mut active_effects {
                if effect.kind == 17 && now_tick < effect.expires_at {
                    vitals.apply_hunger_effect_tick(effect.amplifier);
                }
                if now_tick >= effect.next_periodic && now_tick < effect.expires_at {
                    let amount = f32::from(effect.amplifier) + 1.0;
                    if effect.kind == 10 {
                        vitals.health = (vitals.health + amount).min(20.0);
                    }
                    if effect.kind == 19 {
                        vitals.health = (vitals.health - amount).max(1.0);
                    }
                    effect.next_periodic = now_tick
                        .saturating_add(periodic_effect_interval(effect.kind, effect.amplifier));
                }
            }
            for effect in &active_effects {
                if now_tick >= effect.expires_at {
                    let mut removed = entity_id.to_be_bytes().to_vec();
                    removed.push(effect.kind);
                    send(s, 0x1e, &removed)?;
                }
            }
            active_effects.retain(|effect| now_tick < effect.expires_at);
            let visible_changed = vitals.health != old_health
                || vitals.food != old_food
                || vitals.saturation != old_saturation;
            // Persist hidden exhaustion at most once per second to avoid
            // 20 sync writes/sec/player while a Hunger effect is active.
            if visible_changed
                || (vitals.exhaustion != old_exhaustion && now_tick.is_multiple_of(20))
            {
                player.save_state(player_path, *vitals)?;
                if visible_changed {
                    send(s, 0x06, &health_packet(*vitals))?;
                }
            }
        }
        // Food and drink consumption both finish after 32 server ticks.
        if survival && vitals.health > 0.0 {
            if let Some((began, stack)) = eating {
                if now_tick.saturating_sub(began) >= 32 {
                    eating = None;
                    if inventory.held() == Some(stack) && stack.id == 373 {
                        if let Some((kind, amplifier, duration)) = brewing::effect(stack) {
                            let mut remaining = inventory.clone();
                            if remaining.consume_held()
                                && remaining
                                    .receive_stack(Stack {
                                        id: 374,
                                        count: 1,
                                        damage: 0,
                                    })
                                    .is_some()
                            {
                                // Inventory debit is durable before temporary status or HP gain.
                                remaining.save(inventory_path)?;
                                *inventory = remaining;
                                if kind == 6 {
                                    vitals.health =
                                        (vitals.health + 4.0 * f32::from(amplifier + 1)).min(20.0);
                                } else if kind == 7 {
                                    vitals.health =
                                        (vitals.health - 6.0 * f32::from(amplifier + 1)).max(0.0);
                                } else {
                                    active_effects.retain(|effect| effect.kind != kind);
                                    active_effects.push(ActiveEffect {
                                        kind,
                                        amplifier,
                                        expires_at: now_tick + u64::from(duration),
                                        next_periodic: now_tick.saturating_add(
                                            periodic_effect_interval(kind, amplifier),
                                        ),
                                    });
                                }
                                player.save_state(player_path, *vitals)?;
                                send(s, 0x30, &inventory.window_items())?;
                                send(s, 0x06, &health_packet(*vitals))?;
                                send(
                                    s,
                                    0x1d,
                                    &potion_effect_packet(entity_id, kind, amplifier, duration),
                                )?;
                            }
                        }
                    } else if inventory.held() == Some(stack) && stack.id == 335 {
                        // In 1.7.10 drinking milk clears all effects even when
                        // the player is at full hunger. Charge the milk and
                        // return one empty bucket before removing any effects.
                        let mut remaining = inventory.clone();
                        if remaining.consume_held()
                            && remaining
                                .receive_stack(Stack {
                                    id: 325,
                                    count: 1,
                                    damage: 0,
                                })
                                .is_some()
                        {
                            remaining.save(inventory_path)?;
                            *inventory = remaining;
                            for effect in active_effects.drain(..) {
                                let mut removed = entity_id.to_be_bytes().to_vec();
                                removed.push(effect.kind);
                                send(s, 0x1e, &removed)?;
                            }
                            send(s, 0x30, &inventory.window_items())?;
                            send(s, 0x2f, &inventory.cursor_packet())?;
                        }
                    } else if inventory.held() == Some(stack) && vitals.can_eat(stack) {
                        let mut remaining = inventory.clone();
                        if remaining.consume_held() {
                            let mut after = *vitals;
                            if after.eat(stack) {
                                // Mushroom stew is a nonstackable bowl meal in
                                // Java 1.7.10. Reserve the returned bowl in the
                                // same staged inventory BEFORE persisting the
                                // debit, otherwise a full bag could erase it.
                                let container_returned = stack.id != 282
                                    || remaining
                                        .receive_stack(Stack {
                                            id: 281,
                                            count: 1,
                                            damage: 0,
                                        })
                                        .is_some();
                                if container_returned {
                                    // Debit inventory before player-NBT food credit.
                                    remaining.save(inventory_path)?;
                                    player.save_state(player_path, after)?;
                                    *inventory = remaining;
                                    *vitals = after;
                                    for effect in vitals::food_side_effects(
                                        stack,
                                        food_effect_roll(now_tick, entity_id),
                                    ) {
                                        let expires_at = now_tick
                                            .saturating_add(u64::from(effect.duration_ticks));
                                        // Weaker/shorter food effects cannot erase an
                                        // existing stronger or longer potion effect.
                                        let replace = active_effects
                                            .iter()
                                            .find(|old| old.kind == effect.kind)
                                            .is_none_or(|old| {
                                                effect.amplifier > old.amplifier
                                                    || (effect.amplifier == old.amplifier
                                                        && expires_at > old.expires_at)
                                            });
                                        if replace {
                                            active_effects.retain(|old| old.kind != effect.kind);
                                            active_effects.push(ActiveEffect {
                                                kind: effect.kind,
                                                amplifier: effect.amplifier,
                                                expires_at,
                                                next_periodic: now_tick.saturating_add(
                                                    periodic_effect_interval(
                                                        effect.kind,
                                                        effect.amplifier,
                                                    ),
                                                ),
                                            });
                                            send(
                                                s,
                                                0x1d,
                                                &potion_effect_packet(
                                                    entity_id,
                                                    effect.kind,
                                                    effect.amplifier,
                                                    effect.duration_ticks,
                                                ),
                                            )?;
                                        }
                                    }
                                    if stack.id == 282 {
                                        send(s, 0x30, &inventory.window_items())?;
                                    } else {
                                        send(s, 0x2f, &inventory.set_slot(inventory.held_slot())?)?;
                                    }
                                    send(s, 0x06, &health_packet(*vitals))?;
                                }
                            }
                        }
                    }
                }
            }
        }
        if survival && vitals.health <= 0.0 {
            eating = None;
            if !death_recorded {
                // A death is counted exactly once in this session and the
                // updated shared scoreboard is durable before client notify.
                let changes = {
                    let mut board = scoreboard
                        .lock()
                        .map_err(|_| bad("scoreboard lock poisoned"))?;
                    let mut candidate = board.clone();
                    let changes = candidate.record_death(name)?;
                    candidate.save(scoreboard_path)?;
                    *board = candidate;
                    changes
                };
                for change in changes {
                    hub.broadcast(change);
                }
                death_recorded = true;
            }
            let keep_inventory = game_rules
                .lock()
                .map_err(|_| bad("game rules mutex poisoned"))?
                .keep_inventory();
            if !keep_inventory {
                commit_death_drops(
                    inventory,
                    inventory_path,
                    ground_items,
                    ground_path,
                    world_path,
                    hub,
                    location,
                )?;
            }
            if hardcore {
                // Persist the dead state before ending the connection. Future
                // logins reject this player even after server restart.
                player.save_state(player_path, *vitals)?;
                return Ok(());
            }
        }
        if now_tick.saturating_sub(last_time_tick) >= 20 {
            let mut time = Vec::with_capacity(16);
            time.extend_from_slice(&(now_tick as i64).to_be_bytes());
            time.extend_from_slice(&clock.day_time().to_be_bytes());
            send(s, 0x03, &time)?;
            last_time_tick = now_tick;
        }
        if ping_time.elapsed() >= Duration::from_secs(10) {
            ping_id = ping_id.wrapping_add(1);
            send(s, 0, &ping_id.to_be_bytes())?;
            ping_time = Instant::now();
        }
        if pong_time.elapsed() > Duration::from_secs(40) {
            return Err(bad("keepalive timeout"));
        }
        let data = match packet_reader.poll(s) {
            Ok(Some(packet)) => packet,
            Ok(None) => continue,
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };
        let mut input = Input(&data);
        let id = input.varint()?;
        // Dead players cannot interact with or move through the world until
        // their Protocol 5 Client Status respawn request is accepted.
        if survival && vitals.health <= 0.0 && matches!(id, 4 | 6 | 7 | 8) {
            continue;
        }
        match id {
            2 => {
                if !permissions.interact {
                    continue;
                }
                // Protocol 5 C02 Use Entity: target VarInt and action VarInt.
                let target_id = input.varint()?;
                let action = input.varint()?;
                input.finish()?;
                if survival && vitals.health > 0.0 && action == 0 {
                    if minecarts
                        .lock()
                        .map_err(|_| bad("minecarts mutex poisoned"))?
                        .cart_for(entity_id)
                        .is_some()
                    {
                        continue;
                    }
                    let already_riding = boats
                        .lock()
                        .map_err(|_| bad("boats mutex poisoned"))?
                        .boat_for(entity_id)
                        .is_some()
                        || mobs
                            .lock()
                            .map_err(|_| bad("mob mutex poisoned"))?
                            .is_riding(entity_id);
                    if !already_riding {
                        if let Some(attach) = minecarts
                            .lock()
                            .map_err(|_| bad("minecarts mutex poisoned"))?
                            .mount(target_id, entity_id, *player)
                        {
                            hub.broadcast(attach);
                            continue;
                        }
                    }
                    if let Some(attach) = boats
                        .lock()
                        .map_err(|_| bad("boats mutex poisoned"))?
                        .mount(target_id, entity_id, *player)
                    {
                        hub.broadcast(attach);
                        continue;
                    }
                    let mut creatures = mobs.lock().map_err(|_| bad("mob mutex poisoned"))?;
                    // Leashes and saddles must debit a real held item before
                    // changing the shared entity. Never trust the client slot.
                    if inventory.held().is_some_and(|item| item.id == 420)
                        && creatures.can_leash(target_id, entity_id, *player)
                    {
                        let mut candidate = inventory.clone();
                        if candidate.consume_held() {
                            candidate.save(inventory_path)?;
                            if let Some(event) = creatures.leash(target_id, entity_id, *player) {
                                *inventory = candidate;
                                send(s, 0x2f, &inventory.set_slot(inventory.held_slot())?)?;
                                hub.broadcast(event);
                            }
                        }
                        continue;
                    }
                    if inventory.held().is_some_and(|item| item.id == 329)
                        && creatures.can_saddle(target_id, *player)
                    {
                        let mut candidate = inventory.clone();
                        if candidate.consume_held() {
                            candidate.save(inventory_path)?;
                            if let Some(event) = creatures.saddle(target_id, *player) {
                                *inventory = candidate;
                                send(s, 0x2f, &inventory.set_slot(inventory.held_slot())?)?;
                                hub.broadcast(event);
                            }
                        }
                        continue;
                    }
                    if boats
                        .lock()
                        .map_err(|_| bad("boats mutex poisoned"))?
                        .boat_for(entity_id)
                        .is_none()
                    {
                        if let Some(events) = creatures.use_mount(target_id, entity_id, *player) {
                            for event in events {
                                hub.broadcast(event);
                            }
                            continue;
                        }
                    }
                    // Protocol 5 Use Entity: adult cow + held empty bucket.
                    // Inventory is cloned, exchanged and persisted before the
                    // authoritative window refresh. On a full inventory, the
                    // action has no effect and cannot consume a bucket.
                    if creatures.can_milk(target_id, *player, inventory.held()) {
                        let mut candidate = inventory.clone();
                        if candidate.fill_milk_bucket() {
                            candidate.save(inventory_path)?;
                            *inventory = candidate;
                            send(s, 0x30, &inventory.window_items())?;
                        }
                        continue;
                    }
                    // Shears, a sheep's shared metadata, inventory durability and
                    // spawned wool are all decided by the server.
                    if inventory.held().is_some_and(|item| item.id == 359) {
                        if let Some((wool, event)) =
                            creatures.shear(target_id, *player, inventory.held())
                        {
                            let mut next_inventory = inventory.clone();
                            if next_inventory.wear_held_tool() {
                                next_inventory.save(inventory_path)?;
                                *inventory = next_inventory;
                                send(s, 0x2f, &inventory.set_slot(inventory.held_slot())?)?;
                                let mut ground = ground_items
                                    .lock()
                                    .map_err(|_| bad("ground items mutex poisoned"))?;
                                if !ground.is_full() {
                                    let at = (
                                        player.x.floor() as i32,
                                        player.y.floor() as i32,
                                        player.z.floor() as i32,
                                    );
                                    let dropped = ground.put(ground_path, wool, at)?;
                                    for packet in dropped.spawn() {
                                        hub.broadcast(packet);
                                    }
                                }
                                hub.broadcast(event);
                            }
                            continue;
                        }
                    }
                    // Server-controlled villager barter, never trust a client
                    // supplied output stack. Clone first to ensure we can fit
                    // the reward without deleting the emerald on a full bag.
                    if let Some(output) =
                        creatures.village_offer(target_id, *player, inventory.held())
                    {
                        let mut candidate = inventory.clone();
                        if candidate.consume_held() && candidate.receive_stack(output).is_some() {
                            candidate.save(inventory_path)?;
                            *inventory = candidate;
                            send(s, 0x30, &inventory.window_items())?;
                            continue;
                        }
                    }
                    if let Some(event) =
                        creatures.feed(target_id, *player, inventory.held(), now_tick)
                    {
                        let mut candidate = inventory.clone();
                        if candidate.consume_held() {
                            candidate.save(inventory_path)?;
                            *inventory = candidate;
                            send(s, 0x2f, &inventory.set_slot(inventory.held_slot())?)?;
                            hub.broadcast(event);
                        }
                    }
                }
                if survival && vitals.health > 0.0 && action == 1 {
                    if let Some(events) = boats
                        .lock()
                        .map_err(|_| bad("boats mutex poisoned"))?
                        .break_boat(target_id, *player)
                    {
                        for event in events {
                            hub.broadcast(event);
                        }
                        continue;
                    }
                    let mut creatures = mobs.lock().map_err(|_| bad("mob mutex poisoned"))?;
                    let result =
                        creatures.player_attack(target_id, *player, inventory.held(), now_tick);
                    drop(creatures);
                    if let Some(hit) = result {
                        let mut next_inventory = inventory.clone();
                        if next_inventory.wear_held_tool() {
                            next_inventory.save(inventory_path)?;
                            *inventory = next_inventory;
                            send(s, 0x2f, &inventory.set_slot(inventory.held_slot())?)?;
                        }
                        if hit.experience > 0 {
                            let at = hit
                                .drop
                                .map(|(_, at)| PlayerPosition {
                                    x: at.0 as f64 + 0.5,
                                    y: at.1 as f64,
                                    z: at.2 as f64 + 0.5,
                                })
                                .unwrap_or(*player);
                            if let Some(orb) = mobs
                                .lock()
                                .map_err(|_| bad("mob mutex poisoned"))?
                                .spawn_xp(at, hit.experience as u16)
                            {
                                hub.broadcast(orb);
                            }
                        }
                        if let Some((stack, at)) = hit.drop {
                            let mut ground = ground_items
                                .lock()
                                .map_err(|_| bad("ground items mutex poisoned"))?;
                            if !ground.is_full() {
                                let dropped = ground.put(ground_path, stack, at)?;
                                for packet in dropped.spawn() {
                                    hub.broadcast(packet);
                                }
                            }
                        }
                        for event in hit.events {
                            hub.broadcast(event);
                        }
                    }
                }
            }
            0 => {
                let reply = input.i32()?;
                input.finish()?;
                if reply != ping_id {
                    return Err(bad("unexpected keepalive ID"));
                }
                pong_time = Instant::now();
            }
            1 => {
                let message = input.string(100)?;
                input.finish()?;
                if let Err(disconnect) = chat_limiter.check(&message) {
                    if disconnect {
                        let mut reason = Vec::new();
                        write_string(
                            &json!({"text":"Repeated chat spam"}).to_string(),
                            &mut reason,
                        )?;
                        send(s, 0x40, &reason)?;
                        return Ok(());
                    }
                    as_chat(s, "Chat message rejected by spam filter.")?;
                    continue;
                }
                if message == "/help" {
                    as_chat(
                        s,
                        "Commands: /help, /list. Verified OPs may use /save, /kick, /ban and other level-gated commands.",
                    )?;
                } else if message == "/list" {
                    as_chat(
                        s,
                        &format!("Online ({}): {}", hub.count()?, hub.names()?.join(", ")),
                    )?;
                } else if message.starts_with('/') {
                    // Offline names never supply a verified UUID. Refuse all
                    // administrative mutations before attempting an audit log.
                    if !administration.is_operator(name, verified_uuid)? {
                        as_chat(s, "Permission denied: verified operator required.")?;
                        continue;
                    }
                    if administration.audit(name, &message, "attempt").is_err() {
                        as_chat(s, "Admin audit unavailable; operation refused.")?;
                        continue;
                    }
                    let services = PlayerAdminServices {
                        hub,
                        world,
                        world_path,
                        clock,
                        administration,
                    };
                    let result = run_player_admin_command(
                        &message,
                        administration.operator_level(name, verified_uuid)?,
                        &services,
                        name,
                    );
                    let outcome = if result.is_ok() { "success" } else { "failure" };
                    if administration.audit(name, &message, outcome).is_err() {
                        eprintln!(
                            "CRITICAL: player administrative action could not be audited: {name}"
                        );
                        as_chat(s, "Administrative audit write failed.")?;
                        continue;
                    }
                    match result {
                        Ok(outcome) => as_chat(s, &outcome)?,
                        Err(error) => {
                            as_chat(s, &format!("Administrative command rejected: {error}"))?
                        }
                    }
                } else if !permissions.chat {
                    as_chat(s, "Chat permission denied.")?;
                } else {
                    let mut bytes = Vec::new();
                    write_string(
                        &json!({"text":format!("<{name}> {message}")}).to_string(),
                        &mut bytes,
                    )?;
                    hub.broadcast(Event {
                        id: 0x02,
                        body: bytes,
                    });
                }
            }
            3 => {
                input.u8()?;
                input.finish()?;
                idle_gravity.observe_client(now_tick);
            } // flying/onGround
            4 | 6 => {
                let x = input.f64()?;
                let _stance = input.f64()?;
                let y = input.f64()?;
                let z = input.f64()?;
                if id == 6 {
                    yaw = input.f32()?;
                    pitch = input.f32()?;
                    if !yaw.is_finite() || !pitch.is_finite() {
                        return Err(bad("invalid player look angles"));
                    }
                }
                input.u8()?;
                input.finish()?;
                if !x.is_finite() || !y.is_finite() || !z.is_finite() || !(0.0..256.0).contains(&y)
                {
                    return Err(bad("invalid player coordinates"));
                }
                let candidate = PlayerPosition { x, y, z };
                if !candidate.is_valid() {
                    // Sparse world positions are validated against the global world limit.
                    send_player_position(s, player)?;
                    continue;
                }
                if boats
                    .lock()
                    .map_err(|_| bad("boats mutex poisoned"))?
                    .boat_for(entity_id)
                    .is_some()
                    || minecarts
                        .lock()
                        .map_err(|_| bad("minecarts mutex poisoned"))?
                        .cart_for(entity_id)
                        .is_some()
                    || mobs
                        .lock()
                        .map_err(|_| bad("mob mutex poisoned"))?
                        .is_riding(entity_id)
                {
                    // Riding position comes from server-side boat physics, never C04/C06.
                    continue;
                }
                let mut landing_distance = 0.0;
                if survival {
                    let collision_world = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                    if !physics::valid_movement(&collision_world, player, &candidate) {
                        send_player_position(s, player)?;
                        continue;
                    }
                    let landed = physics::supported(&collision_world, &candidate);
                    let safe_fall = physics::in_water(&collision_world, &candidate)
                        || physics::on_ladder(&collision_world, &candidate);
                    drop(collision_world);
                    let damage = if safe_fall {
                        fall.reset(y);
                        0
                    } else {
                        fall.update(y, landed)
                    };
                    landing_distance = fall.take_landing_distance();
                    if damage != 0 {
                        vitals.health = (vitals.health - f32::from(damage)).max(0.0);
                        player.save_state(player_path, *vitals)?;
                        send(s, 0x06, &health_packet(*vitals))?;
                    }
                }
                if survival {
                    let old_food = vitals.food;
                    let old_saturation = vitals.saturation;
                    let distance = (x - player.x).hypot(z - player.z);
                    if sprinting && vitals.food > 6 {
                        vitals.travel_with_sprint(distance, true);
                    } else {
                        vitals.travel(distance);
                    }
                    if vitals.food != old_food || vitals.saturation != old_saturation {
                        player.save_state(player_path, *vitals)?;
                        send(s, 0x06, &health_packet(*vitals))?;
                    }
                }
                idle_gravity.observe_client(now_tick);
                if survival {
                    let fx = x.floor() as i32;
                    let fz = z.floor() as i32;
                    let fy = y.floor() as i32 - 1;
                    // A fall of at least one block onto farmland tramples it.
                    if fy >= 0 && landing_distance >= 1.0 {
                        let mut soil = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                        if soil.get(fx, fy, fz) >> 4 == 60
                            && soil.update_and_save(world_path, fx, fy, fz, 3 << 4)?
                        {
                            hub.broadcast(block_change_event(&soil, fx, fy, fz));
                        }
                    }
                }
                // A player standing on a pressure plate powers it. Releasing
                // is managed by the bounded 20TPS device timer.
                {
                    let bx = x.floor() as i32;
                    let by = y.floor() as i32 - 1;
                    let bz = z.floor() as i32;
                    // Snapshot peers outside the world lock to preserve ordering.
                    let count = 1 + hub
                        .participants()?
                        .into_iter()
                        .filter(|(other_id, at)| {
                            *other_id != entity_id
                                && at.x.floor() as i32 == bx
                                && at.y.floor() as i32 - 1 == by
                                && at.z.floor() as i32 == bz
                        })
                        .count();
                    let mut w = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                    if world::in_bounds(bx, by, bz) {
                        let activated = if matches!(w.get(bx, by, bz) >> 4, 147 | 148) {
                            w.press_weighted_plate(world_path, bx, by, bz, count)?
                        } else {
                            w.press_plate(world_path, bx, by, bz)?
                        };
                        if activated {
                            hub.broadcast(block_change_event(&w, bx, by, bz));
                        }
                    }
                    // Tripwire triggers are evaluated at feet and body height,
                    // never from untrusted client onGround flags.
                    for ty in [by + 1, by + 2] {
                        if world::in_bounds(bx, ty, bz) {
                            for (tx, hy, tz) in w.touch_tripwire(world_path, bx, ty, bz)? {
                                hub.broadcast(block_change_event(&w, tx, hy, tz));
                            }
                        }
                    }
                }
                location = (x, y, z);
                *player = candidate;
                sync_chunks(s, world, &mut loaded_chunks, chunk_center(&candidate))?;
                if last_movement_broadcast.elapsed() >= Duration::from_millis(50) {
                    hub.move_player(entity_id, candidate, yaw, pitch);
                    last_movement_broadcast = Instant::now();
                }
            }
            5 => {
                yaw = input.f32()?;
                pitch = input.f32()?;
                input.u8()?;
                input.finish()?;
                if !yaw.is_finite() || !pitch.is_finite() {
                    return Err(bad("invalid player look angles"));
                }
                idle_gravity.observe_client(now_tick);
                if last_movement_broadcast.elapsed() >= Duration::from_millis(50) {
                    hub.move_player(entity_id, *player, yaw, pitch);
                    last_movement_broadcast = Instant::now();
                }
            }
            12 => {
                // Protocol 5 Steer Vehicle: float sideways, float forward,
                // bool jump, bool dismount. No client-supplied vehicle ID.
                let sideways = input.f32()?;
                let forward = input.f32()?;
                let _jump = input.u8()? != 0;
                let dismount = input.u8()? != 0;
                input.finish()?;
                if !sideways.is_finite() || !forward.is_finite() {
                    return Err(bad("non-finite vehicle controls"));
                }
                let mut state = boats.lock().map_err(|_| bad("boats mutex poisoned"))?;
                if dismount {
                    if let Some(event) = state.dismount(entity_id) {
                        hub.broadcast(event);
                    }
                    drop(state);
                    if let Some(event) = minecarts
                        .lock()
                        .map_err(|_| bad("minecarts mutex poisoned"))?
                        .dismount(entity_id)
                    {
                        hub.broadcast(event);
                    }
                    if let Some(event) = mobs
                        .lock()
                        .map_err(|_| bad("mob mutex poisoned"))?
                        .dismount(entity_id)
                    {
                        hub.broadcast(event);
                    }
                } else if !state.control(entity_id, sideways, forward, yaw) {
                    drop(state);
                    if minecarts
                        .lock()
                        .map_err(|_| bad("minecarts mutex poisoned"))?
                        .control(entity_id, forward, yaw)
                    {
                        continue;
                    }
                    let carrot = inventory.held().is_some_and(|item| item.id == 398);
                    let collision = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                    let events = mobs
                        .lock()
                        .map_err(|_| bad("mob mutex poisoned"))?
                        .steer_rider(entity_id, &collision, forward, yaw, carrot, now_tick);
                    drop(collision);
                    let moved = !events.is_empty();
                    for event in events {
                        hub.broadcast(event);
                    }
                    if moved && carrot && now_tick.saturating_sub(last_carrot_wear) >= 40 {
                        let mut candidate = inventory.clone();
                        if candidate.wear_carrot_stick() {
                            candidate.save(inventory_path)?;
                            *inventory = candidate;
                            send(s, 0x2f, &inventory.set_slot(inventory.held_slot())?)?;
                            last_carrot_wear = now_tick;
                        }
                    }
                }
            }
            11 => {
                // Java 1.7.10 (Protocol 5) serverbound Entity Action:
                // VarInt entity ID, VarInt action, VarInt horse jump boost.
                let _reported_entity_id = input.varint()?;
                let action = input.varint()?;
                let _jump_boost = input.varint()?;
                input.finish()?;
                if survival {
                    sprinting = sprint_action(sprinting, action, vitals.food);
                }
            }
            7 => {
                if !permissions.build {
                    continue;
                }
                let action = input.i8()?;
                let x = input.i32()?;
                let y = input.u8()? as i32;
                let z = input.i32()?;
                input.i8()?; // face
                input.finish()?;
                if survival {
                    if action == 5 {
                        eating = None; // Release Use Item.
                        if let Some(start) = bow_start.take() {
                            let duration = now_tick.saturating_sub(start) as f64 / 20.0;
                            let charge = ((duration * duration + duration * 2.0) / 3.0).min(1.0);
                            if charge >= 0.1 && inventory.held().is_some_and(|item| item.id == 261)
                            {
                                let mut candidate = inventory.clone();
                                if candidate.consume_item(262) {
                                    let mut creatures =
                                        mobs.lock().map_err(|_| bad("mob mutex poisoned"))?;
                                    if let Some(arrow) = creatures.fire_arrow(
                                        Some(entity_id),
                                        *player,
                                        yaw,
                                        pitch,
                                        charge * 3.0,
                                        (charge * 2.0).ceil() as u8 + 1,
                                    ) {
                                        candidate.save(inventory_path)?;
                                        *inventory = candidate;
                                        send(s, 0x30, &inventory.window_items())?;
                                        hub.broadcast_in_view(*player, arrow);
                                    }
                                }
                            }
                        }
                    }
                    let target = (x, y, z);
                    match action {
                        3 | 4 => {
                            // Protocol 5: 3 = full-stack drop; 4 = one item.
                            // These packets do not contain a real mining target.
                            let mut candidate = inventory.clone();
                            if let Some(stack) = candidate.drop_held(action == 3) {
                                let mut ground = ground_items
                                    .lock()
                                    .map_err(|_| io::Error::other("ground items lock poisoned"))?;
                                if !ground.is_full() {
                                    let target = (
                                        location.0.floor() as i32,
                                        (location.1.floor() as i32).clamp(0, 254),
                                        location.2.floor() as i32,
                                    );
                                    // Inventory debit before entity persistence prevents
                                    // duplication; failure between saves may lose items.
                                    candidate.save(inventory_path)?;
                                    *inventory = candidate;
                                    let item = ground.put(ground_path, stack, target)?;
                                    send(s, 0x2f, &inventory.set_slot(inventory.held_slot())?)?;
                                    for packet in item.spawn() {
                                        hub.broadcast(packet);
                                    }
                                } else {
                                    send(s, 0x2f, &inventory.set_slot(inventory.held_slot())?)?;
                                }
                            }
                        }
                        0 => {
                            let w = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                            mining = if world::in_bounds(x, y, z) && near(location, x, y, z) {
                                MiningProgress::start(target, w.get(x, y, z), inventory.held())
                            } else {
                                None
                            };
                        }
                        1 => mining = None,
                        2 => {
                            let attempt = mining.take();
                            let mut w = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                            let result = attempt
                                .and_then(|m| m.finish(target, w.get(x, y, z), inventory.held()));
                            let mut changed = false;
                            if world::in_bounds(x, y, z) && near(location, x, y, z) {
                                if let Some(result) = result {
                                    let block = w.get(x, y, z) >> 4;
                                    // Safely refuse tile-entity mining until
                                    // their contents can become proper drops.
                                    let occupied = (block == 54
                                        && chests
                                            .lock()
                                            .map_err(|_| bad("chest lock poisoned"))?
                                            .is_nonempty(target))
                                        || ((block == 61 || block == 62)
                                            && furnaces
                                                .lock()
                                                .map_err(|_| bad("furnace lock poisoned"))?
                                                .get(target)
                                                .occupied())
                                        || (matches!(block, 23 | 158)
                                            && devices
                                                .lock()
                                                .map_err(|_| bad("device lock poisoned"))?
                                                .slots(target)
                                                .iter()
                                                .any(Option::is_some))
                                        || (block == 154
                                            && hoppers
                                                .lock()
                                                .map_err(|_| bad("hopper lock poisoned"))?
                                                .has_contents(target))
                                        || (block == 117
                                            && brewers
                                                .lock()
                                                .map_err(|_| bad("brewer lock poisoned"))?
                                                .is_nonempty(target));
                                    if !occupied {
                                        let bonus = survival::additional_drop(
                                            w.get(x, y, z),
                                            target,
                                            now_tick,
                                        );
                                        let mut ground = ground_items.lock().map_err(|_| {
                                            io::Error::other("ground items lock poisoned")
                                        })?;
                                        // A filled entity pool must not destroy a
                                        // block that should have created an item.
                                        if (result.drop.is_none() && bonus.is_none())
                                            || !ground.is_full()
                                        {
                                            let mut candidate = inventory.clone();
                                            let wore =
                                                result.wear_tool && candidate.wear_held_tool();
                                            // Commit durability consumption before
                                            // changing the world, not vice-versa.
                                            if wore {
                                                candidate.save(inventory_path)?;
                                            }
                                            match w.update_and_save(world_path, x, y, z, 0) {
                                                Ok(true) => {
                                                    if wore {
                                                        *inventory = candidate;
                                                        send(
                                                            s,
                                                            0x2f,
                                                            &inventory
                                                                .set_slot(inventory.held_slot())?,
                                                        )?;
                                                    }
                                                    if let Some(drop) = result.drop {
                                                        let stack = Stack {
                                                            id: drop.id,
                                                            count: 1,
                                                            damage: drop.damage,
                                                        };
                                                        // A crash after world save and
                                                        // before entity persistence can
                                                        // lose an item, never clone it.
                                                        let item = ground.put(
                                                            ground_path,
                                                            stack,
                                                            target,
                                                        )?;
                                                        for packet in item.spawn() {
                                                            hub.broadcast(packet);
                                                        }
                                                    }
                                                    if let Some(stack) = bonus {
                                                        if !ground.is_full() {
                                                            let item = ground.put(
                                                                ground_path,
                                                                stack,
                                                                target,
                                                            )?;
                                                            for packet in item.spawn() {
                                                                hub.broadcast(packet);
                                                            }
                                                        }
                                                    }
                                                    changed = true;
                                                    let reward = match block {
                                                        16 => 2,
                                                        21 => 4,
                                                        56 => 5,
                                                        73 | 74 => 3,
                                                        129 => 5,
                                                        153 => 3,
                                                        _ => 0,
                                                    };
                                                    if reward > 0 {
                                                        if let Some(orb) = mobs
                                                            .lock()
                                                            .map_err(|_| bad("mob mutex poisoned"))?
                                                            .spawn_xp(*player, reward)
                                                        {
                                                            hub.broadcast(orb);
                                                        }
                                                    }
                                                }
                                                Ok(false) => {
                                                    if wore {
                                                        inventory.save(inventory_path)?;
                                                    }
                                                }
                                                Err(error) => {
                                                    if wore {
                                                        inventory.save(inventory_path)?;
                                                    }
                                                    return Err(error);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            let event = block_change_event(&w, x, y, z);
                            if changed {
                                hub.broadcast(event);
                            } else {
                                send(s, event.id, &event.body)?;
                            }
                        }
                        _ => mining = None,
                    }
                } else {
                    let mut w = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                    let changed =
                        if (action == 0 || action == 2) && y > 60 && near(location, x, y, z) {
                            // Refuse breaking a nonempty chest until contents
                            // can be safely dropped as item entities.
                            let block = w.get(x, y, z) >> 4;
                            let occupied = (block == 54
                                && chests
                                    .lock()
                                    .map_err(|_| bad("chest lock poisoned"))?
                                    .is_nonempty((x, y, z)))
                                || ((block == 61 || block == 62)
                                    && furnaces
                                        .lock()
                                        .map_err(|_| bad("furnace lock poisoned"))?
                                        .get((x, y, z))
                                        .occupied())
                                || (block == 154
                                    && hoppers
                                        .lock()
                                        .map_err(|_| bad("hopper lock poisoned"))?
                                        .has_contents((x, y, z)))
                                || (block == 117
                                    && brewers
                                        .lock()
                                        .map_err(|_| bad("brewer lock poisoned"))?
                                        .is_nonempty((x, y, z)));
                            if occupied {
                                false
                            } else {
                                w.update_and_save(world_path, x, y, z, 0)?
                            }
                        } else {
                            false
                        };
                    let event = block_change_event(&w, x, y, z);
                    if changed {
                        hub.broadcast(event);
                    } else {
                        send(s, event.id, &event.body)?;
                    }
                }
            }
            8 => {
                if !permissions.build {
                    continue;
                }
                let x = input.i32()?;
                let y = input.u8()? as i32;
                let z = input.i32()?;
                let face = input.i8()?;
                let held = input.slot()?;
                input.take(3)?; // cursor x/y/z
                input.finish()?;

                // Protocol 5 Use Item: air target is (-1,255,-1), face -1.
                // Only an authenticated, selected inventory stack can be used.
                if survival && x == -1 && y == 255 && z == -1 && face == -1 {
                    if let Some(stack) = inventory.held() {
                        if inventory.can_place(held) {
                            if stack.id == 261 {
                                bow_start = Some(now_tick);
                            } else if stack.id == 346 {
                                let mut state =
                                    fishing.lock().map_err(|_| bad("fishing mutex poisoned"))?;
                                if state.has_float(entity_id) {
                                    let mut candidate_state = state.clone();
                                    if let Some(reel) = candidate_state.reel(entity_id, now_tick) {
                                        let mut candidate = inventory.clone();
                                        if candidate.wear_fishing_rod() {
                                            let mut spawned = Vec::new();
                                            if let Some(loot) = reel.loot {
                                                if candidate.receive_stack(loot).is_none() {
                                                    // Never silently discard a caught item when
                                                    // the inventory is full. Stage it as a
                                                    // real ground entity and commit both files.
                                                    let mut ground =
                                                        ground_items.lock().map_err(|_| {
                                                            bad("ground items mutex poisoned")
                                                        })?;
                                                    if !ground.has_capacity_for(1) {
                                                        // Preserve the active bobber and rod:
                                                        // no item is created or consumed.
                                                        continue;
                                                    }
                                                    let at = (
                                                        player.x.floor() as i32,
                                                        player.y.floor() as i32,
                                                        player.z.floor() as i32,
                                                    );
                                                    let (staged, dropped) =
                                                        ground.stage_put_many(&[loot], at)?;
                                                    journal::commit(
                                                        &world_path
                                                            .with_extension("pending-txn.json"),
                                                        &[
                                                            (inventory_path, candidate.encoded()?),
                                                            (ground_path, staged.encoded()?),
                                                        ],
                                                    )?;
                                                    *ground = staged;
                                                    spawned = dropped;
                                                } else {
                                                    candidate.save(inventory_path)?;
                                                }
                                            } else {
                                                candidate.save(inventory_path)?;
                                            }
                                            *inventory = candidate;
                                            *state = candidate_state;
                                            drop(state);
                                            send(s, 0x30, &inventory.window_items())?;
                                            for drop in spawned {
                                                for packet in drop.spawn() {
                                                    hub.broadcast(packet);
                                                }
                                            }
                                            for event in reel.events {
                                                hub.broadcast(event);
                                            }
                                        }
                                    }
                                } else if let Some(event) =
                                    state.cast(entity_id, *player, yaw, pitch, now_tick)
                                {
                                    drop(state);
                                    hub.broadcast_in_view(*player, event);
                                }
                            } else if throwables::Kind::from_item(stack.id).is_some() {
                                let mut state = throwables
                                    .lock()
                                    .map_err(|_| bad("throwables lock poisoned"))?;
                                // Under the bounded lock the item is paid for
                                // before the newly spawned entity is public.
                                let mut next = inventory.clone();
                                if state.can_launch(stack.id, entity_id, name, *player, yaw, pitch)
                                    && next.consume_held()
                                {
                                    // Persist first: failure never leaves an unpaid
                                    // projectile. The same lock covers preflight + launch.
                                    next.save(inventory_path)?;
                                    let event = state
                                        .launch(stack.id, entity_id, name, *player, yaw, pitch)
                                        .ok_or_else(|| {
                                            bad("projectile launch after preflight failed")
                                        })?;
                                    *inventory = next;
                                    drop(state);
                                    send(s, 0x2f, &inventory.set_slot(inventory.held_slot())?)?;
                                    hub.broadcast_in_view(*player, event);
                                }
                            } else if vitals.can_eat(stack)
                                || (stack.id == 373 && brewing::effect(stack).is_some())
                                || stack.id == 335
                            // milk is drinkable at full hunger
                            {
                                eating = Some((now_tick, stack));
                            }
                        }
                    }
                    continue;
                }
                // A fence click ties all of this player's nearby lead endpoints.
                // Re-clicking a tied knot releases the anchored animals.
                if survival
                    && (0..=5).contains(&face)
                    && world::in_bounds(x, y, z)
                    && near(location, x, y, z)
                    && world
                        .lock()
                        .map_err(|_| bad("world mutex poisoned"))?
                        .get(x, y, z)
                        >> 4
                        == 85
                {
                    let anchor = [x, y, z];
                    let mut creatures = mobs.lock().map_err(|_| bad("mob mutex poisoned"))?;
                    let mut effects = creatures.tie_fence(entity_id, anchor);
                    if effects.is_empty() {
                        effects = creatures.untie_fence(anchor);
                    }
                    for effect in effects {
                        hub.broadcast(effect);
                    }
                    continue;
                }
                // Minecart item 328 places a bounded rideable cart on rails.
                if survival
                    && (0..=5).contains(&face)
                    && world::in_bounds(x, y, z)
                    && near(location, x, y, z)
                    && inventory.held().is_some_and(|item| item.id == 328)
                    && inventory.can_place(held)
                {
                    let railway = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                    let position = PlayerPosition {
                        x: f64::from(x) + 0.5,
                        y: f64::from(y) + 0.5,
                        z: f64::from(z) + 0.5,
                    };
                    let mut carts = minecarts
                        .lock()
                        .map_err(|_| bad("minecarts mutex poisoned"))?;
                    if carts.can_place(&railway, position) {
                        let mut candidate = inventory.clone();
                        if candidate.consume_held() {
                            candidate.save(inventory_path)?;
                            let spawned = carts
                                .place(&railway, position, now_tick)
                                .ok_or_else(|| bad("minecart vanished after preflight"))?;
                            *inventory = candidate;
                            drop(carts);
                            drop(railway);
                            send(s, 0x2f, &inventory.set_slot(inventory.held_slot())?)?;
                            hub.broadcast_in_view(position, spawned);
                        }
                    }
                    continue;
                }
                // Boat item 333 places a bounded entity on a water block.
                if survival
                    && (0..=5).contains(&face)
                    && world::in_bounds(x, y, z)
                    && near(location, x, y, z)
                    && inventory.held().is_some_and(|item| item.id == 333)
                    && inventory.can_place(held)
                {
                    let water = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                    let spawn_at = PlayerPosition {
                        x: f64::from(x) + 0.5,
                        y: f64::from(y) + 0.45,
                        z: f64::from(z) + 0.5,
                    };
                    let mut state = boats.lock().map_err(|_| bad("boats mutex poisoned"))?;
                    if state.can_place(&water, spawn_at) {
                        let mut candidate = inventory.clone();
                        if candidate.consume_held() {
                            candidate.save(inventory_path)?;
                            let spawned =
                                state.place(&water, spawn_at, now_tick).ok_or_else(|| {
                                    bad("boat vanished between preflight and placement")
                                })?;
                            *inventory = candidate;
                            drop(state);
                            drop(water);
                            send(s, 0x2f, &inventory.set_slot(inventory.held_slot())?)?;
                            hub.broadcast_in_view(spawn_at, spawned);
                        }
                    }
                    continue;
                }
                // Cauldrons, End eyes and bone meal use the authoritative
                // selected slot. Multi-block trees/portals use one world commit.
                if (0..=5).contains(&face)
                    && world::in_bounds(x, y, z)
                    && near(location, x, y, z)
                    && inventory.can_place(held)
                {
                    let mut w = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                    if let Some(changes) = use_special_item(
                        &mut w,
                        world_path,
                        inventory,
                        inventory_path,
                        survival,
                        (x, y, z),
                    )? {
                        for (cx, cy, cz) in changes {
                            hub.broadcast(block_change_event(&w, cx, cy, cz));
                        }
                        if survival {
                            send(s, 0x30, &inventory.window_items())?;
                        }
                        continue;
                    }
                }
                // Hoe -> farmland. In Survival, commit tool wear first;
                // no item is consumed for tilling soil.
                if face == 1
                    && world::in_bounds(x, y, z)
                    && near(location, x, y, z)
                    && inventory.can_place(held)
                    && inventory
                        .held()
                        .is_some_and(|stack| matches!(stack.id, 290..=294))
                {
                    let mut w = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                    if matches!(w.get(x, y, z) >> 4, 2 | 3) && w.get(x, y + 1, z) == 0 {
                        let mut after = inventory.clone();
                        if !survival || after.wear_held_tool() {
                            if survival {
                                after.save(inventory_path)?;
                            }
                            if w.update_and_save(world_path, x, y, z, 60 << 4)? {
                                if survival {
                                    *inventory = after;
                                    send(s, 0x2f, &inventory.set_slot(inventory.held_slot())?)?;
                                }
                                hub.broadcast(block_change_event(&w, x, y, z));
                            }
                        }
                        continue;
                    }
                }
                // Server-owned switch/button/door/repeater interaction.
                if (0..=5).contains(&face) && world::in_bounds(x, y, z) && near(location, x, y, z) {
                    let mut w = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                    let id = w.get(x, y, z) >> 4;
                    if matches!(id, 69 | 77 | 143 | 64 | 71 | 96 | 93 | 94 | 107 | 149 | 150) {
                        let changed = w.interact_device(world_path, x, y, z)?;
                        for (cx, cy, cz) in changed {
                            hub.broadcast(block_change_event(&w, cx, cy, cz));
                        }
                        continue;
                    }
                }
                if (0..=5).contains(&face) && world::in_bounds(x, y, z) && near(location, x, y, z) {
                    let mut w = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                    let old = w.get(x, y, z);
                    if old >> 4 == 69 {
                        if w.update_and_save(world_path, x, y, z, old ^ 8)? {
                            hub.broadcast(block_change_event(&w, x, y, z));
                        }
                        continue;
                    }
                }
                // Two-part bed placement. The two block IDs are committed in
                // one world save, and the held item is debited before placement.
                if face == 1
                    && world::in_bounds(x, y, z)
                    && near(location, x, y, z)
                    && inventory.can_place(held)
                    && inventory.held().is_some_and(|stack| stack.id == 355)
                {
                    let foot = (x, y + 1, z);
                    let facing = ((yaw / 90.0).floor() as i32 + 2).rem_euclid(4) as u16;
                    let (dx, dz) = match facing {
                        0 => (0, 1),
                        1 => (-1, 0),
                        2 => (0, -1),
                        _ => (1, 0),
                    };
                    let head = (foot.0 + dx, foot.1, foot.2 + dz);
                    let mut w = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                    let supported = world::in_bounds(foot.0, foot.1, foot.2)
                        && world::in_bounds(head.0, head.1, head.2)
                        && w.get(x, y, z) != 0
                        && w.get(foot.0, foot.1, foot.2) == 0
                        && w.get(head.0, head.1, head.2) == 0;
                    if supported {
                        let mut candidate = inventory.clone();
                        let paid = !survival || candidate.consume_held();
                        if paid {
                            if survival {
                                candidate.save(inventory_path)?;
                            }
                            let changed = w.update_many_and_save(
                                world_path,
                                &[
                                    (foot.0, foot.1, foot.2, (26 << 4) | facing),
                                    (head.0, head.1, head.2, (26 << 4) | facing | 8),
                                ],
                            )?;
                            if changed {
                                if survival {
                                    *inventory = candidate;
                                    send(s, 0x30, &inventory.window_items())?;
                                }
                                hub.broadcast(block_change_event(&w, foot.0, foot.1, foot.2));
                                hub.broadcast(block_change_event(&w, head.0, head.1, head.2));
                            } else if survival {
                                inventory.save(inventory_path)?;
                            }
                        }
                    } else {
                        for (bx, by, bz) in [foot, head] {
                            if world::in_bounds(bx, by, bz) {
                                let event = block_change_event(&w, bx, by, bz);
                                send(s, event.id, &event.body)?;
                            }
                        }
                    }
                    continue;
                }
                if (0..=5).contains(&face)
                    && world::in_bounds(x, y, z)
                    && near(location, x, y, z)
                    && world
                        .lock()
                        .map_err(|_| bad("world mutex poisoned"))?
                        .get(x, y, z)
                        >> 4
                        == 26
                {
                    let time = clock.day_time();
                    if survival && (12542..=23458).contains(&time) {
                        let mut candidate = inventory.clone();
                        candidate.set_bed_spawn(x, y, z)?;
                        candidate.save(inventory_path)?;
                        *inventory = candidate;
                        let mut packet = Vec::new();
                        packet.extend_from_slice(&entity_id.to_be_bytes());
                        packet.extend_from_slice(&x.to_be_bytes());
                        packet.push(y as u8);
                        packet.extend_from_slice(&z.to_be_bytes());
                        send(s, 0x0a, &packet)?;
                        // A lone player may complete sleep immediately. More
                        // players must use their own beds; never skip their night.
                        if hub.positions()?.len() <= 1 {
                            clock.skip_to_morning();
                            let mut wake = Vec::new();
                            write_varint(entity_id as u32, &mut wake);
                            wake.push(2); // leave bed animation
                            send(s, 0x0b, &wake)?;
                        }
                    } else if survival {
                        as_chat(s, "You can only sleep at night.")?;
                    }
                    continue;
                }
                if (0..=5).contains(&face)
                    && world::in_bounds(x, y, z)
                    && near(location, x, y, z)
                    && matches!(
                        world
                            .lock()
                            .map_err(|_| bad("world mutex poisoned"))?
                            .get(x, y, z)
                            >> 4,
                        61 | 62
                    )
                {
                    let pos = (x, y, z);
                    active_window = next_window;
                    next_window = if next_window >= 100 {
                        1
                    } else {
                        next_window + 1
                    };
                    active_chest = None;
                    active_chest_partner = None;
                    active_furnace = Some(pos);
                    active_hopper = None;
                    active_device = None;
                    active_brewer = None;
                    let guard = furnaces.lock().map_err(|_| bad("furnace lock poisoned"))?;
                    last_furnace_view = guard.get(pos);
                    drop(guard);
                    let mut opened = vec![active_window];
                    write_string("minecraft:furnace", &mut opened)?;
                    write_string(&json!({"text":"Furnace"}).to_string(), &mut opened)?;
                    opened.push(3);
                    send(s, 0x2d, &opened)?;
                    send(
                        s,
                        0x30,
                        &inventory.furnace_window_items(active_window, &last_furnace_view),
                    )?;
                    for property in last_furnace_view.properties(active_window) {
                        send(s, 0x31, &property)?;
                    }
                    send(s, 0x2f, &inventory.cursor_packet())?;
                    continue;
                }
                if (0..=5).contains(&face)
                    && world::in_bounds(x, y, z)
                    && near(location, x, y, z)
                    && world
                        .lock()
                        .map_err(|_| bad("world mutex poisoned"))?
                        .get(x, y, z)
                        >> 4
                        == 54
                {
                    active_window = next_window;
                    next_window = if next_window >= 100 {
                        1
                    } else {
                        next_window + 1
                    };
                    let pos = (x, y, z);
                    active_chest = Some(pos);
                    active_chest_partner = world
                        .lock()
                        .map_err(|_| bad("world mutex poisoned"))
                        .map(|w| chest_neighbor(&w, pos))?;
                    active_furnace = None;
                    active_hopper = None;
                    active_device = None;
                    active_brewer = None;
                    let generated_loot = world
                        .lock()
                        .map_err(|_| bad("world mutex poisoned"))?
                        .generated_dungeon_loot(pos.0, pos.1, pos.2);
                    let mut guard = chests.lock().map_err(|_| bad("chest lock poisoned"))?;
                    if let Some(loot) = generated_loot {
                        guard.initialize_generated(chest_path, pos, loot)?;
                    }
                    let items = inventory.chest_window_items(
                        active_window,
                        &chest_window_state(&guard, pos, active_chest_partner),
                    );
                    observed_chest_revision = guard.revision();
                    drop(guard);
                    let mut opened = vec![active_window];
                    write_string("minecraft:chest", &mut opened)?;
                    write_string(&json!({"text":"Chest"}).to_string(), &mut opened)?;
                    opened.push(if active_chest_partner.is_some() {
                        54
                    } else {
                        27
                    });
                    send(s, 0x2d, &opened)?;
                    send(s, 0x30, &items)?;
                    send(s, 0x2f, &inventory.cursor_packet())?;
                    continue;
                }
                if (0..=5).contains(&face)
                    && world::in_bounds(x, y, z)
                    && near(location, x, y, z)
                    && world
                        .lock()
                        .map_err(|_| bad("world mutex poisoned"))?
                        .get(x, y, z)
                        >> 4
                        == 117
                {
                    let pos = (x, y, z);
                    active_window = next_window;
                    next_window = if next_window >= 100 {
                        1
                    } else {
                        next_window + 1
                    };
                    active_brewer = Some(pos);
                    active_chest = None;
                    active_chest_partner = None;
                    active_furnace = None;
                    active_hopper = None;
                    active_device = None;
                    let guard = brewers.lock().map_err(|_| bad("brewer lock poisoned"))?;
                    last_brewer_view = guard.get(pos);
                    drop(guard);
                    let mut opened = vec![active_window];
                    write_string("minecraft:brewing_stand", &mut opened)?;
                    write_string(&json!({"text":"Brewing Stand"}).to_string(), &mut opened)?;
                    opened.push(4);
                    send(s, 0x2d, &opened)?;
                    send(
                        s,
                        0x30,
                        &inventory.chest_window_items(active_window, &last_brewer_view.slots),
                    )?;
                    send(s, 0x31, &last_brewer_view.property(active_window))?;
                    send(s, 0x2f, &inventory.cursor_packet())?;
                    continue;
                }
                if (0..=5).contains(&face)
                    && world::in_bounds(x, y, z)
                    && near(location, x, y, z)
                    && matches!(
                        world
                            .lock()
                            .map_err(|_| bad("world mutex poisoned"))?
                            .get(x, y, z)
                            >> 4,
                        23 | 158
                    )
                {
                    let pos = (x, y, z);
                    let guard = devices.lock().map_err(|_| bad("device lock poisoned"))?;
                    last_device_view = guard.slots(pos);
                    active_window = next_window;
                    next_window = if next_window >= 100 {
                        1
                    } else {
                        next_window + 1
                    };
                    active_device = Some(pos);
                    active_chest = None;
                    active_chest_partner = None;
                    active_furnace = None;
                    active_hopper = None;
                    active_brewer = None;
                    let items = inventory.chest_window_items(active_window, &last_device_view);
                    drop(guard);
                    let mut opened = vec![active_window];
                    // Both machines use the 1.7.10 dispenser 3x3 window.
                    write_string("minecraft:dispenser", &mut opened)?;
                    write_string(&json!({"text":"Dispenser"}).to_string(), &mut opened)?;
                    opened.push(dispensers::DEVICE_SLOTS as u8);
                    send(s, 0x2d, &opened)?;
                    send(s, 0x30, &items)?;
                    send(s, 0x2f, &inventory.cursor_packet())?;
                    continue;
                }
                if (0..=5).contains(&face)
                    && world::in_bounds(x, y, z)
                    && near(location, x, y, z)
                    && world
                        .lock()
                        .map_err(|_| bad("world mutex poisoned"))?
                        .get(x, y, z)
                        >> 4
                        == 154
                {
                    let pos = (x, y, z);
                    let mut guard = hoppers.lock().map_err(|_| bad("hopper lock poisoned"))?;
                    guard.register(hopper_path, pos)?;
                    active_window = next_window;
                    next_window = if next_window >= 100 {
                        1
                    } else {
                        next_window + 1
                    };
                    active_hopper = Some(pos);
                    active_device = None;
                    active_brewer = None;
                    active_chest = None;
                    active_chest_partner = None;
                    active_furnace = None;
                    observed_hopper_revision = guard.revision();
                    let items = inventory.chest_window_items(active_window, &guard.slots(pos));
                    drop(guard);
                    let mut opened = vec![active_window];
                    write_string("minecraft:hopper", &mut opened)?;
                    write_string(&json!({"text":"Hopper"}).to_string(), &mut opened)?;
                    opened.push(5);
                    send(s, 0x2d, &opened)?;
                    send(s, 0x30, &items)?;
                    send(s, 0x2f, &inventory.cursor_packet())?;
                    continue;
                }
                if (0..=5).contains(&face)
                    && world::in_bounds(x, y, z)
                    && near(location, x, y, z)
                    && world
                        .lock()
                        .map_err(|_| bad("world mutex poisoned"))?
                        .get(x, y, z)
                        >> 4
                        == 58
                {
                    // The 1.7.10 crafting table is a non-storage window.
                    // Window 0 is always the player's personal 2x2 grid.
                    active_window = next_window;
                    active_chest = None;
                    active_chest_partner = None;
                    active_furnace = None;
                    active_hopper = None;
                    active_device = None;
                    active_brewer = None;
                    next_window = if next_window >= 100 {
                        1
                    } else {
                        next_window + 1
                    };
                    let mut opened = vec![active_window];
                    write_string("minecraft:crafting_table", &mut opened)?;
                    write_string(&json!({"text":"Crafting"}).to_string(), &mut opened)?;
                    opened.push(0); // non-storage window: slot count comes from type
                    send(s, 0x2d, &opened)?;
                    send(s, 0x30, &inventory.window_items_for(active_window))?;
                    send(s, 0x2f, &inventory.cursor_packet())?;
                    continue;
                }
                if (0..=5).contains(&face) && world::in_bounds(x, y, z) && near(location, x, y, z) {
                    let (nx, ny, nz) = match face {
                        0 => (x, y - 1, z),
                        1 => (x, y + 1, z),
                        2 => (x, y, z - 1),
                        3 => (x, y, z + 1),
                        4 => (x - 1, y, z),
                        _ => (x + 1, y, z),
                    };
                    let mut w = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                    let mut changed = false;
                    // Only the selected server-held stack may place blocks.
                    if inventory.can_place(held) {
                        if let Some(stack) = inventory.held() {
                            // Survival inventory items are not always block IDs:
                            // dust ID331 -> wire55, water/lava buckets -> fluid8/10.
                            let block_id = match stack.id {
                                331 => 55,
                                287 => 132, // placed tripwire string
                                379 => 117,
                                326 => 8,
                                327 => 10,
                                295 => 59,                       // wheat seeds
                                391 => 141,                      // carrot
                                392 => 142,                      // potato
                                356 => 93,                       // repeater
                                404 => 149,                      // comparator
                                338 => 83,                       // sugar cane
                                361 => 104,                      // pumpkin seeds
                                362 => 105,                      // melon seeds
                                372 => 115,                      // Nether wart
                                351 if stack.damage == 3 => 127, // cocoa beans
                                324 => 64,                       // wooden door
                                330 => 71,                       // iron door
                                _ => stack.id as u16,
                            };
                            let invalid_chest_merge =
                                block_id == 54 && !can_place_chest(&w, (nx, ny, nz));
                            let metadata = if block_id == 131 {
                                // Hook orientation is its outward face (0=S,
                                // 1=W, 2=N, 3=E), not the wall-torch metadata.
                                match face {
                                    2 => 2,
                                    3 => 0,
                                    4 => 1,
                                    5 => 3,
                                    _ => 0,
                                }
                            } else if matches!(block_id, 50 | 77 | 143) {
                                match face {
                                    1 => 5,
                                    2 => 3,
                                    3 => 4,
                                    4 => 1,
                                    5 => 2,
                                    _ => 5,
                                }
                            } else if matches!(block_id, 29 | 33) {
                                face as u16
                            } else if block_id == 96 {
                                match face {
                                    2 => 1,
                                    3 => 0,
                                    4 => 3,
                                    5 => 2,
                                    _ => 0,
                                }
                            } else if matches!(block_id, 64 | 71 | 93 | 149) {
                                ((yaw.rem_euclid(360.0) / 90.0).round() as u16) & 3
                            } else if block_id == 127 {
                                // Cocoa attaches to the side of a jungle log.
                                match face {
                                    2 => 0,
                                    3 => 2,
                                    4 => 3,
                                    5 => 1,
                                    _ => 0,
                                }
                            } else if matches!(
                                block_id,
                                53 | 67 | 108 | 109 | 114 | 128 | 134 | 135 | 136 | 156 | 163 | 164
                            ) {
                                // 1.7.10 stair orientation and inverted-half bit.
                                let direction = ((yaw.rem_euclid(360.0) / 90.0).round() as u16) & 3;
                                direction | if face == 0 { 4 } else { 0 }
                            } else if block_id == 107 {
                                ((yaw.rem_euclid(360.0) / 90.0).round() as u16) & 3
                            } else if matches!(
                                block_id,
                                59 | 141 | 142 | 104 | 105 | 81 | 83 | 115 | 8 | 10
                            ) {
                                0
                            } else {
                                stack.damage as u16 & 15
                            };
                            if (world::is_placeable(block_id) || block_id == 61)
                                && w.can_support(nx, ny, nz, (block_id << 4) | metadata)
                                && block_id != 7
                                && (block_id != 131 || (2..=5).contains(&face))
                                && !invalid_chest_merge
                                && w.get(x, y, z) != 0
                                && w.get(nx, ny, nz) == 0
                                && near(location, nx, ny, nz)
                            {
                                if survival {
                                    let mut remaining = inventory.clone();
                                    let removed = remaining.consume_held();
                                    let bucket_ok = if matches!(stack.id, 326 | 327) {
                                        remaining
                                            .receive_stack(Stack {
                                                id: 325,
                                                count: 1,
                                                damage: 0,
                                            })
                                            .is_some()
                                    } else {
                                        true
                                    };
                                    if removed && bucket_ok {
                                        // Persist item consumption before the world edit so a
                                        // crash cannot grant a free block on restart.
                                        remaining.save(inventory_path)?;
                                        match if matches!(block_id, 64 | 71) {
                                            w.place_door(world_path, nx, ny, nz, block_id, metadata)
                                        } else {
                                            w.update_and_save(
                                                world_path,
                                                nx,
                                                ny,
                                                nz,
                                                (block_id << 4) | metadata,
                                            )
                                        } {
                                            Ok(true) => {
                                                *inventory = remaining;
                                                changed = true;
                                                send(s, 0x30, &inventory.window_items())?;
                                            }
                                            Ok(false) => {
                                                inventory.save(inventory_path)?;
                                            }
                                            Err(error) => {
                                                inventory.save(inventory_path)?;
                                                return Err(error);
                                            }
                                        }
                                    }
                                } else {
                                    changed = if matches!(block_id, 64 | 71) {
                                        w.place_door(world_path, nx, ny, nz, block_id, metadata)?
                                    } else {
                                        w.update_and_save(
                                            world_path,
                                            nx,
                                            ny,
                                            nz,
                                            (block_id << 4) | metadata,
                                        )?
                                    };
                                }
                                if changed && block_id == 154 {
                                    hoppers
                                        .lock()
                                        .map_err(|_| bad("hopper lock poisoned"))?
                                        .register(hopper_path, (nx, ny, nz))?;
                                }
                            }
                        }
                    } else {
                        send(s, 0x2f, &inventory.set_slot(inventory.held_slot())?)?;
                    }
                    if world::in_bounds(nx, ny, nz) {
                        let event = block_change_event(&w, nx, ny, nz);
                        if changed {
                            hub.broadcast(event);
                            if matches!(w.get(nx, ny, nz) >> 4, 64 | 71)
                                && world::in_bounds(nx, ny + 1, nz)
                            {
                                hub.broadcast(block_change_event(&w, nx, ny + 1, nz));
                            }
                        } else {
                            send(s, event.id, &event.body)?;
                        }
                    }
                }
            }
            9 => {
                let selected = input.i16()?;
                input.finish()?;
                inventory.select(selected)?;
                inventory.save(inventory_path)?;
            }
            16 => {
                let slot = input.i16()?;
                let stack = input.slot()?;
                input.finish()?;
                if survival {
                    // C10 is Creative-only; never mint items in Survival.
                    send(s, 0x30, &inventory.window_items())?;
                } else if inventory.creative_set(slot, stack)? {
                    // Explicit, bounded creative inventory actions only.
                    inventory.save(inventory_path)?;
                    send(s, 0x2f, &inventory.set_slot(slot)?)?;
                } else {
                    send(s, 0x30, &inventory.window_items())?;
                }
            }
            14 => {
                let window = input.i8()?;
                let slot = input.i16()?;
                let button = input.i8()?;
                let action = input.i16()?;
                let mode = input.i8()?;
                // Client-provided clicked-item snapshots must never authorize
                // an inventory change: all modifications are server-owned.
                input.skip_untrusted_slot()?;
                input.finish()?;
                let mut requested_window = window >= 0 && window as u8 == active_window;
                let mut accepted = false;
                let response_items;
                // A left/right click outside the window is an inventory-to-world
                // transaction, NOT an ordinary window slot. First stage both
                // snapshots, then persist together through the redo journal.
                // Afterwards refresh whichever container GUI was open without
                // reprocessing this click as a second transaction.
                if requested_window && slot == -999 && mode == 0 && matches!(button, 0 | 1) {
                    let mut candidate = inventory.clone();
                    if let Some(stack) = candidate.drop_cursor(button) {
                        let mut ground = ground_items
                            .lock()
                            .map_err(|_| bad("ground item lock poisoned"))?;
                        if ground.has_capacity_for(1) {
                            let at = (
                                (location.0.floor() as i32)
                                    .clamp(-world::WORLD_LIMIT, world::WORLD_LIMIT - 1),
                                (location.1.floor() as i32).clamp(0, 254),
                                (location.2.floor() as i32)
                                    .clamp(-world::WORLD_LIMIT, world::WORLD_LIMIT - 1),
                            );
                            let (next_ground, spawned) = ground.stage_put_many(&[stack], at)?;
                            journal::commit(
                                &world_path.with_extension("pending-txn.json"),
                                &[
                                    (inventory_path, candidate.encoded()?),
                                    (ground_path, next_ground.encoded()?),
                                ],
                            )?;
                            *inventory = candidate;
                            *ground = next_ground;
                            for item in spawned {
                                for event in item.spawn() {
                                    hub.broadcast(event);
                                }
                            }
                            accepted = true;
                        }
                    } else {
                        // Clicking outside with an empty cursor is a valid no-op.
                        accepted = true;
                    }
                    requested_window = false;
                }
                if let Some(pos) = active_furnace {
                    let still_open = matches!(
                        world
                            .lock()
                            .map_err(|_| bad("world mutex poisoned"))?
                            .get(pos.0, pos.1, pos.2)
                            >> 4,
                        61 | 62
                    ) && near(location, pos.0, pos.1, pos.2);
                    let mut machines = furnaces.lock().map_err(|_| bad("furnace lock poisoned"))?;
                    let previous = machines.get(pos);
                    let mut next = previous;
                    let mut candidate = inventory.clone();
                    let direction = if still_open && requested_window {
                        candidate.furnace_click(&mut next, slot, button, mode)
                    } else {
                        None
                    };
                    if let Some(direction) = direction {
                        accepted = true;
                        let inv_changed = candidate != *inventory;
                        let furnace_changed = next != previous;
                        if inv_changed && furnace_changed {
                            let staged = machines.staged_update(pos, next)?;
                            journal::commit(
                                &world_path.with_extension("pending-txn.json"),
                                &[
                                    (inventory_path, candidate.encoded()?),
                                    (furnace_path, staged.encoded()?),
                                ],
                            )?;
                            *inventory = candidate;
                            *machines = staged;
                        } else if furnace_changed {
                            machines.update(furnace_path, pos, next)?;
                        } else if inv_changed {
                            candidate.save(inventory_path)?;
                            *inventory = candidate;
                        }
                        let _ = direction;
                    }
                    last_furnace_view = machines.get(pos);
                    response_items =
                        inventory.furnace_window_items(active_window, &last_furnace_view);
                    // Smelt XP is created only after an actual server-owned output withdrawal.
                    if accepted
                        && slot == 2
                        && previous.slots[2].is_some()
                        && last_furnace_view.slots[2].is_none()
                    {
                        let output = previous.slots[2].unwrap();
                        let per_item: u16 = match output.id {
                            265 | 266 | 320 | 364 | 366 | 393 | 336 | 172 | 405 => 1,
                            263 if output.damage == 1 => 1,
                            _ => 0,
                        };
                        let earned = per_item.saturating_mul(u16::from(output.count));
                        if earned > 0 {
                            let orb_at = PlayerPosition {
                                x: pos.0 as f64 + 0.5,
                                y: pos.1 as f64 + 1.0,
                                z: pos.2 as f64 + 0.5,
                            };
                            if let Some(orb) = mobs
                                .lock()
                                .map_err(|_| bad("mob mutex poisoned"))?
                                .spawn_xp(orb_at, earned)
                            {
                                hub.broadcast(orb);
                            }
                        }
                    }
                    drop(machines);
                } else if let Some(pos) = active_chest {
                    let still_open = {
                        let w = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                        w.get(pos.0, pos.1, pos.2) >> 4 == 54
                            && near(location, pos.0, pos.1, pos.2)
                            && active_chest_partner.is_none_or(|partner| {
                                w.get(partner.0, partner.1, partner.2) >> 4 == 54
                            })
                    };
                    let mut storage = chests.lock().map_err(|_| bad("chest lock poisoned"))?;
                    let original = chest_window_state(&storage, pos, active_chest_partner);
                    let mut changed_slots = original.clone();
                    let mut candidate = inventory.clone();
                    let change = if still_open && requested_window {
                        candidate.chest_click_window(
                            active_window,
                            &mut changed_slots,
                            slot,
                            button,
                            mode,
                        )
                    } else {
                        None
                    };
                    if let Some(direction) = change {
                        accepted = true;
                        let inventory_changed = candidate != *inventory;
                        let storage_changed = original != changed_slots;
                        if inventory_changed && storage_changed {
                            let staged = stage_chest_window(
                                &storage,
                                pos,
                                active_chest_partner,
                                &changed_slots,
                            )?;
                            journal::commit(
                                &world_path.with_extension("pending-txn.json"),
                                &[
                                    (inventory_path, candidate.encoded()?),
                                    (chest_path, staged.encoded()?),
                                ],
                            )?;
                            *inventory = candidate;
                            *storage = staged;
                        } else if storage_changed {
                            let staged = stage_chest_window(
                                &storage,
                                pos,
                                active_chest_partner,
                                &changed_slots,
                            )?;
                            storage.commit_staged(chest_path, staged)?;
                        } else if inventory_changed {
                            candidate.save(inventory_path)?;
                            *inventory = candidate;
                        }
                        let _ = direction;
                    }
                    observed_chest_revision = storage.revision();
                    response_items = inventory.chest_window_items(
                        active_window,
                        &chest_window_state(&storage, pos, active_chest_partner),
                    );
                    drop(storage);
                } else if let Some(pos) = active_brewer {
                    let still_open = {
                        let w = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                        w.get(pos.0, pos.1, pos.2) >> 4 == 117
                            && near(location, pos.0, pos.1, pos.2)
                    };
                    let mut storage = brewers.lock().map_err(|_| bad("brewer lock poisoned"))?;
                    let original = storage.get(pos);
                    let mut changed = original.slots;
                    let mut candidate = inventory.clone();
                    let change = if still_open && requested_window {
                        candidate.chest_click_window(
                            active_window,
                            &mut changed,
                            slot,
                            button,
                            mode,
                        )
                    } else {
                        None
                    };
                    let mut next = original;
                    next.slots = changed;
                    if change.is_some() && next.valid() {
                        accepted = true;
                        let inv_changed = candidate != *inventory;
                        let brewer_changed = next != original;
                        if inv_changed && brewer_changed {
                            let staged = storage.staged_update(pos, next)?;
                            journal::commit(
                                &world_path.with_extension("pending-txn.json"),
                                &[
                                    (inventory_path, candidate.encoded()?),
                                    (brewer_path, staged.encoded()?),
                                ],
                            )?;
                            *inventory = candidate;
                            *storage = staged;
                        } else if brewer_changed {
                            let staged = storage.staged_update(pos, next)?;
                            storage.commit_staged(brewer_path, staged)?;
                        } else if inv_changed {
                            candidate.save(inventory_path)?;
                            *inventory = candidate;
                        }
                    }
                    last_brewer_view = storage.get(pos);
                    response_items =
                        inventory.chest_window_items(active_window, &last_brewer_view.slots);
                    drop(storage);
                } else if let Some(pos) = active_device {
                    let still_open = {
                        let w = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                        matches!(w.get(pos.0, pos.1, pos.2) >> 4, 23 | 158)
                            && near(location, pos.0, pos.1, pos.2)
                    };
                    let mut storage = devices.lock().map_err(|_| bad("device lock poisoned"))?;
                    let original = storage.slots(pos);
                    let mut next_slots = original;
                    let mut candidate = inventory.clone();
                    let change = if still_open && requested_window {
                        candidate.chest_click_window(
                            active_window,
                            &mut next_slots,
                            slot,
                            button,
                            mode,
                        )
                    } else {
                        None
                    };
                    if change.is_some() {
                        accepted = true;
                        let item_changed = next_slots != original;
                        let player_changed = candidate != *inventory;
                        if item_changed {
                            let staged = storage.staged_update(pos, next_slots)?;
                            if player_changed {
                                journal::commit(
                                    &world_path.with_extension("pending-txn.json"),
                                    &[
                                        (inventory_path, candidate.encoded()?),
                                        (device_path, staged.encoded()?),
                                    ],
                                )?;
                            } else {
                                staged.save(device_path)?;
                            }
                            *storage = staged;
                        } else if player_changed {
                            candidate.save(inventory_path)?;
                        }
                        if player_changed {
                            *inventory = candidate;
                        }
                    }
                    last_device_view = storage.slots(pos);
                    response_items = inventory.chest_window_items(active_window, &last_device_view);
                    drop(storage);
                } else if let Some(pos) = active_hopper {
                    let still_open = {
                        let w = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                        w.get(pos.0, pos.1, pos.2) >> 4 == 154
                            && near(location, pos.0, pos.1, pos.2)
                    };
                    let mut storage = hoppers.lock().map_err(|_| bad("hopper lock poisoned"))?;
                    let original = storage.slots(pos);
                    let mut changed = original;
                    let mut candidate = inventory.clone();
                    let change = if still_open && requested_window {
                        candidate.chest_click_window(
                            active_window,
                            &mut changed,
                            slot,
                            button,
                            mode,
                        )
                    } else {
                        None
                    };
                    if change.is_some() {
                        accepted = true;
                        let inv_changed = candidate != *inventory;
                        let hopper_changed = changed != original;
                        if inv_changed && hopper_changed {
                            let staged = storage.staged_update(pos, changed)?;
                            journal::commit(
                                &world_path.with_extension("pending-txn.json"),
                                &[
                                    (inventory_path, candidate.encoded()?),
                                    (hopper_path, staged.encoded()?),
                                ],
                            )?;
                            *inventory = candidate;
                            *storage = staged;
                        } else if hopper_changed {
                            let staged = storage.staged_update(pos, changed)?;
                            storage.commit_staged(hopper_path, staged)?;
                        } else if inv_changed {
                            candidate.save(inventory_path)?;
                            *inventory = candidate;
                        }
                    }
                    observed_hopper_revision = storage.revision();
                    response_items =
                        inventory.chest_window_items(active_window, &storage.slots(pos));
                    drop(storage);
                } else {
                    let mut candidate = inventory.clone();
                    accepted |= requested_window
                        && candidate.click_window(active_window, slot, button, mode);
                    if accepted && candidate != *inventory {
                        candidate.save(inventory_path)?;
                        *inventory = candidate;
                    }
                    response_items = inventory.window_items_for(active_window);
                }
                let mut reply = vec![window as u8];
                reply.extend_from_slice(&action.to_be_bytes());
                reply.push(u8::from(accepted));
                send(s, 0x32, &reply)?;
                send(s, 0x30, &response_items)?;
                if active_furnace.is_some() {
                    for property in last_furnace_view.properties(active_window) {
                        send(s, 0x31, &property)?;
                    }
                }
                if active_brewer.is_some() {
                    send(s, 0x31, &last_brewer_view.property(active_window))?;
                }
                send(s, 0x2f, &inventory.cursor_packet())?;
            }
            15 => {
                // Acknowledgement for rejected Click Window transactions.
                input.i8()?; // window
                input.i16()?; // action
                input.u8()?; // accepted
                input.finish()?;
            }
            13 => {
                let closed = input.u8()?;
                input.finish()?;
                if active_window == closed {
                    // Keep unused ingredients saved rather than silently
                    // deleting them if a player closes the crafting window.
                    active_window = 0;
                    active_chest = None;
                    active_chest_partner = None;
                    active_furnace = None;
                    active_hopper = None;
                    active_device = None;
                    active_brewer = None;
                    send(s, 0x30, &inventory.window_items())?;
                }
            }
            22 => {
                // Protocol 5 C16 Client Status: 0 = Perform Respawn.
                let action = input.i8()?;
                input.finish()?;
                if survival && action == 2 {
                    // Vanilla 1.7.10 Client Status: 2 = Open Inventory
                    // Achievement. Never trust an arbitrary client stat name.
                    let mut candidate = achievements.clone();
                    if let Some(event) = candidate.award("openInventory")? {
                        candidate.save(achievements_path)?;
                        *achievements = candidate;
                        send(s, event.id, &event.body)?;
                    }
                }
                if survival && action == 0 && vitals.health <= 0.0 {
                    if hardcore {
                        player.save_state(player_path, *vitals)?;
                        return Ok(());
                    }
                    let keep_inventory = game_rules
                        .lock()
                        .map_err(|_| bad("game rules mutex poisoned"))?
                        .keep_inventory();
                    if !keep_inventory {
                        commit_death_drops(
                            inventory,
                            inventory_path,
                            ground_items,
                            ground_path,
                            world_path,
                            hub,
                            location,
                        )?;
                    }
                    // Preserve the death site before changing the saved player
                    // position. XP orbs belong beside the dropped inventory,
                    // never at a distant bed or fallback world spawn.
                    let death_position = *player;
                    let origin = {
                        let fallback = safe_spawn(world)?;
                        if let Some((x, y, z)) = inventory.bed_spawn() {
                            let guard = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                            if guard.get(x, y, z) >> 4 == 26
                                && world::in_bounds(x, y + 1, z)
                                && guard.get(x, y + 1, z) == 0
                            {
                                PlayerPosition {
                                    x: x as f64 + 0.5,
                                    y: y as f64 + 1.0,
                                    z: z as f64 + 0.5,
                                }
                            } else {
                                fallback
                            }
                        } else {
                            fallback
                        }
                    };
                    let fresh_vitals = Vitals::default();
                    origin.save_state(player_path, fresh_vitals)?;
                    *vitals = fresh_vitals;
                    *player = origin;
                    death_recorded = false;
                    fall = FallTracker::new(origin.y);
                    idle_gravity = IdleGravity::new(now_tick);
                    eating = None;
                    bow_start = None;
                    // Death clears every temporary potion effect; stale
                    // regeneration must never resurrect a dead player before
                    // the server has committed death drops and respawn.
                    active_effects.clear();
                    last_effect_tick = now_tick;
                    hazards = Hazards::new();
                    last_hazards_tick = now_tick;
                    // In vanilla, death loses XP and may leave up to 100 XP in orbs.
                    if !keep_inventory {
                        let drop_amount =
                            (u32::from(experience.level_and_fraction().0) * 7).min(100) as u16;
                        *experience = Experience::default();
                        experience.save(experience_path)?;
                        if let Some(orb) = mobs
                            .lock()
                            .map_err(|_| bad("mob mutex poisoned"))?
                            .spawn_xp(death_position, drop_amount)
                        {
                            hub.broadcast(orb);
                        }
                    }
                    location = (origin.x, origin.y, origin.z);
                    let mut respawn = 0i32.to_be_bytes().to_vec();
                    respawn.extend_from_slice(&[difficulty, 0]); // configured difficulty, survival
                    write_string("default", &mut respawn)?;
                    send(s, 0x07, &respawn)?;
                    loaded_chunks.clear(); // the client forgets chunks on respawn
                    sync_chunks(s, world, &mut loaded_chunks, chunk_center(player))?;
                    for mob in mobs
                        .lock()
                        .map_err(|_| bad("mob mutex poisoned"))?
                        .spawn_events()
                    {
                        send(s, mob.id, &mob.body)?;
                    }
                    send(s, 0x06, &health_packet(*vitals))?;
                    send(s, 0x1f, &experience.packet())?;
                    send(s, 0x30, &inventory.window_items())?;
                    send(s, 0x2f, &inventory.cursor_packet())?;
                    send_player_position(s, player)?;
                    hub.move_player(entity_id, *player, yaw, pitch);
                }
            }
            // Other packets are not trusted inventory or world mutations.
            10..=12 | 17..=21 | 23 => {}
            _ => return Err(bad("invalid play packet ID")),
        }
    }
}
/// Commit both the debited inventory and all spawned death drops as one
/// redo-journaled transaction. Until both snapshots are durable, neither
/// state is visible to players.
fn commit_death_drops(
    inventory: &mut Inventory,
    inventory_path: &Path,
    ground_items: &Mutex<GroundItems>,
    ground_path: &Path,
    world_path: &Path,
    hub: &Hub,
    location: (f64, f64, f64),
) -> io::Result<()> {
    let possessions = inventory.possessions();
    if possessions.is_empty() {
        return Ok(());
    }
    let mut ground = ground_items
        .lock()
        .map_err(|_| io::Error::other("ground items lock poisoned"))?;
    if !ground.has_capacity_for(possessions.len()) {
        return Err(bad("death-drop capacity exhausted"));
    }
    let mut empty = inventory.clone();
    empty.clear_possessions();
    let block = (
        (location.0.floor() as i32).clamp(-world::WORLD_LIMIT, world::WORLD_LIMIT - 1),
        (location.1.floor() as i32).clamp(0, 254),
        (location.2.floor() as i32).clamp(-world::WORLD_LIMIT, world::WORLD_LIMIT - 1),
    );
    let (staged_ground, spawned) = ground.stage_put_many(&possessions, block)?;
    journal::commit(
        &world_path.with_extension("pending-txn.json"),
        &[
            (inventory_path, empty.encoded()?),
            (ground_path, staged_ground.encoded()?),
        ],
    )?;
    *inventory = empty;
    *ground = staged_ground;
    for item in spawned {
        for packet in item.spawn() {
            hub.broadcast(packet);
        }
    }
    Ok(())
}

struct PlayerAdmission {
    hub: Arc<Hub>,
    entity_id: i32,
}
impl Drop for PlayerAdmission {
    fn drop(&mut self) {
        self.hub.remove(self.entity_id);
    }
}
struct ConnectionGuard(Arc<AtomicUsize>);
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
struct ServerContext {
    hub: Arc<Hub>,
    world: Arc<Mutex<World>>,
    path: Arc<PathBuf>,
    whitelist_path: Arc<PathBuf>,
    administration: Arc<Administration>,
    clock: Arc<Clock>,
    ground_items: Arc<Mutex<GroundItems>>,
    ground_path: Arc<PathBuf>,
    mobs: Arc<Mutex<Mobs>>,
    throwables: Arc<Mutex<Throwables>>,
    boats: Arc<Mutex<Boats>>,
    minecarts: Arc<Mutex<Minecarts>>,
    fishing: Arc<Mutex<Fishing>>,
    chests: Arc<Mutex<Chests>>,
    chest_path: Arc<PathBuf>,
    furnaces: Arc<Mutex<Furnaces>>,
    furnace_path: Arc<PathBuf>,
    hoppers: Arc<Mutex<Hoppers>>,
    hopper_path: Arc<PathBuf>,
    devices: Arc<Mutex<Devices>>,
    device_path: Arc<PathBuf>,
    brewers: Arc<Mutex<Brewers>>,
    brewer_path: Arc<PathBuf>,
    survival: bool,
    hardcore: bool,
    difficulty: u8,
    game_rules: Arc<Mutex<GameRules>>,
    scoreboard: Arc<Mutex<Scoreboard>>,
    scoreboard_path: Arc<PathBuf>,
    online_mode: bool,
    auth_key: Option<Arc<ServerIdentity>>,
}
fn handle(
    s: TcpStream,
    context: ServerContext,
    login_permit: security_limits::Permit,
) -> io::Result<()> {
    let ServerContext {
        hub,
        world,
        path,
        whitelist_path,
        administration,
        clock,
        ground_items,
        ground_path,
        mobs,
        throwables,
        boats,
        minecarts,
        fishing,
        chests,
        chest_path,
        furnaces,
        furnace_path,
        hoppers,
        hopper_path,
        devices,
        device_path,
        brewers,
        brewer_path,
        survival,
        hardcore,
        difficulty,
        game_rules,
        scoreboard,
        scoreboard_path,
        online_mode,
        auth_key,
    } = context;
    let mut s = Connection::new(s);
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    s.set_write_timeout(Some(Duration::from_secs(5)))?;
    let peer_ip = s.peer_addr()?.ip();
    let (id, data) = receive(&mut s)?;
    if id != 0 {
        return Err(bad("expected handshake"));
    }
    let mut handshake = Input(&data);
    let version = handshake.varint()?;
    let _hostname = handshake.string(255)?;
    handshake.u16()?;
    let next = handshake.varint()?;
    handshake.finish()?;
    if next == 1 {
        return status(&mut s, &hub);
    }
    if next != 2 {
        return Err(bad("unsupported handshake state"));
    }
    // Rate-limit login attempts, not harmless server-list status handshakes.
    // The pending unauthenticated connection itself remains slot-limited.
    if !login_permit.login_attempt() {
        login_disconnect(&mut s, "Too many login attempts. Please try again later.")?;
        return Ok(());
    }
    if version != PROTOCOL_VERSION {
        login_disconnect(
            &mut s,
            "Use an unmodified Minecraft Java 1.7.10 (protocol 5) client.",
        )?;
        return Ok(());
    }
    let (id, data) = receive(&mut s)?;
    if id != 0 {
        return Err(bad("expected login start"));
    }
    let mut login = Input(&data);
    let name = login.string(16)?;
    login.finish()?;
    if !whitelist::valid_name(&name) {
        login_disconnect(&mut s, "Invalid player name.")?;
        return Ok(());
    }
    // Recheck bans at Login Start. Missing/corrupt DBs are fail-closed.
    match administration.banned(&name, peer_ip) {
        Ok(Some(reason)) => {
            login_disconnect(&mut s, &reason)?;
            return Ok(());
        }
        Err(error) => {
            eprintln!("admin database unavailable: {error}");
            login_disconnect(&mut s, "Administration database unavailable.")?;
            return Ok(());
        }
        Ok(None) => {}
    }
    // Every new login reads the whitelist from disk: no implicit public access.
    let allowed = match Whitelist::load(&whitelist_path) {
        Ok(list) => list.allows(&name),
        Err(error) => {
            eprintln!("whitelist access denied due to read/parse error: {error}");
            false
        }
    };
    if !allowed {
        login_disconnect(&mut s, "You are not on the server whitelist.")?;
        return Ok(());
    }
    // Authenticate before loading or admitting a player. Online failures
    // must never be turned into offline UUIDs, even on Mojang timeout.
    let (name, uuid) = if online_mode {
        let key = auth_key
            .as_ref()
            .ok_or_else(|| bad("missing online-mode RSA identity"))?;
        let profile = key.authenticate(&mut s, &name)?;
        (profile.name, profile.uuid)
    } else {
        let uuid = offline_uuid(&name);
        (name, uuid)
    };
    // A ban or whitelist can change while session verification is in flight.
    // Recheck the authenticated identity before opening any player data.
    let still_allowed = Whitelist::load(&whitelist_path).is_ok_and(|list| list.allows(&name))
        && administration
            .banned_identity(&name, online_mode.then_some(uuid.as_str()), peer_ip)
            .is_ok_and(|reason| reason.is_none());
    if !still_allowed {
        login_disconnect(&mut s, "Login denied: whitelist or ban updated.")?;
        return Ok(());
    }
    let player_file = if online_mode {
        PlayerPosition::authenticated_location(&path, &uuid)?
    } else {
        PlayerPosition::location(&path, &name)
    };
    let (mut player_state, mut player_vitals) = match PlayerPosition::load_state(&player_file) {
        Ok(state) => state,
        Err(err) => {
            eprintln!("player state unavailable for {name}: {err}");
            login_disconnect(
                &mut s,
                "Player save is unreadable. Administrator must repair it.",
            )?;
            return Ok(());
        }
    };
    if hardcore && player_vitals.health <= 0.0 {
        login_disconnect(
            &mut s,
            "Hardcore death: rejoining this world is not permitted.",
        )?;
        return Ok(());
    }
    if !player_file.exists() && !player_file.with_extension("json").exists() {
        player_state = safe_spawn(&world)?;
    }
    // Separate survival resources from old Creative inventories to prevent
    // player-selected Creative stacks becoming free Survival items.
    let inventory_file = if survival {
        player_file.with_extension("survival.inventory.json")
    } else {
        Inventory::location(&player_file)
    };
    let mut inventory_state = match Inventory::load(&inventory_file) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("inventory unavailable for {name}: {err}");
            login_disconnect(
                &mut s,
                "Inventory save is unreadable. Administrator must repair it.",
            )?;
            return Ok(());
        }
    };
    let experience_file = player_file.with_extension("experience.json");
    let mut experience_state = Experience::load(&experience_file)?;
    let achievements_file = player_file.with_extension("achievements.json");
    let mut achievements_state = Achievements::load(&achievements_file)?;
    let (entity_id, events, previous) = match hub.add(name.clone(), uuid.clone(), player_state) {
        Ok(result) => result,
        Err(JoinError::Full) => {
            login_disconnect(&mut s, "Server is full.")?;
            return Ok(());
        }
        Err(JoinError::Duplicate) => {
            login_disconnect(&mut s, "That username is already connected.")?;
            return Ok(());
        }
    };
    let _admission = PlayerAdmission {
        hub: Arc::clone(&hub),
        entity_id,
    };
    // Once Hub admits a named player, the connection is no longer an
    // unauthenticated pending-login slot. The global MAX_CONNECTIONS and
    // Hub MAX_PLAYERS limits still apply throughout the Play session.
    drop(login_permit);
    let mut success = Vec::new();
    write_string(&uuid, &mut success)?;
    write_string(&name, &mut success)?;
    send(&mut s, 0x02, &success)?;
    println!(
        "joined: {name} ({})",
        if online_mode {
            "MOJANG VERIFIED"
        } else {
            "UNAUTHENTICATED OFFLINE MODE"
        }
    );
    join(
        &mut s,
        &world,
        &clock,
        entity_id,
        InitialPlayer {
            position: &player_state,
            inventory: &inventory_state,
            vitals: player_vitals,
            survival,
            hardcore,
            difficulty,
        },
    )?;
    if survival {
        send(&mut s, 0x1f, &experience_state.packet())?;
    }
    for packet in scoreboard
        .lock()
        .map_err(|_| bad("scoreboard lock poisoned"))?
        .snapshot()?
    {
        send(&mut s, packet.id, &packet.body)?;
    }
    let achievements_packet = achievements_state.snapshot()?;
    send(&mut s, achievements_packet.id, &achievements_packet.body)?;
    // 1.7.10 tab list entries precede other player entity spawns.
    let self_list = multiplayer::list_entry(&name, true)?;
    send(&mut s, self_list.id, &self_list.body)?;
    for other in &previous {
        let list = multiplayer::list_entry(&other.name, true)?;
        send(&mut s, list.id, &list.body)?;
        if multiplayer::visible(&player_state, &other.position) {
            let spawn = multiplayer::spawn(other)?;
            send(&mut s, spawn.id, &spawn.body)?;
        }
    }
    // Send existing persisted items to newly joining clients. They are
    // independent of the player's chunk updates in this bounded prototype.
    if survival {
        let ground = ground_items
            .lock()
            .map_err(|_| io::Error::other("ground items lock poisoned"))?;
        for item in ground.items() {
            for packet in item.spawn() {
                send(&mut s, packet.id, &packet.body)?;
            }
        }
        drop(ground);
        let creatures = mobs.lock().map_err(|_| bad("mob mutex poisoned"))?;
        for mob in creatures.spawn_events() {
            send(&mut s, mob.id, &mob.body)?;
        }
        for object in throwables
            .lock()
            .map_err(|_| bad("throwables mutex poisoned"))?
            .snapshot(player_state)
        {
            send(&mut s, object.id, &object.body)?;
        }
        for object in boats
            .lock()
            .map_err(|_| bad("boats mutex poisoned"))?
            .snapshot(player_state)
        {
            send(&mut s, object.id, &object.body)?;
        }
        for object in minecarts
            .lock()
            .map_err(|_| bad("minecarts mutex poisoned"))?
            .snapshot(player_state)
        {
            send(&mut s, object.id, &object.body)?;
        }
        for object in fishing
            .lock()
            .map_err(|_| bad("fishing mutex poisoned"))?
            .snapshot(player_state)
        {
            send(&mut s, object.id, &object.body)?;
        }
    }
    let result = play(
        &mut s,
        &world,
        &path,
        &name,
        &whitelist_path,
        &clock,
        PlayerSession {
            position: &mut player_state,
            vitals: &mut player_vitals,
            experience: &mut experience_state,
            experience_path: &experience_file,
            achievements: &mut achievements_state,
            achievements_path: &achievements_file,
            save_path: &player_file,
            inventory: &mut inventory_state,
            inventory_path: &inventory_file,
            ground_items: &ground_items,
            ground_path: &ground_path,
            mobs: &mobs,
            throwables: &throwables,
            boats: &boats,
            minecarts: &minecarts,
            fishing: &fishing,
            chests: &chests,
            chest_path: &chest_path,
            furnaces: &furnaces,
            furnace_path: &furnace_path,
            hoppers: &hoppers,
            hopper_path: &hopper_path,
            devices: &devices,
            device_path: &device_path,
            brewers: &brewers,
            brewer_path: &brewer_path,
            survival,
            hardcore,
            difficulty,
            game_rules: &game_rules,
            scoreboard: &scoreboard,
            scoreboard_path: &scoreboard_path,
            hub: &hub,
            entity_id,
            events: &events,
            administration: &administration,
            peer_ip,
            verified_uuid: if online_mode { Some(&uuid) } else { None },
        },
    );
    // Save even on a clean disconnect, timeout, or access revocation.
    // Snapshot living entities on disconnect, not only on periodic 20TPS ticks.
    // Always lock world before mobs to match the gameplay tick lock order.
    let mob_saved = {
        let guard = world.lock().map_err(|_| bad("world mutex poisoned"))?;
        mobs.lock()
            .map_err(|_| bad("mob mutex poisoned"))?
            .save(&guard)
    };
    let player_saved = player_state.save_state(&player_file, player_vitals);
    let inventory_saved = inventory_state.save(&inventory_file);
    let experience_saved = experience_state.save(&experience_file);
    let achievements_saved = achievements_state.save(&achievements_file);
    if let Ok(mut state) = boats.lock() {
        if let Some(event) = state.dismount(entity_id) {
            hub.broadcast(event);
        }
    }
    if let Ok(mut state) = minecarts.lock() {
        if let Some(event) = state.dismount(entity_id) {
            hub.broadcast(event);
        }
    }
    if let Ok(mut state) = mobs.lock() {
        for event in state.release_player(entity_id) {
            hub.broadcast(event);
        }
    }
    if let Ok(mut state) = fishing.lock() {
        if let Some(event) = state.remove_owner(entity_id) {
            hub.broadcast(event);
        }
    }
    println!("left: {name}");
    mob_saved?;
    player_saved?;
    inventory_saved?;
    experience_saved?;
    achievements_saved?;
    result
}
/// Services required to execute a level-gated in-game administrative command.
struct PlayerAdminServices<'a> {
    hub: &'a Hub,
    world: &'a Mutex<World>,
    world_path: &'a Path,
    clock: &'a Clock,
    administration: &'a Administration,
}
/// Authorization is based exclusively on the Mojang-verified session UUID.
fn run_player_admin_command(
    message: &str,
    level: u8,
    services: &PlayerAdminServices<'_>,
    actor: &str,
) -> io::Result<String> {
    let PlayerAdminServices {
        hub,
        world,
        world_path,
        clock,
        administration,
    } = services;
    let parts: Vec<&str> = message.split_whitespace().collect();
    let minimum_level = match parts.first().copied() {
        Some("/time") | Some("/weather") => 2,
        Some("/kick") | Some("/ban") | Some("/pardon") | Some("/ban-ip") | Some("/pardon-ip") => 3,
        Some("/save") | Some("/op") | Some("/deop") | Some("/permission") => 4,
        _ => return Err(bad("unknown operator command")),
    };
    if level < minimum_level {
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            "insufficient OP level",
        ));
    }
    match parts.as_slice() {
        ["/save"] => {
            world
                .lock()
                .map_err(|_| bad("world mutex poisoned"))?
                .save(world_path)?;
            Ok("World saved".into())
        }
        ["/kick", target, reason @ ..] => {
            let reason = if reason.is_empty() {
                "Kicked by operator".to_string()
            } else {
                reason.join(" ")
            };
            if !hub.request_admin_kick(target, &reason)? {
                return Err(io::Error::new(ErrorKind::NotFound, "target not online"));
            }
            Ok(format!("Kicked {target}"))
        }
        ["/ban", target, reason @ ..] => {
            let reason = if reason.is_empty() {
                "Banned by operator".to_string()
            } else {
                reason.join(" ")
            };
            let uuid = hub.uuid_for_online_name(target)?;
            administration.set_player_ban_verified(target, uuid.as_deref(), Some(&reason), true)?;
            let _ = hub.request_admin_kick(target, &reason);
            Ok(format!("Banned {target}"))
        }
        ["/pardon", target] => {
            administration.set_player_ban(target, None, false)?;
            Ok(format!("Unbanned {target}"))
        }
        ["/ban-ip", address, reason @ ..] => {
            let reason = if reason.is_empty() {
                "IP banned by operator".to_string()
            } else {
                reason.join(" ")
            };
            administration.set_ip_ban(address, Some(&reason), true)?;
            Ok(format!("IP banned {address}"))
        }
        ["/pardon-ip", address] => {
            administration.set_ip_ban(address, None, false)?;
            Ok(format!("IP unbanned {address}"))
        }
        ["/time", "set", ticks] => {
            let ticks = ticks.parse::<u64>().map_err(|_| bad("invalid time"))?;
            clock.set_day_time(ticks)?;
            let mut body = Vec::with_capacity(16);
            body.extend_from_slice(&(clock.ticks() as i64).to_be_bytes());
            body.extend_from_slice(&clock.day_time().to_be_bytes());
            hub.broadcast(Event { id: 0x03, body });
            Ok(format!("Time set by {actor}"))
        }
        ["/weather", "set", state] => {
            clock.set_weather(state)?;
            for body in clock.weather_packets() {
                hub.broadcast(Event { id: 0x2b, body });
            }
            Ok(format!("Weather set by {actor}"))
        }
        ["/op", target] => {
            let uuid = hub.uuid_for_online_name(target)?.ok_or_else(|| {
                io::Error::new(
                    ErrorKind::NotFound,
                    "target must be authenticated and online",
                )
            })?;
            administration.set_op_verified(target, &uuid)?;
            Ok(format!("OP granted to {target}"))
        }
        ["/deop", target] => {
            administration.set_op(target, false)?;
            Ok(format!("OP revoked from {target}"))
        }
        ["/permission", target, kind, state] => {
            let enabled = match *state {
                "allow" => true,
                "deny" => false,
                _ => return Err(bad("permission state must be allow or deny")),
            };
            administration.set_permission(target, kind, enabled)?;
            Ok(format!("Permission updated for {target}"))
        }
        _ => Err(bad("invalid operator command")),
    }
}

struct ConsoleAccess {
    administration: Arc<Administration>,
    whitelist_path: Arc<PathBuf>,
    online_mode: bool,
}
/// Host console can administer even when online identity services are down.
/// Admin operations always record both intent and final outcome durably.
struct ConsoleGameData {
    rules: Arc<Mutex<GameRules>>,
    rules_path: Arc<PathBuf>,
    scoreboard: Arc<Mutex<Scoreboard>>,
    scoreboard_path: Arc<PathBuf>,
}
fn start_clock_console(
    hub: Arc<Hub>,
    clock: Arc<Clock>,
    clock_path: Arc<PathBuf>,
    game_data: ConsoleGameData,
    world: Arc<Mutex<World>>,
    world_path: Arc<PathBuf>,
    access: ConsoleAccess,
) {
    let ConsoleGameData {
        rules,
        rules_path,
        scoreboard,
        scoreboard_path,
    } = game_data;
    let ConsoleAccess {
        administration,
        whitelist_path,
        online_mode,
    } = access;
    thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else {
                break;
            };
            if line.trim().is_empty() {
                continue;
            }
            if let Err(error) = administration.audit("CONSOLE", &line, "attempt") {
                eprintln!("ADMIN AUDIT UNAVAILABLE, operation refused: {error}");
                continue;
            }
            let args: Vec<_> = line.split_whitespace().collect();
            let result: io::Result<()> = match args.as_slice() {
                ["/gamerule", key] => (|| -> io::Result<()> {
                    let rules = rules.lock().map_err(|_| bad("game rules mutex poisoned"))?;
                    let value = rules.get(key).ok_or_else(|| bad("unsupported game rule"))?;
                    println!("{key} = {value}");
                    Ok(())
                })(),
                ["/gamerule", key, value] => (|| -> io::Result<()> {
                    let value = match *value {
                        "true" => true,
                        "false" => false,
                        _ => return Err(bad("gamerule value must be true or false")),
                    };
                    let mut rules = rules.lock().map_err(|_| bad("game rules mutex poisoned"))?;
                    let mut candidate = *rules;
                    candidate.set(key, value)?;
                    candidate.save(&rules_path)?;
                    *rules = candidate;
                    if *key == "doDaylightCycle" {
                        clock.set_daylight_cycle(value);
                    }
                    println!("{key} = {value}");
                    Ok(())
                })(),
                ["/scoreboard", operation @ ..] => (|| -> io::Result<()> {
                    let mut state = scoreboard
                        .lock()
                        .map_err(|_| bad("scoreboard lock poisoned"))?;
                    let mut candidate = state.clone();
                    let changes = match operation {
                        ["objectives", "add", name, criterion, display] => {
                            candidate.add_objective(name, display, criterion)?
                        }
                        ["objectives", "remove", name] => candidate.remove_objective(name)?,
                        ["objectives", "setdisplay", slot, name] => {
                            let slot = match *slot {
                                "list" => 0,
                                "sidebar" => 1,
                                "belowName" => 2,
                                _ => return Err(bad("invalid scoreboard display slot")),
                            };
                            candidate.set_display(
                                slot,
                                if *name == "clear" { None } else { Some(name) },
                            )?
                        }
                        ["players", "set", holder, objective, value] => {
                            let value = value
                                .parse::<i32>()
                                .map_err(|_| bad("invalid scoreboard score"))?;
                            candidate.set_score(holder, objective, value)?
                        }
                        ["players", "reset", holder] => candidate.reset_player_scores(holder)?,
                        ["teams", "remove", name] => candidate.remove_team(name)?,
                        ["teams", "leave", name, holder] => {
                            candidate.remove_player_from_team(name, holder)?
                        }
                        ["teams", "add", name, display] => candidate.add_team(name, display)?,
                        ["teams", "join", name, holder] => {
                            candidate.add_player_to_team(name, holder)?
                        }
                        _ => return Err(bad("unsupported scoreboard console command")),
                    };
                    candidate.save(&scoreboard_path)?;
                    *state = candidate;
                    drop(state);
                    for event in changes {
                        hub.broadcast(event);
                    }
                    Ok(())
                })(),
                ["/list"] => (|| -> io::Result<()> {
                    println!("Online ({}): {}", hub.count()?, hub.names()?.join(", "));
                    Ok(())
                })(),
                ["/save"] => (|| -> io::Result<()> {
                    world
                        .lock()
                        .map_err(|_| bad("world mutex poisoned"))?
                        .save(&world_path)?;
                    println!("World saved");
                    Ok(())
                })(),
                ["/time", "query", "daytime"] => {
                    println!("Daytime: {}", clock.day_time());
                    Ok(())
                }
                ["/time", "set", requested] => (|| -> io::Result<()> {
                    let requested = requested
                        .parse::<u64>()
                        .map_err(|_| bad("invalid daytime value"))?;
                    clock.set_day_time(requested)?;
                    clock.save_ticks(&clock_path)?;
                    let mut body = Vec::with_capacity(16);
                    body.extend_from_slice(&(clock.ticks() as i64).to_be_bytes());
                    body.extend_from_slice(&clock.day_time().to_be_bytes());
                    hub.broadcast(Event { id: 0x03, body });
                    println!("Daytime set to {}", clock.day_time());
                    Ok(())
                })(),
                ["/weather", "query"] => {
                    let (rain, thunder) = clock.weather();
                    println!("Weather: rain={rain} thunder={thunder}");
                    Ok(())
                }
                ["/weather", "set", kind] => (|| -> io::Result<()> {
                    clock.set_weather(kind)?;
                    clock.save_ticks(&clock_path)?;
                    for body in clock.weather_packets() {
                        hub.broadcast(Event { id: 0x2b, body });
                    }
                    println!("Weather set to {kind}");
                    Ok(())
                })(),
                ["/tp", name, x, y, z] => (|| -> io::Result<()> {
                    let parse = |value: &str| {
                        value
                            .parse::<f64>()
                            .map_err(|_| bad("invalid teleport coordinate"))
                    };
                    let position = PlayerPosition {
                        x: parse(x)?,
                        y: parse(y)?,
                        z: parse(z)?,
                    };
                    if !hub.request_admin_teleport(name, position)? {
                        return Err(io::Error::new(ErrorKind::NotFound, "target not online"));
                    }
                    println!("Teleport queued for {name}");
                    Ok(())
                })(),
                ["/op", name] => (|| -> io::Result<()> {
                    if online_mode {
                        let uuid = hub.uuid_for_online_name(name)?.ok_or_else(|| {
                            io::Error::new(
                                ErrorKind::NotFound,
                                "player must be authenticated and online to grant OP",
                            )
                        })?;
                        administration.set_op_verified(name, &uuid)?;
                    } else {
                        administration.set_op(name, true)?;
                    }
                    println!("OP record saved for {name}");
                    Ok(())
                })(),
                ["/deop", name] => administration.set_op(name, false),
                ["/ban", name, reason @ ..] => (|| -> io::Result<()> {
                    let reason = if reason.is_empty() {
                        "Banned by administrator".to_owned()
                    } else {
                        reason.join(" ")
                    };
                    let uuid = if online_mode {
                        hub.uuid_for_online_name(name)?
                    } else {
                        None
                    };
                    administration.set_player_ban_verified(
                        name,
                        uuid.as_deref(),
                        Some(&reason),
                        true,
                    )?;
                    let _ = hub.request_admin_kick(name, &reason)?;
                    println!("Banned {name}");
                    Ok(())
                })(),
                ["/pardon", name] => administration.set_player_ban(name, None, false),
                ["/ban-ip", address, reason @ ..] => (|| -> io::Result<()> {
                    let reason = if reason.is_empty() {
                        "IP banned by administrator".to_owned()
                    } else {
                        reason.join(" ")
                    };
                    administration.set_ip_ban(address, Some(&reason), true)?;
                    // Active sessions are rechecked within two seconds.
                    println!("Banned IP {address}");
                    Ok(())
                })(),
                ["/pardon-ip", address] => administration.set_ip_ban(address, None, false),
                ["/kick", name, reason @ ..] => (|| -> io::Result<()> {
                    let reason = if reason.is_empty() {
                        "Kicked by administrator".to_owned()
                    } else {
                        reason.join(" ")
                    };
                    if !hub.request_admin_kick(name, &reason)? {
                        return Err(io::Error::new(ErrorKind::NotFound, "target not online"));
                    }
                    println!("Kick queued for {name}");
                    Ok(())
                })(),
                ["/whitelist", "reload"] => (|| -> io::Result<()> {
                    let loaded = Whitelist::load(&whitelist_path)?;
                    println!(
                        "Whitelist validated: {} names; next login/session poll uses it",
                        loaded.len()
                    );
                    Ok(())
                })(),
                ["/whitelist", "list"] => (|| -> io::Result<()> {
                    let loaded = Whitelist::load(&whitelist_path)?;
                    println!("Whitelist: {} entries", loaded.len());
                    Ok(())
                })(),
                ["/permission", name, kind, state] => (|| -> io::Result<()> {
                    let enabled = match *state {
                        "allow" => true,
                        "deny" => false,
                        _ => return Err(bad("permission must be allow or deny")),
                    };
                    administration.set_permission(name, kind, enabled)?;
                    println!("Permission updated for {name}: {kind}={enabled}");
                    Ok(())
                })(),
                _ => Err(bad("unknown administrator console command")),
            };
            let outcome = if result.is_ok() { "success" } else { "failure" };
            if let Err(error) = administration.audit("CONSOLE", &line, outcome) {
                eprintln!("ADMIN AUDIT WRITE FAILURE: {error}");
            }
            if let Err(error) = result {
                eprintln!("ADMIN CONSOLE ERROR: {error}");
            }
        }
    });
}

fn main() -> io::Result<()> {
    // Explicit, offline-only create-if-missing recovery. No network listener,
    // no server.properties parsing, and no silent replacement of a live world.
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    if raw_args
        .first()
        .is_some_and(|arg| arg == "--recover-level-dat")
    {
        if raw_args.len() != 2 {
            return Err(bad("usage: --recover-level-dat STOPPED_WORLD_DIR"));
        }
        let world_directory = Path::new(&raw_args[1]);
        // Preflight is read-only. A corrupt backup or an existing level.dat
        // must not cause recovery to rewrite the world's session.lock.
        match std::fs::symlink_metadata(world_directory.join("level.dat")) {
            Ok(_) => {
                return Err(io::Error::new(
                    ErrorKind::AlreadyExists,
                    "level.dat already exists; manual recovery never overwrites it",
                ));
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let _ = level_dat::read_level_file(&world_directory.join("level.dat_old"))?;
        let mut lock = session_lock::WorldSessionLock::acquire(world_directory)?;
        level_dat::restore_missing_level(world_directory, &mut lock)?;
        println!(
            "Recovered a missing level.dat from validated level.dat_old in {}",
            world_directory.display()
        );
        return Ok(());
    }

    // Reading configuration never converts, modifies, or replaces a native
    // Minecraft world. Without server.properties old IRW CLI behavior stays.
    let properties = if std::env::args().any(|arg| {
        arg == "--help"
            || arg == "--backup-world"
            || arg == "--inspect-player-data"
            || arg == "--inspect-level-dat"
            || arg == "--inspect-anvil-region"
            || arg == "--inspect-anvil-world"
            || arg == "--inspect-anvil-chunk"
    }) {
        None
    } else {
        server_config::ServerProperties::load_optional(Path::new("server.properties"))?
    };
    let mut bind = properties
        .as_ref()
        .map(|config| config.bind)
        .unwrap_or_else(|| "127.0.0.1:25565".parse::<SocketAddr>().unwrap());
    let mut world_path = properties
        .as_ref()
        .map(|config| config.world_path.clone())
        .unwrap_or_else(|| PathBuf::from("world/flat.irw"));
    let mut chunk_inspect: Option<(PathBuf, dimensions::Dimension, i32, i32)> = None;
    let mut world_inspect_path: Option<PathBuf> = None;
    let mut region_inspect_path: Option<PathBuf> = None;
    let mut level_inspect_path: Option<PathBuf> = None;
    let mut player_inspect_path: Option<PathBuf> = None;
    let mut anvil_export_path: Option<PathBuf> = None;
    let mut anvil_import_path: Option<PathBuf> = None;
    let mut backup_destination: Option<PathBuf> = None;
    let mut explicit_world = false;
    let mut whitelist_path = PathBuf::from("whitelist.json");
    let mut insecure_offline_lan = false;
    let mut survival = properties.as_ref().is_some_and(|config| config.survival);
    let hardcore = properties.as_ref().is_some_and(|config| config.hardcore);
    let mut online_mode = properties.as_ref().is_some_and(|config| config.online_mode);
    let mut difficulty = properties.as_ref().map_or(2, |config| config.difficulty);
    let mut requested_seed: Option<u64> = None;
    let mut biome_seed: Option<u64> = None;
    let mut expanded_seed: Option<u64> = None;
    let mut expanded_dimension = dimensions::Dimension::Overworld;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" => {
                println!("Usage: vanilla-server [--bind IP:PORT] [--world PATH] [--whitelist FILE] [--insecure-offline-lan] [--online-mode] [--survival] [--recover-level-dat STOPPED_WORLD_DIR] [--creative] [--export-anvil NEW_DIR] [--import-anvil SOURCE_DIR] [--backup-world NEW_DIR] [--inspect-player-data FILE] [--inspect-level-dat FILE] [--inspect-anvil-region FILE] [--inspect-anvil-world DIR] [--inspect-anvil-chunk DIR overworld|nether|end CX CZ] [--terrain-seed U64] [--biome-seed U64] [--expanded-seed U64] [--dimension overworld|nether|end]");
                println!(
                    "Offline usernames can be impersonated; --online-mode validates Mojang accounts and requires encrypted Protocol 5 login."
                );
                return Ok(());
            }
            "--bind" => {
                bind = args
                    .next()
                    .ok_or_else(|| bad("missing --bind value"))?
                    .parse()
                    .map_err(|_| bad("invalid listen address"))?;
            }
            "--world" => {
                world_path = args
                    .next()
                    .ok_or_else(|| bad("missing --world value"))?
                    .into();
                explicit_world = true;
            }
            "--whitelist" => {
                whitelist_path = args
                    .next()
                    .ok_or_else(|| bad("missing --whitelist value"))?
                    .into()
            }
            "--insecure-offline-lan" => insecure_offline_lan = true,
            "--online-mode" => online_mode = true,
            "--survival" => survival = true,
            "--creative" => survival = false,
            "--difficulty" => {
                difficulty = args
                    .next()
                    .ok_or_else(|| bad("missing --difficulty value"))?
                    .parse::<u8>()
                    .map_err(|_| bad("invalid difficulty"))?;
                if difficulty > 3 {
                    return Err(bad("difficulty must be 0..=3"));
                }
            }
            "--terrain-seed" => {
                requested_seed = Some(
                    args.next()
                        .ok_or_else(|| bad("missing --terrain-seed value"))?
                        .parse()
                        .map_err(|_| bad("invalid terrain seed (expected unsigned integer)"))?,
                );
            }
            "--biome-seed" => {
                biome_seed = Some(
                    args.next()
                        .ok_or_else(|| bad("missing --biome-seed value"))?
                        .parse()
                        .map_err(|_| bad("invalid biome seed"))?,
                );
            }
            "--expanded-seed" => {
                expanded_seed = Some(
                    args.next()
                        .ok_or_else(|| bad("missing --expanded-seed value"))?
                        .parse()
                        .map_err(|_| bad("invalid expanded seed"))?,
                );
            }
            "--dimension" => {
                expanded_dimension = match args
                    .next()
                    .ok_or_else(|| bad("missing --dimension value"))?
                    .as_str()
                {
                    "overworld" => dimensions::Dimension::Overworld,
                    "nether" => dimensions::Dimension::Nether,
                    "end" => dimensions::Dimension::End,
                    _ => return Err(bad("invalid --dimension (overworld/nether/end)")),
                };
            }
            "--export-anvil" => {
                anvil_export_path = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| bad("missing --export-anvil directory"))?,
                ));
            }
            "--import-anvil" => {
                anvil_import_path = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| bad("missing --import-anvil directory"))?,
                ));
            }
            "--inspect-anvil-chunk" => {
                let root = PathBuf::from(
                    args.next()
                        .ok_or_else(|| bad("missing --inspect-anvil-chunk DIR"))?,
                );
                let dimension = match args.next().as_deref() {
                    Some("overworld") => dimensions::Dimension::Overworld,
                    Some("nether") => dimensions::Dimension::Nether,
                    Some("end") => dimensions::Dimension::End,
                    _ => return Err(bad("invalid Anvil inspection dimension")),
                };
                let cx = args
                    .next()
                    .ok_or_else(|| bad("missing Anvil chunk X"))?
                    .parse::<i32>()
                    .map_err(|_| bad("invalid Anvil chunk X"))?;
                let cz = args
                    .next()
                    .ok_or_else(|| bad("missing Anvil chunk Z"))?
                    .parse::<i32>()
                    .map_err(|_| bad("invalid Anvil chunk Z"))?;
                chunk_inspect = Some((root, dimension, cx, cz));
            }
            "--inspect-anvil-world" => {
                world_inspect_path = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| bad("missing --inspect-anvil-world DIR"))?,
                ));
            }
            "--inspect-anvil-region" => {
                region_inspect_path = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| bad("missing --inspect-anvil-region FILE"))?,
                ));
            }
            "--inspect-level-dat" => {
                level_inspect_path = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| bad("missing --inspect-level-dat FILE"))?,
                ));
            }
            "--inspect-player-data" => {
                player_inspect_path = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| bad("missing --inspect-player-data FILE"))?,
                ));
            }
            "--backup-world" => {
                backup_destination = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| bad("missing --backup-world directory"))?,
                ));
            }
            _ => return Err(bad("unknown command-line argument")),
        }
    }
    if hardcore && (!survival || difficulty != 3) {
        return Err(bad("hardcore requires survival mode and difficulty 3"));
    }
    if (requested_seed.is_some() || biome_seed.is_some() || expanded_seed.is_some())
        && (!survival
            || anvil_export_path.is_some()
            || anvil_import_path.is_some()
            || [
                requested_seed.is_some(),
                biome_seed.is_some(),
                expanded_seed.is_some(),
            ]
            .into_iter()
            .filter(|present| *present)
            .count()
                != 1)
    {
        return Err(bad(
            "specify exactly one seed type with --survival, without import/export",
        ));
    }
    if expanded_dimension != dimensions::Dimension::Overworld && expanded_seed.is_none() {
        return Err(bad("--dimension requires --expanded-seed"));
    }
    if anvil_export_path.is_some() && anvil_import_path.is_some() {
        return Err(bad(
            "--export-anvil and --import-anvil are mutually exclusive",
        ));
    }
    if let Some((root, dimension, cx, cz)) = chunk_inspect {
        if world_inspect_path.is_some()
            || region_inspect_path.is_some()
            || level_inspect_path.is_some()
            || player_inspect_path.is_some()
            || backup_destination.is_some()
            || anvil_import_path.is_some()
            || anvil_export_path.is_some()
        {
            return Err(bad(
                "Anvil chunk inspection cannot be combined with file operations",
            ));
        }
        let mut cache = legacy_chunk_store::ReadOnlyChunkCache::open(root, 16)?;
        match cache.get(dimension, cx, cz)? {
            Some(chunk) => println!(
                "Anvil 1.7.10 read-only chunk ({cx},{cz}) dimension={} sections={} entities={} tiles={}",
                dimension.name(),
                chunk.level.sections.len(),
                chunk.level.entities.len(),
                chunk.level.tile_entities.len()
            ),
            None => println!(
                "Anvil 1.7.10 chunk ({cx},{cz}) dimension={} not generated",
                dimension.name()
            ),
        }
        return Ok(());
    }
    if let Some(path) = world_inspect_path {
        if region_inspect_path.is_some()
            || level_inspect_path.is_some()
            || player_inspect_path.is_some()
            || backup_destination.is_some()
            || anvil_import_path.is_some()
            || anvil_export_path.is_some()
        {
            return Err(bad(
                "--inspect-anvil-world cannot be combined with another file operation",
            ));
        }
        let summary = legacy_world_inspect::inspect_world(&path)?;
        println!(
            "Anvil 1.7.10 world validated (read-only): {} dimension(s), {} region(s), {} chunk(s), {} player file(s), {} region byte(s)",
            summary.dimensions,
            summary.regions,
            summary.chunks,
            summary.player_files,
            summary.region_bytes
        );
        return Ok(());
    }
    if let Some(path) = region_inspect_path {
        if level_inspect_path.is_some()
            || player_inspect_path.is_some()
            || backup_destination.is_some()
            || anvil_import_path.is_some()
            || anvil_export_path.is_some()
        {
            return Err(bad(
                "Anvil region inspection cannot be combined with other file operations",
            ));
        }
        let count = legacy_region::inspect_region(&path)?;
        println!("1.7.10 Anvil region validated (read-only): {count} chunks");
        return Ok(());
    }
    if let Some(path) = level_inspect_path {
        if player_inspect_path.is_some()
            || backup_destination.is_some()
            || anvil_import_path.is_some()
            || anvil_export_path.is_some()
        {
            return Err(bad(
                "--inspect-level-dat cannot be combined with other file operations",
            ));
        }
        let level = level_dat::read_level_file(&path)?;
        println!(
            "Anvil 19133 level.dat verified (read-only): name={}, seed={}, time={}",
            level.data.name, level.data.seed, level.data.time
        );
        return Ok(());
    }
    if let Some(path) = player_inspect_path {
        if backup_destination.is_some()
            || anvil_import_path.is_some()
            || anvil_export_path.is_some()
        {
            return Err(bad(
                "--inspect-player-data cannot be combined with backup/import/export",
            ));
        }
        let (inventory, ender) = legacy_inventory_nbt::inspect_player(&path)?;
        println!("Legacy 1.7.10 playerdata verified (read-only): {inventory} inventory stacks, {ender} ender stacks");
        return Ok(());
    }
    if let Some(destination) = backup_destination {
        if !explicit_world
            || anvil_import_path.is_some()
            || anvil_export_path.is_some()
            || requested_seed.is_some()
            || biome_seed.is_some()
            || expanded_seed.is_some()
        {
            return Err(bad(
                "--backup-world requires --world FILE and no import/export/seed",
            ));
        }
        let dir = world_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| bad("--backup-world requires a world subdirectory"))?;
        if !world_path.is_file() && !dir.join("level.dat").is_file() {
            return Err(bad("backup source has no IRW file or vanilla level.dat"));
        }
        let mut lock = session_lock::WorldSessionLock::acquire(dir)?;
        let stats = backup_world::create(dir, &destination, &mut lock)?;
        println!(
            "World backup complete: {} files ({} bytes) at {}",
            stats.files,
            stats.bytes,
            destination.display()
        );
        return Ok(());
    }
    if let Some(source) = anvil_import_path {
        // Only import our own exported flat-world snapshots; never overwrite
        // an existing IRW1 world. This mode does not open a listening socket.
        let (columns, changed_columns) = anvil_import::import(&source, &world_path)?;
        println!(
            "Imported {columns} Anvil columns ({changed_columns} edited columns) into {}",
            world_path.display()
        );
        println!("This is a strict IRW1 conversion, not arbitrary Minecraft world compatibility.");
        return Ok(());
    }
    if let Some(destination) = anvil_export_path {
        // Export mode never binds a TCP socket and never modifies the IRW1 source.
        if !world_path.is_file() {
            return Err(io::Error::new(
                ErrorKind::NotFound,
                "source IRW1 world does not exist",
            ));
        }
        // An offline export cannot mix an IRW snapshot with live inventory
        // updates. Hold the same world lock as the native game server.
        let world_directory = world_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let mut export_lock = session_lock::WorldSessionLock::acquire(world_directory)?;
        export_lock.verify()?;
        for journal in [
            world_path.with_extension("pending-txn.json"),
            Chests::location(&world_path).with_extension("pending-txn.json"),
            Furnaces::location(&world_path).with_extension("pending-txn.json"),
        ] {
            if journal.exists() {
                return Err(bad(
                    "pending save journal: start server recovery before Anvil export",
                ));
            }
        }
        let world = World::load(&world_path)?;
        let count = anvil_export::export_with_storage(&world, &destination, &world_path)?;
        println!(
            "Anvil 1.7.10 snapshot exported: {count} chunks to {}",
            destination.display()
        );
        println!("This is a partial flat-world snapshot, not a completed survival world.");
        return Ok(());
    }
    // Even explicit --world must not place an IRW file inside a vanilla save.
    server_config::verify_legacy_storage(&world_path)?;
    if let Some(config) = properties.as_ref() {
        eprintln!(
            "server.properties: level-name={} (legacy IRW storage; Anvil is NOT enabled)",
            config.level_name
        );
    }
    if !bind.ip().is_loopback() && !insecure_offline_lan && !online_mode {
        return Err(io::Error::new(ErrorKind::PermissionDenied,
            "offline-mode whitelist cannot authenticate usernames: non-loopback bind requires explicit --insecure-offline-lan"));
    }
    let list = Whitelist::load(&whitelist_path)?;
    // Absolute --world paths are isolated deployments (also used by parallel
    // integration tests). Keep their admin files next to that world instead
    // of sharing mutable JSON files in the build/test working directory.
    let administration_root = if world_path.is_absolute() {
        world_path
            .parent()
            .ok_or_else(|| bad("world path has no parent"))?
            .to_path_buf()
    } else {
        PathBuf::from(".")
    };
    let administration = Arc::new(Administration::new(administration_root)?);
    println!(
        "Whitelist: {} permitted name(s) (NOT cryptographic identity authentication)",
        list.len()
    );
    // Acquire a 1.7.10-compatible session.lock before reading or creating
    // world state. The guard remains held until the server process exits.
    let world_directory = world_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut session_guard = session_lock::WorldSessionLock::acquire(world_directory)?;
    session_guard.verify()?;
    // Replay interrupted multi-file credits before loading any container or
    // accepting connections. Corrupt/unrecoverable logs prevent startup.
    journal::recover(&world_path.with_extension("pending-txn.json"))?;
    // Accept older independent log locations for one-time migration.
    journal::recover(&Chests::location(&world_path).with_extension("pending-txn.json"))?;
    journal::recover(&Furnaces::location(&world_path).with_extension("pending-txn.json"))?;
    let loaded_world = if world_path.exists() {
        let existing = World::load(&world_path)?;
        if let Some(seed) = requested_seed {
            if existing.natural_seed() != Some(seed)
                || existing.is_biome_world()
                || existing.extended_identity().is_some()
            {
                return Err(bad("existing world has different generation type or seed"));
            }
        }
        if let Some(seed) = biome_seed {
            if existing.natural_seed() != Some(seed)
                || !existing.is_biome_world()
                || existing.extended_identity().is_some()
            {
                return Err(bad("existing world is not a matching IRW3 biome world"));
            }
        }
        if let Some(seed) = expanded_seed {
            if existing.extended_identity() != Some((seed, expanded_dimension)) {
                return Err(bad(
                    "existing world is not a matching IRW4 seed and dimension",
                ));
            }
        }
        existing
    } else if let Some(seed) = expanded_seed {
        let initial = World::expanded(seed, expanded_dimension);
        initial.save_new(&world_path)?;
        initial
    } else if let Some(seed) = biome_seed {
        let initial = World::with_biomes(seed);
        initial.save_new(&world_path)?;
        initial
    } else if let Some(seed) = requested_seed {
        let initial = World::natural(seed);
        // Create-only: never replace an existing IRW1 world or another seed.
        initial.save_new(&world_path)?;
        initial
    } else {
        // Even a new flat world needs a durable identity so that its clock
        // snapshot belongs to a real, versioned IRW1 world after restart.
        let initial = World::new();
        initial.save_new(&world_path)?;
        initial
    };
    let clock_path = Arc::new(world_path.with_extension("clock"));
    let restored_clock = Clock::load_state(&clock_path)?;
    let (clock, _clock_thread) = Clock::start_from(restored_clock);
    let rules_path = Arc::new(GameRules::location(&world_path));
    let world_rules = Arc::new(Mutex::new(GameRules::load(&rules_path)?));
    let scoreboard_path = Arc::new(Scoreboard::location(&world_path));
    let shared_scoreboard = Arc::new(Mutex::new(Scoreboard::load(&scoreboard_path)?));
    clock.set_daylight_cycle(
        world_rules
            .lock()
            .map_err(|_| bad("game rules mutex poisoned"))?
            .daylight_cycle(),
    );
    let world = Arc::new(Mutex::new(loaded_world));
    let mobs = Arc::new(Mutex::new({
        let guard = world.lock().map_err(|_| bad("world mutex poisoned"))?;
        Mobs::load_or_new(
            &guard,
            survival,
            difficulty,
            &world_path.with_extension("mobs.json"),
        )?
    }));
    let throwables = Arc::new(Mutex::new(Throwables::default()));
    let boats = Arc::new(Mutex::new(Boats::default()));
    let minecarts = Arc::new(Mutex::new(Minecarts::default()));
    let fishing = Arc::new(Mutex::new(Fishing::default()));
    let ground_path = Arc::new(GroundItems::location(&world_path));
    let ground_items = Arc::new(Mutex::new(if survival {
        GroundItems::load(&ground_path)?
    } else {
        GroundItems::default()
    }));
    let chest_path = Arc::new(Chests::location(&world_path));
    let chests = Arc::new(Mutex::new(Chests::load(&chest_path)?));
    let furnace_path = Arc::new(Furnaces::location(&world_path));
    let furnaces = Arc::new(Mutex::new(Furnaces::load(&furnace_path)?));
    let hopper_path = Arc::new(Hoppers::location(&world_path));
    let hoppers = Arc::new(Mutex::new(Hoppers::load(&hopper_path)?));
    let device_path = Arc::new(Devices::location(&world_path));
    let devices = Arc::new(Mutex::new(Devices::load(&device_path)?));
    let brewer_path = Arc::new(Brewers::location(&world_path));
    let brewers = Arc::new(Mutex::new(Brewers::load(&brewer_path)?));
    let listener = TcpListener::bind(bind)?;
    println!(
        "IndustrialRust1710: listening on {} (1.7.10 vanilla, up to 8 players)",
        listener.local_addr()?
    );
    println!("WARNING: offline-mode has no account authentication. Do not expose to the internet.");
    let hub = Arc::new(Hub::default());
    let connections = Arc::new(AtomicUsize::new(0));
    let limiter = Arc::new(ConnectionLimiter::default());
    let path = Arc::new(world_path);
    let allow_path = Arc::new(whitelist_path);
    start_clock_console(
        Arc::clone(&hub),
        Arc::clone(&clock),
        Arc::clone(&clock_path),
        ConsoleGameData {
            rules: Arc::clone(&world_rules),
            rules_path: Arc::clone(&rules_path),
            scoreboard: Arc::clone(&shared_scoreboard),
            scoreboard_path: Arc::clone(&scoreboard_path),
        },
        Arc::clone(&world),
        Arc::clone(&path),
        ConsoleAccess {
            administration: Arc::clone(&administration),
            whitelist_path: Arc::clone(&allow_path),
            online_mode,
        },
    );
    // Furnace simulation is driven by the same monotonic 20Hz clock as the
    // server world. It runs without connected players; it never replays
    // wall-clock downtime after the process was stopped.
    let furnace_worker = Arc::clone(&furnaces);
    let furnace_file = Arc::clone(&furnace_path);
    let furnace_clock = Arc::clone(&clock);
    let clock_snapshot_path = Arc::clone(&clock_path);
    let furnace_world = Arc::clone(&world);
    let furnace_world_path = Arc::clone(&path);
    let furnace_hub = Arc::clone(&hub);
    let leaf_drops = Arc::clone(&ground_items);
    let leaf_drops_file = Arc::clone(&ground_path);
    // Keep the world -> mob lock ordering used by player sessions.
    // TNT blast damage is applied by the *existing* 20TPS world worker.
    let tnt_mobs = Arc::clone(&mobs);
    let ejection_devices = Arc::clone(&devices);
    let ejection_file = Arc::clone(&device_path);
    thread::spawn(move || {
        let mut telemetry =
            telemetry::TickTelemetry::new(furnace_world_path.with_extension("metrics.jsonl"));
        let mut last = furnace_clock.ticks();
        let mut last_clock_snapshot = last;
        let mut last_weather = furnace_clock.weather();
        loop {
            let now = furnace_clock.ticks();
            if now > last {
                let worker_started = Instant::now();
                let steps = (now - last).min(20) as u32;
                let missed = (now - last).saturating_sub(u64::from(steps));
                last = now;
                // One small durable write per second, with no replay of
                // elapsed wall-clock downtime after an unexpected stop.
                if now.saturating_sub(last_clock_snapshot) >= 20 {
                    match furnace_clock.save_ticks(&clock_snapshot_path) {
                        Ok(()) => last_clock_snapshot = now,
                        Err(error) => eprintln!("CLOCK SNAPSHOT ERROR: {error}"),
                    }
                }
                let weather = furnace_clock.weather();
                if weather != last_weather {
                    for body in furnace_clock.weather_packets() {
                        furnace_hub.broadcast(Event { id: 0x2b, body });
                    }
                    last_weather = weather;
                }
                let lighting = {
                    let mut storage = match furnace_worker.lock() {
                        Ok(v) => v,
                        Err(_) => break,
                    };
                    if let Err(error) = storage.advance(&furnace_file, steps, now) {
                        eprintln!("FURNACE TICK STORAGE ERROR: {error}");
                        // Do not simulate more operations without safe persistence.
                        break;
                    }
                    storage.lighting()
                };
                // Only rewrite a block when its lit state changes. Keeping the
                // furnace mutex released avoids a lock cycle with player actions.
                if now % 20 < u64::from(steps) || now < 3 {
                    let mut world = match furnace_world.lock() {
                        Ok(v) => v,
                        Err(_) => break,
                    };
                    for (pos, lit) in lighting {
                        let actual = world.get(pos.0, pos.1, pos.2) >> 4;
                        if actual == 61 || actual == 62 {
                            let target: u16 = if lit { 62 } else { 61 };
                            if actual != target {
                                match world.update_and_save(
                                    &furnace_world_path,
                                    pos.0,
                                    pos.1,
                                    pos.2,
                                    target << 4,
                                ) {
                                    Ok(true) => furnace_hub
                                        .broadcast(block_change_event(&world, pos.0, pos.1, pos.2)),
                                    Ok(false) => {}
                                    Err(error) => eprintln!("FURNACE LIGHT BLOCK ERROR: {error}"),
                                }
                            }
                        }
                    }
                }
                // Gravity uses the same monotonic 20Hz tick clock.
                // Advance at most 20 accumulated ticks after a stalled worker,
                // avoiding unbounded catch-up or a world-wide block scan.
                let mut gravity_world = match furnace_world.lock() {
                    Ok(v) => v,
                    Err(_) => break,
                };
                // Fire uses the same clock/weather source as client rain packets.
                gravity_world.set_raining(weather.0);
                gravity_world.set_day_time(furnace_clock.day_time());
                for _ in 0..steps {
                    match gravity_world.gravity_tick(&furnace_world_path, 64) {
                        Ok(changes) => {
                            for movement in changes {
                                for (x, y, z) in [movement.from, movement.to] {
                                    furnace_hub.broadcast(block_change_event(
                                        &gravity_world,
                                        x,
                                        y,
                                        z,
                                    ));
                                }
                            }
                        }
                        Err(error) => {
                            eprintln!("GRAVITY TICK STORAGE ERROR: {error}");
                            return;
                        }
                    }
                    // Snapshot only when TNT's fuse expires. Exposure must be
                    // sampled before the explosion clears intervening walls.
                    let tnt_exposure = gravity_world
                        .tnt_detonates_next_tick()
                        .then(|| gravity_world.clone());
                    // Queued fluid and redstone updates use the same tick and
                    // save all staged block changes with one world-file commit.
                    match gravity_world.block_tick(&furnace_world_path, 64) {
                        Ok(changed) => {
                            for action in gravity_world.drain_piston_actions() {
                                furnace_hub.broadcast(piston_action_event(action));
                            }
                            for (x, y, z) in changed {
                                furnace_hub.broadcast(block_change_event(&gravity_world, x, y, z));
                            }
                            // Execute each persisted rising-edge exactly once.
                            // Both inventory debit and spawned ground entity are
                            // redo-journaled before either state is published.
                            for pos in gravity_world.drain_device_activations() {
                                let state = gravity_world.get(pos.0, pos.1, pos.2);
                                if !matches!(state >> 4, 23 | 158) || state & 8 == 0 {
                                    continue;
                                }
                                let offset = match state & 7 {
                                    0 => (0, -1, 0),
                                    1 => (0, 1, 0),
                                    2 => (0, 0, -1),
                                    3 => (0, 0, 1),
                                    4 => (-1, 0, 0),
                                    _ => (1, 0, 0),
                                };
                                let dest = (pos.0 + offset.0, pos.1 + offset.1, pos.2 + offset.2);
                                if !world::in_bounds(dest.0, dest.1, dest.2) {
                                    continue;
                                }
                                let mut storage = match ejection_devices.lock() {
                                    Ok(value) => value,
                                    Err(_) => return,
                                };
                                let mut ground = match leaf_drops.lock() {
                                    Ok(value) => value,
                                    Err(_) => return,
                                };
                                if ground.is_full() {
                                    continue;
                                }
                                let seed = now
                                    ^ (pos.0 as u64).wrapping_mul(31)
                                    ^ (pos.2 as u64).wrapping_mul(131);
                                let Some((updated, stack)) = (match storage.stage_eject(pos, seed) {
                                    Ok(result) => result,
                                    Err(error) => {
                                        eprintln!("DEVICE EJECTION ERROR: {error}");
                                        return;
                                    }
                                }) else {
                                    continue;
                                };
                                let (next_ground, spawned) =
                                    match ground.stage_put_many(&[stack], dest) {
                                        Ok(result) => result,
                                        Err(error) => {
                                            eprintln!("DEVICE GROUND ITEM ERROR: {error}");
                                            return;
                                        }
                                    };
                                let device_bytes = match updated.encoded() {
                                    Ok(bytes) => bytes,
                                    Err(error) => {
                                        eprintln!("DEVICE INVENTORY ENCODING ERROR: {error}");
                                        return;
                                    }
                                };
                                let ground_bytes = match next_ground.encoded() {
                                    Ok(bytes) => bytes,
                                    Err(error) => {
                                        eprintln!("DEVICE ITEM ENCODING ERROR: {error}");
                                        return;
                                    }
                                };
                                if let Err(error) = journal::commit(
                                    &furnace_world_path.with_extension("pending-txn.json"),
                                    &[
                                        (&*ejection_file, device_bytes),
                                        (&*leaf_drops_file, ground_bytes),
                                    ],
                                ) {
                                    eprintln!("DEVICE EJECTION JOURNAL ERROR: {error}");
                                    return;
                                }
                                *storage = updated;
                                *ground = next_ground;
                                drop(ground);
                                drop(storage);
                                for item in spawned {
                                    for packet in item.spawn() {
                                        furnace_hub.broadcast(packet);
                                    }
                                }
                            }
                            // Spawn and remove primed TNT entities only after
                            // their block changes were durably committed.
                            let mut tnt_explosions = Vec::new();
                            for tnt in gravity_world.drain_tnt_events() {
                                match tnt {
                                    world::TntEvent::Spawn {
                                        entity_id,
                                        pos: (x, y, z),
                                    } => {
                                        let mut body = Vec::new();
                                        write_varint(entity_id as u32, &mut body);
                                        body.push(50); // Protocol 5 Spawn Object: primed TNT
                                        for coordinate in [x, y, z] {
                                            body.extend_from_slice(
                                                &(coordinate * 32 + 16).to_be_bytes(),
                                            );
                                        }
                                        body.extend_from_slice(&[0, 0]);
                                        body.extend_from_slice(&0i32.to_be_bytes());
                                        furnace_hub.broadcast(Event { id: 0x0e, body });
                                    }
                                    world::TntEvent::Explode {
                                        entity_id,
                                        pos: (x, y, z),
                                    } => {
                                        tnt_explosions.push(mobs::Explosion {
                                            position: PlayerPosition {
                                                x: f64::from(x) + 0.5,
                                                y: f64::from(y) + 0.5,
                                                z: f64::from(z) + 0.5,
                                            },
                                            radius: 4.0,
                                        });
                                        furnace_hub.broadcast(multiplayer::destroy(entity_id));
                                        let mut body = Vec::new();
                                        for coordinate in [x, y, z] {
                                            body.extend_from_slice(
                                                &(coordinate as f32 + 0.5).to_be_bytes(),
                                            );
                                        }
                                        body.extend_from_slice(&4f32.to_be_bytes());
                                        body.extend_from_slice(&0i32.to_be_bytes());
                                        for _ in 0..3 {
                                            body.extend_from_slice(&0f32.to_be_bytes());
                                        }
                                        furnace_hub.broadcast(Event { id: 0x27, body });
                                    }
                                }
                            }
                            // Reuse the same explosion exposure, Mob HP and
                            // per-player pending-damage path as creeper blasts.
                            // This prevents a TNT graphic from being harmless
                            // to all living entities. Entity state is persisted
                            // before broadcasting Mob health/death packets.
                            if !tnt_explosions.is_empty() {
                                let participants = match furnace_hub.participants() {
                                    Ok(value) => value,
                                    Err(error) => {
                                        eprintln!("TNT PLAYER SNAPSHOT ERROR: {error}");
                                        return;
                                    }
                                };
                                let mut creatures = match tnt_mobs.lock() {
                                    Ok(value) => value,
                                    Err(_) => return,
                                };
                                let exposure = tnt_exposure.as_ref().unwrap_or(&gravity_world);
                                let packets = creatures.queue_explosion_hits(
                                    exposure,
                                    &tnt_explosions,
                                    &participants,
                                );
                                if let Err(error) = creatures.save(&gravity_world) {
                                    eprintln!("TNT MOB SAVE ERROR: {error}");
                                    return;
                                }
                                drop(creatures);
                                for packet in packets {
                                    furnace_hub.broadcast(packet);
                                }
                            }
                            let decayed = gravity_world.drain_drops();
                            if !decayed.is_empty() {
                                let mut ground = match leaf_drops.lock() {
                                    Ok(value) => value,
                                    Err(_) => return,
                                };
                                for fallen in decayed {
                                    if ground.is_full() {
                                        break;
                                    }
                                    match ground.put(&leaf_drops_file, fallen.stack, fallen.pos) {
                                        Ok(item) => {
                                            for packet in item.spawn() {
                                                furnace_hub.broadcast(packet);
                                            }
                                        }
                                        Err(error) => {
                                            eprintln!("LEAF DROP STORAGE ERROR: {error}");
                                            return;
                                        }
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            eprintln!("BLOCK TICK STORAGE ERROR: {error}");
                            return;
                        }
                    }
                }
                for (cx, cz) in gravity_world.drain_light_chunks(32) {
                    furnace_hub.broadcast(chunk_relight_event(cx, cz));
                }
                drop(gravity_world);
                telemetry.record(
                    worker_started.elapsed(),
                    steps,
                    missed,
                    furnace_hub.count().unwrap_or(0),
                );
            }
            thread::sleep(Duration::from_millis(10));
        }
    });
    // Hopper movement is an independent bounded job on the same 20Hz clock.
    // Only a single 8-tick transfer cycle is processed after any scheduler stall.
    let hopper_worker = Arc::clone(&hoppers);
    let hopper_file = Arc::clone(&hopper_path);
    let hopper_clock = Arc::clone(&clock);
    let hopper_world = Arc::clone(&world);
    let hopper_chests = Arc::clone(&chests);
    let hopper_chest_file = Arc::clone(&chest_path);
    let hopper_furnaces = Arc::clone(&furnaces);
    let hopper_furnace_file = Arc::clone(&furnace_path);
    let hopper_world_path = Arc::clone(&path);
    thread::spawn(move || {
        let mut last_cycle = hopper_clock.ticks() / 8;
        loop {
            let cycle = hopper_clock.ticks() / 8;
            if cycle > last_cycle {
                last_cycle = cycle;
                let tick_result = (|| -> io::Result<()> {
                    let world = hopper_world
                        .lock()
                        .map_err(|_| bad("world lock poisoned"))?;
                    let mut chests = hopper_chests
                        .lock()
                        .map_err(|_| bad("chest lock poisoned"))?;
                    let mut furnaces = hopper_furnaces
                        .lock()
                        .map_err(|_| bad("furnace lock poisoned"))?;
                    let mut hoppers = hopper_worker
                        .lock()
                        .map_err(|_| bad("hopper lock poisoned"))?;
                    hoppers.advance(
                        &world,
                        &mut chests,
                        &hopper_chest_file,
                        &mut furnaces,
                        &hopper_furnace_file,
                        &hopper_file,
                        &hopper_world_path.with_extension("pending-txn.json"),
                    )?;
                    Ok(())
                })();
                if let Err(error) = tick_result {
                    eprintln!("HOPPER TRANSFER ERROR: {error}; stopping transfers");
                    break;
                }
            }
            thread::sleep(Duration::from_millis(10));
        }
    });
    // Fuel-free 1.7.10 brewing progresses at the shared monotonic 20Hz rate.
    let brewer_worker = Arc::clone(&brewers);
    let brewer_file = Arc::clone(&brewer_path);
    let brewer_clock = Arc::clone(&clock);
    thread::spawn(move || {
        let mut last = brewer_clock.ticks();
        loop {
            let now = brewer_clock.ticks();
            if now > last {
                let steps = (now - last).min(20) as u32;
                last = now;
                let result = brewer_worker
                    .lock()
                    .map_err(|_| bad("brewer lock poisoned"))
                    .and_then(|mut stands| stands.advance(&brewer_file, steps));
                if let Err(error) = result {
                    eprintln!("BREWING TICK STORAGE ERROR: {error}");
                    break;
                }
            }
            thread::sleep(Duration::from_millis(10));
        }
    });
    // Ephemeral thrown entities have a separate bounded, shared 20TPS
    // physics loop and do not block Mob AI session locks during fan-out.
    let projectile_state = Arc::clone(&throwables);
    let projectile_world = Arc::clone(&world);
    let projectile_clock = Arc::clone(&clock);
    let projectile_hub = Arc::clone(&hub);
    let projectile_mobs = Arc::clone(&mobs);
    let projectile_boats = Arc::clone(&boats);
    let projectile_carts = Arc::clone(&minecarts);
    let projectile_fishing = Arc::clone(&fishing);
    thread::spawn(move || {
        let mut last = projectile_clock.ticks();
        loop {
            let now = projectile_clock.ticks();
            if now > last {
                let begin = last.saturating_add(1);
                last = now;
                let world = match projectile_world.lock() {
                    Ok(world) => world,
                    Err(_) => return,
                };
                let mut objects = match projectile_state.lock() {
                    Ok(objects) => objects,
                    Err(_) => return,
                };
                let mut actions = throwables::TickResult::default();
                for tick in begin.max(now.saturating_sub(19))..=now {
                    let step = objects.tick(&world, tick);
                    actions.events.extend(step.events);
                    actions.pearl_teleports.extend(step.pearl_teleports);
                    actions.egg_hatches.extend(step.egg_hatches);
                }
                drop(objects);
                let boat_events = match projectile_boats.lock() {
                    Ok(mut state) => state.tick(&world, now),
                    Err(_) => return,
                };
                let minecart_events = match projectile_carts.lock() {
                    Ok(mut state) => state.tick(&world, now),
                    Err(_) => return,
                };
                let fishing_events = match projectile_fishing.lock() {
                    Ok(mut state) => state.tick(&world, now),
                    Err(_) => return,
                };
                for at in actions.egg_hatches {
                    if let Ok(mut mobs) = projectile_mobs.lock() {
                        if let Some(entity) = mobs.hatch_egg(&world, at) {
                            projectile_hub.broadcast_in_view(at, entity);
                        }
                    }
                }
                drop(world);
                for (at, packet) in actions
                    .events
                    .into_iter()
                    .chain(boat_events)
                    .chain(minecart_events)
                    .chain(fishing_events)
                {
                    projectile_hub.broadcast_in_view(at, packet);
                }
                for (owner, destination) in actions.pearl_teleports {
                    if let Err(error) = projectile_hub.request_pearl_teleport(&owner, destination) {
                        eprintln!("ENDER PEARL TELEPORT ERROR: {error}");
                    }
                }
            }
            thread::sleep(Duration::from_millis(10));
        }
    });
    // Initialize once at startup. A failure here prevents any online-mode bind.
    let server_auth = if online_mode {
        Some(Arc::new(ServerIdentity::generate()?))
    } else {
        None
    };
    for accepted in listener.incoming() {
        let stream = accepted?;
        let peer_ip = stream.peer_addr()?.ip();
        // Rate limit before accessing disk so repeated banned connections cannot
        // turn into an unlimited JSON parsing workload.
        let Some(ip_permit) = limiter.acquire(peer_ip) else {
            continue;
        };
        // Corrupt or missing BAN databases fail closed before thread creation.
        if administration.ip_banned(peer_ip).unwrap_or(true) {
            continue;
        }
        if connections.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
            connections.fetch_sub(1, Ordering::AcqRel);
            continue;
        }
        let admission = ConnectionGuard(Arc::clone(&connections));
        let clients = Arc::clone(&hub);
        let w = Arc::clone(&world);
        let p = Arc::clone(&path);
        let wl = Arc::clone(&allow_path);
        let admin = Arc::clone(&administration);
        let auth_key = server_auth.clone();
        let game_clock = Arc::clone(&clock);
        let drops = Arc::clone(&ground_items);
        let drops_path = Arc::clone(&ground_path);
        let mob_state = Arc::clone(&mobs);
        let thrown_state = Arc::clone(&throwables);
        let boat_state = Arc::clone(&boats);
        let minecart_state = Arc::clone(&minecarts);
        let fishing_state = Arc::clone(&fishing);
        let storage = Arc::clone(&chests);
        let storage_path = Arc::clone(&chest_path);
        let machine_storage = Arc::clone(&furnaces);
        let machine_file = Arc::clone(&furnace_path);
        let hopper_storage = Arc::clone(&hoppers);
        let hopper_file = Arc::clone(&hopper_path);
        let device_storage = Arc::clone(&devices);
        let device_file = Arc::clone(&device_path);
        let brewer_storage = Arc::clone(&brewers);
        let brewer_file = Arc::clone(&brewer_path);
        let rules = Arc::clone(&world_rules);
        let board = Arc::clone(&shared_scoreboard);
        let board_path = Arc::clone(&scoreboard_path);
        thread::spawn(move || {
            let _connection = admission;
            if let Err(e) = handle(
                stream,
                ServerContext {
                    hub: clients,
                    world: w,
                    path: p,
                    whitelist_path: wl,
                    administration: admin,
                    clock: game_clock,
                    ground_items: drops,
                    ground_path: drops_path,
                    mobs: mob_state,
                    throwables: thrown_state,
                    boats: boat_state,
                    minecarts: minecart_state,
                    fishing: fishing_state,
                    chests: storage,
                    chest_path: storage_path,
                    furnaces: machine_storage,
                    furnace_path: machine_file,
                    hoppers: hopper_storage,
                    hopper_path: hopper_file,
                    devices: device_storage,
                    device_path: device_file,
                    brewers: brewer_storage,
                    brewer_path: brewer_file,
                    survival,
                    hardcore,
                    difficulty,
                    game_rules: rules,
                    scoreboard: board,
                    scoreboard_path: board_path,
                    online_mode,
                    auth_key,
                },
                ip_permit,
            ) {
                if e.kind() != ErrorKind::UnexpectedEof {
                    eprintln!("client disconnected: {e}");
                }
            }
        });
    }
    Ok(())
}
#[cfg(test)]
mod h_special_item_transaction_tests {
    use super::*;

    #[test]
    fn cauldron_and_inventory_commit_as_one_recoverable_action() {
        let root =
            std::env::temp_dir().join(format!("ir1710-h-transaction-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let world_path = root.join("world.irw");
        let inventory_path = root.join("player.inventory.json");
        let mut world = World::new();
        let mut inventory = Inventory::default();
        world
            .update_and_save(&world_path, 1, 65, 1, 118 << 4)
            .unwrap();
        inventory
            .creative_set(
                36,
                Some(Stack {
                    id: 326,
                    count: 1,
                    damage: 0,
                }),
            )
            .unwrap();
        inventory.save(&inventory_path).unwrap();
        let changed = use_special_item(
            &mut world,
            &world_path,
            &mut inventory,
            &inventory_path,
            true,
            (1, 65, 1),
        )
        .unwrap()
        .unwrap();
        assert_eq!(changed, vec![(1, 65, 1)]);
        let reloaded_world = World::load(&world_path).unwrap();
        let reloaded_inventory = Inventory::load(&inventory_path).unwrap();
        assert_eq!(reloaded_world.get(1, 65, 1) & 15, 3);
        assert_eq!(reloaded_inventory.held().unwrap().id, 325);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bonemeal_noop_does_not_debit_items_or_write_a_world() {
        let root =
            std::env::temp_dir().join(format!("ir1710-h-bonemeal-noop-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let world_path = root.join("world.irw");
        let inventory_path = root.join("player.inventory.json");
        let mut world = World::new();
        let mut inventory = Inventory::default();
        world
            .update_and_save(&world_path, 1, 65, 1, 1 << 4)
            .unwrap();
        inventory
            .creative_set(
                36,
                Some(Stack {
                    id: 351,
                    count: 2,
                    damage: 15,
                }),
            )
            .unwrap();
        inventory.save(&inventory_path).unwrap();
        let changed = use_special_item(
            &mut world,
            &world_path,
            &mut inventory,
            &inventory_path,
            true,
            (1, 65, 1),
        )
        .unwrap()
        .unwrap();
        assert!(changed.is_empty());
        assert_eq!(inventory.held().unwrap().count, 2);
        assert_eq!(
            Inventory::load(&inventory_path)
                .unwrap()
                .held()
                .unwrap()
                .count,
            2
        );
        assert_eq!(World::load(&world_path).unwrap().get(1, 65, 1), 1 << 4);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod piston_action_wire_tests {
    use super::*;

    #[test]
    fn protocol5_piston_action_uses_legacy_coordinates() {
        let packet = piston_action_event(world::PistonAction {
            pos: (123, 65, -40),
            block_id: 33,
            facing: 5,
            extending: true,
        });
        assert_eq!(packet.id, 0x24);
        assert_eq!(&packet.body[0..4], &123i32.to_be_bytes());
        assert_eq!(&packet.body[4..6], &65i16.to_be_bytes());
        assert_eq!(&packet.body[6..10], &(-40i32).to_be_bytes());
        assert_eq!(&packet.body[10..], &[0, 5, 33]);
    }
}

#[cfg(test)]
mod sprint_action_tests {
    use super::sprint_action;

    #[test]
    fn protocol5_sprint_starts_only_with_food_above_six() {
        assert!(!sprint_action(false, 3, 6));
        assert!(sprint_action(false, 3, 7));
        assert!(sprint_action(true, 0, 20));
        assert!(!sprint_action(true, 4, 20));
    }
}

#[cfg(test)]
mod effect_interval_tests {
    use super::periodic_effect_interval;

    #[test]
    fn amplification_speeds_up_legacy_regeneration_and_poison() {
        assert_eq!(periodic_effect_interval(10, 0), 50);
        assert_eq!(periodic_effect_interval(10, 1), 25);
        assert_eq!(periodic_effect_interval(10, 2), 12);
        assert_eq!(periodic_effect_interval(19, 0), 25);
        assert_eq!(periodic_effect_interval(19, 1), 12);
        assert_eq!(periodic_effect_interval(19, 2), 6);
        assert_eq!(periodic_effect_interval(19, u8::MAX), 1);
        assert_eq!(periodic_effect_interval(1, 0), u64::MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn golden_protocol5_empty_window_cursor_and_chunk_layout() {
        // S30 Window Items: unsigned window ID, signed short slot count,
        // followed by 45 slots with i16(-1) absent-item sentinels.
        let inventory = Inventory::default();
        let mut expected = vec![0, 0, 45];
        expected.extend(std::iter::repeat_n(0xff, 90));
        assert_eq!(inventory.window_items(), expected);
        // S2F Set Slot for the cursor is window=-1, slot=-1, empty item.
        assert_eq!(inventory.cursor_packet(), [0xff; 5]);

        // S21 flat-world chunk: sections 3+4 with block IDs, nibble
        // metadata, both light channels, then 256 biome bytes.
        let (mask, payload) = World::new().chunk_data(0, 0);
        assert_eq!(mask, 0x0018);
        assert_eq!(payload.len(), 8192 + 4096 + 8192 + 256);
        assert_eq!(&payload[payload.len() - 256..], &[1u8; 256]);
    }
    #[test]
    fn protocol5_slot_nbt_length_is_validated() {
        let empty = [-1i16; 1]
            .into_iter()
            .flat_map(i16::to_be_bytes)
            .collect::<Vec<_>>();
        assert!(Input(&empty).slot().unwrap().is_none());
        // Item id=1, count=1, damage=0, NBT=-1 (no tag).
        let mut none = Input(&[0, 1, 1, 0, 0, 255, 255]);
        assert_eq!(
            none.slot().unwrap(),
            Some(Stack {
                id: 1,
                count: 1,
                damage: 0
            })
        );
        none.finish().unwrap();
        // An NBT-bearing client item must not be accepted as a bare stack.
        assert!(Input(&[0, 1, 1, 0, 0, 0, 3, 0, 0, 0]).slot().is_err());
    }
    #[test]
    fn click_window_ignores_untrusted_nbt_snapshot_without_disconnect() {
        // 1.7.10 packet: item id, count, damage, signed NBT bytes length,
        // NBT payload. It must be fully consumed but never become server state.
        let mut snapshot = Input(&[0, 1, 1, 0, 0, 0, 3, 10, 0, 0]);
        snapshot.skip_untrusted_slot().unwrap();
        snapshot.finish().unwrap();

        // A zero/invalid item ID claim is not inventory authority.
        let mut fake = Input(&[0x7f, 0x7f, 0xff, 0, 0, 0xff, 0xff]);
        fake.skip_untrusted_slot().unwrap();
        fake.finish().unwrap();

        let mut truncated = Input(&[0, 1, 1, 0, 0, 0, 4, 10, 0, 0]);
        assert!(truncated.skip_untrusted_slot().is_err());
        let mut illegal_length = Input(&[0, 1, 1, 0, 0, 0xff, 0xfe]);
        assert!(illegal_length.skip_untrusted_slot().is_err());

        // Creative inventory still rejects tagged stacks instead of silently
        // stripping names/enchantments and minting an incorrect item.
        assert!(Input(&[0, 1, 1, 0, 0, 0, 3, 10, 0, 0]).slot().is_err());
    }
    #[test]
    fn offline_uuid_is_deterministic() {
        assert_eq!(offline_uuid("Notch"), offline_uuid("Notch"));
        assert_ne!(offline_uuid("Notch"), offline_uuid("Herobrine"));
        assert_eq!(&offline_uuid("Notch")[14..15], "3");
    }
    #[test]
    fn negative_coordinate_and_view_window() {
        let zero = view_chunks((0, 0));
        let shifted = view_chunks((3, 0));
        assert_eq!(zero.len(), 25);
        assert_eq!(shifted.len(), 25);
        assert_eq!(zero.difference(&shifted).count(), 15);
        assert_eq!(shifted.difference(&zero).count(), 15);
        let player = PlayerPosition {
            x: -0.5,
            y: 65.0,
            z: -16.1,
        };
        assert_eq!(chunk_center(&player), (-1, -2));
        assert_eq!(view_chunks((MIN_CHUNK, MIN_CHUNK)).len(), 9);
    }
    #[test]
    fn parses_bounds_and_slot() {
        assert!(Input(&[2, b'a', b'b']).string(1).is_err());
        let mut input = Input(&[0xff, 0xff]);
        assert!(input.slot().unwrap().is_none());
        input.finish().unwrap();
    }
}
