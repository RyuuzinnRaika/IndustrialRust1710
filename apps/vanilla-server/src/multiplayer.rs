//! Small bounded multiplayer state for Minecraft Java 1.7.10 / Protocol 5.
//! Wire packet structure is independently encoded from the public 1.7 protocol.
//! No Pumpkin code is copied or linked.
use crate::dimensions::Dimension;
use crate::items_1710;
use crate::player::PlayerPosition;
use crate::world::VIEW_DISTANCE;
use legacy_protocol::{write_string, write_varint};
use std::collections::BTreeMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

pub const MAX_PLAYERS: usize = 8;
/// Internal mailbox operation, never serialized as a network packet.
pub const ADMIN_TELEPORT_EVENT: u32 = u32::MAX - 1;
/// Internal mailbox event that applies the 1.7.10 ender-pearl health penalty.
pub const PEARL_TELEPORT_EVENT: u32 = u32::MAX - 2;
/// A session-owned safe Kick: the session sends Disconnect, then persists state.
pub const ADMIN_KICK_EVENT: u32 = u32::MAX - 3;
/// Console-only mode change, distinct from the internal Kick mailbox event.
pub const ADMIN_GAMEMODE_EVENT: u32 = u32::MAX - 4;
/// Internal worker-to-player correction after a server-owned piston move.
/// Session thread rechecks the live collision and persists the new position.
pub const PISTON_PUSH_EVENT: u32 = u32::MAX - 5;
/// Server-owned dispenser arrow hit; the session persists HP before notifying.
pub const DISPENSER_ARROW_DAMAGE_EVENT: u32 = u32::MAX - 6;
/// Validated console/verified-OP item grant; applied by the inventory owner.
pub const ADMIN_GIVE_EVENT: u32 = u32::MAX - 7;
pub const ADMIN_CLEAR_EVENT: u32 = u32::MAX - 8;
const QUEUE_CAPACITY: usize = 512;
#[derive(Clone, Debug)]
pub struct Event {
    pub id: u32,
    pub body: Vec<u8>,
}
#[derive(Clone)]
pub struct Peer {
    pub id: i32,
    pub name: String,
    pub uuid: String,
    pub position: PlayerPosition,
    /// Entity visibility is isolated by dimension, not by coordinates alone.
    pub dimension: Dimension,
    tx: SyncSender<Event>,
    overloaded: Arc<AtomicBool>,
}
#[derive(Default)]
struct Inner {
    next_id: i32,
    players: BTreeMap<i32, Peer>,
}
#[derive(Default)]
pub struct Hub(Mutex<Inner>);

fn poisoned() -> io::Error {
    io::Error::other("multiplayer lock poisoned")
}

fn enqueue(peer: &Peer, packet: &Event) {
    match peer.tx.try_send(packet.clone()) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            // Drop the whole session rather than silently losing a state update.
            // The session thread checks this flag every poll and disconnects.
            if !peer.overloaded.swap(true, Ordering::Release) {
                eprintln!("outgoing packet queue full; disconnecting {}", peer.name);
            }
        }
        Err(TrySendError::Disconnected(_)) => {}
    }
}

/// The server sends player entities only inside the client's loaded view.
pub fn visible(observer: &PlayerPosition, target: &PlayerPosition) -> bool {
    // A non-finite coordinate must never resolve to the spawn chunk. Float
    // casts otherwise silently map NaN to zero, leaking remote entity events.
    let coords = [observer.x, observer.z, target.x, target.z];
    if coords.iter().any(|v| !v.is_finite()) {
        return false;
    }
    // Use 64-bit chunks and an unsigned difference: extreme, untrusted
    // positions cannot overflow a signed subtraction or its absolute value.
    let chunk = |n: f64| (n / 16.0).floor() as i64;
    chunk(observer.x).abs_diff(chunk(target.x)) <= VIEW_DISTANCE as u64
        && chunk(observer.z).abs_diff(chunk(target.z)) <= VIEW_DISTANCE as u64
}

pub fn list_entry(name: &str, online: bool) -> io::Result<Event> {
    // 1.7.10 player info (0x38) is name + bool + signed short latency.
    // Do not use the 1.8+ UUID/action-based packet format.
    let mut body = Vec::new();
    write_string(name, &mut body)?;
    body.push(u8::from(online));
    body.extend_from_slice(&0i16.to_be_bytes());
    Ok(Event { id: 0x38, body })
}

