//! Server-owned, bounded survival environmental damage for Protocol 5.
use crate::inventory::Inventory;
use crate::player::PlayerPosition;
use crate::world::World;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HazardCause {
    Cactus,
    Drowning,
    Suffocation,
    Lava,
    Fire,
    Burning,
}

/// A single tick may contain multiple independent environmental sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HazardAttribution {
    None,
    Single(HazardCause),
    Mixed,
}

impl HazardAttribution {
    pub fn combine(self, next: Self) -> Self {
        match (self, next) {
            (Self::Mixed, _) | (_, Self::Mixed) => Self::Mixed,
            (Self::None, value) | (value, Self::None) => value,
            (Self::Single(a), Self::Single(b)) if a == b => Self::Single(a),
            _ => Self::Mixed,
        }
    }
}

/// Check the horizontal player bounding box, not just the block containing
/// the player's feet: a cactus damages players touching its side.
fn touches_cactus(world: &World, pos: PlayerPosition) -> bool {
    if !pos.x.is_finite() || !pos.y.is_finite() || !pos.z.is_finite() {
        return false;
    }
    const PLAYER_HALF: f64 = 0.3;
    const CACTUS_INSET: f64 = 1.0 / 16.0;
    let player_x_min = pos.x - PLAYER_HALF;
    let player_x_max = pos.x + PLAYER_HALF;
    let player_z_min = pos.z - PLAYER_HALF;
    let player_z_max = pos.z + PLAYER_HALF;
    for bx in (player_x_min.floor() as i32)..=(player_x_max.floor() as i32) {
        for bz in (player_z_min.floor() as i32)..=(player_z_max.floor() as i32) {
            let cactus_x_min = f64::from(bx) + CACTUS_INSET;
            let cactus_x_max = f64::from(bx) + 1.0 - CACTUS_INSET;
            let cactus_z_min = f64::from(bz) + CACTUS_INSET;
            let cactus_z_max = f64::from(bz) + 1.0 - CACTUS_INSET;
            if player_x_max <= cactus_x_min
                || player_x_min >= cactus_x_max
                || player_z_max <= cactus_z_min
                || player_z_min >= cactus_z_max
            {
                continue;
            }
            for by in (pos.y.floor() as i32)..=((pos.y + 1.8).floor() as i32) {
                if world.get(bx, by, bz) >> 4 == 81 {
                    return true;
                }
            }
        }
    }
    false
}

#[derive(Debug, Default)]
pub struct Hazards {
    air: u16,
    wet_ticks: u16,
    fire_ticks: u16,
    last_fire_hit: u64,
    last_suffocation_hit: u64,
    last_cactus_hit: Option<u64>,
}
impl Hazards {
    pub fn new() -> Self {
        Self {
            air: 300,
            ..Self::default()
        }
    }
    /// Compute damage for ONE 20Hz tick. Raw damage is reduced for contact/heat,
    /// but drowning and suffocation bypass armour in vanilla.
    #[cfg(test)]
    pub fn tick(
        &mut self,
        world: &World,
        pos: PlayerPosition,
        tick: u64,
        inventory: &Inventory,
    ) -> f32 {
        self.tick_detailed(world, pos, tick, inventory).0
    }

