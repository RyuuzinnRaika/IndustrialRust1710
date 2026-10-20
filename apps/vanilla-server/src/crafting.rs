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
    let filled: Vec<_> = grid.iter().enumerate().filter(|(_, item)| item.is_some()).map(|(i, _)| (i % width, i / width)).collect();
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
    if w == 2 && h == 2 && (0..2).all(|y| (0..2).all(|x| plank(at(x, y)))) {
        return product(58, 1, 0); // crafting table
    }
    if w == 3 && h == 3 && (0..3).all(|y| (0..3).all(|x| {
        if x == 1 && y == 1 { at(x, y).is_none() } else { plank(at(x, y)) }
    })) {
        return product(54, 1, 0); // chest
    }
    if w == 3 && h == 3 && (0..3).all(|y| (0..3).all(|x| {
        if x == 1 && y == 1 { at(x, y).is_none() } else { same_material(at(x, y), 4) }
    })) {
        return product(61, 1, 0); // furnace
    }
    if width == 3 {
        for (material, pickaxe, axe, shovel, sword, hoe) in [
            (5, 270, 271, 269, 268, 290),
            (4, 274, 275, 273, 272, 291),
        ] {
            let m = |x, y| same_material(at(x, y), material);
            let s = |x, y| stick(at(x, y));
            let blank = |x, y| at(x, y).is_none();
            if w == 3 && h == 3 && (0..3).all(|x| m(x, 0))
                && s(1, 1) && s(1, 2)
                && blank(0, 1) && blank(2, 1) && blank(0, 2) && blank(2, 2) {
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
                    if m(0, 0) && m(1, 0) && m(flip, 1)
                        && s(1 - flip, 1) && blank(flip, 2) && s(1 - flip, 2) {
                        return product(axe, 1, 0);
                    }
                    if m(0, 0) && m(1, 0) && blank(flip, 1)
                        && s(1 - flip, 1) && blank(flip, 2) && s(1 - flip, 2) {
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
    fn item(id: i16) -> Option<Stack> { product(id, 1, 0) }
    #[test]
    fn early_game_recipes_are_bounded_and_shape_checked() {
        assert_eq!(result(&[None, item(17), None, None], 2), product(5, 4, 0));
        assert_eq!(result(&[item(5), item(5), item(5), item(5)], 2), product(58, 1, 0));
        assert_eq!(result(&[item(5), None, item(5), None], 2), product(280, 4, 0));
        assert_eq!(result(&[item(5), item(5), None, None], 2), None);
        assert_eq!(result(&[item(5), item(5), item(5), None, item(280), None, None, item(280), None], 3), product(270, 1, 0));
        assert_eq!(result(&[item(4), item(4), item(4), item(4), None, item(4), item(4), item(4), item(4)], 3), product(61, 1, 0));
        assert_eq!(result(&[item(5), item(5), item(5), item(5), None, item(5), item(5), item(5), item(5)], 3), product(54, 1, 0));
        assert_eq!(result(&[item(3); 9], 3), None);
    }
}
