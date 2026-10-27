//! Standalone 1.7.10 player position persistence using the independently licensed
//! fastnbt crate (MIT OR Apache-2.0), not Pumpkin source code.
//! This remains a prototype and is not a full vanilla player.dat implementation.
use crate::vitals::Vitals;
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

pub const SPAWN_Y: f64 = 65.0;
const MAX_COMPRESSED_BYTES: u64 = 8192;
const MAX_DECOMPRESSED_BYTES: u64 = 32768;
const MAX_LEGACY_BYTES: u64 = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlayerPosition {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

#[derive(Deserialize)]
struct PositionNbt {
    #[serde(rename = "Pos")]
    pos: Vec<f64>,
    #[serde(rename = "Dimension", default)]
    dimension: i32,
    #[serde(rename = "Health", default = "full_health")]
    health: f32,
    #[serde(rename = "foodLevel", default = "full_food")]
    food: i32,
    #[serde(rename = "foodSaturationLevel", default = "default_saturation")]
    saturation: f32,
    #[serde(rename = "foodExhaustionLevel", default)]
    exhaustion: f32,
    #[serde(rename = "foodTickTimer", default)]
    food_timer: i32,
}

fn full_health() -> f32 {
    20.0
}
fn full_food() -> i32 {
    20
}
fn default_saturation() -> f32 {
    5.0
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
    #[serde(rename = "foodLevel")]
    food: i32,
    #[serde(rename = "foodSaturationLevel")]
    saturation: f32,
    #[serde(rename = "foodExhaustionLevel")]
    exhaustion: f32,
    #[serde(rename = "foodTickTimer")]
    food_timer: i32,
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
        let limit = f64::from(crate::world::WORLD_LIMIT);
        self.x.is_finite()
            && self.y.is_finite()
            && self.z.is_finite()
            && (0.0..256.0).contains(&self.y)
            && (-limit..limit).contains(&self.x)
            && (-limit..limit).contains(&self.z)
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

    /// Verified online profiles are stored by UUID in a separate namespace.
    /// This prevents an authenticated player inheriting saves written under
    /// an impersonated username while the server was in offline mode.
    pub fn authenticated_location(world_file: &Path, uuid: &str) -> io::Result<PathBuf> {
        let bytes = uuid.as_bytes();
        if bytes.len() != 36
            || bytes.iter().enumerate().any(|(i, byte)| {
                if matches!(i, 8 | 13 | 18 | 23) {
                    *byte != b'-'
                } else {
                    !byte.is_ascii_hexdigit()
                }
            })
        {
            return Err(invalid("invalid authenticated player UUID"));
        }
        let parent = world_file
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        Ok(parent
            .join("players")
            .join("verified")
            .join(format!("{}.dat", uuid.to_ascii_lowercase())))
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

    pub fn load_state(path: &Path) -> io::Result<(Self, Vitals)> {
        Self::load_state_with_dimension(path).map(|(position, vitals, _)| (position, vitals))
    }

    /// Loads the saved dimension without trusting unrecognized NBT dimension IDs.
    pub fn load_state_with_dimension(path: &Path) -> io::Result<(Self, Vitals, i32)> {
        let meta = match fs::metadata(path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                let legacy = path.with_extension("json");
                return match fs::metadata(&legacy) {
                    Ok(_) => Self::load_legacy_json(&legacy)
                        .map(|position| (position, Vitals::default(), 0)),
                    Err(e) if e.kind() == ErrorKind::NotFound => {
                        Ok((Self::default(), Vitals::default(), 0))
                    }
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
        if !matches!(nbt.dimension, -1..=1) {
            return Err(invalid("unsupported saved player dimension"));
        }
        let vitals = Vitals {
            health: nbt.health,
            food: nbt.food,
            saturation: nbt.saturation,
            exhaustion: nbt.exhaustion,
            food_timer: u32::try_from(nbt.food_timer)
                .map_err(|_| invalid("invalid food tick timer"))?,
        };
        if !vitals.valid() {
            return Err(invalid("invalid player health or food values"));
        }
        Ok((pos, vitals, nbt.dimension))
    }

    #[allow(dead_code)] // Backward-compatible Overworld-only API for legacy callers/tests.
    pub fn save_state(&self, path: &Path, vitals: Vitals) -> io::Result<()> {
        self.save_state_in_dimension(path, vitals, 0)
    }

    /// Produce the exact gzip-encoded player NBT used by normal saves.
    /// This allows one redo transaction to contain food/HP and an inventory
    /// debit without making either file visible before both are prepared.
    pub fn encoded_state_in_dimension(
        &self,
        vitals: Vitals,
        dimension: i32,
    ) -> io::Result<Vec<u8>> {
        if !matches!(dimension, -1..=1) {
            return Err(invalid("unsupported player dimension"));
        }
        if !vitals.valid() {
            return Err(invalid("invalid player health or food values"));
        }
        if !self.is_valid() {
            return Err(invalid("invalid player position"));
        }
        let record = PlayerNbt {
            pos: vec![self.x, self.y, self.z],
            motion: vec![0.0, 0.0, 0.0],
            rotation: vec![0.0, 0.0],
            dimension,
            game_mode: 1,
            health: vitals.health,
            food: vitals.food,
            saturation: vitals.saturation,
            exhaustion: vitals.exhaustion,
            food_timer: vitals.food_timer as i32,
            save_version: 2,
        };
        let binary =
            fastnbt::to_bytes(&record).map_err(|_| invalid("could not serialize player NBT"))?;
        if binary.len() as u64 > MAX_DECOMPRESSED_BYTES {
            return Err(invalid("encoded player NBT too large"));
        }
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&binary)?;
        let compressed = encoder.finish()?;
        if compressed.len() as u64 > MAX_COMPRESSED_BYTES {
            return Err(invalid("compressed player NBT too large"));
        }
        Ok(compressed)
    }

    /// Persists the position, vitals and active dimension in the same NBT write.
    pub fn save_state_in_dimension(
        &self,
        path: &Path,
        vitals: Vitals,
        dimension: i32,
    ) -> io::Result<()> {
        let encoded = self.encoded_state_in_dimension(vitals, dimension)?;
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension("tmp");
        let mut output = File::create(&temp)?;
        output.write_all(&encoded)?;
        output.flush()?;
        output.sync_all()?;
        drop(output);
        fs::rename(temp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verified_uuid_saves_cannot_reuse_offline_username_saves() {
        let world = Path::new("world/flat.irw");
        let id = "d3a0a8b1-4753-4a66-b9c8-f4ac1234c9ca";
        let verified = PlayerPosition::authenticated_location(world, id).unwrap();
        assert_eq!(
            verified,
            Path::new("world/players/verified/d3a0a8b1-4753-4a66-b9c8-f4ac1234c9ca.dat")
        );
        assert_ne!(verified, PlayerPosition::location(world, "Operator"));
        assert!(PlayerPosition::authenticated_location(world, "../danger").is_err());
    }
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
            x: f64::from(crate::world::WORLD_LIMIT),
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
    fn dimension_survives_player_restart_and_rejects_unknown_ids() {
        let parent = std::env::temp_dir().join(format!("ir1710-dimension-{}", std::process::id()));
        let _ = fs::remove_dir_all(&parent);
        let file = parent.join("player.dat");
        let position = PlayerPosition {
            x: -100.5,
            y: 75.0,
            z: 22.5,
        };
        for dimension in [-1, 0, 1] {
            position
                .save_state_in_dimension(&file, Vitals::default(), dimension)
                .unwrap();
            assert_eq!(
                PlayerPosition::load_state_with_dimension(&file).unwrap(),
                (position, Vitals::default(), dimension)
            );
        }
        assert!(position
            .save_state_in_dimension(&file, Vitals::default(), 2)
            .is_err());
        fs::remove_dir_all(parent).unwrap();
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
        pos.save_state(&file, Vitals::default()).unwrap();
        assert!(fs::read(&file).unwrap().starts_with(&[0x1f, 0x8b]));
        assert_eq!(PlayerPosition::load_state(&file).unwrap().0, pos);
        fs::write(&file, b"broken").unwrap();
        assert!(PlayerPosition::load_state(&file).is_err());
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn persisted_health_is_preserved_and_invalid_values_are_rejected() {
        let parent = std::env::temp_dir().join(format!("ir1710-health-{}", std::process::id()));
        let _ = fs::remove_dir_all(&parent);
        fs::create_dir_all(&parent).unwrap();
        let file = parent.join("player.dat");
        let position = PlayerPosition::default();
        position
            .save_state(
                &file,
                Vitals {
                    health: 7.0,
                    food: 8,
                    saturation: 2.0,
                    ..Vitals::default()
                },
            )
            .unwrap();
        assert_eq!(
            PlayerPosition::load_state(&file).unwrap(),
            (
                position,
                Vitals {
                    health: 7.0,
                    food: 8,
                    saturation: 2.0,
                    ..Vitals::default()
                }
            )
        );
        position
            .save_state(
                &file,
                Vitals {
                    health: 0.0,
                    ..Vitals::default()
                },
            )
            .unwrap();
        assert_eq!(PlayerPosition::load_state(&file).unwrap().1.health, 0.0);
        assert!(position
            .save_state(
                &file,
                Vitals {
                    health: f32::NAN,
                    ..Vitals::default()
                }
            )
            .is_err());
        assert!(position
            .save_state(
                &file,
                Vitals {
                    health: 21.0,
                    ..Vitals::default()
                }
            )
            .is_err());
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
        let pos = PlayerPosition::load_state(&file).unwrap().0;
        assert_eq!(pos.x, 7.5);
        pos.save_state(&file, Vitals::default()).unwrap();
        assert!(legacy.exists());
        assert_eq!(PlayerPosition::load_state(&file).unwrap().0, pos);
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
        assert!(PlayerPosition::load_state(&file).is_err());
        fs::remove_dir_all(parent).unwrap();
    }
}