    /// Report the source that actually inflicted damage this tick.
    pub fn tick_detailed(
        &mut self,
        world: &World,
        pos: PlayerPosition,
        tick: u64,
        inventory: &Inventory,
    ) -> (f32, HazardAttribution) {
        let (x, y, z) = (
            pos.x.floor() as i32,
            pos.y.floor() as i32,
            pos.z.floor() as i32,
        );
        let feet = world.get(x, y, z) >> 4;
        let head = world.get(x, (pos.y + 1.62).floor() as i32, z) >> 4;
        let submerged = matches!(head, 8 | 9);
        let mut raw = 0.0_f32;
        let mut bypass = 0.0_f32;
        let mut cause = HazardAttribution::None;
        if touches_cactus(world, pos)
            && self
                .last_cactus_hit
                .is_none_or(|last| tick.saturating_sub(last) >= 10)
        {
            self.last_cactus_hit = Some(tick);
            raw += 1.0;
            cause = cause.combine(HazardAttribution::Single(HazardCause::Cactus));
        }
        if submerged {
            self.air = self.air.saturating_sub(1);
            self.wet_ticks = self.wet_ticks.saturating_add(1);
            if self.air == 0 && self.wet_ticks.is_multiple_of(20) {
                bypass += 2.0;
                cause = cause.combine(HazardAttribution::Single(HazardCause::Drowning));
            }
        } else {
            self.air = 300;
            self.wet_ticks = 0;
        }
        if matches!(
            head,
            1 | 2
                | 3
                | 4
                | 5
                | 7
                | 12
                | 13
                | 14
                | 15
                | 16
                | 17
                | 20
                | 24
                | 35
                | 41
                | 42
                | 45
                | 49
                | 56
                | 57
                | 73
                | 74
                | 80
                | 82
                | 87
                | 89
                | 98
                | 129
                | 155
                | 159
                | 162
                | 168
                | 169
                | 170
                | 172
                | 173
        ) && tick.saturating_sub(self.last_suffocation_hit) >= 10
        {
            self.last_suffocation_hit = tick;
            bypass += 1.0;
            cause = cause.combine(HazardAttribution::Single(HazardCause::Suffocation));
        }
        if matches!(feet, 10 | 11) {
            self.fire_ticks = self.fire_ticks.max(100);
            if tick.saturating_sub(self.last_fire_hit) >= 10 {
                self.last_fire_hit = tick;
                raw += 4.0;
                cause = cause.combine(HazardAttribution::Single(HazardCause::Lava));
            }
        } else if feet == 51 {
            self.fire_ticks = self.fire_ticks.max(80);
        }
        if self.fire_ticks > 0 && !submerged {
            self.fire_ticks -= 1;
            if !matches!(feet, 10 | 11) && tick.saturating_sub(self.last_fire_hit) >= 20 {
                self.last_fire_hit = tick;
                raw += 1.0;
                cause = cause.combine(HazardAttribution::Single(if feet == 51 {
                    HazardCause::Fire
                } else {
                    HazardCause::Burning
                }));
            }
        } else if submerged {
            self.fire_ticks = 0;
        }
        let points = inventory.armor_points().min(20) as f32;
        let damage = (raw * (25.0 - points) / 25.0).ceil() + bypass;
        (
            damage,
            if damage > 0.0 {
                cause
            } else {
                HazardAttribution::None
            },
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn simultaneous_heat_and_drowning_never_claim_single_source() {
        let mut world = World::new();
        world.import_block(0, 64, 0, 10 << 4).unwrap(); // lava at feet
        world.import_block(0, 66, 0, 9 << 4).unwrap(); // water at eye
        let mut hazard = Hazards::new();
        let mut simultaneous = false;
        for tick in 1..=320 {
            let (amount, origin) = hazard.tick_detailed(
                &world,
                PlayerPosition::default(),
                tick,
                &Inventory::default(),
            );
            if amount > 0.0 && origin == HazardAttribution::Mixed {
                simultaneous = true;
            }
        }
        assert!(simultaneous);
        assert_eq!(
            HazardAttribution::Single(HazardCause::Fire)
                .combine(HazardAttribution::Single(HazardCause::Fire)),
            HazardAttribution::Single(HazardCause::Fire)
        );
        assert_eq!(
            HazardAttribution::Single(HazardCause::Fire)
                .combine(HazardAttribution::Single(HazardCause::Drowning)),
            HazardAttribution::Mixed
        );
    }

    #[test]
    fn cactus_side_contact_deals_server_owned_bounded_damage() {
        let mut world = World::new();
        world.import_block(0, 64, 0, 81 << 4).unwrap();
        let contact = PlayerPosition {
            x: 1.2,
            y: 64.0,
            z: 0.5,
        };
        let no_contact = PlayerPosition {
            x: 1.4,
            y: 64.0,
            z: 0.5,
        };
        let mut hazard = Hazards::new();
        let inventory = Inventory::default();
        let (first, source) = hazard.tick_detailed(&world, contact, 1, &inventory);
        assert_eq!(first, 1.0);
        assert_eq!(source, HazardAttribution::Single(HazardCause::Cactus));
        assert_eq!(hazard.tick_detailed(&world, contact, 2, &inventory).0, 0.0);
        assert_eq!(hazard.tick_detailed(&world, contact, 11, &inventory).0, 1.0);
        let mut other = Hazards::new();
        assert_eq!(other.tick_detailed(&world, no_contact, 1, &inventory).0, 0.0);
    }

    #[test]
    fn drowned_player_loses_air_before_health() {
        let mut world = World::new();
        world.import_block(0, 66, 0, 9 << 4).unwrap();
        let mut hazard = Hazards::new();
        let mut damage = 0.0;
        for t in 1..=320 {
            damage += hazard.tick(&world, PlayerPosition::default(), t, &Inventory::default());
        }
        assert!(damage >= 2.0);
        let mut under_water = Hazards::new();
        let mut cause = HazardAttribution::None;
        for t in 1..=320 {
            let (points, source) = under_water.tick_detailed(
                &world,
                PlayerPosition::default(),
                t,
                &Inventory::default(),
            );
            if points > 0.0 {
                cause = source;
            }
        }
        assert_eq!(cause, HazardAttribution::Single(HazardCause::Drowning));
    }
}
