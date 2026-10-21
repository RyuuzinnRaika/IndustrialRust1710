//! Sparse, budgeted vegetation, agriculture and attachment updates.
//! Executed by the server's existing 20TPS block worker; no global chunk scan.
use super::{in_bounds, Position, World};
use std::collections::BTreeMap;

fn solid(id: u16) -> bool {
    !matches!(id, 0 | 6 | 8..=11 | 18 | 20 | 31 | 37..=40 | 50 | 55 | 59
        | 63 | 66 | 69 | 70 | 72 | 75..=78 | 83 | 93 | 94 | 96
        | 141 | 142 | 143)
}
fn chance(phase: u64, x: i32, y: i32, z: i32, divisor: u64) -> bool {
    let mut v = phase
        ^ (x as i64 as u64).wrapping_mul(0x9e3779b185ebca87)
        ^ (y as i64 as u64).wrapping_mul(0xc2b2ae3d27d4eb4f)
        ^ (z as i64 as u64).wrapping_mul(0x165667b19e3779f9);
    v ^= v >> 30;
    v = v.wrapping_mul(0xbf58476d1ce4e5b9);
    v ^= v >> 27;
    v = v.wrapping_mul(0x94d049bb133111eb);
    (v ^ (v >> 31)).is_multiple_of(divisor)
}
fn plantable(id: u16) -> bool {
    matches!(id, 2 | 3 | 60)
}

