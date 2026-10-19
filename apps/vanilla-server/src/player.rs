//! Persistent player location for bounded flat-world prototype.
//! Uses Pumpkin-derived GPLv3 gzip NBT, but is not a complete vanilla player.dat.
//! Older JSON saves from IndustrialRust1710 PR #5 are imported read-only.
use pumpkin_nbt::nbt_compress::{read_gzip_compound_tag, write_gzip_compound_tag_to_bytes};
use pumpkin_nbt::{tag::NbtTag, NbtCompound};
use std::fs::{self, File};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

pub const SPAWN_Y: f64 = 65.0;
const MAX_PLAYER_BYTES: u64 = 8192;
const MAX_LEGACY_BYTES: u64 = 2048;

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
        // username must be validated by whitelist::valid_name before calling.
        let parent = world_file
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        parent
            .join("players")
            .join(format!("{}.dat", username.to_ascii_lowercase()))
    }
    fn load_legacy_json(path: &Path) -> io::Result<Self> {
        let m = fs::metadata(path)?;
        if !m.is_file() || m.len() > MAX_LEGACY_BYTES {
            return Err(invalid("invalid legacy player JSON file size/type"));
        }
        let raw = fs::read(path)?;
        if raw.len() as u64 > MAX_LEGACY_BYTES {
            return Err(invalid("legacy player file is too large"));
        }
        let v: serde_json::Value =
            serde_json::from_slice(&raw).map_err(|_| invalid("invalid legacy player JSON"))?;
        if v.get("version").and_then(serde_json::Value::as_u64) != Some(1) {
            return Err(invalid("unsupported legacy player version"));
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
            return Err(invalid("legacy player location outside world"));
        }
        Ok(pos)
    }
    pub fn load(path: &Path) -> io::Result<Self> {
        let m = match fs::metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                let legacy = path.with_extension("json");
                return match fs::metadata(&legacy) {
                    Ok(_) => Self::load_legacy_json(&legacy),
                    Err(e) if e.kind() == ErrorKind::NotFound => Ok(Self::default()),
                    Err(e) => Err(e),
                };
            }
            Err(e) => return Err(e),
        };
        if !m.is_file() || m.len() > MAX_PLAYER_BYTES {
            return Err(invalid("invalid player NBT file size/type"));
        }
        // Named GZip NBT root. The upstream decoder caps decompression.
        let root = read_gzip_compound_tag(File::open(path)?)
            .map_err(|_| invalid("invalid player NBT file"))?;
        let pos = root
            .get_list("Pos")
            .ok_or_else(|| invalid("missing player Pos NBT tag"))?;
        let [NbtTag::Double(x), NbtTag::Double(y), NbtTag::Double(z)] = pos else {
            return Err(invalid("invalid player Pos NBT list"));
        };
        let position = Self {
            x: *x,
            y: *y,
            z: *z,
        };
        if !position.is_valid() {
            return Err(invalid("player location outside loaded world"));
        }
        Ok(position)
    }
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if !self.is_valid() {
            return Err(invalid("invalid player position"));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut nbt = NbtCompound::new();
        nbt.put_list(
            "Pos",
            vec![
                NbtTag::Double(self.x),
                NbtTag::Double(self.y),
                NbtTag::Double(self.z),
            ],
        );
        nbt.put_list(
            "Motion",
            vec![
                NbtTag::Double(0.0),
                NbtTag::Double(0.0),
                NbtTag::Double(0.0),
            ],
        );
        nbt.put_list("Rotation", vec![NbtTag::Float(0.0), NbtTag::Float(0.0)]);
        nbt.put_int("Dimension", 0);
        nbt.put_int("playerGameType", 1);
        nbt.put_float("Health", 20.0);
        nbt.put_int("IndustrialRustSaveVersion", 1);
        let output = write_gzip_compound_tag_to_bytes(nbt)
            .map_err(|_| invalid("could not encode player NBT"))?;
        if output.len() as u64 > MAX_PLAYER_BYTES {
            return Err(invalid("encoded player NBT too large"));
        }
        let tmp = path.with_extension("tmp");
        let mut f = File::create(&tmp)?;
        f.write_all(&output)?;
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
    fn saves_roundtrip_and_rejects_corruption() {
        let parent = std::env::temp_dir().join(format!("ir1710-player-{}", std::process::id()));
        let file = PlayerPosition::location(&parent.join("flat.irw"), "Test_Player");
        let _ = fs::remove_dir_all(&parent);
        let pos = PlayerPosition {
            x: 4.5,
            y: 65.0,
            z: 2.5,
        };
        pos.save(&file).unwrap();
        assert!(fs::read(&file).unwrap().starts_with(&[0x1f, 0x8b]));
        assert_eq!(PlayerPosition::load(&file).unwrap(), pos);
        fs::write(&file, "broken").unwrap();
        assert!(PlayerPosition::load(&file).is_err());
        fs::remove_dir_all(parent).unwrap();
    }
    #[test]
    fn imports_old_json_without_deleting_it() {
        let parent =
            std::env::temp_dir().join(format!("ir1710-legacy-migrate-{}", std::process::id()));
        let _ = fs::remove_dir_all(&parent);
        let file = PlayerPosition::location(&parent.join("flat.irw"), "Test_Player");
        let legacy = file.with_extension("json");
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(&legacy, r#"{"version":1,"x":7.5,"y":66.0,"z":1.5}"#).unwrap();
        let pos = PlayerPosition::load(&file).unwrap();
        assert_eq!(pos.x, 7.5);
        pos.save(&file).unwrap();
        assert!(legacy.exists());
        assert_eq!(PlayerPosition::load(&file).unwrap(), pos);
        fs::remove_dir_all(parent).unwrap();
    }
}
