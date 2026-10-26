//! J93: read-only integrity check; does not create a session lock or replay redo journals.
//! This validates the formats used by the runtime, not arbitrary modded worlds.
use crate::{
    achievements::Achievements, admin_state, books, brewing::Brewers, chests::Chests, clock::Clock,
    decorations::Decorations, dispensers::Devices, experience::Experience, furnaces::Furnaces,
    game_rules::GameRules, ground_items::GroundItems, hanging::Hangings, hoppers::Hoppers,
    inventory::Inventory, legacy_world_inspect, maps::Maps, minecarts::Minecarts, mobs::Mobs,
    player::PlayerPosition, scoreboard::Scoreboard, world::World,
};
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
/// Cross-file semantic check for tiles with saved inventories. Loading each
/// sidecar validates its own format and item stacks, but a valid inventory
/// at an air/stone block would otherwise hide an orphaned container.
fn verify_tile_anchors<T: serde::Serialize>(
    world: &World,
    tile_state: &T,
    block_ids: &[u16],
) -> io::Result<()> {
    let encoded = serde_json::to_value(tile_state)
        .map_err(|_| invalid("could not inspect tile inventory positions"))?;
    let entries = encoded
        .get("entries")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| invalid("tile inventory entries missing"))?;
    for position in entries.keys() {
        let coords = position
            .split(',')
            .map(str::parse::<i32>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| invalid("bad saved tile inventory position"))?;
        if coords.len() != 3
            || !crate::world::in_bounds(coords[0], coords[1], coords[2])
            || position != &format!("{},{},{}", coords[0], coords[1], coords[2])
        {
            return Err(invalid("invalid saved tile inventory coordinates"));
        }
        if !block_ids.contains(&(world.get(coords[0], coords[1], coords[2]) >> 4)) {
            return Err(invalid(
                "orphaned tile inventory: saved container has no matching world block",
            ));
        }
    }
    Ok(())
}

// Offline user names and authenticated UUIDs have disjoint save namespaces.
// Inspect every recognized sidecar, even when its primary .dat file is absent,
// and reject unknown files instead of silently declaring a partial save valid.
fn inspect_player_directory(
    world_file: &Path,
    directory: &Path,
    verified: bool,
    count: &mut usize,
    skip_verified: bool,
) -> io::Result<()> {
    if !checked_dir(directory)? {
        return Ok(());
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if skip_verified && entry.file_name() == "verified" {
            continue;
        }
        let path = entry.path();
        if !checked_file(&path)? {
            return Err(invalid("player directory contains an unsupported entry"));
        }
        let filename = entry.file_name();
        let filename = filename
            .to_str()
            .ok_or_else(|| invalid("non-UTF8 player filename"))?;
        let suffixes = [
            ".survival.inventory.json",
            ".achievements.json",
            ".experience.json",
            ".inventory.json",
            ".gamemode",
            ".dat",
            ".json",
        ];
        let (suffix_index, stem) = suffixes
            .iter()
            .enumerate()
            .find_map(|(index, suffix)| filename.strip_suffix(suffix).map(|stem| (index, stem)))
            .ok_or_else(|| invalid("unrecognized player save file"))?;
        if verified {
            let canonical = PlayerPosition::authenticated_location(world_file, stem)?;
            if canonical != directory.join(format!("{stem}.dat")) {
                return Err(invalid("non-canonical verified player UUID filename"));
            }
        } else if !crate::whitelist::valid_name(stem) || stem != stem.to_ascii_lowercase() {
            return Err(invalid("invalid offline player filename"));
        }
        match suffix_index {
            0 | 3 => {
                Inventory::load(&path)?;
            }
            1 => {
                Achievements::load(&path)?;
            }
            2 => {
                Experience::load(&path)?;
            }
            4 => {
                admin_state::read(&path, 1)?
                    .ok_or_else(|| invalid("player gamemode disappeared during inspection"))?;
            }
            5 => {
                PlayerPosition::load_state(&path)?;
            }
            6 => {
                // The legacy JSON loader is used only when the NBT save is absent.
                // Refuse ambiguous dual-format player records rather than ignoring one.
                if checked_file(&path.with_extension("dat"))? {
                    return Err(invalid("player has both JSON and NBT positions"));
                }
                PlayerPosition::load_state(&path.with_extension("dat"))?;
            }
            _ => unreachable!(),
        }
        *count = count
            .checked_add(1)
            .ok_or_else(|| invalid("player file count overflow"))?;
        if *count > 10_000 {
            return Err(invalid("too many player save files"));
        }
    }
    Ok(())
}