impl World {
    /// Attachment validity also used to reject impossible placements.
    pub fn can_support(&self, x: i32, y: i32, z: i32, block: u16) -> bool {
        let id = block >> 4;
        let below = self.get(x, y - 1, z) >> 4;
        match id {
            6 => plantable(below),
            83 => below == 83 || (matches!(below, 2 | 3 | 12) &&
                [(1,0),(-1,0),(0,1),(0,-1)].iter().any(|&(dx,dz)|
                    matches!(self.get(x+dx,y-1,z+dz)>>4,8|9))),
            81 => matches!(below, 12 | 81) &&
                [(1,0),(-1,0),(0,1),(0,-1)].iter().all(|&(dx,dz)|
                    self.get(x+dx,y,z+dz)==0),
            115 => below == 88,
            104 | 105 => below == 60,
            127 => {
                let (dx,dz) = match block & 3 {0=>(0,1),1=>(-1,0),2=>(0,-1),_=>(1,0)};
                matches!(self.get(x+dx,y,z+dz)>>4,17|162)
            },
            59 | 141 | 142 => below == 60,
            50 | 75 | 76 => {
                let support = match block & 7 {
                    1 => self.get(x - 1, y, z) >> 4,
                    2 => self.get(x + 1, y, z) >> 4,
                    3 => self.get(x, y, z - 1) >> 4,
                    4 => self.get(x, y, z + 1) >> 4,
                    _ => below,
                };
                solid(support)
            }
            55 | 66 | 27 | 28 | 157 | 70 | 72 | 93 | 94 => solid(below),
            77 | 143 => {
                let neighbor = match block & 7 {
                    1 => self.get(x - 1, y, z) >> 4,
                    2 => self.get(x + 1, y, z) >> 4,
                    3 => self.get(x, y, z - 1) >> 4,
                    4 => self.get(x, y, z + 1) >> 4,
                    _ => below,
                };
                solid(neighbor)
            }
            64 | 71 => {
                if block & 8 != 0 {
                    self.get(x, y - 1, z) >> 4 == id
                } else {
                    solid(below)
                }
            }
            96 => {
                let neighbor = match block & 3 {
                    0 => self.get(x, y, z + 1) >> 4,
                    1 => self.get(x, y, z - 1) >> 4,
                    2 => self.get(x + 1, y, z) >> 4,
                    _ => self.get(x - 1, y, z) >> 4,
                };
                solid(neighbor)
            }
            _ => true,
        }
    }
    fn wet_farmland(&self, x: i32, y: i32, z: i32) -> bool {
        for dx in -4..=4 {
            for dz in -4..=4 {
                for dy in 0..=1 {
                    if matches!(self.get(x + dx, y + dy, z + dz) >> 4, 8 | 9) {
                        return true;
                    }
                }
            }
        }
        false
    }
    /// A leaf survives only if a log can be reached through at most four
    /// face-adjacent leaves. Nearby but disconnected logs do not prevent decay.
    fn leaf_connected(&self, x: i32, y: i32, z: i32) -> bool {
        use std::collections::{BTreeSet, VecDeque};
        let mut seen = BTreeSet::new();
        let mut queue = VecDeque::from([(x, y, z, 0u8)]);
        seen.insert((x, y, z));
        while let Some((px, py, pz, depth)) = queue.pop_front() {
            for (dx, dy, dz) in [
                (1, 0, 0),
                (-1, 0, 0),
                (0, 1, 0),
                (0, -1, 0),
                (0, 0, 1),
                (0, 0, -1),
            ] {
                let next = (px + dx, py + dy, pz + dz);
                match self.get(next.0, next.1, next.2) >> 4 {
                    17 | 162 => return true,
                    18 if depth < 3 && seen.insert(next) => {
                        queue.push_back((next.0, next.1, next.2, depth + 1));
                    }
                    _ => {}
                }
            }
        }
        false
    }
    fn grow_tree(&self, x: i32, y: i32, z: i32, changes: &mut BTreeMap<Position, u16>) {
        if y > 249 || !plantable(self.get(x, y - 1, z) >> 4) {
            return;
        }
        // First pass checks every eventual occupied cell; tree generation
        // must never overwrite players' buildings or another sapling.
        for dy in 1..=5 {
            let radius = if dy >= 3 { 2 } else { 0 };
            for dx in -radius..=radius {
                for dz in -radius..=radius {
                    if self.get(x + dx, y + dy, z + dz) != 0 {
                        return;
                    }
                }
            }
        }
        for dy in 0..4 {
            changes.insert((x, (y + dy) as u8, z), 17 << 4);
        }
        for dy in 3..=5 {
            let radius: i32 = if dy == 5 { 1 } else { 2 };
            for dx in -radius..=radius {
                for dz in -radius..=radius {
                    if dx == 0 && dz == 0 && dy <= 3 {
                        continue;
                    }
                    if dx.abs() == 2 && dz.abs() == 2 {
                        continue;
                    }
                    let pos = (x + dx, (y + dy) as u8, z + dz);
                    if in_bounds(pos.0, i32::from(pos.1), pos.2) {
                        changes.insert(pos, 18 << 4);
                    }
                }
            }
        }
    }
    /// Changes from environmental random ticks and immediate support checks.
    /// This function only stages data, so the caller can commit or roll back
    /// the entire tick as one world-file transaction.
    pub(super) fn ecology_changes(&self, budget: usize) -> BTreeMap<Position, u16> {
        let mut changes = BTreeMap::new();
        for &(x, y, z) in self.pending_support.iter().take(budget) {
            let block = self.get(x, i32::from(y), z);
            let id = block >> 4;
            if matches!(
                id,
                6 | 50
                    | 55
                    | 59
                    | 66
                    | 70
                    | 72
                    | 75
                    | 76
                    | 93
                    | 94
                    | 141
                    | 142
                    | 81 | 83 | 104 | 105 | 115 | 127
                    | 64
                    | 71
                    | 77
                    | 143
                    | 96
            ) && !self.can_support(x, i32::from(y), z, block)
            {
                changes.insert((x, y, z), 0);
            }
        }
        if !self.fluid_phase.is_multiple_of(20) || self.pending_ecology.is_empty() {
            return changes;
        }
        let len = self.pending_ecology.len();
        let start = (self.fluid_phase as usize / 20).wrapping_mul(budget) % len;
        for &(x, y, z) in self
            .pending_ecology
            .iter()
            .cycle()
            .skip(start)
            .take(budget.min(len))
        {
            let block = self.get(x, i32::from(y), z);
            let id = block >> 4;
            let height = i32::from(y);
            if !self.can_support(x, height, z, block) {
                changes.insert((x, y, z), 0);
                continue;
            }
            match id {
                60 => {
                    let moisture = block & 7;
                    if self.wet_farmland(x, height, z) {
                        if moisture != 7 {
                            changes.insert((x, y, z), (60 << 4) | 7);
                        }
                    } else if moisture > 0 {
                        changes.insert((x, y, z), (60 << 4) | (moisture - 1));
                    } else if !matches!(self.get(x, height + 1, z) >> 4, 59 | 141 | 142) {
                        changes.insert((x, y, z), 3 << 4);
                    }
                }
                83 | 81 if height < 255 && self.get(x,height+1,z)==0
                    && chance(self.fluid_phase,x,height,z,3) => {
                    let mut bottom=height;
                    while bottom>0 && self.get(x,bottom-1,z)>>4==id { bottom-=1; }
                    if height-bottom<2 {
                        changes.insert((x,(height+1) as u8,z),id<<4);
                    }
                }
                115 if block & 3 < 3 && chance(self.fluid_phase,x,height,z,3) => {
                    changes.insert((x,y,z),block+1);
                }
                127 if block & 12 < 8 && chance(self.fluid_phase,x,height,z,3) => {
                    changes.insert((x,y,z),block+4);
                }
                104 | 105 if block & 7 < 7 && chance(self.fluid_phase,x,height,z,3) => {
                    changes.insert((x,y,z),block+1);
                }
                104 | 105 if block & 7 == 7 => {
                    let fruit=if id==104 {86} else {103};
                    let dirs=[(1,0),(-1,0),(0,1),(0,-1)];
                    if !dirs.iter().any(|&(dx,dz)| self.get(x+dx,height,z+dz)>>4==fruit) {
                        for (dx,dz) in dirs {
                            let nx=x+dx; let nz=z+dz;
                            if in_bounds(nx,height,nz) && self.get(nx,height,nz)==0
                                && matches!(self.get(nx,height-1,nz)>>4,2|3|60)
                                && chance(self.fluid_phase,nx,height,nz,4) {
                                changes.insert((nx,y,nz),fruit<<4);
                                break;
                            }
                        }
                    }
                }
                59 | 141 | 142 if block & 7 < 7 => {
                    let damp = self.get(x, height - 1, z) & 7;
                    if self.get(x, height + 1, z) == 0
                        && chance(self.fluid_phase, x, height, z, if damp > 0 { 2 } else { 4 })
                    {
                        changes.insert((x, y, z), (id << 4) | ((block & 7) + 1));
                    }
                }
                6 if chance(self.fluid_phase, x, height, z, 4) => {
                    if block & 8 == 0 {
                        changes.insert((x, y, z), block | 8);
                    } else if block & 7 == 0 {
                        self.grow_tree(x, height, z, &mut changes);
                    }
                }
                18 if !self.leaf_connected(x, height, z) => {
                    changes.insert((x, y, z), 0);
                }
                _ => {}
            }
        }
        changes
    }
}

