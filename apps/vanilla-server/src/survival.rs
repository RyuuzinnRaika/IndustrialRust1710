//! Bounded Java Edition 1.7.10 survival mining subset.
//! The server owns timings, harvest tiers, drops and unenchanted tool wear.
//! Not a full replacement for all vanilla block/item metadata, enchantments,
//! underwater penalties, potions, Silk Touch, Fortune or multi-item drops.
use crate::inventory::Stack;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockDrop {
    pub id: i16,
    pub damage: i16,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MineResult {
    /// Missing when the tool cannot harvest an otherwise breakable block.
    pub drop: Option<BlockDrop>,
    /// Only applies on an actual committed Survival block removal.
    pub wear_tool: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolKind {
    Pickaxe,
    Shovel,
    Axe,
    Sword,
    Hoe,
}
#[derive(Clone, Copy, Debug)]
struct Tool {
    kind: ToolKind,
    tier: u8,
    speed: f64,
    max_uses: i16,
}
fn tool(stack: Option<Stack>) -> Option<Tool> {
    let held = stack?;
    let (kind, tier, speed, max_uses) = match held.id {
        270 => (ToolKind::Pickaxe, 0, 2.0, 59),
        274 => (ToolKind::Pickaxe, 1, 4.0, 131),
        257 => (ToolKind::Pickaxe, 2, 6.0, 250),
        278 => (ToolKind::Pickaxe, 3, 8.0, 1561),
        285 => (ToolKind::Pickaxe, 0, 12.0, 32),
        269 => (ToolKind::Shovel, 0, 2.0, 59),
        273 => (ToolKind::Shovel, 1, 4.0, 131),
        256 => (ToolKind::Shovel, 2, 6.0, 250),
        277 => (ToolKind::Shovel, 3, 8.0, 1561),
        284 => (ToolKind::Shovel, 0, 12.0, 32),
        271 => (ToolKind::Axe, 0, 2.0, 59),
        275 => (ToolKind::Axe, 1, 4.0, 131),
        258 => (ToolKind::Axe, 2, 6.0, 250),
        279 => (ToolKind::Axe, 3, 8.0, 1561),
        286 => (ToolKind::Axe, 0, 12.0, 32),
        268 => (ToolKind::Sword, 0, 2.0, 59),
        272 => (ToolKind::Sword, 1, 4.0, 131),
        267 => (ToolKind::Sword, 2, 6.0, 250),
        276 => (ToolKind::Sword, 3, 8.0, 1561),
        283 => (ToolKind::Sword, 0, 12.0, 32),
        290 => (ToolKind::Hoe, 0, 2.0, 59),
        291 => (ToolKind::Hoe, 1, 4.0, 131),
        292 => (ToolKind::Hoe, 2, 6.0, 250),
        293 => (ToolKind::Hoe, 3, 8.0, 1561),
        294 => (ToolKind::Hoe, 0, 12.0, 32),
        _ => return None,
    };
    // Damaged tool metadata must represent an existing, unbroken item.
    if held.count != 1 || held.damage < 0 || held.damage >= max_uses {
        return None;
    }
    Some(Tool {
        kind,
        tier,
        speed,
        max_uses,
    })
}
/// 1.7.10-style damage count and break thresholds for basic tools.
pub fn tool_max_uses(stack: Stack) -> Option<i16> {
    // Shears have 238 uses in Java 1.7.10 but are not mining tools.
    if stack.id == 359 && stack.count == 1 && (0..238).contains(&stack.damage) {
        return Some(238);
    }
    tool(Some(stack)).map(|value| value.max_uses)
}
#[derive(Clone, Copy)]
struct BlockRule {
    hardness: f64,
    kind: Option<ToolKind>,
    tier: u8,
    requires_tool: bool,
    drop: Option<BlockDrop>,
}
fn rule(block: u16) -> Option<BlockRule> {
    let id = block >> 4;
    let damage = (block & 15) as i16;
    let item = |id: i16, damage: i16| Some(BlockDrop { id, damage });
    let (hardness, kind, tier, requires_tool, drop) = match id {
        // Common 1.7.10 wild plants must be breakable even without tools.
        // Tall grass / ferns are removed with no guaranteed block item:
        // optional wheat seeds are decided in additional_drop().
        31 | 32 | 175 => (0.0, None, 0, false, None), // grass, dead bush, tall plants
        37 => (0.0, None, 0, false, item(37, 0)),     // dandelion
        38 => (0.0, None, 0, false, item(38, damage)), // flower variants
        39 | 40 => (0.0, None, 0, false, item(id as i16, 0)),
        81 => (0.4, None, 0, false, item(81, 0)),  // cactus
        83 => (0.0, None, 0, false, item(338, 0)), // sugar cane item
        86 | 91 => (1.0, Some(ToolKind::Axe), 0, false, item(id as i16, 0)),
        111 => (0.0, None, 0, false, item(111, 0)), // lily pad
        106 => (0.2, None, 0, false, None),         // vines: shears-only drop
        78 => (0.1, Some(ToolKind::Shovel), 0, false, None), // snow layer
        79 => (0.5, Some(ToolKind::Pickaxe), 0, false, None), // ice melts
        110 => (0.6, Some(ToolKind::Shovel), 0, false, item(3, 0)), // mycelium
        2 | 3 => (0.5, Some(ToolKind::Shovel), 0, false, item(3, 0)),
        12 => (0.5, Some(ToolKind::Shovel), 0, false, item(12, damage)),
        13 => (0.6, Some(ToolKind::Shovel), 0, false, item(13, 0)),
        80 => (0.2, Some(ToolKind::Shovel), 0, false, item(332, 0)),
        82 => (0.6, Some(ToolKind::Shovel), 0, false, item(337, 0)),
        1 => (1.5, Some(ToolKind::Pickaxe), 0, true, item(4, 0)),
        4 | 48 => (2.0, Some(ToolKind::Pickaxe), 0, true, item(id as i16, 0)),
        24 => (0.8, Some(ToolKind::Pickaxe), 0, true, item(24, damage)),
        14 => (3.0, Some(ToolKind::Pickaxe), 2, true, item(14, 0)),
        15 => (3.0, Some(ToolKind::Pickaxe), 1, true, item(15, 0)),
        16 => (3.0, Some(ToolKind::Pickaxe), 0, true, item(263, 0)),
        21 => (3.0, Some(ToolKind::Pickaxe), 1, true, item(351, 4)),
        49 => (50.0, Some(ToolKind::Pickaxe), 3, true, item(49, 0)),
        56 => (3.0, Some(ToolKind::Pickaxe), 2, true, item(264, 0)),
        73 | 74 => (3.0, Some(ToolKind::Pickaxe), 2, true, item(331, 0)),
        129 => (3.0, Some(ToolKind::Pickaxe), 2, true, item(388, 0)),
        41 | 42 | 57 | 173 => (
            5.0,
            Some(ToolKind::Pickaxe),
            0,
            true,
            item(id as i16, damage),
        ),
        45 | 98 | 155 | 159 | 168 | 172 => (
            1.5,
            Some(ToolKind::Pickaxe),
            0,
            true,
            item(id as i16, damage),
        ),
        6 => (0.0, None, 0, false, item(6, damage & 7)),
        18 | 161 => (0.2, None, 0, false, None), // leaf drops handled separately
        59 => (
            0.0,
            None,
            0,
            false,
            item(if damage == 7 { 296 } else { 295 }, 0),
        ),
        60 => (0.6, Some(ToolKind::Shovel), 0, false, item(3, 0)),
        141 => (0.0, None, 0, false, item(391, 0)),
        142 => (0.0, None, 0, false, item(392, 0)),
        66 => (0.7, Some(ToolKind::Pickaxe), 0, false, item(66, 0)),
        70 | 72 => (0.5, Some(ToolKind::Pickaxe), 0, false, item(id as i16, 0)),
        93 | 94 => (0.0, None, 0, false, item(356, 0)),
        50 => (0.0, None, 0, false, item(50, 0)),  // torch
        55 => (0.0, None, 0, false, item(331, 0)), // redstone dust
        69 => (0.5, None, 0, false, item(69, 0)),  // lever
        76 => (0.0, None, 0, false, item(76, 0)),  // redstone torch
        5 => (2.0, Some(ToolKind::Axe), 0, false, item(5, damage)),
        // The upper two log metadata bits encode the axis, not the item species.
        // Re-mining a sideways log must not mint an orientation-specific item.
        17 => (2.0, Some(ToolKind::Axe), 0, false, item(17, damage & 3)),
        162 => (2.0, Some(ToolKind::Axe), 0, false, item(162, damage & 1)),
        // Standalone early-game building materials. Their orientation/top-half
        // bits belong to the placed block and must not leak into the drop.
        53 | 134 | 135 | 136 | 163 | 164 => {
            (2.0, Some(ToolKind::Axe), 0, false, item(id as i16, 0))
        } // oak/spruce/birch/jungle/acacia/dark oak stairs
        67 => (2.0, Some(ToolKind::Pickaxe), 0, true, item(67, 0)), // cobblestone stairs
        65 => (0.4, Some(ToolKind::Axe), 0, false, item(65, 0)),    // ladder
        85 | 107 => (2.0, Some(ToolKind::Axe), 0, false, item(id as i16, 0)), // fence/gate
        96 => (3.0, Some(ToolKind::Axe), 0, false, item(96, 0)),    // trapdoor
        126 if damage & 7 <= 5 => (2.0, Some(ToolKind::Axe), 0, false, item(126, damage & 7)), // half wooden slab (species retained, upper-half flag stripped)
        44 => (2.0, Some(ToolKind::Pickaxe), 0, true, item(44, damage & 7)),
        54 => (2.5, Some(ToolKind::Axe), 0, false, item(54, 0)),
        58 => (2.5, Some(ToolKind::Axe), 0, false, item(58, 0)),
        25 => (0.8, Some(ToolKind::Axe), 0, false, item(25, 0)), // note block
        61 | 62 => (3.5, Some(ToolKind::Pickaxe), 0, true, item(61, 0)),
        20 => (0.3, None, 0, false, None), // glass: breaks but no drop
        35 => (0.8, None, 0, false, item(35, damage)),
        87 => (0.4, Some(ToolKind::Pickaxe), 0, false, item(87, 0)),
        88 => (0.5, Some(ToolKind::Shovel), 0, false, item(88, 0)),
        153 => (3.0, Some(ToolKind::Pickaxe), 0, true, item(153, 0)),
        89 => (0.3, None, 0, false, item(348, 0)),
        170 => (0.5, Some(ToolKind::Axe), 0, false, item(170, 0)),
        // Previously absent 1.7.10 blocks were treated as unbreakable.
        // Keep fluid/portal/bedrock exclusions below, and don't invent
        // multi-item or Silk Touch drops when those mechanics are unavailable.
        19 => (0.6, None, 0, false, item(19, 0)), // sponge
        22 => (3.0, Some(ToolKind::Pickaxe), 1, true, item(22, 0)), // lapis block
        23 | 158 => (3.5, Some(ToolKind::Pickaxe), 0, true, item(id as i16, 0)), // dispenser/dropper
        29 | 33 => (0.5, Some(ToolKind::Pickaxe), 0, false, item(id as i16, 0)), // pistons
        46 => (0.0, None, 0, false, item(46, 0)), // TNT is instant-break
        47 => (1.5, Some(ToolKind::Axe), 0, false, None), // bookshelf: 3 books from additional_drop
        // A bed/door can be mined from either half; the upper/head half must
        // not drop a second item when its companion is already absent.
        26 => (
            0.2,
            None,
            0,
            false,
            if damage & 8 == 0 { item(355, 0) } else { None },
        ),
        64 => (
            3.0,
            Some(ToolKind::Axe),
            0,
            false,
            if damage & 8 == 0 { item(324, 0) } else { None },
        ),
        71 => (
            5.0,
            Some(ToolKind::Pickaxe),
            0,
            true,
            if damage & 8 == 0 { item(330, 0) } else { None },
        ),
        63 | 68 => (1.0, Some(ToolKind::Axe), 0, false, item(323, 0)), // signs
        84 => (2.0, Some(ToolKind::Axe), 0, false, item(84, 0)),       // jukebox
        116 => (5.0, Some(ToolKind::Pickaxe), 0, false, item(116, 0)),
        117 => (0.5, Some(ToolKind::Pickaxe), 0, false, item(379, 0)), // brewing stand
        140 => (0.0, None, 0, false, item(390, 0)),                    // flower pot
        145 => (
            5.0,
            Some(ToolKind::Pickaxe),
            1,
            true,
            item(145, damage >> 2),
        ),
        95 | 160 => (0.3, None, 0, false, None), // stained glass/panes
        101 => (5.0, Some(ToolKind::Pickaxe), 0, true, item(101, 0)), // iron bars
        102 => (0.3, None, 0, false, None),      // glass pane
        112 | 113 => (2.0, Some(ToolKind::Pickaxe), 0, true, item(id as i16, 0)),
        121 => (3.0, Some(ToolKind::Pickaxe), 0, true, item(121, 0)), // end stone
        123 | 124 => (0.3, Some(ToolKind::Pickaxe), 0, false, item(123, 0)),
        128 | 156 => (0.8, Some(ToolKind::Pickaxe), 0, true, item(id as i16, 0)),
        133 => (5.0, Some(ToolKind::Pickaxe), 2, true, item(133, 0)),
        139 => (2.0, Some(ToolKind::Pickaxe), 0, true, item(139, damage)),
        146 => (2.5, Some(ToolKind::Axe), 0, false, item(146, 0)), // trapped chest
        147 | 148 => (0.5, Some(ToolKind::Pickaxe), 0, false, item(id as i16, 0)),
        152 => (5.0, Some(ToolKind::Pickaxe), 0, true, item(152, 0)),
        154 => (3.0, Some(ToolKind::Pickaxe), 0, true, item(154, 0)), // hopper
        157 => (0.7, Some(ToolKind::Pickaxe), 0, false, item(157, 0)),
        171 => (0.1, None, 0, false, item(171, damage)),
        174 => (0.5, Some(ToolKind::Pickaxe), 0, false, None), // packed ice
        _ => return None,
    };
    Some(BlockRule {
        hardness,
        kind,
        tier,
        requires_tool,
        drop,
    })
}
/// Deterministic, bounded bonus drops for manual harvesting. The primary
/// BlockDrop remains compatible with existing mining/inventory accounting.
pub fn additional_drop(block: u16, pos: (i32, i32, i32), tick: u64) -> Option<Stack> {
    let (x, y, z) = pos;
    let mut roll = tick
        ^ (x as i64 as u64).wrapping_mul(0x9e3779b185ebca87)
        ^ (y as i64 as u64).wrapping_mul(0xc2b2ae3d27d4eb4f)
        ^ (z as i64 as u64).wrapping_mul(0x165667b19e3779f9);
    roll ^= roll >> 30;
    roll = roll.wrapping_mul(0xbf58476d1ce4e5b9);
    roll ^= roll >> 27;
    roll = roll.wrapping_mul(0x94d049bb133111eb);
    roll ^= roll >> 31;
    let (id, count) = match block >> 4 {
        31 if (block & 3) != 0 && roll.is_multiple_of(8) => (295, 1), // Grass/fern seeds
        47 => (340, 3), // Bookshelf always drops three books.
        18 if block & 3 == 0 && roll.is_multiple_of(200) => (260, 1), // Oak apple
        161 if block & 3 == 1 && roll.is_multiple_of(200) => (260, 1), // Dark oak apple
        18 | 161 if roll.is_multiple_of(20) => (6, 1), // Matching sapling
        59 if block & 7 == 7 => (295, 1 + (roll % 3) as u8),
        141 if block & 7 == 7 => (391, 1 + (roll % 3) as u8),
        142 if block & 7 == 7 => (392, 1 + (roll % 3) as u8),
        _ => return None,
    };
    Some(Stack {
        id,
        count,
        damage: if id == 6 {
            if block >> 4 == 161 {
                4 + (block & 1) as i16 // Acacia / dark oak.
            } else {
                (block & 3) as i16
            }
        } else {
            0
        },
        nbt: None,
    })
}

/// Server-authoritative minimum for breaking a known block; tick quantized,
/// 30 game ticks × hardness / speed if harvestable, otherwise 100 × hardness.
/// Not all environmental penalties of vanilla 1.7.10 are modeled.
pub fn mine_rule(block: u16, held: Option<Stack>) -> Option<(Duration, MineResult)> {
    let spec = rule(block)?;
    let equipped = tool(held);
    let effective = equipped.is_some_and(|t| spec.kind == Some(t.kind));
    let can_harvest =
        !spec.requires_tool || (effective && equipped.is_some_and(|t| t.tier >= spec.tier));
    let speed = if effective {
        equipped.map_or(1.0, |t| t.speed)
    } else {
        1.0
    };
    // Instant-break foliage must accept a C07 Start/Finish pair sent in
    // the same client tick; enforcing one 50ms tick made grass and flowers
    // intermittently reappear when the finish arrived immediately.
    let ticks = if spec.hardness == 0.0 {
        0
    } else {
        ((spec.hardness * if can_harvest { 30.0 } else { 100.0 }) / speed)
            .ceil()
            .max(1.0) as u64
    };
    let drop = if can_harvest { spec.drop } else { None };
    Some((
        Duration::from_millis(ticks.saturating_mul(50)),
        MineResult {
            drop,
            // Since Java 1.3.1, breaking a block which is instant by
            // bare hand must not consume tool durability, even if held.
            wear_tool: equipped.is_some() && spec.hardness > 0.0,
        },
    ))
}
#[derive(Clone, Copy, Debug)]
pub struct MiningProgress {
    target: (i32, i32, i32),
    started: Instant,
    block: u16,
    tool: Option<Stack>,
    /// Combined classic underwater/head and airborne mining penalty.
    penalty: u32,
}
impl MiningProgress {
    /// Normal grounded/dry entry point for earlier unit test callers.
    #[cfg(test)]
    pub fn start(target: (i32, i32, i32), block: u16, held: Option<Stack>) -> Option<Self> {
        Self::start_with_environment(target, block, held, false, true)
    }

    pub fn start_with_environment(
        target: (i32, i32, i32),
        block: u16,
        held: Option<Stack>,
        head_submerged: bool,
        on_ground: bool,
    ) -> Option<Self> {
        mine_rule(block, held)?;
        Some(Self {
            target,
            started: Instant::now(),
            block,
            tool: held,
            penalty: environment_penalty(head_submerged, on_ground),
        })
    }
    /// A Protocol 5 client sends only Start for blocks destroyed in its
    /// first mining tick. The shared finish path must commit these immediately.
    pub fn is_instant(&self) -> bool {
        mine_rule(self.block, self.tool).is_some_and(|(duration, _)| {
            duration.saturating_mul(self.penalty) <= Duration::from_millis(50)
        })
    }

    /// Consume the attempt exactly once. The held tool must remain unchanged
    /// between starting and finishing a dig, preventing fast-tool switching.
    #[cfg(test)]
    pub fn finish(
        self,
        target: (i32, i32, i32),
        current: u16,
        held: Option<Stack>,
    ) -> Option<MineResult> {
        if self.target != target || self.block != current || self.tool != held {
            return None;
        }
        self.finish_with_environment(target, current, held, false, true)
    }

    /// Apply the worse of the start/finish penalties, so entering water or
    /// jumping immediately before finishing never bypasses the server timer.
    pub fn finish_with_environment(
        self,
        target: (i32, i32, i32),
        current: u16,
        held: Option<Stack>,
        head_submerged: bool,
        on_ground: bool,
    ) -> Option<MineResult> {
        if self.target != target || self.block != current || self.tool != held {
            return None;
        }
        let (minimum, result) = mine_rule(current, held)?;
        let penalty = self
            .penalty
            .max(environment_penalty(head_submerged, on_ground));
        // Dig progress is reported in client ticks but start/finish packets
        // are processed on separate server polls. Allow one 20TPS tick of
        // quantization slack, not an arbitrary fast-mine bypass.
        (self
            .started
            .elapsed()
            .saturating_add(Duration::from_millis(50))
            >= minimum.saturating_mul(penalty))
        .then_some(result)
    }
}

fn environment_penalty(head_submerged: bool, on_ground: bool) -> u32 {
    let water = if head_submerged { 5 } else { 1 };
    let airborne = if on_ground { 1 } else { 5 };
    water * airborne
}
#[cfg(test)]
mod tests {
    use super::*;
    fn stack(id: i16, damage: i16) -> Option<Stack> {
        Some(Stack {
            id,
            damage,
            count: 1,
            nbt: None,
        })
    }
    #[test]
    fn s03_all_five_tool_materials_have_legacy_speed_tier_and_durability() {
        // Material, pickaxe, shovel, axe, sword, hoe, tier, speed, durability.
        // Gold's speed is highest, but its harvest tier is only wooden.
        for (material, pick, shovel, axe, sword, hoe, tier, speed, uses) in [
            ("wood", 270, 269, 271, 268, 290, 0, 2.0, 59),
            ("stone", 274, 273, 275, 272, 291, 1, 4.0, 131),
            ("iron", 257, 256, 258, 267, 292, 2, 6.0, 250),
            ("gold", 285, 284, 286, 283, 294, 0, 12.0, 32),
            ("diamond", 278, 277, 279, 276, 293, 3, 8.0, 1561),
        ] {
            for id in [pick, shovel, axe, sword, hoe] {
                let item = Stack {
                    id,
                    count: 1,
                    damage: 0,
                    nbt: None,
                };
                let stats = tool(Some(item)).expect("all 25 tool IDs");
                assert_eq!(stats.tier, tier, "{material} id={id}");
                assert_eq!(stats.max_uses, uses, "{material} id={id}");
                assert_eq!(stats.speed, speed, "{material} id={id}");
                assert_eq!(tool_max_uses(item), Some(uses));
                assert_eq!(
                    tool_max_uses(Stack {
                        damage: uses - 1,
                        ..item
                    }),
                    Some(uses),
                    "last valid use: {material} id={id}"
                );
                assert_eq!(tool_max_uses(Stack { damage: uses, ..item }), None);
            }
            // The real server harvest rule must check material tier before
            // giving an ore drop, not just the pickaxe's faster mining time.
            for (ore, required, drop) in [
                (1u16, 0, 4), // stone -> cobblestone
                (16, 0, 263), // coal ore -> coal
                (15, 1, 15), // iron ore -> itself
                (14, 2, 14), // gold ore -> itself
                (56, 2, 264), // diamond ore -> diamond
                (49, 3, 49), // obsidian -> itself
            ] {
                let (_, mined) = mine_rule(ore << 4, stack(pick, 0)).unwrap();
                assert_eq!(
                    mined.drop.map(|item| item.id),
                    (tier >= required).then_some(drop),
                    "{material} pickaxe vs block {ore}"
                );
            }
        }
    }

    #[test]
    fn s03_golden_pickaxe_mines_stone_fast_but_cannot_harvest_iron_ore() {
        let wood = mine_rule(1 << 4, stack(270, 0)).unwrap().0;
        let stone = mine_rule(1 << 4, stack(274, 0)).unwrap().0;
        let iron = mine_rule(1 << 4, stack(257, 0)).unwrap().0;
        let diamond = mine_rule(1 << 4, stack(278, 0)).unwrap().0;
        let gold = mine_rule(1 << 4, stack(285, 0)).unwrap().0;
        assert!(gold < diamond && diamond < iron && iron < stone && stone < wood);

        let (gold_iron_time, gold_iron) = mine_rule(15 << 4, stack(285, 0)).unwrap();
        let (wood_iron_time, wood_iron) = mine_rule(15 << 4, stack(270, 0)).unwrap();
        assert_eq!(gold_iron.drop, None);
        assert_eq!(wood_iron.drop, None);
        // Both drop nothing, but the golden tool still removes the block
        // faster: high speed does not imply a higher harvest tier.
        assert!(gold_iron_time < wood_iron_time);
    }

    #[test]
    fn s03_instant_hand_breaks_do_not_damage_held_iron_tools() {
        let iron_pickaxe = stack(257, 0);
        let iron_sword = stack(267, 0);
        // Grass, dandelions, crops and TNT are all hardness-zero blocks.
        // A mining tool must not lose durability from instant hand breaks.
        for block in [31u16, 37, 38, 59, 141, 142, 46] {
            for held in [iron_pickaxe, iron_sword] {
                let (duration, result) = mine_rule(block << 4, held).unwrap();
                assert_eq!(duration, Duration::ZERO, "block {block}");
                assert!(!result.wear_tool, "block {block}, held {held:?}");
            }
        }
        // Genuine stone and log breaking still consumes tool durability.
        assert!(mine_rule(1 << 4, iron_pickaxe).unwrap().1.wear_tool);
        assert!(mine_rule(17 << 4, iron_sword).unwrap().1.wear_tool);
        assert!(!mine_rule(1 << 4, None).unwrap().1.wear_tool);
    }

    #[test]
    fn s01_log_orientation_is_not_an_inventory_item_variant() {
        // Pre-flattening log metadata has species in its low bits and axis in
        // its upper bits. Item metadata only carries the wood species.
        for (block, species_count) in [(17u16, 4u16), (162u16, 2u16)] {
            for species in 0..species_count {
                for axis in [0u16, 4, 8, 12] {
                    let (_, mined) = mine_rule((block << 4) | species | axis, None).unwrap();
                    assert_eq!(
                        mined.drop,
                        Some(BlockDrop {
                            id: block as i16,
                            damage: species as i16,
                        }),
                        "log block {block}, species {species}, axis {axis}"
                    );
                    assert!(!mined.wear_tool);
                }
            }
        }
    }

    #[test]
    fn s05_basic_shelter_blocks_have_legacy_drops_and_tool_requirements() {
        // A stair's orientation and upside-down bit are block state, not item NBT.
        for block in [53u16, 134, 135, 136, 163, 164, 65, 85, 107, 96] {
            for facing in [0u16, 4, 8] {
                let (bare_time, mined) = mine_rule((block << 4) | facing, None).unwrap();
                assert_eq!(
                    mined.drop,
                    Some(BlockDrop {
                        id: block as i16,
                        damage: 0,
                    }),
                    "expected one matching, orientation-free item for {block}"
                );
                assert!(!mined.wear_tool);
                assert!(bare_time >= Duration::from_millis(50));
            }
        }
        for kind in 0..=5u16 {
            for top in [0u16, 8] {
                let (_, mined) = mine_rule((126 << 4) | top | kind, None).unwrap();
                assert_eq!(
                    mined.drop,
                    Some(BlockDrop {
                        id: 126,
                        damage: kind as i16,
                    })
                );
            }
        }
        for kind in 0..=7u16 {
            for top in [0u16, 8] {
                let block = (44 << 4) | top | kind;
                assert_eq!(mine_rule(block, None).unwrap().1.drop, None);
                assert_eq!(
                    mine_rule(block, stack(270, 0)).unwrap().1.drop,
                    Some(BlockDrop {
                        id: 44,
                        damage: kind as i16,
                    })
                );
            }
        }
        assert_eq!(mine_rule(67 << 4, None).unwrap().1.drop, None);
        assert_eq!(
            mine_rule((67 << 4) | 4, stack(270, 0)).unwrap().1.drop,
            Some(BlockDrop { id: 67, damage: 0 })
        );
        // Multiblock doors/beds are deliberately not changed by this patch:
        // they require the other half and drop to be transacted together.
        assert_eq!(mine_rule(64 << 4, None).unwrap().1.drop.unwrap().id, 324);
        assert_eq!(mine_rule(64 << 4 | 8, None).unwrap().1.drop, None);
        assert_eq!(mine_rule(71 << 4, None).unwrap().1.drop, None);
        assert_eq!(
            mine_rule(71 << 4, stack(270, 0))
                .unwrap()
                .1
                .drop
                .unwrap()
                .id,
            330
        );
        assert_eq!(mine_rule(26 << 4, None).unwrap().1.drop.unwrap().id, 355);
        assert_eq!(mine_rule(26 << 4 | 8, None).unwrap().1.drop, None);
        assert!(mine_rule(7 << 4, None).is_none());
    }

    #[test]
    fn s01_to_s04_minimum_resource_chain_matches_1710_recipe_primitives() {
        let st = |id| {
            Some(Stack {
                id,
                count: 1,
                damage: 0,
                nbt: None,
            })
        };
        let wood = mine_rule(17 << 4, None).unwrap().1.drop.unwrap();
        let planks = crate::crafting::result(&[st(wood.id), None, None, None], 2).unwrap();
        assert_eq!((planks.id, planks.count), (5, 4));
        let sticks = crate::crafting::result(&[st(5), None, st(5), None], 2).unwrap();
        assert_eq!((sticks.id, sticks.count), (280, 4));
        let workbench = crate::crafting::result(&[st(5); 4], 2).unwrap();
        assert_eq!(workbench.id, 58);
        let wooden_pick = crate::crafting::result(
            &[
                st(5),
                st(5),
                st(5),
                None,
                st(280),
                None,
                None,
                st(280),
                None,
            ],
            3,
        )
        .unwrap();
        assert_eq!(wooden_pick.id, 270);
        let cobble = mine_rule(1 << 4, Some(wooden_pick))
            .unwrap()
            .1
            .drop
            .unwrap();
        assert_eq!(cobble.id, 4);
        let stone_pick = crate::crafting::result(
            &[
                st(4),
                st(4),
                st(4),
                None,
                st(280),
                None,
                None,
                st(280),
                None,
            ],
            3,
        )
        .unwrap();
        assert_eq!(stone_pick.id, 274);
        assert_eq!(mine_rule(15 << 4, Some(wooden_pick)).unwrap().1.drop, None);
        let iron_ore = mine_rule(15 << 4, Some(stone_pick))
            .unwrap()
            .1
            .drop
            .unwrap();
        assert_eq!(iron_ore.id, 15);
        let furnace = crate::crafting::result(
            &[st(4), st(4), st(4), st(4), None, st(4), st(4), st(4), st(4)],
            3,
        )
        .unwrap();
        assert_eq!(furnace.id, 61);
        assert_eq!(
            crate::furnaces::recipe(st(iron_ore.id).unwrap())
                .unwrap()
                .id,
            265
        );
        // This is a rules-only smoke test, not proof of live item transfers,
        // inventory debits, client packets, furnace ticks, or persistence.
    }

    #[test]
    fn wild_plants_are_breakable_and_return_legacy_survival_items() {
        // Block IDs on the left; item IDs/metadata on the right.
        for (block, metadata, item_id, item_metadata) in [
            (37, 0, 37, 0),   // dandelion
            (38, 4, 38, 4),   // tulip/flower variant
            (39, 0, 39, 0),   // brown mushroom
            (40, 0, 40, 0),   // red mushroom
            (81, 0, 81, 0),   // cactus
            (83, 0, 338, 0),  // sugar cane becomes the cane item
            (86, 0, 86, 0),   // pumpkin
            (91, 0, 91, 0),   // jack-o-lantern
            (111, 0, 111, 0), // lily pad
        ] {
            let (_, mined) = mine_rule((block << 4) | metadata, None).unwrap();
            assert_eq!(
                mined.drop,
                Some(BlockDrop {
                    id: item_id,
                    damage: item_metadata,
                }),
                "unexpected plant drop for block {block}"
            );
            assert!(!mined.wear_tool);
        }
        for variant in [1u16, 2] {
            let (time, mined) = mine_rule((31 << 4) | variant, None).unwrap();
            // Zero-hardness tall grass and ferns must break in the same client tick.
            assert_eq!(time, Duration::ZERO);
            assert_eq!(mined.drop, None);
            assert!(!mined.wear_tool);
        }
        // Removing grass is never a guaranteed seed grant; outcomes remain
        // deterministic for the same world position and tick.
        let grass = (31u16 << 4) | 1;
        let seeds = (0..128u64)
            .filter_map(|tick| additional_drop(grass, (2, 65, -3), tick))
            .collect::<Vec<_>>();
        assert!(!seeds.is_empty());
        assert!(seeds.len() < 64);
        assert!(seeds.iter().all(|drop| drop.id == 295 && drop.count == 1));
        assert_eq!(
            additional_drop(grass, (2, 65, -3), 7),
            additional_drop(grass, (2, 65, -3), 7)
        );
        assert_eq!(additional_drop(31u16 << 4, (2, 65, -3), 7), None);
    }

    #[test]
    fn base_rules_and_harvest_tiers_are_authoritative() {
        assert!(mine_rule(7 << 4, None).is_none());
        assert!(mine_rule(0, None).is_none());
        assert_eq!(mine_rule(2 << 4, None).unwrap().1.drop.unwrap().id, 3);
        assert_eq!(mine_rule(1 << 4, None).unwrap().1.drop, None);
        assert_eq!(
            mine_rule(1 << 4, stack(270, 0)).unwrap().1.drop.unwrap().id,
            4
        );
        assert_eq!(mine_rule(15 << 4, stack(270, 0)).unwrap().1.drop, None);
        assert_eq!(
            mine_rule(15 << 4, stack(274, 0))
                .unwrap()
                .1
                .drop
                .unwrap()
                .id,
            15
        );
        assert_eq!(mine_rule(14 << 4, stack(274, 0)).unwrap().1.drop, None);
        assert_eq!(
            mine_rule(14 << 4, stack(257, 0))
                .unwrap()
                .1
                .drop
                .unwrap()
                .id,
            14
        );
        assert_eq!(
            mine_rule(56 << 4, stack(257, 0))
                .unwrap()
                .1
                .drop
                .unwrap()
                .id,
            264
        );
        assert_eq!(mine_rule(49 << 4, stack(257, 0)).unwrap().1.drop, None);
        assert_eq!(
            mine_rule(49 << 4, stack(278, 0))
                .unwrap()
                .1
                .drop
                .unwrap()
                .id,
            49
        );
        assert_eq!(mine_rule(20 << 4, None).unwrap().1.drop, None);
        assert!(MiningProgress::start((0, 60, 0), 7 << 4, None).is_none());
    }
    #[test]
    fn new_leaves_are_mineable_and_drop_matching_species() {
        assert!(mine_rule(161 << 4, None).is_some());
        // Deterministically find a non-apple sapling roll to exercise both
        // leaf families without relying on a random distribution.
        for block in [(18 << 4) | 2, (161 << 4), (161 << 4) | 1] {
            let expected = if block >> 4 == 161 {
                4 + (block & 1) as i16
            } else {
                (block & 3) as i16
            };
            let sapling = (0..10_000)
                .find_map(|tick| {
                    let drop = additional_drop(block, (1, 65, 2), tick)?;
                    (drop.id == 6).then_some(drop)
                })
                .expect("each leaf species should drop its matching sapling");
            assert_eq!(sapling.damage, expected);
        }
    }

    #[test]
    fn s03_negative_or_exhausted_pickaxe_damage_cannot_harvest_stone() {
        // Tool damage is signed in the legacy item stack. A negative value
        // must not become a usable pickaxe or bypass the harvest/timing rules.
        let valid = stack(270, 58);
        let (valid_time, valid_mine) = mine_rule(1 << 4, valid).unwrap();
        assert_eq!(tool_max_uses(valid.unwrap()), Some(59));
        assert_eq!(valid_mine.drop, Some(BlockDrop { id: 4, damage: 0 }));
        assert!(valid_mine.wear_tool);

        for damage in [i16::MIN, -1, 59, i16::MAX] {
            let invalid = stack(270, damage);
            assert_eq!(tool_max_uses(invalid.unwrap()), None);
            let (minimum, mined) = mine_rule(1 << 4, invalid).unwrap();
            assert_eq!(mined.drop, None, "invalid damage {damage}");
            assert!(!mined.wear_tool, "invalid damage {damage}");
            assert!(minimum > valid_time, "invalid damage {damage}");
        }
    }

    #[test]
    fn s03_invalid_tool_damage_never_accelerates_mining_or_consumes_durability() {
        // Cover every recognized tool family, not only pickaxes. A malformed
        // signed damage value must behave like an empty hand for mining.
        for (tool_id, block_id) in [
            (270_i16, 1_u16),  // pickaxe / stone
            (269, 3),          // shovel / dirt
            (271, 17),         // axe / log
            (268, 3),          // sword / dirt
            (290, 3),          // hoe / dirt
        ] {
            let bare = mine_rule(block_id << 4, None).unwrap();
            for damage in [i16::MIN, -1] {
                let malformed = stack(tool_id, damage);
                assert_eq!(
                    tool_max_uses(malformed.unwrap()),
                    None,
                    "tool {tool_id}, damage {damage}"
                );
                assert_eq!(
                    mine_rule(block_id << 4, malformed).unwrap(),
                    bare,
                    "tool {tool_id}, damage {damage}"
                );
            }
        }
    }

    #[test]
    fn s03_negative_tool_damage_cannot_fast_finish_or_swap_into_active_dig() {
        // A malformed tool is treated as bare hands throughout the complete
        // server-authoritative dig, not only by the initial harvest rule.
        let target = (3, 64, -2);
        let block = 1_u16 << 4;
        let bare_minimum = mine_rule(block, None).unwrap().0;
        assert!(bare_minimum > Duration::from_secs(1));

        for damage in [i16::MIN, -1] {
            let malformed = stack(270, damage);
            let mut too_early = MiningProgress::start(target, block, malformed).unwrap();
            too_early.started = Instant::now() - Duration::from_millis(100);
            assert!(too_early.finish(target, block, malformed).is_none());

            let mut completed = MiningProgress::start(target, block, malformed).unwrap();
            completed.started = Instant::now() - bare_minimum;
            assert_eq!(
                completed.finish(target, block, malformed),
                mine_rule(block, None).map(|(_, result)| result)
            );

            let mut switched = MiningProgress::start(target, block, stack(270, 0)).unwrap();
            switched.started = Instant::now() - bare_minimum;
            assert!(switched.finish(target, block, malformed).is_none());
        }
    }

    #[test]
    fn s03_negative_tool_damage_cannot_bypass_environment_mining_penalties() {
        // Invalid durability is bare-hand mining even if the player starts
        // submerged and becomes airborne before the finish packet arrives.
        let target = (-3, 63, 5);
        let block = 1_u16 << 4;
        let bare_minimum = mine_rule(block, None).unwrap().0;
        for damage in [i16::MIN, -1] {
            let malformed = stack(270, damage);
            let mut early =
                MiningProgress::start_with_environment(target, block, malformed, true, true)
                    .unwrap();
            early.started = Instant::now() - bare_minimum.saturating_mul(5);
            assert!(
                early
                    .finish_with_environment(target, block, malformed, true, false)
                    .is_none(),
                "invalid damage {damage} bypassed the airborne penalty"
            );

            let mut complete =
                MiningProgress::start_with_environment(target, block, malformed, true, true)
                    .unwrap();
            complete.started = Instant::now() - bare_minimum.saturating_mul(25);
            assert_eq!(
                complete.finish_with_environment(target, block, malformed, true, false),
                mine_rule(block, None).map(|(_, result)| result),
                "invalid damage {damage} changed the bare-hand harvest result"
            );
        }
    }

    #[test]
    fn matching_tools_mine_faster_and_have_correct_durability() {
        let bare = mine_rule(1 << 4, None).unwrap().0;
        let wood = mine_rule(1 << 4, stack(270, 0)).unwrap().0;
        let stone = mine_rule(1 << 4, stack(274, 0)).unwrap().0;
        assert!(bare > wood && wood > stone);
        assert_eq!(tool_max_uses(stack(270, 0).unwrap()), Some(59));
        assert_eq!(tool_max_uses(stack(274, 0).unwrap()), Some(131));
        assert_eq!(tool_max_uses(stack(257, 0).unwrap()), Some(250));
        assert_eq!(tool_max_uses(stack(278, 0).unwrap()), Some(1561));
        assert_eq!(tool_max_uses(stack(285, 0).unwrap()), Some(32));
        assert_eq!(tool_max_uses(stack(274, 131).unwrap()), None);
    }
    #[test]
    fn submerged_and_airborne_mining_penalties_stack_and_cannot_be_skipped() {
        let target = (0, 64, 0);
        let block = 1 << 4;
        let held = stack(257, 0);
        let base = mine_rule(block, held).unwrap().0;
        assert_eq!(environment_penalty(false, true), 1);
        assert_eq!(environment_penalty(true, true), 5);
        assert_eq!(environment_penalty(false, false), 5);
        assert_eq!(environment_penalty(true, false), 25);

        let mut in_water =
            MiningProgress::start_with_environment(target, block, held, true, false).unwrap();
        in_water.started = Instant::now() - base.saturating_mul(5);
        assert!(in_water
            .finish_with_environment(target, block, held, false, true)
            .is_none());
        let mut completed =
            MiningProgress::start_with_environment(target, block, held, true, false).unwrap();
        completed.started = Instant::now() - base.saturating_mul(26);
        assert_eq!(
            completed.finish_with_environment(target, block, held, true, false),
            mine_rule(block, held).map(|(_, result)| result),
        );

        let mut enter_water =
            MiningProgress::start_with_environment(target, block, held, false, true).unwrap();
        enter_water.started = Instant::now() - base.saturating_mul(2);
        assert!(enter_water
            .finish_with_environment(target, block, held, true, true)
            .is_none());
    }

    #[test]
    fn mining_allows_one_tick_of_network_quantization_but_not_instant_dirt() {
        let at = (5, 66, 4);
        let block = 3 << 4;
        let base = mine_rule(block, None).unwrap().0;
        assert!(base >= Duration::from_millis(250));
        let mut close = MiningProgress::start(at, block, None).unwrap();
        close.started = Instant::now() - (base - Duration::from_millis(40));
        assert!(close.finish(at, block, None).is_some());
        let early = MiningProgress::start(at, block, None).unwrap();
        assert!(early.finish(at, block, None).is_none());
    }

    #[test]
    fn instant_break_foliage_accepts_same_tick_finish() {
        let at = (8, 66, -2);
        for id in [31_u16, 32, 37, 38, 39, 40, 83, 111, 175] {
            let block = id << 4;
            let (minimum, _) = mine_rule(block, None).unwrap();
            assert!(minimum.is_zero(), "instant block {id} requires a delay");
            let progress = MiningProgress::start(at, block, None).unwrap();
            assert!(
                progress.finish(at, block, None).is_some(),
                "instant block {id}"
            );
        }
    }

    #[test]
    fn common_1710_surface_blocks_remain_breakable_without_dropping_wrong_items() {
        for (id, expected) in [
            (78_u16, None),
            (79, None),
            (106, None),
            (110, Some(3_i16)),
            (175, None),
            (88, Some(88)),
        ] {
            let (_, result) = mine_rule(id << 4, None).unwrap();
            assert_eq!(result.drop.map(|drop| drop.id), expected, "block {id}");
        }
        assert!(
            mine_rule(7 << 4, None).is_none(),
            "bedrock must remain unbreakable"
        );
    }

    #[test]
    fn doors_and_beds_can_be_mined_but_only_lower_halves_drop_items() {
        for (block, item) in [(26_u16, 355_i16), (64, 324)] {
            let lower = mine_rule(block << 4, None).unwrap().1;
            let upper = mine_rule(block << 4 | 8, None).unwrap().1;
            assert_eq!(lower.drop.unwrap().id, item);
            assert!(upper.drop.is_none());
        }
        let bare_iron = mine_rule(71 << 4, None).unwrap();
        assert!(bare_iron.1.drop.is_none());
        let picked_iron = mine_rule(71 << 4 | 8, stack(257, 0)).unwrap().1;
        assert!(picked_iron.drop.is_none()); // Upper alone does not duplicate doors.
        assert_eq!(
            mine_rule(71 << 4, stack(257, 0))
                .unwrap()
                .1
                .drop
                .unwrap()
                .id,
            330
        );
    }

    #[test]
    fn one_packet_instant_mining_and_missing_legacy_blocks() {
        let at = (0, 64, 0);
        for id in [31_u16, 37, 46, 50, 83] {
            let started = MiningProgress::start(at, id << 4, None).unwrap();
            assert!(started.is_instant(), "block {id} must accept Start alone");
            assert!(started.finish(at, id << 4, None).is_some());
        }
        let dirt = MiningProgress::start(at, 3 << 4, None).unwrap();
        assert!(
            !dirt.is_instant(),
            "normal dirt must still require a Finish"
        );
        for id in [
            19_u16, 22, 23, 29, 33, 47, 95, 101, 112, 121, 123, 133, 139, 146, 147, 152, 154, 156,
            157, 160, 171, 174,
        ] {
            assert!(
                mine_rule(id << 4, None).is_some(),
                "missing breakable 1.7.10 block {id}"
            );
        }
        assert_eq!(
            additional_drop(47 << 4, at, 42).map(|s| (s.id, s.count)),
            Some((340, 3))
        );
        for id in [0_u16, 7, 8, 9, 10, 11, 90, 119, 120, 137, 166] {
            assert!(mine_rule(id << 4, None).is_none(), "protected block {id}");
        }
    }

    #[test]
    fn immediate_finish_or_switched_tool_does_not_break_block() {
        let target = (0, 63, 0);
        let dig = MiningProgress::start(target, 2 << 4, None).unwrap();
        assert!(dig.finish(target, 2 << 4, None).is_none());
        let dig = MiningProgress::start(target, 1 << 4, stack(274, 0)).unwrap();
        assert!(dig.finish(target, 1 << 4, stack(278, 0)).is_none());
        let dig = MiningProgress::start(target, 2 << 4, None).unwrap();
        assert!(dig.finish((1, 63, 0), 2 << 4, None).is_none());
        let dig = MiningProgress::start(target, 2 << 4, None).unwrap();
        assert!(dig.finish(target, 3 << 4, None).is_none());
    }
}
