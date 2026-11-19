use super::World;
use crate::ground_items::GroundItems;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn unsupported_sugar_cane_drops_correct_items_once_and_persists() {
    let root = std::env::temp_dir().join(format!(
        "ir1710-cane-regression-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    let path = root.join("world.irw");
    let drops_path = GroundItems::location(&path);
    let mut world = World::new();
    world.import_block(0, 119, 0, 3 << 4).unwrap();
    world.import_block(1, 119, 0, 9 << 4).unwrap();
    for y in 120..=122 {
        world.import_block(0, y, 0, 83 << 4).unwrap();
    }
    // This models an already committed player removal; its own drop is
    // handled by the player harvest path, not by the support-update tick.
    world.update_and_save(&path, 0, 120, 0, 0).unwrap();
    let mut ground = GroundItems::default();
    let mut produced = Vec::new();
    for _ in 0..6 {
        let (_, spawned) = world
            .block_tick_with_ground(&path, 128, &mut ground, &drops_path)
            .unwrap();
        produced.extend(spawned.into_iter().map(|item| item.stack));
    }
    for y in 120..=122 {
        assert_eq!(world.get(0, y, 0), 0, "remaining block at y={y}");
    }
    assert_eq!(produced.iter().map(|s| u32::from(s.count)).sum::<u32>(), 2);
    assert!(
        produced.iter().all(|s| s.id == 338 && s.damage == 0),
        "unsupported cane should drop item 338: {produced:?}; fixture {root:?}"
    );
    assert!(world.drain_drops().is_empty(), "duplicate standalone drops");
    let reloaded = World::load(&path).unwrap();
    for y in 120..=122 {
        assert_eq!(reloaded.get(0, y, 0), 0);
    }
    let reloaded_ground = GroundItems::load(&drops_path).unwrap();
    let stacks: Vec<_> = reloaded_ground.items().map(|item| &item.stack).collect();
    assert_eq!(stacks.iter().map(|s| u32::from(s.count)).sum::<u32>(), 2);
    assert!(stacks.iter().all(|s| s.id == 338 && s.damage == 0));
    fs::remove_dir_all(root).unwrap();
}
