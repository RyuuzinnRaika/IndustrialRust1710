//! Validate the actual CLI path without a Minecraft client or Java runtime.
//! Recovery must not listen for connections, overwrite files, or create a
//! session.lock when the level backup is corrupt or already restored.
use flate2::{write::GzEncoder, Compression};
use serde::Serialize;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Serialize)]
struct Root {
    #[serde(rename = "Data")]
    data: Data,
}
#[derive(Serialize)]
struct Data {
    version: i32,
    #[serde(rename = "LevelName")]
    name: String,
    #[serde(rename = "RandomSeed")]
    seed: i64,
    #[serde(rename = "SpawnX")]
    spawn_x: i32,
    #[serde(rename = "SpawnY")]
    spawn_y: i32,
    #[serde(rename = "SpawnZ")]
    spawn_z: i32,
    #[serde(rename = "Time")]
    time: i64,
}

fn directory(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let result = std::env::temp_dir().join(format!(
        "ir1710-fs05-{tag}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir(&result).unwrap();
    result
}

fn valid_level() -> Vec<u8> {
    let source = Root {
        data: Data {
            version: 19133,
            name: "Recovered 1.7.10 test".to_owned(),
            seed: -123456,
            spawn_x: 0,
            spawn_y: 64,
            spawn_z: 0,
            time: 24500,
        },
    };
    let nbt = fastnbt::to_bytes(&source).unwrap();
    let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
    gzip.write_all(&nbt).unwrap();
    gzip.finish().unwrap()
}

fn recover(dir: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
        .arg("--recover-level-dat")
        .arg(dir)
        .output()
        .unwrap()
}

#[test]
fn cli_recovers_exact_level_bytes_and_refuses_second_run() {
    let world = directory("roundtrip");
    let original = valid_level();
    fs::write(world.join("level.dat_old"), &original).unwrap();
    let result = recover(&world);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(fs::read(world.join("level.dat")).unwrap(), original);
    assert_eq!(fs::read(world.join("level.dat_old")).unwrap(), original);
    let lock_before = fs::read(world.join("session.lock")).unwrap();
    assert!(!recover(&world).status.success());
    assert_eq!(fs::read(world.join("session.lock")).unwrap(), lock_before);
    assert_eq!(fs::read(world.join("level.dat")).unwrap(), original);
    fs::remove_dir_all(world).unwrap();
}

#[test]
fn cli_corrupt_backup_fails_without_creating_session_or_level() {
    let world = directory("corrupt");
    fs::write(world.join("level.dat_old"), b"not level NBT").unwrap();
    assert!(!recover(&world).status.success());
    assert!(!world.join("level.dat").exists());
    assert!(!world.join("session.lock").exists());
    assert!(!world.join("level.dat.recovering").exists());
    fs::remove_dir_all(world).unwrap();
}
