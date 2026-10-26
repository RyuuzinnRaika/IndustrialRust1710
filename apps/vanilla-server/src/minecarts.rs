//! B14: bounded, server-authoritative rideable minecart foundation.
//! Straight/sloped rails, corners and collision guards are supported.
//! Junction switching, vanilla momentum exchange and durable NBT remain TODO.
use crate::multiplayer::{self, Event};
use crate::player::PlayerPosition;
use crate::vehicles::attachment;
use crate::world::World;
use legacy_protocol::write_varint;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

const FIRST_ID: i32 = 1_500_000;
const MAX_CARTS: usize = 64;
const SAVE_VERSION: u8 = 1;
const MAX_SAVE_BYTES: u64 = 64 * 1024;

type RailCell = (i32, i32, i32);
type RailSample = (u16, u16, f64, bool, RailCell);

#[derive(Clone, Debug)]
struct Cart {
    id: i32,
    position: PlayerPosition,
    speed: f64,
    direction: i8,
    east_west: bool,
    rider: Option<i32>,
    last_tick: u64,
}

fn rail(world: &World, pos: PlayerPosition) -> Option<(u16, u16)> {
    let (x, y, z) = (
        pos.x.floor() as i32,
        (pos.y - 0.5).floor() as i32,
        pos.z.floor() as i32,
    );
    let state = world.get(x, y, z);
    let id = state >> 4;
    (matches!(id, 27 | 28 | 66 | 157) && rail_exits(id, state & 15).is_some())
        .then_some((id, state & 15))
}

fn straight_axis(id: u16, meta: u16) -> Option<bool> {
    match if id == 66 { meta } else { meta & 7 } {
        0 | 4 | 5 => Some(false), // north/south, ascending N/S
        1..=3 => Some(true),      // east/west, ascending E/W
        _ => None,                // corners and switches need an explicit turn solver
    }
}

/** Each rail end points to a neighboring cell (with an elevation offset).
 * Only the ordinary rail block (66) supports curves 6..=9 in 1.7.10.
 */
fn rail_exits(id: u16, meta: u16) -> Option<[(i32, i32, i32); 2]> {
    let shape = if id == 66 { meta } else { meta & 7 };
    Some(match shape {
        0 => [(0, 0, -1), (0, 0, 1)],
        1 => [(-1, 0, 0), (1, 0, 0)],
        2 => [(-1, 0, 0), (1, 1, 0)],
        3 => [(-1, 1, 0), (1, 0, 0)],
        4 => [(0, 1, -1), (0, 0, 1)],
        5 => [(0, 0, -1), (0, 1, 1)],
        6 if id == 66 => [(0, 0, 1), (1, 0, 0)],
        7 if id == 66 => [(0, 0, 1), (-1, 0, 0)],
        8 if id == 66 => [(0, 0, -1), (-1, 0, 0)],
        9 if id == 66 => [(0, 0, -1), (1, 0, 0)],
        _ => return None,
    })
}

/// Validate both reciprocal endpoints and matching heights before crossing
/// a cell boundary; contact with an unconnected perpendicular rail is unsafe.
fn tracks_connected(
    from: (i32, i32, i32),
    from_id: u16,
    from_meta: u16,
    to: (i32, i32, i32),
    to_id: u16,
    to_meta: u16,
) -> bool {
    let (Some(first), Some(second)) = (rail_exits(from_id, from_meta), rail_exits(to_id, to_meta))
    else {
        return false;
    };
    first.iter().any(|&(dx, dy, dz)| {
        (from.0 + dx, from.2 + dz) == (to.0, to.2)
            && second.iter().any(|&(rx, ry, rz)| {
                (to.0 + rx, to.2 + rz) == (from.0, from.2) && from.1 + dy == to.1 + ry
            })
    })
}

/// Rail center height in world coordinates. Ascending rails climb one
/// block over the width of the rail cell. Player-position Y is rail
/// baseline + 0.5, consistent with normal minecart placement.
fn rail_height(y: i32, id: u16, meta: u16, x: f64, z: f64) -> f64 {
    let fraction_x = x.rem_euclid(1.0);
    let fraction_z = z.rem_euclid(1.0);
    let slope = match if id == 66 { meta } else { meta & 7 } {
        2 => fraction_x,
        3 => 1.0 - fraction_x,
        4 => 1.0 - fraction_z,
        5 => fraction_z,
        _ => 0.0,
    };
    f64::from(y) + 0.5 + slope
}

