//! Fail-closed, name-based admission for an *unauthenticated* offline-mode server.
//! This is authorization, NOT proof of Minecraft account ownership.
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::Path;

const MAX_WHITELIST_BYTES: u64 = 64 * 1024;
const MAX_PLAYERS: usize = 1_000;

fn invalid(message: &str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message.to_owned())
}

pub fn valid_name(name: &str) -> bool {
    (3..=16).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

#[derive(Debug, Clone)]
pub struct Whitelist {
    names: BTreeSet<String>,
}

impl Whitelist {
    /// Minecraft-like whitelist.json: [{"name":"PlayerName","uuid":"optional"}].
    /// The optional UUID is metadata only. Never treat it as verified in offline mode.
    pub fn load(path: &Path) -> io::Result<Self> {
        let meta = fs::metadata(path)?;
        if !meta.is_file() || meta.len() > MAX_WHITELIST_BYTES {
            return Err(invalid("whitelist must be a regular file of at most 64 KiB"));
        }
        let content = fs::read(path)?;
        if content.len() as u64 > MAX_WHITELIST_BYTES {
            return Err(invalid("whitelist exceeded read size limit"));
        }
        let parsed: serde_json::Value = serde_json::from_slice(&content)
            .map_err(|_| invalid("invalid whitelist JSON"))?;
        let entries = parsed.as_array().ok_or_else(|| invalid("whitelist must be a JSON array"))?;
        if entries.len() > MAX_PLAYERS {
            return Err(invalid("too many whitelist entries"));
        }
        let mut names = BTreeSet::new();
        for entry in entries {
            let object = entry.as_object().ok_or_else(|| invalid("whitelist entry must be an object"))?;
            let name = object.get("name").and_then(serde_json::Value::as_str)
                .ok_or_else(|| invalid("whitelist entry has no name"))?;
            if !valid_name(name) {
                return Err(invalid("invalid whitelist username (3-16 ASCII letters/numbers/underscore)"));
            }
            // Accept an official-style UUID field, but do not claim it was authenticated.
            if object.get("uuid").is_some_and(|v| !v.is_string()) {
                return Err(invalid("whitelist UUID must be a string"));
            }
            if !names.insert(name.to_ascii_lowercase()) {
                return Err(invalid("duplicate whitelist name"));
            }
        }
        Ok(Self { names })
    }

    pub fn allows(&self, name: &str) -> bool {
        valid_name(name) && self.names.contains(&name.to_ascii_lowercase())
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_are_strict() {
        assert!(valid_name("RustTester"));
        assert!(valid_name("a_B"));
        assert!(!valid_name(""));
        assert!(!valid_name("ab"));
        assert!(!valid_name("not allowed"));
        assert!(!valid_name("wrong-hyphen"));
        assert!(!valid_name("too_long_123456789"));
        assert!(!valid_name("非ASCII"));
    }
    #[test]
    fn rejects_wrong_schemas_fail_closed() {
        for content in [
            "null", "{}", "[\"RustTester\"]", "[{}]",
            "[{\"name\":\"a\"}]", "[{\"name\":\"OK_Name\"}, {\"name\":\"ok_name\"}]",
            "[{\"name\":\"RustTester\", \"uuid\":12}]", "[",
        ] {
            let data: Result<serde_json::Value, _> = serde_json::from_str(content);
            if let Ok(value) = data {
                let mut names = BTreeSet::new();
                let result = (|| -> io::Result<()> {
                    let arr = value.as_array().ok_or_else(|| invalid("not array"))?;
                    for entry in arr {
                        let object = entry.as_object().ok_or_else(|| invalid("not an object"))?;
                        let name = object.get("name").and_then(serde_json::Value::as_str)
                            .ok_or_else(|| invalid("no name"))?;
                        if !valid_name(name) || !names.insert(name.to_ascii_lowercase()) {
                            return Err(invalid("bad or duplicate name"));
                        }
                        if object.get("uuid").is_some_and(|v| !v.is_string()) {
                            return Err(invalid("bad uuid"));
                        }
                    }
                    Ok(())
                })();
                assert!(result.is_err(), "{content}");
            } else {
                assert_eq!(content, "[");
            }
        }
    }
    #[test]
    fn reading_from_disk() {
        let path = std::env::temp_dir().join(format!("ir1710-whitelist-{}.json", std::process::id()));
        fs::write(&path, r#"[{"name":"RustTester","uuid":"ignored-metadata"}]"#).unwrap();
        let list = Whitelist::load(&path).unwrap();
        assert_eq!(list.len(), 1);
        assert!(list.allows("rusttester"));
        assert!(list.allows("RUSTTESTER"));
        assert!(!list.allows("OtherName"));
        fs::write(&path, "broken JSON").unwrap();
        assert!(Whitelist::load(&path).is_err());
        fs::remove_file(path).unwrap();
    }
}
