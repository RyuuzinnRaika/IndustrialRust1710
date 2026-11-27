//! Non-locked, low-contention synthetic matrix; not live vanilla acceptance.
use super::*;
use std::collections::BTreeSet;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn four_orientations_and_delays_follow_sampled_tick_matrix() {
    let temp = fs::canonicalize(std::env::temp_dir()).unwrap();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = temp.join(format!("ir1710-i475-{}-{stamp}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let root = fs::canonicalize(root).unwrap();
    assert_eq!(root.parent(), Some(temp.as_path()));

    // Independent from the code under test: offsets are the output direction
    // derived from the local 1.7.10 client bytecode's rear input arrays.
    let directions = [(0, -1), (1, 0), (0, 1), (-1, 0)];
    // The same static reference maps settings 0..3 to [1,2,3,4] * 2.
    let game_tick_delays = [2_u64, 4, 6, 8];
    for (orientation, &(dx, dz)) in directions.iter().enumerate() {
        for (setting, &delay) in game_tick_delays.iter().enumerate() {
            let path = root.join(format!("case-{orientation}-{setting}.irw"));
            let mut world = World::new();
            let metadata = orientation as u16 | ((setting as u16) << 2);
            let repeater = (0, 80, 0);
            let rear = (-dx, 80, -dz);
            let front = (dx, 80, dz);
            let side_a = (-dz, 80, dx);
            let side_b = (dz, 80, -dx);
            world.import_block(0, 79, 0, 1 << 4).unwrap();
            world.import_block(0, 80, 0, (93 << 4) | metadata).unwrap();
            assert!(world.can_support(0, 80, 0, (93 << 4) | metadata));
            assert_eq!(world.get(rear.0, rear.1, rear.2), 0);
            assert_eq!(world.get(side_a.0, side_a.1, side_a.2), 0);
            assert_eq!(world.get(side_b.0, side_b.1, side_b.2), 0);
            assert!(!world.repeater_locked(0, 80, 0, metadata));
            assert_eq!(world.pending_devices, BTreeSet::from([(0, 80, 0)]));

            for (source, old_id, next_id) in [(152 << 4, 93_u16, 94_u16), (0, 94, 93)] {
                assert!(world
                    .update_and_save(&path, rear.0, rear.1, rear.2, source)
                    .unwrap());
                assert_eq!(world.power_into(rear, repeater), source != 0);
                assert!(!world.repeater_locked(0, 80, 0, metadata));
                assert_eq!(world.get(0, 80, 0), (old_id << 4) | metadata);

                // Source edit and first actual device scan are distinct.
                world.block_tick(&path, 64).unwrap();
                let sampled = world.fluid_phase;
                assert_eq!(
                    world.pending_repeaters.get(&(0, 80, 0)),
                    Some(&(sampled + delay, next_id == 94))
                );
                assert_eq!(world.get(0, 80, 0), (old_id << 4) | metadata);
                for _ in 1..delay {
                    world.block_tick(&path, 64).unwrap();
                    assert_eq!(
                        world.get(0, 80, 0),
                        (old_id << 4) | metadata,
                        "early transition direction={orientation} setting={setting}"
                    );
                }
                let changed = world.block_tick(&path, 64).unwrap();
                assert!(
                    changed.contains(&repeater),
                    "missing due transition direction={orientation} setting={setting}"
                );
                assert_eq!(world.get(0, 80, 0), (next_id << 4) | metadata);
                assert_eq!(
                    world.signal_into(repeater, front),
                    if next_id == 94 { 15 } else { 0 }
                );
                assert_eq!(world.signal_into(repeater, side_a), 0);
                assert_eq!(world.signal_into(repeater, side_b), 0);
                assert_eq!(
                    World::load(&path).unwrap().get(0, 80, 0),
                    (next_id << 4) | metadata
                );
            }
        }
    }
    // Keep the synthetic test directory; never delete an existing world.
}
