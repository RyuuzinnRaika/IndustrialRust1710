//! Vanilla Minecraft Java 1.7.10 inventory item identity and numeric metadata.
//!
//! Block IDs and item IDs are different registries. Deliberately reject 1.8+
//! additions and arbitrary positive 12-bit IDs; this is a vanilla-only server.
//! This does not yet implement per-item NBT (display/enchant/attributes).
//! Unknown/modded items must not be silently rewritten as vanilla stacks.

/// The IDs registered for vanilla 1.7.10 block items, regular items, horse
/// items and music discs. ID 0 represents air and never forms a nonempty Stack.
pub fn exists(id: i16) -> bool {
    matches!(id, 1..=175 | 256..=408 | 417..=422 | 2256..=2267)
}

/// Item count cap, not the default inventory slot size. All tools, armor,
/// buckets containing fluids, potion bottles and unique books are unstackable.
pub fn max_count(id: i16) -> u8 {
    match id {
        323 | 325 | 332 | 344 | 368 | 381 => 16,
        256..=259
        | 261
        | 267..=279
        | 282..=286
        | 290..=294
        | 298..=317
        | 326..=329
        | 333
        | 335
        | 342
        | 343
        | 346
        | 354
        | 355
        | 358
        | 359
        | 373
        | 386
        | 387
        | 398
        | 403
        | 407
        | 408
        | 417..=419
        | 2256..=2267 => 1,
        _ => 64,
    }
}

/// Vanilla material-specific damage limits. A fully worn item may arrive from
/// creative or disk, but live wear logic is responsible for breaking it.
pub fn max_durability(id: i16) -> Option<i16> {
    match id {
        256..=258 | 267 | 292 => Some(250), // iron
        259 | 346 => Some(64),              // flint & steel, fishing rod
        261 => Some(384),                   // bow
        268..=271 | 290 => Some(59),        // wood
        272..=275 | 291 => Some(131),       // stone
        276..=279 | 293 => Some(1561),      // diamond
        283..=286 | 294 => Some(32),        // gold
        298 => Some(55),                    // leather helmet
        299 => Some(80),
        300 => Some(75),
        301 => Some(65),
        302 | 306 => Some(165), // chain/iron helmet
        303 | 307 => Some(240), // chain/iron chestplate
        304 | 308 => Some(225), // chain/iron leggings
        305 | 309 => Some(195), // chain/iron boots
        310 => Some(363),       // diamond armor
        311 => Some(528),
        312 => Some(495),
        313 => Some(429),
        314 => Some(77), // golden armor
        315 => Some(112),
        316 => Some(105),
        317 => Some(91),
        359 => Some(238), // shears
        398 => Some(25),  // carrot on a stick
        _ => None,
    }
}

/// Item metadata is *not* synonymous with durability. Special metadata values
/// such as splash potion bitflags and spawn-egg entity IDs must survive intact.
pub fn valid_damage(id: i16, damage: i16) -> bool {
    if damage < 0 {
        return false;
    }
    if id <= 175 {
        return damage <= 15; // block variant/direction bits
    }
    if let Some(max) = max_durability(id) {
        return damage <= max;
    }
    match id {
        263 | 322 | 350 => damage <= 1, // charcoal, golden apple, cooked fish
        349 => damage <= 3,             // raw fish variants
        351 => damage <= 15,            // dye / ink variants
        358 | 373 | 383 => true,        // filled map, potion bitflags, spawn eggs
        397 => damage <= 4,             // heads, including player head
        _ => damage == 0,
    }
}

/// Single source of truth for loading from disk, creative inventory claims and
/// inventory transfers. Checked before a Stack can be persisted or broadcast.
pub fn valid_stack(id: i16, count: u8, damage: i16) -> bool {
    exists(id) && (1..=max_count(id)).contains(&count) && valid_damage(id, damage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vanilla_id_bounds_reject_unregistered_and_modern_items() {
        for id in [1, 5, 175, 256, 373, 408, 417, 422, 2256, 2267] {
            assert!(exists(id), "missing vanilla id {id}");
        }
        for id in [-1, 0, 176, 255, 409, 416, 423, 2255, 2268, 4095] {
            assert!(!exists(id), "non-1.7.10 item accepted: {id}");
        }
    }

    #[test]
    fn stack_limits_prevent_merging_unstackable_items() {
        for id in [259, 261, 282, 310, 326, 355, 358, 373, 398, 403, 419, 2256] {
            assert_eq!(max_count(id), 1, "must not stack: {id}");
            assert!(!valid_stack(id, 2, 0));
        }
        for id in [323, 325, 332, 344, 368, 381] {
            assert_eq!(max_count(id), 16);
            assert!(valid_stack(id, 16, 0));
            assert!(!valid_stack(id, 17, 0));
        }
        assert_eq!(max_count(264), 64);
        assert!(!valid_stack(264, 0, 0));
        assert!(!valid_stack(264, 65, 0));
    }

    #[test]
    fn distinguish_variant_damage_and_durability() {
        for (id, damage) in [
            (35, 15),
            (263, 1),
            (349, 3),
            (351, 15),
            (373, 0x4000 | 8194),
            (383, 120),
            (397, 3),
            (310, 363),
            (276, 1561),
            (358, 2048),
        ] {
            assert!(valid_stack(id, 1, damage), "{id}:{damage}");
        }
        for (id, damage) in [
            (35, 16),
            (264, 1),
            (350, 2),
            (349, 4),
            (351, 16),
            (397, 5),
            (310, 364),
            (276, 1562),
            (373, -1),
        ] {
            assert!(!valid_stack(id, 1, damage), "{id}:{damage}");
        }
    }
}
