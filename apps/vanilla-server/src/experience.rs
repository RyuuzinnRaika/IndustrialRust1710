//! XP bars, level thresholds and collectible experience orbs for Protocol 5.
use crate::{
    multiplayer::{self, Event},
    player::PlayerPosition,
};
use legacy_protocol::write_varint;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, ErrorKind, Write},
    path::{Path, PathBuf},
};

const MAX_TOTAL: u32 = 2_000_000;
const MAX_ORBS: usize = 256;
pub fn to_next(level: u16) -> u32 {
    let n = u32::from(level);
    // Java 1.7.10, not the post-1.8 experience curve.
    if n < 15 {
        17
    } else if n < 30 {
        17 + (n - 15) * 3
    } else {
        62 + (n - 30) * 7
    }
}
fn level_of(total: u32) -> (u16, u32) {
    let mut remain = total;
    let mut level = 0u16;
    while level < 1000 {
        let step = to_next(level);
        if remain < step {
            break;
        }
        remain -= step;
        level += 1;
    }
    (level, remain)
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Experience {
    #[serde(default = "save_version")]
    version: u8,
    total: u32,
}
fn save_version() -> u8 {
    1
}
impl Experience {
    pub fn new() -> Self {
        Self {
            version: 1,
            total: 0,
        }
    }
    pub fn location(player_path: &Path) -> PathBuf {
        player_path.with_extension("xp.json")
    }
    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::metadata(path) {
            Ok(meta) if !meta.is_file() || meta.len() > 1024 => {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    "invalid experience save",
                ))
            }
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Self::new()),
            Err(e) => return Err(e),
        }
        let value: Self = serde_json::from_slice(&fs::read(path)?)
            .map_err(|_| io::Error::new(ErrorKind::InvalidData, "invalid experience JSON"))?;
        if value.version != 1 || value.total > MAX_TOTAL {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "unsupported experience save",
            ));
        }
        Ok(value)
    }
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension("xp.tmp");
        let mut f = File::create(&temp)?;
        f.write_all(&serde_json::to_vec(self).map_err(io::Error::other)?)?;
        f.sync_all()?;
        drop(f);
        fs::rename(temp, path)
    }
    pub fn add(&mut self, value: u32) {
        self.total = self.total.saturating_add(value).min(MAX_TOTAL);
    }
    #[cfg(test)]
    pub fn level(&self) -> u16 {
        level_of(self.total).0
    }
    pub fn reset(&mut self) {
        *self = Self::new();
    }
    pub fn packet(&self) -> Event {
        let (level, progress) = level_of(self.total);
        let mut body = Vec::new();
        let fraction = progress as f32 / to_next(level) as f32;
        body.extend_from_slice(&fraction.to_be_bytes());
        body.extend_from_slice(&(level.min(i16::MAX as u16) as i16).to_be_bytes());
        body.extend_from_slice(&(self.total.min(i16::MAX as u32) as i16).to_be_bytes());
        Event { id: 0x1f, body }
    }
}
#[derive(Clone, Copy, Debug)]
struct Orb {
    id: i32,
    position: PlayerPosition,
    value: u16,
    created: u64,
}
impl Orb {
    fn spawn(self) -> Event {
        let mut body = Vec::with_capacity(20);
        write_varint(self.id as u32, &mut body);
        for v in [self.position.x, self.position.y, self.position.z] {
            body.extend_from_slice(&((v * 32.0).floor() as i32).to_be_bytes());
        }
        body.extend_from_slice(&(self.value.min(i16::MAX as u16) as i16).to_be_bytes());
        Event { id: 0x11, body }
    }
}
#[derive(Debug)]
pub struct Orbs {
    items: BTreeMap<i32, Orb>,
    next_id: i32,
}
impl Default for Orbs {
    fn default() -> Self {
        Self {
            items: BTreeMap::new(),
            next_id: 900_000,
        }
    }
}
impl Orbs {
    pub fn spawn(&mut self, position: PlayerPosition, amount: u16, tick: u64) -> Option<Event> {
        if self.items.len() >= MAX_ORBS || amount == 0 {
            return None;
        }
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let orb = Orb {
            id,
            position,
            value: amount,
            created: tick,
        };
        self.items.insert(id, orb);
        Some(orb.spawn())
    }
    /// Re-send existing orbs to clients joining or respawning mid-session.
    pub fn spawn_events(&self) -> impl Iterator<Item = Event> + '_ {
        self.items.values().copied().map(Orb::spawn)
    }
    /// Despawn expired orbs for every client, not only the player polling XP.
    pub fn expire(&mut self, now: u64) -> Vec<Event> {
        let ids: Vec<i32> = self
            .items
            .values()
            .filter(|orb| now.saturating_sub(orb.created) >= 6000)
            .map(|orb| orb.id)
            .collect();
        for id in &ids {
            self.items.remove(id);
        }
        ids.into_iter().map(multiplayer::destroy).collect()
    }
    /// Candidate remains present until XP has been durably credited.
    pub fn nearby(&mut self, position: PlayerPosition, now: u64) -> Option<(i32, u16)> {
        // Expiration is handled by expire(), which broadcasts destroy events.
        // Do not remove silently or remote clients will retain ghost orbs.
        self.items
            .values()
            .find(|orb| {
                if now.saturating_sub(orb.created) >= 6000 {
                    return false;
                }
                let d = (position.x - orb.position.x).powi(2)
                    + (position.y - orb.position.y).powi(2)
                    + (position.z - orb.position.z).powi(2);
                d < 2.25
            })
            .map(|orb| (orb.id, orb.value))
    }
    pub fn collect(&mut self, id: i32, collector: i32) -> Option<Vec<Event>> {
        self.items.remove(&id)?;
        let mut body = Vec::with_capacity(10);
        write_varint(id as u32, &mut body);
        write_varint(collector as u32, &mut body);
        Some(vec![Event { id: 0x0d, body }, multiplayer::destroy(id)])
    }
}
/// Approximate unenchanted 1.7.10 furnace XP by output item count.
/// Integer awards are used until fractional batch accounting is persisted.
pub fn smelting_reward(output_id: i16, quantity: u8) -> u16 {
    let per_item: f32 = match output_id {
        265 => 0.7,
        266 => 1.0,
        20 | 1 | 263 => 0.1,
        320 | 364 | 366 | 350 | 393 => 0.35,
        336 => 0.3,
        348 => 0.2,
        264 => 1.0,
        _ => 0.0,
    };
    (per_item * f32::from(quantity)).round() as u16
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn level_boundaries_and_packet() {
        let mut state = Experience::new();
        assert_eq!(state.level(), 0);
        assert_eq!(to_next(14), 17);
        assert_eq!(to_next(15), 17);
        assert_eq!(to_next(16), 20);
        assert_eq!(to_next(29), 59);
        assert_eq!(to_next(30), 62);
        assert_eq!(to_next(31), 69);
        let mut thresholds = Experience::new();
        thresholds.add(272);
        assert_eq!(thresholds.level(), 16);
        thresholds.add(553);
        assert_eq!(thresholds.level(), 30);
        thresholds.add(62);
        assert_eq!(thresholds.level(), 31);
        state.add(17);
        assert_eq!(state.level(), 1);
        assert_eq!(state.packet().id, 0x1f);
        assert_eq!(state.packet().body.len(), 8);
        state.add(1000);
        assert!(state.level() > 16);
    }
    #[test]
    fn orb_cannot_be_collected_twice() {
        let mut orbs = Orbs::default();
        let pos = PlayerPosition::default();
        assert_eq!(orbs.spawn(pos, 5, 0).unwrap().id, 0x11);
        assert_eq!(orbs.spawn_events().count(), 1);
        let (id, amount) = orbs.nearby(pos, 5).unwrap();
        assert_eq!(amount, 5);
        let packets = orbs.collect(id, 10).unwrap();
        use legacy_protocol::read_varint;
        let mut body = packets[0].body.as_slice();
        assert_eq!(read_varint(&mut body).unwrap(), id as u32);
        assert_eq!(read_varint(&mut body).unwrap(), 10);
        assert!(body.is_empty());
        assert_eq!(packets[1].id, 0x13);
        assert!(orbs.collect(id, 10).is_none());
        assert!(orbs.spawn_events().next().is_none());
        orbs.spawn(pos, 3, 0).unwrap();
        assert_eq!(orbs.expire(5999).len(), 0);
        assert_eq!(orbs.expire(6000).len(), 1);
        assert!(orbs.spawn_events().next().is_none());
    }
}
