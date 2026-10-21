//! Environmental Survival hazards sampled at the authoritative 20Hz tick.
use crate::{physics, player::PlayerPosition, world::World};

#[derive(Debug)]
pub struct Ambient {
    last_tick: u64,
    air: i32,
    drown_timer: u8,
    burning: u16,
    burn_timer: u8,
    lava_timer: u8,
    suffocate_timer: u8,
}
impl Ambient {
    pub fn new(tick: u64) -> Self {
        Self {
            last_tick: tick,
            air: 300,
            drown_timer: 0,
            burning: 0,
            burn_timer: 0,
            lava_timer: 0,
            suffocate_timer: 0,
        }
    }
    pub fn air(&self) -> i32 {
        self.air
    }
    /// Damage points over bounded elapsed server ticks. No offline catch-up.
    pub fn advance(&mut self, tick: u64, world: &World, pos: &PlayerPosition, armor: u8) -> f32 {
        let steps = tick.saturating_sub(self.last_tick).min(20);
        self.last_tick = tick;
        let eye_block = world.get(
            pos.x.floor() as i32,
            (pos.y + 1.62).floor() as i32,
            pos.z.floor() as i32,
        ) >> 4;
        let submerged = matches!(eye_block, 8 | 9);
        let lava = physics::in_lava(world, pos);
        let standing_fire = [0.1, 0.9].into_iter().any(|offset| {
            world.get(
                pos.x.floor() as i32,
                (pos.y + offset).floor() as i32,
                pos.z.floor() as i32,
            ) >> 4
                == 51
        });
        let choking = physics::collides(world, pos);
        let mut raw = 0.0_f32;
        let mut bypass = 0.0_f32;
        for _ in 0..steps {
            if submerged {
                if self.air > 0 {
                    self.air -= 1;
                } else {
                    self.drown_timer = self.drown_timer.saturating_add(1);
                    if self.drown_timer >= 20 {
                        self.drown_timer = 0;
                        bypass += 2.0;
                    }
                }
            } else {
                self.air = (self.air + 4).min(300);
                self.drown_timer = 0;
            }
            if lava {
                self.burning = 100;
                self.lava_timer += 1;
                if self.lava_timer >= 10 {
                    self.lava_timer = 0;
                    raw += 4.0;
                }
            } else {
                self.lava_timer = 0;
                if standing_fire {
                    self.burning = 100;
                }
                if self.burning > 0 {
                    self.burning -= 1;
                    self.burn_timer += 1;
                    if self.burn_timer >= 20 {
                        self.burn_timer = 0;
                        raw += 1.0;
                    }
                } else {
                    self.burn_timer = 0;
                }
            }
            if choking {
                self.suffocate_timer += 1;
                if self.suffocate_timer >= 10 {
                    self.suffocate_timer = 0;
                    bypass += 1.0;
                }
            } else {
                self.suffocate_timer = 0;
            }
        }
        bypass + crate::vitals::armored_damage(raw, armor)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn drowning_after_fifteen_seconds_and_air_recovers() {
        let mut world = World::new();
        let at = PlayerPosition {
            x: 0.5,
            y: 65.0,
            z: 0.5,
        };
        world.import_block(0, 66, 0, 9 << 4).unwrap();
        let mut state = Ambient::new(0);
        let mut total = 0.0;
        for i in 1..=340 {
            total += state.advance(i, &world, &at, 0);
        }
        assert_eq!(total, 4.0);
        let world = World::new();
        for i in 341..=420 {
            state.advance(i, &world, &at, 0);
        }
        assert!(state.air() > 0);
    }
    #[test]
    fn lava_and_armor_reduce_burning_damage() {
        let mut world = World::new();
        let at = PlayerPosition {
            x: 0.5,
            y: 64.0,
            z: 0.5,
        };
        world.import_block(0, 64, 0, 10 << 4).unwrap();
        let mut bare = Ambient::new(0);
        let mut armored = Ambient::new(0);
        assert!(bare.advance(20, &world, &at, 0) > armored.advance(20, &world, &at, 20));
    }
}
