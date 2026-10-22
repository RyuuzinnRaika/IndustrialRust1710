//! Offline server.properties preflight. Never opens a listener or writes to the world.
#[path = "../server_config.rs"]
mod server_config;

use std::env;
use std::io::{self, ErrorKind};
use std::path::Path;

fn check(path: &Path) -> io::Result<()> {
    let config = server_config::ServerProperties::load_optional(path)?
        .ok_or_else(|| io::Error::new(ErrorKind::NotFound, "server.properties is missing"))?;
    server_config::verify_legacy_storage(&config.world_path)?;
    println!("Configuration is valid for supported options.");
    println!("Bind: {}", config.bind);
    println!("World: {}", config.world_path.display());
    println!(
        "Mode: {}",
        if config.survival { "survival" } else { "creative" }
    );
    println!("Difficulty: {}", config.difficulty);
    println!("Hardcore: {}", config.hardcore);
    println!("Online authentication: {}", config.online_mode);
    println!("Level name: {}", config.level_name);
    if !config.online_mode {
        eprintln!(
            "WARNING: offline usernames are unauthenticated; do not expose this server publicly."
        );
    }
    if !config.bind.ip().is_loopback() {
        eprintln!(
            "WARNING: binding to a non-loopback address; verify firewall and account authentication."
        );
    }
    Ok(())
}

fn main() -> io::Result<()> {
    let mut args = env::args().skip(1);
    if args.next().as_deref() != Some("--check-config") {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "usage: vanilla-check-config --check-config [PATH_TO_SERVER.PROPERTIES]",
        ));
    }
    let path = args.next().unwrap_or_else(|| "server.properties".into());
    if args.next().is_some() {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "too many arguments",
        ));
    }
    check(Path::new(&path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn missing_config_does_not_create_files() {
        let base = env::temp_dir().join(format!("ir1710-check-missing-{}", std::process::id()));
        let _ = fs::remove_file(&base);
        let failure = check(&base).unwrap_err();
        assert_eq!(failure.kind(), ErrorKind::NotFound);
        assert!(!base.exists());
    }

    #[test]
    fn malformed_config_fails_closed() {
        let base = env::temp_dir().join(format!("ir1710-check-invalid-{}", std::process::id()));
        fs::write(&base, b"online-mode=false\nwhite-list=false\n").unwrap();
        assert!(check(&base).is_err());
        fs::remove_file(base).unwrap();
    }
}
