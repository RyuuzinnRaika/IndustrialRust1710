//! Bounded persistent sign text for legacy (Protocol 5) worlds.
//! This module is storage/validation only; network packet dispatch is separate.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, ErrorKind, Write};
use std::path::Path;

const MAX_SIGNS: usize = 8192;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_LINE_CHARS: usize = 15;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sign {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub lines: [String; 4],
}

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signs {
    entries: BTreeMap<String, Sign>,
}

fn key(x: i32, y: i32, z: i32) -> String {
    format!("{x},{y},{z}")
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

fn valid_line(s: &str) -> bool {
    s.chars().count() <= MAX_LINE_CHARS
        && !s.chars().any(|c| c.is_control() || c == '\u{00a7}')
}

impl Signs {
    pub fn get(&self, x: i32, y: i32, z: i32) -> Option<&Sign> {
        self.entries.get(&key(x, y, z))
    }

    /// Call only after server-side block, chunk and edit-authorization checks.
    pub fn set(&mut self, sign: Sign) -> io::Result<()> {
        if !(0..=255).contains(&sign.y) {
            return Err(invalid("sign height out of bounds"));
        }
        if !sign.lines.iter().all(|line| valid_line(line)) {
            return Err(invalid("invalid sign text"));
        }
        let k = key(sign.x, sign.y, sign.z);
        if !self.entries.contains_key(&k) && self.entries.len() >= MAX_SIGNS {
            return Err(invalid("sign storage full"));
        }
        self.entries.insert(k, sign);
        Ok(())
    }

    pub fn remove(&mut self, x: i32, y: i32, z: i32) -> bool {
        self.entries.remove(&key(x, y, z)).is_some()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Sign> {
        self.entries.values()
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        };
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(invalid("sign file too large"));
        }
        let loaded: Self = serde_json::from_slice(&bytes)?;
        if loaded.entries.len() > MAX_SIGNS {
            return Err(invalid("too many signs"));
        }
        for (key_name, sign) in &loaded.entries {
            if *key_name != key(sign.x, sign.y, sign.z)
                || !(0..=255).contains(&sign.y)
                || !sign.lines.iter().all(|line| valid_line(line))
            {
                return Err(invalid("invalid persisted sign"));
            }
        }
        Ok(loaded)
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(invalid("sign save too large"));
        }
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let filename = path.file_name().ok_or_else(|| invalid("sign path has no filename"))?;
        let tmp = parent.join(format!(".{}.tmp", filename.to_string_lossy()));
        let write_result = (|| {
            let mut out = File::create(&tmp)?;
            out.write_all(&bytes)?;
            out.sync_all()?;
            fs::rename(&tmp, path)?;
            Ok(())
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        write_result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_overlong_and_control_text() {
        let mut signs = Signs::default();
        let sign = |text: &str| Sign {
            x: 0, y: 64, z: 0,
            lines: [text.to_owned(), String::new(), String::new(), String::new()],
        };
        assert!(signs.set(sign("hello")).is_ok());
        assert!(signs.set(sign("abcdefghijklmnop")).is_err());
        assert!(signs.set(sign("hi\nthere")).is_err());
    }
    #[test]
    fn overwrites_without_duplicate() {
        let mut signs = Signs::default();
        for value in ["first", "second"] {
            signs.set(Sign { x: 5, y: 64, z: -2, lines: [value.into(), "".into(), "".into(), "".into()] }).unwrap();
        }
        assert_eq!(signs.iter().count(), 1);
        assert_eq!(signs.get(5, 64, -2).unwrap().lines[0], "second");
        assert!(signs.remove(5, 64, -2));
        assert!(signs.get(5, 64, -2).is_none());
    }
}
