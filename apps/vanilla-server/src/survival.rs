//! Deliberately limited Survival mining rules for Java 1.7.10.
//! No claim of full Vanilla hardness, tools, physics, hunger or entity drops.
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockDrop {
    pub id: i16,
    pub damage: i16,
}
/// Returns the basic unenchanted, hand-mining rule. Bedrock never breaks.
pub fn hand_mine(block: u16) -> Option<(Duration, BlockDrop)> {
    let id = block >> 4;
    let damage = (block & 15) as i16;
    match id {
        2 | 3 => Some((Duration::from_millis(750), BlockDrop { id: 3, damage: 0 })),
        12 => Some((Duration::from_millis(750), BlockDrop { id: 12, damage: 0 })),
        13 => Some((Duration::from_millis(900), BlockDrop { id: 13, damage: 0 })),
        1 => Some((Duration::from_millis(4500), BlockDrop { id: 4, damage: 0 })),
        4 => Some((Duration::from_millis(4500), BlockDrop { id: 4, damage: 0 })),
        5 => Some((Duration::from_millis(2250), BlockDrop { id: 5, damage })),
        17 => Some((Duration::from_millis(3000), BlockDrop { id: 17, damage })),
        _ => None,
    }
}
#[derive(Clone, Copy, Debug)]
pub struct MiningProgress {
    target: (i32, i32, i32),
    started: Instant,
    block: u16,
}
impl MiningProgress {
    pub fn start(target: (i32, i32, i32), block: u16) -> Option<Self> {
        hand_mine(block)?;
        Some(Self { target, started: Instant::now(), block })
    }
    /// Consume the attempt exactly once before checking its validity.
    pub fn finish(self, target: (i32, i32, i32), current: u16) -> Option<BlockDrop> {
        let (minimum, drop) = hand_mine(current)?;
        if self.target == target && self.block == current && self.started.elapsed() >= minimum {
            Some(drop)
        } else {
            None
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deny_bedrock_and_unlisted_blocks() {
        assert!(hand_mine(7 << 4).is_none());
        assert!(hand_mine(0).is_none());
        assert_eq!(hand_mine(2 << 4).unwrap().1.id, 3);
        assert_eq!(hand_mine(1 << 4).unwrap().1.id, 4);
        assert!(MiningProgress::start((0, 60, 0), 7 << 4).is_none());
    }
    #[test]
    fn forbid_instant_mining_and_other_targets() {
        let dig = MiningProgress::start((0, 63, 0), 2 << 4).unwrap();
        assert!(dig.finish((0, 63, 0), 2 << 4).is_none());
        let dig = MiningProgress::start((0, 63, 0), 2 << 4).unwrap();
        assert!(dig.finish((1, 63, 0), 2 << 4).is_none());
        let dig = MiningProgress::start((0, 63, 0), 2 << 4).unwrap();
        assert!(dig.finish((0, 63, 0), 3 << 4).is_none());
    }
}
