//! Synthetic cocoa support/drop regression. Static Java reference is not a live client test.
use super::*;
use crate::ground_items::GroundItems;

const COCOA: (i32, i32, i32) = (0, 80, 0);
// The local Java 1.7.10 client bytecode `p.a`/`p.b` arrays give these offsets.
const SUPPORTS: [(i32, i32); 4] = [(0, 1), (-1, 0), (0, -1), (1, 0)];

fn check_drop(direction: u16, age: u16, expected: u8) {
    let (scratch, path) = isolated_chunk_regression_world("cocoa-support");
    let temp = std::env::temp_dir().canonicalize().unwrap();
    let scratch_resolved = scratch.canonicalize().unwrap();
    assert_eq!(scratch_resolved.parent(), Some(temp.as_path()));
    let drop_path = GroundItems::location(&path);
    let mut world = World::new();
    let (dx, dz) = SUPPORTS[usize::from(direction)];
    let metadata = (age << 2) | direction;
    let block = (127 << 4) | metadata;
    assert!(world
        .update_and_save(&path, dx, 80, dz, (17 << 4) | 3)
        .unwrap());
    assert!(world
        .update_and_save(&path, COCOA.0, COCOA.1, COCOA.2, block)
        .unwrap());
    assert!(world.can_support(COCOA.0, COCOA.1, COCOA.2, block));
    assert_eq!(world.get(COCOA.0, COCOA.1, COCOA.2), block);
    assert!(world.update_and_save(&path, dx, 80, dz, 0).unwrap());
    assert!(!world.can_support(COCOA.0, COCOA.1, COCOA.2, block));
    let mut ground = GroundItems::default();
    let (changed, spawned) = world
        .block_tick_with_ground(&path, 64, &mut ground, &drop_path)
        .unwrap();
    assert_eq!(changed.iter().filter(|&&p| p == COCOA).count(), 1);
    assert_eq!(world.get(COCOA.0, COCOA.1, COCOA.2), 0);
    let beans: Vec<_> = spawned
        .iter()
        .filter(|x| x.stack.id == 351 && x.stack.damage == 3)
        .collect();
    assert_eq!(beans.len(), 1, "one cocoa drop stack");
    assert_eq!(
        beans[0].stack.count, expected,
        "cocoa drop count direction={direction} age={age}"
    );
    assert_eq!(spawned.len(), 1);
    assert!(world.drain_drops().is_empty());
    let mut reloaded = World::load(&path).unwrap();
    let mut persisted = GroundItems::load(&drop_path).unwrap();
    assert_eq!(reloaded.get(COCOA.0, COCOA.1, COCOA.2), 0);
    assert_eq!(
        persisted
            .items()
            .map(|item| (item.stack.id, item.stack.damage, item.stack.count))
            .collect::<Vec<_>>(),
        vec![(351, 3, expected)]
    );
    let (changed_again, spawned_again) = reloaded
        .block_tick_with_ground(&path, 64, &mut persisted, &drop_path)
        .unwrap();
    assert!(!changed_again.contains(&COCOA));
    assert!(spawned_again.is_empty());
    assert_eq!(
        World::load(&path).unwrap().get(COCOA.0, COCOA.1, COCOA.2),
        0
    );
    assert_eq!(
        GroundItems::load(&drop_path)
            .unwrap()
            .items()
            .map(|item| (item.stack.id, item.stack.damage, item.stack.count))
            .collect::<Vec<_>>(),
        vec![(351, 3, expected)]
    );
    assert_eq!(scratch.canonicalize().unwrap(), scratch_resolved);
    for owned in [&path, &drop_path] {
        assert_eq!(
            owned.canonicalize().unwrap().parent(),
            Some(scratch_resolved.as_path())
        );
        std::fs::remove_file(owned).unwrap();
    }
    std::fs::remove_dir(scratch).unwrap();
}

#[test]
fn age_zero_drops_one_in_all_directions() {
    for d in 0..4 {
        check_drop(d, 0, 1);
    }
}
#[test]
fn age_two_drops_three_in_all_directions() {
    for d in 0..4 {
        check_drop(d, 2, 3);
    }
}
#[test]
fn age_one_south_drops_one() {
    check_drop(0, 1, 1);
}
#[test]
fn age_one_west_drops_one() {
    check_drop(1, 1, 1);
}
#[test]
fn age_one_north_drops_one() {
    check_drop(2, 1, 1);
}
#[test]
fn age_one_east_drops_one() {
    check_drop(3, 1, 1);
}

// Age 3 is an out-of-range metadata value, not an ordinary cocoa growth stage.
#[test]
fn unusual_age_three_drops_three_in_all_directions() {
    for d in 0..4 {
        check_drop(d, 3, 3);
    }
}

#[test]
fn piston_breaks_stage_one_cocoa_for_one_bean() {
    let (scratch, path) = isolated_chunk_regression_world("cocoa-piston");
    let temp = std::env::temp_dir().canonicalize().unwrap();
    let scratch_resolved = scratch.canonicalize().unwrap();
    assert_eq!(scratch_resolved.parent(), Some(temp.as_path()));
    let drop_path = GroundItems::location(&path);
    let mut world = World::new();
    for (x, y, z, block) in [
        (-1, 64, 0, 152 << 4),
        (0, 64, 0, (33 << 4) | 5),
        (1, 64, 1, (17 << 4) | 3),
        (1, 64, 0, (127 << 4) | 4),
    ] {
        assert!(world.update_and_save(&path, x, y, z, block).unwrap());
    }
    assert!(world.can_support(1, 64, 0, (127 << 4) | 4));
    let mut ground = GroundItems::default();
    let (changed, spawned) = world
        .block_tick_with_ground(&path, 64, &mut ground, &drop_path)
        .unwrap();
    assert!(changed.contains(&(1, 64, 0)));
    assert_eq!(world.get(1, 64, 0) >> 4, 34);
    let beans: Vec<_> = spawned
        .iter()
        .filter(|item| item.stack.id == 351 && item.stack.damage == 3)
        .map(|item| item.stack.count)
        .collect();
    assert_eq!(beans, vec![1], "stage-one piston cocoa drop count");
    assert_eq!(World::load(&path).unwrap().get(1, 64, 0) >> 4, 34);
    let mut restored = World::load(&path).unwrap();
    let mut persisted = GroundItems::load(&drop_path).unwrap();
    assert_eq!(
        persisted
            .items()
            .filter(|item| item.stack.id == 351 && item.stack.damage == 3)
            .map(|item| item.stack.count)
            .collect::<Vec<_>>(),
        vec![1]
    );
    let (_, later) = restored
        .block_tick_with_ground(&path, 64, &mut persisted, &drop_path)
        .unwrap();
    assert!(later
        .iter()
        .all(|item| item.stack.id != 351 || item.stack.damage != 3));
    assert_eq!(
        GroundItems::load(&drop_path)
            .unwrap()
            .items()
            .filter(|item| item.stack.id == 351 && item.stack.damage == 3)
            .map(|item| item.stack.count)
            .collect::<Vec<_>>(),
        vec![1]
    );
    assert_eq!(scratch.canonicalize().unwrap(), scratch_resolved);
    for owned in [&path, &drop_path] {
        assert_eq!(
            owned.canonicalize().unwrap().parent(),
            Some(scratch_resolved.as_path())
        );
        std::fs::remove_file(owned).unwrap();
    }
    std::fs::remove_dir(scratch).unwrap();
}