fn angle(a: f32) -> u8 {
    (a.rem_euclid(360.0) * 256.0 / 360.0) as i32 as u8
}
fn fixed(a: f64) -> i32 {
    (a * 32.0).floor() as i32
}
pub fn spawn(peer: &Peer) -> io::Result<Event> {
    // Protocol 5 named entity spawn (0x0C).
    let mut body = Vec::with_capacity(90);
    write_varint(peer.id as u32, &mut body);
    write_string(&peer.uuid, &mut body)?;
    write_string(&peer.name, &mut body)?;
    write_varint(0, &mut body); // Empty skin/profile properties.
    for coord in [peer.position.x, peer.position.y, peer.position.z] {
        body.extend_from_slice(&fixed(coord).to_be_bytes());
    }
    body.extend_from_slice(&[0, 0]); // yaw, pitch
    body.extend_from_slice(&0i16.to_be_bytes()); // no held item
    body.push(0x7f); // end of entity metadata
    Ok(Event { id: 0x0c, body })
}
pub fn teleport(id: i32, pos: &PlayerPosition, yaw: f32, pitch: f32) -> Event {
    let mut body = Vec::with_capacity(18);
    body.extend_from_slice(&id.to_be_bytes());
    for coord in [pos.x, pos.y, pos.z] {
        body.extend_from_slice(&fixed(coord).to_be_bytes());
    }
    body.push(angle(yaw));
    body.push(angle(pitch));
    Event { id: 0x18, body }
}
pub fn destroy(id: i32) -> Event {
    let mut body = Vec::with_capacity(5);
    body.push(1); // Signed byte count, always 1.
    body.extend_from_slice(&id.to_be_bytes());
    Event { id: 0x13, body }
}

#[derive(Debug)]
pub enum JoinError {
    Full,
    Duplicate,
}
impl Hub {
    /// Healthy sessions have not lost any authoritative outbound packets.
    pub fn is_healthy(&self, id: i32) -> bool {
        self.0
            .lock()
            .ok()
            .and_then(|inner| {
                inner
                    .players
                    .get(&id)
                    .map(|peer| !peer.overloaded.load(Ordering::Acquire))
            })
            .unwrap_or(false)
    }
    pub fn count(&self) -> io::Result<usize> {
        Ok(self.0.lock().map_err(|_| poisoned())?.players.len())
    }
    pub fn names(&self) -> io::Result<Vec<String>> {
        let inner = self.0.lock().map_err(|_| poisoned())?;
        Ok(inner.players.values().map(|p| p.name.clone()).collect())
    }
    /// Resolve a live player's authoritative last-reported position.
    pub fn player_position(&self, name: &str) -> io::Result<Option<PlayerPosition>> {
        let inner = self.0.lock().map_err(|_| poisoned())?;
        Ok(inner
            .players
            .values()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .map(|p| p.position))
    }

