//! Java 1.7.10 workstation rules that do not require arbitrary ItemStack NBT.
//! Enchanted-item creation, anvil renaming, and enchanted-book combination MUST
//! wait until the live Stack representation preserves NBT. Never silently strip tags.
use crate::inventory::Stack;
use crate::items_1710;

/// Counts bookshelves at the vanilla two-block offset, with an air gap.
#[allow(dead_code)] // D35 offer model, pending generic ItemStack NBT
pub fn bookshelf_count(mut block: impl FnMut(i32, i32, i32) -> u16) -> u8 {
    let mut found = 0u8;
    for dx in -1i32..=1 {
        for dz in -1i32..=1 {
            if dx.abs() != 1 && dz.abs() != 1 {
                continue;
            }
            // The horizontal gap at both heights must be empty.
            if block(dx, 0, dz) != 0 || block(dx, 1, dz) != 0 {
                continue;
            }
            for dy in 0..=1 {
                if block(dx * 2, dy, dz * 2) == 47 {
                    found = found.saturating_add(1);
                }
                if dx != 0 && dz != 0 {
                    if block(dx * 2, dy, dz) == 47 {
                        found = found.saturating_add(1);
                    }
                    if block(dx, dy, dz * 2) == 47 {
                        found = found.saturating_add(1);
                    }
                }
            }
        }
    }
    found.min(15)
}

