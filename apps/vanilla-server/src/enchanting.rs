//! Server-authoritative 1.7.10-style enchanting. Never trust client-provided
//! item, enchantment, or XP values. The NBT codec preserves unknown tags.
use crate::inventory::Stack;
use crate::{item_nbt, items_1710};
use fastnbt::Value;
use rand::{rngs::StdRng, Rng, SeedableRng};
use std::collections::HashMap;
use std::io;

#[derive(Clone, Copy)]
struct Rule {
    id: i16,
    max: u8,
    weight: u16,
    min: u8,
    step: u8,
    kind: u8,
}
// Kinds: 1 armor, 2 boots, 3 helmets, 4 swords, 5 tools,
// 6 bow, 7 rod, 8 universal; books support all.
const RULES: &[Rule] = &[
    Rule { id: 0, max: 4, weight: 10, min: 1, step: 11, kind: 1 },
    Rule { id: 1, max: 4, weight: 5, min: 10, step: 8, kind: 1 },
    Rule { id: 2, max: 4, weight: 5, min: 5, step: 6, kind: 2 },
    Rule { id: 3, max: 4, weight: 2, min: 5, step: 8, kind: 1 },
    Rule { id: 4, max: 4, weight: 5, min: 3, step: 6, kind: 1 },
    Rule { id: 5, max: 3, weight: 2, min: 10, step: 10, kind: 3 },
    Rule { id: 6, max: 1, weight: 2, min: 1, step: 0, kind: 3 },
    Rule { id: 7, max: 3, weight: 1, min: 10, step: 20, kind: 1 },
    Rule { id: 16, max: 5, weight: 10, min: 1, step: 11, kind: 4 },
    Rule { id: 17, max: 5, weight: 5, min: 5, step: 8, kind: 4 },
    Rule { id: 18, max: 5, weight: 5, min: 5, step: 8, kind: 4 },
    Rule { id: 19, max: 2, weight: 5, min: 5, step: 20, kind: 4 },
    Rule { id: 20, max: 2, weight: 2, min: 10, step: 20, kind: 4 },
    Rule { id: 21, max: 3, weight: 2, min: 15, step: 9, kind: 4 },
    Rule { id: 32, max: 5, weight: 10, min: 1, step: 10, kind: 5 },
    Rule { id: 33, max: 1, weight: 1, min: 15, step: 0, kind: 5 },
    Rule { id: 34, max: 3, weight: 5, min: 5, step: 8, kind: 8 },
    Rule { id: 35, max: 3, weight: 2, min: 15, step: 9, kind: 5 },
    Rule { id: 48, max: 5, weight: 10, min: 1, step: 10, kind: 6 },
    Rule { id: 49, max: 2, weight: 2, min: 12, step: 20, kind: 6 },
    Rule { id: 50, max: 1, weight: 2, min: 20, step: 0, kind: 6 },
    Rule { id: 51, max: 1, weight: 1, min: 20, step: 0, kind: 6 },
    Rule { id: 61, max: 3, weight: 2, min: 15, step: 9, kind: 7 },
    Rule { id: 62, max: 3, weight: 2, min: 15, step: 9, kind: 7 },
];

fn supports(id: i16, kind: u8) -> bool {
    if id == 340 {
        return true;
    }
    let is_helmet = matches!(id, 298 | 302 | 306 | 310 | 314);
    let is_boots = matches!(id, 301 | 305 | 309 | 313 | 317);
    let is_armor = (298..=317).contains(&id);
    let is_sword = matches!(id, 267 | 268 | 272 | 276 | 283);
    let is_tool = matches!(id, 256..=258 | 269..=271 | 273..=275 | 277..=279 | 284..=286);
    match kind {
        1 => is_armor,
        2 => is_boots,
        3 => is_helmet,
        4 => is_sword,
        5 => is_tool,
        6 => id == 261,
        7 => id == 346,
        8 => is_armor || is_sword || is_tool || matches!(id, 261 | 346),
        _ => false,
    }
}
fn conflicting(a: i16, b: i16) -> bool {
    (a != b)
        && ((a <= 4 && b <= 4)
            || ((16..=18).contains(&a) && (16..=18).contains(&b))
            || (matches!(a, 33 | 35) && matches!(b, 33 | 35)))
}
fn enchantability(id: i16) -> u8 {
    if id == 340 { return 1; }
    if matches!(id, 261 | 346) { return 1; }
    if (298..=301).contains(&id) { return 15; }
    if (302..=305).contains(&id) { return 12; }
    if (306..=309).contains(&id) { return 9; }
    if (310..=313).contains(&id) { return 10; }
    if (314..=317).contains(&id) { return 25; }
    if matches!(id, 268..=271 | 290) { return 15; }
    if matches!(id, 272..=275 | 291) { return 5; }
    if matches!(id, 256..=258 | 267 | 292) { return 14; }
    if matches!(id, 276..=279 | 293) { return 10; }
    if matches!(id, 283..=286 | 294) { return 22; }
    1
}
fn eligible(id: i16, power: u32) -> Vec<(Rule, u8)> {
    RULES
        .iter()
        .filter(|r| supports(id, r.kind))
        .filter_map(|rule| {
            let level = (1..=rule.max).rev().find(|&lvl| {
                let minimum = u32::from(rule.min) + u32::from(lvl - 1) * u32::from(rule.step);
                // Vanilla enchantments have a finite effective-power band.
                (minimum..=minimum + 15).contains(&power)
            })?;
            Some((*rule, level))
        })
        .collect()
}
fn choose(rng: &mut StdRng, choices: &[(Rule, u8)]) -> Option<(Rule, u8)> {
    let total: u32 = choices.iter().map(|(rule, _)| u32::from(rule.weight)).sum();
    if total == 0 { return None; }
    let mut ticket = rng.gen_range(0..total);
    choices.iter().find_map(|&(rule, lvl)| {
        if ticket < u32::from(rule.weight) { Some((rule, lvl)) }
        else { ticket -= u32::from(rule.weight); None }
    })
}
pub fn can_enchant(item: Stack) -> bool {
    if !items_1710::enchantable_in_table(item.id) || item.count != 1 || !item.is_valid() {
        return false;
    }
    let Some(handle) = item.nbt else { return true };
    match item_nbt::value(handle) {
        Ok(Value::Compound(fields)) => !fields.contains_key("ench") && !fields.contains_key("StoredEnchantments"),
        _ => false,
    }
}

