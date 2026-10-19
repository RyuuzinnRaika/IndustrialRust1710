//! Vanilla-only Minecraft 1.7.10 / Protocol 5 server proof of concept.
//! Offline-mode by design: bind to loopback unless you trust every client on your LAN.
//! This is NOT a Forge server, public-server auth, survival engine or Anvil implementation.
mod anvil_export;
mod anvil_import;
mod clock;
mod ground_items;
mod inventory;
mod multiplayer;
mod player;
mod survival;
mod whitelist;
mod world;

use clock::Clock;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use ground_items::GroundItems;
use inventory::{Inventory, Stack};
use legacy_protocol::{
    read_frame, read_varint, write_frame, write_string, write_varint, BufferedFrameReader,
    PROTOCOL_VERSION,
};
use multiplayer::{Event, Hub, JoinError, MAX_PLAYERS};
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
fn join(
    s: &mut TcpStream,
    world: &Mutex<World>,
    clock: &Clock,
    position: &PlayerPosition,
    entity_id: i32,
    inventory: &Inventory,
    survival: bool,
) -> io::Result<()> {
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
    for coordinate in [0i32, 64, 0] {
        spawn.extend_from_slice(&coordinate.to_be_bytes());
    }
    send(s, 0x05, &spawn)?;
    let mut health = Vec::new();
    health.extend_from_slice(&20.0f32.to_be_bytes());
    health.extend_from_slice(&20i16.to_be_bytes());
    health.extend_from_slice(&5.0f32.to_be_bytes());
    send(s, 0x06, &health)?;
    let mut time = Vec::new();
    time.extend_from_slice(&(clock.ticks() as i64).to_be_bytes());
    time.extend_from_slice(&clock.day_time().to_be_bytes());
    send(s, 0x03, &time)?;

    for (cx, cz) in view_chunks(chunk_center(position)) {
        send_chunk(s, world, cx, cz)?;
    }

    // Clientbound 0x30 / 0x09: authoritative Creative inventory and hotbar.
    send(s, 0x30, &inventory.window_items())?;
    send(s, 0x09, &[inventory.selected()])?;
    // Player starts at their last safely persisted position.
    send_player_position(s, position)?;
    as_chat(
        s,
        if survival {
            "Rust survival prototype: basic hand mining and block inventory; no mobs/crafting/physics."
        } else {
            "Rust vanilla prototype: creative flat world. /help for commands."
        },
    )?;
    Ok(())
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
    save_path: &'a Path,
    inventory: &'a mut Inventory,
    inventory_path: &'a Path,
    ground_items: &'a Mutex<GroundItems>,
    ground_path: &'a Path,
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
        save_path: player_path,
        inventory,
        inventory_path,
        ground_items,
        ground_path,
        survival,
        hub,
        entity_id,
        events,
    } = session;
    s.set_read_timeout(Some(Duration::from_millis(50)))?;
    let mut location = (player.x, player.y, player.z);
    let mut loaded_chunks = view_chunks(chunk_center(player));
    let mut yaw: f32 = 0.0;
    let mut pitch: f32 = 0.0;
    let mut last_movement_broadcast = Instant::now() - Duration::from_millis(50);
    let started = Instant::now();
    let mut ping_time = started;
    let mut pong_time = started;
    let mut ping_id: i32 = 0;
    let mut last_time_tick: u64 = clock.ticks();
    let mut admission_check = started;
    let mut saved_at = started;
    // Keep fragmented TCP frames across short receive timeouts.
    let mut packet_reader = BufferedFrameReader::new();
    let mut mining: Option<MiningProgress> = None;
    let mut last_pickup = Instant::now();
    loop {
        while let Ok(event) = events.try_recv() {
            send(s, event.id, &event.body)?;
        }
        if saved_at.elapsed() >= Duration::from_secs(5) {
            player.save(player_path)?;
            inventory.save(inventory_path)?;
            saved_at = Instant::now();
        }
        if survival && last_pickup.elapsed() >= Duration::from_millis(200) {
            let mut ground = ground_items
                .lock()
                .map_err(|_| io::Error::other("ground items lock poisoned"))?;
            if let Some(item) = ground.nearby_ready(location) {
                let mut candidate = inventory.clone();
                if let Some(slot) = candidate.receive_one(item.stack) {
                    // Commit removal to disk before inventory credit to avoid
                    // duplicate pickup after process interruption. This is not
                    // yet a cross-file atomic transaction and can lose items.
                    if let Some(collected) = ground.take(ground_path, item.id)? {
                        candidate.save(inventory_path)?;
                        *inventory = candidate;
                        for packet in collected.collected(entity_id) {
                            hub.broadcast(packet);
                        }
                        send(s, 0x2f, &inventory.set_slot(slot)?)?;
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
        match id {
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
                    as_chat(s, "Commands: /help, /save, /list. Survival prototype supports only simple hand mining; crafting and mobs are unsupported.")?;
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
                    let target = (x, y, z);
                    match action {
                        0 => {
                            let w = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                            mining = if world::in_bounds(x, y, z) && near(location, x, y, z) {
                                MiningProgress::start(target, w.get(x, y, z))
                            } else {
                                None
                            };
                        }
                        1 => mining = None,
                        2 => {
                            let attempt = mining.take();
                            let mut w = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                            let mined = attempt.and_then(|m| m.finish(target, w.get(x, y, z)));
                            let mut changed = false;
                            if world::in_bounds(x, y, z) && near(location, x, y, z) {
                                if let Some(item) = mined {
                                    let mut ground = ground_items.lock().map_err(|_| {
                                        io::Error::other("ground items lock poisoned")
                                    })?;
                                    if !ground.is_full()
                                        && w.update_and_save(world_path, x, y, z, 0)?
                                    {
                                        let stack = Stack {
                                            id: item.id,
                                            count: 1,
                                            damage: item.damage,
                                        };
                                        // World saved first; an interrupted insertion can lose
                                        // the item, never multiply it.
                                        let dropped = ground.put(ground_path, stack, (x, y, z))?;
                                        for packet in dropped.spawn() {
                                            hub.broadcast(packet);
                                        }
                                        changed = true;
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
                            w.update_and_save(world_path, x, y, z, 0)?
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
                            let block_id = stack.id as u16;
                            if world::is_placeable(block_id)
                                && block_id != 7
                                && w.get(x, y, z) != 0
                                && w.get(nx, ny, nz) == 0
                                && near(location, nx, ny, nz)
                            {
                                if survival {
                                    let mut remaining = inventory.clone();
                                    if remaining.consume_held() {
                                        // Persist item consumption before the world edit so a
                                        // crash cannot grant a free block on restart.
                                        remaining.save(inventory_path)?;
                                        match w.update_and_save(
                                            world_path,
                                            nx,
                                            ny,
                                            nz,
                                            (block_id << 4) | (stack.damage as u16 & 15),
                                        ) {
                                            Ok(true) => {
                                                *inventory = remaining;
                                                changed = true;
                                                send(
                                                    s,
                                                    0x2f,
                                                    &inventory.set_slot(inventory.held_slot())?,
                                                )?;
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
                                    changed = w.update_and_save(
                                        world_path,
                                        nx,
                                        ny,
                                        nz,
                                        (block_id << 4) | (stack.damage as u16 & 15),
                                    )?;
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
                // Legacy Click Window transaction semantics are not implemented
                // yet. Reject rather than trust the claimed client inventory.
                let window = input.i8()?;
                input.i16()?; // clicked slot
                input.i8()?; // button
                let action = input.i16()?;
                input.i8()?; // mode
                input.slot()?;
                input.finish()?;
                let mut reply = vec![window as u8];
                reply.extend_from_slice(&action.to_be_bytes());
                reply.push(0); // accepted=false
                send(s, 0x32, &reply)?;
                send(s, 0x30, &inventory.window_items())?;
            }
            13 => {
                input.u8()?; // close player window
                input.finish()?;
            }
            // Other packets are not trusted inventory or world mutations.
            2 | 10..=12 | 15 | 17..=23 => {}
            _ => return Err(bad("invalid play packet ID")),
        }
    }
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
    survival: bool,
}
fn handle(mut s: TcpStream, context: ServerContext) -> io::Result<()> {
    let ServerContext {
        hub, world, path, whitelist_path, clock, ground_items, ground_path, survival,
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
    let mut player_state = match PlayerPosition::load(&player_file) {
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
        &player_state,
        entity_id,
        &inventory_state,
        survival,
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
            save_path: &player_file,
            inventory: &mut inventory_state,
            inventory_path: &inventory_file,
            ground_items: &ground_items,
            ground_path: &ground_path,
            survival,
            hub: &hub,
            entity_id,
            events: &events,
        },
    );
    // Save even on a clean disconnect, timeout, or access revocation.
    let player_saved = player_state.save(&player_file);
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
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" => {
                println!("Usage: vanilla-server [--bind IP:PORT] [--world PATH] [--whitelist FILE] [--insecure-offline-lan] [--survival] [--export-anvil NEW_DIR] [--import-anvil SOURCE_DIR]");
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
    let world = Arc::new(Mutex::new(World::load(&world_path)?));
    let ground_path = Arc::new(GroundItems::location(&world_path));
    let ground_items = Arc::new(Mutex::new(if survival {
        GroundItems::load(&ground_path)?
    } else {
        GroundItems::default()
    }));
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
        thread::spawn(move || {
            let _connection = admission;
            if let Err(e) = handle(stream, ServerContext {
                hub:clients, world:w, path:p, whitelist_path:wl,
                clock:game_clock, ground_items:drops, ground_path:drops_path, survival,
            }) {
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
