//! A01: non-destructive terrain/Protocol 5 regressions for existing IRW worlds.
//!
//! These tests do not generate or modify user worlds and do not claim
//! bit-for-bit Minecraft Java 1.7.10 terrain compatibility.

use super::World;
use crate::dimensions::Dimension;

/// Decode the pre-1.8 chunk's block-ID and metadata planes independently
/// from World::chunk_data. All section IDs precede all nibble metadata.
fn protocol5_block(mask: u16, bytes: &[u8], x: usize, y: usize, z: usize) -> u16 {
    assert!(x < 16 && y < 256 && z < 16);
    let section = y / 16;
    assert_ne!(mask & (1u16 << section), 0, "sampled section is absent");
    let preceding = (0..section)
        .filter(|&index| mask & (1u16 << index) != 0)
        .count();
    let sections = mask.count_ones() as usize;
    let cell = (y % 16) * 256 + z * 16 + x;
    let id = u16::from(bytes[preceding * 4096 + cell]);
    let packed = bytes[sections * 4096 + preceding * 2048 + cell / 2];
    let meta = if cell % 2 == 0 {
        packed & 0x0f
    } else {
        packed >> 4
    };
    (id << 4) | u16::from(meta)
}

#[test]
fn expanded_overworld_chunk_wire_matches_live_blocks_and_biomes_across_origin() {
    let world = World::expanded(42, Dimension::Overworld);
    // Both sides of the negative-coordinate chunk boundary are significant:
    // integer division must not silently map (-1, -1) to chunk (0, 0).
    for (chunk_x, chunk_z) in [(-1, -1), (0, 0)] {
        let (mask, bytes) = world.chunk_data(chunk_x, chunk_z);
        assert_eq!(mask, 0x01ff, "IRW4 Overworld must transmit terrain sections");
        assert_eq!(bytes.len(), mask.count_ones() as usize * 10240 + 256);
        for z in [0usize, 7, 15] {
            for x in [0usize, 7, 15] {
                let world_x = chunk_x * 16 + x as i32;
                let world_z = chunk_z * 16 + z as i32;
                assert_eq!(
                    bytes[bytes.len() - 256 + z * 16 + x],
                    world.biome_at(world_x, world_z),
                    "wire biome mismatch at ({world_x}, {world_z})"
                );
                for y in [0usize, 31, 61, 62, 63, 64, 65, 70, 95, 127, 128] {
                    assert_eq!(
                        protocol5_block(mask, &bytes, x, y, z),
                        world.get(world_x, y as i32, world_z),
                        "wire block mismatch at ({world_x}, {y}, {world_z})"
                    );
                }
            }
        }
    }
}

#[test]
fn flat_world_remains_flat_and_has_distinct_wire_layout_from_irw4() {
    let flat = World::new();
    assert!(!flat.is_natural());
    assert_eq!(flat.extended_identity(), None);
    assert_eq!(flat.surface_height_at(-1, -1), None);
    let (flat_mask, flat_bytes) = flat.chunk_data(-1, 0);
    assert_eq!(flat_mask, 0x0018);
    assert_eq!(flat_bytes.len(), 2 * 10240 + 256);
    assert!(flat_bytes[flat_bytes.len() - 256..].iter().all(|&id| id == 1));
    assert_eq!(protocol5_block(flat_mask, &flat_bytes, 15, 63, 0) >> 4, 2);
    assert_eq!(protocol5_block(flat_mask, &flat_bytes, 15, 64, 0), 0);

    let expanded = World::expanded(42, Dimension::Overworld);
    assert_eq!(expanded.extended_identity(), Some((42, Dimension::Overworld)));
    assert_eq!(expanded.chunk_data(-1, 0).0, 0x01ff);
}

#[test]
fn irw4_save_and_reload_preserve_seed_and_negative_coordinate_terrain() {
    // An isolated temporary path: never touch world/flat.irw or a user's save.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "industrialrust1710-a01-{}-{stamp}.irw",
        std::process::id()
    ));
    let original = World::expanded(42, Dimension::Overworld);
    original.save_new(&path).unwrap();
    let restored = World::load(&path).unwrap();
    assert_eq!(restored.extended_identity(), Some((42, Dimension::Overworld)));
    for (x, z) in [(-17, -17), (-16, -1), (-1, 0), (0, 0), (15, 16), (16, 15)] {
        assert_eq!(restored.biome_at(x, z), original.biome_at(x, z));
        let height = original.surface_height_at(x, z).unwrap();
        assert_eq!(restored.surface_height_at(x, z), Some(height));
        for y in [0, height - 1, height, height + 1, 127] {
            assert_eq!(
                restored.get(x, y, z),
                original.get(x, y, z),
                "reloaded block mismatch at ({x}, {y}, {z})"
            );
        }
    }
    std::fs::remove_file(path).unwrap();
}

#[test]
fn irw4_seed_42_near_spawn_is_not_superflat() {
    // The Windows no-argument launcher selects IRW4 Overworld seed 42.
    // Check the generator itself, not just that packet bytes match a flat
    // source. Sample both sides of zero and several chunk boundaries.
    let world = World::expanded(42, Dimension::Overworld);
    let mut min_height = i32::MAX;
    let mut max_height = i32::MIN;
    let mut biome_ids = std::collections::BTreeSet::new();
    for x in (-64..=64).step_by(16) {
        for z in (-64..=64).step_by(16) {
            let surface = world
                .surface_height_at(x, z)
                .expect("IRW4 Overworld must have a generated surface");
            min_height = min_height.min(surface);
            max_height = max_height.max(surface);
            biome_ids.insert(world.biome_at(x, z));
        }
    }
    assert!(
        max_height - min_height >= 12,
        "seed 42 terrain collapsed to a nearly flat plane: {min_height}..={max_height}"
    );
    assert!(
        biome_ids.len() >= 3,
        "seed 42 should expose multiple biome families near spawn: {biome_ids:?}"
    );
}
