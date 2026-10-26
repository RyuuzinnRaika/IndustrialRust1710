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

/// Candidate enchantments by vanilla 1.7.10 numeric enchantment ID.
/// This is a conservative, one-enchantment baseline rather than the complete
/// weighted/multiple-enchantment vanilla enchantment algorithm. Keeping it
/// separate from the live GUI prevents an incomplete ItemStack model from
/// stripping client-visible NBT.
fn enchantment_candidates(item: i16) -> &'static [(i16, i16)] {
    // (enchantment ID, maximum level)
    const ARMOR: &[(i16, i16)] = &[(0, 4), (1, 4), (3, 4), (4, 4), (7, 3), (34, 3)];
    const HELMETS: &[(i16, i16)] = &[
        (0, 4),
        (1, 4),
        (3, 4),
        (4, 4),
        (5, 3),
        (6, 1),
        (7, 3),
        (34, 3),
    ];
    const BOOTS: &[(i16, i16)] = &[(0, 4), (1, 4), (2, 4), (3, 4), (4, 4), (7, 3), (34, 3)];
    const SWORDS: &[(i16, i16)] = &[
        (16, 5),
        (17, 5),
        (18, 5),
        (19, 2),
        (20, 2),
        (21, 3),
        (34, 3),
    ];
    const TOOLS: &[(i16, i16)] = &[(32, 5), (33, 1), (34, 3), (35, 3)];
    const BOWS: &[(i16, i16)] = &[(48, 5), (49, 2), (50, 1), (51, 1), (34, 3)];
    const BOOKS: &[(i16, i16)] = &[
        (0, 4),
        (2, 4),
        (5, 3),
        (16, 5),
        (19, 2),
        (21, 3),
        (32, 5),
        (33, 1),
        (34, 3),
        (35, 3),
        (48, 5),
        (50, 1),
    ];
    match item {
        340 => BOOKS,
        261 => BOWS,
        267 | 268 | 272 | 276 | 283 => SWORDS,
        256..=258 | 269..=271 | 273..=275 | 277..=279 | 284..=286 | 290..=294 => TOOLS,
        298 | 302 | 306 | 310 | 314 => HELMETS,
        301 | 305 | 309 | 313 | 317 => BOOTS,
        299 | 300 | 303 | 304 | 307 | 308 | 311 | 312 | 315 | 316 => ARMOR,
        _ => &[],
    }
}

