//! Minecraft Java 1.7.10 workstation rules.
//! Anvil durability repairs and compatible enchantment merging preserve NBT.
//! Renaming and full prior-work cost compatibility remain separate work.
use crate::inventory::Stack;
use crate::{enchanting, item_nbt, items_1710};
use fastnbt::Value;
use std::collections::{HashMap, HashSet};

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

/// Only edit vanilla enchantments that can be decoded without ambiguity.
/// Unknown top-level and per-enchantment NBT fields are kept verbatim.
fn nbt_root(stack: Stack) -> Option<HashMap<String, Value>> {
    match stack.nbt {
        None => Some(HashMap::new()),
        Some(handle) => match item_nbt::value(handle).ok()? {
            Value::Compound(fields) => Some(fields),
            _ => None,
        },
    }
}

fn enchantment_entries(entries: &[Value]) -> Option<Vec<(i16, i16)>> {
    let mut seen = HashSet::new();
    let mut parsed = Vec::with_capacity(entries.len());
    for entry in entries {
        let Value::Compound(fields) = entry else {
            return None;
        };
        let (Some(Value::Short(id)), Some(Value::Short(level))) =
            (fields.get("id"), fields.get("lvl"))
        else {
            return None;
        };
        let max = enchanting::anvil_level_limit(403, *id)?;
        if !(1..=max).contains(level) || !seen.insert(*id) {
            return None;
        }
        parsed.push((*id, *level));
    }
    Some(parsed)
}

/// Merge only compatible, valid 1.7.10 enchantments. An invalid enchant tag,
/// incompatible pair, or unrepresentable output must not consume either input.
fn merge_anvil_enchantments(left: Stack, right: Stack) -> Option<(u64, u16)> {
    let source_key = if right.id == 403 {
        "StoredEnchantments"
    } else {
        "ench"
    };
    let target_key = if left.id == 403 {
        "StoredEnchantments"
    } else {
        "ench"
    };
    let source = nbt_root(right)?;
    let Some(Value::List(incoming)) = source.get(source_key) else {
        return None;
    };
    let incoming_parsed = enchantment_entries(incoming)?;
    if incoming_parsed.is_empty() {
        return None;
    }
    let mut root = nbt_root(left)?;
    let mut target_entries = match root.get(target_key) {
        None => Vec::new(),
        Some(Value::List(items)) => items.clone(),
        _ => return None,
    };
    let mut existing = enchantment_entries(&target_entries)?;
    // An imported or client-forged enchantment incompatible with the item
    // must never become a valid anvil result through an otherwise valid book.
    if existing
        .iter()
        .any(|&(id, _)| enchanting::anvil_level_limit(left.id, id).is_none())
    {
        return None;
    }
    let mut levels = 0u16;
    let mut changed = false;
    for (entry, &(id, incoming_level)) in incoming.iter().zip(&incoming_parsed) {
        let max = enchanting::anvil_level_limit(left.id, id)?;
        if existing
            .iter()
            .any(|&(other, _)| enchanting::anvil_enchant_conflicts(id, other))
        {
            return None;
        }
        let prior = existing
            .iter()
            .find(|&&(other, _)| other == id)
            .map(|&(_, lvl)| lvl);
        let next_level = prior.map_or(incoming_level, |old| {
            if old == incoming_level && old < max {
                old + 1
            } else {
                old.max(incoming_level)
            }
        });
        if next_level > max {
            return None;
        }
        if prior.is_some_and(|old| old >= next_level) {
            continue;
        }
        // Preserve any unknown fields in the original entry. When a new
        // enchantment is added, preserve the source entry's extra tags.
        if let Some(index) = existing.iter().position(|&(other, _)| other == id) {
            let Value::Compound(fields) = &mut target_entries[index] else {
                return None;
            };
            fields.insert("lvl".to_owned(), Value::Short(next_level));
            existing[index].1 = next_level;
        } else {
            target_entries.push(entry.clone());
            existing.push((id, next_level));
        }
        let multiplier = if right.id == 403 { 1u16 } else { 2u16 };
        levels = levels.checked_add((next_level as u16).checked_mul(multiplier)?)?;
        changed = true;
    }
    if !changed || levels == 0 {
        return None;
    }
    root.insert(target_key.to_owned(), Value::List(target_entries));
    let handle = item_nbt::from_value(&Value::Compound(root)).ok()?;
    Some((handle, levels))
}

