//! Persistent level/progress accounting with 1.7.10 Protocol 5 XP packets.
//! XP values and awarded totals are server-owned and bounded.
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, ErrorKind},
    path::Path,
};
const MAX_XP: u32 = 10_000_000;
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Experience {
    pub total: u32,
}
fn required(level: u32) -> u32 {
    // Minecraft Java Edition 1.7.10's pre-1.8 XP progression.
    // 0..=15: 17, 16..=30: 3*level - 28, 31+: 7*level - 148.
    match level {
        0..=15 => 17,
        16..=30 => 3 * level - 28,
        _ => 7 * level - 148,
    }
}
impl Experience {
    pub fn load(path: &Path) -> io::Result<Self> {
        let data = match fs::read(path) {
            Ok(data) => data,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        };
        if data.len() > 128 {
            return Err(io::Error::new(ErrorKind::InvalidData, "oversized xp save"));
        }
        let xp: Self = serde_json::from_slice(&data)
            .map_err(|_| io::Error::new(ErrorKind::InvalidData, "bad xp save"))?;
        if xp.total > MAX_XP {
            return Err(io::Error::new(ErrorKind::InvalidData, "xp overflow"));
        }
        Ok(xp)
    }
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("xp.tmp");
        fs::write(&temporary, serde_json::to_vec(self)?)?;
        fs::rename(temporary, path)
    }
    pub fn grant(&mut self, amount: u32) -> bool {
        if amount == 0 {
            return false;
        }
        self.total = self.total.saturating_add(amount).min(MAX_XP);
        true
    }
    /// Spend complete XP levels, preserving points already earned within the
    /// current level. Reject insufficient balance without changing the account.
    pub fn spend_levels(&mut self, levels: u16) -> bool {
        let (current, _) = self.level_and_fraction();
        if current < levels {
            return false;
        }
        let points: u32 = ((current - levels)..current)
            .map(|level| required(u32::from(level)))
            .sum();
        self.total -= points;
        true
    }
    pub fn level_and_fraction(&self) -> (u16, f32) {
        let mut remaining = self.total;
        let mut level = 0u32;
        while level < 1000 && remaining >= required(level) {
            remaining -= required(level);
            level += 1;
        }
        (level as u16, remaining as f32 / required(level) as f32)
    }
    pub fn packet(&self) -> Vec<u8> {
        let (level, progress) = self.level_and_fraction();
        let mut bytes = progress.to_be_bytes().to_vec();
        bytes.extend_from_slice(&(level as i16).to_be_bytes());
        bytes.extend_from_slice(&(self.total.min(i16::MAX as u32) as i16).to_be_bytes());
        bytes
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn experience_levels_follow_minecraft_1710_thresholds() {
        let mut e = Experience::default();
        assert_eq!(e.level_and_fraction().0, 0);
        assert!(e.grant(7));
        assert_eq!(e.level_and_fraction().0, 0);
        e.grant(10);
        assert_eq!(e.level_and_fraction().0, 1);
        e.grant(17);
        assert_eq!(e.level_and_fraction().0, 2);
        assert_eq!(e.packet().len(), 8);
        assert_eq!(required(0), 17);
        assert_eq!(required(15), 17);
        assert_eq!(required(16), 20);
        assert_eq!(required(30), 62);
        assert_eq!(required(31), 69);
        e.total = 255;
        assert_eq!(e.level_and_fraction().0, 15);
        e.total = 825;
        assert_eq!(e.level_and_fraction().0, 30);
    }
    #[test]
    fn spend_levels_is_atomic_and_does_not_mint_xp() {
        let mut xp = Experience { total: 50 };
        assert_eq!(xp.level_and_fraction().0, 2);
        assert!(!xp.spend_levels(3));
        assert_eq!(xp.total, 50);
        assert!(xp.spend_levels(2));
        assert_eq!(xp.total, 16);
        assert_eq!(xp.level_and_fraction().0, 0);
        assert!(xp.spend_levels(0));
        assert_eq!(xp.total, 16);
    }

}