/// Applies one server-selected enchantment to the NBT-capable *legacy*
/// ItemStack, preserving all unknown fields and nested existing tags.
///
/// Returns None rather than overwriting malformed NBT or an existing enchant.
/// The live inventory::Stack still lacks an arbitrary NBT field, so callers
/// MUST NOT convert this value back to that lossy representation.
/// This function is an isolated D31/D35 migration primitive, not the GUI handler.
#[allow(dead_code)] // migration primitive; live Stack still lacks arbitrary NBT
pub(crate) fn enchant_nbt_item(
    input: &crate::legacy_inventory_nbt::SlotItem,
    offer_cost: u8,
    server_seed: u64,
) -> Option<crate::legacy_inventory_nbt::SlotItem> {
    use fastnbt::Value;

    if offer_cost == 0
        || offer_cost > 30
        || input.count != 1
        || !items_1710::enchantable_in_table(input.id)
    {
        return None;
    }
    let candidates = enchantment_candidates(input.id);
    if candidates.is_empty() {
        return None;
    }

    // Deterministic and wholly server-owned. No client-provided enchantment
    // ID or level is accepted.
    let mixed = server_seed
        ^ (u64::from(offer_cost) << 24)
        ^ (input.id as u16 as u64).wrapping_mul(0x9E3779B97F4A7C15);
    let mixed = mixed ^ (mixed >> 30);
    let mixed = mixed.wrapping_mul(0xBF58476D1CE4E5B9);
    let mixed = mixed ^ (mixed >> 27);
    let mixed = mixed.wrapping_mul(0x94D049BB133111EB);
    let mixed = mixed ^ (mixed >> 31);
    let (enchant_id, max_level) = candidates[(mixed as usize) % candidates.len()];
    let level = ((i16::from(offer_cost) + 6) / 7).clamp(1, max_level);

    let mut tags = match &input.tag {
        None => Default::default(),
        Some(Value::Compound(tags)) => tags.clone(),
        Some(_) => return None,
    };
    // Never replace custom/modded enchant data, even when vanilla tags have
    // an unexpected type; a repair tool must handle that situation explicitly.
    if tags.contains_key("ench") || tags.contains_key("StoredEnchantments") {
        return None;
    }
    let entry = Value::Compound(
        [
            ("id".to_string(), Value::Short(enchant_id)),
            ("lvl".to_string(), Value::Short(level)),
        ]
        .into_iter()
        .collect(),
    );
    let is_book = input.id == 340;
    tags.insert(
        (if is_book {
            "StoredEnchantments"
        } else {
            "ench"
        })
        .to_string(),
        Value::List(vec![entry]),
    );
    let mut output = input.clone();
    output.id = if is_book { 403 } else { input.id };
    output.tag = Some(Value::Compound(tags));
    // Bounded serialized NBT is a prerequisite for later Protocol 5 packets.
    let bytes = fastnbt::to_bytes(output.tag.as_ref()?).ok()?;
    if bytes.len() > 32 * 1024 {
        return None;
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn stack(id: i16, count: u8, damage: i16) -> Stack {
        Stack {
            id,
            count,
            damage,
            nbt: None,
        }
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
    #[test]
    fn d31_d35_enchantment_keeps_unknown_item_and_nested_nbt_fields() {
        use fastnbt::Value;
        use std::collections::BTreeMap;
        let mut tags = match Value::Compound(Default::default()) {
            Value::Compound(tags) => tags,
            _ => unreachable!(),
        };
        tags.insert(
            "CustomUnknown".to_string(),
            Value::List(vec![Value::Int(17), Value::Int(29)]),
        );
        let mut extra = BTreeMap::new();
        extra.insert("ModItemField".into(), Value::String("keep me".into()));
        let input = crate::legacy_inventory_nbt::SlotItem {
            slot: 0,
            id: 276,
            count: 1,
            damage: 14,
            tag: Some(Value::Compound(tags)),
            extra,
        };
        let enchanted = enchant_nbt_item(&input, 30, 42).unwrap();
        assert_eq!(enchanted.id, 276);
        assert_eq!(enchanted.damage, 14);
        assert!(
            matches!(enchanted.extra.get("ModItemField"), Some(Value::String(s)) if s == "keep me")
        );
        let Value::Compound(ref tag) = enchanted.tag.as_ref().unwrap() else {
            panic!("compound")
        };
        assert!(
            matches!(tag.get("CustomUnknown"), Some(Value::List(v)) if v == &vec![Value::Int(17), Value::Int(29)])
        );
        assert!(matches!(tag.get("ench"), Some(Value::List(v)) if v.len() == 1));
        let bytes = fastnbt::to_bytes(&enchanted).unwrap();
        let back: crate::legacy_inventory_nbt::SlotItem = fastnbt::from_bytes(&bytes).unwrap();
        assert_eq!(back.id, 276);
        assert_eq!(back.damage, 14);
        assert!(matches!(back.tag, Some(Value::Compound(ref fields))
            if fields.contains_key("CustomUnknown") && fields.contains_key("ench")));
        assert!(enchant_nbt_item(&back, 30, 42).is_none()); // never overwrite
    }

    #[test]
    fn d35_book_enchantment_and_invalid_offers_are_fail_closed() {
        let book = crate::legacy_inventory_nbt::SlotItem {
            slot: 2,
            id: 340,
            count: 1,
            damage: 0,
            tag: None,
            extra: Default::default(),
        };
        let result = enchant_nbt_item(&book, 11, 123).unwrap();
        assert_eq!(result.id, 403);
        assert!(matches!(result.tag, Some(fastnbt::Value::Compound(ref tag))
            if tag.contains_key("StoredEnchantments") && !tag.contains_key("ench")));
        assert!(enchant_nbt_item(&book, 0, 2).is_none());
        assert!(enchant_nbt_item(&book, 31, 2).is_none());
        let mut stacked = book.clone();
        stacked.count = 2;
        assert!(enchant_nbt_item(&stacked, 10, 2).is_none());
        let mut invalid = book.clone();
        invalid.tag = Some(fastnbt::Value::String("not a compound".into()));
        assert!(enchant_nbt_item(&invalid, 10, 2).is_none());
    }
}
