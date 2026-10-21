//! Server-owned, bounded survival environmental damage for Protocol 5.
use crate::inventory::Inventory;
use crate::player::PlayerPosition;
use crate::world::World;

#[derive(Debug, Default)]
pub struct Hazards {
    air: u16,
    wet_ticks: u16,
    fire_ticks: u16,
    last_fire_hit: u64,
    last_suffocation_hit: u64,
}
impl Hazards {
    pub fn new() -> Self { Self { air: 300, ..Self::default() } }
    /// Compute damage for ONE 20Hz tick. Raw damage is reduced for contact/heat,
    /// but drowning and suffocation bypass armour in vanilla.
    pub fn tick(&mut self, world: &World, pos: PlayerPosition, tick: u64, inventory: &Inventory) -> f32 {
        let (x,y,z)=(pos.x.floor() as i32,pos.y.floor() as i32,pos.z.floor() as i32);
        let feet=world.get(x,y,z)>>4;
        let head=world.get(x,(pos.y+1.62).floor() as i32,z)>>4;
        let submerged = matches!(head,8|9);
        let mut raw = 0.0_f32;
        let mut bypass = 0.0_f32;
        if submerged {
            self.air = self.air.saturating_sub(1);
            self.wet_ticks = self.wet_ticks.saturating_add(1);
            if self.air == 0 && self.wet_ticks % 20 == 0 { bypass += 2.0; }
        } else {
            self.air = 300;
            self.wet_ticks = 0;
        }
        if matches!(head, 1|2|3|4|5|7|12|13|14|15|16|17|20|24|35|41|42|45|49|56|57|73|74|80|82|87|89|98|129|155|159|162|168|169|170|172|173)
            && tick.saturating_sub(self.last_suffocation_hit) >= 10 {
            self.last_suffocation_hit = tick;
            bypass += 1.0;
        }
        if matches!(feet,10|11) {
            self.fire_ticks = self.fire_ticks.max(100);
            if tick.saturating_sub(self.last_fire_hit) >= 10 {
                self.last_fire_hit = tick;
                raw += 4.0;
            }
        } else if feet == 51 {
            self.fire_ticks = self.fire_ticks.max(80);
        }
        if self.fire_ticks > 0 && !submerged {
            self.fire_ticks -= 1;
            if !matches!(feet,10|11) && tick.saturating_sub(self.last_fire_hit) >= 20 {
                self.last_fire_hit = tick;
                raw += 1.0;
            }
        } else if submerged {
            self.fire_ticks = 0;
        }
        let points=inventory.armor_points().min(20) as f32;
        (raw*(25.0-points)/25.0).ceil()+bypass
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn drowned_player_loses_air_before_health() {
        let mut world=World::new();
        world.import_block(0,65,0,9<<4).unwrap();
        let mut hazard=Hazards::new();
        let mut damage=0.0;
        for t in 1..=320 {
            damage+=hazard.tick(&world,PlayerPosition::default(),t,&Inventory::default());
        }
        assert!(damage>=2.0);
    }
}
