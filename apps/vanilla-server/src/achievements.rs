//! Persisted player achievements and 1.7.10 Protocol 5 statistic notifications.
use crate::multiplayer::Event;
use legacy_protocol::{write_string, write_varint};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, ErrorKind, Write};
use std::path::Path;

const MAX_BYTES: u64 = 8192;
// Supported vanilla 1.7.10 achievement identifiers and immediate prerequisites.
// Further gameplay triggers can be integrated without changing the save format.
const DEFINITIONS: &[(&str, Option<&str>)] = &[
    ("openInventory", None),
    ("mineWood", Some("openInventory")),
    ("buildWorkBench", Some("mineWood")),
    ("buildPickaxe", Some("buildWorkBench")),
    ("buildFurnace", Some("buildPickaxe")),
    ("acquireIron", Some("buildFurnace")),
    ("buildHoe", Some("buildWorkBench")),
    ("makeBread", Some("buildHoe")),
    ("bakeCake", Some("buildHoe")),
    ("buildSword", Some("buildWorkBench")),
    ("killEnemy", Some("buildSword")),
    ("killCow", Some("buildSword")),
    ("diamonds", Some("acquireIron")),
    ("enchantments", Some("diamonds")),
    ("breedCow", Some("killCow")),
    ("buildBetterPickaxe", Some("buildPickaxe")),
    ("cookFish", Some("buildFurnace")),
    ("onARail", Some("acquireIron")),
    ("flyPig", Some("killCow")),
    ("snipeSkeleton", Some("killEnemy")),
    ("diamondsToYou", Some("diamonds")),
    ("portal", Some("diamonds")),
    ("ghast", Some("portal")),
    ("blazeRod", Some("portal")),
    ("potion", Some("blazeRod")),
    ("theEnd", Some("blazeRod")),
    ("theEnd2", Some("theEnd")),
    ("overkill", Some("enchantments")),
    ("bookcase", Some("enchantments")),
    ("exploreAllBiomes", Some("theEnd")),
    ("spawnWither", Some("theEnd2")),
    ("killWither", Some("spawnWither")),
    ("fullBeacon", Some("killWither")),
    ("overpowered", Some("buildBetterPickaxe")),
];

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, reason)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Achievements {
    version: u8,
    unlocked: BTreeSet<String>,
}
impl Default for Achievements {
    fn default() -> Self {
        Self {
            version: 1,
            unlocked: BTreeSet::new(),
        }
    }
}
impl Achievements {
    /// Server-confirmed pickup triggers for vanilla 1.7.10 achievements.
    /// Never award on a client-supplied item snapshot or failed inventory transfer.
    pub fn collected_item_trigger(item_id: i16) -> Option<&'static str> {
        match item_id {
            17 | 162 => Some("mineWood"), // all six log variants
            265 => Some("acquireIron"),
            264 => Some("diamonds"),
            334 => Some("killCow"), // leather
            369 => Some("blazeRod"),
            _ => None,
        }
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        let md = match fs::symlink_metadata(path) {
            Ok(md) => md,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        };
        if !md.file_type().is_file() || md.len() > MAX_BYTES {
            return Err(invalid("invalid achievements save"));
        }
        let result: Self = serde_json::from_slice(&fs::read(path)?)
            .map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?;
        if result.version != 1
            || result.unlocked.len() > DEFINITIONS.len()
            || result
                .unlocked
                .iter()
                .any(|name| !DEFINITIONS.iter().any(|(k, _)| k == name))
        {
            return Err(invalid("invalid achievements list"));
        }
        // Never replay a corrupted save where a child is unlocked without its parent.
        for (name, parent) in DEFINITIONS {
            if result.unlocked.contains(*name)
                && parent.is_some_and(|p| !result.unlocked.contains(p))
            {
                return Err(invalid("achievement prerequisite missing"));
            }
        }
        Ok(result)
    }
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(self)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid("achievements too large"));
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("achievements.tmp");
        let mut file = File::create(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(tmp, path)?;
        #[cfg(unix)]
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    }
    fn packet(keys: impl IntoIterator<Item = String>) -> io::Result<Event> {
        let list: Vec<String> = keys.into_iter().collect();
        let mut body = Vec::new();
        write_varint(list.len() as u32, &mut body);
        for key in list {
            write_string(&format!("achievement.{key}"), &mut body)?;
            write_varint(1, &mut body);
        }
        Ok(Event { id: 0x37, body })
    }
    pub fn snapshot(&self) -> io::Result<Event> {
        Self::packet(self.unlocked.iter().cloned())
    }
    /// Returns None if unknown, already granted or its parent is not unlocked.
    /// Caller MUST persist the candidate before acknowledging the unlock.
    pub fn award(&mut self, key: &str) -> io::Result<Option<Event>> {
        let Some((_, parent)) = DEFINITIONS.iter().find(|(name, _)| *name == key) else {
            return Ok(None);
        };
        if self.unlocked.contains(key) || parent.is_some_and(|p| !self.unlocked.contains(p)) {
            return Ok(None);
        }
        let event = Self::packet([key.to_owned()])?;
        self.unlocked.insert(key.to_owned());
        Ok(Some(event))
    }
    #[cfg(test)]
    pub fn has(&self, key: &str) -> bool {
        self.unlocked.contains(key)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prerequisites_duplicate_unlock_and_stat_packet() {
        let mut stats = Achievements::default();
        assert!(stats.award("mineWood").unwrap().is_none());
        let packet = stats.award("openInventory").unwrap().unwrap();
        assert_eq!(packet.id, 0x37);
        assert!(packet
            .body
            .windows(25)
            .any(|data| data == b"achievement.openInventory"));
        assert!(stats.award("openInventory").unwrap().is_none());
        assert!(stats.award("mineWood").unwrap().is_some());
        assert!(stats.has("mineWood"));
    }
    #[test]
    fn achievements_persist_and_corrupt_data_rejected() {
        let path = std::env::temp_dir().join(format!(
            "ir1710-achievements-{}-{:?}.json",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_file(&path);
        let mut state = Achievements::load(&path).unwrap();
        state.award("openInventory").unwrap();
        state.save(&path).unwrap();
        assert!(Achievements::load(&path).unwrap().has("openInventory"));
        fs::write(&path, b"{broken").unwrap();
        assert!(Achievements::load(&path).is_err());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn vanilla_1710_catalogue_and_prerequisites() {
        // All 34 Java 1.7.10 names must be recognized even when their
        // gameplay triggers are implemented in a different module.
        assert_eq!(DEFINITIONS.len(), 34);
        let mut known = BTreeSet::new();
        for (name, parent) in DEFINITIONS {
            assert!(known.insert(*name), "duplicate achievement: {name}");
            if let Some(parent) = parent {
                assert!(DEFINITIONS.iter().any(|(key, _)| key == parent));
            }
        }
        let mut stats = Achievements::default();
        assert!(stats.award("fullBeacon").unwrap().is_none());
        assert!(stats.award("openInventory").unwrap().is_some());
        assert!(stats.award("mineWood").unwrap().is_some());
        assert!(stats.award("notAVanillaAchievement").unwrap().is_none());
        for (item, achievement) in [
            (17, "mineWood"),
            (162, "mineWood"),
            (265, "acquireIron"),
            (264, "diamonds"),
            (334, "killCow"),
            (369, "blazeRod"),
        ] {
            assert_eq!(Achievements::collected_item_trigger(item), Some(achievement));
        }
        assert_eq!(Achievements::collected_item_trigger(0), None);
    }

    #[test]
    fn corrupt_child_without_parent_is_rejected() {
        let path = std::env::temp_dir().join(format!(
            "ir1710-achievement-orphans-{}-{:?}.json",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_file(&path);
        fs::write(&path, br#"{"version":1,"unlocked":["fullBeacon"]}"#).unwrap();
        assert!(Achievements::load(&path).is_err());
        fs::remove_file(path).unwrap();
    }
}
