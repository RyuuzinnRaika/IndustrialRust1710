//! Safe, deliberately incomplete Minecraft Java 1.7.10 server.properties support.
//! This is NOT a vanilla Anvil world loader. An existing vanilla world is never
//! replaced or silently treated as an IRW1/IRW4 world.
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, ErrorKind};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Component, Path, PathBuf};

const MAX_PROPERTIES_BYTES: u64 = 64 * 1024;

/// Explain the actual authentication risk of the requested listener.
/// A name whitelist in offline-mode is NOT identity authentication.
pub fn offline_exposure_warning(bind: SocketAddr, online_mode: bool) -> Option<&'static str> {
    if online_mode {
        None
    } else if bind.ip().is_loopback() {
        Some("WARNING: offline-mode usernames are unauthenticated; use loopback for testing and never forward this port to the internet.")
    } else {
        Some("CRITICAL: offline-mode is listening on a non-loopback address. Anyone who can reach it may impersonate a whitelisted username. Restrict to a trusted LAN using firewall rules; never forward this port to the internet.")
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

/// Prevent a user-controlled path from escaping its selected directory
/// through an explicit '..' component before any lock or write is attempted.
/// Safe relative paths and intentionally provided absolute paths remain valid.
pub fn reject_parent_traversal(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(invalid("unsafe path: empty or parent-directory traversal"));
    }
    Ok(())
}

fn invalid_property(key: &str, expected: &str) -> io::Error {
    io::Error::new(
        ErrorKind::InvalidData,
        format!("server.properties {key}: expected {expected}"),
    )
}

#[derive(Debug)]
pub struct ServerProperties {
    pub bind: SocketAddr,
    pub world_path: PathBuf,
    pub survival: bool,
    pub hardcore: bool,
    pub online_mode: bool,
    pub difficulty: u8,
    pub level_name: String,
    /// Seed for the opt-in IRW3 biome generator, not FLAT terrain.
    // Also compiled by the standalone vanilla-check-config binary, which does not
    // currently consume the seed. The full server CLI uses this field.
    #[allow(dead_code)]
    pub level_seed: Option<u64>,
}

