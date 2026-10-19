//! Vanilla-only Minecraft 1.7.10 / Protocol 5 server proof of concept.
//! Offline-mode by design: bind to loopback unless you trust every client on your LAN.
//! This is NOT a Forge server, public-server auth, survival engine or Anvil implementation.
mod clock;
mod whitelist;
mod world;

use clock::Clock;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use legacy_protocol::{
    read_frame, read_varint, write_frame, write_string, write_varint, PROTOCOL_VERSION,
};
use serde_json::json;
use std::io::{self, ErrorKind, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use whitelist::Whitelist;
use world::{World, MAX_CHUNK, MIN_CHUNK};

const MAX_CONNECTIONS: usize = 32;
const SPAWN_Y: f64 = 65.0;
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
    fn slot(&mut self) -> io::Result<Option<(u16, u8)>> {
        let block = self.i16()?;
        if block == -1 {
            return Ok(None);
        }
        if block < 0 {
            return Err(bad("invalid item ID"));
        }
        let _count = self.u8()?;
        let meta = self.i16()?;
        let nbt_length = self.i16()?;
        if nbt_length < -1 {
            return Err(bad("invalid NBT length"));
        }
        if nbt_length > 0 {
            self.take(nbt_length as usize)?;
        }
        Ok(Some((block as u16, (meta as u16 & 15) as u8)))
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
fn status(s: &mut TcpStream, occupied: &AtomicBool) -> io::Result<()> {
    let (id, body) = receive(s)?;
    if id != 0 || !body.is_empty() {
        return Err(bad("invalid status request"));
    }
    let status = json!({
        "version":{"name":"1.7.10","protocol":PROTOCOL_VERSION},
        "players":{"max":1,"online":usize::from(occupied.load(Ordering::Acquire)),"sample":[]},
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
fn join(s: &mut TcpStream, world: &Mutex<World>, clock: &Clock) -> io::Result<()> {
    // S01 Join Game: entity i32, gamemode u8 (1=creative), dimension i8,
    // difficulty u8, maxPlayers u8, levelType string.
    let mut join = Vec::new();
    join.extend_from_slice(&1i32.to_be_bytes());
    join.extend_from_slice(&[1, 0, 1, 1]);
    write_string("default", &mut join)?;
    send(s, 0x01, &join)?;

    let mut abilities = vec![0x0d]; // invulnerable, may fly, creative
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

    for cz in MIN_CHUNK..=MAX_CHUNK {
        for cx in MIN_CHUNK..=MAX_CHUNK {
            let uncompressed = world
                .lock()
                .map_err(|_| bad("world mutex poisoned"))?
                .chunk_data(cx, cz);
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
            encoder.write_all(&uncompressed)?;
            let compressed = encoder.finish()?;
            let mut body = Vec::with_capacity(compressed.len() + 20);
            body.extend_from_slice(&cx.to_be_bytes());
            body.extend_from_slice(&cz.to_be_bytes());
            body.push(1); // ground-up
            body.extend_from_slice(&0x0018u16.to_be_bytes()); // y=48..79
            body.extend_from_slice(&0u16.to_be_bytes()); // no Add arrays
            body.extend_from_slice(&(compressed.len() as i32).to_be_bytes());
            body.extend_from_slice(&compressed);
            send(s, 0x21, &body)?;
        }
    }

    // 1.7.x server->client position packet has x/y/z (not stance).
    let mut position = Vec::new();
    position.extend_from_slice(&0.5f64.to_be_bytes());
    position.extend_from_slice(&SPAWN_Y.to_be_bytes());
    position.extend_from_slice(&0.5f64.to_be_bytes());
    position.extend_from_slice(&0f32.to_be_bytes()); // yaw
    position.extend_from_slice(&0f32.to_be_bytes()); // pitch
    position.push(0); // onGround
    send(s, 0x08, &position)?;
    as_chat(
        s,
        "Rust vanilla prototype: creative flat world. /help for commands.",
    )?;
    Ok(())
}
fn block_change(s: &mut TcpStream, world: &World, x: i32, y: i32, z: i32) -> io::Result<()> {
    let block = world.get(x, y, z);
    let mut data = Vec::new();
    data.extend_from_slice(&x.to_be_bytes());
    data.push(y as u8);
    data.extend_from_slice(&z.to_be_bytes());
    write_varint((block >> 4) as u32, &mut data);
    data.push((block & 15) as u8);
    send(s, 0x23, &data)
}
fn near(player: (f64, f64, f64), x: i32, y: i32, z: i32) -> bool {
    let dx = player.0 - (x as f64 + 0.5);
    let dy = player.1 - (y as f64 + 0.5);
    let dz = player.2 - (z as f64 + 0.5);
    dx * dx + dy * dy + dz * dz <= 64.0
}
fn play(
    s: &mut TcpStream,
    world: &Mutex<World>,
    world_path: &Path,
    name: &str,
    whitelist_path: &Path,
    clock: &Clock,
) -> io::Result<()> {
    s.set_read_timeout(Some(Duration::from_secs(1)))?;
    let mut location = (0.5, SPAWN_Y, 0.5);
    let started = Instant::now();
    let mut ping_time = started;
    let mut pong_time = started;
    let mut ping_id: i32 = 0;
    let mut last_time_tick: u64 = clock.ticks();
    let mut admission_check = started;
    loop {
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
        let (id, data) = match receive(s) {
            Ok(packet) => packet,
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::TimedOut | ErrorKind::WouldBlock | ErrorKind::Interrupted
                ) =>
            {
                continue
            }
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };
        let mut input = Input(&data);
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
                    as_chat(s, "Commands: /help, /save. Creative blocks only; mobs and Forge are unsupported.")?;
                } else if message == "/save" {
                    world
                        .lock()
                        .map_err(|_| bad("world mutex poisoned"))?
                        .save(world_path)?;
                    as_chat(s, "World saved.")?;
                } else if !message.starts_with('/') {
                    as_chat(s, &format!("<{name}> {message}"))?;
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
                    input.f32()?;
                    input.f32()?;
                }
                input.u8()?;
                input.finish()?;
                if !x.is_finite()
                    || !y.is_finite()
                    || !z.is_finite()
                    || x.abs() > 128.0
                    || z.abs() > 128.0
                    || !(0.0..=255.0).contains(&y)
                {
                    return Err(bad("invalid player coordinates"));
                }
                location = (x, y, z);
            }
            5 => {
                input.f32()?;
                input.f32()?;
                input.u8()?;
                input.finish()?;
            }
            7 => {
                let action = input.i8()?;
                let x = input.i32()?;
                let y = input.u8()? as i32;
                let z = input.i32()?;
                input.i8()?; // face
                input.finish()?;
                let mut w = world.lock().map_err(|_| bad("world mutex poisoned"))?;
                if (action == 0 || action == 2) && y > 60 && near(location, x, y, z) {
                    w.update_and_save(world_path, x, y, z, 0)?;
                }
                block_change(s, &w, x, y, z)?; // authoritative rollback
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
                    if let Some((block_id, meta)) = held {
                        if world::is_placeable(block_id)
                            && w.get(x, y, z) != 0
                            && w.get(nx, ny, nz) == 0
                            && near(location, nx, ny, nz)
                        {
                            w.update_and_save(
                                world_path,
                                nx,
                                ny,
                                nz,
                                (block_id << 4) | meta as u16,
                            )?;
                        }
                    }
                    if world::in_bounds(nx, ny, nz) {
                        block_change(s, &w, nx, ny, nz)?;
                    }
                }
            }
            // These packets are explicitly not interpreted as trusted mutations.
            2 | 9..=23 => {}
            _ => return Err(bad("invalid play packet ID")),
        }
    }
}
struct Admission(Arc<AtomicBool>);
impl Drop for Admission {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
struct ConnectionGuard(Arc<AtomicUsize>);
impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
fn handle(
    mut s: TcpStream,
    occupied: Arc<AtomicBool>,
    world: Arc<Mutex<World>>,
    path: Arc<PathBuf>,
    whitelist_path: Arc<PathBuf>,
    clock: Arc<Clock>,
) -> io::Result<()> {
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
        return status(&mut s, &occupied);
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
    if occupied
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        login_disconnect(&mut s, "Prototype accepts one player at a time.")?;
        return Ok(());
    }
    let _admission = Admission(occupied);
    let mut success = Vec::new();
    write_string(&offline_uuid(&name), &mut success)?;
    write_string(&name, &mut success)?;
    send(&mut s, 0x02, &success)?;
    println!("joined: {name} (UNAUTHENTICATED OFFLINE MODE)");
    join(&mut s, &world, &clock)?;
    let result = play(&mut s, &world, &path, &name, &whitelist_path, &clock);
    println!("left: {name}");
    result
}
fn main() -> io::Result<()> {
    let mut bind = "127.0.0.1:25565".parse::<SocketAddr>().unwrap();
    let mut world_path = PathBuf::from("world/flat.irw");
    let mut whitelist_path = PathBuf::from("whitelist.json");
    let mut insecure_offline_lan = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" => {
                println!("Usage: vanilla-server [--bind IP:PORT] [--world PATH] [--whitelist FILE] [--insecure-offline-lan]");
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
            _ => return Err(bad("unknown command-line argument")),
        }
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
    let listener = TcpListener::bind(bind)?;
    println!(
        "IndustrialRust1710: listening on {} (1.7.10 vanilla, 1 player)",
        listener.local_addr()?
    );
    println!("WARNING: offline-mode has no account authentication. Do not expose to the internet.");
    let occupied = Arc::new(AtomicBool::new(false));
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
        let clients = Arc::clone(&occupied);
        let w = Arc::clone(&world);
        let p = Arc::clone(&path);
        let wl = Arc::clone(&allow_path);
        let game_clock = Arc::clone(&clock);
        thread::spawn(move || {
            let _connection = admission;
            if let Err(e) = handle(stream, clients, w, p, wl, game_clock) {
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
    fn parses_bounds_and_slot() {
        assert!(Input(&[2, b'a', b'b']).string(1).is_err());
        let mut input = Input(&[0xff, 0xff]);
        assert!(input.slot().unwrap().is_none());
        input.finish().unwrap();
    }
}
