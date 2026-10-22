//! Restricted and crash-aware tiny administrative overrides.
//! Never trust an offline player's chat message with these settings.
use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::Path;

fn invalid() -> io::Error {
    io::Error::new(ErrorKind::InvalidData, "invalid administrative override")
}

pub fn read(path: &Path, max: u8) -> io::Result<Option<u8>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !meta.file_type().is_file() || meta.len() != 1 {
        return Err(invalid());
    }
    let bytes = fs::read(path)?;
    if bytes.len() != 1 || bytes[0] < b'0' || bytes[0] > b'0' + max {
        return Err(invalid());
    }
    Ok(Some(bytes[0] - b'0'))
}

pub fn write(path: &Path, value: u8, max: u8) -> io::Result<()> {
    if value > max {
        return Err(invalid());
    }
    // Do not replace symlinks or unrecognized legacy data.
    read(path, max)?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("admin.tmp");
    let mut file = OpenOptions::new().write(true).create_new(true).open(&temp)?;
    if let Err(error) = file.write_all(&[b'0' + value]).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(temp);
        return Err(error);
    }
    drop(file);
    if let Err(error) = fs::rename(&temp, path) {
        let _ = fs::remove_file(temp);
        return Err(error);
    }
    #[cfg(unix)]
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

pub fn parse_gamemode(value: &str) -> io::Result<u8> {
    match value {
        "0" | "survival" => Ok(0),
        "1" | "creative" => Ok(1),
        _ => Err(io::Error::new(ErrorKind::InvalidInput, "gamemode must be survival/creative (0/1)")),
    }
}

pub fn parse_difficulty(value: &str) -> io::Result<u8> {
    match value {
        "0" | "peaceful" => Ok(0),
        "1" | "easy" => Ok(1),
        "2" | "normal" => Ok(2),
        "3" | "hard" => Ok(3),
        _ => Err(io::Error::new(ErrorKind::InvalidInput, "difficulty must be peaceful/easy/normal/hard (0..3)")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_strict_console_values_without_unsafe_fallbacks() {
        for (text, number) in [("survival", 0), ("creative", 1), ("0", 0), ("1", 1)] {
            assert_eq!(parse_gamemode(text).unwrap(), number);
        }
        for value in ["spectator", "2", "-1", "CREATIVE", ""] {
            assert!(parse_gamemode(value).is_err());
        }
        for (text, number) in [("peaceful", 0), ("easy", 1), ("normal", 2), ("hard", 3)] {
            assert_eq!(parse_difficulty(text).unwrap(), number);
        }
        for value in ["4", "-1", "HARD", ""] {
            assert!(parse_difficulty(value).is_err());
        }
    }

    #[test]
    fn persists_bounded_overrides_and_rejects_corruption() {
        let path = std::env::temp_dir().join(format!("e49-{}-{:?}.gamemode", std::process::id(), std::thread::current().id()));
        let _ = fs::remove_file(&path);
        assert_eq!(read(&path, 1).unwrap(), None);
        write(&path, 1, 1).unwrap();
        assert_eq!(read(&path, 1).unwrap(), Some(1));
        assert!(write(&path, 2, 1).is_err());
        assert_eq!(read(&path, 1).unwrap(), Some(1));
        fs::write(&path, b"bad-data").unwrap();
        assert_eq!(read(&path, 1).unwrap_err().kind(), ErrorKind::InvalidData);
        assert!(write(&path, 0, 1).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"bad-data");
        fs::remove_file(path).unwrap();
    }
}
