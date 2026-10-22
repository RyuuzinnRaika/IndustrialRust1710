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
}
