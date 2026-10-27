//! Durable world-wide spawn point for Minecraft Java 1.7.10.
//! The native IRW world format has no mutable level.dat spawn fields. Keep
//! this setting in a bounded sidecar until level.dat write support exists.
use crate::player::PlayerPosition;
use crate::world::{self, World};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

const VERSION: u8 = 1;
const MAX_BYTES: u64 = 256;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldSpawn {
    version: u8,
    x: i32,
    y: i32,
    z: i32,
}

impl WorldSpawn {
    pub fn new(x: i32, y: i32, z: i32) -> io::Result<Self> {
        if !world::in_bounds(x, y, z) {
            return Err(invalid("world spawn outside supported bounds"));
        }
        Ok(Self {
            version: VERSION,
            x,
            y,
            z,
        })
    }

    pub fn from_position(position: PlayerPosition) -> io::Result<Self> {
        if !position.is_valid() {
            return Err(invalid("invalid world spawn position"));
        }
        Self::new(
            position.x.floor() as i32,
            position.y.floor() as i32,
            position.z.floor() as i32,
        )
    }

    pub fn coordinates(self) -> (i32, i32, i32) {
        (self.x, self.y, self.z)
    }

    /// Protocol 5 S05 Spawn Position: X, Y, Z as three big-endian i32.
    pub fn packet(self) -> Vec<u8> {
        let mut payload = Vec::with_capacity(12);
        for component in [self.x, self.y, self.z] {
            payload.extend_from_slice(&component.to_be_bytes());
        }
        payload
    }

    /// A configured coordinate can be a surface block, or air above ground.
    /// Don't respawn inside opaque terrain, water or a 200-block fall.
    pub fn safe_player_position(self, world: &World) -> Option<PlayerPosition> {
        let allowed = |x: i32, y: i32, z: i32| -> bool {
            if !world::in_bounds(x, y + 1, z) || y < 1 {
                return false;
            }
            let ground = world.get(x, y - 1, z) >> 4;
            ground != 0
                && !matches!(ground, 8..=11 | 30..=32)
                && world.get(x, y, z) == 0
                && world.get(x, y + 1, z) == 0
        };
        // 1.7.10 commands commonly specify the top of the surface block.
        let y = if allowed(self.x, self.y, self.z) {
            self.y
        } else if self.y < 255 && allowed(self.x, self.y + 1, self.z) {
            self.y + 1
        } else {
            return None;
        };
        let player = PlayerPosition {
            x: f64::from(self.x) + 0.5,
            y: f64::from(y),
            z: f64::from(self.z) + 0.5,
        };
        player.is_valid().then_some(player)
    }
}

fn location(world_path: &Path) -> PathBuf {
    world_path.with_extension("worldspawn.json")
}

/// Missing file means the original spawn at x=0,z=0. Corrupted settings are
/// an error rather than silently changing where players appear.
pub fn load(world_path: &Path) -> io::Result<Option<WorldSpawn>> {
    let path = location(world_path);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_file() || metadata.len() > MAX_BYTES {
        return Err(invalid("invalid world spawn sidecar"));
    }
    let bytes = fs::read(&path)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid("oversized world spawn sidecar"));
    }
    let point: WorldSpawn =
        serde_json::from_slice(&bytes).map_err(|_| invalid("corrupt world spawn sidecar"))?;
    if point.version != VERSION || Self::valid(point).is_none() {
        return Err(invalid("invalid world spawn record"));
    }
    Ok(Some(point))
}

struct SelfCheck;
impl SelfCheck {
    fn valid(point: WorldSpawn) -> Option<WorldSpawn> {
        (WorldSpawn::new(point.x, point.y, point.z).ok() == Some(point)).then_some(point)
    }
}

/// Stored state always precedes world-wide packet publication. The existing
/// value is left untouched if validation or any disk operation fails.
pub fn save(world_path: &Path, point: WorldSpawn) -> io::Result<()> {
    if SelfCheck::valid(point).is_none() {
        return Err(invalid("invalid world spawn point"));
    }
    let path = location(world_path);
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(&point)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid("oversized world spawn serialization"));
    }
    let temp = path.with_extension("worldspawn.tmp");
    let mut writer = OpenOptions::new().write(true).create_new(true).open(&temp)?;
    if let Err(error) = writer.write_all(&bytes).and_then(|_| writer.sync_all()) {
        drop(writer);
        let _ = fs::remove_file(temp);
        return Err(error);
    }
    drop(writer);
    if let Err(error) = fs::rename(&temp, &path) {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    #[cfg(unix)]
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

pub fn effective(world_path: &Path, world: &World) -> io::Result<WorldSpawn> {
    match load(world_path)? {
        Some(spawn) => Ok(spawn),
        None => WorldSpawn::new(0, world.spawn_y() as i32 - 1, 0),
    }
}

pub fn player_spawn(world_path: &Path, world: &World) -> io::Result<PlayerPosition> {
    let point = effective(world_path, world)?;
    Ok(point.safe_player_position(world).unwrap_or(PlayerPosition {
        x: 0.5,
        y: world.spawn_y(),
        z: 0.5,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_spawn_packets_and_safe_surface() {
        let world = World::default();
        let existing = effective(Path::new("/tmp/nonexistent-worldspawn-in-test.irw"), &world);
        assert!(existing.is_ok());
        let spawn = WorldSpawn::new(10, 64, -20).unwrap();
        assert_eq!(spawn.packet().len(), 12);
        assert_eq!(spawn.packet()[0..4], 10i32.to_be_bytes());
        assert!(spawn.safe_player_position(&world).is_some());
        assert!(WorldSpawn::new(0, -1, 0).is_err());
        assert!(WorldSpawn::new(i32::MAX, 65, 0).is_err());
    }

    #[test]
    fn world_spawn_persists_and_rejects_corruption() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-worldspawn-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let path = root.join("world.irw");
        let point = WorldSpawn::new(-8, 64, 9).unwrap();
        assert_eq!(load(&path).unwrap(), None);
        save(&path, point).unwrap();
        assert_eq!(load(&path).unwrap(), Some(point));
        let sidecar = location(&path);
        fs::write(&sidecar, b"{\"version\":1,\"x\":0,\"y\":-5,\"z\":0}").unwrap();
        assert!(load(&path).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
