//! Group C: deterministic, server-side spawn eligibility rules.
//! Pure rules are separated from world sampling so they can be tested without networking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnCategory { Hostile, Passive, Ambient, Water }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Difficulty { Peaceful, Easy, Normal, Hard }

#[derive(Clone, Copy, Debug)]
pub struct SpawnContext {
    pub category: SpawnCategory,
    pub difficulty: Difficulty,
    pub sky_light: u8,
    pub block_light: u8,
    pub is_thundering: bool,
    pub distance_to_nearest_player: f64,
    pub distance_to_world_spawn: f64,
    pub eligible_chunks: usize,
    pub current_category_count: usize,
    pub solid_floor: bool,
    pub free_space: bool,
    pub biome_allows_kind: bool,
    pub grass_floor: bool,
}

pub fn category_cap(category: SpawnCategory, eligible_chunks: usize) -> usize {
    let per_289 = match category {
        SpawnCategory::Hostile => 70,
        SpawnCategory::Passive => 10,
        SpawnCategory::Ambient => 15,
        SpawnCategory::Water => 5,
    };
    per_289 * eligible_chunks / 289
}

/// Reject invalid positions before creating entity IDs or sending spawn packets.
/// This does not replace pack selection, biome sampling or group spawning.
pub fn eligible(ctx: SpawnContext) -> bool {
    if !ctx.free_space || !ctx.solid_floor || !ctx.biome_allows_kind
        || !ctx.distance_to_nearest_player.is_finite()
        || !ctx.distance_to_world_spawn.is_finite()
        || ctx.distance_to_nearest_player < 24.0
        || ctx.distance_to_nearest_player > 128.0
        || ctx.distance_to_world_spawn < 24.0
        || ctx.current_category_count >= category_cap(ctx.category, ctx.eligible_chunks)
    { return false; }
    match ctx.category {
        SpawnCategory::Hostile => {
            ctx.difficulty != Difficulty::Peaceful
                && ctx.block_light <= 7
                && ctx.sky_light <= if ctx.is_thundering { 10 } else { 7 }
        }
        SpawnCategory::Passive => ctx.grass_floor && ctx.block_light.max(ctx.sky_light) >= 9,
        SpawnCategory::Ambient | SpawnCategory::Water => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context() -> SpawnContext {
        SpawnContext {
            category: SpawnCategory::Hostile, difficulty: Difficulty::Normal,
            sky_light: 0, block_light: 0, is_thundering: false,
            distance_to_nearest_player: 32.0, distance_to_world_spawn: 50.0,
            eligible_chunks: 289, current_category_count: 0,
            solid_floor: true, free_space: true, biome_allows_kind: true,
            grass_floor: false,
        }
    }
    #[test] fn hostile_rejects_peaceful_and_bright_positions() {
        let mut c=context(); assert!(eligible(c));
        c.difficulty=Difficulty::Peaceful; assert!(!eligible(c));
        c.difficulty=Difficulty::Normal; c.block_light=8; assert!(!eligible(c));
        c.block_light=0; c.sky_light=8; assert!(!eligible(c));
    }
    #[test] fn distance_and_cap_are_enforced() {
        let mut c=context();
        c.distance_to_nearest_player=23.9; assert!(!eligible(c));
        c.distance_to_nearest_player=32.0;
        c.current_category_count=70; assert!(!eligible(c));
        c.current_category_count=0; c.distance_to_world_spawn=10.0;
        assert!(!eligible(c));
    }
    #[test] fn passive_needs_grass_and_light() {
        let mut c=context(); c.category=SpawnCategory::Passive;
        assert!(!eligible(c)); c.grass_floor=true; c.sky_light=15;
        assert!(eligible(c)); c.current_category_count=10;
        assert!(!eligible(c));
    }
    #[test] fn caps_scale_by_eligible_chunks() {
        assert_eq!(category_cap(SpawnCategory::Hostile,289),70);
        assert_eq!(category_cap(SpawnCategory::Passive,289),10);
        assert_eq!(category_cap(SpawnCategory::Hostile,0),0);
    }
}
