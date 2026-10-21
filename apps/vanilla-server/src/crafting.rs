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
    // 1.7.10 shaped utility/transport recipes; border-trimmed just like tools.
    if w == 3 && h == 1 {
        if (0..3).all(|x| same_material(at(x, 0), 338)) {
            return product(339, 3, 0); // paper
        }
        if (0..3).all(|x| plank(at(x, 0))) {
            return product(126, 6, at(0, 0)?.damage & 7); // wooden slabs
        }
        if (0..3).all(|x| same_material(at(x, 0), 1)) {
            return product(44, 6, 0); // stone slabs
        }
    }
    if w == 1 && h == 1 && same_material(at(0, 0), 338) {
        return product(353, 1, 0); // sugar cane -> sugar
    }
    if w == 2 && h == 2 {
        if same_material(at(0, 0), 265)
            && at(1, 0).is_none()
            && at(0, 1).is_none()
            && same_material(at(1, 1), 265)
            || at(0, 0).is_none()
                && same_material(at(1, 0), 265)
                && same_material(at(0, 1), 265)
                && at(1, 1).is_none()
        {
            return product(359, 1, 0); // shears (either diagonal)
        }
    }
    if w == 3 && h == 2 {
        if (0..3).all(|x| same_material(at(x, 0), 35))
            && (0..3).all(|x| plank(at(x, 1)))
        {
            return product(355, 1, 0); // bed
        }
        if (0..2).all(|y| (0..3).all(|x| plank(at(x, y)))) {
            return product(324, 1, 0); // wooden door in 1.7.10
        }
        if (0..2).all(|y| (0..3).all(|x| stick(at(x, y)))) {
            return product(85, 2, 0); // old fence recipe
        }
        if (0..2).all(|y| (0..3).all(|x| same_material(at(x, y), 20))) {
            return product(102, 16, 0); // glass panes
        }
        if (0..2).all(|y| (0..3).all(|x| same_material(at(x, y), 265))) {
            return product(101, 16, 0); // iron bars
        }
    }
    if w == 3 && h == 3 {
        if (0..3).all(|y| (0..3).all(|x| {
            if x == 0 || x == 2 {
                stick(at(x, y))
            } else if y == 0 || y == 2 {
                at(x, y).is_none()
            } else {
                stick(at(x, y))
            }
        })) {
            return product(65, 3, 0); // ladders
        }
        if (0..3).all(|y| (0..3).all(|x| {
            if x == 0 || x == 2 {
                same_material(at(x, y), 265)
            } else if y == 1 {
                stick(at(x, y))
            } else {
                at(x, y).is_none()
            }
        })) {
            return product(66, 16, 0); // rails
        }
        if (0..3).all(|y| (0..3).all(|x| {
            if x == 0 || x == 2 {
                same_material(at(x, y), 266)
            } else if y == 1 {
                stick(at(x, y))
            } else if y == 2 {
                same_material(at(x, y), 331)
            } else {
                at(x, y).is_none()
            }
        })) {
            return product(27, 6, 0); // powered rails
        }
    }
    if w == 3 && h == 2
        && plank(at(0, 0)) && at(1, 0).is_none() && plank(at(2, 0))
        && (0..3).all(|x| plank(at(x, 1)))
    {
        return product(333, 1, 0); // boat
    }
    if w == 3 && h == 2 && same_material(at(0, 0), 265)
        && at(1, 0).is_none() && same_material(at(2, 0), 265)
        && at(0, 1).is_none() && same_material(at(1, 1), 265)
        && at(2, 1).is_none()
    {
        return product(325, 1, 0); // bucket
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
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    fn item(id: i16) -> Option<Stack> {
        product(id, 1, 0)
    }
    #[test]
    fn utility_recipes_and_invalid_shapes() {
        let shaped = |items: &[i16], width: usize| {
            let grid: Vec<_> = items.iter().map(|&id| if id == 0 { None } else { item(id) }).collect();
            result(&grid, width)
        };
        assert_eq!(shaped(&[35,35,35,5,5,5,0,0,0],3), product(355,1,0));
        assert_eq!(shaped(&[265,0,265,0,265,0,0,0,0],3), product(325,1,0));
        assert_eq!(shaped(&[338,338,338,0,0,0,0,0,0],3), product(339,3,0));
        assert_eq!(shaped(&[338,0,0,0],2), product(353,1,0));
        assert_eq!(shaped(&[265,0,0,265],2), product(359,1,0));
        assert_eq!(shaped(&[0,265,265,0],2), product(359,1,0));
        assert_eq!(shaped(&[5,0,5,5,5,5,0,0,0],3), product(333,1,0));
        assert_eq!(shaped(&[280,0,280,280,280,280,280,0,280],3), product(65,3,0));
        assert_eq!(shaped(&[265,0,265,265,280,265,265,0,265],3), product(66,16,0));
        assert_eq!(shaped(&[266,0,266,266,280,266,266,331,266],3), product(27,6,0));
        assert_eq!(shaped(&[5,5,5,5,5,5,0,0,0],3), product(324,1,0));
        assert_eq!(shaped(&[280,280,280,280,280,280,0,0,0],3), product(85,2,0));
        assert_eq!(shaped(&[20,20,20,20,20,20,0,0,0],3), product(102,16,0));
        assert_eq!(shaped(&[265,265,265,265,265,265,0,0,0],3), product(101,16,0));
        assert_eq!(shaped(&[5,5,5,0,0,0,0,0,0],3), product(126,6,0));
        assert_eq!(shaped(&[1,1,1,0,0,0,0,0,0],3), product(44,6,0));
        assert_eq!(shaped(&[35,35,35,5,5,4,0,0,0],3), None);
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
        assert_eq!(result(&[item(5), item(5), None, None], 2), None);
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
