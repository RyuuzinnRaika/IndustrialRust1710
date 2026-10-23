//! Bounded four-neighbour A* routing for server-owned hostile and passive mobs.
//! No infinite search, no cross-chunk allocation and no client-trusted paths.
use crate::physics;
use crate::player::PlayerPosition;
use crate::world::World;
use std::collections::{BTreeMap, BTreeSet};

type Cell = (i32, i32, i32);
const DIRECTIONS: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];

fn cell(p: PlayerPosition) -> Cell {
    (p.x.floor() as i32, p.y.floor() as i32, p.z.floor() as i32)
}
fn position(c: Cell) -> PlayerPosition {
    PlayerPosition {
        x: c.0 as f64 + 0.5,
        y: c.1 as f64,
        z: c.2 as f64 + 0.5,
    }
}
fn heuristic(a: Cell, b: Cell) -> i32 {
    (a.0 - b.0).abs() + (a.2 - b.2).abs() + (a.1 - b.1).abs() * 2
}
/// A route edge is only usable if a ground mob can clear the *whole* move.
/// In particular a step-up must clear its head before moving horizontally,
/// and a step-down must move over the ledge before descending.
fn can_transition(world: &World, from: Cell, to: Cell) -> bool {
    let start = position(from);
    let end = position(to);
    if to.1 > from.1 {
        let raised = PlayerPosition { y: end.y, ..start };
        !physics::collides(world, &raised)
            && physics::valid_movement(world, &start, &raised)
            && physics::valid_movement(world, &raised, &end)
    } else if to.1 < from.1 {
        let across = PlayerPosition { y: start.y, ..end };
        !physics::collides(world, &across)
            && physics::valid_movement(world, &start, &across)
            && physics::valid_movement(world, &across, &end)
    } else {
        physics::valid_movement(world, &start, &end)
    }
}

/// Advance one bounded tick towards an A* waypoint, including one-block
/// ascents and descents. Never teleport directly through a wall or ceiling.
/// The caller owns the speed, entity ID and shared teleport broadcast.
pub fn advance_step(
    world: &World,
    from: PlayerPosition,
    waypoint: PlayerPosition,
    speed: f64,
) -> Option<PlayerPosition> {
    if !from.is_valid()
        || !waypoint.is_valid()
        || !speed.is_finite()
        || !(0.0..=0.5).contains(&speed)
        || speed == 0.0
        || (waypoint.y - from.y).abs() > 1.01
    {
        return None;
    }
    let dx = waypoint.x - from.x;
    let dz = waypoint.z - from.z;
    let length = dx.hypot(dz);
    if length <= 1e-6 {
        return None;
    }
    let portion = speed.min(length) / length;
    let horizontal = PlayerPosition {
        x: from.x + dx * portion,
        z: from.z + dz * portion,
        ..from
    };

    // Rising requires a clear jump arc, a clear head-space and a landing
    // surface. A horizontal collision test alone would stall at a step.
    if waypoint.y > from.y + 0.5 {
        let raised = PlayerPosition {
            y: waypoint.y,
            ..from
        };
        let landing = PlayerPosition {
            y: waypoint.y,
            ..horizontal
        };
        if !physics::collides(world, &raised)
            && physics::valid_movement(world, &from, &raised)
            && physics::valid_movement(world, &raised, &landing)
            && physics::supported(world, &landing)
        {
            return Some(landing);
        }
    }
    if physics::valid_movement(world, &from, &horizontal)
        && physics::supported(world, &horizontal)
    {
        return Some(horizontal);
    }
    // Walk out over the edge first, then land. This avoids interpolating
    // diagonally through the vertical side of the higher block.
    if waypoint.y + 0.5 < from.y {
        let landing = PlayerPosition {
            y: waypoint.y,
            ..horizontal
        };
        if physics::valid_movement(world, &from, &horizontal)
            && physics::valid_movement(world, &horizontal, &landing)
            && physics::supported(world, &landing)
        {
            return Some(landing);
        }
    }
    None
}

