//! Vanilla-only Minecraft 1.7.10 / Protocol 5 server proof of concept.
//! Offline-mode by design: bind to loopback unless you trust every client on your LAN.
//! This is NOT a Forge server, public-server auth, survival engine or Anvil implementation.
mod achievements;
mod admin;
mod admin_state;
mod anvil_export;
mod anvil_import;
mod backup_rotation;
mod backup_world;
mod books;
mod brewing;
mod chests;
mod chunk_pipeline;
mod clock;
mod crafting;
mod decorations;
mod dimensions;
mod dispensers;
mod enchanting;
mod experience;
mod fishing;
mod furnaces;
mod game_rules;
mod ground_items;
mod hanging;
mod hoppers;
mod inventory;
mod item_nbt;
mod items_1710;
mod journal;
#[allow(dead_code)] // E46: opt-in block-to-Anvil projection; live save switch gated.
mod legacy_chunk_patch;
mod legacy_chunk_store;
mod legacy_entity_nbt;
mod legacy_inventory_nbt;
mod legacy_nbt;
mod legacy_region;
#[allow(dead_code)] // FS08 offline copy-on-write stage; live Anvil writes disabled.
mod legacy_region_rewrite;
#[allow(dead_code)] // E47 safe opt-in TileEntity inventory COW; not wired to live IRW.
mod legacy_tile_rewrite;
mod legacy_world_inspect;
mod level_dat;
mod maps;
mod migration_staging;
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
mod verify_world;
mod vitals;
mod whitelist;
mod window_open;
mod workstations;
mod world;
mod worldgen;

