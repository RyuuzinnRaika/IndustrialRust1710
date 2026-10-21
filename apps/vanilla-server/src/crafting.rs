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
                return product(ingredient, count, 0);
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
        return product(126, 6, 0);
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
        return product(330, 3, 0);
    }
    if matches(&[&[-1, -1], &[-1, -1], &[-1, -1]]) {
        return product(324, 3, 0);
    }
    if matches(&[&[-1, -1, -1], &[-1, -1, -1], &[0, 280, 0]]) {
        return product(323, 3, 0);
    }
    if matches(&[&[280, 0, 280], &[280, 280, 280], &[280, 0, 280]]) {
        return product(65, 3, 0);
    }
    if matches(&[&[280, -1, 280], &[280, -1, 280]]) {
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

    if matches(&[&[0, 265], &[265, 0]]) {
        return product(359, 1, 0);
    }
    if matches(&[&[331, 331], &[331, 331]]) {
        return product(152, 1, 0);
    }

    if matches(&[&[296, 296, 296], &[296, 296, 296], &[296, 296, 296]]) {
        return product(170, 1, 0);
    }
    if matches(&[&[263, 263, 263], &[263, 263, 263], &[263, 263, 263]]) {
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
    if matches(&[&[265, 331, 265], &[265, 280, 265], &[265, 331, 265]]) {
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
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    fn item(id: i16) -> Option<Stack> {
        product(id, 1, 0)
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
}
