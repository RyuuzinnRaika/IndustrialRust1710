//! Small bounded multiplayer state for Minecraft Java 1.7.10 / Protocol 5.
//! Wire packet structure is independently encoded from the public 1.7 protocol.
//! No Pumpkin code is copied or linked.
use crate::player::PlayerPosition;
use legacy_protocol::{write_string, write_varint};
use std::collections::BTreeMap;
use std::io;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Mutex;

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
            // A misbehaving or disconnected client cannot block the whole server.
            // TODO: proactively disconnect clients whose queues repeatedly overflow.
            eprintln!("outgoing packet queue full for {}", peer.name);
        }
        Err(TrySendError::Disconnected(_)) => {}
    }
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
        };
        // Existing players learn of the new user in queue order.
        let presence = list_entry(&peer.name, true).map_err(|_| JoinError::Full)?;
        let entity = spawn(&peer).map_err(|_| JoinError::Full)?;
        for other in &existing {
            enqueue(other, &presence);
            enqueue(other, &entity);
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
                    enqueue(peer, &entity);
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
                peer.position = position;
                let packet = teleport(id, &position, yaw, pitch);
                for other in inner.players.values() {
                    if other.id != id {
                        enqueue(other, &packet);
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
