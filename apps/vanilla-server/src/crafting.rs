//! Small, server-authoritative subset of Minecraft Java 1.7.10 crafting.
//! Output is *derived* from ingredient slots, never stored or client supplied.
use crate::inventory::Stack;

fn product(id: i16, count: u8, damage: i16) -> Option<Stack> {
    Some(Stack { id, count, damage })
}
fn plank(item: Option<Stack>) -> bool {
    item.is_some_and(|s| s.id == 5)
}
fn stick(item: Option<Stack>) -> bool {
    item.is_some_and(|s| s.id == 280)
}
fn same_material(item: Option<Stack>, id: i16) -> bool {
    item.is_some_and(|s| s.id == id)
}

/// Match recipes after trimming empty border rows/columns, which allows a
/// shape to be positioned anywhere on the 2x2 or 3x3 crafting grid.
pub fn result(grid: &[Option<Stack>], width: usize) -> Option<Stack> {
    if !(width == 2 || width == 3) || grid.len() != width * width {
        return None;
    }
    let filled: Vec<_> = grid
        .iter()
        .enumerate()
        .filter(|(_, item)| item.is_some())
        .map(|(i, _)| (i % width, i / width))
        .collect();
    if filled.is_empty() {
        return None;
    }
    let min_x = filled.iter().map(|p| p.0).min()?;
    let max_x = filled.iter().map(|p| p.0).max()?;
    let min_y = filled.iter().map(|p| p.1).min()?;
    let max_y = filled.iter().map(|p| p.1).max()?;
    let w = max_x - min_x + 1;
    let h = max_y - min_y + 1;
    let at = |x: usize, y: usize| grid[(min_y + y) * width + min_x + x];
    if w == 1 && h == 1 {
        let item = at(0, 0)?;
        if item.id == 17 {
            return product(5, 4, item.damage & 3);
        }
        if item.id == 162 {
            return product(5, 4, 4 + (item.damage & 1));
        }
    }
    if w == 1 && h == 2 && plank(at(0, 0)) && plank(at(0, 1)) {
        return product(280, 4, 0); // sticks
    }
    if w == 1 && h == 2 && at(0, 0).is_some_and(|item| item.id == 263) && stick(at(0, 1)) {
        return product(50, 4, 0); // four torches from coal/charcoal and a stick
    }
    if w == 1 && h == 2 && same_material(at(0, 0), 331) && stick(at(0, 1)) {
        return product(76, 1, 0); // active redstone torch
    }
    if w == 1 && h == 2 && stick(at(0, 0)) && same_material(at(0, 1), 4) {
        return product(69, 1, 0); // lever
    }
    if w == 3 && h == 1 && (0..3).all(|x| same_material(at(x, 0), 296)) {
        return product(297, 1, 0); // bread from wheat
    }
    if w == 2 && h == 2 && (0..2).all(|y| (0..2).all(|x| plank(at(x, y)))) {
        return product(58, 1, 0); // crafting table
    }
    if w == 3
        && h == 3
        && (0..3).all(|y| {
            (0..3).all(|x| {
                if x == 1 && y == 1 {
                    at(x, y).is_none()
                } else {
                    plank(at(x, y))
                }
            })
        })
    {
        return product(54, 1, 0); // chest
    }
    if w == 3
        && h == 3
        && (0..3).all(|y| {
            (0..3).all(|x| {
                if x == 1 && y == 1 {
                    at(x, y).is_none()
                } else {
                    same_material(at(x, y), 4)
                }
            })
        })
    {
        return product(61, 1, 0); // furnace
    }
    if width == 3 {
        // All equipment patterns use exact shaped layouts in the trimmed grid.
        if w == 1
            && h == 3
            && same_material(at(0, 0), 318)
            && stick(at(0, 1))
            && same_material(at(0, 2), 288)
        {
            return product(262, 4, 0); // flint + stick + feather = four arrows
        }
        if w == 3 && h == 3 {
            for mirror in 0..2 {
                let (outer, inner) = if mirror == 0 { (2, 1) } else { (0, 1) };
                if stick(at(inner, 1))
                    && stick(at(outer, 0))
                    && stick(at(outer, 2))
                    && (0..3).all(|y| same_material(at(if mirror == 0 { 0 } else { 2 }, y), 287))
                    && at(inner, 0).is_none()
                    && at(inner, 2).is_none()
                {
                    return product(261, 1, 0); // bow
                }
            }
        }
        for (material, helmet, chest, leggings, boots) in [
            (334, 298, 299, 300, 301), // leather
            (265, 306, 307, 308, 309), // iron
            (266, 314, 315, 316, 317), // gold
            (264, 310, 311, 312, 313), // diamond
        ] {
            let m = |x, y| same_material(at(x, y), material);
            let e = |x, y| at(x, y).is_none();
            if w == 3 && h == 2 && (0..3).all(|x| m(x, 0)) && m(0, 1) && e(1, 1) && m(2, 1) {
                return product(helmet, 1, 0);
            }
            if w == 3
                && h == 3
                && m(0, 0)
                && e(1, 0)
                && m(2, 0)
                && (0..3).all(|x| m(x, 1) && m(x, 2))
            {
                return product(chest, 1, 0);
            }
            if w == 3
                && h == 3
                && (0..3).all(|x| m(x, 0))
                && m(0, 1)
                && e(1, 1)
                && m(2, 1)
                && m(0, 2)
                && e(1, 2)
                && m(2, 2)
            {
                return product(leggings, 1, 0);
            }
            if w == 3 && h == 2 && m(0, 0) && e(1, 0) && m(2, 0) && m(0, 1) && e(1, 1) && m(2, 1) {
                return product(boots, 1, 0);
            }
        }
    }
    if width == 3 {
        for (material, pickaxe, axe, shovel, sword, hoe) in [
            (5, 270, 271, 269, 268, 290),
            (4, 274, 275, 273, 272, 291),
            (265, 257, 258, 256, 267, 292),
            (266, 285, 286, 284, 283, 294),
            (264, 278, 279, 277, 276, 293),
        ] {
            let m = |x, y| same_material(at(x, y), material);
            let s = |x, y| stick(at(x, y));
            let blank = |x, y| at(x, y).is_none();
            if w == 3
                && h == 3
                && (0..3).all(|x| m(x, 0))
                && s(1, 1)
                && s(1, 2)
                && blank(0, 1)
                && blank(2, 1)
                && blank(0, 2)
                && blank(2, 2)
            {
                return product(pickaxe, 1, 0);
            }
            if w == 1 && h == 3 && m(0, 0) && s(0, 1) && s(0, 2) {
                return product(shovel, 1, 0);
            }
            if w == 1 && h == 3 && m(0, 0) && m(0, 1) && s(0, 2) {
                return product(sword, 1, 0);
            }
            if w == 2 && h == 3 {
                for flip in 0..2 {
                    if m(0, 0)
                        && m(1, 0)
                        && m(flip, 1)
                        && s(1 - flip, 1)
                        && blank(flip, 2)
                        && s(1 - flip, 2)
                    {
                        return product(axe, 1, 0);
                    }
                    if m(0, 0)
                        && m(1, 0)
                        && blank(flip, 1)
                        && s(1 - flip, 1)
                        && blank(flip, 2)
                        && s(1 - flip, 2)
                    {
                        return product(hoe, 1, 0);
                    }
                }
            }
        }
    }
    additional_result(grid, width)
}

