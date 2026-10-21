//! Server-owned Minecraft 1.7.10 Survival food and health subset.
//! Bounded food/exhaustion arithmetic and 20TPS healing/starvation.
//! Does not yet implement potions, item NBT, food effects or full sprint logic.
use crate::inventory::Stack;

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
    fn nutrition(stack: Stack) -> Option<(i32, f32)> {
        match (stack.id, stack.damage) {
            (260, _) => Some((4, 0.3)),     // apple
            (282, _) => Some((6, 0.6)),     // mushroom stew (bowl returned by caller)
            (297, _) => Some((5, 0.6)),     // bread
            (319, _) => Some((3, 0.3)),     // raw pork
            (320, _) => Some((8, 0.8)),     // cooked pork
            (349, 0 | 1) => Some((2, 0.1)), // cod / salmon
            (349, 2 | 3) => Some((1, 0.1)), // clownfish / pufferfish (effects TODO)
            (350, 0) => Some((5, 0.6)),     // cooked cod
            (350, 1) => Some((6, 0.8)),     // cooked salmon
            (357, _) => Some((2, 0.1)),     // cookie
            (360, _) => Some((2, 0.3)),     // melon
            (363, _) => Some((3, 0.3)),     // raw beef
            (364, _) => Some((8, 0.8)),     // steak
            (365, _) => Some((2, 0.3)),     // raw chicken
            (366, _) => Some((6, 0.6)),     // cooked chicken
            (367, _) => Some((4, 0.1)),     // rotten flesh (Hunger effect TODO)
            (375, _) => Some((2, 0.8)),     // spider eye (Poison effect TODO)
            (391, _) => Some((3, 0.6)),     // carrot
            (392, _) => Some((1, 0.3)),     // potato
            (393, _) => Some((5, 0.6)),     // baked potato
            (394, _) => Some((2, 0.3)),     // poisonous potato (Poison effect TODO)
            (400, _) => Some((8, 0.3)),     // pumpkin pie
            (396, _) => Some((6, 1.2)),     // golden carrot
            _ => None,
        }
    }

    pub fn can_eat(&self, stack: Stack) -> bool {
        self.health > 0.0 && self.food < 20 && Self::nutrition(stack).is_some()
    }

    /// Apply food only after the inventory was durably charged.
    pub fn eat(&mut self, stack: Stack) -> bool {
        let Some((restore, modifier)) = Self::nutrition(stack) else {
            return false;
        };
        if !self.can_eat(stack) {
            return false;
        }
        self.food = (self.food + restore).min(20);
        self.saturation = (self.saturation + restore as f32 * modifier * 2.0).min(self.food as f32);
        true
    }

    /// Apply and settle exhaustion. Use for movement, combat and regeneration.
    /// Four exhaustion points drain saturation first, then one food point.
    pub fn add_exhaustion(&mut self, amount: f32) -> bool {
        if self.health <= 0.0 || !amount.is_finite() || amount <= 0.0 {
            return false;
        }
        let before = (self.food, self.saturation, self.exhaustion);
        self.exhaustion = (self.exhaustion + amount).min(40.0);
        self.settle_exhaustion();
        before != (self.food, self.saturation, self.exhaustion)
    }

    fn settle_exhaustion(&mut self) {
        while self.exhaustion >= 4.0 {
            self.exhaustion -= 4.0;
            if self.saturation > 0.0 {
                self.saturation = (self.saturation - 1.0).max(0.0);
            } else if self.food > 0 {
                self.food -= 1;
            }
        }
    }

    /// Walking movement, retained for callers that do not track sprint state.
    pub fn travel(&mut self, horizontal_blocks: f64) -> bool {
        self.travel_with_sprint(horizontal_blocks, false)
    }

    /// The 1.7.10 client reports sprint start/stop through Entity Action.
    /// Exhaustion is authoritative and bounded even on large position jumps.
    pub fn travel_with_sprint(&mut self, horizontal_blocks: f64, sprinting: bool) -> bool {
        if self.health <= 0.0 || !horizontal_blocks.is_finite() || horizontal_blocks <= 0.0 {
            return false;
        }
        let per_block = if sprinting { 0.1 } else { 0.01 };
        self.add_exhaustion(horizontal_blocks.min(64.0) as f32 * per_block)
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
                        self.add_exhaustion(3.0);
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
    fn sprinting_drains_saturation_faster_without_creating_negative_food() {
        let mut walking = Vitals::default();
        let mut sprinting = Vitals::default();
        assert!(walking.travel(40.0));
        assert!(sprinting.travel_with_sprint(40.0, true));
        assert_eq!(walking.saturation, 5.0);
        assert_eq!(sprinting.saturation, 4.0);
        assert!(sprinting.valid());
        assert!(!sprinting.travel_with_sprint(f64::NAN, true));
        assert!(!sprinting.travel_with_sprint(-1.0, true));
    }

    #[test]
    fn legacy_stew_and_emergency_food_have_bounded_nutrition() {
        for (id, food_gain, saturation_gain) in [
            (282, 6, 7.2_f32),
            (367, 4, 0.8),
            (375, 2, 3.2),
            (394, 2, 1.2),
        ] {
            let stack = Stack {
                id,
                count: 1,
                damage: 0,
            };
            let mut vitals = Vitals {
                food: 5,
                saturation: 0.0,
                ..Vitals::default()
            };
            assert!(vitals.can_eat(stack));
            assert!(vitals.eat(stack));
            assert_eq!(vitals.food, 5 + food_gain);
            assert!((vitals.saturation - saturation_gain).abs() < 0.001);
        }
    }

    #[test]
    fn metadata_food_and_regeneration_exhaustion_are_server_accounted() {
        let mut v = Vitals {
            food: 10,
            saturation: 0.0,
            ..Vitals::default()
        };
        assert!(v.eat(Stack {
            id: 396,
            count: 1,
            damage: 0
        }));
        assert_eq!(v.food, 16);
        assert!((v.saturation - 14.4).abs() < 0.001);
        let mut salmon = Vitals {
            food: 10,
            saturation: 0.0,
            ..Vitals::default()
        };
        assert!(salmon.eat(Stack {
            id: 350,
            count: 1,
            damage: 1
        }));
        assert_eq!(salmon.food, 16);
        assert!((salmon.saturation - 9.6).abs() < 0.001);
        let mut regen = Vitals {
            food: 20,
            saturation: 0.0,
            health: 18.0,
            exhaustion: 2.0,
            ..Vitals::default()
        };
        for _ in 0..4 {
            regen.tick(20);
        }
        assert_eq!(regen.health, 19.0);
        assert_eq!(regen.food, 19);
        assert!((regen.exhaustion - 1.0).abs() < 0.001);
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
