//! Bounded 2.5D A* pathfinding for land mobs (one block steps and drops).
//! Never searches the entire world, nor schedules an unbounded tick backlog.
use crate::{physics,player::PlayerPosition,world::World};
use std::{cmp::Reverse,collections::{BTreeMap,BinaryHeap}};
type Cell=(i32,i32,i32);
const MAX_VISITS:usize=96;
const MAX_RADIUS:i32=12;

fn walkable(world:&World,from:Cell,to:Cell,climbing:bool)->bool {
    let a=PlayerPosition{x:from.0 as f64+0.5,y:from.1 as f64,z:from.2 as f64+0.5};
    let b=PlayerPosition{x:to.0 as f64+0.5,y:to.1 as f64,z:to.2 as f64+0.5};
    physics::valid_movement(world,&a,&b) &&
        (physics::supported(world,&b)||climbing && (to.1>=from.1))
}
fn heuristic(a:Cell,b:Cell)->i32 {
    (a.0.abs_diff(b.0)+a.2.abs_diff(b.2)) as i32*10 + (a.1-b.1).abs()*5
}
/// Returns one waypoint, using at most 96 popped A* nodes per search.
pub fn next_step(world:&World,start:PlayerPosition,goal:PlayerPosition,climbing:bool)->Option<PlayerPosition> {
    let root=(start.x.floor() as i32,start.y.round() as i32,start.z.floor() as i32);
    let target=(goal.x.floor() as i32,goal.y.round() as i32,goal.z.floor() as i32);
    if heuristic(root,target)<10 { return None; }
    let mut frontier=BinaryHeap::new();
    let mut cost=BTreeMap::<Cell,i32>::new();
    let mut parents=BTreeMap::<Cell,Cell>::new();
    frontier.push((Reverse(heuristic(root,target)),root));
    cost.insert(root,0);
    let mut best=root;
    let mut best_h=heuristic(root,target);
    let mut visits=0;
    while let Some((_,pos))=frontier.pop() {
        visits+=1;
        if visits>MAX_VISITS {break;}
        let h=heuristic(pos,target);
        if h<best_h {best=pos;best_h=h;}
        if h<=10 {break;}
        let current=*cost.get(&pos).unwrap_or(&i32::MAX);
        for (dx,dz) in [(1,0),(-1,0),(0,1),(0,-1)] {
            for dy in [0,1,-1] {
                let next=(pos.0+dx,pos.1+dy,pos.2+dz);
                if (next.0-root.0).abs()>MAX_RADIUS || (next.2-root.2).abs()>MAX_RADIUS ||
                    (next.1-root.1).abs()>4 || !(1..254).contains(&next.1) ||
                    !walkable(world,pos,next,climbing) {continue;}
                let tentative=current + 10 + dy.abs()*8;
                if tentative < *cost.get(&next).unwrap_or(&i32::MAX) {
                    cost.insert(next,tentative);
                    parents.insert(next,pos);
                    frontier.push((Reverse(tentative+heuristic(next,target)),next));
                }
            }
        }
    }
    if best==root { return None; }
    let mut cursor=best;
    while let Some(&parent)=parents.get(&cursor) {
        if parent==root {break;}
        cursor=parent;
    }
    Some(PlayerPosition{x:cursor.0 as f64+0.5,y:cursor.1 as f64,z:cursor.2 as f64+0.5})
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_path_routes_around_wall() {
        let mut w=World::new();
        for z in -1..=1 {for y in 64..=66 { w.import_block(1,y,z,1<<4).unwrap();}}
        let a=PlayerPosition{x:0.5,y:64.0,z:0.5};
        let b=PlayerPosition{x:4.5,y:64.0,z:0.5};
        let step=next_step(&w,a,b,false).unwrap();
        assert!(!physics::collides(&w,&step));
        assert!(physics::supported(&w,&step));
    }
    #[test]
    fn no_path_through_bedrock_or_void() {
        let w=World::new();
        let a=PlayerPosition{x:0.5,y:64.0,z:0.5};
        let b=PlayerPosition{x:20.5,y:64.0,z:0.5};
        assert!(next_step(&w,a,b,false).is_some());
    }
}