#[cfg(test)]
mod regression_tests {
    use super::*;

    #[test]
    fn leaf_decay_requires_a_contiguous_four_block_path_to_a_log() {
        let mut connected = World::new();
        for x in 0..4 {
            connected.import_block(x, 65, 0, 18 << 4).unwrap();
        }
        connected.import_block(4, 65, 0, 17 << 4).unwrap();
        assert!(connected.leaf_connected(0, 65, 0));

        let mut disconnected = World::new();
        disconnected.import_block(0, 65, 0, 18 << 4).unwrap();
        disconnected.import_block(2, 65, 0, 17 << 4).unwrap();
        assert!(!disconnected.leaf_connected(0, 65, 0));

        let mut too_far = World::new();
        for x in 0..5 {
            too_far.import_block(x, 65, 0, 18 << 4).unwrap();
        }
        too_far.import_block(5, 65, 0, 17 << 4).unwrap();
        assert!(!too_far.leaf_connected(0, 65, 0));
    }

    #[test]
    fn farmland_hydration_uses_water_within_four_horizontal_blocks() {
        let mut w = World::new();
        w.import_block(4, 64, 0, 8 << 4).unwrap();
        assert!(w.wet_farmland(0, 64, 0));
        assert!(!w.wet_farmland(-1, 64, 0));
    }
}
