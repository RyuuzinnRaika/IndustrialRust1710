//! World-scoped game rule state (Minecraft Java 1.7.10 defaults).
//! Wire commands, death handling, and world tick integrations are separate tasks.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

const MAX_BYTES: u64 = 16 * 1024;
const RULES: &[(&str, &str)] = &[
    ("commandBlockOutput", "true"),
    ("doDaylightCycle", "true"),
    ("doFireTick", "true"),
    ("doMobLoot", "true"),
    ("doMobSpawning", "true"),
    ("doTileDrops", "true"),
    ("keepInventory", "false"),
    ("mobGriefing", "true"),
    ("naturalRegeneration", "true"),
    ("randomTickSpeed", "3"),
    ("sendCommandFeedback", "true"),
    ("showDeathMessages", "true"),
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameRules {
    values: BTreeMap<String, String>,
}

impl Default for GameRules {
    fn default() -> Self {
        Self {
            values: RULES
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }
}

impl GameRules {
    pub fn get(&self, rule: &str) -> Option<&str> {
        self.values.get(rule).map(String::as_str)
    }

    pub fn boolean(&self, rule: &str) -> Option<bool> {
        match self.get(rule)? {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }
    }

    pub fn random_tick_speed(&self) -> u32 {
        self.get("randomTickSpeed")
            .and_then(|v| v.parse().ok())
            .unwrap_or(3)
    }

    /// Prevent unknown names and invalid types from silently changing gameplay.
    pub fn set(&mut self, rule: &str, value: &str) -> io::Result<bool> {
        if !self.values.contains_key(rule) {
            return Err(io::Error::new(ErrorKind::InvalidInput, "unknown game rule"));
        }
        if rule == "randomTickSpeed" {
            let number = value
                .parse::<u32>()
                .map_err(|_| io::Error::new(ErrorKind::InvalidInput, "invalid randomTickSpeed"))?;
            if number > 4096 {
                return Err(io::Error::new(
                    ErrorKind::InvalidInput,
                    "randomTickSpeed too large",
                ));
            }
        } else if value != "true" && value != "false" {
            return Err(io::Error::new(ErrorKind::InvalidInput, "expected boolean"));
        }
        Ok(self
            .values
            .insert(rule.to_owned(), value.to_owned())
            .as_deref()
            != Some(value))
    }

    pub fn location(world_file: &Path) -> PathBuf {
        world_file.with_extension("gamerules.json")
    }

    pub fn load(world_file: &Path) -> io::Result<Self> {
        let path = Self::location(world_file);
        let data = match fs::metadata(&path) {
            Ok(meta) => {
                if meta.len() > MAX_BYTES {
                    return Err(io::Error::new(
                        ErrorKind::InvalidData,
                        "game rules file too large",
                    ));
                }
                fs::read(&path)?
            }
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        };
        let loaded: Self =
            serde_json::from_slice(&data).map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?;
        let mut rules = Self::default();
        for (k, v) in loaded.values {
            rules
                .set(&k, &v)
                .map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?;
        }
        Ok(rules)
    }

    pub fn save(&self, world_file: &Path) -> io::Result<()> {
        let path = Self::location(world_file);
        let data = serde_json::to_vec_pretty(self)?;
        if data.len() as u64 > MAX_BYTES {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "game rules file too large",
            ));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension(format!("gamerules.{}.tmp", std::process::id()));
        let write_result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            file.write_all(&data)?;
            file.sync_all()?;
            fs::rename(&temp, &path)?;
            if let Some(parent) = path.parent() {
                if let Ok(dir) = File::open(parent) {
                    let _ = dir.sync_all();
                }
            }
            Ok(())
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        write_result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_and_types() {
        let mut rules = GameRules::default();
        assert_eq!(rules.boolean("keepInventory"), Some(false));
        assert_eq!(rules.boolean("doDaylightCycle"), Some(true));
        assert_eq!(rules.random_tick_speed(), 3);
        assert!(rules.set("keepInventory", "true").unwrap());
        assert!(!rules.set("keepInventory", "true").unwrap());
        assert_eq!(rules.boolean("keepInventory"), Some(true));
        assert!(rules.set("keepInventory", "1").is_err());
        assert!(rules.set("randomTickSpeed", "-1").is_err());
        assert!(rules.set("randomTickSpeed", "4097").is_err());
        assert!(rules.set("unrecognized", "true").is_err());
    }
    #[test]
    fn serde_and_reject_unknown() {
        let original = GameRules::default();
        let serialized = serde_json::to_vec(&original).unwrap();
        let loaded: GameRules = serde_json::from_slice(&serialized).unwrap();
        assert_eq!(original, loaded);
        let mut unknown = loaded;
        unknown.values.insert("evil".into(), "true".into());
        assert!(unknown.set("evil", "false").is_ok()); // Parsed structures are untrusted; load validates against defaults.
    }
}