/// Three 1.7.10 enchanting offer levels using java.util.Random's 48-bit
/// LCG and nextInt(bound), not a substitute xorshift distribution.
/// Seed is server-owned; this does NOT create enchanted ItemStack NBT.
pub fn enchanting_costs(shelves: u8, seed: u64) -> [u8; 3] {
    let shelves = u32::from(shelves.min(15));
    let mut state = (seed ^ 0x5DEECE66Du64) & ((1u64 << 48) - 1);
    let mut next_int = |bound: u32| -> u32 {
        assert!(bound > 0);
        loop {
            state = state.wrapping_mul(0x5DEECE66D).wrapping_add(11) & ((1u64 << 48) - 1);
            let bits = (state >> 17) as u32; // Java Random.next(31)
            if bound.is_power_of_two() {
                return ((u64::from(bits) * u64::from(bound)) >> 31) as u32;
            }
            let value = bits % bound;
            if u64::from(bits - value) + u64::from(bound - 1) <= i32::MAX as u64 {
                return value;
            }
        }
    };
    let mut costs = [0u8; 3];
    for (option, cost) in costs.iter_mut().enumerate() {
        let base = 1 + next_int(8) + (shelves >> 1) + next_int(shelves + 1);
        let level = match option {
            0 => (base / 3).max(1),
            1 => 2 * base / 3 + 1,
            _ => base.max(2 * shelves),
        };
        // Vanilla hides an offer below the minimum for its row.
        *cost = if level < option as u32 + 1 {
            0
        } else {
            level as u8
        };
    }
    costs
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Repair {
    pub result: Stack,
    /// Total amount consumed from the right slot; left slot always consumes one.
    pub consumed: u8,
    /// Level cost (not raw XP points).
    pub levels: u16,
}

/// Durability-only anvil repair for untagged stacks. Never accepts names or
/// enchantments because the present Stack does not preserve them.
pub fn repair(left: Stack, right: Stack) -> Option<Repair> {
    if !left.is_valid() || !right.is_valid() || left.count != 1 {
        return None;
    }
    let max = items_1710::max_durability(left.id)?;
    if left.damage < 0 || left.damage >= max || max <= 0 {
        return None;
    }
    let material = match left.id {
        256..=258 | 267 | 292 | 302..=309 => 265, // iron and chain
        268..=271 | 290 => 5,                     // wood
        272..=275 | 291 => 4,                     // cobblestone
        276..=279 | 293 | 310..=313 => 264,       // diamond
        283..=286 | 294 | 314..=317 => 266,       // gold
        _ => -1,
    };
    let material = if (298..=301).contains(&left.id) {
        334
    } else {
        material
    };
    if right.id == material && material > 0 {
        let per_unit = (max / 4).max(1);
        let units = u16::from(right.count).min(4);
        let available = u16::try_from(left.damage).ok()?;
        let required = available.div_ceil(per_unit as u16);
        let consumed = units.min(required) as u8;
        if consumed == 0 {
            return None;
        }
        let restored = (i32::from(per_unit) * i32::from(consumed)).min(i32::from(left.damage));
        let result = Stack {
            damage: left.damage - restored as i16,
            ..left
        };
        return Some(Repair {
            result,
            consumed,
            levels: u16::from(consumed),
        });
    }
    if right.id == left.id && right.count == 1 {
        let left_remaining = i32::from(max - left.damage);
        let right_remaining = i32::from(max - right.damage);
        let combined =
            (left_remaining + right_remaining + i32::from(max) * 12 / 100).min(i32::from(max));
        let new_damage = (i32::from(max) - combined) as i16;
        if new_damage >= left.damage {
            return None;
        }
        return Some(Repair {
            result: Stack {
                damage: new_damage,
                ..left
            },
            consumed: 1,
            levels: 2,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    fn stack(id: i16, count: u8, damage: i16) -> Stack {
        Stack { id, count, damage }
    }
    #[test]
    fn bookshelf_gap_and_cap_are_server_calculated() {
        let mut solid = |x: i32, y: i32, z: i32| {
            if x == 2 && z == 0 && y == 0 {
                47
            } else {
                0
            }
        };
        assert_eq!(bookshelf_count(&mut solid), 1);
        let obstructed = |x: i32, y: i32, z: i32| {
            if x == 1 && y == 0 && z == 0 {
                1
            } else if x == 2 && y == 0 && z == 0 {
                47
            } else {
                0
            }
        };
        assert_eq!(bookshelf_count(obstructed), 0);
        let all = |x: i32, _y: i32, z: i32| {
            if (x.abs() == 2 || z.abs() == 2) && (x.abs() <= 2 && z.abs() <= 2) {
                47
            } else {
                0
            }
        };
        assert_eq!(bookshelf_count(all), 15);
    }
    #[test]
    fn java_random_enchant_offer_vectors_match_1710_rule() {
        assert_eq!(enchanting_costs(0, 0), [2, 2, 6]);
        assert_eq!(enchanting_costs(15, 42), [4, 9, 30]);
        assert_eq!(enchanting_costs(15, 0), [8, 13, 30]);
        assert_eq!(enchanting_costs(4, 12345), [1, 9, 13]);
        assert_eq!(enchanting_costs(1, 10), [2, 3, 0]);
    }
    #[test]
    fn costs_are_bounded_and_repeatable() {
        assert_eq!(enchanting_costs(15, 42), enchanting_costs(99, 42));
        for seed in 0..500 {
            let costs = enchanting_costs(15, seed);
            assert!((1..=10).contains(&costs[0]));
            assert!((1..=21).contains(&costs[1]));
            assert_eq!(costs[2], 30);
        }
    }
    #[test]
    fn repairing_iron_pickaxe_consumes_only_needed_ingots() {
        let plan = repair(stack(257, 1, 100), stack(265, 4, 0)).unwrap();
        assert!(plan.result.damage < 100);
        assert!((1..=4).contains(&plan.consumed));
        assert_eq!(plan.levels, u16::from(plan.consumed));
    }
    #[test]
    fn rejecting_invalid_anvil_inputs_preserves_inventory() {
        assert!(repair(stack(257, 2, 100), stack(265, 3, 0)).is_none());
        assert!(repair(stack(257, 1, 0), stack(265, 3, 0)).is_none());
        assert!(repair(stack(257, 1, 100), stack(264, 3, 0)).is_none());
    }
}