/// Find the actual rail in a column, including a high end of a slope
/// whose rail block is one Y below the cart's current feet position.
/// Refuse transitions with a vertical gap or a perpendicular rail.
fn straight_track(world: &World, at: PlayerPosition, axis: Option<bool>) -> Option<RailSample> {
    let (x, z) = (at.x.floor() as i32, at.z.floor() as i32);
    let base = (at.y - 0.5).floor() as i32;
    let mut selected: Option<(RailSample, f64)> = None;
    for y in [base, base - 1, base + 1] {
        let block = world.get(x, y, z);
        let (id, meta) = (block >> 4, block & 15);
        if !matches!(id, 27 | 28 | 66 | 157) {
            continue;
        }
        let curved = id == 66 && (6..=9).contains(&meta);
        let east_west = if curved {
            // A corner has both axes: preserve the current one until its center.
            axis.unwrap_or(true)
        } else {
            let Some(straight) = straight_axis(id, meta) else {
                continue;
            };
            if axis.is_some_and(|wanted| wanted != straight) {
                continue;
            }
            straight
        };
        let height = rail_height(y, id, meta, at.x, at.z);
        let distance = (height - at.y).abs();
        if distance > 0.51 {
            continue;
        }
        if selected.is_none_or(|candidate| distance < candidate.1) {
            selected = Some(((id, meta, height, east_west, (x, y, z)), distance));
        }
    }
    selected.map(|(sample, _)| sample)
}

impl Cart {
    fn spawn(&self) -> Event {
        let mut body = Vec::new();
        write_varint(self.id as u32, &mut body);
        body.push(10); // Protocol 5 object type: minecart
        for value in [self.position.x, self.position.y, self.position.z] {
            body.extend_from_slice(&((value * 32.0).floor() as i32).to_be_bytes());
        }
        body.extend_from_slice(&[0, 0]); // pitch, yaw
        body.extend_from_slice(&0i32.to_be_bytes()); // rideable minecart subtype
        Event { id: 0x0e, body }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredCart {
    id: i32,
    position: PlayerPosition,
    speed: f64,
    direction: i8,
    east_west: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CartSave {
    version: u8,
    next: i32,
    carts: Vec<StoredCart>,
}

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, reason)
}

#[derive(Clone, Default)]
pub struct Minecarts {
    next: i32,
    carts: BTreeMap<i32, Cart>,
}

impl Minecarts {
    pub fn location(world_path: &Path) -> PathBuf {
        world_path.with_extension("minecarts.json")
    }

    /// Persist only permanent vehicle data. Rider entity IDs are session-local
    /// and must never attach a new player to a cart after a server restart.
    pub fn encoded(&self) -> io::Result<Vec<u8>> {
        if self.carts.len() > MAX_CARTS
            || self.next < 0
            || FIRST_ID.checked_add(self.next).is_none()
        {
            return Err(invalid("minecart registry exceeds bounds"));
        }
        let carts = self
            .carts
            .values()
            .map(|cart| StoredCart {
                id: cart.id,
                position: cart.position,
                speed: cart.speed,
                direction: cart.direction,
                east_west: cart.east_west,
            })
            .collect();
        let snapshot = CartSave {
            version: SAVE_VERSION,
            next: self.next,
            carts,
        };
        Self::validate_snapshot(&snapshot)?;
        let bytes = serde_json::to_vec(&snapshot)?;
        if bytes.len() as u64 > MAX_SAVE_BYTES {
            return Err(invalid("minecart snapshot is too large"));
        }
        Ok(bytes)
    }

    fn validate_snapshot(snapshot: &CartSave) -> io::Result<()> {
        if snapshot.version != SAVE_VERSION || snapshot.next < 0 || snapshot.carts.len() > MAX_CARTS
        {
            return Err(invalid("invalid minecart snapshot metadata"));
        }
        let next = FIRST_ID
            .checked_add(snapshot.next)
            .ok_or_else(|| invalid("minecart identifier overflow"))?;
        let mut previous = FIRST_ID - 1;
        for cart in &snapshot.carts {
            if cart.id <= previous
                || cart.id < FIRST_ID
                || cart.id >= next
                || !cart.position.is_valid()
                || !cart.speed.is_finite()
                || cart.speed.abs() > 0.4
                || !matches!(cart.direction, -1 | 1)
            {
                return Err(invalid("invalid or duplicate saved minecart"));
            }
            previous = cart.id;
        }
        Ok(())
    }