fn inspect_irw(world_file: &Path) -> io::Result<Summary> {
    if !checked_file(world_file)? {
        return Err(invalid("IRW world file missing"));
    }
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
            return Err(invalid(
                "unfinished transaction journal; offline recovery required",
            ));
        }
    }
    macro_rules! check {
        ($path:expr, $loader:expr) => {{
            let path: PathBuf = $path;
            if checked_file(&path)? {
                $loader?;
                count += 1;
            }
        }};
    }
    check!(
        world_file.with_extension("clock"),
        Clock::load_state(&world_file.with_extension("clock"))
    );
    let mobs_path = world_file.with_extension("mobs.json");
    check!(
        mobs_path.clone(),
        Mobs::load_or_new(&world, false, 1, &mobs_path)
    );
    let drops = GroundItems::location(world_file);
    check!(drops.clone(), GroundItems::load(&drops));
    let chests = Chests::location(world_file);
    check!(chests.clone(), {
        let tiles = Chests::load(&chests)?;
        verify_tile_anchors(&world, &tiles, &[54])
    });
    let furnaces = Furnaces::location(world_file);
    check!(furnaces.clone(), {
        let tiles = Furnaces::load(&furnaces)?;
        verify_tile_anchors(&world, &tiles, &[61, 62])
    });
    let hoppers = Hoppers::location(world_file);
    check!(hoppers.clone(), {
        let tiles = Hoppers::load(&hoppers)?;
        verify_tile_anchors(&world, &tiles, &[154])
    });
    let brewers = Brewers::location(world_file);
    check!(brewers.clone(), Brewers::load(&brewers));
    // Check every sidecar loaded by the live server rather than reporting a
    // healthy world while a different persisted subsystem cannot start.
    let devices = Devices::location(world_file);
    check!(devices.clone(), Devices::load(&devices));
    let decorations = Decorations::location(world_file);
    check!(decorations.clone(), Decorations::load(&decorations));
    let hangings = Hangings::location(world_file);
    check!(hangings.clone(), Hangings::load(&hangings));
    let maps = Maps::location(world_file);
    check!(maps.clone(), Maps::load(&maps));
    let minecarts = Minecarts::location(world_file);
    check!(minecarts.clone(), Minecarts::load(&minecarts, 0));
    let rules = GameRules::location(world_file);
    check!(rules.clone(), GameRules::load(&rules));
    let scoreboard = Scoreboard::location(world_file);
    check!(scoreboard.clone(), Scoreboard::load(&scoreboard));
    let book_path = world_file.with_extension("books.json");
    check!(book_path, books::inspect_save(world_file));
    let difficulty = world_file.with_extension("difficulty");
    check!(difficulty.clone(), admin_state::read(&difficulty, 3));

    let player_dir = parent.join("players");
    if checked_dir(&player_dir)? {
        // The verified subdirectory is the only permitted nested player folder.
        for entry in fs::read_dir(&player_dir)? {
            let entry = entry?;
            if entry.file_name() == "verified" {
                if !checked_dir(&entry.path())? {
                    return Err(invalid("verified player storage must be a real directory"));
                }
            } else if !checked_file(&entry.path())? {
                return Err(invalid("unexpected directory or link inside players"));
            }
        }
        inspect_player_directory(
            world_file,
            &player_dir.join("verified"),
            true,
            &mut count,
            false,
        )?;
        // Offline files live beside the verified namespace; inspect them separately.
        // A temporary collection is unnecessary: just inspect regular root entries.
        inspect_player_directory(world_file, &player_dir, false, &mut count, true)?;
    }
    Ok(Summary {
        description: "IRW",
        files: count,
    })
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
                return Err(invalid(
                    "mixed Anvil and IRW world: refuse ambiguous storage",
                ));
            }
            if vanilla {
                let result = legacy_world_inspect::inspect_world(source)?;
                Ok(Summary {
                    description: "Anvil 1.7.10",
                    files: result.regions + result.player_files + 1,
                })
            } else if irw {
                inspect_irw(&source.join("flat.irw"))
            } else {
                Err(invalid(
                    "world directory has neither level.dat nor flat.irw",
                ))
            }
        }
        Ok(_) => Err(invalid(
            "world path must be a real directory or regular IRW file",
        )),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspect_checks_every_sidecar_and_online_player_namespace() {
        let root = std::env::temp_dir().join(format!(
            "j93-verified-sidecars-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let world = root.join("flat.irw");
        World::new().save_new(&world).unwrap();
        let uuid = "00000000-0000-0000-0000-000000000001";
        let player = PlayerPosition::authenticated_location(&world, uuid).unwrap();
        PlayerPosition::default()
            .save_state(&player, crate::vitals::Vitals::default())
            .unwrap();
        assert_eq!(inspect(&root).unwrap().files, 2);
        for extension in [
            "books.json",
            "decorations.json",
            "dispensers.json",
            "hanging.json",
            "maps.json",
            "minecarts.json",
            "gamerules.json",
            "scoreboard.json",
            "difficulty",
        ] {
            let path = world.with_extension(extension);
            fs::write(&path, b"corrupt").unwrap();
            assert!(inspect(&root).is_err(), "accepted corrupt {extension}");
            fs::remove_file(path).unwrap();
            assert_eq!(inspect(&root).unwrap().files, 2);
        }
        for extension in ["achievements.json", "gamemode"] {
            let path = player.with_extension(extension);
            fs::write(&path, b"corrupt").unwrap();
            assert!(
                inspect(&root).is_err(),
                "accepted corrupt player {extension}"
            );
            fs::remove_file(path).unwrap();
        }
        let bad_name = player.with_file_name("BAD-uuid.dat");
        PlayerPosition::default()
            .save_state(&bad_name, crate::vitals::Vitals::default())
            .unwrap();
        assert!(inspect(&root).is_err());
        fs::remove_file(bad_name).unwrap();
        assert_eq!(inspect(&root).unwrap().files, 2);
        assert!(!root.join("session.lock").exists());
        fs::remove_dir_all(root).unwrap();
    }

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
    #[test]
    fn inspects_verified_uuid_saves_and_sidecars_without_mutation() {
        let root = std::env::temp_dir().join(format!("f56-online-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let world = root.join("flat.irw");
        World::new().save_new(&world).unwrap();
        let uuid = "d3a0a8b1-4753-4a66-b9c8-f4ac1234c9ca";
        let player = PlayerPosition::authenticated_location(&world, uuid).unwrap();
        PlayerPosition::default()
            .save_state(&player, crate::vitals::Vitals::default())
            .unwrap();
        assert_eq!(inspect(&root).unwrap().files, 2);
        fs::write(player.with_extension("gamemode"), b"1").unwrap();
        assert_eq!(inspect(&root).unwrap().files, 3);
        fs::write(player.with_extension("gamemode"), b"9").unwrap();
        assert!(inspect(&root).is_err());
        fs::remove_file(player.with_extension("gamemode")).unwrap();
        fs::write(player.parent().unwrap().join("unknown.tmp"), b"partial").unwrap();
        assert!(inspect(&root).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_invalid_verified_player_names_and_nested_links() {
        let root = std::env::temp_dir().join(format!("f56-names-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("players/verified")).unwrap();
        World::new().save_new(&root.join("flat.irw")).unwrap();
        let bad = root.join("players/verified/not-a-uuid.dat");
        fs::write(&bad, b"bad").unwrap();
        assert!(inspect(&root).is_err());
        fs::remove_file(&bad).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/tmp", root.join("players/verified/link")).unwrap();
            assert!(inspect(&root).is_err());
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_orphaned_chests_furnaces_and_hoppers_without_mutating_saves() {
        let root = std::env::temp_dir().join(format!(
            "f56-container-anchors-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let save = root.join("flat.irw");
        let mut world = World::new();
        world.save_new(&save).unwrap();

        let items = serde_json::json!({
            "version": 1,
            "entries": {
                "1,65,1": [
                    {"id":4,"count":3,"damage":0},
                    null,null,null,null,null,null,null,null,
                    null,null,null,null,null,null,null,null,
                    null,null,null,null,null,null,null,null,
                    null,null
                ]
            }
        });
        let chests = Chests::location(&save);
        fs::write(&chests, serde_json::to_vec(&items).unwrap()).unwrap();
        let world_before = fs::read(&save).unwrap();
        let chest_before = fs::read(&chests).unwrap();
        assert!(inspect(&save).is_err(), "orphaned chest items accepted");
        assert_eq!(fs::read(&save).unwrap(), world_before);
        assert_eq!(fs::read(&chests).unwrap(), chest_before);
        assert!(!root.join("session.lock").exists());
        world.update_and_save(&save, 1, 65, 1, 54 << 4).unwrap();
        assert!(
            inspect(&save).is_ok(),
            "real chest should own its inventory"
        );

        fs::remove_file(&chests).unwrap();
        let furnace_file = Furnaces::location(&save);
        let furnace = serde_json::json!({
            "version": 1,
            "entries": {
                "2,65,1": {
                    "slots":[{"id":15,"count":1,"damage":0},null,null],
                    "burn_left":0,"burn_total":0,"cook":0
                }
            }
        });
        fs::write(&furnace_file, serde_json::to_vec(&furnace).unwrap()).unwrap();
        assert!(inspect(&save).is_err(), "orphaned furnace accepted");
        world.update_and_save(&save, 2, 65, 1, 61 << 4).unwrap();
        assert!(inspect(&save).is_ok(), "unlit furnace should be valid");
        world.update_and_save(&save, 2, 65, 1, 62 << 4).unwrap();
        assert!(inspect(&save).is_ok(), "lit furnace should be valid");
        fs::remove_file(&furnace_file).unwrap();

        let hoppers = Hoppers::location(&save);
        let hopper = serde_json::json!({
            "version": 1,
            "entries": {"3,65,1": [
                {"id":4,"count":2,"damage":0},null,null,null,null
            ]}
        });
        fs::write(&hoppers, serde_json::to_vec(&hopper).unwrap()).unwrap();
        assert!(inspect(&save).is_err(), "orphaned hopper accepted");
        world.update_and_save(&save, 3, 65, 1, 154 << 4).unwrap();
        assert!(inspect(&save).is_ok(), "hopper should own its items");
        assert!(!root.join("session.lock").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn world_sidecars_are_loaded_and_corruption_is_rejected() {
        let root = std::env::temp_dir().join(format!("f56-sidecars-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let file = root.join("flat.irw");
        World::new().save_new(&file).unwrap();
        let score = Scoreboard::location(&file);
        fs::write(&score, b"invalid scoreboard").unwrap();
        assert!(inspect(&root).is_err());
        fs::remove_file(&score).unwrap();
        let map = Maps::location(&file);
        fs::write(&map, b"invalid map").unwrap();
        assert!(inspect(&root).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
