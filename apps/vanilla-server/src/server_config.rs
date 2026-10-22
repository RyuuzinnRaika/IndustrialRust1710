//! Safe, deliberately incomplete Minecraft Java 1.7.10 server.properties support.
//! This is NOT a vanilla Anvil world loader. An existing vanilla world is never
//! replaced or silently treated as an IRW1/IRW4 world.
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, ErrorKind};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

const MAX_PROPERTIES_BYTES: u64 = 64 * 1024;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

#[derive(Debug)]
pub struct ServerProperties {
    pub bind: SocketAddr,
    pub world_path: PathBuf,
    pub survival: bool,
    pub hardcore: bool,
    pub difficulty: u8,
    pub level_name: String,
}

fn boolean(map: &BTreeMap<String, String>, key: &str, default: bool) -> io::Result<bool> {
    match map.get(key).map(String::as_str) {
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        None => Ok(default),
        _ => Err(invalid("server.properties has invalid boolean")),
    }
}

fn legacy_world_name(name: &str) -> io::Result<()> {
    // A strict, cross-platform subset of 1.7.10 folder names. This prevents
    // traversal, drive prefixes, separators, and Windows reserved components.
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == b'_' || ch == b'-')
    {
        return Err(invalid("unsafe level-name in server.properties"));
    }
    let upper = name.to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "COM1" | "COM2" | "COM3" | "LPT1" | "LPT2" | "LPT3"
    ) {
        return Err(invalid("reserved level-name in server.properties"));
    }
    Ok(())
}

/// Never let an IRW prototype start writing alongside real vanilla level.dat.
pub fn verify_legacy_storage(world_file: &Path) -> io::Result<()> {
    let parent = world_file
        .parent()
        .ok_or_else(|| invalid("world path has no parent"))?;
    match fs::symlink_metadata(parent) {
        Ok(meta) if !meta.file_type().is_dir() => {
            return Err(invalid("world directory is not a real directory"));
        }
        Ok(_) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    }
    if parent.join("level.dat").exists() || parent.join("region").exists() {
        return Err(invalid(
            "vanilla Anvil world detected: read/write support is not implemented; refusing IRW fallback",
        ));
    }
    Ok(())
}