/// Applies a validated server offer; returns None for already enchanted,
/// ineligible or invalid inputs without modifying the inventory or XP.
pub fn apply(item: Stack, cost: u8, seed: u64) -> io::Result<Option<Stack>> {
    if cost == 0 || !can_enchant(item) {
        return Ok(None);
    }
    let mut rng = StdRng::seed_from_u64(seed ^ (item.id as u16 as u64) << 32 ^ u64::from(cost));
    let factor = enchantability(item.id);
    let width = u32::from(factor / 4 + 1);
    let base = u32::from(cost) + 1 + rng.gen_range(0..width) + rng.gen_range(0..width);
    let scale = 1.0 + (rng.gen::<f64>() + rng.gen::<f64>() - 1.0) * 0.15;
    let mut power = ((f64::from(base) * scale).round() as u32).max(1);
    let mut choices = eligible(item.id, power);
    let mut chosen = Vec::new();
    if let Some(first) = choose(&mut rng, &choices) {
        chosen.push(first);
    } else {
        return Ok(None);
    }
    if item.id != 340 {
        while power > 0 && chosen.len() < 4 && rng.gen_range(0..50) <= power {
            choices.retain(|(rule, _)| !chosen.iter().any(|(other, _)| rule.id == other.id || conflicting(rule.id, other.id)));
            let Some(entry) = choose(&mut rng, &choices) else { break };
            chosen.push(entry);
            power /= 2;
        }
    }
    let mut root = match item.nbt {
        Some(id) => match item_nbt::value(id)? {
            Value::Compound(value) => value,
            _ => return Ok(None),
        },
        None => HashMap::new(),
    };
    let enchantments = chosen.into_iter().map(|(rule, lvl)| {
        Value::Compound(HashMap::from([
            ("id".into(), Value::Short(rule.id)),
            ("lvl".into(), Value::Short(i16::from(lvl))),
        ]))
    }).collect();
    let book = item.id == 340;
    root.insert(if book { "StoredEnchantments" } else { "ench" }.to_string(), Value::List(enchantments));
    let nbt = item_nbt::from_value(&Value::Compound(root))?;
    Ok(Some(Stack { id: if book { 403 } else { item.id }, nbt: Some(nbt), ..item }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn enchanter_preserves_existing_unknown_tag_and_uses_correct_nbt_key() {
        let tag = item_nbt::from_value(&Value::Compound(HashMap::from([
            ("Mystery".to_string(), Value::String("keep me".into())),
        ]))).unwrap();
        let input = Stack { id: 257, count: 1, damage: 20, nbt: Some(tag) };
        let enchanted = apply(input, 30, 42).unwrap().unwrap();
        assert_eq!(enchanted.damage, 20);
        assert!(matches!(item_nbt::value(enchanted.nbt.unwrap()).unwrap(), Value::Compound(map)
            if map.contains_key("ench") && map.get("Mystery") == Some(&Value::String("keep me".into()))));
        assert!(!can_enchant(enchanted));
        assert!(apply(enchanted, 30, 42).unwrap().is_none());
        let book = apply(Stack { id: 340, count: 1, damage: 0, nbt: None }, 30, 42).unwrap().unwrap();
        assert_eq!(book.id, 403);
        assert!(matches!(item_nbt::value(book.nbt.unwrap()).unwrap(), Value::Compound(map)
            if map.contains_key("StoredEnchantments")));
    }
    #[test]
    fn incompatible_enchantments_are_never_selected_together() {
        for id in [257, 276, 301, 261] {
            for seed in 0..100 {
                let out = apply(Stack { id, count: 1, damage: 0, nbt: None }, 30, seed).unwrap();
                if let Some(stack) = out {
                    let Value::Compound(root) = item_nbt::value(stack.nbt.unwrap()).unwrap() else { panic!("compound") };
                    let Value::List(tags) = &root["ench"] else { panic!("enchant list") };
                    let ids: Vec<i16> = tags.iter().map(|tag| {
                        let Value::Compound(map) = tag else { panic!("enchant entry") };
                        let Value::Short(id) = map["id"] else { panic!("id") };
                        id
                    }).collect();
                    for (i, &a) in ids.iter().enumerate() {
                        for &b in &ids[i + 1..] { assert!(!conflicting(a, b)); }
                    }
                }
            }
        }
    }
}
