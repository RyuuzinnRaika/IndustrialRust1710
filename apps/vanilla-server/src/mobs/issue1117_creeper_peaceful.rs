//! Issue #1117 server behavior; not a vanilla-equivalence assertion.
use super::{can_see, Kind, Mobs};
use crate::multiplayer::{self, Event};
use crate::player::PlayerPosition;
use crate::world::World;

fn setup() -> (World, Mobs, PlayerPosition, i32) {
    let world = World::new();
    let creeper = PlayerPosition {
        x: 0.5,
        y: 64.0,
        z: 0.5,
    };
    let player = PlayerPosition { x: 2.0, ..creeper };
    assert!(can_see(&world, creeper, player));
    let mut mobs = Mobs::default();
    mobs.set_difficulty(3);
    mobs.insert(Kind::Creeper, creeper, false);
    let id = mobs.next_id - 1;
    for tick in 1..=29 {
        mobs.tick(tick, &world, &[player]);
        assert!(mobs.take_explosions().is_empty(), "exploded at tick {tick}");
        assert!(mobs.take_drops().is_empty(), "dropped at tick {tick}");
    }
    let state = &mobs.entities[&id];
    assert_eq!(state.fuse, 29);
    assert!(state.creeper_igniting);
    assert!(can_see(&world, state.position, player));
    let distance = ((state.position.x - player.x).powi(2)
        + (state.position.y - player.y).powi(2)
        + (state.position.z - player.z).powi(2))
    .sqrt();
    assert!(
        distance < 7.0,
        "fuse must still be in ignition range: {distance}"
    );
    (world, mobs, player, id)
}

fn destroy_count(events: &[Event], id: i32) -> usize {
    let expected = multiplayer::destroy(id);
    events
        .iter()
        .filter(|event| event.id == expected.id && event.body == expected.body)
        .count()
}

#[test]
fn pre_explosion_peaceful_switch_despawns_before_the_next_update() {
    let (world, mut mobs, player, id) = setup();
    mobs.set_difficulty(0);
    // A repeated sample of the same tick must not process a second update.
    assert!(mobs.tick(29, &world, &[player]).is_empty());
    assert!(mobs.entities.contains_key(&id));
    let changed = mobs.tick(30, &world, &[player]);
    assert!(!mobs.entities.contains_key(&id));
    assert_eq!(destroy_count(&changed, id), 1);
    assert!(mobs.take_explosions().is_empty());
    assert!(mobs.take_drops().is_empty());
    let later = mobs.tick(31, &world, &[player]);
    assert_eq!(destroy_count(&later, id), 0);
    assert!(mobs.take_explosions().is_empty());
    assert!(mobs.take_drops().is_empty());
}

#[test]
fn hard_control_explodes_after_identical_preconditions() {
    let (world, mut mobs, player, id) = setup();
    let changed = mobs.tick(30, &world, &[player]);
    assert!(!mobs.entities.contains_key(&id));
    assert_eq!(mobs.take_explosions().len(), 1);
    assert_eq!(destroy_count(&changed, id), 1);
    let later = mobs.tick(31, &world, &[player]);
    assert_eq!(destroy_count(&later, id), 0);
    assert!(mobs.take_explosions().is_empty());
}
