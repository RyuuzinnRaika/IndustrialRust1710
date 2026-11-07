//! ADD-080 / #505: public binding must never be an accidental offline
//! default. This test invokes only read-only configuration and rejected
//! startup paths; it must never create or mutate a real user world.
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "industrialrust-add080-{}-{now}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("unique scratch root");
        Self(root)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("ADD-080 scratch retained: {}", self.0.display());
        } else {
            fs::remove_dir_all(&self.0).expect("remove only the isolated test directory");
        }
    }
}

#[test]
fn add080_default_config_is_loopback_and_offline_public_bind_fails_closed() {
    let root = Scratch::new();
    let config = root.0.join("server.properties");
    fs::write(
        &config,
        "online-mode=false\nwhite-list=true\nserver-ip=\nserver-port=25565\nlevel-name=TestWorld\nlevel-type=FLAT\nview-distance=2\nmax-players=8\ngamemode=0\n",
    )
    .unwrap();

    // --check-config is read-only: an empty server-ip means loopback, never
    // the wildcard address that the original Java server may use.
    let checked = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
        .arg("--check-config")
        .arg(&config)
        .current_dir(&root.0)
        .output()
        .expect("run config verifier");
    assert!(
        checked.status.success(),
        "check-config rejected safe loopback properties: {}",
        String::from_utf8_lossy(&checked.stderr)
    );
    assert!(
        String::from_utf8_lossy(&checked.stdout).contains("bind=127.0.0.1:25565"),
        "empty server-ip must resolve to loopback"
    );
    assert!(!root.0.join("TestWorld").exists());

    // An operator may request a public listener only through a deliberate
    // security override or real online auth. Without either, fail before
    // opening a socket or touching a world, for both IPv4 and IPv6.
    for (index, address) in ["0.0.0.0:25565", "[::]:25565", "192.0.2.5:25565"]
        .iter()
        .enumerate()
    {
        let world = root.0.join(format!("denied-{index}.irw"));
        let result = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
            .args(["--bind", address, "--world"])
            .arg(&world)
            .arg("--whitelist")
            .arg(root.0.join("missing-whitelist.json"))
            .current_dir(&root.0)
            .output()
            .expect("launch rejected offline bind");
        assert!(!result.status.success(), "{address} must not start in offline mode");
        assert!(
            String::from_utf8_lossy(&result.stderr)
                .contains("non-loopback bind requires explicit --insecure-offline-lan"),
            "unexpected rejection cause for {address}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!world.exists(), "rejected bind must not create a world");
    }

    // Direct public server.properties must also fail read-only validation
    // in offline mode, while retaining the original config bytes.
    let public = fs::read_to_string(&config)
        .unwrap()
        .replace("server-ip=\n", "server-ip=0.0.0.0\n");
    fs::write(&config, public.as_bytes()).unwrap();
    let check_public = Command::new(env!("CARGO_BIN_EXE_vanilla-server"))
        .arg("--check-config")
        .arg(&config)
        .current_dir(&root.0)
        .output()
        .expect("check explicitly public properties");
    assert!(!check_public.status.success());
    assert_eq!(fs::read(&config).unwrap(), public.as_bytes());
    assert!(!root.0.join("TestWorld").exists());
}
