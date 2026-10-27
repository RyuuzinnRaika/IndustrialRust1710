//! Per-player /spawnpoint override for Minecraft Java 1.7.10.
//! Kept outside the position NBT so normal movement, death, and portal saves
//! cannot accidentally erase a configured respawn point. The owning player
//! session is the only writer; verified profiles have separate UUID paths.
use crate::player::PlayerPosition;
use crate::world::{self, World};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, ErrorKind, Write};
use std::path::Path;

const VERSION: u8 = 1;
const MAX_BYTES: u64 = 256;

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, reason)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spawnpoint {
    version: u8,
    dimension: i32,
    x: i32,
    y: i32,
    z: i32,
}

impl Spawnpoint {
    pub fn new(x: i32, y: i32, z: i32) -> io::Result<Self> {
        if !(1..=254).contains(&y) || !world::in_bounds(x, y, z) {
            return Err(invalid("spawnpoint outside safe Overworld coordinates"));
        }
        Ok(Self {
            version: VERSION,
            dimension: 0,
            x,
            y,
            z,
        })
    }

    pub fn from_position(position: PlayerPosition) -> io::Result<Self> {
        if !position.is_valid() {
            return Err(invalid("invalid player position for spawnpoint"));
        }
        Self::new(
            position.x.floor() as i32,
            position.y.floor() as i32,
            position.z.floor() as i32,
        )
    }

    pub fn coords(self) -> (i32, i32, i32) {
        (self.x, self.y, self.z)
    }

    pub fn position(self) -> PlayerPosition {
        PlayerPosition {
            x: f64::from(self.x) + 0.5,
            y: f64::from(self.y),
            z: f64::from(self.z) + 0.5,
        }
    }

    /// Only support clear-air, supported-ground spawn positions initially.
    /// Reject lava, water and unbounded positions rather than respawning
    /// players inside a block or into the wrong dimension.
    pub fn safe_in(self, world: &World) -> bool {
        if !world::in_bounds(self.x, self.y + 1, self.z) {
            return false;
        }
        // The legacy flat spawn is one block above the nearest support
        // surface. Accept at most a one-block drop, but never an unbounded fall.
        let has_ground = (1..=2).any(|distance| {
            if self.y < distance {
                return false;
            }
            let block = world.get(self.x, self.y - distance, self.z) >> 4;
            block != 0 && !matches!(block, 8..=11 | 30..=32)
        });
        has_ground
            && world.get(self.x, self.y, self.z) == 0
            && world.get(self.x, self.y + 1, self.z) == 0
    }
}

fn storage_path(player_file: &Path) -> std::path::PathBuf {
    player_file.with_extension("spawnpoint.json")
}

pub fn load(player_file: &Path) -> io::Result<Option<Spawnpoint>> {
    let path = storage_path(player_file);
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Err(invalid("invalid spawnpoint file type or length"));
    }
    let raw = fs::read(&path)?;
    if raw.len() as u64 > MAX_BYTES {
        return Err(invalid("spawnpoint file exceeds maximum length"));
    }
    let parsed: Spawnpoint =
        serde_json::from_slice(&raw).map_err(|_| invalid("invalid spawnpoint JSON"))?;
    if parsed.version != VERSION
        || parsed.dimension != 0
        || Spawnpoint::new(parsed.x, parsed.y, parsed.z).ok() != Some(parsed)
    {
        return Err(invalid("invalid or incompatible spawnpoint record"));
    }
    Ok(Some(parsed))
}

pub fn save(player_file: &Path, spawnpoint: Spawnpoint) -> io::Result<()> {
    if Spawnpoint::new(spawnpoint.x, spawnpoint.y, spawnpoint.z).ok() != Some(spawnpoint) {
        return Err(invalid("invalid spawnpoint to save"));
    }
    let path = storage_path(player_file);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes =
        serde_json::to_vec(&spawnpoint).map_err(|_| invalid("spawnpoint serialization failed"))?;
    let temp = path.with_extension("spawnpoint.tmp");
    let mut file = File::create(&temp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    // Never remove the original first: failure must preserve the prior spawn.
    fs::rename(temp, path)
}

pub fn clear(player_file: &Path) -> io::Result<()> {
    match fs::remove_file(storage_path(player_file)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinate_validation_and_centering() {
        assert!(Spawnpoint::new(0, 65, 0).is_ok());
        assert!(Spawnpoint::new(0, 0, 0).is_err());
        assert!(Spawnpoint::new(0, 255, 0).is_err());
        assert!(Spawnpoint::new(i32::MAX, 65, 0).is_err());
        let point = Spawnpoint::new(-2, 65, 3).unwrap();
        assert_eq!(point.position().x, -1.5);
        assert_eq!(point.coords(), (-2, 65, 3));
        let flat = World::default();
        assert!(point.safe_in(&flat));
    }

    #[test]
    fn persistence_and_corruption_fail_closed() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-spawnpoint-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let player = root.join("players").join("alice.dat");
        assert_eq!(load(&player).unwrap(), None);
        let p = Spawnpoint::new(-2, 65, 3).unwrap();
        save(&player, p).unwrap();
        assert_eq!(load(&player).unwrap(), Some(p));
        fs::write(
            storage_path(&player),
            b"{\"version\":1,\"dimension\":-1,\"x\":0,\"y\":65,\"z\":0}",
        )
        .unwrap();
        assert!(load(&player).is_err());
        save(&player, p).unwrap();
        clear(&player).unwrap();
        assert_eq!(load(&player).unwrap(), None);
        fs::remove_dir_all(root).unwrap();
    }
}