use achievements::Achievements;
use admin::Administration;
use brewing::{Brewer, Brewers, Position as BrewerPosition};
use chests::{Chests, Position as ChestPosition};
use clock::Clock;
use decorations::Decorations;
use dispensers::{Devices, Position as DevicePosition};
use experience::Experience;
use fishing::Fishing;
use furnaces::{Furnace, Furnaces, Position as FurnacePosition};
use game_rules::GameRules;
use ground_items::GroundItems;
use hanging::Hangings;
use hoppers::{Hoppers, Position as HopperPosition};
use inventory::{Inventory, Stack};
use legacy_protocol::{
    read_frame, read_varint, write_frame, write_string, write_varint, BufferedFrameReader,
    PROTOCOL_VERSION,
};
use maps::Maps;
use minecarts::Minecarts;
use mobs::Mobs;
use multiplayer::{
    Event, Hub, JoinError, ADMIN_GAMEMODE_EVENT, ADMIN_KICK_EVENT, ADMIN_TELEPORT_EVENT,
    MAX_PLAYERS, PEARL_TELEPORT_EVENT,
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
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
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
use window_open::WindowKind;
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
        // A client snapshot is parsed, but never authorizes inventory mutation.
        if nbt_length < -1 {
            return Err(bad("invalid item NBT length"));
        }
        let nbt = if nbt_length >= 0 {
            Some(item_nbt::intern(self.take(nbt_length as usize)?)?)
        } else {
            None
        };
        let stack = Stack {
            id,
            count,
            damage,
            nbt,
        };
        if !stack.is_valid() {
            return Err(bad("invalid item ID, count or damage"));
        }
        Ok(Some(stack))
    }
    /// Parse a C10 Creative Inventory Action without allowing invalid NBT
    /// to disconnect the session or silently discarding custom item tags.
    /// Outer None means reject-and-resync; Some(None) clears a valid slot.
    /// The caller MUST still enforce Creative mode and its permitted slots.
    fn creative_slot(&mut self) -> io::Result<Option<Option<Stack>>> {
        let id = self.i16()?;
        if id == -1 {
            return Ok(Some(None));
        }
        let count = self.u8()?;
        let damage = self.i16()?;
        let nbt_length = self.i16()?;
        if nbt_length < -1 {
            return Err(bad("invalid creative item NBT length"));
        }
        // Consume the exact declared length even when the item is invalid,
        // so only a truncated packet is fatal. Validate the untagged identity
        // BEFORE allocating an interned NBT handle for an untrusted packet.
        let wire_nbt = if nbt_length >= 0 {
            Some(self.take(nbt_length as usize)?)
        } else {
            None
        };
        let mut stack = Stack {
            id,
            count,
            damage,
            nbt: None,
        };
        if !stack.is_valid() {
            return Ok(None);
        }
        if let Some(bytes) = wire_nbt {
            // Intern validates compressed length, decompression bounds and
            // the NBT Compound root; invalid tags are rejected atomically.
            let Ok(handle) = item_nbt::intern(bytes) else {
                return Ok(None);
            };
            stack.nbt = Some(handle);
        }
        Ok(Some(Some(stack)))
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
/// Vanilla 1.7.10 sends a VarInt-length UTF-8 String in MC|ItemName.
fn parse_anvil_name(payload: &[u8]) -> Option<String> {
    let mut input = Input(payload);
    let name = input.string(30).ok()?;
    input.finish().ok()?;
    // The vanilla GUI limits the field to 30 UTF-16 code units.
    if name.encode_utf16().count() > 30 || name.chars().any(char::is_control) {
        return None;
    }
    Some(name)
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
        Err(e) if matches!(e.kind(), ErrorKind::UnexpectedEof | ErrorKind::TimedOut) => Ok(()),
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
fn send_visible_signs(
    s: &mut Connection,
    decorations: &Mutex<Decorations>,
    chunks: &BTreeSet<(i32, i32)>,
) -> io::Result<()> {
    let (signs, pots) = {
        let storage = decorations
            .lock()
            .map_err(|_| bad("decorations lock poisoned"))?;
        (
            storage.sign_packets_in(chunks)?,
            storage.pot_packets_in(chunks)?,
        )
    };
    for body in signs {
        send(s, 0x33, &body)?;
    }
    for body in pots {
        send(s, 0x35, &body)?;
    }
    Ok(())
}
/// Keep hanging entities in sync with the client's chunk set. A chunk unload
/// destroys client-side entities; they must be spawned again on re-entry.
fn send_hanging_delta(
    s: &mut Connection,
    hangings: &Mutex<Hangings>,
    previous: &BTreeSet<(i32, i32)>,
    current: &BTreeSet<(i32, i32)>,
) -> io::Result<()> {
    let events = {
        let state = hangings.lock().map_err(|_| bad("hanging lock poisoned"))?;
        let leaving = previous.difference(current).copied().collect();
        let entering = current.difference(previous).copied().collect();
        let mut events = state.destroy_events_in(&leaving);
        events.extend(state.spawn_events_in(&entering)?);
        events
    };
    // Never hold the hanging-world mutex while writing to a network socket.
    for event in events {
        send(s, event.id, &event.body)?;
    }
    Ok(())
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
/// Pair only identical chest types: trapped (146) and normal (54) never mix.
/// Canonical ordering is shared by all viewers regardless of clicked side.
fn chest_neighbor(world: &World, pos: ChestPosition) -> Option<ChestPosition> {
    let (x, y, z) = pos;
    let chest_id = world.get(x, y, z) >> 4;
    if !matches!(chest_id, 54 | 146) {
        return None;
    }
    [(x - 1, y, z), (x + 1, y, z), (x, y, z - 1), (x, y, z + 1)]
        .into_iter()
        .find(|&(x, y, z)| world.get(x, y, z) >> 4 == chest_id)
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
    inventory_path: &'a mut PathBuf,
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
    decorations: &'a Mutex<Decorations>,
    decoration_path: &'a Path,
    maps: &'a Mutex<Maps>,
    map_path: &'a Path,
    hangings: &'a Mutex<Hangings>,
    hanging_path: &'a Path,
    dimension_worlds: Option<&'a dimensions::DimensionWorlds>,
    session_dimension: &'a mut dimensions::Dimension,
    survival: bool,
    hardcore: bool,
    difficulty: &'a AtomicU8,
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
/// Drive 1.7.10 tripwire collision from server-authoritative Mob/drop
/// locations on the existing world worker. Bounded entity registries are
/// sampled periodically, deduplicating touching entities into block cells.
fn refresh_entity_tripwires(
    world: &mut World,
    path: &Path,
    mob_positions: &[PlayerPosition],
    drop_positions: &[[i32; 3]],
) -> io::Result<Vec<(i32, i32, i32)>> {
    let mut feet = BTreeSet::new();
    for position in mob_positions {
        if position.is_valid() {
            feet.insert((
                position.x.floor() as i32,
                position.y.floor() as i32,
                position.z.floor() as i32,
            ));
        }
    }
    for &[x, y, z] in drop_positions {
        feet.insert((x.div_euclid(32), y.div_euclid(32), z.div_euclid(32)));
    }
    let mut changed = BTreeSet::new();
    for (x, y, z) in feet {
        for ty in [y, y.saturating_add(1)] {
            if world::in_bounds(x, ty, z) && world.get(x, ty, z) >> 4 == 132 {
                changed.extend(world.touch_tripwire(path, x, ty, z)?);
            }
        }
    }
    Ok(changed.into_iter().collect())
}

/// Classic Java 1.7.10 mining penalties apply when the player's *head*
/// is underwater, not merely when their feet touch a water block.
fn mining_environment(world: &World, player: &PlayerPosition) -> (bool, bool) {
    let x = player.x.floor() as i32;
    let z = player.z.floor() as i32;
    let head_y = (player.y + 1.62).floor() as i32;
    let submerged = matches!(world.get(x, head_y, z) >> 4, 8 | 9);
    (submerged, physics::supported(world, player))
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
        decorations,
        decoration_path,
        maps,
        map_path,
        hangings,
        hanging_path,
        dimension_worlds,
        session_dimension,
        mut survival,
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
    // Rebind these borrows after an actual cross-dimension transition.
    let mut world = world;
    let mut world_path = world_path;
    let mut portal_since: Option<(dimensions::Dimension, u64)> = None;
    let mut portal_cooldown_until = 0u64;
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
    // Only the same player who placed a sign may submit the next C12 edit.
    let mut pending_sign: Option<(i32, i32, i32)> = None;
    let mut active_window: u8 = 0;
    let mut active_chest: Option<ChestPosition> = None;
    let mut active_chest_partner: Option<ChestPosition> = None;
    let mut active_ender_chest: Option<ChestPosition> = None;
    let mut active_furnace: Option<FurnacePosition> = None;
    let mut active_hopper: Option<HopperPosition> = None;
    let mut active_device: Option<DevicePosition> = None;
    let mut last_device_view = [None; dispensers::DEVICE_SLOTS];
    let mut active_brewer: Option<BrewerPosition> = None;
    let mut active_anvil: Option<(i32, i32, i32)> = None;
    let mut active_enchant: Option<(i32, i32, i32)> = None;
    let mut enchant_shelves: u8 = 0;
    let mut enchant_offers: [u8; 3] = [0; 3];
    let mut enchant_seed: u64 = rand::random();
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
    let mut last_map_tick = clock.ticks();
    loop {
        if !hub.is_healthy(entity_id) {
            return Err(io::Error::other(
                "outbound packet overflow or session removed",
            ));
        }
        while let Ok(event) = events.try_recv() {
            if event.id == ADMIN_GAMEMODE_EVENT {
                if event.body.len() != 1 || event.body[0] > 1 {
                    return Err(bad("invalid internal gamemode"));
                }
                let mode = event.body[0];
                let next_survival = mode == 0;
                if survival == next_survival {
                    continue;
                }
                // Inventory stores are mode-specific: load the destination
                // before changing the player mode or client-visible state.
                let next_file = if next_survival {
                    player_path.with_extension("survival.inventory.json")
                } else {
                    Inventory::location(player_path)
                };
                let next_inventory = Inventory::load(&next_file)?;
                inventory.save(inventory_path.as_path())?;
                admin_state::write(&player_path.with_extension("gamemode"), mode, 1)?;
                *inventory = next_inventory;
                *inventory_path = next_file;
                survival = next_survival;
                mining = None;
                eating = None;
                bow_start = None;
                sprinting = false;
                if active_window != 0 {
                    send(s, 0x2e, &[active_window])?;
                }
                active_window = 0;
                active_chest = None;
                active_ender_chest = None;
                active_chest_partner = None;
                active_furnace = None;
                active_hopper = None;
                active_device = None;
                active_brewer = None;
                active_anvil = None;
                active_enchant = None;
                // Game State 3 is Protocol 5's gamemode notification.
                let mut state = vec![3u8];
                state.extend_from_slice(&(mode as f32).to_be_bytes());
                send(s, 0x2b, &state)?;
                let mut abilities = vec![if survival { 0 } else { 0x0d }];
                abilities.extend_from_slice(&0.05f32.to_be_bytes());
                abilities.extend_from_slice(&0.1f32.to_be_bytes());
                send(s, 0x39, &abilities)?;
                send(s, 0x30, &inventory.window_items())?;
                send(s, 0x2f, &inventory.cursor_packet())?;
                send(s, 0x09, &[inventory.selected()])?;
                if survival {
                    send(s, 0x1f, &experience.packet())?;
                }
                continue;
            }
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
                target.save_state_in_dimension(
                    player_path,
                    next_vitals,
                    session_dimension.protocol_id() as i32,
                )?;
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
                let old_chunks = loaded_chunks.clone();
                sync_chunks(s, world, &mut loaded_chunks, chunk_center(player))?;
                let entering = loaded_chunks.difference(&old_chunks).copied().collect();
                send_visible_signs(s, decorations, &entering)?;
                send_hanging_delta(s, hangings, &old_chunks, &loaded_chunks)?;
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
            player.save_state_in_dimension(
                player_path,
                *vitals,
                session_dimension.protocol_id() as i32,
            )?;
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
        if now_tick > last_map_tick && now_tick.is_multiple_of(4) {
            last_map_tick = now_tick;
            if let Some(Stack {
                id: 358, damage, ..
            }) = inventory.held()
            {
                let terrain = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                let packet = maps.lock().map_err(|_| bad("map lock poisoned"))?.refresh(
                    map_path,
                    &terrain,
                    damage,
                    (player.x.floor() as i32, player.z.floor() as i32),
                    yaw,
                    now_tick / 4,
                )?;
                if let Some(packet) = packet {
                    send(s, 0x34, &packet)?;
                }
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
                player.save_state_in_dimension(
                    player_path,
                    *vitals,
                    session_dimension.protocol_id() as i32,
                )?;
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
                candidate.save_state_in_dimension(
                    player_path,
                    next_vitals,
                    session_dimension.protocol_id() as i32,
                )?;
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
                player.save_state_in_dimension(
                    player_path,
                    next,
                    session_dimension.protocol_id() as i32,
                )?;
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
                player.save_state_in_dimension(
                    player_path,
                    *vitals,
                    session_dimension.protocol_id() as i32,
                )?;
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
                player.save_state_in_dimension(
                    player_path,
                    *vitals,
                    session_dimension.protocol_id() as i32,
                )?;
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
                                        nbt: None,
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
                                player.save_state_in_dimension(
                                    player_path,
                                    *vitals,
                                    session_dimension.protocol_id() as i32,
                                )?;
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
                                    nbt: None,
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
                                            nbt: None,
                                        })
                                        .is_some();
                                if container_returned {
                                    // Debit inventory before player-NBT food credit.
                                    remaining.save(inventory_path)?;
                                    player.save_state_in_dimension(
                                        player_path,
                                        after,
                                        session_dimension.protocol_id() as i32,
                                    )?;
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
                player.save_state_in_dimension(
                    player_path,
                    *vitals,
                    session_dimension.protocol_id() as i32,
                )?;
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
            18 => {
                // Protocol 5 C12: Int X, Short Y, Int Z, four Strings.
                // Reject unsolicited edits; only the placing player gets S36.
                let pos = (input.i32()?, input.i16()? as i32, input.i32()?);
                let lines = [
                    input.string(15)?,
                    input.string(15)?,
                    input.string(15)?,
                    input.string(15)?,
                ];
                input.finish()?;
                if pending_sign.take() == Some(pos)
                    && world::in_bounds(pos.0, pos.1, pos.2)
                    && near(location, pos.0, pos.1, pos.2)
                {
                    let sign_block = world
                        .lock()
                        .map_err(|_| bad("world mutex poisoned"))?
                        .get(pos.0, pos.1, pos.2)
                        >> 4;
                    if matches!(sign_block, 63 | 68) {
                        decorations
                            .lock()
                            .map_err(|_| bad("decorations lock poisoned"))?
                            .set_sign(decoration_path, pos, lines.clone())?;
                        hub.broadcast(Event {
                            id: 0x33,
                            body: decorations::sign_packet(pos, &lines)?,
                        });
                    }
                }
            }
            2 => {
                if !permissions.interact {
                    continue;
                }
                // Minecraft Java 1.7.10 C02 Use Entity: target Int, action Byte.
                // Interact-at (action 2) also carries three Float coordinates.
                let target_id = input.i32()?;
                let action = input.i8()?;
                if action == 2 {
                    input.f32()?;
                    input.f32()?;
                    input.f32()?;
                }
                input.finish()?;
                // Frame/painting IDs are disjoint from mobs. Server owns all
                // insertions, rotations, destruction and resulting item drops.
                let mut hanging_state =
                    hangings.lock().map_err(|_| bad("hanging lock poisoned"))?;
                if let Some(entity) = hanging_state.get(target_id).cloned() {
                    let at = (
                        player.x.floor() as i32,
                        player.y.floor() as i32,
                        player.z.floor() as i32,
                    );
                    if !entity.near(at) {
                        continue;
                    }
                    if action == 1 {
                        if let Some((next, removed)) = hanging_state.stage_remove(target_id)? {
                            if survival {
                                let drops = removed.drops();
                                let mut ground = ground_items
                                    .lock()
                                    .map_err(|_| bad("ground items lock poisoned"))?;
                                if !ground.has_capacity_for(drops.len()) {
                                    continue;
                                }
                                let (staged_ground, spawned) =
                                    ground.stage_put_many(&drops, removed.drop_position())?;
                                journal::commit(
                                    &world_path.with_extension("pending-txn.json"),
                                    &[
                                        (hanging_path, next.encoded()?),
                                        (ground_path, staged_ground.encoded()?),
                                    ],
                                )?;
                                *ground = staged_ground;
                                *hanging_state = next;
                                hub.broadcast(removed.destroy());
                                for dropped in spawned {
                                    for event in dropped.spawn() {
                                        hub.broadcast(event);
                                    }
                                }
                            } else {
                                next.save(hanging_path)?;
                                *hanging_state = next;
                                hub.broadcast(removed.destroy());
                            }
                        }
                    } else if action == 0 && entity.is_frame() {
                        if let Some((next, debit, event)) =
                            hanging_state.stage_frame_use(target_id, inventory.held())?
                        {
                            if debit && survival {
                                let mut candidate = inventory.clone();
                                if candidate.consume_held() {
                                    journal::commit(
                                        &world_path.with_extension("pending-txn.json"),
                                        &[
                                            (inventory_path, candidate.encoded()?),
                                            (hanging_path, next.encoded()?),
                                        ],
                                    )?;
                                    *inventory = candidate;
                                    *hanging_state = next;
                                    send(s, 0x2f, &inventory.set_slot(inventory.held_slot())?)?;
                                    hub.broadcast(event);
                                }
                            } else {
                                next.save(hanging_path)?;
                                *hanging_state = next;
                                hub.broadcast(event);
                            }
                        }
                    }
                    continue;
                }
                drop(hanging_state);
                if vitals.health > 0.0 && action == 0 {
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
                    // The remaining animal interactions debit items or wear
                    // tools and are currently implemented for Survival only.
                    if !survival {
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
                    if let Some(events) =
                        creatures.feed(target_id, *player, inventory.held(), now_tick)
                    {
                        let mut candidate = inventory.clone();
                        if candidate.consume_held() {
                            candidate.save(inventory_path)?;
                            *inventory = candidate;
                            send(s, 0x2f, &inventory.set_slot(inventory.held_slot())?)?;
                            for event in events {
                                hub.broadcast(event);
                            }
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
                        player.save_state_in_dimension(
                            player_path,
                            *vitals,
                            session_dimension.protocol_id() as i32,
                        )?;
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
                        player.save_state_in_dimension(
                            player_path,
                            *vitals,
                            session_dimension.protocol_id() as i32,
                        )?;
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
                // A10: server-authoritative source-frame and destination validation.
                // Nether requires 4 seconds' contact; End gateways activate in either direction.
                if let Some(worlds) = dimension_worlds {
                    let px = x.floor() as i32;
                    let py = y.floor() as i32;
                    let pz = z.floor() as i32;
                    let source_portal = {
                        let guard = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                        [(px, py, pz), (px, py + 1, pz), (px, py - 1, pz)]
                            .into_iter()
                            .find_map(|(bx, by, bz)| {
                                dimensions::portal_target(&guard, bx, by, bz)
                                    .map(|target| ((bx, by, bz), target))
                            })
                    };
                    if now_tick >= portal_cooldown_until {
                        if let Some(((bx, by, bz), target)) = source_portal {
                            let entered_at = match portal_since {
                                Some((current, since)) if current == target => since,
                                _ => now_tick,
                            };
                            portal_since = Some((target, entered_at));
                            let is_ready = dimensions::portal_contact_ready(
                                *session_dimension,
                                target,
                                now_tick.saturating_sub(entered_at),
                            );
                            if is_ready {
                                if let Some(arrival) =
                                    worlds.resolve_portal(*session_dimension, bx, by, bz)?
                                {
                                    let destination = PlayerPosition {
                                        x: arrival.x,
                                        y: arrival.y,
                                        z: arrival.z,
                                    };
                                    // Persist player inventory and checkpoint first.
                                    inventory.save(inventory_path.as_path())?;
                                    experience.save(experience_path)?;
                                    achievements.save(achievements_path)?;
                                    destination.save_state_in_dimension(
                                        player_path,
                                        *vitals,
                                        arrival.dimension.protocol_id() as i32,
                                    )?;
                                    if !hub.change_dimension(
                                        entity_id,
                                        arrival.dimension,
                                        destination,
                                    )? {
                                        return Err(bad("portal traveler was removed from hub"));
                                    }
                                    if active_window != 0 {
                                        send(s, 0x2e, &[active_window])?;
                                        active_window = 0;
                                    }
                                    active_chest = None;
                                    active_ender_chest = None;
                                    active_chest_partner = None;
                                    active_furnace = None;
                                    active_hopper = None;
                                    active_device = None;
                                    active_brewer = None;
                                    active_anvil = None;
                                    active_enchant = None;
                                    *session_dimension = arrival.dimension;
                                    world = worlds.world_ref(arrival.dimension);
                                    world_path = worlds.path_ref(arrival.dimension);
                                    *player = destination;
                                    location = (destination.x, destination.y, destination.z);
                                    fall = FallTracker::new(destination.y);
                                    idle_gravity = IdleGravity::new(now_tick);
                                    mining = None;
                                    eating = None;
                                    bow_start = None;
                                    portal_since = None;
                                    portal_cooldown_until = now_tick.saturating_add(100);
                                    let mut respawn = (arrival.dimension.protocol_id() as i32)
                                        .to_be_bytes()
                                        .to_vec();
                                    respawn.extend_from_slice(&[
                                        difficulty.load(Ordering::Acquire),
                                        u8::from(!survival) | if hardcore { 0x08 } else { 0 },
                                    ]);
                                    write_string("default", &mut respawn)?;
                                    send(s, 0x07, &respawn)?;
                                    loaded_chunks.clear();
                                    pending_chunk_relights.clear();
                                    sync_chunks(
                                        s,
                                        world,
                                        &mut loaded_chunks,
                                        chunk_center(player),
                                    )?;
                                    send_player_position(s, player)?;
                                    last_movement_broadcast = Instant::now();
                                    continue;
                                }
                            }
                        } else {
                            portal_since = None;
                        }
                    }
                }
                let old_chunks = loaded_chunks.clone();
                sync_chunks(s, world, &mut loaded_chunks, chunk_center(&candidate))?;
                if loaded_chunks != old_chunks {
                    let entering = loaded_chunks.difference(&old_chunks).copied().collect();
                    send_visible_signs(s, decorations, &entering)?;
                    send_hanging_delta(s, hangings, &old_chunks, &loaded_chunks)?;
                }
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
                // Java 1.7.10 C0B Entity Action: Int entity ID, Byte action,
                // Int horse jump boost (9 bytes). None are VarInts here.
                let _reported_entity_id = input.i32()?;
                let action = i32::from(input.i8()?);
                let _jump_boost = input.i32()?;
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
                            if w.get(x, y, z) >> 4 == 25
                                && w.get(x, y + 1, z) == 0
                                && world::in_bounds(x, y, z)
                                && near(location, x, y, z)
                            {
                                let pos = (x, y, z);
                                let pitch = decorations
                                    .lock()
                                    .map_err(|_| bad("decorations lock poisoned"))?
                                    .note(pos);
                                hub.broadcast(Event {
                                    id: 0x24,
                                    body: decorations::note_action(
                                        pos,
                                        decorations::instrument(w.get(x, y - 1, z) >> 4),
                                        pitch,
                                    ),
                                });
                            }
                            mining = if world::in_bounds(x, y, z) && near(location, x, y, z) {
                                let (submerged, grounded) = mining_environment(&w, player);
                                MiningProgress::start_with_environment(
                                    target,
                                    w.get(x, y, z),
                                    inventory.held(),
                                    submerged,
                                    grounded,
                                )
                            } else {
                                None
                            };
                        }
                        1 => mining = None,
                        2 => {
                            let attempt = mining.take();
                            let mut w = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                            let (submerged, grounded) = mining_environment(&w, player);
                            let result = attempt.and_then(|m| {
                                m.finish_with_environment(
                                    target,
                                    w.get(x, y, z),
                                    inventory.held(),
                                    submerged,
                                    grounded,
                                )
                            });
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
                                                .is_nonempty(target))
                                        || (matches!(block, 84 | 140)
                                            && decorations
                                                .lock()
                                                .map_err(|_| bad("decorations lock poisoned"))?
                                                .occupied(target));
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
                                                    decorations
                                                        .lock()
                                                        .map_err(|_| {
                                                            bad("decorations lock poisoned")
                                                        })?
                                                        .remove(decoration_path, target)?;
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
                                                            nbt: None,
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
                                        .is_nonempty((x, y, z)))
                                || (matches!(block, 84 | 140)
                                    && decorations
                                        .lock()
                                        .map_err(|_| bad("decorations lock poisoned"))?
                                        .occupied((x, y, z)));
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
                        decorations
                            .lock()
                            .map_err(|_| bad("decorations lock poisoned"))?
                            .remove(decoration_path, (x, y, z))?;
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
                if x == -1 && y == 255 && z == -1 && face == -1 {
                    if inventory.can_place(held) && inventory.held().is_some_and(|s| s.id == 395) {
                        let mut map_storage = maps.lock().map_err(|_| bad("map lock poisoned"))?;
                        let current_x = player.x.floor() as i32;
                        let current_z = player.z.floor() as i32;
                        if let Some((next_maps, new_id)) =
                            map_storage.stage_new(current_x, current_z)?
                        {
                            let mut remaining = inventory.clone();
                            let _ = remaining.consume_held();
                            if remaining
                                .receive_stack(Stack {
                                    id: 358,
                                    count: 1,
                                    damage: new_id,
                                    nbt: None,
                                })
                                .is_some()
                            {
                                journal::commit(
                                    &world_path.with_extension("pending-txn.json"),
                                    &[
                                        (inventory_path, remaining.encoded()?),
                                        (map_path, next_maps.encoded()?),
                                    ],
                                )?;
                                *inventory = remaining;
                                *map_storage = next_maps;
                                send(s, 0x30, &inventory.window_items())?;
                            }
                        }
                    }
                    if !survival {
                        continue;
                    }
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
                if (0..=5).contains(&face)
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
                        let candidate = if survival {
                            let mut candidate = inventory.clone();
                            if !candidate.consume_held() {
                                continue;
                            }
                            Some(candidate)
                        } else {
                            None
                        };
                        let mut staged_carts = carts.clone();
                        let spawned = staged_carts
                            .place(&railway, position, now_tick)
                            .ok_or_else(|| bad("minecart vanished after preflight"))?;
                        let cart_file = Minecarts::location(world_path);
                        let debited = if let Some(candidate) = candidate {
                            // Survival debit and placement must be one transaction.
                            journal::commit(
                                &world_path.with_extension("pending-txn.json"),
                                &[
                                    (inventory_path, candidate.encoded()?),
                                    (cart_file.as_path(), staged_carts.encoded()?),
                                ],
                            )?;
                            *inventory = candidate;
                            true
                        } else {
                            journal::commit(
                                &world_path.with_extension("pending-txn.json"),
                                &[(cart_file.as_path(), staged_carts.encoded()?)],
                            )?;
                            false
                        };
                        *carts = staged_carts;
                        drop(carts);
                        drop(railway);
                        if debited {
                            send(s, 0x2f, &inventory.set_slot(inventory.held_slot())?)?;
                        }
                        hub.broadcast_in_view(position, spawned);
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
                // Item frames and paintings are hanging entities, not blocks.
                if (2..=5).contains(&face)
                    && world::in_bounds(x, y, z)
                    && near(location, x, y, z)
                    && inventory.can_place(held)
                    && inventory
                        .held()
                        .is_some_and(|stack| matches!(stack.id, 321 | 389))
                {
                    let stack = inventory
                        .held()
                        .ok_or_else(|| bad("missing hanging item"))?;
                    let terrain = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                    let mut hanging_state =
                        hangings.lock().map_err(|_| bad("hanging lock poisoned"))?;
                    if let Some((next, placed)) =
                        hanging_state.stage_place(&terrain, (x, y, z), face, stack.id)?
                    {
                        if survival {
                            let mut candidate = inventory.clone();
                            if candidate.consume_held() {
                                journal::commit(
                                    &world_path.with_extension("pending-txn.json"),
                                    &[
                                        (inventory_path, candidate.encoded()?),
                                        (hanging_path, next.encoded()?),
                                    ],
                                )?;
                                *inventory = candidate;
                                *hanging_state = next;
                                send(s, 0x2f, &inventory.set_slot(inventory.held_slot())?)?;
                                for event in placed.spawn()? {
                                    hub.broadcast(event);
                                }
                            }
                        } else {
                            next.save(hanging_path)?;
                            *hanging_state = next;
                            for event in placed.spawn()? {
                                hub.broadcast(event);
                            }
                        }
                    }
                    continue;
                }
                // World-owned decoration interactions precede ordinary item placement.
                if (0..=5).contains(&face) && world::in_bounds(x, y, z) && near(location, x, y, z) {
                    let mut w = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                    let block = w.get(x, y, z);
                    let pos = (x, y, z);
                    match block >> 4 {
                        25 => {
                            let pitch = decorations
                                .lock()
                                .map_err(|_| bad("decorations lock poisoned"))?
                                .next_note(decoration_path, pos)?;
                            if w.get(x, y + 1, z) == 0 {
                                let instrument = decorations::instrument(w.get(x, y - 1, z) >> 4);
                                hub.broadcast(Event {
                                    id: 0x24,
                                    body: decorations::note_action(pos, instrument, pitch),
                                });
                            }
                            continue;
                        }
                        92 => {
                            if survival && vitals.health > 0.0 && vitals.food < 20 {
                                let bites = block & 7;
                                if bites <= 5 {
                                    let next = if bites == 5 {
                                        0
                                    } else {
                                        (92 << 4) | (bites + 1)
                                    };
                                    if w.update_and_save(world_path, x, y, z, next)? {
                                        vitals.food = (vitals.food + 2).min(20);
                                        vitals.saturation =
                                            (vitals.saturation + 0.4).min(vitals.food as f32);
                                        player.save_state_in_dimension(
                                            player_path,
                                            *vitals,
                                            session_dimension.protocol_id() as i32,
                                        )?;
                                        send(s, 0x06, &health_packet(*vitals))?;
                                        hub.broadcast(block_change_event(&w, x, y, z));
                                    }
                                }
                            }
                            continue;
                        }
                        84 => {
                            let mut stored = decorations
                                .lock()
                                .map_err(|_| bad("decorations lock poisoned"))?;
                            if let Some(disc) = stored.record(pos) {
                                let mut candidate = inventory.clone();
                                if !survival
                                    || candidate
                                        .receive_stack(Stack {
                                            id: disc,
                                            count: 1,
                                            damage: 0,
                                            nbt: None,
                                        })
                                        .is_some()
                                {
                                    let staged = stored.stage_record(pos, None)?;
                                    let mut changes = vec![(decoration_path, staged.encoded()?)];
                                    if survival {
                                        changes.push((inventory_path, candidate.encoded()?));
                                    }
                                    journal::commit(
                                        &world_path.with_extension("pending-txn.json"),
                                        &changes,
                                    )?;
                                    *stored = staged;
                                    w.update_and_save(world_path, x, y, z, 84 << 4)?;
                                    if survival {
                                        *inventory = candidate;
                                        send(s, 0x30, &inventory.window_items())?;
                                    }
                                    hub.broadcast(block_change_event(&w, x, y, z));
                                    hub.broadcast(Event {
                                        id: 0x28,
                                        body: decorations::disc_event(pos, None),
                                    });
                                }
                            } else if inventory.can_place(held)
                                && inventory
                                    .held()
                                    .is_some_and(|item| (2256..=2267).contains(&item.id))
                            {
                                let disc = inventory.held().unwrap().id;
                                let mut candidate = inventory.clone();
                                if !survival || candidate.consume_held() {
                                    let staged = stored.stage_record(pos, Some(disc))?;
                                    let mut changes = vec![(decoration_path, staged.encoded()?)];
                                    if survival {
                                        changes.push((inventory_path, candidate.encoded()?));
                                    }
                                    journal::commit(
                                        &world_path.with_extension("pending-txn.json"),
                                        &changes,
                                    )?;
                                    *stored = staged;
                                    if survival {
                                        *inventory = candidate;
                                        send(s, 0x30, &inventory.window_items())?;
                                    }
                                    w.update_and_save(world_path, x, y, z, (84 << 4) | 1)?;
                                    hub.broadcast(block_change_event(&w, x, y, z));
                                    hub.broadcast(Event {
                                        id: 0x28,
                                        body: decorations::disc_event(pos, Some(disc)),
                                    });
                                }
                            }
                            continue;
                        }
                        140 => {
                            let mut stored = decorations
                                .lock()
                                .map_err(|_| bad("decorations lock poisoned"))?;
                            if let Some(stack) = stored.pot(pos) {
                                let mut candidate = inventory.clone();
                                if !survival || candidate.receive_stack(stack).is_some() {
                                    let staged = stored.stage_pot(pos, None)?;
                                    let mut changes = vec![(decoration_path, staged.encoded()?)];
                                    if survival {
                                        changes.push((inventory_path, candidate.encoded()?));
                                    }
                                    journal::commit(
                                        &world_path.with_extension("pending-txn.json"),
                                        &changes,
                                    )?;
                                    *stored = staged;
                                    if survival {
                                        *inventory = candidate;
                                        send(s, 0x30, &inventory.window_items())?;
                                    }
                                    hub.broadcast(Event {
                                        id: 0x35,
                                        body: decorations::pot_packet(pos, None)?,
                                    });
                                }
                            } else if inventory.can_place(held)
                                && inventory
                                    .held()
                                    .is_some_and(decorations::pot_item_supported)
                            {
                                let item = inventory.held().unwrap();
                                let plant = Stack { count: 1, ..item };
                                let mut candidate = inventory.clone();
                                if !survival || candidate.consume_held() {
                                    let staged = stored.stage_pot(pos, Some(plant))?;
                                    let mut changes = vec![(decoration_path, staged.encoded()?)];
                                    if survival {
                                        changes.push((inventory_path, candidate.encoded()?));
                                    }
                                    journal::commit(
                                        &world_path.with_extension("pending-txn.json"),
                                        &changes,
                                    )?;
                                    *stored = staged;
                                    if survival {
                                        *inventory = candidate;
                                        send(s, 0x30, &inventory.window_items())?;
                                    }
                                    hub.broadcast(Event {
                                        id: 0x35,
                                        body: decorations::pot_packet(pos, Some(plant))?,
                                    });
                                }
                            }
                            continue;
                        }
                        63 | 68 => continue, // signs are editable only immediately after placement
                        _ => {}
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
                    active_ender_chest = None;
                    active_chest_partner = None;
                    active_furnace = Some(pos);
                    active_hopper = None;
                    active_device = None;
                    active_brewer = None;
                    active_anvil = None;
                    active_enchant = None;
                    let guard = furnaces.lock().map_err(|_| bad("furnace lock poisoned"))?;
                    last_furnace_view = guard.get(pos);
                    drop(guard);
                    let opened =
                        window_open::encode(active_window, WindowKind::Furnace, "Furnace", 3)?;
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
                // Ender Chest storage follows the player, not the block:
                // all Ender Chests in every dimension open the same 27 slots.
                if (0..=5).contains(&face)
                    && world::in_bounds(x, y, z)
                    && near(location, x, y, z)
                    && world
                        .lock()
                        .map_err(|_| bad("world mutex poisoned"))?
                        .get(x, y, z)
                        >> 4
                        == 130
                {
                    active_window = next_window;
                    next_window = if next_window >= 100 {
                        1
                    } else {
                        next_window + 1
                    };
                    active_chest = None;
                    active_chest_partner = None;
                    active_ender_chest = Some((x, y, z));
                    active_furnace = None;
                    active_hopper = None;
                    active_device = None;
                    active_brewer = None;
                    active_anvil = None;
                    active_enchant = None;
                    let opened =
                        window_open::encode(active_window, WindowKind::Chest, "Ender Chest", 27)?;
                    send(s, 0x2d, &opened)?;
                    send(s, 0x30, &inventory.ender_chest_window_items(active_window))?;
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
                        54 | 146
                    )
                {
                    active_window = next_window;
                    next_window = if next_window >= 100 {
                        1
                    } else {
                        next_window + 1
                    };
                    let pos = (x, y, z);
                    active_chest = Some(pos);
                    active_ender_chest = None;
                    active_chest_partner = world
                        .lock()
                        .map_err(|_| bad("world mutex poisoned"))
                        .map(|w| chest_neighbor(&w, pos))?;
                    active_furnace = None;
                    active_hopper = None;
                    active_device = None;
                    active_brewer = None;
                    active_anvil = None;
                    active_enchant = None;
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
                    let slots = if active_chest_partner.is_some() {
                        54
                    } else {
                        27
                    };
                    let opened =
                        window_open::encode(active_window, WindowKind::Chest, "Chest", slots)?;
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
                    active_ender_chest = None;
                    active_chest_partner = None;
                    active_furnace = None;
                    active_hopper = None;
                    active_device = None;
                    active_anvil = None;
                    active_enchant = None;
                    let guard = brewers.lock().map_err(|_| bad("brewer lock poisoned"))?;
                    last_brewer_view = guard.get(pos);
                    drop(guard);
                    let opened = window_open::encode(
                        active_window,
                        WindowKind::BrewingStand,
                        "Brewing Stand",
                        4,
                    )?;
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
                    // Resolve the block before locking device storage, keeping
                    // the world/device lock ordering unchanged.
                    let block_id = world
                        .lock()
                        .map_err(|_| bad("world mutex poisoned"))?
                        .get(x, y, z)
                        >> 4;
                    let Some(title) = dispensers::window_title(block_id) else {
                        // The block may have been replaced after the initial check.
                        continue;
                    };
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
                    active_ender_chest = None;
                    active_chest_partner = None;
                    active_furnace = None;
                    active_hopper = None;
                    active_brewer = None;
                    active_anvil = None;
                    active_enchant = None;
                    let items = inventory.chest_window_items(active_window, &last_device_view);
                    drop(guard);
                    let kind = if block_id == 158 {
                        WindowKind::Dropper
                    } else {
                        WindowKind::Dispenser
                    };
                    let opened = window_open::encode(
                        active_window,
                        kind,
                        title,
                        dispensers::DEVICE_SLOTS as u8,
                    )?;
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
                    active_anvil = None;
                    active_enchant = None;
                    active_chest = None;
                    active_ender_chest = None;
                    active_chest_partner = None;
                    active_furnace = None;
                    observed_hopper_revision = guard.revision();
                    let items = inventory.chest_window_items(active_window, &guard.slots(pos));
                    drop(guard);
                    let opened =
                        window_open::encode(active_window, WindowKind::Hopper, "Hopper", 5)?;
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
                        == 116
                {
                    // Enchanting table is a one-slot, non-storage window.
                    // Enchant buttons remain fail-closed until live ItemStack
                    // NBT can preserve enchantments and arbitrary other tags.
                    let shelves = {
                        let guard = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                        workstations::bookshelf_count(|dx, dy, dz| {
                            let (bx, by, bz) = (x + dx, y + dy, z + dz);
                            if world::in_bounds(bx, by, bz) {
                                guard.get(bx, by, bz) >> 4
                            } else {
                                0
                            }
                        })
                    };
                    active_window = next_window;
                    next_window = if next_window >= 100 {
                        1
                    } else {
                        next_window + 1
                    };
                    active_chest = None;
                    active_ender_chest = None;
                    active_chest_partner = None;
                    active_furnace = None;
                    active_hopper = None;
                    active_device = None;
                    active_brewer = None;
                    active_anvil = None;
                    active_enchant = Some((x, y, z));
                    enchant_shelves = shelves;
                    enchant_seed = rand::random();
                    enchant_offers = if inventory.enchanting_available() {
                        workstations::enchanting_costs(enchant_shelves, enchant_seed)
                    } else {
                        [0; 3]
                    };
                    let opened = window_open::encode(
                        active_window,
                        WindowKind::EnchantingTable,
                        "Enchant",
                        0,
                    )?;
                    send(s, 0x2d, &opened)?;
                    send(s, 0x30, &inventory.enchanting_window_items(active_window))?;
                    for (property, cost) in enchant_offers.iter().copied().enumerate() {
                        let mut update = vec![active_window];
                        update.extend_from_slice(&(property as i16).to_be_bytes());
                        update.extend_from_slice(&(cost as i16).to_be_bytes());
                        send(s, 0x31, &update)?;
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
                        == 145
                {
                    active_window = next_window;
                    next_window = if next_window >= 100 {
                        1
                    } else {
                        next_window + 1
                    };
                    active_chest = None;
                    active_ender_chest = None;
                    active_chest_partner = None;
                    active_furnace = None;
                    active_hopper = None;
                    active_device = None;
                    active_brewer = None;
                    active_anvil = Some((x, y, z));
                    inventory.clear_anvil_name();
                    active_enchant = None;
                    let opened =
                        window_open::encode(active_window, WindowKind::Anvil, "Repair & Name", 0)?;
                    send(s, 0x2d, &opened)?;
                    send(s, 0x30, &inventory.anvil_window_items(active_window))?;
                    send(s, 0x31, &inventory.anvil_cost_property(active_window))?;
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
                    active_ender_chest = None;
                    active_chest_partner = None;
                    active_furnace = None;
                    active_hopper = None;
                    active_device = None;
                    active_brewer = None;
                    active_anvil = None;
                    active_enchant = None;
                    next_window = if next_window >= 100 {
                        1
                    } else {
                        next_window + 1
                    };
                    let opened = window_open::encode(
                        active_window,
                        WindowKind::CraftingTable,
                        "Crafting",
                        0,
                    )?;
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
                                323 => {
                                    if face == 1 {
                                        63
                                    } else if (2..=5).contains(&face) {
                                        68
                                    } else {
                                        0
                                    }
                                } // standing/wall sign
                                354 => 92,                       // cake
                                390 => 140,                      // flower pot
                                324 => 64,                       // wooden door
                                330 => 71,                       // iron door
                                _ => stack.id as u16,
                            };
                            let invalid_chest_merge =
                                block_id == 54 && !can_place_chest(&w, (nx, ny, nz));
                            let metadata = if block_id == 63 {
                                ((yaw.rem_euclid(360.0) * 16.0 / 360.0).round() as u16) & 15
                            } else if block_id == 68 {
                                face as u16
                            } else if block_id == 131 {
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
                                                nbt: None,
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
                            if matches!(w.get(nx, ny, nz) >> 4, 63 | 68) {
                                let pos = (nx, ny, nz);
                                // A block change must precede S36 sign editor.
                                pending_sign = Some(pos);
                                decorations
                                    .lock()
                                    .map_err(|_| bad("decorations lock poisoned"))?
                                    .set_sign(decoration_path, pos, Default::default())?;
                                send(s, 0x36, &decorations::sign_editor_packet(pos))?;
                            }
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
                if survival {
                    // Survival must never allocate Creative-supplied NBT.
                    // Consume untrusted bytes without interning or minting.
                    input.skip_untrusted_slot()?;
                    input.finish()?;
                    send(s, 0x30, &inventory.window_items())?;
                } else {
                    let requested = input.creative_slot()?;
                    input.finish()?;
                    if let Some(stack) = requested {
                        if inventory.creative_set(slot, stack)? {
                            // Persist only an accepted inventory mutation.
                            inventory.save(inventory_path)?;
                            send(s, 0x2f, &inventory.set_slot(slot)?)?;
                        } else {
                            send(s, 0x30, &inventory.window_items())?;
                        }
                    } else {
                        // Invalid or unrepresentable NBT cannot mint an item.
                        // Keep the session alive and restore server state.
                        send(s, 0x30, &inventory.window_items())?;
                    }
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
                if let Some(pos) = active_ender_chest {
                    let still_open = world
                        .lock()
                        .map_err(|_| bad("world mutex poisoned"))?
                        .get(pos.0, pos.1, pos.2)
                        >> 4
                        == 130
                        && near(location, pos.0, pos.1, pos.2);
                    let mut candidate = inventory.clone();
                    accepted |= still_open
                        && requested_window
                        && candidate
                            .ender_chest_click_window(active_window, slot, button, mode)
                            .is_some();
                    if accepted && candidate != *inventory {
                        candidate.save(inventory_path)?;
                        *inventory = candidate;
                    }
                    response_items = inventory.ender_chest_window_items(active_window);
                } else if let Some(pos) = active_enchant {
                    let still_open = world
                        .lock()
                        .map_err(|_| bad("world mutex poisoned"))?
                        .get(pos.0, pos.1, pos.2)
                        >> 4
                        == 116
                        && near(location, pos.0, pos.1, pos.2);
                    let mut candidate = inventory.clone();
                    accepted |= still_open
                        && requested_window
                        && candidate.enchanting_click_window(slot, button, mode);
                    if accepted && candidate != *inventory {
                        if candidate.enchanting_input() != inventory.enchanting_input() {
                            enchant_seed = rand::random();
                            enchant_offers = if candidate.enchanting_available() {
                                workstations::enchanting_costs(enchant_shelves, enchant_seed)
                            } else {
                                [0; 3]
                            };
                        }
                        candidate.save(inventory_path)?;
                        *inventory = candidate;
                    }
                    response_items = inventory.enchanting_window_items(active_window);
                } else if let Some(pos) = active_anvil {
                    let still_open = world
                        .lock()
                        .map_err(|_| bad("world mutex poisoned"))?
                        .get(pos.0, pos.1, pos.2)
                        >> 4
                        == 145
                        && near(location, pos.0, pos.1, pos.2);
                    let mut candidate = inventory.clone();
                    let mut next_xp = *experience;
                    accepted |= still_open
                        && requested_window
                        && candidate.anvil_click_window(slot, button, mode, &mut next_xp, survival);
                    if accepted {
                        let inventory_changed = candidate != *inventory;
                        let xp_changed = next_xp != *experience;
                        if inventory_changed && xp_changed {
                            journal::commit(
                                &world_path.with_extension("pending-txn.json"),
                                &[
                                    (inventory_path, candidate.encoded()?),
                                    (
                                        experience_path,
                                        serde_json::to_vec(&next_xp)
                                            .map_err(|_| bad("encode anvil XP"))?,
                                    ),
                                ],
                            )?;
                            *inventory = candidate;
                            *experience = next_xp;
                        } else if inventory_changed {
                            candidate.save(inventory_path)?;
                            *inventory = candidate;
                        } else if xp_changed {
                            next_xp.save(experience_path)?;
                            *experience = next_xp;
                        }
                        if xp_changed {
                            send(s, 0x1f, &experience.packet())?;
                        }
                    }
                    response_items = inventory.anvil_window_items(active_window);
                } else if let Some(pos) = active_furnace {
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
                if active_anvil.is_some() {
                    send(s, 0x31, &inventory.anvil_cost_property(active_window))?;
                }
                if active_enchant.is_some() {
                    for (property, cost) in enchant_offers.iter().copied().enumerate() {
                        let mut update = vec![active_window];
                        update.extend_from_slice(&(property as i16).to_be_bytes());
                        update.extend_from_slice(&(cost as i16).to_be_bytes());
                        send(s, 0x31, &update)?;
                    }
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
                    // Recover both unused anvil inputs before releasing the
                    // GUI. If both cannot fit, keep them persisted in the
                    // session inventory for a future anvil reopen.
                    if active_anvil.is_some() {
                        let mut staged = inventory.clone();
                        if staged.reclaim_anvil_inputs() && staged != *inventory {
                            staged.save(inventory_path)?;
                            *inventory = staged;
                        }
                    }
                    if active_enchant.is_some() {
                        let mut staged = inventory.clone();
                        if staged.reclaim_enchanting_input() && staged != *inventory {
                            staged.save(inventory_path)?;
                            *inventory = staged;
                        }
                    }
                    // Keep unused crafting ingredients saved rather than
                    // deleting them when the player closes a crafting window.
                    active_window = 0;
                    active_chest = None;
                    active_ender_chest = None;
                    active_chest_partner = None;
                    active_furnace = None;
                    active_hopper = None;
                    active_device = None;
                    active_brewer = None;
                    active_anvil = None;
                    active_enchant = None;
                    send(s, 0x30, &inventory.window_items())?;
                }
            }
            17 => {
                // Protocol 5 Enchant Item (C11) contains window and selected row.
                // Never use a client-supplied stack, candidate or experience total.
                let window = input.i8()?;
                let choice = input.i8()?;
                input.finish()?;
                if let Some(pos) = active_enchant {
                    if window == active_window as i8 && (0..=2).contains(&choice) {
                        let still_open = world
                            .lock()
                            .map_err(|_| bad("world mutex poisoned"))?
                            .get(pos.0, pos.1, pos.2)
                            >> 4
                            == 116
                            && near(location, pos.0, pos.1, pos.2);
                        if still_open {
                            let mut candidate = inventory.clone();
                            let mut next_xp = *experience;
                            if candidate.enchant_choice(
                                choice,
                                enchant_offers,
                                enchant_seed,
                                &mut next_xp,
                                survival,
                            ) {
                                journal::commit(
                                    &world_path.with_extension("pending-txn.json"),
                                    &[
                                        (inventory_path, candidate.encoded()?),
                                        (
                                            experience_path,
                                            serde_json::to_vec(&next_xp)
                                                .map_err(|_| bad("encode enchanting XP"))?,
                                        ),
                                    ],
                                )?;
                                *inventory = candidate;
                                *experience = next_xp;
                                enchant_offers = [0; 3]; // enchanted input stays withdrawable
                                send(s, 0x1f, &experience.packet())?;
                            }
                        }
                        send(s, 0x30, &inventory.enchanting_window_items(active_window))?;
                        for (property, cost) in enchant_offers.iter().copied().enumerate() {
                            let mut update = vec![active_window];
                            update.extend_from_slice(&(property as i16).to_be_bytes());
                            update.extend_from_slice(&(cost as i16).to_be_bytes());
                            send(s, 0x31, &update)?;
                        }
                        send(s, 0x2f, &inventory.cursor_packet())?;
                    }
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
                        player.save_state_in_dimension(
                            player_path,
                            *vitals,
                            session_dimension.protocol_id() as i32,
                        )?;
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
                    // Death in the Nether or End must respawn in the Overworld.
                    let respawn_dimension = if dimension_worlds.is_some() {
                        dimensions::Dimension::Overworld
                    } else {
                        *session_dimension
                    };
                    let respawn_world = if let Some(worlds) = dimension_worlds {
                        worlds.world_ref(respawn_dimension)
                    } else {
                        world
                    };
                    let origin = {
                        let fallback = safe_spawn(respawn_world)?;
                        if let Some((x, y, z)) = inventory.bed_spawn() {
                            let guard = respawn_world
                                .lock()
                                .map_err(|_| bad("world mutex poisoned"))?;
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
                    origin.save_state_in_dimension(
                        player_path,
                        fresh_vitals,
                        respawn_dimension.protocol_id() as i32,
                    )?;
                    if respawn_dimension != *session_dimension {
                        if !hub.change_dimension(entity_id, respawn_dimension, origin)? {
                            return Err(bad("respawning player no longer in hub"));
                        }
                        if let Some(worlds) = dimension_worlds {
                            world = worlds.world_ref(respawn_dimension);
                            world_path = worlds.path_ref(respawn_dimension);
                        }
                        *session_dimension = respawn_dimension;
                        portal_since = None;
                        portal_cooldown_until = now_tick.saturating_add(100);
                    }
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
                    let mut respawn = (respawn_dimension.protocol_id() as i32)
                        .to_be_bytes()
                        .to_vec();
                    respawn.extend_from_slice(&[difficulty.load(Ordering::Acquire), 0]); // current difficulty, survival
                    write_string("default", &mut respawn)?;
                    send(s, 0x07, &respawn)?;
                    loaded_chunks.clear(); // the client forgets chunks and entities on respawn
                    sync_chunks(s, world, &mut loaded_chunks, chunk_center(player))?;
                    send_visible_signs(s, decorations, &loaded_chunks)?;
                    send_hanging_delta(s, hangings, &BTreeSet::new(), &loaded_chunks)?;
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
            23 => {
                // Protocol 5 C17 Custom Payload: String channel, Short length,
                // bytes. Client NBT is untrusted; books::process validates
                // held item identity, compressed NBT and page/title bounds.
                let channel = input.string(20)?;
                let length = input.i16()?;
                if length < 0 {
                    return Err(bad("negative plugin payload size"));
                }
                let payload = input.take(length as usize)?;
                input.finish()?;
                if channel == "MC|ItemName" {
                    // Never apply a name from a closed, remote or stale anvil.
                    // This updates only the transient preview, not disk or XP;
                    // taking the output is the authoritative transaction.
                    if let (Some(pos), Some(requested)) = (active_anvil, parse_anvil_name(payload))
                    {
                        let valid_anvil = world
                            .lock()
                            .map_err(|_| bad("world mutex poisoned"))?
                            .get(pos.0, pos.1, pos.2)
                            >> 4
                            == 145
                            && near(location, pos.0, pos.1, pos.2);
                        if active_window != 0 && valid_anvil && inventory.set_anvil_name(requested)
                        {
                            send(s, 0x30, &inventory.anvil_window_items(active_window))?;
                            send(s, 0x31, &inventory.anvil_cost_property(active_window))?;
                        }
                    }
                } else if matches!(channel.as_str(), "MC|BEdit" | "MC|BSign")
                    && books::process(
                        &channel,
                        payload,
                        inventory,
                        inventory_path,
                        world_path,
                        name,
                    )?
                {
                    send(s, 0x2f, &inventory.set_slot(inventory.held_slot())?)?;
                    send(s, 0x30, &inventory.window_items())?;
                }
            }
            // Other packets are not trusted inventory or world mutations.
            10..=12 | 17..=21 => {}
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
    dimension_worlds: Option<Arc<dimensions::DimensionWorlds>>,
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
    decorations: Arc<Mutex<Decorations>>,
    decoration_path: Arc<PathBuf>,
    maps: Arc<Mutex<Maps>>,
    map_path: Arc<PathBuf>,
    hangings: Arc<Mutex<Hangings>>,
    hanging_path: Arc<PathBuf>,
    survival: bool,
    hardcore: bool,
    difficulty: Arc<AtomicU8>,
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
        dimension_worlds,
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
        decorations,
        decoration_path,
        maps,
        map_path,
        hangings,
        hanging_path,
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
    let survival = admin_state::read(&player_file.with_extension("gamemode"), 1)?
        .map_or(survival, |mode| mode == 0);
    if hardcore && !survival {
        login_disconnect(&mut s, "Hardcore mode cannot be Creative.")?;
        return Ok(());
    }
    let (mut player_state, mut player_vitals, saved_dimension_id) =
        match PlayerPosition::load_state_with_dimension(&player_file) {
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
    // Saved NBT selects the live world on reconnect, but single-world mode
    // continues to use its explicitly configured dimension.
    let initial_dimension = world
        .lock()
        .map_err(|_| bad("world mutex poisoned"))?
        .dimension();
    let admission_dimension = if dimension_worlds.is_some() {
        dimensions::Dimension::from_protocol(saved_dimension_id as i8)
            .ok_or_else(|| bad("invalid saved player dimension"))?
    } else {
        initial_dimension
    };
    let world = if let Some(worlds) = dimension_worlds.as_ref() {
        worlds.world(admission_dimension)
    } else {
        world
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
    let mut inventory_file = if survival {
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
    // A disconnect can happen without Close Window. Recover unspent anvil
    // inputs on the next login, but only if both stacks fit; otherwise retain
    // their persisted slots until a future recovery can complete.
    let mut recovered = inventory_state.clone();
    if recovered.reclaim_anvil_inputs() && recovered != inventory_state {
        recovered.save(&inventory_file)?;
        inventory_state = recovered;
    }
    let experience_file = player_file.with_extension("experience.json");
    let mut experience_state = Experience::load(&experience_file)?;
    let achievements_file = player_file.with_extension("achievements.json");
    let mut achievements_state = Achievements::load(&achievements_file)?;
    let (entity_id, events, previous) = match hub.add_in_dimension(
        name.clone(),
        uuid.clone(),
        player_state,
        admission_dimension,
    ) {
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
            difficulty: difficulty.load(Ordering::Acquire),
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
    // Chunks do not embed tile entities: replay persisted sign text after joining.
    send_visible_signs(
        &mut s,
        &decorations,
        &view_chunks(chunk_center(&player_state)),
    )?;
    send_hanging_delta(
        &mut s,
        &hangings,
        &BTreeSet::new(),
        &view_chunks(chunk_center(&player_state)),
    )?;
    // 1.7.10 tab list entries precede other player entity spawns.
    let self_list = multiplayer::list_entry(&name, true)?;
    send(&mut s, self_list.id, &self_list.body)?;
    for other in &previous {
        let list = multiplayer::list_entry(&other.name, true)?;
        send(&mut s, list.id, &list.body)?;
        if other.dimension == admission_dimension
            && multiplayer::visible(&player_state, &other.position)
        {
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
        for object in fishing
            .lock()
            .map_err(|_| bad("fishing mutex poisoned"))?
            .snapshot(player_state)
        {
            send(&mut s, object.id, &object.body)?;
        }
    }
    // Vehicles exist independently of the joining player's game mode.
    // Snapshot under the lock, then release it before writing to the socket.
    let visible_boats = boats
        .lock()
        .map_err(|_| bad("boats mutex poisoned"))?
        .snapshot(player_state);
    let visible_carts = minecarts
        .lock()
        .map_err(|_| bad("minecarts mutex poisoned"))?
        .snapshot(player_state);
    for object in visible_boats.into_iter().chain(visible_carts) {
        send(&mut s, object.id, &object.body)?;
    }
    let mut session_dimension = admission_dimension;
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
            inventory_path: &mut inventory_file,
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
            decorations: &decorations,
            decoration_path: &decoration_path,
            maps: &maps,
            map_path: &map_path,
            hangings: &hangings,
            hanging_path: &hanging_path,
            dimension_worlds: dimension_worlds.as_deref(),
            session_dimension: &mut session_dimension,
            survival,
            hardcore,
            difficulty: &difficulty,
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
    let player_saved = player_state.save_state_in_dimension(
        &player_file,
        player_vitals,
        session_dimension.protocol_id() as i32,
    );
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
        Some("/kick") | Some("/ban") | Some("/pardon") | Some("/pardon-uuid") | Some("/ban-ip")
        | Some("/pardon-ip") => 3,
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
        ["/pardon-uuid", uuid] => {
            administration.pardon_uuid(uuid)?;
            Ok(format!("Unbanned verified UUID {uuid}"))
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
            if let Some(uuid) = hub.uuid_for_online_name(target)? {
                administration.revoke_op_verified(target, &uuid)?;
            } else {
                // Offline target: the persisted name is the only lookup key.
                administration.set_op(target, false)?;
            }
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
    difficulty_state: Arc<AtomicU8>,
    difficulty_path: Arc<PathBuf>,
    mobs: Arc<Mutex<Mobs>>,
    hardcore: bool,
}
#[allow(clippy::too_many_arguments)] // Console worker coordinates the explicit shutdown flag.
fn start_clock_console(
    hub: Arc<Hub>,
    clock: Arc<Clock>,
    clock_path: Arc<PathBuf>,
    game_data: ConsoleGameData,
    world: Arc<Mutex<World>>,
    world_path: Arc<PathBuf>,
    access: ConsoleAccess,
    shutdown: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    let ConsoleGameData {
        rules,
        rules_path,
        scoreboard,
        scoreboard_path,
        difficulty_state,
        difficulty_path,
        mobs,
        hardcore,
    } = game_data;
    let ConsoleAccess {
        administration,
        whitelist_path,
        online_mode,
    } = access;
    thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            // stdin is blocking and may not produce another line during an
            // emergency shutdown. Refuse new admin mutations if it wakes later.
            if shutdown.load(Ordering::Acquire) {
                break;
            }
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
            if args.as_slice() == ["/stop"] {
                // A failed final audit must not silently shut down the server.
                if let Err(error) = administration.audit("CONSOLE", "/stop", "accepted") {
                    eprintln!("STOP AUDIT FAILED, shutdown refused: {error}");
                    continue;
                }
                shutdown.store(true, Ordering::Release);
                hub.broadcast(Event {
                    id: ADMIN_KICK_EVENT,
                    body: b"Server shutting down".to_vec(),
                });
                println!("Server stopping; waiting for players and world workers");
                break;
            }
            let result: io::Result<()> = match args.as_slice() {
                ["/gamemode", requested, target] => (|| -> io::Result<()> {
                    if hardcore {
                        return Err(bad("gamemode changes are forbidden in hardcore"));
                    }
                    let mode = admin_state::parse_gamemode(requested)?;
                    if !hub.request_admin_gamemode(target, mode)? {
                        return Err(io::Error::new(
                            ErrorKind::NotFound,
                            "target player not online",
                        ));
                    }
                    println!("Gamemode change queued for {target}: {mode}");
                    Ok(())
                })(),
                ["/difficulty", "query"] => {
                    println!("Difficulty: {}", difficulty_state.load(Ordering::Acquire));
                    Ok(())
                }
                ["/difficulty", requested] => (|| -> io::Result<()> {
                    let next = admin_state::parse_difficulty(requested)?;
                    if hardcore && next != 3 {
                        return Err(bad("hardcore difficulty must remain hard (3)"));
                    }
                    // Do not publish the change until its restart override is
                    // durable and the mob state is ready to observe it.
                    let mut state = mobs.lock().map_err(|_| bad("mob mutex poisoned"))?;
                    admin_state::write(&difficulty_path, next, 3)?;
                    state.set_difficulty(next);
                    difficulty_state.store(next, Ordering::Release);
                    println!("Difficulty set to {next} (new joins/respawns see updated setting)");
                    Ok(())
                })(),
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
                ["/deop", name] => (|| -> io::Result<()> {
                    if online_mode {
                        if let Some(uuid) = hub.uuid_for_online_name(name)? {
                            administration.revoke_op_verified(name, &uuid)
                        } else {
                            administration.set_op(name, false)
                        }
                    } else {
                        administration.set_op(name, false)
                    }
                })(),
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
                ["/pardon-uuid", uuid] => administration.pardon_uuid(uuid),
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
    })
}

/// Join every worker even when an earlier one panicked. Early-returning from
/// the shutdown drain would detach remaining workers and skip final saves.
fn join_shutdown_worker(
    worker: thread::JoinHandle<()>,
    name: &'static str,
    first_failure: &mut Option<&'static str>,
) {
    if worker.join().is_err() {
        eprintln!("SHUTDOWN WORKER PANIC: {name}");
        first_failure.get_or_insert(name);
    }
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

    // Read-only diagnostics never acquire session.lock, create a world or bind TCP.
    if raw_args.iter().any(|arg| arg == "--verify-world") {
        if raw_args.len() != 2 || raw_args[0] != "--verify-world" {
            return Err(bad("usage: --verify-world WORLD_DIR_OR_IRW_FILE"));
        }
        let summary = verify_world::inspect(Path::new(&raw_args[1]))?;
        println!(
            "World verified (read-only): {} ({} files)",
            summary.description, summary.files
        );
        return Ok(());
    }
    if raw_args.iter().any(|arg| arg == "--prepare-anvil-copy") {
        if raw_args.len() != 3 || raw_args[0] != "--prepare-anvil-copy" {
            return Err(bad("usage: --prepare-anvil-copy SOURCE_DIR NEW_DEST_DIR"));
        }
        let stats = migration_staging::prepare_anvil_copy(
            Path::new(&raw_args[1]),
            Path::new(&raw_args[2]),
        )?;
        println!(
            "Verified working copy: {} files ({} bytes); no conversion performed",
            stats.files, stats.bytes
        );
        return Ok(());
    }
    if raw_args.iter().any(|arg| arg == "--check-config") {
        if !((raw_args.len() == 1 || raw_args.len() == 2) && raw_args[0] == "--check-config") {
            return Err(bad("usage: --check-config [server.properties]"));
        }
        let config_file = raw_args
            .get(1)
            .map(String::as_str)
            .unwrap_or("server.properties");
        let props = server_config::ServerProperties::load_optional(Path::new(config_file))?
            .ok_or_else(|| io::Error::new(ErrorKind::NotFound, "server.properties not found"))?;
        server_config::verify_legacy_storage(&props.world_path)?;
        if !props.bind.ip().is_loopback() && !props.online_mode {
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                "offline-mode on a LAN/public address needs manual security review",
            ));
        }
        println!(
            "Configuration accepted: bind={}, world={}, difficulty={}, seed={:?} (read-only)",
            props.bind,
            props.world_path.display(),
            props.difficulty,
            props.level_seed
        );
        return Ok(());
    }
    // Reading configuration never converts, modifies, or replaces a native
    // Minecraft world. Without server.properties old IRW CLI behavior stays.
    let properties = if std::env::args().any(|arg| {
        arg == "--help"
            || arg == "--backup-world"
            || arg == "--backup-rotate"
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
    let mut backup_rotation: Option<(PathBuf, usize)> = None;
    let mut explicit_world = false;
    let mut whitelist_path = PathBuf::from("whitelist.json");
    let mut insecure_offline_lan = false;
    let mut survival = properties.as_ref().is_some_and(|config| config.survival);
    let hardcore = properties.as_ref().is_some_and(|config| config.hardcore);
    let mut online_mode = properties.as_ref().is_some_and(|config| config.online_mode);
    let mut difficulty = properties.as_ref().map_or(2, |config| config.difficulty);
    let mut requested_seed: Option<u64> = None;
    let mut biome_seed: Option<u64> = properties.as_ref().and_then(|config| config.level_seed);
    let mut expanded_seed: Option<u64> = None;
    let mut dimension_root: Option<PathBuf> = None;
    let mut expanded_dimension = dimensions::Dimension::Overworld;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" => {
                println!("Usage: vanilla-server [--bind IP:PORT] [--world PATH] [--whitelist FILE] [--insecure-offline-lan] [--online-mode] [--survival] [--recover-level-dat STOPPED_WORLD_DIR] [--creative] [--export-anvil NEW_DIR] [--import-anvil SOURCE_DIR] [--backup-world NEW_DIR] [--backup-rotate BACKUP_ROOT KEEP] [--verify-world DIR_OR_IRW] [--check-config [FILE]] [--prepare-anvil-copy SOURCE NEW_DIR] [--inspect-player-data FILE] [--inspect-level-dat FILE] [--inspect-anvil-region FILE] [--inspect-anvil-world DIR] [--inspect-anvil-chunk DIR overworld|nether|end CX CZ] [--terrain-seed U64] [--biome-seed U64] [--expanded-seed U64] [--dimension overworld|nether|end] [--dimension-root DIR]");
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
            "--dimension-root" => {
                dimension_root = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| bad("missing --dimension-root directory"))?,
                ));
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
            "--backup-rotate" => {
                let root = PathBuf::from(
                    args.next()
                        .ok_or_else(|| bad("missing --backup-rotate directory"))?,
                );
                let keep = args
                    .next()
                    .ok_or_else(|| bad("missing --backup-rotate retention"))?
                    .parse::<usize>()
                    .map_err(|_| bad("invalid backup retention"))?;
                backup_rotation = Some((root, keep));
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
    // Rotation must be a standalone offline command with an explicit world path.
    if backup_rotation.is_some()
        && !(raw_args.len() == 5
            && raw_args.iter().any(|arg| arg == "--world")
            && raw_args.iter().any(|arg| arg == "--backup-rotate"))
    {
        return Err(bad(
            "--backup-rotate requires only --world FILE --backup-rotate ROOT KEEP",
        ));
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
    if let Some(root) = dimension_root.as_ref() {
        if root.as_os_str().is_empty()
            || expanded_seed.is_none()
            || explicit_world
            || chunk_inspect.is_some()
            || world_inspect_path.is_some()
            || region_inspect_path.is_some()
            || level_inspect_path.is_some()
            || player_inspect_path.is_some()
            || anvil_import_path.is_some()
            || anvil_export_path.is_some()
            || backup_destination.is_some()
            || backup_rotation.is_some()
        {
            return Err(bad(
                "--dimension-root requires --expanded-seed and cannot combine with --world or offline operations",
            ));
        }
        world_path = dimensions::DimensionManager::new(
            root,
            expanded_seed.ok_or_else(|| bad("missing --expanded-seed"))?,
        )
        .path(expanded_dimension);
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
    if let Some((root, keep)) = backup_rotation {
        if !explicit_world
            || backup_destination.is_some()
            || anvil_import_path.is_some()
            || anvil_export_path.is_some()
            || requested_seed.is_some()
            || biome_seed.is_some()
            || expanded_seed.is_some()
        {
            return Err(bad(
                "--backup-rotate requires only --world with no other file operations",
            ));
        }
        let parent = world_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| bad("world file needs a parent directory"))?;
        if !world_path.is_file() && !parent.join("level.dat").is_file() {
            return Err(bad("offline backup source missing"));
        }
        let mut lock = session_lock::WorldSessionLock::acquire(parent)?;
        let (destination, stats) = backup_rotation::rotate(&world_path, &root, keep, &mut lock)?;
        println!(
            "Rotated backup: {} files ({} bytes) at {}",
            stats.files,
            stats.bytes,
            destination.display()
        );
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
    let administration_root = if let Some(root) = dimension_root.as_ref() {
        root.clone()
    } else if world_path.is_absolute() {
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
    let world_directory = dimension_root
        .as_deref()
        .or_else(|| {
            world_path
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
        })
        .unwrap_or_else(|| Path::new("."));
    let session_guard = Arc::new(Mutex::new(session_lock::WorldSessionLock::acquire(
        world_directory,
    )?));
    session_guard
        .lock()
        .map_err(|_| bad("session lock mutex poisoned"))?
        .verify()?;

    // Replay interrupted multi-file credits before loading any container or
    // accepting connections. Corrupt/unrecoverable logs prevent startup.
    journal::recover(&world_path.with_extension("pending-txn.json"))?;
    // Accept older independent log locations for one-time migration.
    journal::recover(&Chests::location(&world_path).with_extension("pending-txn.json"))?;
    journal::recover(&Furnaces::location(&world_path).with_extension("pending-txn.json"))?;
    let dimension_worlds = if let Some(root) = dimension_root.as_ref() {
        Some(Arc::new(dimensions::DimensionWorlds::load_all(
            dimensions::DimensionManager::new(
                root,
                expanded_seed.ok_or_else(|| bad("missing --expanded-seed"))?,
            ),
        )?))
    } else {
        None
    };
    books::initialize(&world_path)?;
    let mut loaded_world = if world_path.exists() {
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
    let hanging_path = Arc::new(Hangings::location(&world_path));
    let hangings = Arc::new(Mutex::new(Hangings::load(&hanging_path)?));
    let map_path = Arc::new(Maps::location(&world_path));
    let maps = Arc::new(Mutex::new(Maps::load(&map_path)?));
    let decoration_path = Arc::new(Decorations::location(&world_path));
    let decorations = Arc::new(Mutex::new(Decorations::load(&decoration_path)?));
    let clock_path = Arc::new(world_path.with_extension("clock"));
    let restored_clock = Clock::load_state(&clock_path)?;
    let (clock, clock_thread) = Clock::start_from(restored_clock);
    let difficulty_path = Arc::new(world_path.with_extension("difficulty"));
    let difficulty = admin_state::read(&difficulty_path, 3)?.unwrap_or(difficulty);
    if hardcore && difficulty != 3 {
        return Err(bad("hardcore difficulty override must remain 3"));
    }
    let shared_difficulty = Arc::new(AtomicU8::new(difficulty));
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
    let world = if let Some(worlds) = dimension_worlds.as_ref() {
        worlds.bind_save_sessions(Arc::clone(&session_guard))?;
        worlds.world(expanded_dimension)
    } else {
        loaded_world.bind_save_session(&world_path, Arc::clone(&session_guard))?;
        Arc::new(Mutex::new(loaded_world))
    };
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
    let minecart_path = Arc::new(Minecarts::location(&world_path));
    let minecarts = Arc::new(Mutex::new(Minecarts::load(&minecart_path, clock.ticks())?));
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
    let shutdown = Arc::new(AtomicBool::new(false));
    let console_thread = start_clock_console(
        Arc::clone(&hub),
        Arc::clone(&clock),
        Arc::clone(&clock_path),
        ConsoleGameData {
            rules: Arc::clone(&world_rules),
            rules_path: Arc::clone(&rules_path),
            scoreboard: Arc::clone(&shared_scoreboard),
            scoreboard_path: Arc::clone(&scoreboard_path),
            difficulty_state: Arc::clone(&shared_difficulty),
            difficulty_path: Arc::clone(&difficulty_path),
            mobs: Arc::clone(&mobs),
            hardcore,
        },
        Arc::clone(&world),
        Arc::clone(&path),
        ConsoleAccess {
            administration: Arc::clone(&administration),
            whitelist_path: Arc::clone(&allow_path),
            online_mode,
        },
        Arc::clone(&shutdown),
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
    let wire_mobs = Arc::clone(&mobs);
    let ejection_devices = Arc::clone(&devices);
    let ejection_file = Arc::clone(&device_path);
    let ejection_throwables = Arc::clone(&throwables);
    let ejection_chests = Arc::clone(&chests);
    let ejection_hoppers = Arc::clone(&hoppers);
    let ejection_chest_file = Arc::clone(&chest_path);
    let ejection_hopper_file = Arc::clone(&hopper_path);
    let ejection_furnaces = Arc::clone(&furnaces);
    let ejection_furnace_file = Arc::clone(&furnace_path);
    let ejection_brewers = Arc::clone(&brewers);
    let ejection_brewer_file = Arc::clone(&brewer_path);
    let tick_shutdown = Arc::clone(&shutdown);
    let tick_thread = thread::spawn(move || {
        let mut telemetry =
            telemetry::TickTelemetry::new(furnace_world_path.with_extension("metrics.jsonl"));
        let mut last = furnace_clock.ticks();
        let mut last_clock_snapshot = last;
        let mut last_weather = furnace_clock.weather();
        while !tick_shutdown.load(Ordering::Acquire) {
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
                // Take a committed inventory snapshot before locking World.
                // Never hold the device mutex while taking the world mutex:
                // rising-edge ejection uses the opposite lock order.
                let device_comparator_inputs = match ejection_devices.lock() {
                    Ok(storage) => storage.comparator_inputs(),
                    Err(_) => return,
                };
                let (chest_comparator_inputs, chest_snapshot) = match ejection_chests.lock() {
                    Ok(storage) => (storage.comparator_inputs(), storage.comparator_snapshot()),
                    Err(_) => return,
                };
                let furnace_comparator_inputs = match furnace_worker.lock() {
                    Ok(storage) => storage.comparator_inputs(),
                    Err(_) => return,
                };
                let hopper_comparator_inputs = match ejection_hoppers.lock() {
                    Ok(storage) => storage.comparator_inputs(),
                    Err(_) => return,
                };
                let mut gravity_world = match furnace_world.lock() {
                    Ok(v) => v,
                    Err(_) => break,
                };
                // Pair only real adjacent chest blocks, after releasing the chest
                // mutex and acquiring World. Both halves share one 54-slot signal.
                let chest_comparator_inputs = chests::combine_chest_comparator_inputs(
                    &gravity_world,
                    &chest_snapshot,
                    chest_comparator_inputs,
                );
                gravity_world.set_inventory_comparator_inputs(
                    device_comparator_inputs,
                    chest_comparator_inputs,
                    furnace_comparator_inputs,
                    hopper_comparator_inputs,
                );
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
                                let Some(offset) = dispensers::facing_offset(state) else {
                                    // Malformed 6/7 facing must not eject or debit inventory.
                                    continue;
                                };
                                let dest = (pos.0 + offset.0, pos.1 + offset.1, pos.2 + offset.2);
                                if !world::in_bounds(dest.0, dest.1, dest.2) {
                                    continue;
                                }
                                let mut storage = match ejection_devices.lock() {
                                    Ok(value) => value,
                                    Err(_) => return,
                                };
                                let seed = now
                                    ^ (pos.0 as u64).wrapping_mul(31)
                                    ^ (pos.2 as u64).wrapping_mul(131);
                                // Droppers insert into another dispenser or dropper
                                // without creating a ground entity. Both inventories
                                // share one persisted device file and one journal commit.
                                if state >> 4 == 158
                                    && matches!(
                                        gravity_world.get(dest.0, dest.1, dest.2) >> 4,
                                        23 | 158
                                    )
                                {
                                    let staged = match storage.stage_transfer_one(pos, dest, seed) {
                                        Ok(value) => value,
                                        Err(error) => {
                                            eprintln!("DROPPER DEVICE TRANSFER ERROR: {error}");
                                            continue;
                                        }
                                    };
                                    if let Some(updated) = staged {
                                        let bytes = match updated.encoded() {
                                            Ok(value) => value,
                                            Err(error) => {
                                                eprintln!("DROPPER DEVICE ENCODING ERROR: {error}");
                                                continue;
                                            }
                                        };
                                        if let Err(error) = journal::commit(
                                            &furnace_world_path.with_extension("pending-txn.json"),
                                            &[(&*ejection_file, bytes)],
                                        ) {
                                            eprintln!("DROPPER DEVICE JOURNAL ERROR: {error}");
                                            continue;
                                        }
                                        *storage = updated;
                                    }
                                    continue;
                                }
                                let Some((updated, stack)) = (match storage.stage_eject(pos, seed) {
                                    Ok(result) => result,
                                    Err(error) => {
                                        eprintln!("DEVICE EJECTION ERROR: {error}");
                                        continue;
                                    }
                                }) else {
                                    continue;
                                };
                                // Dispensers fire arrows, eggs and snowballs as server-owned
                                // projectiles. Stage the projectile in a detached
                                // bounded registry so capacity failure cannot debit
                                // the item; publish only after the inventory journal
                                // commits. Droppers always eject ordinary items.
                                if state >> 4 == 23 && throwables::is_dispenser_projectile(stack.id)
                                {
                                    let mut live_projectiles = match ejection_throwables.lock() {
                                        Ok(value) => value,
                                        Err(_) => return,
                                    };
                                    let mut staged_projectiles = live_projectiles.clone();
                                    let Some((at, event)) =
                                        staged_projectiles.launch_dispenser(stack.id, pos, offset)
                                    else {
                                        continue;
                                    };
                                    let device_bytes = match updated.encoded() {
                                        Ok(bytes) => bytes,
                                        Err(error) => {
                                            eprintln!(
                                                "DISPENSER PROJECTILE ENCODING ERROR: {error}"
                                            );
                                            continue;
                                        }
                                    };
                                    if let Err(error) = journal::commit(
                                        &furnace_world_path.with_extension("pending-txn.json"),
                                        &[(&*ejection_file, device_bytes)],
                                    ) {
                                        eprintln!("DISPENSER PROJECTILE JOURNAL ERROR: {error}");
                                        continue;
                                    }
                                    *storage = updated;
                                    *live_projectiles = staged_projectiles;
                                    drop(live_projectiles);
                                    drop(storage);
                                    furnace_hub.broadcast_in_view(at, event);
                                    continue;
                                }
                                // A 1.7.10 dropper sends one item directly into
                                // a normal or trapped chest it faces. If the chest is full, do
                                // not consume or create a ground item.
                                if state >> 4 == 158
                                    && dispensers::dropper_chest_target(
                                        gravity_world.get(dest.0, dest.1, dest.2) >> 4,
                                    )
                                {
                                    let mut chests = match ejection_chests.lock() {
                                        Ok(value) => value,
                                        Err(_) => return,
                                    };
                                    // Treat either same-type double chest as a 54-slot
                                    // inventory, even if the targeted half is full.
                                    // Keep the canonical order shared with chest windows.
                                    let partner = chest_neighbor(&gravity_world, dest);
                                    let mut slots = chest_window_state(&chests, dest, partner);
                                    if dispensers::insert_one_into_window(&mut slots, stack) {
                                        let staged_chest =
                                            stage_chest_window(&chests, dest, partner, &slots);
                                        let next_chest = match staged_chest {
                                            Ok(value) => value,
                                            Err(error) => {
                                                eprintln!("DROPPER CHEST TRANSFER ERROR: {error}");
                                                continue;
                                            }
                                        };
                                        let result = (|| -> io::Result<()> {
                                            journal::commit(
                                                &furnace_world_path
                                                    .with_extension("pending-txn.json"),
                                                &[
                                                    (&*ejection_file, updated.encoded()?),
                                                    (&*ejection_chest_file, next_chest.encoded()?),
                                                ],
                                            )?;
                                            Ok(())
                                        })();
                                        if let Err(error) = result {
                                            eprintln!("DROPPER TRANSFER JOURNAL ERROR: {error}");
                                            continue;
                                        }
                                        *storage = updated;
                                        *chests = next_chest;
                                    }
                                    continue;
                                }
                                // Droppers also insert into hoppers (including disabled
                                // hoppers). If all five slots are full, leave the source
                                // untouched and do not spawn a ground item.
                                if state >> 4 == 158
                                    && dispensers::dropper_hopper_target(
                                        gravity_world.get(dest.0, dest.1, dest.2) >> 4,
                                    )
                                {
                                    let mut hoppers = match ejection_hoppers.lock() {
                                        Ok(value) => value,
                                        Err(_) => return,
                                    };
                                    let mut slots = hoppers.slots(dest);
                                    if dispensers::insert_one(&mut slots, stack) {
                                        let next_hopper = match hoppers.staged_update(dest, slots) {
                                            Ok(value) => value,
                                            Err(error) => {
                                                eprintln!("DROPPER HOPPER TRANSFER ERROR: {error}");
                                                continue;
                                            }
                                        };
                                        let result = (|| -> io::Result<()> {
                                            journal::commit(
                                                &furnace_world_path
                                                    .with_extension("pending-txn.json"),
                                                &[
                                                    (&*ejection_file, updated.encoded()?),
                                                    (
                                                        &*ejection_hopper_file,
                                                        next_hopper.encoded()?,
                                                    ),
                                                ],
                                            )?;
                                            Ok(())
                                        })();
                                        if let Err(error) = result {
                                            eprintln!("DROPPER HOPPER JOURNAL ERROR: {error}");
                                            continue;
                                        }
                                        *storage = updated;
                                        *hoppers = next_hopper;
                                    }
                                    continue;
                                }
                                // Furnace insertion is sided in vanilla 1.7.10:
                                // from above -> input, from the side/below -> fuel.
                                // A rejected item or full slot stays in the dropper.
                                if state >> 4 == 158
                                    && matches!(
                                        gravity_world.get(dest.0, dest.1, dest.2) >> 4,
                                        61 | 62
                                    )
                                {
                                    let block_id = gravity_world.get(dest.0, dest.1, dest.2) >> 4;
                                    let Some(slot_index) =
                                        dispensers::dropper_furnace_slot(block_id, offset, stack)
                                    else {
                                        continue;
                                    };
                                    let mut furnaces = match ejection_furnaces.lock() {
                                        Ok(value) => value,
                                        Err(_) => return,
                                    };
                                    let mut furnace = furnaces.get(dest);
                                    let mut target = [furnace.slots[slot_index]];
                                    if dispensers::insert_one(&mut target, stack) {
                                        furnace.slots[slot_index] = target[0];
                                        let next_furnaces =
                                            match furnaces.staged_update(dest, furnace) {
                                                Ok(value) => value,
                                                Err(error) => {
                                                    eprintln!(
                                                        "DROPPER FURNACE TRANSFER ERROR: {error}"
                                                    );
                                                    continue;
                                                }
                                            };
                                        let result = (|| -> io::Result<()> {
                                            journal::commit(
                                                &furnace_world_path
                                                    .with_extension("pending-txn.json"),
                                                &[
                                                    (&*ejection_file, updated.encoded()?),
                                                    (
                                                        &*ejection_furnace_file,
                                                        next_furnaces.encoded()?,
                                                    ),
                                                ],
                                            )?;
                                            Ok(())
                                        })();
                                        if let Err(error) = result {
                                            eprintln!("DROPPER FURNACE JOURNAL ERROR: {error}");
                                            continue;
                                        }
                                        *storage = updated;
                                        *furnaces = next_furnaces;
                                    }
                                    continue;
                                }
                                // Brewing stands have four legacy slots: top supplies
                                // ingredients and all other faces supply bottles.
                                // Reject full/invalid slots without ejecting an item.
                                if state >> 4 == 158
                                    && gravity_world.get(dest.0, dest.1, dest.2) >> 4 == 117
                                {
                                    let mut brewers = match ejection_brewers.lock() {
                                        Ok(value) => value,
                                        Err(_) => return,
                                    };
                                    let mut stand = brewers.get(dest);
                                    if dispensers::dropper_brewer_insert(&mut stand, offset, stack)
                                    {
                                        let next_brewers = match brewers.staged_update(dest, stand)
                                        {
                                            Ok(value) => value,
                                            Err(error) => {
                                                eprintln!(
                                                    "DROPPER BREWING TRANSFER ERROR: {error}"
                                                );
                                                continue;
                                            }
                                        };
                                        let result = (|| -> io::Result<()> {
                                            journal::commit(
                                                &furnace_world_path
                                                    .with_extension("pending-txn.json"),
                                                &[
                                                    (&*ejection_file, updated.encoded()?),
                                                    (
                                                        &*ejection_brewer_file,
                                                        next_brewers.encoded()?,
                                                    ),
                                                ],
                                            )?;
                                            Ok(())
                                        })();
                                        if let Err(error) = result {
                                            eprintln!("DROPPER BREWING JOURNAL ERROR: {error}");
                                            continue;
                                        }
                                        *storage = updated;
                                        *brewers = next_brewers;
                                    }
                                    continue;
                                }
                                let mut ground = match leaf_drops.lock() {
                                    Ok(value) => value,
                                    Err(_) => return,
                                };
                                if ground.is_full() {
                                    continue;
                                }
                                let (next_ground, spawned) =
                                    match ground.stage_put_many(&[stack], dest) {
                                        Ok(result) => result,
                                        Err(error) => {
                                            eprintln!("DEVICE GROUND ITEM ERROR: {error}");
                                            continue;
                                        }
                                    };
                                let device_bytes = match updated.encoded() {
                                    Ok(bytes) => bytes,
                                    Err(error) => {
                                        eprintln!("DEVICE INVENTORY ENCODING ERROR: {error}");
                                        continue;
                                    }
                                };
                                let ground_bytes = match next_ground.encoded() {
                                    Ok(bytes) => bytes,
                                    Err(error) => {
                                        eprintln!("DEVICE ITEM ENCODING ERROR: {error}");
                                        continue;
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
                                    continue;
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
                // Every fifth existing 20TPS tick, check bounded Mob and
                // ground-item contacts. A stationary entity renews hook timers;
                // when it leaves, the normal block_tick releases the signal.
                // World -> Mob -> Ground lock order matches session updates.
                if survival && now % 5 < u64::from(steps) {
                    let positions = match wire_mobs.lock() {
                        Ok(creatures) => creatures.contact_positions(),
                        Err(_) => return,
                    };
                    let drops = match leaf_drops.lock() {
                        Ok(ground) => ground.items().map(|item| item.position).collect::<Vec<_>>(),
                        Err(_) => return,
                    };
                    match refresh_entity_tripwires(
                        &mut gravity_world,
                        &furnace_world_path,
                        &positions,
                        &drops,
                    ) {
                        Ok(updated) => {
                            for (x, y, z) in updated {
                                furnace_hub.broadcast(block_change_event(&gravity_world, x, y, z));
                            }
                        }
                        Err(error) => {
                            eprintln!("ENTITY TRIPWIRE STORAGE ERROR: {error}");
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
        // The timer usually stops between ten-second telemetry reports.
        // Persist that final partial window before the worker is joined.
        if let Err(error) = telemetry.finish(furnace_hub.count().unwrap_or(0)) {
            eprintln!("METRICS FINAL WRITE ERROR: {error}");
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
    let hopper_shutdown = Arc::clone(&shutdown);
    let hopper_thread = thread::spawn(move || {
        let mut last_cycle = hopper_clock.ticks() / 8;
        while !hopper_shutdown.load(Ordering::Acquire) {
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
    let brewing_shutdown = Arc::clone(&shutdown);
    let brewing_thread = thread::spawn(move || {
        let mut last = brewer_clock.ticks();
        while !brewing_shutdown.load(Ordering::Acquire) {
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
    let projectile_cart_file = Arc::clone(&minecart_path);
    let projectile_world_file = Arc::clone(&path);
    let projectile_fishing = Arc::clone(&fishing);
    let projectile_shutdown = Arc::clone(&shutdown);
    let projectile_thread = thread::spawn(move || {
        let mut last = projectile_clock.ticks();
        let mut last_cart_checkpoint = last;
        let mut last_cart_bytes: Option<Vec<u8>> = None;
        while !projectile_shutdown.load(Ordering::Acquire) {
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
                    Ok(mut state) => {
                        let events = state.tick(&world, now);
                        if now.saturating_sub(last_cart_checkpoint) >= 20 {
                            // Persist at most once a second. A stationary
                            // registry does not incur a redundant disk write.
                            let bytes = match state.encoded() {
                                Ok(bytes) => bytes,
                                Err(error) => {
                                    eprintln!("MINECART ENCODING ERROR: {error}");
                                    return;
                                }
                            };
                            if last_cart_bytes.as_ref() != Some(&bytes) {
                                if let Err(error) = journal::commit(
                                    &projectile_world_file.with_extension("pending-txn.json"),
                                    &[(&*projectile_cart_file, bytes.clone())],
                                ) {
                                    eprintln!("MINECART CHECKPOINT ERROR: {error}");
                                    return;
                                }
                                last_cart_bytes = Some(bytes);
                            }
                            last_cart_checkpoint = now;
                        }
                        events
                    }
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
        // A normal /stop also checkpoints the final sub-second position.
        if let Ok(_world) = projectile_world.lock() {
            if let Ok(state) = projectile_carts.lock() {
                match state.encoded() {
                    Ok(bytes) if last_cart_bytes.as_ref() != Some(&bytes) => {
                        if let Err(error) = journal::commit(
                            &projectile_world_file.with_extension("pending-txn.json"),
                            &[(&*projectile_cart_file, bytes)],
                        ) {
                            eprintln!("MINECART FINAL CHECKPOINT ERROR: {error}");
                        }
                    }
                    Err(error) => eprintln!("MINECART FINAL ENCODING ERROR: {error}"),
                    _ => {}
                }
            }
        }
    });
    // Initialize once at startup. A failure here prevents any online-mode bind.
    let server_auth = if online_mode {
        Some(Arc::new(ServerIdentity::generate()?))
    } else {
        None
    };
    listener.set_nonblocking(true)?;
    let mut client_threads: Vec<thread::JoinHandle<()>> = Vec::new();
    let mut shutdown_worker_failure: Option<&'static str> = None;
    while !shutdown.load(Ordering::Acquire) {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
                continue;
            }
            Err(error) => return Err(error),
        };
        // Do not let the listener's nonblocking accept mode leak into client
        // sockets. Status clients send the latency Ping only after reading the
        // Status Response, so an immediate WouldBlock here must not close the
        // connection before that second packet arrives.
        stream.set_nonblocking(false)?;
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        // Reap finished connections and retain handles until they have joined.
        let mut active = Vec::new();
        for worker in client_threads.drain(..) {
            if worker.is_finished() {
                join_shutdown_worker(worker, "client", &mut shutdown_worker_failure);
            } else {
                active.push(worker);
            }
        }
        client_threads = active;
        if shutdown_worker_failure.is_some() {
            // A failed session worker is fatal, but do not abandon live
            // sessions or world workers before they have saved their state.
            shutdown.store(true, Ordering::Release);
            hub.broadcast(Event {
                id: ADMIN_KICK_EVENT,
                body: b"Server shutting down".to_vec(),
            });
            break;
        }
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
        let active_dimensions = dimension_worlds.clone();
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
        let decoration_state = Arc::clone(&decorations);
        let decoration_file = Arc::clone(&decoration_path);
        let map_state = Arc::clone(&maps);
        let map_file = Arc::clone(&map_path);
        let hanging_state = Arc::clone(&hangings);
        let hanging_file = Arc::clone(&hanging_path);
        let peer_difficulty = Arc::clone(&shared_difficulty);
        client_threads.push(thread::spawn(move || {
            let _connection = admission;
            if let Err(e) = handle(
                stream,
                ServerContext {
                    hub: clients,
                    world: w,
                    dimension_worlds: active_dimensions,
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
                    decorations: decoration_state,
                    decoration_path: decoration_file,
                    maps: map_state,
                    map_path: map_file,
                    hangings: hanging_state,
                    hanging_path: hanging_file,
                    survival,
                    hardcore,
                    difficulty: peer_difficulty,
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
        }));
    }
    // No new sessions are admitted after /stop. Drain active sessions first:
    // their disconnect paths persist position, inventory and player state.
    drop(listener);
    for worker in client_threads {
        join_shutdown_worker(worker, "client", &mut shutdown_worker_failure);
    }
    join_shutdown_worker(tick_thread, "tick", &mut shutdown_worker_failure);
    join_shutdown_worker(hopper_thread, "hopper", &mut shutdown_worker_failure);
    join_shutdown_worker(brewing_thread, "brewing", &mut shutdown_worker_failure);
    join_shutdown_worker(
        projectile_thread,
        "projectile",
        &mut shutdown_worker_failure,
    );
    clock.stop();
    join_shutdown_worker(clock_thread, "clock", &mut shutdown_worker_failure);
    if shutdown_worker_failure.is_some() && !console_thread.is_finished() {
        // Emergency shutdown may be triggered without another console line.
        // A blocked stdin read cannot be interrupted portably. The thread
        // checks shutdown before it can execute any more commands; allow the
        // process to exit after flushing rather than deadlocking here.
        drop(console_thread);
    } else {
        join_shutdown_worker(console_thread, "console", &mut shutdown_worker_failure);
    }
    session_guard
        .lock()
        .map_err(|_| bad("session lock mutex poisoned"))?
        .verify()?;
    {
        let world_guard = world.lock().map_err(|_| bad("world lock poisoned"))?;
        world_guard.save(&path)?;
        mobs.lock()
            .map_err(|_| bad("mob lock poisoned"))?
            .save(&world_guard)?;
    }
    // Serialize disjoint files only after gameplay workers and sessions stopped.
    // Existing journal protocol ensures each small group is crash-recoverable.
    let journal_path = path.with_extension("pending-txn.json");
    journal::commit(
        &journal_path,
        &[
            (
                chest_path.as_ref().as_path(),
                chests
                    .lock()
                    .map_err(|_| bad("chests lock poisoned"))?
                    .encoded()?,
            ),
            (
                furnace_path.as_ref().as_path(),
                furnaces
                    .lock()
                    .map_err(|_| bad("furnaces lock poisoned"))?
                    .encoded()?,
            ),
            (
                hopper_path.as_ref().as_path(),
                hoppers
                    .lock()
                    .map_err(|_| bad("hoppers lock poisoned"))?
                    .encoded()?,
            ),
        ],
    )?;
    journal::commit(
        &journal_path,
        &[
            (
                brewer_path.as_ref().as_path(),
                brewers
                    .lock()
                    .map_err(|_| bad("brewers lock poisoned"))?
                    .encoded()?,
            ),
            (
                ground_path.as_ref().as_path(),
                ground_items
                    .lock()
                    .map_err(|_| bad("ground lock poisoned"))?
                    .encoded()?,
            ),
            (
                device_path.as_ref().as_path(),
                devices
                    .lock()
                    .map_err(|_| bad("devices lock poisoned"))?
                    .encoded()?,
            ),
        ],
    )?;
    journal::commit(
        &journal_path,
        &[
            (
                decoration_path.as_ref().as_path(),
                decorations
                    .lock()
                    .map_err(|_| bad("decorations lock poisoned"))?
                    .encoded()?,
            ),
            (
                map_path.as_ref().as_path(),
                maps.lock()
                    .map_err(|_| bad("maps lock poisoned"))?
                    .encoded()?,
            ),
            (
                hanging_path.as_ref().as_path(),
                hangings
                    .lock()
                    .map_err(|_| bad("hangings lock poisoned"))?
                    .encoded()?,
            ),
        ],
    )?;
    world_rules
        .lock()
        .map_err(|_| bad("rules lock poisoned"))?
        .save(&rules_path)?;
    shared_scoreboard
        .lock()
        .map_err(|_| bad("scoreboard lock poisoned"))?
        .save(&scoreboard_path)?;
    clock.save_ticks(&clock_path)?;
    if let Some(worlds) = dimension_worlds.as_ref() {
        // All live world handles are now quiescent. Persist all dimensions
        // together under the root's session.lock after per-world sidecars.
        worlds.save_all()?;
    }
    session_guard
        .lock()
        .map_err(|_| bad("session lock mutex poisoned"))?
        .verify()?;
    if let Some(worker) = shutdown_worker_failure {
        return Err(io::Error::other(format!(
            "shutdown saved state, but worker panicked: {worker}"
        )));
    }
    println!("Server safely stopped; player sessions, workers and persistent state flushed.");
    Ok(())
}
#[cfg(test)]
mod f54_shutdown_worker_tests {
    use super::*;

    #[test]
    fn panic_does_not_skip_joining_remaining_workers() {
        let joined = Arc::new(AtomicUsize::new(0));
        let completed = Arc::clone(&joined);
        let panicking = thread::spawn(|| panic!("F54 synthetic worker crash"));
        let successful = thread::spawn(move || {
            completed.fetch_add(1, Ordering::Release);
        });
        let mut failure = None;
        join_shutdown_worker(panicking, "synthetic", &mut failure);
        join_shutdown_worker(successful, "other", &mut failure);
        assert_eq!(failure, Some("synthetic"));
        assert_eq!(joined.load(Ordering::Acquire), 1);
    }
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
                    nbt: None,
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
                    nbt: None,
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
    fn protocol5_anvil_rename_payload_is_bounded_utf8_and_varint_encoded() {
        let mut valid = Vec::new();
        write_string("Named Sword", &mut valid).unwrap();
        assert_eq!(parse_anvil_name(&valid), Some("Named Sword".into()));
        let mut japanese = Vec::new();
        write_string("名前変更", &mut japanese).unwrap();
        assert_eq!(parse_anvil_name(&japanese), Some("名前変更".into()));
        assert_eq!(parse_anvil_name(&[0]), Some(String::new()));
        assert!(parse_anvil_name(&[10, b't', b'o']).is_none());
        assert!(parse_anvil_name(&[2, 0xff, 0xff]).is_none());
        assert!(parse_anvil_name(&[1, b'A', 0]).is_none());
        assert!(parse_anvil_name(&[1, 10]).is_none());
        let mut too_long = Vec::new();
        write_string(&"a".repeat(31), &mut too_long).unwrap();
        assert!(parse_anvil_name(&too_long).is_none());
        let mut utf16_oversized = Vec::new();
        write_string(&"😀".repeat(16), &mut utf16_oversized).unwrap();
        assert!(parse_anvil_name(&utf16_oversized).is_none());
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
                damage: 0,
                nbt: None,
            })
        );
        none.finish().unwrap();
        // An NBT-bearing client item must not be accepted as a bare stack.
        assert!(Input(&[0, 1, 1, 0, 0, 0, 3, 0, 0, 0]).slot().is_err());
    }
    #[test]
    fn creative_c10_parses_vanilla_hotbar_without_losing_inventory() {
        // Slot 36, stone x64, metadata 0, no NBT.
        let payload = [0, 36, 0, 1, 64, 0, 0, 255, 255];
        let mut input = Input(&payload);
        let slot = input.i16().unwrap();
        let requested = input.creative_slot().unwrap().unwrap();
        input.finish().unwrap();
        let mut inv = Inventory::default();
        assert!(inv.creative_set(slot, requested).unwrap());
        assert_eq!(inv.held().unwrap().id, 1);
        assert_eq!(inv.held().unwrap().count, 64);
        assert_eq!(
            inv.set_slot(36).unwrap(),
            vec![0, 0, 36, 0, 1, 64, 0, 0, 255, 255]
        );
        assert!(inv.encoded().is_ok());

        // Vanilla C10 can clear the same slot with -1 as the slot item ID.
        let mut clear = Input(&[0, 36, 255, 255]);
        let slot = clear.i16().unwrap();
        let requested = clear.creative_slot().unwrap().unwrap();
        clear.finish().unwrap();
        assert!(inv.creative_set(slot, requested).unwrap());
        assert!(inv.held().is_none());
    }

    #[test]
    fn creative_c10_preserves_enchanted_and_unknown_nbt_through_save_and_wire() {
        use fastnbt::Value;
        use std::collections::HashMap;

        let original = Value::Compound(HashMap::from([
            (
                "display".to_owned(),
                Value::Compound(HashMap::from([(
                    "Name".to_owned(),
                    Value::String("Named sword".to_owned()),
                )])),
            ),
            (
                "ench".to_owned(),
                Value::List(vec![Value::Compound(HashMap::from([
                    ("id".to_owned(), Value::Short(16)),
                    ("lvl".to_owned(), Value::Short(4)),
                ]))]),
            ),
            ("UnknownModField".to_owned(), Value::Int(193)),
        ]));
        let source_handle = item_nbt::from_value(&original).unwrap();
        let compressed = item_nbt::wire(source_handle).unwrap();

        // The clientbound S2F slot also provides the exact C10 item layout.
        let mut inv = Inventory::default();
        let item = Stack {
            id: 276,
            count: 1,
            damage: 17,
            nbt: Some(source_handle),
        };
        assert!(inv.creative_set(36, Some(item)).unwrap());
        let slot_packet = inv.set_slot(36).unwrap();
        let mut c10 = Vec::from(36i16.to_be_bytes());
        c10.extend_from_slice(&slot_packet[3..]);
        let mut incoming = Input(&c10);
        assert_eq!(incoming.i16().unwrap(), 36);
        let received = incoming.creative_slot().unwrap().unwrap().unwrap();
        incoming.finish().unwrap();
        assert_eq!(received.id, item.id);
        assert_eq!(received.damage, item.damage);
        assert_eq!(received.count, item.count);
        assert_eq!(item_nbt::wire(received.nbt.unwrap()).unwrap(), compressed);
        assert_eq!(item_nbt::value(received.nbt.unwrap()).unwrap(), original);

        let mut restored = Inventory::default();
        assert!(restored.creative_set(36, Some(received)).unwrap());
        let saved = restored.encoded().unwrap();
        let loaded: Inventory = serde_json::from_slice(&saved).unwrap();
        assert_eq!(loaded.held(), Some(received));
        assert_eq!(loaded.set_slot(36).unwrap(), slot_packet);
    }

    #[test]
    fn creative_c10_rejects_invalid_nbt_and_unregistered_ids_without_mutation() {
        let inventory = Inventory::default();
        let original = inventory.clone();

        // Valid item, but the claimed compressed NBT is not a gzip Compound.
        let mut invalid_tag = Input(&[0, 1, 1, 0, 0, 0, 3, 10, 0, 0]);
        assert_eq!(invalid_tag.creative_slot().unwrap(), None);
        invalid_tag.finish().unwrap();
        assert_eq!(inventory, original);

        // Invalid item identity with an otherwise valid NBT blob: never mint.
        let nbt = item_nbt::from_value(&fastnbt::Value::Compound(std::collections::HashMap::new()))
            .unwrap();
        let bytes = item_nbt::wire(nbt).unwrap();
        let mut claim = Vec::from(167i16.to_be_bytes());
        claim.extend_from_slice(&[1, 0, 0]);
        claim.extend_from_slice(&(bytes.len() as i16).to_be_bytes());
        claim.extend_from_slice(&bytes);
        let mut invalid_id = Input(&claim);
        assert_eq!(invalid_id.creative_slot().unwrap(), None);
        invalid_id.finish().unwrap();
        assert_eq!(inventory, original);
    }

    #[test]
    fn creative_c10_tagged_or_invalid_items_do_not_disconnect_or_mint() {
        // Tagged inventory items cannot be represented as a bare Stack in
        // main yet. Do not disconnect and, especially, do not strip their NBT.
        let tagged = [0, 36, 1, 147, 1, 0, 0, 0, 3, 10, 0, 0];
        let mut input = Input(&tagged);
        assert_eq!(input.i16().unwrap(), 36);
        assert_eq!(input.creative_slot().unwrap(), None);
        input.finish().unwrap();

        // A modern ID or excess count is a rejected creative selection, not
        // an excuse to close a valid player's Protocol 5 connection.
        for item in [
            [0, 36, 1, 167, 1, 0, 0, 255, 255],
            [0, 36, 0, 1, 65, 0, 0, 255, 255],
        ] {
            let mut input = Input(&item);
            assert_eq!(input.i16().unwrap(), 36);
            assert_eq!(input.creative_slot().unwrap(), None);
            input.finish().unwrap();
        }
        let mut inv = Inventory::default();
        let initial = inv.clone();
        assert!(!inv.creative_set(0, None).unwrap());
        assert_eq!(inv, initial);
        let mut malformed = Input(&[0, 1, 1, 0, 0, 255, 254]);
        assert!(malformed.creative_slot().is_err());
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

#[cfg(test)]
mod h74_entity_contact_tests {
    use super::*;

    #[test]
    fn stationary_mob_and_item_trigger_wire_then_release_after_departure() {
        let path = std::env::temp_dir().join(format!(
            "ir1710-h74-entity-contacts-{}.irw",
            std::process::id()
        ));
        let mut w = World::new();
        for x in [-3, 3] {
            w.import_block(x, 64, 0, 1 << 4).unwrap();
        }
        w.import_block(-2, 64, 0, (131 << 4) | 3).unwrap();
        w.import_block(2, 64, 0, (131 << 4) | 1).unwrap();
        for x in -1..=1 {
            w.import_block(x, 64, 0, 132 << 4).unwrap();
        }
        w.save(&path).unwrap();
        let mob = [PlayerPosition {
            x: 0.5,
            y: 64.0,
            z: 0.5,
        }];
        let drops = [[16, 64 * 32 + 16, 16]];
        let changed = refresh_entity_tripwires(&mut w, &path, &mob, &drops).unwrap();
        assert_eq!(changed.len(), 5, "contacts must be deduplicated");
        assert_eq!(w.get(-2, 64, 0) & 8, 8);
        for _ in 0..6 {
            w.block_tick(&path, 64).unwrap();
        }
        assert!(refresh_entity_tripwires(&mut w, &path, &mob, &drops)
            .unwrap()
            .is_empty());
        for _ in 0..9 {
            w.block_tick(&path, 64).unwrap();
        }
        assert_ne!(w.get(-2, 64, 0) & 8, 0);
        w.block_tick(&path, 64).unwrap();
        assert_eq!(w.get(-2, 64, 0) & 8, 0);
        assert_eq!(World::load(&path).unwrap().get(0, 64, 0) & 1, 0);
        std::fs::remove_file(path).ok();
    }
}

#[cfg(test)]
mod b13_trapped_double_chest_tests {
    use super::*;

    #[test]
    fn normal_and_trapped_chests_pair_only_with_their_own_type() {
        let left = (10, 64, 10);
        let right = (11, 64, 10);
        let mut world = World::new();
        for chest_id in [54_u16, 146] {
            world
                .import_block(left.0, left.1, left.2, chest_id << 4)
                .unwrap();
            world
                .import_block(right.0, right.1, right.2, chest_id << 4)
                .unwrap();
            assert_eq!(chest_neighbor(&world, left), Some(right));
            assert_eq!(chest_neighbor(&world, right), Some(left));
        }

        world.import_block(left.0, left.1, left.2, 54 << 4).unwrap();
        world
            .import_block(right.0, right.1, right.2, 146 << 4)
            .unwrap();
        assert_eq!(chest_neighbor(&world, left), None);
        assert_eq!(chest_neighbor(&world, right), None);

        world.import_block(left.0, left.1, left.2, 0).unwrap();
        assert_eq!(chest_neighbor(&world, left), None);
        assert_eq!(chest_neighbor(&world, right), None);
    }

    #[test]
    fn trapped_double_chest_dropper_stages_other_half_without_mutating_source() {
        let left = (20, 64, 20);
        let right = (21, 64, 20);
        let mut world = World::new();
        for (x, y, z) in [left, right] {
            world.import_block(x, y, z, 146 << 4).unwrap();
        }
        let full = Stack {
            id: 4,
            count: 64,
            damage: 0,
            nbt: None,
        };
        let single = Stack { count: 1, ..full };
        let mut right_slots = [Some(full); 27];
        right_slots[0] = None;
        let original = Chests::default()
            .staged_update(left, [Some(full); 27])
            .unwrap()
            .staged_update(right, right_slots)
            .unwrap();
        let partner = chest_neighbor(&world, left);
        assert_eq!(partner, Some(right));
        let mut slots = chest_window_state(&original, left, partner);
        assert_eq!(slots.len(), 54);
        assert!(dispensers::insert_one_into_window(&mut slots, single));
        assert_eq!(slots[27], Some(single));

        let staged = stage_chest_window(&original, left, partner, &slots).unwrap();
        assert_eq!(staged.slots(right)[0], Some(single));
        assert_eq!(original.slots(right)[0], None);
        assert!(staged.encoded().is_ok());

        let mut full_slots = vec![Some(full); 54];
        assert!(!dispensers::insert_one_into_window(&mut full_slots, single));
        assert_eq!(full_slots, vec![Some(full); 54]);
    }
}