impl ServerProperties {
    pub fn load_optional(path: &Path) -> io::Result<Option<Self>> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        if !metadata.file_type().is_file() || metadata.len() > MAX_PROPERTIES_BYTES {
            return Err(invalid("server.properties is not a regular bounded file"));
        }
        let raw = fs::read_to_string(path)?;
        if raw.len() as u64 > MAX_PROPERTIES_BYTES {
            return Err(invalid("server.properties exceeds size limit"));
        }
        Self::parse(&raw).map(Some)
    }

    fn parse(raw: &str) -> io::Result<Self> {
        let mut values = BTreeMap::new();
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| invalid("invalid server.properties assignment"))?;
            let key = key.trim();
            if key.is_empty()
                || values
                    .insert(key.to_owned(), value.trim().to_owned())
                    .is_some()
            {
                return Err(invalid("empty or duplicate server.properties key"));
            }
        }
        // Vanilla defaults to online-mode=true. Never claim to authenticate a
        // user using only the current offline username whitelist.
        if boolean(&values, "online-mode", true)? {
            return Err(invalid(
                "online-mode=true needs Mojang session authentication (not implemented)",
            ));
        }
        if !boolean(&values, "white-list", false)? {
            return Err(invalid(
                "white-list=false is unsafe for the current offline-mode server",
            ));
        }
        if boolean(&values, "enable-rcon", false)? || boolean(&values, "enable-query", false)? {
            return Err(invalid("RCON/Query listeners are not implemented"));
        }
        let level_name = values
            .get("level-name")
            .map(String::as_str)
            .unwrap_or("world");
        legacy_world_name(level_name)?;
        let port = values
            .get("server-port")
            .map(String::as_str)
            .unwrap_or("25565")
            .parse::<u16>()
            .map_err(|_| invalid("invalid server-port"))?;
        if port == 0 {
            return Err(invalid("server-port cannot be zero"));
        }
        let host = values.get("server-ip").map(String::as_str).unwrap_or("");
        let ip = if host.is_empty() {
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        } else {
            host.parse::<IpAddr>()
                .map_err(|_| invalid("invalid server-ip"))?
        };
        let hardcore = boolean(&values, "hardcore", false)?;
        let survival = match values.get("gamemode").map(String::as_str).unwrap_or("0") {
            "0" | "survival" => true,
            "1" | "creative" => false,
            _ => return Err(invalid("unsupported gamemode in server.properties")),
        };
        let difficulty = values
            .get("difficulty")
            .map(String::as_str)
            .unwrap_or("1")
            .parse::<u8>()
            .map_err(|_| invalid("difficulty must be in 0..=3"))?;
        if difficulty > 3 {
            return Err(invalid("difficulty must be in 0..=3"));
        }
        if hardcore && (!survival || difficulty != 3) {
            return Err(invalid(
                "hardcore requires gamemode=survival and difficulty=3",
            ));
        }
        // These settings remain fixed until their runtime subsystems can honour them.
        for (name, only) in [("view-distance", "2"), ("max-players", "8")] {
            if values.get(name).is_some_and(|value| value != only) {
                return Err(invalid(
                    "server.properties requests an unsupported view-distance/difficulty/max-players",
                ));
            }
        }
        if values
            .get("level-seed")
            .is_some_and(|value| !value.is_empty())
        {
            return Err(invalid(
                "level-seed is not yet connected to world generation",
            ));
        }
        if values
            .get("level-type")
            .is_some_and(|value| value != "FLAT")
        {
            return Err(invalid("level-type must be FLAT with legacy IRW storage"));
        }
        let accepted = [
            "server-port",
            "server-ip",
            "level-name",
            "gamemode",
            "hardcore",
            "online-mode",
            "white-list",
            "enable-rcon",
            "enable-query",
            "view-distance",
            "difficulty",
            "max-players",
            "level-seed",
            "level-type",
        ];
        let ignored: Vec<&str> = values
            .keys()
            .map(String::as_str)
            .filter(|key| !accepted.contains(key))
            .collect();
        if !ignored.is_empty() {
            eprintln!(
                "server.properties: unsupported keys are NOT applied: {}",
                ignored.join(", ")
            );
        }
        Ok(Self {
            bind: SocketAddr::new(ip, port),
            world_path: PathBuf::from(level_name).join("flat.irw"),
            survival,
            hardcore,
            difficulty,
            level_name: level_name.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAFE: &str = "\
online-mode=false
white-list=true
server-ip=
server-port=25580
level-name=TestWorld
level-type=FLAT
view-distance=2
difficulty=1
max-players=8
gamemode=1
";

    #[test]
    fn accepts_safe_1710_config_and_maps_legacy_irw_path() {
        let p = ServerProperties::parse(SAFE).unwrap();
        assert_eq!(p.bind.to_string(), "127.0.0.1:25580");
        assert_eq!(p.world_path, PathBuf::from("TestWorld/flat.irw"));
        assert!(!p.survival);
        assert!(!p.hardcore);
        let hardcore = SAFE
            .replace("gamemode=1", "gamemode=0")
            .replace("difficulty=1", "difficulty=3");
        assert!(
            ServerProperties::parse(&(hardcore.clone() + "hardcore=true\n"))
                .unwrap()
                .hardcore
        );
        assert!(ServerProperties::parse(&(SAFE.to_owned() + "hardcore=true\n")).is_err());
        assert_eq!(p.difficulty, 1);
        for difficulty in 0..=3 {
            let text = SAFE.replace("difficulty=1", &format!("difficulty={difficulty}"));
            assert_eq!(
                ServerProperties::parse(&text).unwrap().difficulty,
                difficulty
            );
        }
    }

    #[test]
    fn refuses_auth_bypass_public_server_and_traversal() {
        for bad in [
            SAFE.replace("online-mode=false", "online-mode=true"),
            SAFE.replace("white-list=true", "white-list=false"),
            SAFE.replace("level-name=TestWorld", "level-name=../outside"),
            SAFE.replace("level-name=TestWorld", "level-name=CON"),
            SAFE.replace("gamemode=1", "gamemode=2"),
            SAFE.replace("view-distance=2", "view-distance=10"),
            SAFE.replace("difficulty=1", "difficulty=4"),
            SAFE.replace("server-port=25580", "server-port=0"),
            SAFE.replace("online-mode=false", "online-mode=false\nonline-mode=false"),
        ] {
            assert!(ServerProperties::parse(&bad).is_err(), "{bad}");
        }
        assert!(ServerProperties::parse("level-name=world").is_err());
    }

    #[test]
    fn detects_native_anvil_world_without_writing_irw() {
        let root = std::env::temp_dir().join(format!(
            "ir1710-props-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let world = root.join("world");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&world).unwrap();
        let world_file = world.join("flat.irw");
        assert!(verify_legacy_storage(&world_file).is_ok());
        fs::write(world.join("level.dat"), b"example").unwrap();
        assert!(verify_legacy_storage(&world_file).is_err());
        assert!(!world_file.exists());
        fs::remove_dir_all(root).unwrap();
    }
}
