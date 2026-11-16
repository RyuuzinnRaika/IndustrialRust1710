//! Offline server.properties preflight. Never opens a listener or writes to the world.
#[path = "../server_config.rs"]
mod server_config;

use std::env;
use std::io::{self, ErrorKind};
use std::path::Path;

/// Human-readable preflight for the configured listener, not a WAN reachability probe.
/// Binding to a non-loopback IP exposes the listener to reachable networks only.
fn safety_diagnostic_ja(bind: std::net::SocketAddr, online_mode: bool) -> &'static str {
    match (bind.ip().is_loopback(), online_mode) {
        (true, true) => "ループバック待受です。このPC以外からは直接接続できません。online-mode=trueで公式アカウント認証を要求します。",
        (true, false) => "ループバック待受です。このPC以外からは直接接続できません。ただしonline-mode=falseではユーザー名を認証しません。",
        (false, true) => "外部インターフェース待受です。到達可能なLANやインターネットから接続され得ます。online-mode=trueですが、ファイアウォールと公開ポートを確認してください。",
        (false, false) => "危険: 外部インターフェース待受かつonline-mode=falseです。到達可能な相手がユーザー名を偽装でき、ホワイトリストだけでは本人確認できません。公開を避け、待受先とファイアウォールを見直してください。",
    }
}

fn check(path: &Path) -> io::Result<()> {
    let config = server_config::ServerProperties::load_optional(path)?
        .ok_or_else(|| io::Error::new(ErrorKind::NotFound, "server.properties is missing"))?;
    server_config::verify_legacy_storage(&config.world_path)?;
    println!("Configuration is valid for supported options.");
    println!("Bind: {}", config.bind);
    println!("World: {}", config.world_path.display());
    println!(
        "Mode: {}",
        if config.survival {
            "survival"
        } else {
            "creative"
        }
    );
    println!("Difficulty: {}", config.difficulty);
    println!("Hardcore: {}", config.hardcore);
    println!("Online authentication: {}", config.online_mode);
    println!(
        "公開範囲・認証診断: {}",
        safety_diagnostic_ja(config.bind, config.online_mode)
    );
    println!("Level name: {}", config.level_name);
    let warning = server_config::offline_exposure_warning(config.bind, config.online_mode);
    if let Some(message) = warning {
        eprintln!("{message}");
    }
    if config.online_mode && !config.bind.ip().is_loopback() {
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
    fn japanese_diagnostic_covers_loopback_external_ipv4_ipv6_and_auth() {
        for address in ["127.0.0.1:25565", "[::1]:25565"] {
            let bind = address.parse().unwrap();
            for online in [true, false] {
                let message = safety_diagnostic_ja(bind, online);
                assert!(message.contains("ループバック"), "{address}");
                assert!(message.contains(if online {
                    "online-mode=true"
                } else {
                    "online-mode=false"
                }));
                assert!(!message.contains("危険:"));
            }
        }
        for address in ["0.0.0.0:25565", "192.168.11.8:25565", "[::]:25565"] {
            let bind = address.parse().unwrap();
            for online in [true, false] {
                let message = safety_diagnostic_ja(bind, online);
                assert!(message.contains("外部インターフェース"), "{address}");
                assert!(message.contains(if online {
                    "online-mode=true"
                } else {
                    "online-mode=false"
                }));
                assert_eq!(message.contains("危険:"), !online);
            }
        }
    }

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
