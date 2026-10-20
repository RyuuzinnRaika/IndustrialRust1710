//! Minimal server-owned Zombie (entity type 54) for natural Survival IRW2.
//! Uses Minecraft 1.7.10 Protocol 5 spawn/teleport/status/destroy packets.
//! Does not yet model vanilla pathfinding, lighting, difficulty or persistence.
use crate::inventory::Stack;
use crate::multiplayer::{self, Event};
use crate::physics;
use crate::player::PlayerPosition;
use crate::world::World;
use legacy_protocol::write_varint;
use std::collections::BTreeMap;

const BASE_ID: i32 = 500_000;
const ATTACK_INTERVAL: u64 = 20;
const MAX_STEPS: u64 = 4;

#[derive(Clone, Copy, Debug)]
struct Mob {
    id: i32,
    position: PlayerPosition,
    health: i16,
    hit_tick: u64,
    bite_tick: u64,
}
impl Mob {
    fn spawn(self) -> Event {
        let mut body = Vec::new();
        write_varint(self.id as u32, &mut body);
        body.push(54); // Zombie, 1.7.10 entity type
        for value in [self.position.x, self.position.y, self.position.z] {
            body.extend_from_slice(&((value * 32.0).floor() as i32).to_be_bytes());
        }
        body.extend_from_slice(&[0, 0, 0]); // yaw, pitch, head pitch
        for _ in 0..3 {
            body.extend_from_slice(&0i16.to_be_bytes()); // initial velocity
        }
        body.extend_from_slice(&[0, 0, 0x7f]); // metadata flags, end
        Event { id: 0x0f, body }
    }
    fn status(self, value: u8) -> Event {
        let mut body = self.id.to_be_bytes().to_vec();
        body.push(value); // 2=hurt, 3=death
        Event { id: 0x1a, body }
    }
    fn nearby(self, position: PlayerPosition, radius: f64) -> bool {
        let dx = self.position.x - position.x;
        let dz = self.position.z - position.z;
        let dy = self.position.y - position.y;
        dx * dx + dy * dy + dz * dz <= radius * radius
    }
}
#[derive(Debug)]
pub struct Attack {
    pub events: Vec<Event>,
    pub drop: Option<(Stack, (i32, i32, i32))>,
}
#[derive(Default, Debug)]
pub struct Mobs {
    entities: BTreeMap<i32, Mob>,
    last_tick: u64,
}
impl Mobs {
    /// Only spawn in the opt-in natural Survival world; flat E2E behavior is unchanged.
    pub fn new(world: &World, enabled: bool) -> Self {
        let mut state = Self::default();
        if enabled && world.is_natural() {
            for (n, (x, z)) in [(8, 9), (-8, 7), (7, -8), (-9, -7)].into_iter().enumerate() {
                let pos = PlayerPosition {
                    x: f64::from(x) + 0.5,
                    y: world.natural_seed().map_or(65.0, |seed| {
                        f64::from(crate::terrain::surface(seed, x, z) + 1)
                    }),
                    z: f64::from(z) + 0.5,
                };
                if !physics::collides(world, &pos) && physics::supported(world, &pos) {
                    let mob = Mob {
                        id: BASE_ID + n as i32,
                        position: pos,
                        health: 20,
                        hit_tick: 0,
                        bite_tick: 0,
                    };
                    state.entities.insert(mob.id, mob);
                }
            }
        }
        state
    }
    pub fn spawn_events(&self) -> impl Iterator<Item = Event> + '_ {
        self.entities.values().copied().map(Mob::spawn)
    }
    /// A bounded 20TPS movement step pursuing the closest nearby player.
    pub fn tick(
        &mut self,
        now: u64,
        world: &World,
        players: &[PlayerPosition],
    ) -> Vec<Event> {
        let steps = now.saturating_sub(self.last_tick).min(MAX_STEPS);
        self.last_tick = now;
        let mut events = Vec::new();
        for _ in 0..steps {
            for mob in self.entities.values_mut() {
                let nearest = players.iter().copied().filter(|position| {
                    mob.nearby(*position, 16.0)
                }).min_by(|left, right| {
                    let distance = |p: PlayerPosition| {
                        (p.x - mob.position.x).powi(2) + (p.z - mob.position.z).powi(2)
                    };
                    distance(*left).total_cmp(&distance(*right))
                });
                let Some(target) = nearest else { continue; };
                let dx = target.x - mob.position.x;
                let dz = target.z - mob.position.z;
                let length = dx.hypot(dz);
                if length < 1.5 {
                    continue;
                }
                let candidate = PlayerPosition {
                    x: mob.position.x + dx / length * 0.11,
                    y: mob.position.y,
                    z: mob.position.z + dz / length * 0.11,
                };
                if physics::valid_movement(world, &mob.position, &candidate)
                    && physics::supported(world, &candidate)
                {
                    mob.position = candidate;
                    events.push(multiplayer::teleport(mob.id, &candidate, 0.0, 0.0));
                }
            }
        }
        events
    }
    /// Server checks the actual target, reach, held item, and attack cooldown.
    pub fn player_attack(
        &mut self,
        id: i32,
        at: PlayerPosition,
        held: Option<Stack>,
        tick: u64,
    ) -> Option<Attack> {
        let mob = self.entities.get_mut(&id)?;
        if !mob.nearby(at, 3.0) || (mob.hit_tick != 0 && tick - mob.hit_tick < 10) {
            return None;
        }
        mob.hit_tick = tick;
        let damage = match held.map(|stack| stack.id) {
            Some(268) | Some(283) => 4,
            Some(272) => 5,
            Some(267) => 6,
            Some(276) => 7,
            _ => 1,
        };
        mob.health -= damage;
        let killed = mob.health <= 0;
        let outcome = Attack {
            events: if killed {
                vec![mob.status(3), multiplayer::destroy(id)]
            } else {
                vec![mob.status(2)]
            },
            drop: killed.then_some((
                Stack { id: 367, count: 1, damage: 0 }, // rotten flesh
                (
                    mob.position.x.floor() as i32,
                    mob.position.y.floor() as i32,
                    mob.position.z.floor() as i32,
                ),
            )),
        };
        if killed {
            self.entities.remove(&id);
        }
        Some(outcome)
    }
    /// Nearby zombie applies a bounded normal-difficulty melee hit.
    pub fn hurt_player(&mut self, player: PlayerPosition, tick: u64) -> bool {
        for mob in self.entities.values_mut() {
            if mob.nearby(player, 1.6)
                && (mob.bite_tick == 0 || tick.saturating_sub(mob.bite_tick) >= ATTACK_INTERVAL)
            {
                mob.bite_tick = tick;
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protocol5_zombie_spawn_attack_and_rotten_flesh() {
        let world = World::natural(42);
        let mut zombies = Mobs::new(&world, true);
        assert!(!zombies.entities.is_empty());
        let mob = *zombies.entities.values().next().unwrap();
        let packet = mob.spawn();
        assert_eq!(packet.id, 0x0f);
        assert_eq!(packet.body[3], 54);
        assert!(packet.body.ends_with(&[0, 0, 0x7f]));
        assert!(zombies.player_attack(mob.id, PlayerPosition::default(), None, 100).is_none());
        for tick in [100, 111] {
            let outcome = zombies.player_attack(mob.id, mob.position, Some(Stack {
                id: 276, count: 1, damage: 0
            }), tick).unwrap();
            assert!(outcome.drop.is_none());
        }
        let result = zombies.player_attack(mob.id, mob.position, Some(Stack {
            id: 276, count: 1, damage: 0
        }), 122).unwrap();
        assert_eq!(result.drop.unwrap().0.id, 367);
        assert!(result.events.iter().any(|packet| packet.id == 0x13));
        assert!(zombies.player_attack(mob.id, mob.position, None, 150).is_none());
    }
    #[test]
    fn zombie_avoids_invalid_attacks_and_flat_worlds() {
        assert!(Mobs::new(&World::new(), true).entities.is_empty());
        assert!(Mobs::new(&World::natural(42), false).entities.is_empty());
    }
}
