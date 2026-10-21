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
    match level {
        0..=15 => 2 * level + 7,
        16..=30 => 5 * level - 38,
        _ => 9 * level - 158,
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
    fn experience_levels_at_seven_and_seventeen() {
        let mut e = Experience::default();
        assert!(e.grant(7));
        assert_eq!(e.level_and_fraction().0, 1);
        e.grant(10);
        assert_eq!(e.level_and_fraction().0, 2);
        assert_eq!(e.packet().len(), 8);
    }
}