    /// Deliver one packet to a connected player without broadcasting a whisper.
    pub fn send_to_name(&self, name: &str, packet: Event) -> io::Result<bool> {
        let inner = self.0.lock().map_err(|_| poisoned())?;
        if let Some(peer) = inner
            .players
            .values()
            .find(|p| p.name.eq_ignore_ascii_case(name))
        {
            enqueue(peer, &packet);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Command grants must not mutate another thread's inventory directly.
    pub fn request_admin_give(
        &self,
        name: &str,
        id: i16,
        count: u8,
        damage: i16,
    ) -> io::Result<bool> {
        if !(1..=64).contains(&count) || !items_1710::valid_damage(id, damage) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid give item",
            ));
        }
        let mut body = Vec::with_capacity(5);
        body.extend_from_slice(&id.to_be_bytes());
        body.push(count);
        body.extend_from_slice(&damage.to_be_bytes());
        let inner = self.0.lock().map_err(|_| poisoned())?;
        if let Some(peer) = inner
            .players
            .values()
            .find(|p| p.name.eq_ignore_ascii_case(name))
        {
            enqueue(
                peer,
                &Event {
                    id: ADMIN_GIVE_EVENT,
                    body,
                },
            );
            Ok(true)
        } else {
            Ok(false)
        }
    }
    /// Bounded inventory mutation request. Player session alone owns the save.
    pub fn request_admin_clear(
        &self,
        name: &str,
        item: i16,
        data: i16,
        max: i32,
    ) -> io::Result<bool> {
        if (item != -1 && !items_1710::exists(item)) || data < -1 || !(-1..=4096).contains(&max) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid clear request",
            ));
        }
        let mut body = Vec::with_capacity(8);
        body.extend_from_slice(&item.to_be_bytes());
        body.extend_from_slice(&data.to_be_bytes());
        body.extend_from_slice(&max.to_be_bytes());
        let inner = self.0.lock().map_err(|_| poisoned())?;
        if let Some(peer) = inner
            .players
            .values()
            .find(|p| p.name.eq_ignore_ascii_case(name))
        {
            enqueue(
                peer,
                &Event {
                    id: ADMIN_CLEAR_EVENT,
                    body,
                },
            );
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// UUID attached to the currently admitted peer. Caller must ensure the
    /// server is configured online-mode before treating it as authenticated.
    pub fn uuid_for_online_name(&self, name: &str) -> io::Result<Option<String>> {
        let inner = self.0.lock().map_err(|_| poisoned())?;
        Ok(inner
            .players
            .values()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .map(|p| p.uuid.clone()))
    }
    /// Queue a privileged teleport to be applied by the owning session thread.
    /// Updating Hub coordinates alone would not update the player's saved state.
    pub fn request_admin_teleport(&self, name: &str, position: PlayerPosition) -> io::Result<bool> {
        self.queue_teleport(name, position, ADMIN_TELEPORT_EVENT)
    }
    /// Queue a gamemode change. The player thread owns its inventory, and is
    /// responsible for writing its override before notifying the client.
    pub fn request_admin_gamemode(&self, name: &str, mode: u8) -> io::Result<bool> {
        if mode > 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "only survival and creative are implemented",
            ));
        }
        let inner = self.0.lock().map_err(|_| poisoned())?;
        let Some(peer) = inner
            .players
            .values()
            .find(|p| p.name.eq_ignore_ascii_case(name))
        else {
            return Ok(false);
        };
        peer.tx
            .try_send(Event {
                id: ADMIN_GAMEMODE_EVENT,
                body: vec![mode],
            })
            .map_err(|_| {
                peer.overloaded.store(true, Ordering::Release);
                io::Error::other("target session mailbox unavailable")
            })?;
        Ok(true)
    }
    pub fn request_pearl_teleport(&self, name: &str, position: PlayerPosition) -> io::Result<bool> {
        self.queue_teleport(name, position, PEARL_TELEPORT_EVENT)
    }
    /// Only the target's owning session may mutate its persisted health.
    /// Never send a network packet with this private event ID.
    pub fn request_dispenser_arrow_damage(&self, entity_id: i32) -> io::Result<bool> {
        let inner = self.0.lock().map_err(|_| poisoned())?;
        let Some(peer) = inner.players.get(&entity_id) else {
            return Ok(false);
        };
        if peer.dimension != Dimension::Overworld {
            return Ok(false);
        }
        peer.tx
            .try_send(Event {
                id: DISPENSER_ARROW_DAMAGE_EVENT,
                body: Vec::new(),
            })
            .map_err(|_| {
                peer.overloaded.store(true, Ordering::Release);
                io::Error::other("target session damage mailbox unavailable")
            })?;
        Ok(true)
    }
    fn queue_teleport(
        &self,
        name: &str,
        position: PlayerPosition,
        event_id: u32,
    ) -> io::Result<bool> {
        if !position.is_valid() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid teleport coordinates",
            ));
        }
        let inner = self.0.lock().map_err(|_| poisoned())?;
        let Some(peer) = inner
            .players
            .values()
            .find(|p| p.name.eq_ignore_ascii_case(name))
        else {
            return Ok(false);
        };
        let mut body = Vec::with_capacity(24);
        for coord in [position.x, position.y, position.z] {
            body.extend_from_slice(&coord.to_be_bytes());
        }
        peer.tx
            .try_send(Event { id: event_id, body })
            .map_err(|_| {
                peer.overloaded.store(true, Ordering::Release);
                io::Error::other("target session mailbox unavailable")
            })?;
        Ok(true)
    }
    /// Queue an administrative Kick without removing the player from Hub early.
    pub fn request_admin_kick(&self, name: &str, reason: &str) -> io::Result<bool> {
        if reason.is_empty() || reason.len() > 256 || reason.chars().any(char::is_control) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid kick reason",
            ));
        }
        let inner = self.0.lock().map_err(|_| poisoned())?;
        let Some(peer) = inner
            .players
            .values()
            .find(|p| p.name.eq_ignore_ascii_case(name))
        else {
            return Ok(false);
        };
        peer.tx
            .try_send(Event {
                id: ADMIN_KICK_EVENT,
                body: reason.as_bytes().to_vec(),
            })
            .map_err(|_| {
                peer.overloaded.store(true, Ordering::Release);
                io::Error::other("target session mailbox unavailable")
            })?;
        Ok(true)
    }

    pub fn positions(&self) -> io::Result<Vec<PlayerPosition>> {
        let inner = self.0.lock().map_err(|_| poisoned())?;
        Ok(inner.players.values().map(|peer| peer.position).collect())
    }
    /// Stable entity IDs are required for one-shot area damage distribution.
    pub fn participants(&self) -> io::Result<Vec<(i32, PlayerPosition)>> {
        let inner = self.0.lock().map_err(|_| poisoned())?;
        Ok(inner
            .players
            .values()
            .map(|peer| (peer.id, peer.position))
            .collect())
    }
    /// Legacy single-world admission. Explicit multiworld routing must call
    /// `add_in_dimension` so identical coordinates in other dimensions do
    /// not create ghost player entities.
    #[allow(dead_code)] // Staged A10 multi-dimension API; enabled when session routing is integrated.
    #[allow(dead_code)] // Staged API until live multiworld session routing is connected.
    pub fn add(
        &self,
        name: String,
        uuid: String,
        position: PlayerPosition,
    ) -> Result<(i32, Receiver<Event>, Vec<Peer>), JoinError> {
        self.add_in_dimension(name, uuid, position, Dimension::Overworld)
    }

    pub fn add_in_dimension(
        &self,
        name: String,
        uuid: String,
        position: PlayerPosition,
        dimension: Dimension,
    ) -> Result<(i32, Receiver<Event>, Vec<Peer>), JoinError> {
        let mut inner = self.0.lock().map_err(|_| JoinError::Full)?;
        if inner.players.len() >= MAX_PLAYERS {
            return Err(JoinError::Full);
        }
        if inner
            .players
            .values()
            .any(|p| p.name.eq_ignore_ascii_case(&name))
        {
            return Err(JoinError::Duplicate);
        }
        let (tx, rx) = sync_channel(QUEUE_CAPACITY);
        let existing = inner.players.values().cloned().collect::<Vec<_>>();
        // A monotonically increasing entity ID prevents reuse on reconnect.
        if inner.next_id == i32::MAX {
            return Err(JoinError::Full);
        }
        inner.next_id += 1;
        let id = inner.next_id;
        let peer = Peer {
            id,
            name,
            uuid,
            position,
            dimension,
            tx,
            overloaded: Arc::new(AtomicBool::new(false)),
        };
        // Existing players learn of the new user in queue order.
        let presence = list_entry(&peer.name, true).map_err(|_| JoinError::Full)?;
        let entity = spawn(&peer).map_err(|_| JoinError::Full)?;
        for other in &existing {
            enqueue(other, &presence);
            if other.dimension == peer.dimension && visible(&other.position, &peer.position) {
                enqueue(other, &entity);
            }
        }
        inner.players.insert(id, peer);
        Ok((id, rx, existing))
    }
    pub fn remove(&self, id: i32) {
        if let Ok(mut inner) = self.0.lock() {
            if let Some(gone) = inner.players.remove(&id) {
                let removed = list_entry(&gone.name, false).ok();
                let entity = destroy(id);
                for peer in inner.players.values() {
                    if peer.dimension == gone.dimension && visible(&peer.position, &gone.position) {
                        enqueue(peer, &entity);
                    }
                    if let Some(item) = &removed {
                        enqueue(peer, item);
                    }
                }
            }
        }
    }
    pub fn move_player(&self, id: i32, position: PlayerPosition, yaw: f32, pitch: f32) {
        if let Ok(mut inner) = self.0.lock() {
            if let Some(peer) = inner.players.get_mut(&id) {
                let old_position = peer.position;
                peer.position = position;
                let moved = peer.clone();
                let packet = teleport(id, &position, yaw, pitch);
                let appeared = spawn(&moved).ok();
                let vanished = destroy(id);
                for other in inner.players.values() {
                    if other.id == id || other.dimension != moved.dimension {
                        continue;
                    }
                    // Notify *both* directions of the visibility transition.
                    match (
                        visible(&other.position, &old_position),
                        visible(&other.position, &position),
                    ) {
                        (true, true) => enqueue(other, &packet),
                        (true, false) => enqueue(other, &vanished),
                        (false, true) => {
                            if let Some(event) = &appeared {
                                enqueue(other, event);
                            }
                        }
                        (false, false) => {}
                    }
                    match (
                        visible(&old_position, &other.position),
                        visible(&position, &other.position),
                    ) {
                        (true, false) => enqueue(&moved, &destroy(other.id)),
                        (false, true) => {
                            if let Ok(event) = spawn(other) {
                                enqueue(&moved, &event);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    /// Atomically move the server's player-interest membership across
    /// dimensions. The session layer MUST prepare the destination world,
    /// persist player state and send Protocol 5 Respawn/chunks independently;
    /// this only manages peer visibility. Global tab-list entries remain.
    /// Invalid coordinates or same-dimension transitions are rejected without
    /// mutating any player state.
    #[allow(dead_code)] // Staged A10 multi-dimension API; enabled when session routing is integrated.
    #[allow(dead_code)] // Staged API until live multiworld session routing is connected.
    pub fn change_dimension(
        &self,
        id: i32,
        dimension: Dimension,
        position: PlayerPosition,
    ) -> io::Result<bool> {
        if !position.is_valid() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid arrival position",
            ));
        }
        let mut inner = self.0.lock().map_err(|_| poisoned())?;
        let Some(current) = inner.players.get_mut(&id) else {
            return Ok(false);
        };
        if current.dimension == dimension {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "source and destination dimensions match",
            ));
        }
        let departing = current.clone();
        current.dimension = dimension;
        current.position = position;
        let arriving = current.clone();
        let arrival_spawn = spawn(&arriving)?;
        let departure_destroy = destroy(id);
        for other in inner.players.values() {
            if other.id == id {
                continue;
            }
            if other.dimension == departing.dimension
                && visible(&other.position, &departing.position)
            {
                enqueue(other, &departure_destroy);
                enqueue(&arriving, &destroy(other.id));
            }
            if other.dimension == dimension && visible(&other.position, &position) {
                enqueue(other, &arrival_spawn);
                enqueue(&arriving, &spawn(other)?);
            }
        }
        Ok(true)
    }

    #[allow(dead_code)] // Staged A10 multi-dimension API; enabled when session routing is integrated.
    #[allow(dead_code)] // Staged API until live multiworld session routing is connected.
    pub fn positions_in_dimension(&self, dimension: Dimension) -> io::Result<Vec<PlayerPosition>> {
        let inner = self.0.lock().map_err(|_| poisoned())?;
        Ok(inner
            .players
            .values()
            .filter(|peer| peer.dimension == dimension)
            .map(|peer| peer.position)
            .collect())
    }

    #[allow(dead_code)] // Staged A10 multi-dimension API; enabled when session routing is integrated.
    #[allow(dead_code)] // Staged API until live multiworld session routing is connected.
    pub fn participants_in_dimension(
        &self,
        dimension: Dimension,
    ) -> io::Result<Vec<(i32, PlayerPosition)>> {
        let inner = self.0.lock().map_err(|_| poisoned())?;
        Ok(inner
            .players
            .values()
            .filter(|peer| peer.dimension == dimension)
            .map(|peer| (peer.id, peer.position))
            .collect())
    }

    /// Dimension-aware packet fanout for world-owned game events.
    #[allow(dead_code)] // Staged A10 multi-dimension API; enabled when session routing is integrated.
    #[allow(dead_code)] // Staged API until live multiworld session routing is connected.
    pub fn broadcast_in_dimension_view(
        &self,
        dimension: Dimension,
        origin: PlayerPosition,
        packet: Event,
    ) {
        if let Ok(inner) = self.0.lock() {
            for peer in inner.players.values() {
                if peer.dimension == dimension && visible(&peer.position, &origin) {
                    enqueue(peer, &packet);
                }
            }
        }
    }

    #[allow(dead_code)] // Staged A10 multi-dimension API; enabled when session routing is integrated.
    #[allow(dead_code)] // Staged API until live multiworld session routing is connected.
    pub fn broadcast_in_dimension(&self, dimension: Dimension, packet: Event) {
        if let Ok(inner) = self.0.lock() {
            for peer in inner.players.values() {
                if peer.dimension == dimension {
                    enqueue(peer, &packet);
                }
            }
        }
    }

    /// Single-world compatibility fanout. A multiworld loop must use the
    /// explicit dimension-aware variant to avoid leaking entity packets.
    /// Persistent entities still require a separate spawn/despawn index.
    pub fn broadcast_in_view(&self, origin: PlayerPosition, packet: Event) {
        if let Ok(inner) = self.0.lock() {
            for peer in inner.players.values() {
                if visible(&peer.position, &origin) {
                    enqueue(peer, &packet);
                }
            }
        }
    }

    pub fn broadcast(&self, packet: Event) {
        if let Ok(inner) = self.0.lock() {
            for peer in inner.players.values() {
                enqueue(peer, &packet);
            }
        }
    }

    /// Fan out a packet to every peer except the session that sent it.
    pub fn broadcast_except(&self, excluded_id: i32, packet: Event) {
        if let Ok(inner) = self.0.lock() {
            for peer in inner.players.values() {
                if peer.id != excluded_id {
                    enqueue(peer, &packet);
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nonfinite_and_extreme_positions_never_enter_interest_range() {
        let origin = PlayerPosition::default();
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let invalid_x = PlayerPosition { x: value, ..origin };
            let invalid_z = PlayerPosition { z: value, ..origin };
            assert!(!visible(&origin, &invalid_x));
            assert!(!visible(&invalid_x, &origin));
            assert!(!visible(&origin, &invalid_z));
            assert!(!visible(&invalid_z, &origin));
        }

        let positive = PlayerPosition {
            x: f64::MAX,
            y: 65.0,
            z: f64::MAX,
        };
        let negative = PlayerPosition {
            x: -f64::MAX,
            y: 65.0,
            z: -f64::MAX,
        };
        assert!(!visible(&positive, &negative));
        assert!(!visible(&negative, &positive));
        assert!(!visible(&origin, &positive));
        assert!(!visible(&origin, &negative));

        // Chunk boundaries are inclusive for a configured view radius.
        let edge = PlayerPosition {
            x: (VIEW_DISTANCE as f64) * 16.0 + 0.5,
            ..origin
        };
        let outside = PlayerPosition {
            x: (VIEW_DISTANCE as f64 + 1.0) * 16.0 + 0.5,
            ..origin
        };
        assert!(visible(&origin, &edge));
        assert!(!visible(&origin, &outside));
    }

    #[test]
    fn out_of_view_players_spawn_on_entry_and_destroy_on_exit() {
        let hub = Hub::default();
        let (a, a_rx, _) = hub
            .add("Alice".into(), "uuid-a".into(), PlayerPosition::default())
            .unwrap();
        let far = PlayerPosition {
            x: 160.5,
            y: 65.0,
            z: 0.5,
        };
        let (_b, b_rx, previous) = hub.add("Bob".into(), "uuid-b".into(), far).unwrap();
        assert_eq!(previous.len(), 1); // globally listed, not spawned
        assert!(b_rx.try_recv().is_err());
        assert_eq!(a_rx.try_recv().unwrap().id, 0x38); // global player list
        assert!(a_rx.try_recv().is_err());
        hub.move_player(
            a,
            PlayerPosition {
                x: 161.5,
                y: 65.0,
                z: 0.5,
            },
            0.0,
            0.0,
        );
        assert_eq!(b_rx.try_recv().unwrap().id, 0x0c);
        assert_eq!(a_rx.try_recv().unwrap().id, 0x0c);
        hub.move_player(
            a,
            PlayerPosition {
                x: 162.5,
                y: 65.0,
                z: 0.5,
            },
            0.0,
            0.0,
        );
        assert_eq!(b_rx.try_recv().unwrap().id, 0x18);
        hub.move_player(a, PlayerPosition::default(), 0.0, 0.0);
        assert_eq!(b_rx.try_recv().unwrap().id, 0x13);
        assert_eq!(a_rx.try_recv().unwrap().id, 0x13);
        assert!(b_rx.try_recv().is_err());
    }

    #[test]
    fn broadcast_except_leaves_sender_stream_for_ordered_direct_packets() {
        let hub = Hub::default();
        let (alice, alice_rx, _) = hub
            .add("Alice".into(), "uuid-a".into(), PlayerPosition::default())
            .unwrap();
        let (_bob, bob_rx, _) = hub
            .add("Bob".into(), "uuid-b".into(), PlayerPosition::default())
            .unwrap();
        while alice_rx.try_recv().is_ok() {}
        while bob_rx.try_recv().is_ok() {}

        hub.broadcast_except(
            alice,
            Event {
                id: 0x1b,
                body: vec![1],
            },
        );

        assert!(alice_rx.try_recv().is_err());
        assert_eq!(bob_rx.try_recv().unwrap().id, 0x1b);
    }

    #[test]
    fn dimension_membership_isolates_entities_but_not_global_tab_list() {
        let hub = Hub::default();
        let pos = PlayerPosition::default();
        let (alice, alice_rx, _) = hub
            .add_in_dimension("Alice".into(), "uuid-a".into(), pos, Dimension::Overworld)
            .unwrap();
        let (bob, bob_rx, seen) = hub
            .add_in_dimension("Bob".into(), "uuid-b".into(), pos, Dimension::Nether)
            .unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].dimension, Dimension::Overworld);
        assert_eq!(alice_rx.try_recv().unwrap().id, 0x38);
        assert!(alice_rx.try_recv().is_err());
        assert!(bob_rx.try_recv().is_err());
        assert_eq!(
            hub.positions_in_dimension(Dimension::Overworld)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            hub.participants_in_dimension(Dimension::Nether).unwrap()[0].0,
            bob
        );

        hub.broadcast_in_dimension_view(
            Dimension::Nether,
            pos,
            Event {
                id: 0x22,
                body: vec![1],
            },
        );
        assert_eq!(bob_rx.try_recv().unwrap().id, 0x22);
        assert!(alice_rx.try_recv().is_err());

        assert!(hub.change_dimension(alice, Dimension::Nether, pos).unwrap());
        assert_eq!(bob_rx.try_recv().unwrap().id, 0x0c);
        assert_eq!(alice_rx.try_recv().unwrap().id, 0x0c);
        assert_eq!(
            hub.positions_in_dimension(Dimension::Overworld)
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            hub.positions_in_dimension(Dimension::Nether).unwrap().len(),
            2
        );
        assert!(hub.change_dimension(alice, Dimension::Nether, pos).is_err());
        assert!(hub
            .change_dimension(bob, Dimension::End, PlayerPosition { x: f64::NAN, ..pos })
            .is_err());
        assert_eq!(
            hub.positions_in_dimension(Dimension::Nether).unwrap().len(),
            2
        );
        hub.remove(alice);
        assert_eq!(bob_rx.try_recv().unwrap().id, 0x13);
    }

    #[test]
    fn dimension_change_destroys_old_visible_peers_and_spawns_new_peers() {
        let hub = Hub::default();
        let pos = PlayerPosition::default();
        let (alice, alice_rx, _) = hub
            .add_in_dimension("Alice".into(), "a".into(), pos, Dimension::Overworld)
            .unwrap();
        let (_cara, cara_rx, _) = hub
            .add_in_dimension("Cara".into(), "c".into(), pos, Dimension::Overworld)
            .unwrap();
        // Clear Cara's initial spawn packet for Alice, and Alice's initial
        // tab-list + Cara entity packets.
        assert_eq!(alice_rx.try_recv().unwrap().id, 0x38);
        assert_eq!(alice_rx.try_recv().unwrap().id, 0x0c);
        let (_bob, bob_rx, _) = hub
            .add_in_dimension("Bob".into(), "b".into(), pos, Dimension::Nether)
            .unwrap();
        assert_eq!(alice_rx.try_recv().unwrap().id, 0x38);
        assert_eq!(cara_rx.try_recv().unwrap().id, 0x38);
        assert!(hub.change_dimension(alice, Dimension::Nether, pos).unwrap());
        assert_eq!(cara_rx.try_recv().unwrap().id, 0x13);
        assert_eq!(alice_rx.try_recv().unwrap().id, 0x13);
        assert_eq!(alice_rx.try_recv().unwrap().id, 0x0c);
        assert_eq!(bob_rx.try_recv().unwrap().id, 0x0c);
        assert!(cara_rx.try_recv().is_err());
        assert!(bob_rx.try_recv().is_err());

        hub.move_player(alice, PlayerPosition { x: 1.5, ..pos }, 0.0, 0.0);
        assert_eq!(bob_rx.try_recv().unwrap().id, 0x18);
        assert!(cara_rx.try_recv().is_err());
        hub.remove(alice);
        assert_eq!(bob_rx.try_recv().unwrap().id, 0x13);
        // The global player tab-list must still notify other dimensions.
        assert_eq!(bob_rx.try_recv().unwrap().id, 0x38);
        assert_eq!(cara_rx.try_recv().unwrap().id, 0x38);
        assert!(cara_rx.try_recv().is_err());
    }
    #[test]
    fn queue_overflow_marks_session_unhealthy_instead_of_silent_desync() {
        let hub = Hub::default();
        let (id, _rx, _) = hub
            .add("FullQueue".into(), "uuid".into(), PlayerPosition::default())
            .unwrap();
        for _ in 0..=QUEUE_CAPACITY {
            hub.broadcast(Event {
                id: 0x03,
                body: vec![],
            });
        }
        assert!(!hub.is_healthy(id));
    }
    #[test]
    fn golden_protocol5_player_entity_bytes() {
        // Independently fixed Protocol 5 S0C, S13, S18 and S38 fixtures.
        let hub = Hub::default();
        let (id, _rx, _) = hub
            .add("A".into(), "u".into(), PlayerPosition::default())
            .unwrap();
        let peer = hub.0.lock().unwrap().players[&id].clone();
        assert_eq!(
            spawn(&peer).unwrap().body,
            [
                1, 1, b'u', 1, b'A', 0, // ID, UUID, name, properties
                0, 0, 0, 16, 0, 0, 8, 32, 0, 0, 0, 16, // fixed-point XYZ
                0, 0, 0, 0, 0x7f, // yaw, pitch, held item, metadata terminator
            ]
        );
        assert_eq!(destroy(id).body, [1, 0, 0, 0, 1]);
        assert_eq!(list_entry("A", true).unwrap().body, [1, b'A', 1, 0, 0]);
        assert_eq!(
            teleport(id, &PlayerPosition::default(), 90.0, 0.0).body,
            [0, 0, 0, 1, 0, 0, 0, 16, 0, 0, 8, 32, 0, 0, 0, 16, 64, 0]
        );
    }

    #[test]
    fn protocol5_player_list_and_spawn() {
        let list = list_entry("RustTester", true).unwrap();
        assert_eq!(list.id, 0x38);
        assert_eq!(list.body[0], 10);
        assert_eq!(&list.body[1..11], b"RustTester");
        assert_eq!(list.body[11..], [1, 0, 0]);
        let hub = Hub::default();
        let (id, _rx, _) = hub
            .add(
                "RustTester".to_string(),
                "00000000-0000-3000-8000-000000000000".into(),
                PlayerPosition::default(),
            )
            .unwrap();
        assert_eq!(id, 1);
        assert_eq!(hub.count().unwrap(), 1);
        assert_eq!(spawn(&hub.0.lock().unwrap().players[&id]).unwrap().id, 0x0c);
        hub.remove(id);
        assert_eq!(hub.count().unwrap(), 0);
    }
    #[test]
    fn privileged_teleport_is_session_owned_and_rejects_invalid_inputs() {
        let hub = Hub::default();
        let (_id, rx, _) = hub
            .add("Alex".into(), "uuid-a".into(), PlayerPosition::default())
            .unwrap();
        let target = PlayerPosition {
            x: -15.5,
            y: 70.0,
            z: 19.25,
        };
        assert!(hub.request_admin_teleport("aLeX", target).unwrap());
        let event = rx.try_recv().unwrap();
        assert_eq!(event.id, ADMIN_TELEPORT_EVENT);
        assert_eq!(event.body.len(), 24);
        assert_eq!(
            f64::from_be_bytes(event.body[0..8].try_into().unwrap()),
            -15.5
        );
        assert!(hub.request_pearl_teleport("Alex", target).unwrap());
        let pearl = rx.try_recv().unwrap();
        assert_eq!(pearl.id, PEARL_TELEPORT_EVENT);
        assert_eq!(pearl.body, event.body);
        assert!(!hub.request_admin_teleport("not-online", target).unwrap());
        assert!(hub
            .request_admin_teleport(
                "Alex",
                PlayerPosition {
                    x: f64::NAN,
                    ..target
                }
            )
            .is_err());
    }
    #[test]
    fn duplicate_denied_case_insensitively() {
        let hub = Hub::default();
        let _ = hub
            .add(
                "RustTester".to_string(),
                "uid".into(),
                PlayerPosition::default(),
            )
            .unwrap();
        assert!(matches!(
            hub.add("rusttester".into(), "uid".into(), PlayerPosition::default()),
            Err(JoinError::Duplicate)
        ));
    }
    #[test]
    fn add011_simultaneous_case_insensitive_logins_have_one_owner_and_one_presence() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        const CONTENDERS: usize = 12;
        let hub = Arc::new(Hub::default());
        let (observer_id, observer_rx, _) = hub
            .add(
                "Observer".into(),
                "observer-uuid".into(),
                PlayerPosition::default(),
            )
            .unwrap();
        let barrier = Arc::new(Barrier::new(CONTENDERS));
        let results = thread::scope(|scope| {
            let mut workers = Vec::new();
            for i in 0..CONTENDERS {
                let hub = Arc::clone(&hub);
                let barrier = Arc::clone(&barrier);
                workers.push(scope.spawn(move || {
                    barrier.wait();
                    let name = if i % 2 == 0 {
                        "RustTester"
                    } else {
                        "rusttester"
                    };
                    hub.add(
                        name.to_string(),
                        format!("session-uuid-{i}"),
                        PlayerPosition::default(),
                    )
                    .map(|(id, _rx, previous)| (id, previous.len()))
                }));
            }
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        });

        let winners: Vec<_> = results
            .iter()
            .filter_map(|result| result.as_ref().ok())
            .collect();
        assert_eq!(winners.len(), 1, "only one session can own the identity");
        assert_eq!(
            winners[0].1, 1,
            "only the observer existed before admission"
        );
        assert_eq!(hub.count().unwrap(), 2);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(JoinError::Duplicate)))
                .count(),
            CONTENDERS - 1
        );
        let owner = winners[0].0;
        assert_ne!(owner, observer_id);

        // The observer sees exactly one player-list addition and one entity
        // spawn, not a phantom admission by any losing connection.
        assert_eq!(observer_rx.try_recv().unwrap().id, 0x38);
        assert_eq!(observer_rx.try_recv().unwrap().id, 0x0c);
        assert!(observer_rx.try_recv().is_err());
        hub.remove(owner);
        assert_eq!(observer_rx.try_recv().unwrap().id, 0x13);
        assert_eq!(observer_rx.try_recv().unwrap().id, 0x38);
        assert!(observer_rx.try_recv().is_err());
        assert_eq!(hub.count().unwrap(), 1);
    }
    #[test]
    fn movement_events_and_departure() {
        let hub = Hub::default();
        let (a, _a_rx, _) = hub
            .add("Alice".into(), "uuid-a".into(), PlayerPosition::default())
            .unwrap();
        let (b, b_rx, previous) = hub
            .add("Bob".into(), "uuid-b".into(), PlayerPosition::default())
            .unwrap();
        assert_eq!(previous.len(), 1);
        hub.move_player(
            a,
            PlayerPosition {
                x: 2.5,
                y: 65.0,
                z: 3.5,
            },
            90.0,
            0.0,
        );
        // A's move delivered to B.
        assert_eq!(b_rx.try_recv().unwrap().id, 0x18);
        hub.remove(a);
        assert_eq!(b_rx.try_recv().unwrap().id, 0x13);
        assert_eq!(b_rx.try_recv().unwrap().id, 0x38);
        hub.remove(b);
    }
    #[test]
    fn spatial_projectile_events_are_not_sent_to_distant_players() {
        let hub = Hub::default();
        let near = PlayerPosition::default();
        let far = PlayerPosition {
            x: 160.5,
            y: 65.0,
            z: 0.5,
        };
        let (_a, a_rx, _) = hub.add("Near".into(), "uuid-a".into(), near).unwrap();
        let (_b, b_rx, _) = hub.add("Far".into(), "uuid-b".into(), far).unwrap();
        // One global player list entry already exists for A; discard it.
        assert_eq!(a_rx.try_recv().unwrap().id, 0x38);
        assert!(a_rx.try_recv().is_err());
        assert!(b_rx.try_recv().is_err());

        hub.broadcast_in_view(
            near,
            Event {
                id: 0x0e,
                body: vec![1, 2, 3],
            },
        );
        assert_eq!(a_rx.try_recv().unwrap().body, [1, 2, 3]);
        assert!(b_rx.try_recv().is_err());

        // A distant explosion is visible only to B.
        hub.broadcast_in_view(
            far,
            Event {
                id: 0x27,
                body: vec![4, 5],
            },
        );
        assert_eq!(b_rx.try_recv().unwrap().body, [4, 5]);
        assert!(a_rx.try_recv().is_err());
        // Broadcast remains global for chat and world state.
        hub.broadcast(Event {
            id: 0x02,
            body: vec![],
        });
        assert_eq!(a_rx.try_recv().unwrap().id, 0x02);
        assert_eq!(b_rx.try_recv().unwrap().id, 0x02);
    }
    #[test]
    fn dispenser_arrow_damage_is_addressed_only_to_overworld_target() {
        let hub = Hub::default();
        let (alice, alice_rx, _) = hub
            .add_in_dimension(
                "Alice".into(),
                "uuid-a".into(),
                PlayerPosition::default(),
                Dimension::Overworld,
            )
            .unwrap();
        let (bob, bob_rx, _) = hub
            .add_in_dimension(
                "Bob".into(),
                "uuid-b".into(),
                PlayerPosition::default(),
                Dimension::Nether,
            )
            .unwrap();
        // Joining Bob generates a player-list entry for Alice.
        assert_eq!(alice_rx.try_recv().unwrap().id, 0x38);
        assert!(hub.request_dispenser_arrow_damage(alice).unwrap());
        assert_eq!(
            alice_rx.try_recv().unwrap().id,
            DISPENSER_ARROW_DAMAGE_EVENT
        );
        assert!(alice_rx.try_recv().is_err());
        assert!(!hub.request_dispenser_arrow_damage(bob).unwrap());
        assert!(!hub.request_dispenser_arrow_damage(999).unwrap());
        assert!(bob_rx.try_recv().is_err());
    }
}
