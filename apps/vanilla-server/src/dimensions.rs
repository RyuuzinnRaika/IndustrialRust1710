//! IRW4 dimension identities, disjoint files and safe coordinate conversion.
//! Minecraft 1.7.10 dimension IDs: Nether=-1, Overworld=0, End=1.
// This module owns independent world handles; Protocol 5 session switching is
// intentionally separate and must not be assumed by callers.
#![allow(dead_code)]
use crate::world::{World, WORLD_LIMIT};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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
    /// A real directory at the final root is not sufficient: an ancestor
    /// symlink can still redirect all dimension files outside the locked
    /// world tree. Reject every existing non-directory path component before
    /// loading, recovering or creating any dimension files.
    fn validate_root_ancestors(&self) -> io::Result<()> {
        for ancestor in self.root.ancestors() {
            if ancestor.as_os_str().is_empty() {
                continue;
            }
            match fs::symlink_metadata(ancestor) {
                Ok(meta) if !meta.file_type().is_dir() => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "unsafe dimension root ancestor directory",
                    ));
                }
                Ok(_) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }
    /// Inspect an existing dimension without creating any missing files.
    /// Symlinks (including broken links) and non-regular world files are
    /// rejected before World::load can follow them. The server must hold a
    /// session lock on this root while loading and creating dimensions.
    fn inspect_existing(&self, dimension: Dimension) -> io::Result<Option<World>> {
        let invalid = |reason| io::Error::new(io::ErrorKind::InvalidData, reason);
        self.validate_root_ancestors()?;
        match fs::symlink_metadata(&self.root) {
            Ok(meta) if !meta.file_type().is_dir() => {
                return Err(invalid("dimension root is not a real directory"));
            }
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        // Never create prototype files alongside a real Minecraft level.
        crate::server_config::verify_legacy_storage(&self.path(Dimension::Overworld))?;
        let path = self.path(dimension);
        if let Some(parent) = path.parent() {
            match fs::symlink_metadata(parent) {
                Ok(meta) if !meta.file_type().is_dir() => {
                    return Err(invalid("dimension parent is not a real directory"));
                }
                Ok(_) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
        }
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        // Current IRW4 limits: at most 65,536 block edits and 4,096 TNT
        // records. Reject excessive input before loading it into memory.
        if !meta.file_type().is_file() || meta.len() > 1_048_576 {
            return Err(invalid("unsafe or oversized IRW4 dimension file"));
        }
        let loaded = World::load(&path)?;
        if loaded.extended_identity() != Some((self.seed, dimension)) {
            return Err(invalid("IRW4 dimension identity/seed mismatch"));
        }
        Ok(Some(loaded))
    }

    fn create_new(&self, dimension: Dimension) -> io::Result<World> {
        self.validate_root_ancestors()?;
        let fresh = World::expanded(self.seed, dimension);
        fresh.save_new(&self.path(dimension))?;
        Ok(fresh)
    }

    pub fn load_or_create(&self, dimension: Dimension) -> io::Result<World> {
        match self.inspect_existing(dimension)? {
            Some(world) => Ok(world),
            None => self.create_new(dimension),
        }
    }
}

/// Three independent, live IRW4 world handles. All three are loaded and checked
/// against the same generation seed before the caller exposes any handle.
/// The server's session router can retain these handles between transitions;
/// loading each world for every teleport would lose unsaved state.
///
/// Ownership of session.lock and recovery of any pending journals remains the
/// responsibility of the server bootstrap before calling `load_all`.
#[derive(Debug)]
pub struct DimensionWorlds {
    manager: DimensionManager,
    paths: [PathBuf; 3],
    overworld: Arc<Mutex<World>>,
    nether: Arc<Mutex<World>>,
    end: Arc<Mutex<World>>,
}

impl DimensionWorlds {
    pub fn load_all(manager: DimensionManager) -> io::Result<Self> {
        manager.validate_root_ancestors()?;
        // Recover a pending three-dimension save before looking at any
        // world snapshot. The server is responsible for holding session.lock.
        // Reject prototype storage inside native Anvil level directories.
        crate::server_config::verify_legacy_storage(&manager.path(Dimension::Overworld))?;
        match fs::symlink_metadata(&manager.root) {
            Ok(meta) if !meta.file_type().is_dir() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "dimension root is not a real directory",
                ));
            }
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        crate::journal::recover(&manager.root.join("dimensions.pending-txn.json"))?;
        // Preflight all existing dimensions *before* writing any new one.
        // A corrupt End must not leave a newly created Overworld or Nether
        // behind after an otherwise failed server startup.
        let existing_overworld = manager.inspect_existing(Dimension::Overworld)?;
        let existing_nether = manager.inspect_existing(Dimension::Nether)?;
        let existing_end = manager.inspect_existing(Dimension::End)?;
        let overworld = match existing_overworld {
            Some(world) => world,
            None => manager.create_new(Dimension::Overworld)?,
        };
        let nether = match existing_nether {
            Some(world) => world,
            None => manager.create_new(Dimension::Nether)?,
        };
        let end = match existing_end {
            Some(world) => world,
            None => manager.create_new(Dimension::End)?,
        };
        let paths = [
            manager.path(Dimension::Overworld),
            manager.path(Dimension::Nether),
            manager.path(Dimension::End),
        ];
        Ok(Self {
            manager,
            paths,
            overworld: Arc::new(Mutex::new(overworld)),
            nether: Arc::new(Mutex::new(nether)),
            end: Arc::new(Mutex::new(end)),
        })
    }

    pub fn world(&self, dimension: Dimension) -> Arc<Mutex<World>> {
        match dimension {
            Dimension::Overworld => Arc::clone(&self.overworld),
            Dimension::Nether => Arc::clone(&self.nether),
            Dimension::End => Arc::clone(&self.end),
        }
    }

    pub fn path(&self, dimension: Dimension) -> PathBuf {
        self.manager.path(dimension)
    }

    /// Borrow the live world without constructing a new lock or loading a disk snapshot.
    pub fn world_ref(&self, dimension: Dimension) -> &Mutex<World> {
        match dimension {
            Dimension::Overworld => self.overworld.as_ref(),
            Dimension::Nether => self.nether.as_ref(),
            Dimension::End => self.end.as_ref(),
        }
    }

    /// Stable path reference for a session that switches worlds while connected.
    pub fn path_ref(&self, dimension: Dimension) -> &Path {
        match dimension {
            Dimension::Overworld => &self.paths[0],
            Dimension::Nether => &self.paths[1],
            Dimension::End => &self.paths[2],
        }
    }

    /// Bind all dimension writes to one root-level session lock. The router
    /// must never keep an unbound secondary world alive while accepting players.
    /// Each World verifies its own path before publishing a durable mutation.
    pub fn bind_save_sessions(
        &self,
        session: Arc<Mutex<crate::session_lock::WorldSessionLock>>,
    ) -> io::Result<()> {
        for dimension in [Dimension::Overworld, Dimension::Nether, Dimension::End] {
            self.world(dimension)
                .lock()
                .map_err(|_| io::Error::other("dimension world mutex poisoned"))?
                .bind_save_session(&self.path(dimension), Arc::clone(&session))?;
        }
        Ok(())
    }

    /// Commit all three IRW4 snapshots as a crash-recoverable transaction.
    /// A caller must quiesce writers before taking snapshots and hold the
    /// server's session lock until commit finishes. If commit fails, stop
    /// further writes and let load_all replay the prepared redo journal.
    pub fn save_all(&self) -> io::Result<()> {
        let dimensions = [Dimension::Overworld, Dimension::Nether, Dimension::End];
        let paths = dimensions.map(|dim| self.manager.path(dim));
        let snapshots = dimensions.map(|dim| {
            self.world(dim)
                .lock()
                .map_err(|_| io::Error::other("dimension world mutex poisoned"))?
                .encoded()
        });
        let [overworld, nether, end] = snapshots;
        crate::journal::commit(
            &self.manager.root.join("dimensions.pending-txn.json"),
            &[
                (paths[0].as_path(), overworld?),
                (paths[1].as_path(), nether?),
                (paths[2].as_path(), end?),
            ],
        )
    }

    /// Validate the block *and* its intact source frame before attempting travel.
    /// The caller owns packet sequencing, per-dimension entity routing and the
    /// player's durable inventory/session checkpoint. No state is changed here.
    pub fn resolve_portal(
        &self,
        source: Dimension,
        x: i32,
        y: i32,
        z: i32,
    ) -> io::Result<Option<PortalDestination>> {
        let target = {
            let source_world = self.world(source);
            let guard = source_world
                .lock()
                .map_err(|_| io::Error::other("dimension world mutex poisoned"))?;
            portal_target(&guard, x, y, z)
        };
        match target {
            Some(target) => self.resolve_arrival(
                source,
                target,
                f64::from(x) + 0.5,
                f64::from(y),
                f64::from(z) + 0.5,
            ),
            None => Ok(None),
        }
    }

    /// Resolve a portal destination against the actual target world, without
    /// touching source world state. None means that the transition must be
    /// rejected; never teleport to a solid, liquid or out-of-bounds block.
    pub fn resolve_arrival(
        &self,
        source: Dimension,
        target: Dimension,
        x: f64,
        y: f64,
        z: f64,
    ) -> io::Result<Option<PortalDestination>> {
        let Some(anchor) = destination(source, target, x, y, z) else {
            return Ok(None);
        };
        let target_world = self.world(target);
        let guard = target_world
            .lock()
            .map_err(|_| io::Error::other("dimension world mutex poisoned"))?;
        Ok(safe_arrival(&guard, anchor))
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
    if source == target {
        return None;
    }
    let limit = WORLD_LIMIT as f64;
    if !x.is_finite()
        || !y.is_finite()
        || !z.is_finite()
        || !(0.0..256.0).contains(&y)
        || !(-limit..limit).contains(&x)
        || !(-limit..limit).contains(&z)
    {
        return None;
    }
    let scale = match (source, target) {
        (Dimension::Overworld, Dimension::Nether) => 1.0 / 8.0,
        (Dimension::Nether, Dimension::Overworld) => 8.0,
        (Dimension::Overworld, Dimension::End) | (Dimension::End, Dimension::Overworld) => 1.0,
        // There is no direct Nether <-> End travel in Minecraft 1.7.10.
        _ => return None,
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
    if !anchor.valid() || world.dimension() != anchor.dimension {
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
                        // The Nether bedrock ceiling is not a safe arrival
                        // platform even if its top has two blocks of air.
                        if !(2..=254).contains(&y)
                            || (anchor.dimension == Dimension::Nether && y > 120)
                            || !crate::world::in_bounds(x, y, z)
                        {
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
/// A Nether portal is an obsidian-bordered rectangle of 2..=21 portal
/// blocks wide and 3..=21 tall. Test both possible horizontal orientations;
/// inspecting only a portal block is insufficient because it may be orphaned
/// by a frame break. Reads are bounded and never generate or edit a world.
fn valid_nether_frame(world: &World, x: i32, y: i32, z: i32, dx: i32, dz: i32) -> bool {
    let id = |u, v| world.get(x + u * dx, v, z + u * dz) >> 4;
    if id(0, y) != 90 {
        return false;
    }
    let mut left = 0;
    while left < 21 && id(-left - 1, y) == 90 {
        left += 1;
    }
    let mut down = 0;
    while down < 21 && id(0, y - down - 1) == 90 {
        down += 1;
    }
    let (origin, floor) = (-left, y - down);
    let mut width = 0;
    while width < 22 && id(origin + width, floor) == 90 {
        width += 1;
    }
    let mut height = 0;
    while height < 22 && id(origin, floor + height) == 90 {
        height += 1;
    }
    if !(2..=21).contains(&width) || !(3..=21).contains(&height) {
        return false;
    }
    for u in -1..=width {
        for v in -1..=height {
            // Minecraft 1.7.10 allows the four decorative frame corners
            // to be absent; only edges along the interior are mandatory.
            if (u == -1 || u == width) && (v == -1 || v == height) {
                continue;
            }
            let block = id(origin + u, floor + v);
            let expected = if u == -1 || u == width || v == -1 || v == height {
                49 // Obsidian frame
            } else {
                90 // Portal interior
            };
            if block != expected {
                return false;
            }
        }
    }
    true
}

/// The 1.7.10 End gateway from the Overworld uses a 3x3 portal plane and
/// twelve surrounding End Portal Frame blocks with filled eyes (metadata 4).
/// The frames must also face inward, matching the activation check in
/// world/special.rs; a filled but misoriented ring is not a valid gateway.
fn valid_end_entry(world: &World, x: i32, y: i32, z: i32) -> bool {
    if world.get(x, y, z) >> 4 != 119 {
        return false;
    }
    for offset_x in 0..3 {
        for offset_z in 0..3 {
            let min_x = x - offset_x;
            let min_z = z - offset_z;
            let mut intact = true;
            for dx in 0..3 {
                for dz in 0..3 {
                    intact &= world.get(min_x + dx, y, min_z + dz) >> 4 == 119;
                }
            }
            for i in 0..3 {
                // 1.7.10 frame metadata: 0=S, 1=W, 2=N, 3=E.
                // Each side must face the center, not merely contain an eye.
                for (fx, fz, facing) in [
                    (min_x - 1, min_z + i, 3),
                    (min_x + 3, min_z + i, 1),
                    (min_x + i, min_z - 1, 0),
                    (min_x + i, min_z + 3, 2),
                ] {
                    let state = world.get(fx, y, fz);
                    intact &= state >> 4 == 120 && state & 4 != 0 && state & 3 == facing;
                }
            }
            if intact {
                return true;
            }
        }
    }
    false
}

/// Recognize a physically intact source portal. This only resolves where a
/// transition *could* go; a server must not emit a Respawn packet until the
/// destination world and the per-player state have both been prepared.
pub fn portal_target(world: &World, x: i32, y: i32, z: i32) -> Option<Dimension> {
    if !crate::world::in_bounds(x, y, z) {
        return None;
    }
    let kind = world.get(x, y, z) >> 4;
    match (world.dimension(), kind) {
        (Dimension::Overworld, 90)
            if valid_nether_frame(world, x, y, z, 1, 0)
                || valid_nether_frame(world, x, y, z, 0, 1) =>
        {
            Some(Dimension::Nether)
        }
        (Dimension::Nether, 90)
            if valid_nether_frame(world, x, y, z, 1, 0)
                || valid_nether_frame(world, x, y, z, 0, 1) =>
        {
            Some(Dimension::Overworld)
        }
        (Dimension::Overworld, 119) if valid_end_entry(world, x, y, z) => Some(Dimension::End),
        // The return portal is constructed from bedrock after the dragon dies.
        // Restrict recognition to the central island and supporting bedrock.
        (Dimension::End, 119)
            if x.abs() <= 8 && z.abs() <= 8 && world.get(x, y - 1, z) >> 4 == 7 =>
        {
            Some(Dimension::Overworld)
        }
        _ => None,
    }
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
    /// Reject symlinks and non-directories at *every existing* parent level.
    /// Checking only the immediate parent misses an ancestor symlink when
    /// the checkpoint is stored under a nested directory.
    fn validate_parent_components(path: &Path, kind: io::ErrorKind) -> io::Result<()> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            for ancestor in parent.ancestors() {
                if ancestor.as_os_str().is_empty() {
                    continue;
                }
                match fs::symlink_metadata(ancestor) {
                    Ok(meta) if !meta.file_type().is_dir() => {
                        return Err(io::Error::new(
                            kind,
                            "unsafe travel checkpoint ancestor directory",
                        ));
                    }
                    Ok(_) => {}
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                    Err(err) => return Err(err),
                }
            }
        }
        Ok(())
    }

    pub fn new(destination: PortalDestination) -> Option<Self> {
        destination.valid().then_some(Self {
            version: 1,
            destination,
        })
    }
    pub fn load(path: &Path) -> io::Result<Option<Self>> {
        Self::validate_parent_components(path, io::ErrorKind::InvalidData)?;
        let meta = match fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        // Never follow a substituted checkpoint symlink or read an unbounded
        // attacker-created file into memory before validating its size.
        if !meta.file_type().is_file() || meta.len() > 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsafe or oversized travel checkpoint",
            ));
        }
        let bytes = fs::read(path)?;
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
        // Validate before mkdir to avoid creating directories through an
        // existing ancestor symlink, then revalidate the newly created path.
        Self::validate_parent_components(path, io::ErrorKind::InvalidInput)?;
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
            Self::validate_parent_components(path, io::ErrorKind::InvalidInput)?;
        }
        // Loading refuses symlinks and non-files; saving must not silently
        // replace them either. Check before creating the staging file.
        match fs::symlink_metadata(path) {
            Ok(meta) if !meta.file_type().is_file() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unsafe travel checkpoint destination",
                ));
            }
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        let staging = path.with_extension("dimension.tmp");
        // create_new refuses a pre-existing staging file (including symlinks).
        // A stale file from a crash must be investigated, not overwritten.
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)?;
        file.write_all(&serde_json::to_vec(self)?)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&staging, path)?;
        // The checkpoint's contents are durable after file.sync_all(), but
        // the renamed directory entry also needs syncing on Unix. Without
        // this, a power loss can roll back the checkpoint name after success.
        #[cfg(unix)]
        {
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn portal_detection_rejects_orphaned_and_broken_nether_frames() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-portal-frame-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("world.irw");
        let mut world = World::expanded(7, Dimension::Overworld);
        world.save_new(&path).unwrap();

        // X-aligned, two-block-wide and three-high interior, with obsidian ring.
        for u in -1..=2 {
            for v in -1..=3 {
                let block = if u == -1 || u == 2 || v == -1 || v == 3 {
                    49
                } else {
                    90
                };
                world
                    .update_and_save(&path, u, 80 + v, 0, block << 4)
                    .unwrap();
            }
        }
        assert_eq!(portal_target(&world, 0, 81, 0), Some(Dimension::Nether));
        // A missing corner does not invalidate a legitimate 1.7.10 portal.
        world.update_and_save(&path, -1, 79, 0, 0).unwrap();
        assert_eq!(portal_target(&world, 0, 81, 0), Some(Dimension::Nether));
        world.update_and_save(&path, -1, 81, 0, 0).unwrap();
        assert_eq!(portal_target(&world, 0, 81, 0), None);
        assert_eq!(portal_target(&world, 0, -1, 0), None);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn nether_portal_z_axis_crosses_negative_chunk_boundary_without_corners() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-z-portal-boundary-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("world.irw");
        let mut world = World::expanded(31, Dimension::Overworld);
        world.save_new(&path).unwrap();

        // The interior spans Z=15..16, across a chunk boundary, at X=-16.
        // Frame corners are optional in Minecraft 1.7.10.
        for z in 14..=17 {
            for y in 79..=83 {
                if (z == 14 || z == 17) && (y == 79 || y == 83) {
                    continue;
                }
                let id = if z == 14 || z == 17 || y == 79 || y == 83 {
                    49
                } else {
                    90
                };
                world.update_and_save(&path, -16, y, z, id << 4).unwrap();
            }
        }
        for z in 15..=16 {
            assert_eq!(portal_target(&world, -16, 81, z), Some(Dimension::Nether));
        }

        // A broken side frame invalidates the entire portal.
        world.update_and_save(&path, -16, 81, 14, 0).unwrap();
        assert_eq!(portal_target(&world, -16, 81, 15), None);
        world.update_and_save(&path, -16, 81, 14, 49 << 4).unwrap();

        // A missing interior tile also invalidates the other interior tile.
        world.update_and_save(&path, -16, 81, 16, 0).unwrap();
        assert_eq!(portal_target(&world, -16, 81, 15), None);
        world.update_and_save(&path, -16, 81, 16, 90 << 4).unwrap();
        assert_eq!(portal_target(&world, -16, 81, 15), Some(Dimension::Nether));
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn end_entry_requires_all_twelve_filled_frames() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-end-frame-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("world.irw");
        let mut world = World::expanded(19, Dimension::Overworld);
        world.save_new(&path).unwrap();
        for x in 0..3 {
            for z in 0..3 {
                world.update_and_save(&path, x, 75, z, 119 << 4).unwrap();
            }
        }
        for i in 0..3 {
            for (x, z, facing) in [(-1, i, 3), (3, i, 1), (i, -1, 0), (i, 3, 2)] {
                world
                    .update_and_save(&path, x, 75, z, (120 << 4) | 4 | facing)
                    .unwrap();
            }
        }
        assert_eq!(portal_target(&world, 1, 75, 1), Some(Dimension::End));
        // An eye-filled frame facing away from the portal must invalidate
        // travel, even if the portal blocks have not been removed yet.
        world
            .update_and_save(&path, -1, 75, 1, (120 << 4) | 4 | 1)
            .unwrap();
        assert_eq!(portal_target(&world, 1, 75, 1), None);
        world
            .update_and_save(&path, -1, 75, 1, (120 << 4) | 4 | 3)
            .unwrap();
        assert_eq!(portal_target(&world, 1, 75, 1), Some(Dimension::End));
        world.update_and_save(&path, -1, 75, 1, 120 << 4).unwrap();
        assert_eq!(portal_target(&world, 1, 75, 1), None);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn nether_arrival_never_uses_bedrock_roof() {
        let nether = World::expanded(17, Dimension::Nether);
        let arrival = safe_arrival(
            &nether,
            PortalDestination {
                dimension: Dimension::Nether,
                x: 0.5,
                y: 120.0,
                z: 0.5,
            },
        );
        assert!(arrival.is_none_or(|position| position.y <= 120.0));
        assert!(destination(Dimension::Overworld, Dimension::Overworld, 0.0, 64.0, 0.0).is_none());
    }
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
        assert!(destination(Dimension::Nether, Dimension::Overworld, 1e8, 70.0, 0.0).is_none());
        // Valid source coordinates still clamp the 8:1 destination at the
        // target world edge without generating out-of-bounds chunks.
        assert!(
            destination(Dimension::Nether, Dimension::Overworld, 16_380.0, 70.0, 0.0)
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
    fn checkpoint_refuses_oversized_input_without_parsing() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-checkpoint-bounds-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("pending.json");
        fs::write(&path, vec![0u8; 2048]).unwrap();
        assert_eq!(
            TravelCheckpoint::load(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData,
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            fs::remove_file(&path).unwrap();
            let target = root.join("target.json");
            fs::write(&target, "{}").unwrap();
            symlink(&target, &path).unwrap();
            assert_eq!(
                TravelCheckpoint::load(&path).unwrap_err().kind(),
                io::ErrorKind::InvalidData,
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn checkpoint_replacement_retains_latest_destination_without_staging() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-travel-replace-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("pending.json");
        let first = TravelCheckpoint::new(
            destination(Dimension::Overworld, Dimension::Nether, 80.0, 64.0, 0.0).unwrap(),
        )
        .unwrap();
        first.save(&path).unwrap();
        let replacement = TravelCheckpoint::new(
            destination(Dimension::Overworld, Dimension::End, 80.0, 64.0, 0.0).unwrap(),
        )
        .unwrap();
        replacement.save(&path).unwrap();
        assert_eq!(
            TravelCheckpoint::load(&path).unwrap().unwrap().destination,
            replacement.destination
        );
        assert!(!path.with_extension("dimension.tmp").exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn checkpoint_save_rejects_symlink_parent_and_destination() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "ir1710-checkpoint-symlink-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        let checkpoint = TravelCheckpoint::new(
            destination(Dimension::Overworld, Dimension::Nether, 80.0, 64.0, 0.0).unwrap(),
        )
        .unwrap();

        let linked_parent = root.join("linked-parent");
        symlink(&outside, &linked_parent).unwrap();
        let redirected = linked_parent.join("pending.json");
        assert_eq!(
            checkpoint.save(&redirected).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(!outside.join("pending.json").exists());

        let victim = outside.join("victim.json");
        fs::write(&victim, "keep").unwrap();
        let linked_file = root.join("pending.json");
        symlink(&victim, &linked_file).unwrap();
        assert_eq!(
            checkpoint.save(&linked_file).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
        let link_meta = fs::symlink_metadata(&linked_file).unwrap();
        assert!(link_meta.file_type().is_symlink());
        assert!(!linked_file.with_extension("dimension.tmp").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn checkpoint_rejects_symlinked_ancestor_before_nested_directory_creation() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "ir1710-checkpoint-ancestor-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let outside = root.join("outside");
        fs::create_dir_all(outside.join("nested")).unwrap();
        let checkpoint = TravelCheckpoint::new(
            destination(Dimension::Overworld, Dimension::Nether, 80.0, 64.0, 0.0).unwrap(),
        )
        .unwrap();
        let original = outside.join("nested/pending.json");
        checkpoint.save(&original).unwrap();
        let before = fs::read(&original).unwrap();

        let linked = root.join("linked");
        symlink(&outside, &linked).unwrap();
        let redirected = linked.join("nested/pending.json");
        assert_eq!(
            TravelCheckpoint::load(&redirected).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            checkpoint.save(&redirected).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(fs::read(&original).unwrap(), before);
        assert!(!redirected.with_extension("dimension.tmp").exists());

        let missing = linked.join("new/child/pending.json");
        assert_eq!(
            checkpoint.save(&missing).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(!outside.join("new").exists());

        let regular = root.join("regular/child/pending.json");
        checkpoint.save(&regular).unwrap();
        assert_eq!(
            TravelCheckpoint::load(&regular)
                .unwrap()
                .unwrap()
                .destination,
            checkpoint.destination
        );
        fs::remove_dir_all(root).unwrap();
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
    #[test]
    fn refuses_cross_dimension_arrival_and_invalid_source_coordinates() {
        let overworld = World::expanded(42, Dimension::Overworld);
        let nether_anchor = PortalDestination {
            dimension: Dimension::Nether,
            x: 0.5,
            y: 80.0,
            z: 0.5,
        };
        assert!(safe_arrival(&overworld, nether_anchor).is_none());
        for (x, y, z) in [
            (f64::INFINITY, 70.0, 0.0),
            (16_384.0, 70.0, 0.0),
            (-16_385.0, 70.0, 0.0),
            (0.0, -1.0, 0.0),
            (0.0, 256.0, 0.0),
        ] {
            assert!(destination(Dimension::Overworld, Dimension::Nether, x, y, z).is_none());
        }
    }

    #[cfg(unix)]
    #[test]
    fn dimension_root_rejects_symlinked_ancestor_without_writing_worlds() {
        use std::os::unix::fs::symlink;

        let base = std::env::temp_dir().join(format!(
            "ir1710-dimension-ancestor-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&base);
        let real = base.join("real");
        fs::create_dir_all(&real).unwrap();
        symlink(&real, base.join("alias")).unwrap();

        // The final world directory does not exist yet, so checking only
        // symlink_metadata(root) would miss the redirected ancestor.
        let redirected = DimensionManager::new(base.join("alias/worlds"), 42);
        assert_eq!(
            DimensionWorlds::load_all(redirected.clone())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            redirected
                .load_or_create(Dimension::Overworld)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert!(!real.join("worlds").exists());

        // The same nested path must work when every ancestor is real.
        let normal = DimensionManager::new(real.join("worlds"), 42);
        let worlds = DimensionWorlds::load_all(normal).unwrap();
        assert!(worlds.path(Dimension::Overworld).is_file());
        drop(worlds);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn preflight_rejects_corrupt_end_before_creating_other_dimensions() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-preflight-end-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let wrong_seed = DimensionManager::new(&root, 9);
        wrong_seed.load_or_create(Dimension::End).unwrap();
        let expected = DimensionManager::new(&root, 10);
        assert_eq!(
            DimensionWorlds::load_all(expected.clone())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert!(!expected.path(Dimension::Overworld).exists());
        assert!(!expected.path(Dimension::Nether).exists());
        assert!(expected.path(Dimension::End).exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn preflight_rejects_unsafe_paths_and_vanilla_world_without_writes() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-preflight-path-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let manager = DimensionManager::new(&root, 42);
        fs::write(root.join("level.dat"), b"vanilla").unwrap();
        assert!(DimensionWorlds::load_all(manager.clone()).is_err());
        assert!(!manager.path(Dimension::Overworld).exists());
        fs::remove_file(root.join("level.dat")).unwrap();
        fs::create_dir_all(manager.path(Dimension::End)).unwrap();
        assert_eq!(
            DimensionWorlds::load_all(manager.clone())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert!(!manager.path(Dimension::Overworld).exists());
        fs::remove_dir_all(manager.path(Dimension::End)).unwrap();
        fs::remove_dir(root.join("DIM1")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink(
                root.join("missing-world"),
                manager.path(Dimension::Overworld),
            )
            .unwrap();
            assert_eq!(
                DimensionWorlds::load_all(manager.clone())
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidData
            );
            fs::remove_file(manager.path(Dimension::Overworld)).unwrap();
            symlink(&root, root.join("DIM1")).unwrap();
            assert_eq!(
                DimensionWorlds::load_all(manager.clone())
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidData
            );
            fs::remove_file(root.join("DIM1")).unwrap();
        }
        assert!(!manager.path(Dimension::Overworld).exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn preflight_loads_existing_world_and_creates_only_missing_dimensions() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-preflight-mixed-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let manager = DimensionManager::new(&root, 42);
        let mut nether = manager.load_or_create(Dimension::Nether).unwrap();
        nether
            .update_and_save(&manager.path(Dimension::Nether), 8, 65, 8, 49 << 4)
            .unwrap();
        let worlds = DimensionWorlds::load_all(manager.clone()).unwrap();
        assert_eq!(
            worlds
                .world(Dimension::Nether)
                .lock()
                .unwrap()
                .get(8, 65, 8),
            49 << 4
        );
        for dim in [Dimension::Overworld, Dimension::Nether, Dimension::End] {
            assert!(worlds.path(dim).is_file());
            assert_eq!(
                worlds.world(dim).lock().unwrap().extended_identity(),
                Some((42, dim))
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pending_dimension_save_recovers_all_three_worlds_before_loading() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-dim-recovery-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let manager = DimensionManager::new(&root, 1234);
        drop(DimensionWorlds::load_all(manager.clone()).unwrap());
        let dims = [Dimension::Overworld, Dimension::Nether, Dimension::End];
        let fingerprint = |bytes: &[u8]| {
            bytes.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
            })
        };
        let mut files = Vec::new();
        for (i, dimension) in dims.into_iter().enumerate() {
            let mut world = World::expanded(1234, dimension);
            world.import_block(4, 75, 4, ((i + 1) as u16) << 4).unwrap();
            let bytes = world.encoded().unwrap();
            let relative = match dimension {
                Dimension::Overworld => "overworld.irw",
                Dimension::Nether => "DIM-1/nether.irw",
                Dimension::End => "DIM1/end.irw",
            };
            files.push(serde_json::json!({
                "relative": relative,
                "checksum": fingerprint(&bytes),
                "contents": bytes,
            }));
        }
        // Simulate a crash after the redo record was published but before
        // any of its three snapshots was replaced.
        fs::write(
            root.join("dimensions.pending-txn.json"),
            serde_json::to_vec(&serde_json::json!({
                "version": 1,
                "files": files,
            }))
            .unwrap(),
        )
        .unwrap();
        let recovered = DimensionWorlds::load_all(manager).unwrap();
        for (i, dimension) in dims.into_iter().enumerate() {
            assert_eq!(
                recovered.world(dimension).lock().unwrap().get(4, 75, 4),
                ((i + 1) as u16) << 4
            );
        }
        assert!(!root.join("dimensions.pending-txn.json").exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn multi_world_handles_are_isolated_and_survive_reload() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-multi-world-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let manager = DimensionManager::new(&root, 417);
        let worlds = DimensionWorlds::load_all(manager.clone()).unwrap();
        let blocks = [
            (Dimension::Overworld, 1u16 << 4),
            (Dimension::Nether, 49u16 << 4),
            (Dimension::End, 121u16 << 4),
        ];
        for (dim, block) in blocks {
            worlds
                .world(dim)
                .lock()
                .unwrap()
                .update_and_save(&worlds.path(dim), 2, 75, 3, block)
                .unwrap();
        }
        worlds.save_all().unwrap();
        assert!(worlds
            .resolve_arrival(Dimension::Overworld, Dimension::End, 0.5, 70.0, 0.5,)
            .unwrap()
            .is_some());
        assert!(worlds
            .resolve_arrival(Dimension::Overworld, Dimension::Nether, f64::NAN, 70.0, 0.0,)
            .unwrap()
            .is_none());
        drop(worlds);

        let reloaded = DimensionWorlds::load_all(manager.clone()).unwrap();
        for (dim, block) in blocks {
            assert_eq!(reloaded.world(dim).lock().unwrap().get(2, 75, 3), block);
            assert_eq!(
                reloaded.world(dim).lock().unwrap().extended_identity(),
                Some((417, dim))
            );
        }
        assert!(DimensionWorlds::load_all(DimensionManager::new(&root, 418)).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn live_dimension_handles_share_session_lock_and_reject_wrong_save_path() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-shared-dimension-lock-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let manager = DimensionManager::new(&root, 84);
        let worlds = DimensionWorlds::load_all(manager.clone()).unwrap();
        let session = Arc::new(Mutex::new(
            crate::session_lock::WorldSessionLock::acquire(&root).unwrap(),
        ));
        worlds.bind_save_sessions(Arc::clone(&session)).unwrap();
        {
            let handle = worlds.world(Dimension::Nether);
            let mut nether = handle.lock().unwrap();
            assert!(nether
                .update_and_save(&manager.path(Dimension::Overworld), 5, 75, 5, 49 << 4)
                .is_err());
            assert!(nether
                .update_and_save(&manager.path(Dimension::Nether), 5, 75, 5, 49 << 4)
                .unwrap());
        }
        worlds.save_all().unwrap();
        assert_eq!(
            DimensionWorlds::load_all(manager)
                .unwrap()
                .world(Dimension::Nether)
                .lock()
                .unwrap()
                .get(5, 75, 5),
            49 << 4
        );
        drop(worlds);
        drop(session);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn direct_nether_to_end_travel_is_rejected() {
        assert!(destination(Dimension::Nether, Dimension::End, 0.0, 64.0, 0.0).is_none());
        assert!(destination(Dimension::End, Dimension::Nether, 0.0, 64.0, 0.0).is_none());
        assert!(destination(Dimension::Overworld, Dimension::End, 0.0, 64.0, 0.0).is_some());
        assert!(destination(Dimension::End, Dimension::Overworld, 0.0, 64.0, 0.0).is_some());
    }
}