fn boolean(map: &BTreeMap<String, String>, key: &str, default: bool) -> io::Result<bool> {
    match map.get(key).map(String::as_str) {
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        None => Ok(default),
        _ => Err(invalid_property(key, "true or false")),
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
    reject_parent_traversal(world_file)?;
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
        // Explicit online-mode now performs the Protocol 5 RSA/AES session handshake.
        // Missing server.properties still preserves the historical offline CLI mode.
        // Do not silently switch a legacy deployment to a different auth mode.
        // Unlike vanilla's default, this prototype requires an explicit choice.
        if !values.contains_key("online-mode") {
            return Err(invalid("server.properties must explicitly set online-mode"));
        }
        let online_mode = boolean(&values, "online-mode", false)?;
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
            .map_err(|_| invalid_property("server-port", "an integer in 1..=65535"))?;
        if port == 0 {
            return Err(invalid_property("server-port", "an integer in 1..=65535"));
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
            .map_err(|_| invalid_property("difficulty", "an integer in 0..=3"))?;
        if difficulty > 3 {
            return Err(invalid_property("difficulty", "an integer in 0..=3"));
        }
        if hardcore && (!survival || difficulty != 3) {
            return Err(invalid(
                "hardcore requires gamemode=survival and difficulty=3",
            ));
        }
        // These settings remain fixed until their runtime subsystems can honour them.
        for (name, only) in [("view-distance", "2"), ("max-players", "8")] {
            if values.get(name).is_some_and(|value| value != only) {
                return Err(invalid_property(name, only));
            }
        }
        let level_seed = values
            .get("level-seed")
            .filter(|value| !value.is_empty())
            .map(|seed| {
                // Java Edition interprets numeric longs directly and strings via Java's UTF-16 hashCode.
                seed.parse::<i64>()
                    .map(|number| number as u64)
                    .unwrap_or_else(|_| {
                        let hash = seed.encode_utf16().fold(0i32, |hash, code_unit| {
                            hash.wrapping_mul(31).wrapping_add(i32::from(code_unit))
                        });
                        (i64::from(hash)) as u64
                    })
            });
        let level_type = values
            .get("level-type")
            .map(String::as_str)
            .unwrap_or("FLAT");
        match (level_type, level_seed) {
            ("FLAT", None) => {}
            ("DEFAULT", Some(_)) if survival => {}
            ("DEFAULT", Some(_)) => {
                return Err(invalid(
                    "DEFAULT seeded generation requires survival gamemode",
                ))
            }
            ("FLAT", Some(_)) => {
                return Err(invalid(
                    "level-seed is not applied to FLAT; choose level-type=DEFAULT",
                ))
            }
            _ => return Err(invalid("unsupported level-type/level-seed combination")),
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
        if let Some(key) = values
            .keys()
            .map(String::as_str)
            .find(|key| !accepted.contains(key))
        {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                format!(
                    "unsupported server.properties key '{key}'; remove it or correct the spelling"
                ),
            ));
        }
        Ok(Self {
            bind: SocketAddr::new(ip, port),
            world_path: PathBuf::from(level_name).join("flat.irw"),
            survival,
            hardcore,
            online_mode,
            difficulty,
            level_name: level_name.to_string(),
            level_seed,
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
    fn world_paths_allow_normal_relative_and_absolute_paths_but_reject_parent_traversal() {
        for safe in ["world/flat.irw", "./world/flat.irw", "world", "."] {
            assert!(reject_parent_traversal(Path::new(safe)).is_ok(), "{safe}");
        }
        let absolute = std::env::temp_dir().join("ir1710-path-test/flat.irw");
        assert!(reject_parent_traversal(&absolute).is_ok());
        for unsafe_path in [
            "",
            "../outside/flat.irw",
            "world/../outside.irw",
            "world/nested/../../outside.irw",
        ] {
            let error = reject_parent_traversal(Path::new(unsafe_path)).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::InvalidData, "{unsafe_path}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_parent_traversal_and_drive_prefixes_are_distinguished() {
        assert!(reject_parent_traversal(Path::new(r"C:\Worlds\flat.irw")).is_ok());
        assert!(reject_parent_traversal(Path::new(r"C:\Worlds\..\escape.irw")).is_err());
    }

    #[test]
    fn offline_warning_matches_authentication_and_ipv4_ipv6_exposure() {
        for local in ["127.0.0.1:25565", "[::1]:25565"] {
            let bind: SocketAddr = local.parse().unwrap();
            let warning = offline_exposure_warning(bind, false).unwrap();
            assert!(warning.starts_with("WARNING:"));
            assert!(!warning.contains("CRITICAL:"));
            assert_eq!(offline_exposure_warning(bind, true), None);
        }
        for public in ["0.0.0.0:25565", "192.168.0.5:25565", "[::]:25565"] {
            let bind: SocketAddr = public.parse().unwrap();
            let warning = offline_exposure_warning(bind, false).unwrap();
            assert!(warning.starts_with("CRITICAL:"));
            assert!(warning.contains("impersonate"));
            assert_eq!(offline_exposure_warning(bind, true), None);
        }
    }

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
        assert!(!p.online_mode);
        // An existing configuration must not silently change authentication.
        assert!(ServerProperties::parse(&SAFE.replace("online-mode=false\n", "")).is_err());
        assert!(
            ServerProperties::parse(&SAFE.replace("online-mode=false", "online-mode=true"))
                .unwrap()
                .online_mode
        );
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
    fn unknown_keys_fail_closed_and_identify_the_typo() {
        for key in ["white-lits", "server-prort", "motd"] {
            let input = format!("{SAFE}{key}=ignored\n");
            let error = ServerProperties::parse(&input).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::InvalidData);
            assert!(error.to_string().contains(key), "{error}");
            assert!(error.to_string().contains("remove"), "{error}");
        }
    }

    #[test]
    fn invalid_value_errors_identify_the_setting() {
        for (key, input) in [
            (
                "online-mode",
                SAFE.replace("online-mode=false", "online-mode=no"),
            ),
            (
                "white-list",
                SAFE.replace("white-list=true", "white-list=1"),
            ),
            ("hardcore", format!("{SAFE}hardcore=perhaps\n")),
            ("enable-rcon", format!("{SAFE}enable-rcon=1\n")),
            (
                "server-port",
                SAFE.replace("server-port=25580", "server-port=abc"),
            ),
            (
                "server-port",
                SAFE.replace("server-port=25580", "server-port=0"),
            ),
            (
                "difficulty",
                SAFE.replace("difficulty=1", "difficulty=easy"),
            ),
            ("difficulty", SAFE.replace("difficulty=1", "difficulty=4")),
            (
                "view-distance",
                SAFE.replace("view-distance=2", "view-distance=far"),
            ),
            (
                "max-players",
                SAFE.replace("max-players=8", "max-players=unlimited"),
            ),
        ] {
            let error = ServerProperties::parse(&input).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::InvalidData);
            assert!(error.to_string().contains(key), "{key}: {error}");
        }
    }

    #[test]
    fn seed_uses_java_long_and_string_hash_without_changing_flat_worlds() {
        let base = SAFE
            .replace("level-type=FLAT", "level-type=DEFAULT")
            .replace("gamemode=1", "gamemode=0");
        let signed = ServerProperties::parse(&format!("{base}level-seed=-123\n")).unwrap();
        assert_eq!(signed.level_seed, Some((-123i64) as u64));
        let hashed = ServerProperties::parse(&format!("{base}level-seed=Hello\n")).unwrap();
        assert_eq!(hashed.level_seed, Some(69609650));
        assert!(ServerProperties::parse(&format!("{SAFE}level-seed=123\n")).is_err());
        assert!(ServerProperties::parse(&format!("{base}level-seed=1\ngamemode=0\n")).is_err());
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
