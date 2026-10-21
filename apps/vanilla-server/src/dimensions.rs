//! IRW4 dimension identities, disjoint files and safe coordinate conversion.
//! Minecraft 1.7.10 dimension IDs: Nether=-1, Overworld=0, End=1.
// These APIs are preparatory until the multi-world session router is connected.
#![allow(dead_code)]
use crate::world::{World, WORLD_LIMIT};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[repr(i8)]
pub enum Dimension {
    Nether = -1,
    Overworld = 0,
    End = 1,
}
impl Dimension {
    pub fn protocol_id(self) -> i8 {
        self as i8
    }
    pub fn from_protocol(value: i8) -> Option<Self> {
        match value {
            -1 => Some(Self::Nether),
            0 => Some(Self::Overworld),
            1 => Some(Self::End),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Overworld => "overworld",
            Self::Nether => "nether",
            Self::End => "end",
        }
    }
}
/// Preserves the three 1.7.10 worlds under a directory tree. A different
/// world identity or seed is always an error, never an implicit conversion.
#[derive(Clone, Debug)]
pub struct DimensionManager {
    root: PathBuf,
    seed: u64,
}
impl DimensionManager {
    pub fn new(root: impl AsRef<Path>, seed: u64) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            seed,
        }
    }
    pub fn path(&self, dimension: Dimension) -> PathBuf {
        match dimension {
            Dimension::Overworld => self.root.join("overworld.irw"),
            Dimension::Nether => self.root.join("DIM-1").join("nether.irw"),
            Dimension::End => self.root.join("DIM1").join("end.irw"),
        }
    }
    pub fn load_or_create(&self, dimension: Dimension) -> io::Result<World> {
        let path = self.path(dimension);
        if path.exists() {
            let loaded = World::load(&path)?;
            if loaded.extended_identity() != Some((self.seed, dimension)) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "IRW4 dimension identity/seed mismatch",
                ));
            }
            Ok(loaded)
        } else {
            let fresh = World::expanded(self.seed, dimension);
            fresh.save_new(&path)?;
            Ok(fresh)
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortalDestination {
    pub dimension: Dimension,
    pub x: f64,
    pub y: f64,
    pub z: f64,
}
impl PortalDestination {
    pub fn valid(self) -> bool {
        let limit = WORLD_LIMIT as f64;
        self.x.is_finite()
            && self.y.is_finite()
            && self.z.is_finite()
            && (-limit..limit).contains(&self.x)
            && (-limit..limit).contains(&self.z)
            && (1.0..255.0).contains(&self.y)
    }
}
/// Coordinate ratio is precisely 8:1 when crossing Overworld/Nether.
/// The End uses its own spawn platform, never an arbitrary scaled position.
/// This chooses the search anchor; finding/building a safe portal frame is a
/// separate gameplay operation.
pub fn destination(
    source: Dimension,
    target: Dimension,
    x: f64,
    y: f64,
    z: f64,
) -> Option<PortalDestination> {
    if !x.is_finite() || !y.is_finite() || !z.is_finite() || !(0.0..256.0).contains(&y) {
        return None;
    }
    let scale = match (source, target) {
        (Dimension::Overworld, Dimension::Nether) => 1.0 / 8.0,
        (Dimension::Nether, Dimension::Overworld) => 8.0,
        _ => 1.0,
    };
    let (x, y, z) = if target == Dimension::End || source == Dimension::End {
        (0.5, 80.0, 0.5)
    } else {
        (x * scale, y.clamp(32.0, 120.0), z * scale)
    };
    let limit = WORLD_LIMIT as f64 - 2.0;
    Some(PortalDestination {
        dimension: target,
        x: x.clamp(-limit, limit),
        y,
        z: z.clamp(-limit, limit),
    })
}
/// Finds a standable arrival close to a portal's coordinate anchor without
/// assuming that the anchor itself is air. Search is finite, deterministic,
/// and limited to the destination World; it never creates chunks or writes
/// blocks. A caller must refuse teleportation when no safe space exists.
pub fn safe_arrival(world: &World, anchor: PortalDestination) -> Option<PortalDestination> {
    if !anchor.valid() {
        return None;
    }
    let (cx, cz) = (anchor.x.floor() as i32, anchor.z.floor() as i32);
    let desired_y = anchor.y.floor() as i32;
    for radius in 0i32..=8 {
        for dx in -radius..=radius {
            for dz in -radius..=radius {
                if dx.abs().max(dz.abs()) != radius {
                    continue;
                }
                let (x, z) = (cx + dx, cz + dz);
                for offset in 0..=112 {
                    for y in [desired_y - offset, desired_y + offset] {
                        if !(2..=254).contains(&y) || !crate::world::in_bounds(x, y, z) {
                            continue;
                        }
                        // Conservative, full-footprint ground: never accept
                        // fluids, fire, plants, portals or non-solid tile entities.
                        let ground = world.get(x, y - 1, z) >> 4;
                        if !matches!(
                            ground,
                            1 | 2 | 3 | 4 | 5 | 7 | 12 | 13 | 24 | 49 | 87 | 88 | 89 | 112 | 121
                        ) {
                            continue;
                        }
                        if world.get(x, y, z) != 0 || world.get(x, y + 1, z) != 0 {
                            continue;
                        }
                        let chosen = PortalDestination {
                            dimension: anchor.dimension,
                            x: f64::from(x) + 0.5,
                            y: f64::from(y),
                            z: f64::from(z) + 0.5,
                        };
                        if chosen.valid() {
                            return Some(chosen);
                        }
                    }
                }
            }
        }
    }
    None
}
/// Durable travel checkpoint for the session/router layer. The session must
/// save player inventory/vitals and send S07 Respawn before changing worlds.
/// The checkpoint never replaces a player.dat (it is supplemental).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TravelCheckpoint {
    version: u8,
    pub destination: PortalDestination,
}
impl TravelCheckpoint {
    pub fn new(destination: PortalDestination) -> Option<Self> {
        destination.valid().then_some(Self {
            version: 1,
            destination,
        })
    }
    pub fn load(path: &Path) -> io::Result<Option<Self>> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        if bytes.len() > 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "oversized travel checkpoint",
            ));
        }
        let checkpoint: Self = serde_json::from_slice(&bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid travel checkpoint"))?;
        if checkpoint.version != 1 || !checkpoint.destination.valid() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid travel checkpoint identity",
            ));
        }
        Ok(Some(checkpoint))
    }
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if self.version != 1 || !self.destination.valid() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid travel destination",
            ));
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let staging = path.with_extension("dimension.tmp");
        let mut file = File::create(&staging)?;
        file.write_all(&serde_json::to_vec(self)?)?;
        file.sync_all()?;
        drop(file);
        fs::rename(staging, path)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nether_overworld_scale_and_bounds() {
        let p = destination(Dimension::Overworld, Dimension::Nether, 160.0, 75.0, -80.0).unwrap();
        assert_eq!(p.x, 20.0);
        assert_eq!(p.z, -10.0);
        let reverse = destination(p.dimension, Dimension::Overworld, p.x, p.y, p.z).unwrap();
        assert_eq!(reverse.x, 160.0);
        assert_eq!(reverse.z, -80.0);
        assert!(
            destination(Dimension::Nether, Dimension::Overworld, f64::NAN, 70.0, 0.0).is_none()
        );
        assert!(
            destination(Dimension::Nether, Dimension::Overworld, 1e8, 70.0, 0.0)
                .unwrap()
                .valid()
        );
    }
    #[test]
    fn world_identification_prevents_dimension_cross_load() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-dims-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let manager = DimensionManager::new(&root, 42);
        for d in [Dimension::Overworld, Dimension::Nether, Dimension::End] {
            let mut world = manager.load_or_create(d).unwrap();
            assert_eq!(world.extended_identity(), Some((42, d)));
            world
                .update_and_save(&manager.path(d), 1, 74, 1, 1 << 4)
                .unwrap();
            assert_eq!(manager.load_or_create(d).unwrap().get(1, 74, 1), 1 << 4);
        }
        assert!(DimensionManager::new(&root, 43)
            .load_or_create(Dimension::Nether)
            .is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn safe_portal_arrival_requires_real_floor_and_headroom() {
        let flat = World::new();
        let anchor = PortalDestination {
            dimension: Dimension::Overworld,
            x: 0.5,
            y: 80.0,
            z: 0.5,
        };
        assert_eq!(safe_arrival(&flat, anchor).unwrap().y, 64.0);
        let end = World::expanded(42, Dimension::End);
        let anchor = PortalDestination {
            dimension: Dimension::End,
            x: 0.5,
            y: 80.0,
            z: 0.5,
        };
        let destination = safe_arrival(&end, anchor).unwrap();
        let y = destination.y as i32;
        let x = destination.x.floor() as i32;
        let z = destination.z.floor() as i32;
        assert_ne!(end.get(x, y - 1, z), 0);
        assert_eq!(end.get(x, y, z), 0);
        assert_eq!(end.get(x, y + 1, z), 0);
        assert!(safe_arrival(
            &flat,
            PortalDestination {
                x: f64::NAN,
                ..anchor
            }
        )
        .is_none());
    }
    #[test]
    fn checkpoint_roundtrip() {
        let file = std::env::temp_dir().join(format!("ir1710-travel-{}.json", std::process::id()));
        let dest = destination(Dimension::Overworld, Dimension::Nether, -80.0, 64.0, 80.0).unwrap();
        let check = TravelCheckpoint::new(dest).unwrap();
        check.save(&file).unwrap();
        assert_eq!(
            TravelCheckpoint::load(&file).unwrap().unwrap().destination,
            dest
        );
        fs::remove_file(file).unwrap();
    }
}