/// Returns the next *cell* along a path, not a teleport destination. Caller
/// advances with its own per-tick speed and validates collisions again.
pub fn next_step(
    world: &World,
    from: PlayerPosition,
    target: PlayerPosition,
    budget: usize,
) -> Option<PlayerPosition> {
    if budget == 0 || !from.is_valid() || !target.is_valid() {
        return None;
    }
    let start = cell(from);
    let goal = cell(target);
    if start == goal {
        return None;
    }
    let mut open = BTreeSet::new();
    let mut scores = BTreeMap::new();
    let mut parents = BTreeMap::new();
    open.insert((heuristic(start, goal), 0i32, start));
    scores.insert(start, 0i32);
    let mut best = start;
    let mut expansions = 0;
    while let Some(&(f, cost, here)) = open.iter().next() {
        open.remove(&(f, cost, here));
        if scores.get(&here) != Some(&cost) {
            continue;
        }
        if heuristic(here, goal) < heuristic(best, goal) {
            best = here;
        }
        if here == goal {
            best = here;
            break;
        }
        expansions += 1;
        if expansions >= budget.min(128) {
            break;
        }
        for (dx, dz) in DIRECTIONS {
            for dy in [0, 1, -1] {
                let neighbour = (here.0 + dx, here.1 + dy, here.2 + dz);
                let p = position(neighbour);
                if !p.is_valid()
                    || !physics::supported(world, &p)
                    || physics::collides(world, &p)
                    || !can_transition(world, here, neighbour)
                {
                    continue;
                }
                // One-block ascent/descent is permitted; full-wall tunnelling isn't.
                let next_cost = cost + if dy == 0 { 10 } else { 14 };
                if scores.get(&neighbour).is_some_and(|old| *old <= next_cost) {
                    continue;
                }
                scores.insert(neighbour, next_cost);
                parents.insert(neighbour, here);
                open.insert((
                    next_cost + heuristic(neighbour, goal) * 10,
                    next_cost,
                    neighbour,
                ));
            }
        }
    }
    if best == start {
        return None;
    }
    while let Some(&previous) = parents.get(&best) {
        if previous == start {
            return Some(position(best));
        }
        best = previous;
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn climbs_a_one_block_step_without_entering_the_block() {
        let mut world = World::new();
        world.import_block(1, 64, 0, 1 << 4).unwrap();
        let start = PlayerPosition { x: 0.5, y: 64.0, z: 0.5 };
        let goal = PlayerPosition { x: 1.5, y: 65.0, z: 0.5 };
        assert_eq!(next_step(&world, start, goal, 128).map(cell), Some(cell(goal)));
        let stepped = advance_step(&world, start, goal, 0.24).expect("jump onto block");
        assert_eq!(stepped.y, 65.0);
        assert!(stepped.x > start.x);
        assert!(!physics::collides(&world, &stepped));
        assert!(physics::supported(&world, &stepped));
    }

    #[test]
    fn descends_only_after_clearing_the_step_edge() {
        let mut world = World::new();
        world.import_block(1, 64, 0, 1 << 4).unwrap();
        let low = PlayerPosition { x: 0.5, y: 64.0, z: 0.5 };
        let mut pos = PlayerPosition { x: 1.5, y: 65.0, z: 0.5 };
        for _ in 0..8 {
            pos = advance_step(&world, pos, low, 0.24).expect("walk off ledge safely");
            assert!(!physics::collides(&world, &pos));
            assert!(physics::supported(&world, &pos));
        }
        assert_eq!(pos.y, low.y);
        assert!((pos.x - low.x).abs() < 0.01);
    }

    #[test]
    fn overhead_obstruction_prevents_stepping_up() {
        let mut world = World::new();
        for y in 64..=66 {
            world.import_block(1, y, 0, 1 << 4).unwrap();
        }
        let from = PlayerPosition { x: 0.5, y: 64.0, z: 0.5 };
        let blocked = PlayerPosition { x: 1.5, y: 65.0, z: 0.5 };
        assert!(advance_step(&world, from, blocked, 0.24).is_none());
        assert!(!can_transition(&world, cell(from), cell(blocked)));
    }

    #[test]
    fn searches_around_a_wall_with_a_budget() {
        let mut world = World::new();
        for x in 1..=3 {
            world.import_block(x, 64, 0, 1 << 4).unwrap();
        }
        let start = PlayerPosition {
            x: 0.5,
            y: 64.0,
            z: 0.5,
        };
        let goal = PlayerPosition {
            x: 5.5,
            y: 64.0,
            z: 0.5,
        };
        assert!(next_step(&world, start, goal, 128).is_some());
        assert!(next_step(&world, start, goal, 0).is_none());
    }
}
