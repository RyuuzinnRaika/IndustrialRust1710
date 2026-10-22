//! J93: read-only integrity check; does not create a session lock or replay redo journals.
//! This validates the formats used by the runtime, not arbitrary modded worlds.
use crate::{brewing::Brewers, chests::Chests, clock::Clock, experience::Experience,
    furnaces::Furnaces, ground_items::GroundItems, hoppers::Hoppers,
    inventory::Inventory, legacy_world_inspect, mobs::Mobs, player::PlayerPosition,
    world::World};
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

#[derive(Debug)]
pub struct Summary {
    pub description: &'static str,
    pub files: usize,
}

fn checked_file(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_file() => Ok(true),
        Ok(_) => Err(invalid("world contains symbolic link or non-regular file")),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}
fn checked_dir(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_dir() => Ok(true),
        Ok(_) => Err(invalid("world contains symbolic link or non-directory")),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}
fn inspect_irw(world_file: &Path) -> io::Result<Summary> {
    if !checked_file(world_file)? { return Err(invalid("IRW world file missing")); }
    let parent = world_file.parent().unwrap_or_else(|| Path::new("."));
    let world = World::load(world_file)?;
    let mut count = 1usize;
    // Redo journal must be recovered by an exclusive runtime, never by this checker.
    for file in [
        world_file.with_extension("pending-txn.json"),
        Chests::location(world_file).with_extension("pending-txn.json"),
        Furnaces::location(world_file).with_extension("pending-txn.json"),
    ] {
        if checked_file(&file)? {
            return Err(invalid("unfinished transaction journal; offline recovery required"));
        }
    }
    macro_rules! check {
        ($path:expr, $loader:expr) => {{
            let path: PathBuf = $path;
            if checked_file(&path)? { $loader?; count += 1; }
        }};
    }
    check!(world_file.with_extension("clock"), Clock::load_state(&world_file.with_extension("clock")));
    let mobs_path = world_file.with_extension("mobs.json");
    check!(mobs_path.clone(), Mobs::load_or_new(&world, false, 1, &mobs_path));
    let drops = GroundItems::location(world_file);
    check!(drops.clone(), GroundItems::load(&drops));
    let chests = Chests::location(world_file);
    check!(chests.clone(), Chests::load(&chests));
    let furnaces = Furnaces::location(world_file);
    check!(furnaces.clone(), Furnaces::load(&furnaces));
    let hoppers = Hoppers::location(world_file);
    check!(hoppers.clone(), Hoppers::load(&hoppers));
    let brewers = Brewers::location(world_file);
    check!(brewers.clone(), Brewers::load(&brewers));

    let player_dir = parent.join("players");
    if checked_dir(&player_dir)? {
        let mut player_files = 0usize;
        for entry in fs::read_dir(&player_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !checked_file(&path)? { return Err(invalid("players folder contains unsupported entry")); }
            player_files += 1;
            if player_files > 10_000 { return Err(invalid("too many player files")); }
            let name = entry.file_name();
            let Some(name) = name.to_str() else { return Err(invalid("non-UTF8 player filename")); };
            if name.ends_with(".dat") {
                let stem = name.trim_end_matches(".dat");
                if !crate::whitelist::valid_name(stem) || stem != stem.to_ascii_lowercase() {
                    return Err(invalid("invalid player filename"));
                }
                PlayerPosition::load_state(&path)?;
                count += 1;
                let inventory = Inventory::location(&path);
                check!(inventory.clone(), Inventory::load(&inventory));
                let survival = path.with_extension("survival.inventory.json");
                check!(survival.clone(), Inventory::load(&survival));
                let xp = path.with_extension("experience.json");
                check!(xp.clone(), Experience::load(&xp));
            }
        }
    }
    Ok(Summary { description: "IRW", files: count })
}

/// Inspect the source as either a native Anvil directory or an IRW file/parent.
/// This function must never acquire a write lock, create files or replay recovery.
pub fn inspect(source: &Path) -> io::Result<Summary> {
    match fs::symlink_metadata(source) {
        Ok(meta) if meta.file_type().is_file() => {
            if source.extension().is_none_or(|x| x != "irw") {
                return Err(invalid("expected .irw file"));
            }
            inspect_irw(source)
        }
        Ok(meta) if meta.file_type().is_dir() => {
            let vanilla = checked_file(&source.join("level.dat"))?;
            let irw = checked_file(&source.join("flat.irw"))?;
            if vanilla && irw {
                return Err(invalid("mixed Anvil and IRW world: refuse ambiguous storage"));
            }
            if vanilla {
                let result = legacy_world_inspect::inspect_world(source)?;
                Ok(Summary { description: "Anvil 1.7.10", files: result.regions + result.player_files + 1 })
            } else if irw {
                inspect_irw(&source.join("flat.irw"))
            } else {
                Err(invalid("world directory has neither level.dat nor flat.irw"))
            }
        }
        Ok(_) => Err(invalid("world path must be a real directory or regular IRW file")),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspect_detects_corruption_without_creating_session_lock() {
        let root = std::env::temp_dir().join(format!("j93-world-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let save = root.join("flat.irw");
        World::new().save_new(&save).unwrap();
        assert_eq!(inspect(&root).unwrap().description, "IRW");
        assert!(!root.join("session.lock").exists());
        fs::write(save.with_extension("clock"), b"invalid clock").unwrap();
        assert!(inspect(&root).is_err());
        assert!(!root.join("session.lock").exists());
        fs::remove_file(save.with_extension("clock")).unwrap();
        fs::write(save.with_extension("pending-txn.json"), b"unfinished").unwrap();
        assert!(inspect(&root).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
