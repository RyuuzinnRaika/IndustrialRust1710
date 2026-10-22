//! Small bounded multiplayer state for Minecraft Java 1.7.10 / Protocol 5.
//! Wire packet structure is independently encoded from the public 1.7 protocol.
//! No Pumpkin code is copied or linked.
use crate::ground_items::GroundItem;
use crate::player::PlayerPosition;
use crate::world::VIEW_DISTANCE;
use legacy_protocol::{write_string, write_varint};
use std::collections::BTreeMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

pub const MAX_PLAYERS: usize = 8;
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
    tx: SyncSender<Event>,
    overloaded: Arc<AtomicBool>,
}
#[derive(Default)]
struct Inner {
    next_id: i32,
    players: BTreeMap<i32, Peer>,
    ground_items: BTreeMap<i32, GroundItem>,
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
    // Untrusted Play position coordinates must not overflow signed 32-bit
    // subtraction/abs in a debug server or create phantom entities at NaN.
    let chunk = |n: f64| -> Option<i64> {
        if !n.is_finite() || n < f64::from(i32::MIN) || n > f64::from(i32::MAX) {
            return None;
        }
        Some((n.floor() as i64).div_euclid(16))
    };
    let (Some(ax), Some(az), Some(bx), Some(bz)) = (
        chunk(observer.x),
        chunk(observer.z),
        chunk(target.x),
        chunk(target.z),
    ) else {
        return false;
    };
    (ax - bx).abs() <= i64::from(VIEW_DISTANCE) && (az - bz).abs() <= i64::from(VIEW_DISTANCE)
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
    pub fn add(
        &self,
        name: String,
        uuid: String,
        position: PlayerPosition,
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
            tx,
            overloaded: Arc::new(AtomicBool::new(false)),
        };
        // Existing players learn of the new user in queue order.
        let presence = list_entry(&peer.name, true).map_err(|_| JoinError::Full)?;
        let entity = spawn(&peer).map_err(|_| JoinError::Full)?;
        for other in &existing {
            enqueue(other, &presence);
            if visible(&other.position, &peer.position) {
                enqueue(other, &entity);
            }
        }
        // Queue initial dropped-item spawns after the protocol login has been
        // admitted; only chunks visible to the joining player are included.
        for item in inner.ground_items.values() {
            if visible(&peer.position, &item.world_position()) {
                for event in item.spawn() {
                    enqueue(&peer, &event);
                }
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
                    if visible(&peer.position, &gone.position) {
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
                    if other.id == id {
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
                // Server-owned items are spawned/destroyed on view transitions.
                for item in inner.ground_items.values() {
                    match (
                        visible(&old_position, &item.world_position()),
                        visible(&position, &item.world_position()),
                    ) {
                        (false, true) => {
                            for event in item.spawn() {
                                enqueue(&moved, &event);
                            }
                        }
                        (true, false) => enqueue(&moved, &destroy(item.id)),
                        _ => {}
                    }
                }
            }
        }
    }
    /// Called after a dropped item is durably stored on disk.
    pub fn register_ground_item(&self, item: &GroundItem) {
        if let Ok(mut inner) = self.0.lock() {
            if inner.ground_items.contains_key(&item.id) {
                return;
            }
            inner.ground_items.insert(item.id, item.clone());
            for peer in inner.players.values() {
                if visible(&peer.position, &item.world_position()) {
                    for event in item.spawn() {
                        enqueue(peer, &event);
                    }
                }
            }
        }
    }
    /// Remove an item only after durable pickup; never send distant clients
    /// packets for an entity they were not supposed to have seen.
    pub fn forget_ground_item(&self, item_id: i32, collector: Option<i32>) {
        if let Ok(mut inner) = self.0.lock() {
            if let Some(item) = inner.ground_items.remove(&item_id) {
                for peer in inner.players.values() {
                    if visible(&peer.position, &item.world_position()) {
                        if let Some(player_id) = collector {
                            enqueue(peer, &item.collected(player_id)[0]);
                        }
                        enqueue(peer, &destroy(item_id));
                    }
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
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn visibility_rejects_untrusted_and_overflow_coordinates() {
        let center = PlayerPosition::default();
        let mut remote = center;
        remote.x = f64::NAN;
        assert!(!visible(&center, &remote));
        remote.x = f64::INFINITY;
        assert!(!visible(&remote, &center));
        remote.x = f64::from(i32::MAX);
        assert!(!visible(&center, &remote));
        let left = PlayerPosition {
            x: f64::from(i32::MIN),
            y: 65.0,
            z: 0.0,
        };
        let right = PlayerPosition {
            x: f64::from(i32::MAX),
            y: 65.0,
            z: 0.0,
        };
        assert!(!visible(&left, &right));
        let adjacent = PlayerPosition {
            x: -0.01,
            y: center.y,
            z: center.z,
        };
        assert!(visible(&center, &adjacent));
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
    fn ground_items_are_sent_only_in_chunk_view_and_replayed_on_entry() {
        use crate::inventory::Stack;
        let hub = Hub::default();
        let near = PlayerPosition::default();
        let far = PlayerPosition {
            x: 160.5,
            y: 65.0,
            z: 0.5,
        };
        let (a, a_rx, _) = hub.add("Near".into(), "near".into(), near).unwrap();
        let (b, b_rx, _) = hub.add("Far".into(), "far".into(), far).unwrap();
        assert_eq!(a_rx.try_recv().unwrap().id, 0x38); // global tab-list
        let item = GroundItem {
            id: 1_000_000_001,
            position: [16, 65 * 32, 16],
            stack: Stack {
                id: 1,
                count: 1,
                damage: 0,
            },
        };
        hub.register_ground_item(&item);
        assert_eq!(a_rx.try_recv().unwrap().id, 0x0e);
        assert_eq!(a_rx.try_recv().unwrap().id, 0x1c);
        assert!(b_rx.try_recv().is_err());
        hub.move_player(b, near, 0.0, 0.0);
        assert_eq!(b_rx.try_recv().unwrap().id, 0x0c); // sees A
        assert_eq!(b_rx.try_recv().unwrap().id, 0x0e); // sees item
        assert_eq!(b_rx.try_recv().unwrap().id, 0x1c);
        hub.forget_ground_item(item.id, Some(a));
        assert_eq!(a_rx.try_recv().unwrap().id, 0x0c); // B appears
        assert_eq!(a_rx.try_recv().unwrap().id, 0x0d); // collect
        assert_eq!(a_rx.try_recv().unwrap().id, 0x13); // destroy
        assert_eq!(b_rx.try_recv().unwrap().id, 0x0d);
        assert_eq!(b_rx.try_recv().unwrap().id, 0x13);
        let (_c, c_rx, _) = hub.add("Late".into(), "late".into(), near).unwrap();
        assert!(c_rx.try_recv().is_err()); // picked-up item never reappears
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
}
