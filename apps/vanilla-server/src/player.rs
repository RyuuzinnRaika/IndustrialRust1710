//! Persistent player location for bounded flat-world prototype.
//! This is NOT Mojang player.dat/NBT. Requires a validated username.
use serde_json::json;
use std::fs::{self, File};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

pub const SPAWN_Y: f64 = 65.0;
const MAX_PLAYER_BYTES: u64 = 2048;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerPosition {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}
impl Default for PlayerPosition {
    fn default() -> Self {
        Self {
            x: 0.5,
            y: SPAWN_Y,
            z: 0.5,
        }
    }
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}
impl PlayerPosition {
    pub fn is_valid(&self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.z.is_finite()
            && (0.0..256.0).contains(&self.y)
            && (-32.0..48.0).contains(&self.x)
            && (-32.0..48.0).contains(&self.z)
    }
    pub fn location(world_file: &Path, username: &str) -> PathBuf {
        // username must already be validated by whitelist::valid_name.
        let parent = world_file
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        parent
            .join("players")
            .join(format!("{}.json", username.to_ascii_lowercase()))
    }
    pub fn load(path: &Path) -> io::Result<Self> {
        let m = match fs::metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        };
        if !m.is_file() || m.len() > MAX_PLAYER_BYTES {
            return Err(invalid("invalid player save file size/type"));
        }
        let raw = fs::read(path)?;
        if raw.len() as u64 > MAX_PLAYER_BYTES {
            return Err(invalid("player file too large"));
        }
        let v: serde_json::Value =
            serde_json::from_slice(&raw).map_err(|_| invalid("invalid player file JSON"))?;
        if v.get("version").and_then(serde_json::Value::as_u64) != Some(1) {
            return Err(invalid("unsupported player file version"));
        }
        let pos = Self {
            x: v.get("x")
                .and_then(serde_json::Value::as_f64)
                .ok_or_else(|| invalid("missing x"))?,
            y: v.get("y")
                .and_then(serde_json::Value::as_f64)
                .ok_or_else(|| invalid("missing y"))?,
            z: v.get("z")
                .and_then(serde_json::Value::as_f64)
                .ok_or_else(|| invalid("missing z"))?,
        };
        if !pos.is_valid() {
            return Err(invalid("player position out of loaded world"));
        }
        Ok(pos)
    }
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if !self.is_valid() {
            return Err(invalid("invalid player position"));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        let mut f = File::create(&tmp)?;
        let data = json!({"version":1,"x":self.x,"y":self.y,"z":self.z}).to_string();
        f.write_all(data.as_bytes())?;
        f.sync_all()?;
        drop(f);
        fs::rename(tmp, path)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn boundaries() {
        assert!(PlayerPosition::default().is_valid());
        assert!(!(PlayerPosition {
            x: f64::NAN,
            y: 65.0,
            z: 0.5
        })
        .is_valid());
        assert!(!(PlayerPosition {
            x: 48.0,
            y: 65.0,
            z: 0.5
        })
        .is_valid());
        assert!(!(PlayerPosition {
            x: 0.5,
            y: -1.0,
            z: 0.5
        })
        .is_valid());
    }
    #[test]
    fn saves_roundtrip() {
        let parent = std::env::temp_dir().join(format!("ir1710-player-{}", std::process::id()));
        let file = PlayerPosition::location(&parent.join("flat.irw"), "Test_Player");
        let _ = fs::remove_dir_all(&parent);
        let pos = PlayerPosition {
            x: 4.5,
            y: 65.0,
            z: 2.5,
        };
        pos.save(&file).unwrap();
        assert_eq!(PlayerPosition::load(&file).unwrap(), pos);
        fs::write(&file, "broken").unwrap();
        assert!(PlayerPosition::load(&file).is_err());
        fs::remove_dir_all(parent).unwrap();
    }
}
