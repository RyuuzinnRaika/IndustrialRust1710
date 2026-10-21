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
    if held.count != 1 || held.damage >= max_uses {
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
        2 | 3 => (0.5, Some(ToolKind::Shovel), 0, false, item(3, 0)),
        12 => (0.5, Some(ToolKind::Shovel), 0, false, item(12, damage)),
        13 => (0.6, Some(ToolKind::Shovel), 0, false, item(13, 0)),
        80 => (0.2, Some(ToolKind::Shovel), 0, false, item(332, 0)),
        82 => (0.6, Some(ToolKind::Shovel), 0, false, item(337, 0)),
        1 => (1.5, Some(ToolKind::Pickaxe), 0, true, item(4, 0)),
        4 => (2.0, Some(ToolKind::Pickaxe), 0, true, item(4, 0)),
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
        18 => (0.2, None, 0, false, None), // leaf drops handled separately
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
        17 | 162 => (2.0, Some(ToolKind::Axe), 0, false, item(id as i16, damage)),
        54 => (2.5, Some(ToolKind::Axe), 0, false, item(54, 0)),
        58 => (2.5, Some(ToolKind::Axe), 0, false, item(58, 0)),
        61 | 62 => (3.5, Some(ToolKind::Pickaxe), 0, true, item(61, 0)),
        20 => (0.3, None, 0, false, None), // glass: breaks but no drop
        35 => (0.8, None, 0, false, item(35, damage)),
        87 => (0.4, Some(ToolKind::Pickaxe), 0, true, item(87, 0)),
        89 => (0.3, None, 0, false, item(348, 0)),
        170 => (0.5, Some(ToolKind::Axe), 0, false, item(170, 0)),
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
pub fn additional_drop(block:u16, pos:(i32,i32,i32), tick:u64) -> Option<Stack> {
    let (x,y,z)=pos;
    let mut roll=tick
        ^ (x as i64 as u64).wrapping_mul(0x9e3779b185ebca87)
        ^ (y as i64 as u64).wrapping_mul(0xc2b2ae3d27d4eb4f)
        ^ (z as i64 as u64).wrapping_mul(0x165667b19e3779f9);
    roll ^= roll >> 30;
    roll=roll.wrapping_mul(0xbf58476d1ce4e5b9);
    roll ^= roll >> 27;
    roll=roll.wrapping_mul(0x94d049bb133111eb);
    roll ^= roll >> 31;
    let (id,count) = match block >> 4 {
        18 if roll % 200 == 0 => (260,1), // apple
        18 if roll % 20 == 0 => (6,1), // sapling
        59 if block & 7 == 7 => (295,1 + (roll % 3) as u8),
        141 if block & 7 == 7 => (391,1 + (roll % 3) as u8),
        142 if block & 7 == 7 => (392,1 + (roll % 3) as u8),
        _ => return None,
    };
    Some(Stack {id, count, damage:0})
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
    let ticks = ((spec.hardness * if can_harvest { 30.0 } else { 100.0 }) / speed)
        .ceil()
        .max(1.0) as u64;
    let drop = if can_harvest { spec.drop } else { None };
    Some((
        Duration::from_millis(ticks.saturating_mul(50)),
        MineResult {
            drop,
            wear_tool: equipped.is_some(),
        },
    ))
}
#[derive(Clone, Copy, Debug)]
pub struct MiningProgress {
    target: (i32, i32, i32),
    started: Instant,
    block: u16,
    tool: Option<Stack>,
}
impl MiningProgress {
    pub fn start(target: (i32, i32, i32), block: u16, held: Option<Stack>) -> Option<Self> {
        mine_rule(block, held)?;
        Some(Self {
            target,
            started: Instant::now(),
            block,
            tool: held,
        })
    }
    /// Consume the attempt exactly once. The held tool must remain unchanged
    /// between starting and finishing a dig, preventing fast-tool switching.
    pub fn finish(
        self,
        target: (i32, i32, i32),
        current: u16,
        held: Option<Stack>,
    ) -> Option<MineResult> {
        if self.target != target || self.block != current || self.tool != held {
            return None;
        }
        let (minimum, result) = mine_rule(current, held)?;
        (self.started.elapsed() >= minimum).then_some(result)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn stack(id: i16, damage: i16) -> Option<Stack> {
        Some(Stack {
            id,
            damage,
            count: 1,
        })
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
