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
    PlayerPosition { x: c.0 as f64 + 0.5, y: c.1 as f64, z: c.2 as f64 + 0.5 }
}
fn heuristic(a: Cell, b: Cell) -> i32 {
    (a.0 - b.0).abs() + (a.2 - b.2).abs() + (a.1 - b.1).abs() * 2
}
/// Returns the next *cell* along a path, not a teleport destination. Caller
/// advances with its own per-tick speed and validates collisions again.
pub fn next_step(world: &World, from: PlayerPosition, target: PlayerPosition, budget: usize) -> Option<PlayerPosition> {
    if budget == 0 || !from.is_valid() || !target.is_valid() { return None; }
    let start = cell(from);
    let goal = cell(target);
    if start == goal { return None; }
    let mut open = BTreeSet::new();
    let mut scores = BTreeMap::new();
    let mut parents = BTreeMap::new();
    open.insert((heuristic(start,goal), 0i32, start));
    scores.insert(start, 0i32);
    let mut best = start;
    let mut expansions = 0;
    while let Some(&(f, cost, here)) = open.iter().next() {
        open.remove(&(f,cost,here));
        if scores.get(&here) != Some(&cost) { continue; }
        if heuristic(here,goal) < heuristic(best,goal) { best = here; }
        if here.0 == goal.0 && here.2 == goal.2 { best = here; break; }
        expansions += 1;
        if expansions >= budget.min(128) { break; }
        for (dx,dz) in DIRECTIONS {
            for dy in [0,1,-1] {
                let neighbour = (here.0+dx, here.1+dy, here.2+dz);
                let p = position(neighbour);
                if !p.is_valid() || !physics::supported(world,&p) || physics::collides(world,&p) { continue; }
                // One-block ascent/descent is permitted; full-wall tunnelling isn't.
                let next_cost = cost + if dy == 0 {10} else {14};
                if scores.get(&neighbour).is_some_and(|old| *old <= next_cost) { continue; }
                scores.insert(neighbour,next_cost);
                parents.insert(neighbour,here);
                open.insert((next_cost + heuristic(neighbour,goal)*10,next_cost,neighbour));
            }
        }
    }
    if best == start { return None; }
    while let Some(&previous) = parents.get(&best) {
        if previous == start { return Some(position(best)); }
        best = previous;
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn searches_around_a_wall_with_a_budget() {
        let mut world=World::new();
        for x in 1..=3 { world.import_block(x,64,0,1<<4).unwrap(); }
        let start=PlayerPosition{x:0.5,y:64.0,z:0.5};
        let goal=PlayerPosition{x:5.5,y:64.0,z:0.5};
        assert!(next_step(&world,start,goal,128).is_some());
        assert!(next_step(&world,start,goal,0).is_none());
    }
}
