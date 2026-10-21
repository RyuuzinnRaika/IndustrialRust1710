//! Server-owned Minecraft 1.7.10 Survival food and health subset.
//! Bounded food/exhaustion arithmetic and 20TPS healing/starvation.
//! Does not yet implement potions, item NBT, food effects or full sprint logic.
use crate::inventory::Stack;

/// Vanilla 1.7.10 armor formula (damage after pre-enchantment protection).
pub fn armored_damage(amount: f32, armor_points: u8) -> f32 {
    amount * (25.0 - f32::from(armor_points.min(20))) / 25.0
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vitals {
    pub health: f32,
    pub food: i32,
    pub saturation: f32,
    pub exhaustion: f32,
    pub food_timer: u32,
}

impl Default for Vitals {
    fn default() -> Self {
        Self {
            health: 20.0,
            food: 20,
            saturation: 5.0,
            exhaustion: 0.0,
            food_timer: 0,
        }
    }
}

impl Vitals {
    pub fn valid(&self) -> bool {
        self.health.is_finite()
            && (0.0..=20.0).contains(&self.health)
            && (0..=20).contains(&self.food)
            && self.saturation.is_finite()
            && (0.0..=20.0).contains(&self.saturation)
            && self.exhaustion.is_finite()
            && (0.0..=40.0).contains(&self.exhaustion)
            && self.food_timer <= 80
    }

    /// Limited 1.7.10 food set. Saturation modifier is the vanilla-like multiplier.
    fn nutrition(id: i16) -> Option<(i32, f32)> {
        match id {
            260 => Some((4, 0.3)), // apple
            297 => Some((5, 0.6)), // bread
            319 => Some((3, 0.3)), // raw pork
            320 => Some((8, 0.8)), // cooked pork
            349 => Some((2, 0.1)), // raw fish
            350 => Some((5, 0.6)), // cooked fish
            357 => Some((2, 0.1)), // cookie
            360 => Some((2, 0.3)), // melon
            363 => Some((3, 0.3)), // raw beef
            364 => Some((8, 0.8)), // steak
            365 => Some((2, 0.3)), // raw chicken
            366 => Some((6, 0.6)), // cooked chicken
            391 => Some((3, 0.6)), // carrot
            392 => Some((1, 0.3)), // potato
            393 => Some((5, 0.6)), // baked potato
            400 => Some((8, 0.3)), // pumpkin pie
            _ => None,
        }
    }

    pub fn can_eat(&self, stack: Stack) -> bool {
        self.health > 0.0 && self.food < 20 && Self::nutrition(stack.id).is_some()
    }

    /// Apply food only after the inventory was durably charged.
    pub fn eat(&mut self, stack: Stack) -> bool {
        let Some((restore, modifier)) = Self::nutrition(stack.id) else {
            return false;
        };
        if !self.can_eat(stack) {
            return false;
        }
        self.food = (self.food + restore).min(20);
        self.saturation = (self.saturation + restore as f32 * modifier * 2.0).min(self.food as f32);
        true
    }

    /// Vanilla-inspired exhaustion: 4 points drains saturation before food.
    pub fn travel(&mut self, horizontal_blocks: f64) -> bool {
        if self.health <= 0.0 || !horizontal_blocks.is_finite() || horizontal_blocks <= 0.0 {
            return false;
        }
        let before = (self.food, self.saturation, self.exhaustion);
        self.exhaustion = (self.exhaustion + (horizontal_blocks.min(64.0) as f32 * 0.01)).min(40.0);
        while self.exhaustion >= 4.0 {
            self.exhaustion -= 4.0;
            if self.saturation > 0.0 {
                self.saturation = (self.saturation - 1.0).max(0.0);
            } else if self.food > 0 {
                self.food -= 1;
            }
        }
        before != (self.food, self.saturation, self.exhaustion)
    }

    /// Returns true if health or food changed. Must run only once per game tick.
    pub fn tick(&mut self, steps: u64) -> bool {
        let before = (self.health, self.food, self.saturation);
        if self.health <= 0.0 {
            return false;
        }
        for _ in 0..steps.min(20) {
            let recovering = self.food >= 18 && self.health < 20.0;
            let starving = self.food == 0 && self.health > 1.0;
            if recovering || starving {
                self.food_timer += 1;
                if self.food_timer >= 80 {
                    self.food_timer = 0;
                    if recovering {
                        self.health = (self.health + 1.0).min(20.0);
                        self.exhaustion = (self.exhaustion + 3.0).min(40.0);
                    } else {
                        self.health = (self.health - 1.0).max(1.0);
                    }
                }
            } else {
                self.food_timer = 0;
            }
        }
        before != (self.health, self.food, self.saturation)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn armor_reduction_is_bounded() {
        assert_eq!(super::armored_damage(5.0, 0), 5.0);
        assert_eq!(super::armored_damage(5.0, 20), 1.0);
        assert_eq!(super::armored_damage(5.0, 99), 1.0);
    }

    use super::*;
    fn food(id: i16) -> Stack {
        Stack {
            id,
            count: 1,
            damage: 0,
        }
    }
    #[test]
    fn apple_and_steak_restore_food_without_exceeding_limits() {
        let mut v = Vitals {
            food: 12,
            saturation: 0.0,
            ..Vitals::default()
        };
        assert!(v.eat(food(260)));
        assert_eq!(v.food, 16);
        assert!((v.saturation - 2.4).abs() < 0.001);
        assert!(v.eat(food(364)));
        assert_eq!(v.food, 20);
        assert!(v.saturation <= 20.0);
        assert!(!v.eat(food(364))); // full hunger
        assert!(!v.eat(food(1))); // not food
    }
    #[test]
    fn travel_drains_saturation_then_food_and_ticking_heals() {
        let mut v = Vitals {
            health: 15.0,
            food: 20,
            saturation: 1.0,
            ..Vitals::default()
        };
        for _ in 0..8 {
            v.travel(64.0);
        }
        assert_eq!(v.food, 20);
        for _ in 0..7 {
            v.travel(64.0);
        }
        assert!(v.food < 20);
        let mut healthy = Vitals {
            health: 15.0,
            ..Vitals::default()
        };
        assert!(!healthy.tick(20));
        for _ in 0..3 {
            healthy.tick(20);
        }
        assert_eq!(healthy.health, 16.0);
    }
    #[test]
    fn normal_difficulty_starvation_stops_at_one_health() {
        let mut v = Vitals {
            food: 0,
            saturation: 0.0,
            health: 3.0,
            ..Vitals::default()
        };
        for _ in 0..20 {
            v.tick(20);
        }
        assert_eq!(v.health, 1.0);
        assert!(v.valid());
        v.exhaustion = f32::NAN;
        assert!(!v.valid());
    }
}
