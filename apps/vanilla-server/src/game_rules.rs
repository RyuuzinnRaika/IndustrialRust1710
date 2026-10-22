//! World-scoped, durable game rules for the IRW save format.
//! Only advertise rules that are wired to server-authoritative behavior.
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

const MAX_BYTES: u64 = 4096;
const VERSION: u8 = 1;

fn default_enabled() -> bool {
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameRules {
    version: u8,
    #[serde(rename = "doDaylightCycle")]
    daylight_cycle: bool,
    #[serde(rename = "keepInventory")]
    keep_inventory: bool,
    // Existing version-1 save files predate this field.
    #[serde(rename = "doMobSpawning", default = "default_enabled")]
    mob_spawning: bool,
}

impl Default for GameRules {
    fn default() -> Self {
        Self {
            version: VERSION,
            daylight_cycle: true,
            keep_inventory: false,
            mob_spawning: true,
        }
    }
}

impl GameRules {
    pub fn location(world_path: &Path) -> PathBuf {
        world_path.with_extension("gamerules.json")
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        };
        if !metadata.file_type().is_file() || metadata.len() > MAX_BYTES {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "invalid game rules file",
            ));
        }
        let rules: Self = serde_json::from_slice(&fs::read(path)?)
            .map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?;
        if rules.version != VERSION {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "unsupported game rules version",
            ));
        }
        Ok(rules)
    }

    pub fn get(self, key: &str) -> Option<bool> {
        match key {
            "doDaylightCycle" => Some(self.daylight_cycle),
            "keepInventory" => Some(self.keep_inventory),
            "doMobSpawning" => Some(self.mob_spawning),
            _ => None,
        }
    }

    pub fn daylight_cycle(self) -> bool {
        self.daylight_cycle
    }
    pub fn keep_inventory(self) -> bool {
        self.keep_inventory
    }
    pub fn mob_spawning(self) -> bool {
        self.mob_spawning
    }

    pub fn set(&mut self, key: &str, value: bool) -> io::Result<()> {
        match key {
            "doDaylightCycle" => self.daylight_cycle = value,
            "keepInventory" => self.keep_inventory = value,
            "doMobSpawning" => self.mob_spawning = value,
            _ => {
                return Err(io::Error::new(
                    ErrorKind::InvalidInput,
                    "unsupported game rule",
                ))
            }
        }
        Ok(())
    }

    pub fn save(self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent().filter(|v| !v.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension("gamerules.tmp");
        let bytes = serde_json::to_vec_pretty(&self)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "oversized game rules",
            ));
        }
        let mut file = File::create(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)?;
        #[cfg(unix)]
        if let Some(parent) = path.parent().filter(|v| !v.as_os_str().is_empty()) {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persists_world_rules_and_rejects_unknown_or_corrupt_values() {
        let path = std::env::temp_dir().join(format!(
            "ir1710-game-rules-{}-{:?}.json",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_file(&path);
        let mut rules = GameRules::load(&path).unwrap();
        assert_eq!(rules.get("keepInventory"), Some(false));
        assert_eq!(rules.get("doDaylightCycle"), Some(true));
        assert_eq!(rules.get("doMobSpawning"), Some(true));
        assert!(rules.set("missing", true).is_err());
        rules.set("keepInventory", true).unwrap();
        rules.set("doDaylightCycle", false).unwrap();
        rules.set("doMobSpawning", false).unwrap();
        rules.save(&path).unwrap();
        assert_eq!(GameRules::load(&path).unwrap(), rules);
        // Migration: old version-1 worlds must retain vanilla natural spawning.
        fs::write(
            &path,
            br#"{"version":1,"doDaylightCycle":true,"keepInventory":false}"#,
        )
        .unwrap();
        assert!(GameRules::load(&path).unwrap().mob_spawning());
        fs::write(
            &path,
            br#"{"version":1,"doDaylightCycle":"false","keepInventory":true}"#,
        )
        .unwrap();
        assert!(GameRules::load(&path).is_err());
        fs::remove_file(path).unwrap();
    }
}
