//! Standalone 1.7.10 player position persistence using the independently licensed
//! fastnbt crate (MIT OR Apache-2.0), not Pumpkin source code.
//! This remains a prototype and is not a full vanilla player.dat implementation.
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

pub const SPAWN_Y: f64 = 65.0;
const MAX_COMPRESSED_BYTES: u64 = 8192;
const MAX_DECOMPRESSED_BYTES: u64 = 32768;
const MAX_LEGACY_BYTES: u64 = 2048;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerPosition {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

#[derive(Deserialize)]
struct PositionNbt {
    #[serde(rename = "Pos")]
    pos: Vec<f64>,
}

#[derive(Serialize)]
struct PlayerNbt {
    #[serde(rename = "Pos")]
    pos: Vec<f64>,
    #[serde(rename = "Motion")]
    motion: Vec<f64>,
    #[serde(rename = "Rotation")]
    rotation: Vec<f32>,
    #[serde(rename = "Dimension")]
    dimension: i32,
    #[serde(rename = "playerGameType")]
    game_mode: i32,
    #[serde(rename = "Health")]
    health: f32,
    #[serde(rename = "IndustrialRustSaveVersion")]
    save_version: i32,
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
        // The caller validated this name with whitelist::valid_name.
        let parent = world_file
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        parent
            .join("players")
            .join(format!("{}.dat", username.to_ascii_lowercase()))
    }

    fn load_legacy_json(path: &Path) -> io::Result<Self> {
        let meta = fs::metadata(path)?;
        if !meta.is_file() || meta.len() > MAX_LEGACY_BYTES {
            return Err(invalid("invalid legacy player JSON file size/type"));
        }
        let raw = fs::read(path)?;
        if raw.len() as u64 > MAX_LEGACY_BYTES {
            return Err(invalid("legacy player file is too large"));
        }
        let value: serde_json::Value =
            serde_json::from_slice(&raw).map_err(|_| invalid("invalid legacy player JSON"))?;
        if value.get("version").and_then(serde_json::Value::as_u64) != Some(1) {
            return Err(invalid("unsupported legacy player version"));
        }
        let pos = Self {
            x: value
                .get("x")
                .and_then(serde_json::Value::as_f64)
                .ok_or_else(|| invalid("missing x"))?,
            y: value
                .get("y")
                .and_then(serde_json::Value::as_f64)
                .ok_or_else(|| invalid("missing y"))?,
            z: value
                .get("z")
                .and_then(serde_json::Value::as_f64)
                .ok_or_else(|| invalid("missing z"))?,
        };
        if !pos.is_valid() {
            return Err(invalid("legacy player location outside world"));
        }
        Ok(pos)
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        let meta = match fs::metadata(path) {
            Ok(meta) => meta,
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
        if !meta.is_file() || meta.len() > MAX_COMPRESSED_BYTES {
            return Err(invalid("invalid player NBT file size/type"));
        }
        let mut decompressed = Vec::new();
        let file = File::open(path)?;
        let decoder = GzDecoder::new(file);
        decoder
            .take(MAX_DECOMPRESSED_BYTES + 1)
            .read_to_end(&mut decompressed)?;
        if decompressed.len() as u64 > MAX_DECOMPRESSED_BYTES {
            return Err(invalid("NBT decompression limit exceeded"));
        }
        let nbt: PositionNbt = fastnbt::from_bytes(&decompressed)
            .map_err(|_| invalid("invalid player NBT document"))?;
        if nbt.pos.len() != 3 {
            return Err(invalid("player Pos must contain three coordinates"));
        }
        let pos = Self {
            x: nbt.pos[0],
            y: nbt.pos[1],
            z: nbt.pos[2],
        };
        if !pos.is_valid() {
            return Err(invalid("player location outside loaded world"));
        }
        Ok(pos)
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if !self.is_valid() {
            return Err(invalid("invalid player position"));
        }
        let record = PlayerNbt {
            pos: vec![self.x, self.y, self.z],
            motion: vec![0.0, 0.0, 0.0],
            rotation: vec![0.0, 0.0],
            dimension: 0,
            game_mode: 1,
            health: 20.0,
            save_version: 1,
        };
        let binary =
            fastnbt::to_bytes(&record).map_err(|_| invalid("could not serialize player NBT"))?;
        if binary.len() as u64 > MAX_DECOMPRESSED_BYTES {
            return Err(invalid("encoded player NBT too large"));
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension("tmp");
        let file = File::create(&temp)?;
        let mut encoder = GzEncoder::new(file, Compression::default());
        encoder.write_all(&binary)?;
        let mut completed = encoder.finish()?;
        completed.flush()?;
        completed.sync_all()?;
        if fs::metadata(&temp)?.len() > MAX_COMPRESSED_BYTES {
            return Err(invalid("compressed player NBT too large"));
        }
        fs::rename(temp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_locations() {
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
        let parent = std::env::temp_dir().join(format!("ir1710-permissive-{}", std::process::id()));
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
        fs::write(&file, b"broken").unwrap();
        assert!(PlayerPosition::load(&file).is_err());
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn migrates_legacy_json_without_removing_source() {
        let parent =
            std::env::temp_dir().join(format!("ir1710-permissive-migrate-{}", std::process::id()));
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

    #[test]
    fn rejects_oversized_gzip_output() {
        let parent =
            std::env::temp_dir().join(format!("ir1710-permissive-size-{}", std::process::id()));
        fs::create_dir_all(&parent).unwrap();
        let file = parent.join("oversized.dat");
        let out = File::create(&file).unwrap();
        let mut zipped = GzEncoder::new(out, Compression::default());
        zipped.write_all(&vec![0u8; 32769]).unwrap();
        zipped.finish().unwrap();
        assert!(PlayerPosition::load(&file).is_err());
        fs::remove_dir_all(parent).unwrap();
    }
}