    /// Load a bounded versioned snapshot or start empty for old worlds. The
    /// caller runs journal recovery first; malformed saves fail closed.
    pub fn load(path: &Path, tick: u64) -> io::Result<Self> {
        let len = match fs::metadata(path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        if len > MAX_SAVE_BYTES {
            return Err(invalid("minecart file is too large"));
        }
        let bytes = fs::read(path)?;
        if bytes.len() as u64 > MAX_SAVE_BYTES {
            return Err(invalid("minecart file is too large"));
        }
        let snapshot: CartSave = serde_json::from_slice(&bytes)?;
        Self::validate_snapshot(&snapshot)?;
        let carts = snapshot
            .carts
            .into_iter()
            .map(|saved| {
                (
                    saved.id,
                    Cart {
                        id: saved.id,
                        position: saved.position,
                        speed: saved.speed,
                        direction: saved.direction,
                        east_west: saved.east_west,
                        rider: None,
                        last_tick: tick,
                    },
                )
            })
            .collect();
        Ok(Self {
            next: snapshot.next,
            carts,
        })
    }

    pub fn can_place(&self, world: &World, at: PlayerPosition) -> bool {
        self.carts.len() < MAX_CARTS
            && self.next.checked_add(FIRST_ID).is_some()
            && at.is_valid()
            && rail(world, at).is_some()
            && self.carts.values().all(|other| {
                (other.position.x - at.x).hypot(other.position.z - at.z) > 1.0
                    || (other.position.y - at.y).abs() > 1.0
            })
    }

    pub fn place(&mut self, world: &World, at: PlayerPosition, tick: u64) -> Option<Event> {
        if !self.can_place(world, at) {
            return None;
        }
        let id = FIRST_ID.checked_add(self.next)?;
        self.next = self.next.checked_add(1)?;
        let (x, y, z) = (
            at.x.floor() as i32,
            (at.y - 0.5).floor() as i32,
            at.z.floor() as i32,
        );
        let state = world.get(x, y, z);
        let at = PlayerPosition {
            y: rail_height(y, state >> 4, state & 15, at.x, at.z),
            ..at
        };
        let cart = Cart {
            id,
            position: at,
            speed: 0.0,
            direction: 1,
            east_west: straight_axis(state >> 4, state & 15).unwrap_or(false),
            rider: None,
            last_tick: tick,
        };
        let spawned = cart.spawn();
        self.carts.insert(id, cart);
        Some(spawned)
    }

    pub fn mount(&mut self, id: i32, rider: i32, position: PlayerPosition) -> Option<Event> {
        // Non-finite coordinates bypass ordinary distance comparisons (NaN
        // compares false), allowing a malformed position to mount remotely.
        if !position.is_valid() || self.cart_for(rider).is_some() {
            return None;
        }
        let cart = self.carts.get_mut(&id)?;
        if cart.rider.is_some()
            || (cart.position.x - position.x).hypot(cart.position.z - position.z) > 4.0
            || (cart.position.y - position.y).abs() > 3.0
        {
            return None;
        }
        cart.rider = Some(rider);
        Some(attachment(rider, id))
    }

    pub fn cart_for(&self, rider: i32) -> Option<i32> {
        self.carts
            .values()
            .find(|c| c.rider == Some(rider))
            .map(|c| c.id)
    }

    pub fn rider_position(&self, rider: i32) -> Option<PlayerPosition> {
        self.carts
            .values()
            .find(|c| c.rider == Some(rider))
            .map(|c| PlayerPosition {
                y: c.position.y + 0.1,
                ..c.position
            })
    }

    pub fn dismount(&mut self, rider: i32) -> Option<Event> {
        let cart = self.carts.values_mut().find(|c| c.rider == Some(rider))?;
        cart.rider = None;
        Some(attachment(rider, -1))
    }

    /// A bounded push along the rail axis. Never accepts a client position.
    pub fn control(&mut self, rider: i32, forward: f32, heading: f32) -> bool {
        // Reject malformed or out-of-range client movement rather than
        // letting a large finite input inject an impossible cart impulse.
        if !forward.is_finite() || !heading.is_finite() || !(-1.0..=1.0).contains(&forward) {
            return false;
        }
        let Some(cart) = self.carts.values_mut().find(|c| c.rider == Some(rider)) else {
            return false;
        };
        // Minecraft yaw: 0=south, 90=west, 180=north,
        // 270=east. Steering direction must follow the rail axis.
        let yaw = heading.rem_euclid(360.0);
        cart.direction = if cart.east_west {
            if yaw < 180.0 {
                -1
            } else {
                1
            }
        } else if !(90.0..270.0).contains(&yaw) {
            1
        } else {
            -1
        };
        if forward.abs() > 0.01 {
            cart.speed = (cart.speed + f64::from(forward) * 0.012).clamp(-0.4, 0.4);
        }
        true
    }

    pub fn tick(&mut self, world: &World, now: u64) -> Vec<(PlayerPosition, Event)> {
        let mut events = Vec::new();
        // Use a bounded snapshot, updated after each cart moves, so no two
        // server-authoritative carts can occupy the same physical volume.
        let mut occupied: BTreeMap<_, _> = self
            .carts
            .iter()
            .map(|(&id, cart)| (id, cart.position))
            .collect();
        for cart in self.carts.values_mut() {
            let steps = now.saturating_sub(cart.last_tick).min(20);
            if steps == 0 {
                continue;
            }
            cart.last_tick = now;
            for _ in 0..steps {
                let Some((id, meta, height, east_west, current_cell)) =
                    straight_track(world, cart.position, Some(cart.east_west))
                else {
                    cart.speed = 0.0;
                    break;
                };
                cart.position.y = height;
                cart.east_west = east_west;
                if id == 27 {
                    if meta & 8 != 0 {
                        let drive = if cart.speed < 0.0 { -1.0 } else { 1.0 };
                        cart.speed = (cart.speed + drive * 0.06).clamp(-0.4, 0.4);
                    } else {
                        cart.speed *= 0.5;
                    }
                } else {
                    cart.speed *= if cart.rider.is_some() { 0.995 } else { 0.96 };
                }
                if cart.speed.abs() < 0.002 {
                    cart.speed = 0.0;
                    break;
                }
                let advance = cart.speed * f64::from(cart.direction);
                let mut next = PlayerPosition {
                    x: cart.position.x + if east_west { advance } else { 0.0 },
                    z: cart.position.z + if east_west { 0.0 } else { advance },
                    ..cart.position
                };
                let mut next_axis = east_west;
                let mut next_direction = cart.direction;
                if id == 66 && (6..=9).contains(&meta) {
                    let center = if east_west {
                        f64::from(current_cell.0) + 0.5
                    } else {
                        f64::from(current_cell.2) + 0.5
                    };
                    let coordinate = if east_west {
                        cart.position.x
                    } else {
                        cart.position.z
                    };
                    let to_center = center - coordinate;
                    // Turn at the center, continuing with any distance left
                    // in this tick along the connected outgoing half-track.
                    if to_center.abs() > 1e-9
                        && to_center.signum() == advance.signum()
                        && to_center.abs() <= advance.abs()
                    {
                        let exits = rail_exits(id, meta).expect("valid corner shape");
                        let entry_sign = -advance.signum() as i32;
                        let has_entry = exits.iter().any(|&(dx, _, dz)| {
                            if east_west {
                                dx == entry_sign
                            } else {
                                dz == entry_sign
                            }
                        });
                        if !has_entry {
                            cart.speed = 0.0;
                            break;
                        }
                        let outgoing = exits
                            .iter()
                            .find(|&&(dx, _, _)| (dx != 0) != east_west)
                            .expect("corner has a perpendicular exit");
                        let remainder = advance.abs() - to_center.abs();
                        next = cart.position;
                        if east_west {
                            next.x = center;
                            next.z += f64::from(outgoing.2) * remainder;
                        } else {
                            next.z = center;
                            next.x += f64::from(outgoing.0) * remainder;
                        }
                        next_axis = !east_west;
                        let outbound = if next_axis { outgoing.0 } else { outgoing.2 };
                        next_direction = (f64::from(outbound) * cart.speed.signum()) as i8;
                    }
                }
                // Uphill/downhill transitions must physically meet; a rail
                // one block above or below cannot teleport the cart.
                let Some((next_id, next_meta, next_height, _, next_cell)) =
                    straight_track(world, next, Some(next_axis))
                else {
                    cart.speed = 0.0;
                    break;
                };
                if current_cell != next_cell
                    && !tracks_connected(current_cell, id, meta, next_cell, next_id, next_meta)
                {
                    cart.speed = 0.0;
                    break;
                }
                if (next_height - cart.position.y).abs() > 0.51 {
                    cart.speed = 0.0;
                    break;
                }
                let next = PlayerPosition {
                    y: next_height,
                    ..next
                };
                if !next.is_valid() {
                    cart.speed = 0.0;
                    break;
                }
                let collision = occupied.iter().any(|(&id, position)| {
                    id != cart.id
                        && (position.x - next.x).abs() < 0.9
                        && (position.y - next.y).abs() < 0.75
                        && (position.z - next.z).abs() < 0.9
                });
                if collision {
                    cart.speed = 0.0;
                    break;
                }
                cart.position = next;
                cart.east_west = next_axis;
                cart.direction = next_direction;
                occupied.insert(cart.id, next);
            }
            events.push((
                cart.position,
                multiplayer::teleport(cart.id, &cart.position, 0.0, 0.0),
            ));
        }
        events
    }

    pub fn snapshot(&self, observer: PlayerPosition) -> Vec<Event> {
        let mut events = Vec::new();
        for cart in self
            .carts
            .values()
            .filter(|c| multiplayer::visible(&observer, &c.position))
        {
            events.push(cart.spawn());
            if let Some(rider) = cart.rider {
                events.push(attachment(rider, cart.id));
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn durable_snapshot_restores_carts_without_reusing_player_attachment() {
        let path = std::env::temp_dir().join(format!("ir1710-carts-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut world = World::new();
        world.import_block(0, 65, 0, (66 << 4) | 1).unwrap();
        let position = PlayerPosition {
            x: 0.5,
            y: 65.5,
            z: 0.5,
        };
        let mut carts = Minecarts::default();
        assert!(carts.place(&world, position, 10).is_some());
        assert!(carts.mount(FIRST_ID, 22, position).is_some());
        carts.carts.get_mut(&FIRST_ID).unwrap().speed = 0.3;
        std::fs::write(&path, carts.encoded().unwrap()).unwrap();
        let mut restored = Minecarts::load(&path, 500).unwrap();
        let saved = restored.carts.get(&FIRST_ID).unwrap();
        assert_eq!(saved.speed, 0.3);
        assert_eq!(saved.position, position);
        assert_eq!(saved.last_tick, 500);
        assert_eq!(saved.rider, None);
        assert!(restored.place(&world, position, 500).is_none());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn corrupted_and_duplicate_cart_entries_fail_closed() {
        let path =
            std::env::temp_dir().join(format!("ir1710-carts-corrupt-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        for malformed in [
            r#"{"version":1,"next":1,"carts":[{"id":1500000,"position":{"x":0.5,"y":65.5,"z":0.5},"speed":0.1,"direction":1,"east_west":false},{"id":1500000,"position":{"x":0.5,"y":65.5,"z":0.5},"speed":0.1,"direction":1,"east_west":false}]}"#,
            r#"{"version":1,"next":1,"carts":[{"id":1500000,"position":{"x":0.5,"y":65.5,"z":0.5},"speed":99.0,"direction":1,"east_west":false}]}"#,
            r#"{"version":2,"next":0,"carts":[]}"#,
        ] {
            std::fs::write(&path, malformed).unwrap();
            assert!(Minecarts::load(&path, 10).is_err());
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn ascending_east_connects_lower_and_upper_tracks() {
        let mut world = World::new();
        world.import_block(0, 64, 0, (27 << 4) | 9).unwrap();
        world.import_block(1, 64, 0, (27 << 4) | 10).unwrap();
        world.import_block(2, 65, 0, (27 << 4) | 9).unwrap();
        let at = PlayerPosition {
            x: 0.8,
            y: 64.5,
            z: 0.5,
        };
        let mut carts = Minecarts::default();
        carts.place(&world, at, 0).unwrap();
        let cart = carts.carts.get_mut(&FIRST_ID).unwrap();
        cart.speed = 0.34;
        cart.direction = 1;
        for tick in 1..7 {
            carts.tick(&world, tick);
        }
        let at = carts.carts[&FIRST_ID].position;
        assert!(at.x > 2.0, "cart failed to climb: {}", at.x);
        assert!(
            (at.y - 65.5).abs() < 0.05,
            "incorrect slope height: {}",
            at.y
        );
    }

    #[test]
    fn slope_rejects_unconnected_one_block_jump() {
        let mut world = World::new();
        world.import_block(0, 64, 0, (27 << 4) | 9).unwrap();
        world.import_block(1, 65, 0, (27 << 4) | 9).unwrap();
        let at = PlayerPosition {
            x: 0.85,
            y: 64.5,
            z: 0.5,
        };
        let mut carts = Minecarts::default();
        carts.place(&world, at, 0).unwrap();
        let cart = carts.carts.get_mut(&FIRST_ID).unwrap();
        cart.speed = 0.35;
        carts.tick(&world, 1);
        assert!(carts.carts[&FIRST_ID].position.x < 1.0);
        assert_eq!(carts.carts[&FIRST_ID].speed, 0.0);
    }

    #[test]
    fn minecart_stops_before_perpendicular_unconnected_track() {
        let mut world = World::new();
        world.import_block(0, 65, 0, (27 << 4) | 9).unwrap();
        world.import_block(1, 65, 0, (27 << 4) | 8).unwrap();
        let origin = PlayerPosition {
            x: 0.85,
            y: 65.5,
            z: 0.5,
        };
        let mut carts = Minecarts::default();
        carts.place(&world, origin, 0).unwrap();
        let cart = carts.carts.get_mut(&FIRST_ID).unwrap();
        cart.speed = 0.35;
        cart.direction = 1;
        carts.tick(&world, 1);
        assert!((carts.carts[&FIRST_ID].position.x - origin.x).abs() < f64::EPSILON);
        assert_eq!(carts.carts[&FIRST_ID].speed, 0.0);
    }

    #[test]
    fn colliding_minecarts_stop_before_overlapping() {
        let mut world = World::new();
        for x in 0..4 {
            // Keep the target cart parked on an ordinary rail; powered
            // rails accelerate even an unridden cart in this simulation.
            let rail = if x == 2 { (66 << 4) | 1 } else { (27 << 4) | 9 };
            world.import_block(x, 65, 0, rail).unwrap();
        }
        let mut carts = Minecarts::default();
        let left = PlayerPosition {
            x: 0.5,
            y: 65.5,
            z: 0.5,
        };
        let right = PlayerPosition {
            x: 2.5,
            y: 65.5,
            z: 0.5,
        };
        carts.place(&world, left, 0).unwrap();
        carts.place(&world, right, 0).unwrap();
        carts.carts.get_mut(&FIRST_ID).unwrap().speed = 0.4;
        // Powered rails may re-accelerate a blocked cart on later ticks;
        // the safety property is that the vehicles never overlap.
        for tick in 1..40 {
            carts.tick(&world, tick);
            let first = &carts.carts[&FIRST_ID];
            let other = &carts.carts[&(FIRST_ID + 1)];
            assert!(
                other.position.x - first.position.x >= 0.9,
                "overlapping carts after tick {tick}"
            );
        }
    }

    #[test]
    fn player_yaw_respects_east_west_and_north_south_tracks() {
        let mut world = World::new();
        world.import_block(0, 65, 0, (27 << 4) | 9).unwrap();
        world.import_block(0, 65, 1, (27 << 4) | 8).unwrap();
        let mut east_west = Minecarts::default();
        let at = PlayerPosition {
            x: 0.5,
            y: 65.5,
            z: 0.5,
        };
        east_west.place(&world, at, 0).unwrap();
        east_west.mount(FIRST_ID, 22, at).unwrap();
        assert!(east_west.control(22, 1.0, 90.0));
        assert_eq!(east_west.carts[&FIRST_ID].direction, -1);
        assert!(east_west.control(22, 1.0, 270.0));
        assert_eq!(east_west.carts[&FIRST_ID].direction, 1);

        let mut north_south = Minecarts::default();
        let at = PlayerPosition {
            x: 0.5,
            y: 65.5,
            z: 1.5,
        };
        north_south.place(&world, at, 0).unwrap();
        north_south.mount(FIRST_ID, 23, at).unwrap();
        assert!(north_south.control(23, 1.0, 0.0));
        assert_eq!(north_south.carts[&FIRST_ID].direction, 1);
        assert!(north_south.control(23, 1.0, 180.0));
        assert_eq!(north_south.carts[&FIRST_ID].direction, -1);
    }

    #[test]
    fn all_four_vanilla_curves_turn_at_the_center() {
        for (shape, incoming_x, outgoing_z) in [(6, 1, 1), (7, -1, 1), (8, -1, -1), (9, 1, -1)] {
            let mut world = World::new();
            world.import_block(0, 65, 0, (66 << 4) | shape).unwrap();
            world
                .import_block(incoming_x, 65, 0, (27 << 4) | 9)
                .unwrap();
            for distance in 1..=3 {
                world
                    .import_block(0, 65, outgoing_z * distance, (27 << 4) | 8)
                    .unwrap();
            }
            let origin = PlayerPosition {
                x: f64::from(incoming_x) + 0.5,
                y: 65.5,
                z: 0.5,
            };
            let mut carts = Minecarts::default();
            carts.place(&world, origin, 0).unwrap();
            let cart = carts.carts.get_mut(&FIRST_ID).unwrap();
            cart.speed = 0.4;
            cart.direction = -incoming_x as i8;
            for tick in 1..8 {
                carts.tick(&world, tick);
            }
            let result = &carts.carts[&FIRST_ID];
            assert!(
                (result.position.x - 0.5).abs() < 1e-6,
                "corner {shape} left its centerline: {:?}",
                result.position
            );
            assert!(
                (result.position.z - 0.5) * f64::from(outgoing_z) > 0.5,
                "corner {shape} did not turn: {:?}",
                result.position
            );
            assert_eq!(result.position.y, 65.5);
        }
    }

    #[test]
    fn curve_rejects_nonreciprocal_neighbor() {
        let mut world = World::new();
        world.import_block(-1, 65, 0, (27 << 4) | 9).unwrap();
        world.import_block(0, 65, 0, (66 << 4) | 6).unwrap(); // east and south, no west
        let at = PlayerPosition {
            x: -0.2,
            y: 65.5,
            z: 0.5,
        };
        let mut carts = Minecarts::default();
        carts.place(&world, at, 0).unwrap();
        let cart = carts.carts.get_mut(&FIRST_ID).unwrap();
        cart.speed = 0.35;
        cart.direction = 1;
        carts.tick(&world, 1);
        assert_eq!(carts.carts[&FIRST_ID].position.x, at.x);
        assert_eq!(carts.carts[&FIRST_ID].speed, 0.0);
    }

    #[test]
    fn minecart_placement_rejects_invalid_rail_shapes() {
        let at = PlayerPosition {
            x: 0.5,
            y: 65.5,
            z: 0.5,
        };
        let mut world = World::new();
        // Ordinary rails only have shapes 0..=9. Powered, detector and
        // activator rails only have shapes 0..=5 (bit 3 is their power flag).
        for (id, meta) in [
            (66u16, 10u16),
            (66, 15),
            (27, 6),
            (27, 7),
            (28, 6),
            (157, 7),
        ] {
            world.import_block(0, 65, 0, (id << 4) | meta).unwrap();
            let mut carts = Minecarts::default();
            assert!(!carts.can_place(&world, at), "invalid rail {id}:{meta}");
            assert!(
                carts.place(&world, at, 0).is_none(),
                "invalid rail {id}:{meta}"
            );
            assert!(carts.carts.is_empty());
        }
        // Preserve normal and curved rails, plus the valid power flag on
        // powered/detector/activator rails.
        for (id, meta) in [
            (66u16, 0u16),
            (66, 9),
            (27, 8),
            (27, 13),
            (28, 9),
            (157, 13),
        ] {
            world.import_block(0, 65, 0, (id << 4) | meta).unwrap();
            let mut carts = Minecarts::default();
            assert!(carts.can_place(&world, at), "valid rail {id}:{meta}");
            assert!(
                carts.place(&world, at, 0).is_some(),
                "valid rail {id}:{meta}"
            );
        }
    }

    #[test]
    fn only_ordinary_rails_support_curves() {
        assert!(rail_exits(66, 6).is_some());
        assert!(rail_exits(66, 9).is_some());
        assert!(rail_exits(27, 6).is_none());
        assert!(rail_exits(28, 7).is_none());
        assert!(rail_exits(157, 6).is_none());
    }

    #[test]
    fn minecart_mount_rejects_invalid_player_positions() {
        let mut world = World::new();
        world.import_block(0, 65, 0, (66 << 4) | 1).unwrap();
        let at = PlayerPosition {
            x: 0.5,
            y: 65.5,
            z: 0.5,
        };
        let mut carts = Minecarts::default();
        assert!(carts.place(&world, at, 0).is_some());
        for invalid in [
            PlayerPosition { x: f64::NAN, ..at },
            PlayerPosition {
                y: f64::INFINITY,
                ..at
            },
            PlayerPosition {
                z: f64::NEG_INFINITY,
                ..at
            },
            PlayerPosition {
                x: f64::from(crate::world::WORLD_LIMIT),
                ..at
            },
            PlayerPosition { y: 256.0, ..at },
        ] {
            assert!(carts.mount(FIRST_ID, 42, invalid).is_none());
            assert_eq!(carts.cart_for(42), None);
        }
        assert!(carts.mount(FIRST_ID, 42, at).is_some());
    }

    #[test]
    fn minecart_control_rejects_invalid_impulses_without_mutation() {
        let mut world = World::new();
        world.import_block(0, 65, 0, (66 << 4) | 1).unwrap();
        let at = PlayerPosition {
            x: 0.5,
            y: 65.5,
            z: 0.5,
        };
        let mut carts = Minecarts::default();
        assert!(carts.place(&world, at, 0).is_some());
        assert!(carts.mount(FIRST_ID, 42, at).is_some());
        let before = carts.carts[&FIRST_ID].clone();
        for (forward, heading) in [
            (f32::NAN, 90.0),
            (f32::INFINITY, 90.0),
            (f32::NEG_INFINITY, 90.0),
            (2.0, 90.0),
            (-2.0, 90.0),
            (f32::MAX, 90.0),
            (1.0, f32::NAN),
            (1.0, f32::INFINITY),
        ] {
            assert!(!carts.control(42, forward, heading));
            let after = &carts.carts[&FIRST_ID];
            assert_eq!(after.speed, before.speed);
            assert_eq!(after.direction, before.direction);
        }
        assert!(carts.control(42, 1.0, 270.0));
        assert!(carts.carts[&FIRST_ID].speed > before.speed);
    }

    #[test]
    fn minecart_mount_and_rail_bounds() {
        let mut world = World::new();
        world.import_block(0, 65, 0, 27 << 4 | 9).unwrap();
        world.import_block(1, 65, 0, 27 << 4 | 9).unwrap();
        let at = PlayerPosition {
            x: 0.5,
            y: 65.5,
            z: 0.5,
        };
        let mut carts = Minecarts::default();
        let packet = carts.place(&world, at, 0).unwrap();
        assert_eq!(packet.id, 0x0e);
        let mut data = packet.body.as_slice();
        assert_eq!(
            legacy_protocol::read_varint(&mut data).unwrap(),
            FIRST_ID as u32
        );
        assert_eq!(data[0], 10);
        let id = FIRST_ID;
        assert_eq!(carts.mount(id, 11, at).unwrap().id, 0x1b);
        assert!(carts.mount(id, 12, at).is_none());
        assert!(carts.control(11, 1.0, 90.0));
        for tick in 1..10 {
            carts.tick(&world, tick);
        }
        assert!(carts.rider_position(11).is_some());
        assert_eq!(carts.dismount(11).unwrap().id, 0x1b);
        assert!(carts.cart_for(11).is_none());
    }
}
