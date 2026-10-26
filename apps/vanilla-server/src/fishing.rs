//! Protocol 5 fishing float, water contact and server-owned catch rolls.
//! One float per player; no client-submitted loot, hit or entity position.
use crate::inventory::Stack;
use crate::multiplayer::{self, Event};
use crate::player::PlayerPosition;
use crate::world::World;
use legacy_protocol::write_varint;
use std::collections::BTreeMap;

const FIRST_ID: i32 = 1_500_000;
const LIMIT: usize = 32;

#[derive(Clone, Debug)]
struct Bobber {
    id: i32,
    owner: i32,
    at: PlayerPosition,
    motion: [f64; 3],
    born: u64,
    in_water: bool,
    bite_at: Option<u64>,
}
impl Bobber {
    fn spawn(&self) -> Event {
        let mut body = Vec::new();
        write_varint(self.id as u32, &mut body);
        body.push(90); // Fishing hook object
        for value in [self.at.x, self.at.y, self.at.z] {
            body.extend_from_slice(&((value * 32.0).floor() as i32).to_be_bytes());
        }
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(&self.owner.to_be_bytes());
        for value in self.motion {
            body.extend_from_slice(
                &((value * 8000.0).clamp(-32768.0, 32767.0) as i16).to_be_bytes(),
            );
        }
        Event { id: 0x0e, body }
    }
}
#[derive(Default, Clone)]
pub struct Fishing {
    next: i32,
    floats: BTreeMap<i32, Bobber>,
}
pub struct Reel {
    pub events: Vec<Event>,
    pub loot: Option<Stack>,
}
impl Fishing {
    pub fn has_float(&self, owner: i32) -> bool {
        self.floats.contains_key(&owner)
    }
    pub fn can_cast(&self, owner: i32, player: PlayerPosition, yaw: f32, pitch: f32) -> bool {
        owner > 0
            && !self.has_float(owner)
            && self.floats.len() < LIMIT
            && self.next.checked_add(FIRST_ID).is_some()
            && player.is_valid()
            && player.y < 254.0
            && yaw.is_finite()
            && pitch.is_finite()
    }
    pub fn cast(
        &mut self,
        owner: i32,
        player: PlayerPosition,
        yaw: f32,
        pitch: f32,
        now: u64,
    ) -> Option<Event> {
        if !self.can_cast(owner, player, yaw, pitch) {
            return None;
        }
        let id = FIRST_ID.checked_add(self.next)?;
        self.next = self.next.checked_add(1)?;
        let heading = (yaw as f64).to_radians();
        let elevation = (pitch as f64).to_radians();
        let float = Bobber {
            id,
            owner,
            at: PlayerPosition {
                y: player.y + 1.4,
                ..player
            },
            motion: [
                -heading.sin() * elevation.cos() * 0.9,
                -elevation.sin() * 0.9 + 0.15,
                heading.cos() * elevation.cos() * 0.9,
            ],
            born: now,
            in_water: false,
            bite_at: None,
        };
        let event = float.spawn();
        self.floats.insert(owner, float);
        Some(event)
    }
    /// Catch is only possible after server-observed water entry and a bite interval.
    pub fn reel(&mut self, owner: i32, now: u64) -> Option<Reel> {
        let float = self.floats.remove(&owner)?;
        let mut result = Reel {
            events: vec![multiplayer::destroy(float.id)],
            loot: None,
        };
        if let Some(bite) = float.bite_at {
            if float.in_water && now >= bite && now.saturating_sub(bite) <= 40 {
                let seed = (float.id as u64).wrapping_mul(0x9e3779b97f4a7c15) ^ bite;
                let rolled = seed ^ (seed >> 17) ^ (seed >> 31);
                // 1.7.10 categories: ~85% fish, 10% junk, 5% treasure.
                result.loot = Some(match rolled % 100 {
                    0..=84 => Stack {
                        id: 349,
                        count: 1,
                        damage: match (rolled >> 8) % 10 {
                            0 => 1,
                            1 => 2,
                            2 => 3,
                            _ => 0,
                        },
                        nbt: None,
                    },
                    85..=94 => Stack {
                        id: 287,
                        count: 1,
                        damage: 0,
                        nbt: None,
                    }, // string (junk)
                    _ => Stack {
                        id: 266,
                        count: 1,
                        damage: 0,
                        nbt: None,
                    }, // gold ingot (treasure)
                });
            }
        }
        Some(result)
    }
    pub fn remove_owner(&mut self, owner: i32) -> Option<Event> {
        self.floats
            .remove(&owner)
            .map(|value| multiplayer::destroy(value.id))
    }
    pub fn snapshot(&self, observer: PlayerPosition) -> Vec<Event> {
        self.floats
            .values()
            .filter(|f| multiplayer::visible(&observer, &f.at))
            .map(Bobber::spawn)
            .collect()
    }
    /// Called from the existing 20TPS world thread; positions are never accepted from clients.
    pub fn tick(&mut self, world: &World, now: u64) -> Vec<(PlayerPosition, Event)> {
        let mut output = Vec::new();
        let mut expired = Vec::new();
        for float in self.floats.values_mut() {
            if now.saturating_sub(float.born) >= 1_200 {
                expired.push(float.owner);
                continue;
            }
            if float.in_water {
                if float.bite_at.is_none() && now.saturating_sub(float.born) >= 100 {
                    let delay =
                        (float.id as u64 ^ float.born).wrapping_mul(1_103_515_245) % 140 + 40;
                    float.bite_at = Some(now + delay);
                }
                if float.bite_at.is_some_and(|at| now > at.saturating_add(40)) {
                    float.bite_at = None; // wait for another bite
                    float.born = now.saturating_sub(100);
                }
                continue;
            }
            let next = PlayerPosition {
                x: float.at.x + float.motion[0],
                y: float.at.y + float.motion[1],
                z: float.at.z + float.motion[2],
            };
            if !next.is_valid() {
                expired.push(float.owner);
                continue;
            }
            let kind = world.get(
                next.x.floor() as i32,
                next.y.floor() as i32,
                next.z.floor() as i32,
            ) >> 4;
            if matches!(kind, 8 | 9) {
                float.in_water = true;
                float.at = next;
                float.motion = [0.0; 3];
            } else if kind != 0 {
                expired.push(float.owner);
                continue;
            } else {
                float.at = next;
                float.motion[0] *= 0.91;
                float.motion[1] = float.motion[1] * 0.91 - 0.03;
                float.motion[2] *= 0.91;
            }
            output.push((
                float.at,
                multiplayer::teleport(float.id, &float.at, 0.0, 0.0),
            ));
        }
        for owner in expired {
            if let Some(value) = self.floats.remove(&owner) {
                output.push((value.at, multiplayer::destroy(value.id)));
            }
        }
        output
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_one_float_per_owner_and_no_dry_catches() {
        let at = PlayerPosition {
            x: 0.5,
            y: 70.0,
            z: 0.5,
        };
        let mut f = Fishing::default();
        let packet = f.cast(4, at, 0.0, 0.0, 10).unwrap();
        assert_eq!(packet.id, 0x0e);
        assert!(!f.can_cast(4, at, 0.0, 0.0));
        assert!(f.reel(4, 200).unwrap().loot.is_none());
        assert!(!f.has_float(4));
        assert!(f.cast(4, at, f32::NAN, 0.0, 210).is_none());
    }
}
