//! Vanilla-only Minecraft 1.7.10 / Protocol 5 server proof of concept.
//! Offline-mode by design: bind to loopback unless you trust every client on your LAN.
//! This is NOT a Forge server, public-server auth, survival engine or Anvil implementation.
mod anvil_export;
mod anvil_import;
mod chests;
mod clock;
mod crafting;
mod furnaces;
mod ground_items;
mod inventory;
mod mobs;
mod multiplayer;
mod physics;
mod player;
mod survival;
mod terrain;
mod vitals;
mod whitelist;
mod world;

use chests::{Chests, Position as ChestPosition};
use clock::Clock;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use furnaces::{Furnace, Furnaces, Position as FurnacePosition};
use ground_items::GroundItems;
use inventory::{ChestDirection, Inventory, Stack};
use legacy_protocol::{
    read_frame, read_varint, write_frame, write_string, write_varint, BufferedFrameReader,
    PROTOCOL_VERSION,
};
use mobs::Mobs;
use multiplayer::{Event, Hub, JoinError, MAX_PLAYERS};
use physics::{FallTracker, IdleGravity};
use player::PlayerPosition;
use serde_json::json;
use std::collections::BTreeSet;
use std::io::{self, ErrorKind, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use survival::MiningProgress;
use vitals::Vitals;
use whitelist::Whitelist;
use world::{World, MAX_CHUNK, MIN_CHUNK, VIEW_DISTANCE};

const MAX_CONNECTIONS: usize = 32;
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
    fn finish(&self) -> io::Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(bad("unexpected packet bytes"))
        }
    }
}
fn receive(s: &mut TcpStream) -> io::Result<(i32, Vec<u8>)> {
    let data = read_frame(s)?;
    let mut r = Input(&data);
    let id = r.varint()?;
    Ok((id, r.0.to_vec()))
}
fn send(s: &mut TcpStream, id: u32, body: &[u8]) -> io::Result<()> {
    let mut packet = Vec::with_capacity(5 + body.len());
    write_varint(id, &mut packet);
    packet.extend_from_slice(body);
    write_frame(s, &packet)
}
fn as_chat(s: &mut TcpStream, message: &str) -> io::Result<()> {
    let mut bytes = Vec::new();
    write_string(&json!({"text": message}).to_string(), &mut bytes)?;
    send(s, 0x02, &bytes)
}
fn login_disconnect(s: &mut TcpStream, reason: &str) -> io::Result<()> {
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
fn status(s: &mut TcpStream, hub: &Hub) -> io::Result<()> {
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
}
fn join(
    s: &mut TcpStream,
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
    } = initial;
    // S01 Join Game: entity i32, gamemode u8 (1=creative), dimension i8,
    // difficulty u8, maxPlayers u8, levelType string.
    let mut join = Vec::new();
    join.extend_from_slice(&entity_id.to_be_bytes());
    join.extend_from_slice(&[u8::from(!survival), 0, 1, MAX_PLAYERS as u8]);
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

    for (cx, cz) in view_chunks(chunk_center(position)) {
        send_chunk(s, world, cx, cz)?;
    }

    // Clientbound 0x30 / 0x09: authoritative Creative inventory and hotbar.
    send(s, 0x30, &inventory.window_items())?;
    send(s, 0x2f, &inventory.cursor_packet())?;
    send(s, 0x09, &[inventory.selected()])?;
    // Player starts at their last safely persisted position.
    send_player_position(s, position)?;
    as_chat(
        s,
        if survival {
            "Rust survival: mining, crafting, basic collision, fall damage; no mobs or full physics."
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
fn send_chunk(s: &mut TcpStream, world: &Mutex<World>, cx: i32, cz: i32) -> io::Result<()> {
    let (mask, data) = world
        .lock()
        .map_err(|_| bad("world mutex poisoned"))?
        .chunk_data(cx, cz);
    let mut compressor = ZlibEncoder::new(Vec::new(), Compression::fast());
    compressor.write_all(&data)?;
    let compressed = compressor.finish()?;
    let mut packet = Vec::with_capacity(17 + compressed.len());
    packet.extend_from_slice(&cx.to_be_bytes());
    packet.extend_from_slice(&cz.to_be_bytes());
    packet.push(1);
    packet.extend_from_slice(&mask.to_be_bytes());
    packet.extend_from_slice(&0u16.to_be_bytes());
    packet.extend_from_slice(&(compressed.len() as i32).to_be_bytes());
    packet.extend_from_slice(&compressed);
    send(s, 0x21, &packet)
}
fn unload_chunk(s: &mut TcpStream, cx: i32, cz: i32) -> io::Result<()> {
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
    s: &mut TcpStream,
    world: &Mutex<World>,
    loaded: &mut BTreeSet<(i32, i32)>,
    center: (i32, i32),
) -> io::Result<()> {
    let desired = view_chunks(center);
    for &(cx, cz) in loaded.difference(&desired) {
        unload_chunk(s, cx, cz)?;
    }
    for &(cx, cz) in desired.difference(loaded) {
        send_chunk(s, world, cx, cz)?;
    }
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
fn send_player_position(s: &mut TcpStream, location: &PlayerPosition) -> io::Result<()> {
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
struct PlayerSession<'a> {
    position: &'a mut PlayerPosition,
    vitals: &'a mut Vitals,
    save_path: &'a Path,
    inventory: &'a mut Inventory,
    inventory_path: &'a Path,
    ground_items: &'a Mutex<GroundItems>,
    ground_path: &'a Path,
    mobs: &'a Mutex<Mobs>,
    chests: &'a Mutex<Chests>,
    chest_path: &'a Path,
    furnaces: &'a Mutex<Furnaces>,
    furnace_path: &'a Path,
    survival: bool,
    hub: &'a Hub,
    entity_id: i32,
    events: &'a Receiver<Event>,
}
fn play(
    s: &mut TcpStream,
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
        save_path: player_path,
        inventory,
        inventory_path,
        ground_items,
        ground_path,
        mobs,
        chests,
        chest_path,
        furnaces,
        furnace_path,
        survival,
        hub,
        entity_id,
        events,
    } = session;
    s.set_read_timeout(Some(Duration::from_millis(50)))?;
    let mut location = (player.x, player.y, player.z);
    let mut fall = FallTracker::new(player.y);
    let mut idle_gravity = IdleGravity::new(clock.ticks());
    let mut loaded_chunks = view_chunks(chunk_center(player));
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
    let mut saved_at = started;
    // Keep fragmented TCP frames across short receive timeouts.
    let mut packet_reader = BufferedFrameReader::new();
    let mut mining: Option<MiningProgress> = None;
    let mut active_window: u8 = 0;
    let mut active_chest: Option<ChestPosition> = None;
    let mut active_furnace: Option<FurnacePosition> = None;
    let mut last_furnace_view = Furnace::default();
    let mut observed_chest_revision: u64 = 0;
    let mut next_window: u8 = 1;
    let mut last_pickup = Instant::now();
    let mut eating: Option<(u64, Stack)> = None;
    loop {
        while let Ok(event) = events.try_recv() {
            send(s, event.id, &event.body)?;
        }
        // Poll the single shared chest revision to synchronize viewers.
        // Do not send a chest's window-id to unrelated players.
        if let Some(pos) = active_chest {
            let guard = chests.lock().map_err(|_| bad("chest lock poisoned"))?;
            if guard.revision() != observed_chest_revision {
                let data = inventory.chest_window_items(active_window, &guard.slots(pos));
                observed_chest_revision = guard.revision();
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
                    // Commit removal to disk before inventory credit to avoid
                    // duplicate pickup after process interruption. This is not
                    // yet a cross-file atomic transaction and can lose items.
                    if let Some(collected) = ground.take(ground_path, item.id)? {
                        candidate.save(inventory_path)?;
                        *inventory = candidate;
                        for packet in collected.collected(entity_id) {
                            hub.broadcast(packet);
                        }
                        for slot in slots {
                            send(s, 0x2f, &inventory.set_slot(slot)?)?;
                        }
                    }
                }
            }
            last_pickup = Instant::now();
        }
        if admission_check.elapsed() >= Duration::from_secs(2) {
            // Reload on a bounded interval: file removal, corruption or revocation
            // causes active sessions to be disconnected (fail closed).
            let allowed = Whitelist::load(whitelist_path).is_ok_and(|list| list.allows(name));
            if !allowed {
                let mut reason = Vec::new();
                write_string(
                    &json!({"text":"Whitelist access revoked or unavailable"}).to_string(),
                    &mut reason,
                )?;
                send(s, 0x40, &reason)?;
                return Ok(());
            }
            admission_check = Instant::now();
        }
        let now_tick = clock.ticks();
        // The first active session advances the shared mob simulation.
        if survival && vitals.health > 0.0 {
            let players = hub.positions()?;
            let w = world.lock().map_err(|_| bad("world mutex poisoned"))?;
            let mut creatures = mobs.lock().map_err(|_| bad("mob mutex poisoned"))?;
            let updates = creatures.tick(now_tick, &w, &players);
            let hurt = creatures.hurt_player(*player, now_tick);
            drop(creatures);
            drop(w);
            for update in updates {
                hub.broadcast(update);
            }
            if hurt {
                vitals.health = (vitals.health - 2.0).max(0.0);
                player.save_state(player_path, *vitals)?;
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
            drop(gravity_world);
            if let Some(candidate) = simulated {
                let damage = fall.update(candidate.y, grounded);
                let mut next_vitals = *vitals;
                next_vitals.health = (next_vitals.health - f32::from(damage)).max(0.0);
                // Save accepted position and HP before sending an update.
                candidate.save_state(player_path, next_vitals)?;
                *player = candidate;
                *vitals = next_vitals;
                location = (candidate.x, candidate.y, candidate.z);
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
        // A food right-click starts a 32 tick use animation. The server owns
        // completion, selection, consumption, and the resulting food values.
        if survival && vitals.health > 0.0 {
            if let Some((began, stack)) = eating {
                if now_tick.saturating_sub(began) >= 32 {
                    eating = None;
                    if inventory.held() == Some(stack) && vitals.can_eat(stack) {
                        let mut remaining = inventory.clone();
                        if remaining.consume_held() {
                            let mut after = *vitals;
                            if after.eat(stack) {
                                // Debit inventory before player-NBT food credit.
                                remaining.save(inventory_path)?;
                                player.save_state(player_path, after)?;
                                *inventory = remaining;
                                *vitals = after;
                                send(s, 0x2f, &inventory.set_slot(inventory.held_slot())?)?;
                                send(s, 0x06, &health_packet(*vitals))?;
                            }
                        }
                    }
                }
            }
        }
        if survival && vitals.health <= 0.0 {
            eating = None;
            commit_death_drops(
                inventory,
                inventory_path,
                ground_items,
                ground_path,
                hub,
                location,
            )?;
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
                // Protocol 5 C02 Use Entity: target VarInt and action VarInt.
                let target_id = input.varint()?;
                let action = input.varint()?;
                input.finish()?;
                if survival && vitals.health > 0.0 && action == 1 {
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
                if message == "/help" {
                    as_chat(s, "Commands: /help, /save, /list. Survival supports crafting, inventory, basic collision and fall damage; mobs remain unsupported.")?;
                } else if message == "/list" {
                    as_chat(
                        s,
                        &format!("Online ({}): {}", hub.count()?, hub.names()?.join(", ")),
                    )?;
                } else if message == "/save" {
                    world
                        .lock()
                        .map_err(|_| bad("world mutex poisoned"))?
                        .save(world_path)?;
                    as_chat(s, "World saved.")?;
                } else if !message.starts_with('/') {
                    let mut bytes = Vec::new();
                    write_string(
                        &json!({"text":format!("<{name}> {message}")}).to_string(),
                        &mut bytes,
                    )?;
                    hub.broadcast(Event {
                        id: 0x02,
                        body: bytes,
                    });
                } else {
                    as_chat(s, "Unknown command; try /help")?;
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
                if survival {
                    let collision_world = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                    if !physics::valid_movement(&collision_world, player, &candidate) {
                        send_player_position(s, player)?;
                        continue;
                    }
                    let landed = physics::supported(&collision_world, &candidate);
                    drop(collision_world);
                    let damage = fall.update(y, landed);
                    if damage != 0 {
                        vitals.health = (vitals.health - f32::from(damage)).max(0.0);
                        player.save_state(player_path, *vitals)?;
                        send(s, 0x06, &health_packet(*vitals))?;
                    }
                }
                if survival {
                    let old_food = vitals.food;
                    let old_saturation = vitals.saturation;
                    vitals.travel((x - player.x).hypot(z - player.z));
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
                    if fy >= 0 && player.y - y >= 1.0 {
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
                    let mut w = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                    if world::in_bounds(bx, by, bz) && w.press_plate(world_path, bx, by, bz)? {
                        hub.broadcast(block_change_event(&w, bx, by, bz));
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
            7 => {
                let action = input.i8()?;
                let x = input.i32()?;
                let y = input.u8()? as i32;
                let z = input.i32()?;
                input.i8()?; // face
                input.finish()?;
                if survival {
                    if action == 5 {
                        eating = None; // Release Use Item.
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
                                                .occupied());
                                    if !occupied {
                                        let mut ground = ground_items.lock().map_err(|_| {
                                            io::Error::other("ground items lock poisoned")
                                        })?;
                                        // A filled entity pool must not destroy a
                                        // block that should have created an item.
                                        if result.drop.is_none() || !ground.is_full() {
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
                                                    changed = true;
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
                                        .occupied());
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
                        if inventory.can_place(held) && vitals.can_eat(stack) {
                            eating = Some((now_tick, stack));
                        }
                    }
                    continue;
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
                    if matches!(id, 69 | 77 | 143 | 64 | 96 | 93 | 94) {
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
                    active_furnace = Some(pos);
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
                    active_furnace = None;
                    let guard = chests.lock().map_err(|_| bad("chest lock poisoned"))?;
                    let items = inventory.chest_window_items(active_window, &guard.slots(pos));
                    observed_chest_revision = guard.revision();
                    drop(guard);
                    let mut opened = vec![active_window];
                    write_string("minecraft:chest", &mut opened)?;
                    write_string(&json!({"text":"Chest"}).to_string(), &mut opened)?;
                    opened.push(27); // 3 rows of nine, excluding player inventory
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
                    active_furnace = None;
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
                                326 => 8,
                                327 => 10,
                                295 => 59,  // wheat seeds
                                391 => 141, // carrot
                                392 => 142, // potato
                                356 => 93,  // repeater
                                324 => 64,  // wooden door
                                330 => 71,  // iron door
                                _ => stack.id as u16,
                            };
                            let adjacent_chest = block_id == 54
                                && [(nx + 1, nz), (nx - 1, nz), (nx, nz + 1), (nx, nz - 1)]
                                    .iter()
                                    .any(|&(cx, cz)| w.get(cx, ny, cz) >> 4 == 54);
                            let metadata = if matches!(block_id, 50 | 77 | 143) {
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
                            } else if matches!(block_id, 64 | 71 | 93) {
                                ((yaw.rem_euclid(360.0) / 90.0).round() as u16) & 3
                            } else if matches!(block_id, 59 | 141 | 142 | 8 | 10) {
                                0
                            } else {
                                stack.damage as u16 & 15
                            };
                            if (world::is_placeable(block_id) || block_id == 61)
                                && w.can_support(nx, ny, nz, (block_id << 4) | metadata)
                                && block_id != 7
                                && !adjacent_chest
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
                let _claimed = input.slot()?;
                input.finish()?;
                let requested_window = window >= 0 && window as u8 == active_window;
                let mut accepted = false;
                let response_items;
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
                        // Source-first, analogous to the chest transactions:
                        // preserves at-most-once credit during crashes.
                        if direction == ChestDirection::IntoChest {
                            if inv_changed {
                                candidate.save(inventory_path)?;
                                *inventory = candidate;
                            }
                            if furnace_changed {
                                machines.update(furnace_path, pos, next)?;
                            }
                        } else {
                            if furnace_changed {
                                machines.update(furnace_path, pos, next)?;
                            }
                            if inv_changed {
                                candidate.save(inventory_path)?;
                                *inventory = candidate;
                            }
                        }
                    }
                    last_furnace_view = machines.get(pos);
                    response_items =
                        inventory.furnace_window_items(active_window, &last_furnace_view);
                    drop(machines);
                } else if let Some(pos) = active_chest {
                    let still_open = world
                        .lock()
                        .map_err(|_| bad("world mutex poisoned"))?
                        .get(pos.0, pos.1, pos.2)
                        >> 4
                        == 54
                        && near(location, pos.0, pos.1, pos.2);
                    let mut storage = chests.lock().map_err(|_| bad("chest lock poisoned"))?;
                    let original = storage.slots(pos);
                    let mut changed_slots = original;
                    let mut candidate = inventory.clone();
                    let change = if still_open && requested_window {
                        candidate.chest_click(&mut changed_slots, slot, button, mode)
                    } else {
                        None
                    };
                    if let Some(direction) = change {
                        accepted = true;
                        let inventory_changed = candidate != *inventory;
                        let storage_changed = original != changed_slots;
                        // The transfer source is persisted first, to avoid
                        // item duplication at a crash boundary. Two files
                        // are still not an atomic transaction: loss is possible.
                        if direction == ChestDirection::IntoChest {
                            if inventory_changed {
                                candidate.save(inventory_path)?;
                                *inventory = candidate;
                            }
                            if storage_changed {
                                storage.update(chest_path, pos, changed_slots)?;
                            }
                        } else {
                            if storage_changed {
                                storage.update(chest_path, pos, changed_slots)?;
                            }
                            if inventory_changed {
                                candidate.save(inventory_path)?;
                                *inventory = candidate;
                            }
                        }
                    }
                    observed_chest_revision = storage.revision();
                    response_items =
                        inventory.chest_window_items(active_window, &storage.slots(pos));
                    drop(storage);
                } else {
                    let mut candidate = inventory.clone();
                    accepted = requested_window
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
                    active_furnace = None;
                    send(s, 0x30, &inventory.window_items())?;
                }
            }
            22 => {
                // Protocol 5 C16 Client Status: 0 = Perform Respawn.
                let action = input.i8()?;
                input.finish()?;
                if survival && action == 0 && vitals.health <= 0.0 {
                    commit_death_drops(
                        inventory,
                        inventory_path,
                        ground_items,
                        ground_path,
                        hub,
                        location,
                    )?;
                    let origin = safe_spawn(world)?;
                    let fresh_vitals = Vitals::default();
                    origin.save_state(player_path, fresh_vitals)?;
                    *vitals = fresh_vitals;
                    *player = origin;
                    fall = FallTracker::new(origin.y);
                    idle_gravity = IdleGravity::new(now_tick);
                    eating = None;
                    location = (origin.x, origin.y, origin.z);
                    let mut respawn = 0i32.to_be_bytes().to_vec();
                    respawn.extend_from_slice(&[1, 0]); // normal difficulty, survival
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
/// Debit all player possessions before publishing death drops. A cross-file
/// transaction journal is not yet implemented: a storage fault can lose items,
/// but cannot re-credit the source inventory and duplicate them.
fn commit_death_drops(
    inventory: &mut Inventory,
    inventory_path: &Path,
    ground_items: &Mutex<GroundItems>,
    ground_path: &Path,
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
    empty.save(inventory_path)?;
    *inventory = empty;
    let block = (
        (location.0.floor() as i32).clamp(-world::WORLD_LIMIT, world::WORLD_LIMIT - 1),
        (location.1.floor() as i32).clamp(0, 254),
        (location.2.floor() as i32).clamp(-world::WORLD_LIMIT, world::WORLD_LIMIT - 1),
    );
    for item in ground.put_many(ground_path, &possessions, block)? {
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
    clock: Arc<Clock>,
    ground_items: Arc<Mutex<GroundItems>>,
    ground_path: Arc<PathBuf>,
    mobs: Arc<Mutex<Mobs>>,
    chests: Arc<Mutex<Chests>>,
    chest_path: Arc<PathBuf>,
    furnaces: Arc<Mutex<Furnaces>>,
    furnace_path: Arc<PathBuf>,
    survival: bool,
}
fn handle(mut s: TcpStream, context: ServerContext) -> io::Result<()> {
    let ServerContext {
        hub,
        world,
        path,
        whitelist_path,
        clock,
        ground_items,
        ground_path,
        mobs,
        chests,
        chest_path,
        furnaces,
        furnace_path,
        survival,
    } = context;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    s.set_write_timeout(Some(Duration::from_secs(5)))?;
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
    let player_file = PlayerPosition::location(&path, &name);
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
    let uuid = offline_uuid(&name);
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
    let mut success = Vec::new();
    write_string(&uuid, &mut success)?;
    write_string(&name, &mut success)?;
    send(&mut s, 0x02, &success)?;
    println!("joined: {name} (UNAUTHENTICATED OFFLINE MODE)");
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
        },
    )?;
    // 1.7.10 tab list entries precede other player entity spawns.
    let self_list = multiplayer::list_entry(&name, true)?;
    send(&mut s, self_list.id, &self_list.body)?;
    for other in &previous {
        let list = multiplayer::list_entry(&other.name, true)?;
        send(&mut s, list.id, &list.body)?;
        let spawn = multiplayer::spawn(other)?;
        send(&mut s, spawn.id, &spawn.body)?;
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
        for mob in mobs
            .lock()
            .map_err(|_| bad("mob mutex poisoned"))?
            .spawn_events()
        {
            send(&mut s, mob.id, &mob.body)?;
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
            save_path: &player_file,
            inventory: &mut inventory_state,
            inventory_path: &inventory_file,
            ground_items: &ground_items,
            ground_path: &ground_path,
            mobs: &mobs,
            chests: &chests,
            chest_path: &chest_path,
            furnaces: &furnaces,
            furnace_path: &furnace_path,
            survival,
            hub: &hub,
            entity_id,
            events: &events,
        },
    );
    // Save even on a clean disconnect, timeout, or access revocation.
    let player_saved = player_state.save_state(&player_file, player_vitals);
    let inventory_saved = inventory_state.save(&inventory_file);
    println!("left: {name}");
    player_saved?;
    inventory_saved?;
    result
}
fn main() -> io::Result<()> {
    let mut bind = "127.0.0.1:25565".parse::<SocketAddr>().unwrap();
    let mut world_path = PathBuf::from("world/flat.irw");
    let mut anvil_export_path: Option<PathBuf> = None;
    let mut anvil_import_path: Option<PathBuf> = None;
    let mut whitelist_path = PathBuf::from("whitelist.json");
    let mut insecure_offline_lan = false;
    let mut survival = false;
    let mut requested_seed: Option<u64> = None;
    let mut biome_seed: Option<u64> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" => {
                println!("Usage: vanilla-server [--bind IP:PORT] [--world PATH] [--whitelist FILE] [--insecure-offline-lan] [--survival] [--export-anvil NEW_DIR] [--import-anvil SOURCE_DIR] [--terrain-seed U64] [--biome-seed U64]");
                println!(
                    "Whitelist is checked by player NAME only; offline accounts can be impersonated. Bind defaults to loopback."
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
                    .into()
            }
            "--whitelist" => {
                whitelist_path = args
                    .next()
                    .ok_or_else(|| bad("missing --whitelist value"))?
                    .into()
            }
            "--insecure-offline-lan" => insecure_offline_lan = true,
            "--survival" => survival = true,
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
            _ => return Err(bad("unknown command-line argument")),
        }
    }
    if (requested_seed.is_some() || biome_seed.is_some())
        && (!survival
            || anvil_export_path.is_some()
            || anvil_import_path.is_some()
            || (requested_seed.is_some() && biome_seed.is_some()))
    {
        return Err(bad(
            "specify exactly one seed type with --survival, without import/export",
        ));
    }
    if anvil_export_path.is_some() && anvil_import_path.is_some() {
        return Err(bad(
            "--export-anvil and --import-anvil are mutually exclusive",
        ));
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
        let world = World::load(&world_path)?;
        let count = anvil_export::export(&world, &destination)?;
        println!(
            "Anvil 1.7.10 snapshot exported: {count} chunks to {}",
            destination.display()
        );
        println!("This is a partial flat-world snapshot, not a completed survival world.");
        return Ok(());
    }
    if !bind.ip().is_loopback() && !insecure_offline_lan {
        return Err(io::Error::new(ErrorKind::PermissionDenied,
            "offline-mode whitelist cannot authenticate usernames: non-loopback bind requires explicit --insecure-offline-lan"));
    }
    let list = Whitelist::load(&whitelist_path)?;
    println!(
        "Whitelist: {} permitted name(s) (NOT cryptographic identity authentication)",
        list.len()
    );
    let (clock, _clock_thread) = Clock::start();
    let loaded_world = if world_path.exists() {
        let existing = World::load(&world_path)?;
        if let Some(seed) = requested_seed {
            if existing.natural_seed() != Some(seed) || existing.is_biome_world() {
                return Err(bad("existing world has different generation type or seed"));
            }
        }
        if let Some(seed) = biome_seed {
            if existing.natural_seed() != Some(seed) || !existing.is_biome_world() {
                return Err(bad("existing world is not a matching IRW3 biome world"));
            }
        }
        existing
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
        World::new()
    };
    let world = Arc::new(Mutex::new(loaded_world));
    let mobs = Arc::new(Mutex::new({
        let guard = world.lock().map_err(|_| bad("world mutex poisoned"))?;
        Mobs::new(&guard, survival)
    }));
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
    let listener = TcpListener::bind(bind)?;
    println!(
        "IndustrialRust1710: listening on {} (1.7.10 vanilla, up to 8 players)",
        listener.local_addr()?
    );
    println!("WARNING: offline-mode has no account authentication. Do not expose to the internet.");
    let hub = Arc::new(Hub::default());
    let connections = Arc::new(AtomicUsize::new(0));
    let path = Arc::new(world_path);
    let allow_path = Arc::new(whitelist_path);
    // Furnace simulation is driven by the same monotonic 20Hz clock as the
    // server world. It runs without connected players; it never replays
    // wall-clock downtime after the process was stopped.
    let furnace_worker = Arc::clone(&furnaces);
    let furnace_file = Arc::clone(&furnace_path);
    let furnace_clock = Arc::clone(&clock);
    let furnace_world = Arc::clone(&world);
    let furnace_world_path = Arc::clone(&path);
    let furnace_hub = Arc::clone(&hub);
    thread::spawn(move || {
        let mut last = furnace_clock.ticks();
        let mut last_weather = furnace_clock.weather();
        loop {
            let now = furnace_clock.ticks();
            if now > last {
                let steps = (now - last).min(20) as u32;
                last = now;
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
                    // Queued fluid and redstone updates use the same tick and
                    // save all staged block changes with one world-file commit.
                    match gravity_world.block_tick(&furnace_world_path, 64) {
                        Ok(changed) => {
                            for (x, y, z) in changed {
                                furnace_hub.broadcast(block_change_event(&gravity_world, x, y, z));
                            }
                        }
                        Err(error) => {
                            eprintln!("BLOCK TICK STORAGE ERROR: {error}");
                            return;
                        }
                    }
                }
            }
            thread::sleep(Duration::from_millis(10));
        }
    });
    for accepted in listener.incoming() {
        let stream = accepted?;
        if connections.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
            connections.fetch_sub(1, Ordering::AcqRel);
            continue;
        }
        let admission = ConnectionGuard(Arc::clone(&connections));
        let clients = Arc::clone(&hub);
        let w = Arc::clone(&world);
        let p = Arc::clone(&path);
        let wl = Arc::clone(&allow_path);
        let game_clock = Arc::clone(&clock);
        let drops = Arc::clone(&ground_items);
        let drops_path = Arc::clone(&ground_path);
        let mob_state = Arc::clone(&mobs);
        let storage = Arc::clone(&chests);
        let storage_path = Arc::clone(&chest_path);
        let machine_storage = Arc::clone(&furnaces);
        let machine_file = Arc::clone(&furnace_path);
        thread::spawn(move || {
            let _connection = admission;
            if let Err(e) = handle(
                stream,
                ServerContext {
                    hub: clients,
                    world: w,
                    path: p,
                    whitelist_path: wl,
                    clock: game_clock,
                    ground_items: drops,
                    ground_path: drops_path,
                    mobs: mob_state,
                    chests: storage,
                    chest_path: storage_path,
                    furnaces: machine_storage,
                    furnace_path: machine_file,
                    survival,
                },
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
mod tests {
    use super::*;
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