/// Return a server-owned anvil preview. Material and durability repairs retain
/// the left stack's NBT; matching items and enchanted books may additionally
/// merge compatible enchantments while preserving unknown NBT fields.
pub fn repair(left: Stack, right: Stack) -> Option<Repair> {
    if !left.is_valid() || !right.is_valid() || left.count != 1 {
        return None;
    }
    let max = items_1710::max_durability(left.id).unwrap_or(0);
    if left.damage < 0 || (max > 0 && left.damage > max) {
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
    if right.id == material && material > 0 && max > 0 && left.damage > 0 {
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
    if right.count != 1 || (right.id != left.id && right.id != 403) {
        return None;
    }
    // Book+book or enchanted book+tool may add enchantments without repairing
    // durability. Two matching tools can perform both operations atomically.
    let enchant = merge_anvil_enchantments(left, right);
    let mut repaired = left.damage;
    let mut levels = 0u16;
    if right.id == left.id && max > 0 && left.damage > 0 {
        let left_remaining = i32::from(max - left.damage);
        let right_remaining = i32::from(max - right.damage);
        let combined =
            (left_remaining + right_remaining + i32::from(max) * 12 / 100).min(i32::from(max));
        let new_damage = (i32::from(max) - combined) as i16;
        if new_damage < left.damage {
            repaired = new_damage;
            levels = 2;
        }
    }
    let mut result = Stack {
        damage: repaired,
        ..left
    };
    if let Some((nbt, enchant_cost)) = enchant {
        result.nbt = Some(nbt);
        levels = levels.checked_add(enchant_cost)?;
    }
    // The level cap is a conservative Survival-compatible guard until the
    // complete 1.7.10 prior-work/Creative exception rules are implemented.
    if levels == 0 || levels > 39 {
        return None;
    }
    Some(Repair {
        result,
        consumed: 1,
        levels,
    })
}

/// Calculate the anvil result after an optional MC|ItemName update.
/// A missing request retains the existing custom name; an empty request
/// removes the custom name without deleting other display or mod NBT fields.
/// Rename-only results use no right-hand ingredients and cost one level.
pub fn repair_with_name(left: Stack, right: Option<Stack>, name: Option<&str>) -> Option<Repair> {
    if !left.is_valid() || left.count != 1 {
        return None;
    }
    let mut plan = right.and_then(|ingredient| repair(left, ingredient));
    let Some(requested) = name else {
        return plan;
    };
    // The 1.7.10 anvil text box accepts at most 30 UTF-16 code units.
    // Reject control characters rather than creating invisible NBT names.
    if requested.encode_utf16().count() > 30 || requested.chars().any(char::is_control) {
        return None;
    }
    // Do not manufacture a rename-only output while a nonempty right slot
    // contains an invalid or incompatible ingredient.
    if right.is_some() && plan.is_none() {
        return None;
    }

    let left_root = nbt_root(left)?;
    let previous = match left_root.get("display") {
        None => None,
        Some(Value::Compound(fields)) => match fields.get("Name") {
            None => None,
            Some(Value::String(value)) => Some(value.as_str()),
            _ => return None,
        },
        _ => return None,
    };
    let unchanged = previous == Some(requested) || (requested.is_empty() && previous.is_none());
    if unchanged {
        return plan;
    }

    let mut output = plan.map_or(left, |value| value.result);
    let mut root = nbt_root(output)?;
    if requested.is_empty() {
        if let Some(Value::Compound(display)) = root.get_mut("display") {
            display.remove("Name");
            if display.is_empty() {
                root.remove("display");
            }
        }
    } else {
        let entry = root
            .entry("display".to_owned())
            .or_insert_with(|| Value::Compound(HashMap::new()));
        let Value::Compound(display) = entry else {
            return None;
        };
        display.insert("Name".to_owned(), Value::String(requested.to_owned()));
    }
    output.nbt = if root.is_empty() {
        None
    } else {
        Some(item_nbt::from_value(&Value::Compound(root)).ok()?)
    };
    let mut result = plan.take().unwrap_or(Repair {
        result: left,
        consumed: 0,
        levels: 0,
    });
    result.result = output;
    result.levels = result.levels.checked_add(1)?;
    (result.levels <= 39).then_some(result)
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
    fn enchanted_item(id: i16, damage: i16, enchant_id: i16, level: i16) -> Stack {
        use std::collections::HashMap;
        let key = if id == 403 {
            "StoredEnchantments"
        } else {
            "ench"
        };
        let value = Value::Compound(HashMap::from([
            ("UnknownCustom".to_string(), Value::Int(904)),
            (
                key.to_string(),
                Value::List(vec![Value::Compound(HashMap::from([
                    ("id".to_string(), Value::Short(enchant_id)),
                    ("lvl".to_string(), Value::Short(level)),
                    ("ExtraTag".to_string(), Value::String("unchanged".into())),
                ]))]),
            ),
        ]));
        Stack {
            id,
            count: 1,
            damage,
            nbt: Some(item_nbt::from_value(&value).unwrap()),
        }
    }

    #[test]
    fn d36_two_enchanted_swords_upgrade_equal_levels_without_losing_tags() {
        let left = enchanted_item(276, 0, 16, 3); // sharpness III
        let right = enchanted_item(276, 100, 16, 3);
        let plan = repair(left, right).unwrap();
        assert_eq!(plan.consumed, 1);
        assert_eq!(plan.result.damage, 0);
        assert_eq!(plan.levels, 8);
        let Value::Compound(root) = item_nbt::value(plan.result.nbt.unwrap()).unwrap() else {
            panic!("compound")
        };
        assert_eq!(root.get("UnknownCustom"), Some(&Value::Int(904)));
        let Some(Value::List(entries)) = root.get("ench") else {
            panic!("enchantments")
        };
        let Value::Compound(entry) = &entries[0] else {
            panic!("enchantment compound")
        };
        assert_eq!(entry.get("lvl"), Some(&Value::Short(4)));
        assert_eq!(
            entry.get("ExtraTag"),
            Some(&Value::String("unchanged".into()))
        );
        assert_eq!(plan.result.id, left.id);
        assert!(plan.result.is_valid());
    }

    #[test]
    fn d36_enchanted_book_can_enchant_pristine_tool_and_preserves_custom_nbt() {
        let left = stack(257, 1, 0); // pristine iron pickaxe
        let right = enchanted_item(403, 0, 32, 3); // Efficiency III
        let plan = repair(left, right).unwrap();
        assert_eq!(plan.result.id, 257);
        assert_eq!(plan.result.damage, 0);
        assert_eq!(plan.levels, 3);
        let Value::Compound(root) = item_nbt::value(plan.result.nbt.unwrap()).unwrap() else {
            panic!("compound")
        };
        assert!(root.contains_key("ench"));
        assert!(!root.contains_key("StoredEnchantments"));
        assert_eq!(plan.consumed, 1);
        assert_eq!(right.nbt.map(item_nbt::is_live), Some(true));
    }

    #[test]
    fn d36_rejects_conflicting_or_invalid_enchants_and_preserves_inputs() {
        let sharp = enchanted_item(276, 0, 16, 3);
        let smite = enchanted_item(403, 0, 17, 3);
        assert!(repair(sharp, smite).is_none()); // Sharpness vs Smite
        let invalid_tool = enchanted_item(257, 0, 16, 1);
        assert!(repair(invalid_tool, enchanted_item(403, 0, 32, 1)).is_none());
        let already_maxed = enchanted_item(276, 0, 16, 5);
        assert!(repair(already_maxed, enchanted_item(403, 0, 16, 5)).is_none());
        // Unrelated enchantments cannot be applied to an incompatible item.
        assert!(repair(stack(257, 1, 0), smite).is_none());

        let wrong = Value::Compound(std::collections::HashMap::from([(
            "StoredEnchantments".to_string(),
            Value::String("not a list".into()),
        )]));
        let malformed_book = Stack {
            nbt: Some(item_nbt::from_value(&wrong).unwrap()),
            ..stack(403, 1, 0)
        };
        assert!(repair(stack(276, 1, 10), malformed_book).is_none());
    }

    #[test]
    fn d36_rename_only_creates_display_nbt_and_clearing_keeps_unknown_tags() {
        let stone = stack(1, 1, 0);
        let plan = repair_with_name(stone, None, Some("工具の名前")).unwrap();
        assert_eq!(plan.levels, 1);
        assert_eq!(plan.consumed, 0);
        assert_eq!(plan.result.id, stone.id);
        let Value::Compound(root) = item_nbt::value(plan.result.nbt.unwrap()).unwrap() else {
            panic!("compound");
        };
        let Some(Value::Compound(display)) = root.get("display") else {
            panic!("display");
        };
        assert_eq!(
            display.get("Name"),
            Some(&Value::String("工具の名前".into()))
        );
        assert!(repair_with_name(stone, None, Some("")).is_none());

        let custom = Value::Compound(HashMap::from([
            ("CustomUnknown".to_owned(), Value::Int(71)),
            (
                "display".to_owned(),
                Value::Compound(HashMap::from([
                    ("Name".to_owned(), Value::String("old".into())),
                    (
                        "Lore".to_owned(),
                        Value::List(vec![Value::String("keep".into())]),
                    ),
                ])),
            ),
        ]));
        let existing = Stack {
            nbt: Some(item_nbt::from_value(&custom).unwrap()),
            ..stone
        };
        let result = repair_with_name(existing, None, Some("")).unwrap();
        assert_eq!(result.levels, 1);
        let Value::Compound(root) = item_nbt::value(result.result.nbt.unwrap()).unwrap() else {
            panic!("compound");
        };
        assert_eq!(root.get("CustomUnknown"), Some(&Value::Int(71)));
        let Some(Value::Compound(display)) = root.get("display") else {
            panic!("display");
        };
        assert!(!display.contains_key("Name"));
        assert_eq!(
            display.get("Lore"),
            Some(&Value::List(vec![Value::String("keep".into())]))
        );
        assert!(repair_with_name(existing, None, Some("old")).is_none());
    }

    #[test]
    fn d36_rename_and_repair_combine_and_reject_malformed_names() {
        let left = stack(257, 1, 100);
        let right = stack(265, 2, 0);
        let repaired = repair(left, right).unwrap();
        let named = repair_with_name(left, Some(right), Some("New Pickaxe")).unwrap();
        assert_eq!(named.levels, repaired.levels + 1);
        assert_eq!(named.consumed, repaired.consumed);
        assert_eq!(named.result.damage, repaired.result.damage);
        assert!(named.result.nbt.is_some());
        // Repeated names, control characters and oversized GUI input cannot
        // increase the cost or create invalid item metadata.
        assert!(repair_with_name(left, None, Some(&"A".repeat(31))).is_none());
        assert!(repair_with_name(left, None, Some(&format!("A{}B", char::from(10)))).is_none());
        assert!(repair_with_name(left, None, Some(&"😀".repeat(16))).is_none());
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
