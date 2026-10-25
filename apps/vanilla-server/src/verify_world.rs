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
/// Online-mode players live under players/verified/<uuid>.dat, whereas
/// offline players use players/<name>.dat. Both must pass identical sidecar
/// integrity checks; neither folder may contain nested symlinks.
fn verified_uuid(stem: &str) -> bool {
    stem.len() == 36
        && stem.bytes().enumerate().all(|(i, byte)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
}

fn inspect_players(
    directory: &Path,
    verified: bool,
    entry_count: &mut usize,
    file_count: &mut usize,
) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| invalid("non-UTF8 player filename"))?;
        if !verified && name == "verified" {
            if !checked_dir(&path)? {
                return Err(invalid("verified player directory missing"));
            }
            inspect_players(&path, true, entry_count, file_count)?;
            continue;
        }
        if !checked_file(&path)? {
            return Err(invalid("players folder contains unsupported entry"));
        }
        *entry_count = entry_count
            .checked_add(1)
            .ok_or_else(|| invalid("too many player files"))?;
        if *entry_count > 10_000 {
            return Err(invalid("too many player files"));
        }
        if !name.ends_with(".dat") {
            continue;
        }
        let stem = name.trim_end_matches(".dat");
        if (verified && !verified_uuid(stem))
            || (!verified
                && (!crate::whitelist::valid_name(stem) || stem != stem.to_ascii_lowercase()))
        {
            return Err(invalid("invalid player filename"));
        }
        PlayerPosition::load_state(&path)?;
        *file_count += 1;
        macro_rules! check_player {
            ($path:expr, $loader:expr) => {{
                let sidecar: PathBuf = $path;
                if checked_file(&sidecar)? {
                    $loader?;
                    *file_count += 1;
                }
            }};
        }
        let inventory = Inventory::location(&path);
        check_player!(inventory.clone(), Inventory::load(&inventory));
        let survival = path.with_extension("survival.inventory.json");
        check_player!(survival.clone(), Inventory::load(&survival));
        let xp = path.with_extension("experience.json");
        check_player!(xp.clone(), Experience::load(&xp));
        let achievements = path.with_extension("achievements.json");
        check_player!(achievements.clone(), Achievements::load(&achievements));
        let gamemode = path.with_extension("gamemode");
        check_player!(gamemode.clone(), admin_state::read(&gamemode, 1));
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
    check!(chests.clone(), Chests::load(&chests));
    let furnaces = Furnaces::location(world_file);
    check!(furnaces.clone(), Furnaces::load(&furnaces));
    let hoppers = Hoppers::location(world_file);
    check!(hoppers.clone(), Hoppers::load(&hoppers));
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
        let mut player_entries = 0usize;
        inspect_players(&player_dir, false, &mut player_entries, &mut count)?;
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
}