/// Additional vanilla 1.7.10 recipes, matched against a trimmed 2x2/3x3 grid.
/// -1 means any plank metadata, -2 any wool color and -3 any coal metadata.
fn additional_result(grid: &[Option<Stack>], width: usize) -> Option<Stack> {
    let occupied: Vec<_> = grid
        .iter()
        .enumerate()
        .filter(|(_, v)| v.is_some())
        .map(|(n, _)| (n % width, n / width))
        .collect();
    let left = occupied.iter().map(|p| p.0).min()?;
    let top = occupied.iter().map(|p| p.1).min()?;
    let w = occupied.iter().map(|p| p.0).max()? - left + 1;
    let h = occupied.iter().map(|p| p.1).max()? - top + 1;
    let at = |x: usize, y: usize| grid[(top + y) * width + left + x];
    let matches = |rows: &[&[i16]]| -> bool {
        if rows.len() != h || rows.iter().any(|row| row.len() != w) {
            return false;
        }
        rows.iter().enumerate().all(|(y, row)| {
            row.iter().enumerate().all(|(x, id)| match (id, at(x, y)) {
                (0, None) => true,
                (&-1, Some(stack)) => stack.id == 5,
                (&-2, Some(stack)) => stack.id == 35,
                (&-3, Some(stack)) => stack.id == 263,
                (id, Some(stack)) => *id == stack.id,
                _ => false,
            })
        })
    };
    let shapeless = |ingredients: &[i16]| -> bool {
        if ingredients.len() != occupied.len() {
            return false;
        }
        let mut remaining = ingredients.to_vec();
        for item in grid.iter().flatten() {
            let Some(index) = remaining.iter().position(|id| match id {
                -1 => item.id == 5,
                -2 => item.id == 35,
                -3 => item.id == 263,
                id => *id == item.id,
            }) else {
                return false;
            };
            remaining.remove(index);
        }
        remaining.is_empty()
    };
    if w == 1 && h == 1 {
        let single = at(0, 0)?;
        for (block, ingredient, count) in [
            (41, 266, 9),
            (42, 265, 9),
            (57, 264, 9),
            (22, 351, 9),
            (133, 388, 9),
            (173, 263, 9),
            (170, 296, 9),
        ] {
            if single.id == block {
                return product(ingredient, count, if block == 22 { 4 } else { 0 });
            }
        }
        if single.id == 338 {
            return product(353, 1, 0);
        } // sugar cane to sugar
    }
    if shapeless(&[318, 265]) {
        return product(259, 1, 0);
    } // flint and steel
    if matches(&[&[338, 338, 338]]) {
        return product(339, 3, 0);
    } // paper
    if shapeless(&[339, 339, 339, 334]) {
        return product(340, 1, 0);
    }
    if matches(&[&[318], &[280], &[288]]) {
        return product(262, 4, 0);
    }
    if matches(&[&[265, 265, 265], &[265, 265, 265]]) {
        return product(101, 16, 0);
    }
    if matches(&[&[20, 20, 20], &[20, 20, 20]]) {
        return product(102, 16, 0);
    }
    if matches(&[&[-1, -1, -1]]) {
        let variant = at(0, 0)?.damage;
        if (0..3).all(|x| at(x, 0).is_some_and(|item| item.damage == variant)) {
            return product(126, 6, variant & 7);
        }
    }
    if matches(&[&[4, 4, 4]]) {
        return product(44, 6, 3);
    }
    if matches(&[&[1, 1, 1]]) {
        return product(44, 6, 0);
    }
    if matches(&[&[45, 45, 45]]) {
        return product(44, 6, 4);
    }
    if matches(&[&[155, 155, 155]]) {
        return product(44, 6, 7);
    }
    if matches(&[&[-1, -1, -1], &[-1, -1, -1]]) {
        return product(96, 2, 0);
    }
    if matches(&[&[-1, -1]]) {
        return product(72, 1, 0);
    }
    if matches(&[&[265, 265], &[265, 265], &[265, 265]]) {
        return product(330, 1, 0);
    }
    if matches(&[&[-1, -1], &[-1, -1], &[-1, -1]]) {
        return product(324, 1, 0);
    }
    if matches(&[&[-1, -1, -1], &[-1, -1, -1], &[0, 280, 0]]) {
        return product(323, 3, 0);
    }
    if matches(&[&[280, 0, 280], &[280, 280, 280], &[280, 0, 280]]) {
        return product(65, 3, 0);
    }
    // Before Java 1.8, wooden fences consume six sticks (not planks).
    if matches(&[&[280, 280, 280], &[280, 280, 280]]) {
        return product(85, 2, 0);
    }
    if matches(&[&[-1, 0, -1], &[-1, -1, -1]]) {
        return product(333, 1, 0);
    }
    if matches(&[&[-2, -2, -2], &[-1, -1, -1]]) {
        return product(355, 1, 0);
    }
    if matches(&[&[265, 0, 265], &[0, 265, 0]]) {
        return product(325, 1, 0);
    }

    if matches(&[&[0, 265], &[265, 0]]) || matches(&[&[265, 0], &[0, 265]]) {
        return product(359, 1, 0);
    }
    // Storage blocks must debit all nine source items.
    if matches(&[&[331, 331, 331], &[331, 331, 331], &[331, 331, 331]]) {
        return product(152, 1, 0);
    }
    if matches(&[&[351, 351, 351], &[351, 351, 351], &[351, 351, 351]])
        && grid.iter().flatten().all(|item| item.damage == 4)
    {
        return product(22, 1, 0);
    }

    if matches(&[&[296, 296, 296], &[296, 296, 296], &[296, 296, 296]]) {
        return product(170, 1, 0);
    }
    if matches(&[&[263, 263, 263], &[263, 263, 263], &[263, 263, 263]])
        && grid.iter().flatten().all(|item| item.damage == 0)
    {
        return product(173, 1, 0);
    }
    for (item, block) in [(265, 42), (266, 41), (264, 57), (388, 133)] {
        if matches(&[
            &[item, item, item],
            &[item, item, item],
            &[item, item, item],
        ]) {
            return product(block, 1, 0);
        }
    }
    for (material, helmet, chestplate, leggings, boots) in [
        (334, 298, 299, 300, 301),
        (265, 306, 307, 308, 309),
        (264, 310, 311, 312, 313),
        (266, 314, 315, 316, 317),
    ] {
        if matches(&[&[material, material, material], &[material, 0, material]]) {
            return product(helmet, 1, 0);
        }
        if matches(&[
            &[material, 0, material],
            &[material, material, material],
            &[material, material, material],
        ]) {
            return product(chestplate, 1, 0);
        }
        if matches(&[
            &[material, material, material],
            &[material, 0, material],
            &[material, 0, material],
        ]) {
            return product(leggings, 1, 0);
        }
        if matches(&[&[material, 0, material], &[material, 0, material]]) {
            return product(boots, 1, 0);
        }
    }
    if matches(&[&[0, 280, 287], &[280, 0, 287], &[0, 280, 287]]) {
        return product(261, 1, 0);
    }
    // Six gold ingots, one stick and one redstone dust: powered rails.
    if matches(&[&[266, 0, 266], &[266, 280, 266], &[266, 331, 266]]) {
        return product(27, 6, 0);
    }
    if matches(&[&[265, 0, 265], &[265, 280, 265], &[265, 0, 265]]) {
        return product(66, 16, 0);
    }
    if matches(&[&[76, 331, 76], &[1, 1, 1]]) {
        return product(356, 1, 0);
    }
    if matches(&[&[1, 1], &[1, 1]]) {
        return product(98, 4, 0);
    }
    // Three milk buckets, sugar, an egg and three wheat produce a cake.
    // Inventory::spend_ingredients returns the empty buckets atomically.
    if matches(&[&[335, 335, 335], &[353, 344, 353], &[296, 296, 296]]) {
        return product(354, 1, 0);
    }
    // Workstations, containers and redstone machines from Java 1.7.10.
    if matches(&[&[0, 369, 0], &[4, 4, 4]]) {
        return product(117, 1, 0); // brewing stand
    }
    if matches(&[&[0, 340, 0], &[264, 49, 264], &[49, 49, 49]]) {
        return product(116, 1, 0); // enchanting table
    }
    if matches(&[&[42, 42, 42], &[0, 265, 0], &[265, 265, 265]]) {
        return product(145, 1, 0); // anvil
    }
    if matches(&[&[265, 0, 265], &[265, 54, 265], &[0, 265, 0]]) {
        return product(154, 1, 0); // hopper
    }
    if matches(&[&[265, 0, 265], &[265, 0, 265], &[265, 265, 265]]) {
        return product(118, 1, 0); // cauldron
    }
    if matches(&[&[4, 4, 4], &[4, 261, 4], &[4, 331, 4]]) {
        return product(23, 1, 0); // dispenser
    }
    if matches(&[&[4, 4, 4], &[4, 0, 4], &[4, 331, 4]]) {
        return product(158, 1, 0); // dropper
    }
    if matches(&[&[-1, -1, -1], &[4, 265, 4], &[4, 331, 4]]) {
        return product(33, 1, 0); // piston
    }
    if shapeless(&[33, 341]) {
        return product(29, 1, 0); // sticky piston
    }
    if matches(&[&[-1, -1, -1], &[340, 340, 340], &[-1, -1, -1]]) {
        return product(47, 1, 0); // bookshelf
    }
    if matches(&[&[-1, -1, -1], &[-1, 331, -1], &[-1, -1, -1]]) {
        return product(25, 1, 0); // note block
    }
    if matches(&[&[-1, -1, -1], &[-1, 264, -1], &[-1, -1, -1]]) {
        return product(84, 1, 0); // jukebox
    }
    if matches(&[&[0, 265, 0], &[265, 331, 265], &[0, 265, 0]]) {
        return product(345, 1, 0); // compass
    }
    if matches(&[&[0, 266, 0], &[266, 331, 266], &[0, 266, 0]]) {
        return product(347, 1, 0); // clock
    }
    if matches(&[&[0, 76, 0], &[76, 406, 76], &[1, 1, 1]]) {
        return product(404, 1, 0); // comparator
    }
    if matches(&[&[265, 0, 265], &[265, 70, 265], &[265, 331, 265]]) {
        return product(28, 6, 0); // detector rails
    }
    if matches(&[&[265, 280, 265], &[265, 76, 265], &[265, 280, 265]]) {
        return product(157, 6, 0); // activator rails
    }
    if matches(&[&[371, 371, 371], &[371, 391, 371], &[371, 371, 371]]) {
        return product(396, 1, 0); // golden carrot
    }
    if matches(&[&[49, 49, 49], &[49, 381, 49], &[49, 49, 49]]) {
        return product(130, 1, 0); // ender chest
    }
    if matches(&[&[280, -1, 280], &[280, -1, 280]]) {
        return product(107, 1, 0); // fence gate
    }
    if matches(&[&[265, 0, 265], &[265, 265, 265]]) {
        return product(328, 1, 0); // minecart
    }
    if matches(&[&[20, 0, 20], &[0, 20, 0]]) {
        return product(374, 3, 0); // glass bottles
    }
    if shapeless(&[54, 131]) {
        return product(146, 1, 0); // trapped chest
    }
    if shapeless(&[368, 377]) {
        return product(381, 1, 0); // eye of ender
    }
    // Common 1.7.10 survival recipes (pre-1.8 output counts and IDs).
    if w == 1 && h == 1 {
        let single = at(0, 0)?;
        match single.id {
            5 => return product(143, 1, 0), // wooden button (any plank)
            1 if single.damage == 0 => return product(77, 1, 0), // stone button
            352 => return product(351, 3, 15), // bone meal
            369 => return product(377, 2, 0), // blaze powder
            _ => {}
        }
    }
    if w == 2 && h == 1 && (0..2).all(|x| at(x, 0).is_some_and(|s| s.id == 1 && s.damage == 0)) {
        return product(70, 1, 0); // stone pressure plate
    }
    if matches(&[&[266, 266]]) {
        return product(147, 1, 0); // light weighted pressure plate
    }
    if matches(&[&[265, 265]]) {
        return product(148, 1, 0); // heavy weighted pressure plate
    }
    if matches(&[&[-1, 0, -1], &[0, -1, 0]]) {
        return product(281, 4, 0); // four wooden bowls
    }
    if matches(&[&[280, 0, 280], &[280, 280, 280], &[280, 0, 280]]) {
        return product(65, 3, 0); // three ladders
    }
    if matches(&[&[-1, -1, -1], &[-1, -1, -1], &[0, 280, 0]]) {
        return product(323, 3, 0); // three signs in Java 1.7.10
    }
    if matches(&[&[265], &[280], &[-1]]) {
        return product(131, 2, 0); // tripwire hooks
    }
    if matches(&[&[86], &[50]]) {
        return product(91, 1, 0); // jack o'lantern
    }
    if matches(&[&[336, 336], &[336, 336]]) {
        return product(45, 1, 0); // brick block
    }
    if matches(&[&[337, 337], &[337, 337]]) {
        return product(82, 1, 0); // clay block
    }
    if matches(&[&[332, 332], &[332, 332]]) {
        return product(80, 1, 0); // snow block
    }
    if matches(&[&[348, 348], &[348, 348]]) {
        return product(89, 1, 0); // glowstone
    }
    if matches(&[&[405, 405], &[405, 405]]) {
        return product(112, 1, 0); // nether brick block
    }
    if matches(&[&[406, 406], &[406, 406]]) {
        return product(155, 1, 0); // quartz block
    }
    if matches(&[&[280, 280, 280], &[280, 334, 280], &[280, 280, 280]]) {
        return product(389, 1, 0); // item frame
    }
    if matches(&[&[280, 280, 280], &[280, -2, 280], &[280, 280, 280]]) {
        return product(321, 1, 0); // painting (any wool)
    }
    if matches(&[&[0, 0, 280], &[0, 280, 287], &[280, 0, 287]]) {
        return product(346, 1, 0); // fishing rod
    }
    if matches(&[&[289, 12, 289], &[12, 289, 12], &[289, 12, 289]]) {
        return product(46, 1, 0); // TNT
    }
    if matches(&[&[266, 266, 266], &[266, 260, 266], &[266, 266, 266]]) {
        return product(322, 1, 0); // golden apple
    }
    if matches(&[&[41, 41, 41], &[41, 260, 41], &[41, 41, 41]]) {
        return product(322, 1, 1); // enchanted golden apple (craftable in 1.7.10)
    }
    if matches(&[&[371, 371, 371], &[371, 360, 371], &[371, 371, 371]]) {
        return product(382, 1, 0); // glistering melon
    }
    if shapeless(&[39, 40, 281]) {
        return product(282, 1, 0); // mushroom stew, bowl returned by eating
    }
    if shapeless(&[375, 353, 39]) {
        return product(376, 1, 0); // fermented spider eye
    }
    if shapeless(&[341, 377]) {
        return product(378, 1, 0); // magma cream
    }
    for (machine, item) in [(54, 342), (61, 343), (46, 407), (154, 408)] {
        if shapeless(&[328, machine]) {
            return product(item, 1, 0); // chest/furnace/TNT/hopper minecart
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn expanded_1710_survival_recipes_are_shaped_and_bounded() {
        let make = |ids: [i16; 9]| ids.map(|id| if id == 0 { None } else { product(id, 1, 0) });
        let cases = [
            ([280, 0, 280, 280, 280, 280, 280, 0, 280], (65, 3, 0)),
            ([5, 5, 5, 5, 5, 5, 0, 280, 0], (323, 3, 0)),
            ([280, 280, 280, 280, 334, 280, 280, 280, 280], (389, 1, 0)),
            ([266, 266, 266, 266, 260, 266, 266, 266, 266], (322, 1, 0)),
            ([41, 41, 41, 41, 260, 41, 41, 41, 41], (322, 1, 1)),
            ([289, 12, 289, 12, 289, 12, 289, 12, 289], (46, 1, 0)),
            ([0, 0, 280, 0, 280, 287, 280, 0, 287], (346, 1, 0)),
            ([0, 265, 0, 0, 280, 0, 0, 5, 0], (131, 2, 0)),
        ];
        for (inputs, (id, count, damage)) in cases {
            assert_eq!(result(&make(inputs), 3), product(id, count, damage));
        }
        // Missing just one ingredient cannot accidentally produce a free item.
        assert_eq!(result(&make([280, 0, 280, 280, 0, 280, 280, 0, 280]), 3), None);
        // 1.7.10 buttons require real stone, not a granite metadata variant.
        assert_eq!(result(&[product(1, 1, 1), None, None, None], 2), None);
    }

    #[test]
    fn expanded_1710_shapeless_and_metadata_recipes() {
        let make = |ids: [i16; 4]| ids.map(|id| if id == 0 { None } else { product(id, 1, 0) });
        for (ids, output) in [
            ([39, 40, 281, 0], (282, 1, 0)),
            ([375, 353, 39, 0], (376, 1, 0)),
            ([341, 377, 0, 0], (378, 1, 0)),
            ([328, 154, 0, 0], (408, 1, 0)),
            ([337, 337, 337, 337], (82, 1, 0)),
            ([406, 406, 406, 406], (155, 1, 0)),
        ] {
            assert_eq!(result(&make(ids), 2), product(output.0, output.1, output.2));
        }
        assert_eq!(result(&make([352, 0, 0, 0]), 2), product(351, 3, 15));
        assert_eq!(result(&make([369, 0, 0, 0]), 2), product(377, 2, 0));
        assert_eq!(result(&make([1, 1, 0, 0]), 2), product(70, 1, 0));
        assert_eq!(result(&make([266, 266, 0, 0]), 2), product(147, 1, 0));
    }

    #[test]
    fn legacy_workstations_and_bucket_cake_have_bounded_recipes() {
        let make = |ids: [i16; 9]| {
            ids.map(|id| {
                (id != 0).then_some(Stack {
                    id,
                    count: 1,
                    damage: 0,
                })
            })
        };
        assert_eq!(
            result(&make([0, 369, 0, 4, 4, 4, 0, 0, 0]), 3),
            product(117, 1, 0)
        );
        assert_eq!(
            result(&make([0, 340, 0, 264, 49, 264, 49, 49, 49]), 3),
            product(116, 1, 0)
        );
        assert_eq!(
            result(&make([42, 42, 42, 0, 265, 0, 265, 265, 265]), 3),
            product(145, 1, 0)
        );
        assert_eq!(
            result(&make([335, 335, 335, 353, 344, 353, 296, 296, 296]), 3),
            product(354, 1, 0)
        );
        // The 1.8-style fence made from four planks and two sticks is not a 1.7.10 recipe.
        assert_eq!(result(&make([5, 280, 5, 5, 280, 5, 0, 0, 0]), 3), None);
    }
    use super::*;
    fn item(id: i16) -> Option<Stack> {
        product(id, 1, 0)
    }
    #[test]
    fn shears_accept_both_mirrored_diagonals_without_extra_material() {
        assert_eq!(
            result(&[item(265), None, None, item(265)], 2),
            product(359, 1, 0)
        );
        assert_eq!(
            result(&[None, item(265), item(265), None], 2),
            product(359, 1, 0)
        );
        assert_eq!(result(&[item(265), None, None, None], 2), None);
    }
    #[test]
    fn armor_bow_and_arrow_require_exact_legacy_patterns() {
        assert_eq!(
            result(
                &[
                    item(265),
                    item(265),
                    item(265),
                    item(265),
                    None,
                    item(265),
                    None,
                    None,
                    None
                ],
                3
            ),
            product(306, 1, 0)
        ); // iron helmet
        assert_eq!(
            result(
                &[
                    item(334),
                    None,
                    item(334),
                    item(334),
                    None,
                    item(334),
                    None,
                    None,
                    None
                ],
                3
            ),
            product(301, 1, 0)
        ); // leather boots
        assert_eq!(
            result(
                &[
                    item(287),
                    None,
                    item(280),
                    item(287),
                    item(280),
                    None,
                    item(287),
                    None,
                    item(280)
                ],
                3
            ),
            product(261, 1, 0)
        );
        assert_eq!(
            result(
                &[
                    None,
                    item(318),
                    None,
                    None,
                    item(280),
                    None,
                    None,
                    item(288),
                    None
                ],
                3
            ),
            product(262, 4, 0)
        );
        assert_eq!(
            result(
                &[item(265), None, None, None, None, None, None, None, None],
                3
            ),
            None
        );
    }
    #[test]
    fn early_game_recipes_are_bounded_and_shape_checked() {
        assert_eq!(result(&[None, item(17), None, None], 2), product(5, 4, 0));
        assert_eq!(
            result(&[item(5), item(5), item(5), item(5)], 2),
            product(58, 1, 0)
        );
        assert_eq!(
            result(&[item(5), None, item(5), None], 2),
            product(280, 4, 0)
        );
        assert_eq!(
            result(&[item(5), item(5), None, None], 2),
            product(72, 1, 0)
        );
        assert_eq!(
            result(
                &[
                    item(5),
                    item(5),
                    item(5),
                    None,
                    item(280),
                    None,
                    None,
                    item(280),
                    None
                ],
                3
            ),
            product(270, 1, 0)
        );
        assert_eq!(
            result(
                &[
                    item(4),
                    item(4),
                    item(4),
                    item(4),
                    None,
                    item(4),
                    item(4),
                    item(4),
                    item(4)
                ],
                3
            ),
            product(61, 1, 0)
        );
        assert_eq!(
            result(
                &[
                    item(5),
                    item(5),
                    item(5),
                    item(5),
                    None,
                    item(5),
                    item(5),
                    item(5),
                    item(5)
                ],
                3
            ),
            product(54, 1, 0)
        );
        assert_eq!(result(&[item(3); 9], 3), None);
        assert_eq!(
            result(&[item(331), None, item(280), None], 2),
            product(76, 1, 0)
        );
        assert_eq!(
            result(&[item(280), None, item(4), None], 2),
            product(69, 1, 0)
        );
        assert_eq!(
            result(&[item(263), None, item(280), None], 2),
            product(50, 4, 0)
        );
        assert_eq!(
            result(
                &[
                    item(296),
                    item(296),
                    item(296),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None
                ],
                3
            ),
            product(297, 1, 0)
        );
        assert_eq!(
            result(
                &[
                    item(265),
                    item(265),
                    item(265),
                    None,
                    item(280),
                    None,
                    None,
                    item(280),
                    None,
                ],
                3
            ),
            product(257, 1, 0)
        );
        assert_eq!(
            result(
                &[
                    item(264),
                    item(264),
                    item(264),
                    None,
                    item(280),
                    None,
                    None,
                    item(280),
                    None,
                ],
                3
            ),
            product(278, 1, 0)
        );
    }
    #[test]
    fn java_1710_recipes_conserve_items_and_reject_modern_patterns() {
        let iron_door = [
            item(265),
            item(265),
            None,
            item(265),
            item(265),
            None,
            item(265),
            item(265),
            None,
        ];
        assert_eq!(result(&iron_door, 3), product(330, 1, 0));
        let wood_door = [
            item(5),
            item(5),
            None,
            item(5),
            item(5),
            None,
            item(5),
            item(5),
            None,
        ];
        assert_eq!(result(&wood_door, 3), product(324, 1, 0));
        assert_eq!(result(&[item(331); 4], 2), None);
        assert_eq!(result(&[item(331); 9], 3), product(152, 1, 0));
        assert_eq!(result(&[item(263); 9], 3), product(173, 1, 0));
        assert_eq!(result(&[product(263, 1, 1); 9], 3), None);
        assert_eq!(result(&[item(22), None, None, None], 2), product(351, 9, 4));
        assert_eq!(result(&[product(351, 1, 4); 9], 3), product(22, 1, 0));
        assert_eq!(result(&[item(351); 9], 3), None);

        let old_fence = [
            item(280),
            item(280),
            item(280),
            item(280),
            item(280),
            item(280),
            None,
            None,
            None,
        ];
        assert_eq!(result(&old_fence, 3), product(85, 2, 0));
        let modern_fence = [
            item(280),
            item(5),
            item(280),
            item(280),
            item(5),
            item(280),
            None,
            None,
            None,
        ];
        // This modern fence shape is actually a valid fence GATE in Java 1.7.10.
        assert_eq!(result(&modern_fence, 3), product(107, 1, 0));

        let powered_rail = [
            item(266),
            None,
            item(266),
            item(266),
            item(280),
            item(266),
            item(266),
            item(331),
            item(266),
        ];
        assert_eq!(result(&powered_rail, 3), product(27, 6, 0));
        let iron_rail = [
            item(265),
            None,
            item(265),
            item(265),
            item(280),
            item(265),
            item(265),
            item(331),
            item(265),
        ];
        assert_eq!(result(&iron_rail, 3), None);
        let birch_slabs = [
            product(5, 1, 2),
            product(5, 1, 2),
            product(5, 1, 2),
            None,
            None,
            None,
            None,
            None,
            None,
        ];
        assert_eq!(result(&birch_slabs, 3), product(126, 6, 2));
        let mixed_slabs = [
            product(5, 1, 2),
            product(5, 1, 3),
            product(5, 1, 2),
            None,
            None,
            None,
            None,
            None,
            None,
        ];
        assert_eq!(result(&mixed_slabs, 3), None);
    }
}
