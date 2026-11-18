use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const TEST_MARKER: &[u8] = b"IndustrialRust1710 verified offline backup v1\n";
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn real_directory(metadata: &fs::Metadata) -> bool {
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    true
}

struct Fixture {
    root: PathBuf,
    parent: PathBuf,
    backup: PathBuf,
    scratch: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        // Target/test binary location is operator-controlled; never use TEMP.
        let executable = std::env::current_exe().unwrap();
        let target = fs::canonicalize(executable.parent().unwrap()).unwrap();
        let parent = target.join("restore-drill-cli-test-fixtures");
        match fs::create_dir(&parent) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => panic!("cannot create test parent: {error}"),
        }
        let metadata = fs::symlink_metadata(&parent).unwrap();
        assert!(real_directory(&metadata));
        let parent = fs::canonicalize(parent).unwrap();
        assert_eq!(parent.parent(), Some(target.as_path()));
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for _ in 0..100 {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let root = parent.join(format!("cli-{}-{stamp}-{id}", std::process::id()));
            match fs::create_dir(&root) {
                Ok(()) => {
                    let backup = root.join("backup");
                    let scratch = root.join("scratch");
                    fs::create_dir(&backup).unwrap();
                    fs::create_dir(&scratch).unwrap();
                    fs::write(backup.join(".industrial1710-backup"), TEST_MARKER).unwrap();
                    fs::write(backup.join("flat.irw"), b"synthetic-world").unwrap();
                    fs::create_dir(backup.join("empty")).unwrap();
                    return Self {
                        root,
                        parent,
                        backup,
                        scratch,
                    };
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot create fixture: {error}"),
            }
        }
        panic!("unable to allocate fixture");
    }

    fn options(&self) -> Options {
        Options {
            server: std::env::current_exe().unwrap(),
            backup: self.backup.clone(),
            scratch_root: self.scratch.clone(),
            protected: vec![],
            timeout: Duration::from_secs(10),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let owned = fs::symlink_metadata(&self.root).is_ok_and(|metadata| {
            real_directory(&metadata)
                && self
                    .root
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("cli-"))
                && fs::canonicalize(&self.root)
                    .is_ok_and(|path| path.parent() == Some(self.parent.as_path()))
        });
        if !owned {
            eprintln!(
                "refusing to remove unverified test fixture {}",
                self.root.display()
            );
            return;
        }
        if let Err(error) = fs::remove_dir_all(&self.root) {
            if std::thread::panicking() {
                eprintln!("fixture cleanup failed: {error}");
            } else {
                panic!("fixture cleanup failed: {error}");
            }
        }
    }
}

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(|value| OsString::from(*value)).collect()
}

#[test]
fn cli_arguments_validate_required_values_duplicates_and_timeout() {
    let required = [
        "--server",
        "exe",
        "--backup",
        "backup",
        "--scratch-root",
        "scratch",
    ];
    let options = parse_args(args(&required)).unwrap();
    assert_eq!(options.timeout, Duration::from_secs(120));
    assert!(parse_args(args(&required[..4])).is_err());
    assert!(parse_args(args(&["--server"])).is_err());
    for extra in [
        vec!["--server", "other"],
        vec!["--backup", "other"],
        vec!["--scratch-root", "other"],
        vec!["--timeout-seconds", "0"],
        vec!["--timeout-seconds", "86401"],
        vec!["--timeout-seconds", "bad"],
        vec!["--unknown", "value"],
        vec!["--timeout-seconds", "1", "--timeout-seconds", "2"],
    ] {
        let mut values = args(&required);
        values.extend(args(&extra));
        assert!(
            parse_args(values).is_err(),
            "accepted invalid arguments: {extra:?}"
        );
    }
    let mut values = args(&required);
    values.extend(args(&[
        "--protect-world",
        "one",
        "--protect-world",
        "two",
        "--timeout-seconds",
        "1",
    ]));
    let options = parse_args(values).unwrap();
    assert_eq!(options.protected.len(), 2);
    assert_eq!(options.timeout, Duration::from_secs(1));
}

#[test]
fn overlapping_backup_scratch_and_protected_paths_are_rejected_before_writes() {
    let fixture = Fixture::new();
    let limits = Limits::default();
    let initial = snapshot::inspect_backup(&fixture.backup, &limits)
        .unwrap()
        .1;
    let nested = fixture.backup.join("nested");
    fs::create_dir(&nested).unwrap();
    for candidate in [
        fixture.backup.join("."),
        nested.clone(),
        fixture.root.clone(),
    ] {
        let mut options = fixture.options();
        options.scratch_root = candidate;
        assert!(run(options).is_err());
    }
    fs::remove_dir(&nested).unwrap();
    for protected in [
        fixture.backup.join("."),
        fixture.backup.join("flat.irw"),
        fixture.scratch.join("."),
    ] {
        let mut options = fixture.options();
        options.protected.push(protected);
        assert!(run(options).is_err());
    }
    assert_eq!(
        snapshot::inspect_backup(&fixture.backup, &limits)
            .unwrap()
            .1,
        initial
    );
    assert_eq!(fs::read_dir(&fixture.scratch).unwrap().count(), 0);
}

#[test]
fn copied_snapshot_matches_and_subsequent_mutation_is_detected() {
    let fixture = Fixture::new();
    let limits = Limits::default();
    let initial = snapshot::inspect_backup(&fixture.backup, &limits)
        .unwrap()
        .1;
    copy_snapshot(&fixture.backup, &fixture.scratch, &initial).unwrap();
    assert_snapshot(&fixture.scratch, &initial, &limits).unwrap();
    assert_snapshot(&fixture.backup, &initial, &limits).unwrap();
    fs::write(fixture.scratch.join("flat.irw"), b"changed-world!!").unwrap();
    assert!(assert_snapshot(&fixture.scratch, &initial, &limits).is_err());
    assert_snapshot(&fixture.backup, &initial, &limits).unwrap();
}

#[test]
fn failing_native_child_retains_only_synthetic_scratch_and_preserves_backup() {
    let fixture = Fixture::new();
    let limits = Limits::default();
    let initial = snapshot::inspect_backup(&fixture.backup, &limits)
        .unwrap()
        .1;
    // The Rust test harness rejects --verify-world; the real child fails safely.
    let result = run(fixture.options());
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("native verification failed"));
    assert_snapshot(&fixture.backup, &initial, &limits).unwrap();
    let retained: Vec<_> = fs::read_dir(&fixture.scratch).unwrap().collect();
    assert_eq!(retained.len(), 1);
    assert_snapshot(&retained[0].as_ref().unwrap().path(), &initial, &limits).unwrap();
}

#[test]
fn unreaped_child_error_preserves_scratch_and_backup() {
    let fixture = Fixture::new();
    let limits = Limits::default();
    let initial = snapshot::inspect_backup(&fixture.backup, &limits)
        .unwrap()
        .1;
    let mut observed_scratch = None;
    let result = run_with(fixture.options(), |server, target, scratch, timeout| {
        let expected_server = fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
        assert_eq!(server, expected_server.as_path());
        assert_eq!(timeout, Duration::from_secs(10));
        assert_eq!(target, scratch.join("flat.irw").as_path());
        assert_snapshot(scratch, &initial, &limits).unwrap();
        observed_scratch = Some(scratch.to_path_buf());
        Err(process::VerifyProcessError::unreaped_for_test())
    });
    let error = result.unwrap_err().to_string();
    assert!(error.contains("child reaped: false"), "{error}");
    let retained = observed_scratch.expect("verifier must be called");
    assert_eq!(
        retained.parent(),
        Some(fs::canonicalize(&fixture.scratch).unwrap().as_path())
    );
    assert_snapshot(&retained, &initial, &limits).unwrap();
    assert_snapshot(&fixture.backup, &initial, &limits).unwrap();
    assert_eq!(fs::read_dir(&fixture.scratch).unwrap().count(), 1);
    // No actual child exists in this injected-error case; fixture cleanup is safe.
}

fn check_protected_irw_parent(backup_in_live: bool) {
    let fixture = Fixture::new();
    let live = fixture.root.join("live-world");
    fs::create_dir(&live).unwrap();
    let world = live.join("flat.irw");
    fs::write(&world, b"live-world-sentinel").unwrap();
    let live_scratch = live.join("scratch");
    fs::create_dir(&live_scratch).unwrap();
    let live_backup = live.join("backup");
    fs::create_dir(&live_backup).unwrap();
    fs::write(live_backup.join(".industrial1710-backup"), TEST_MARKER).unwrap();
    fs::write(live_backup.join("saved.irw"), b"synthetic-backup").unwrap();
    let limits = Limits::default();
    let before = snapshot::snapshot_tree(&live, &limits).unwrap();
    let mut options = fixture.options();
    options.protected.push(world);
    if backup_in_live {
        options.backup = live_backup;
    } else {
        options.scratch_root = live_scratch;
    }
    let mut called = false;
    let error = run_with(options, |_, _, _, _| {
        called = true;
        Err(process::VerifyProcessError::unreaped_for_test())
    })
    .unwrap_err();
    assert!(
        error.to_string().contains("protected world"),
        "unexpected error: {error}"
    );
    assert!(!called, "native verifier must not be invoked");
    assert_eq!(snapshot::snapshot_tree(&live, &limits).unwrap(), before);
    assert_eq!(fs::read_dir(&fixture.scratch).unwrap().count(), 0);
}

#[test]
fn protected_irw_parent_refuses_scratch_before_writes() {
    check_protected_irw_parent(false);
}

#[test]
fn protected_irw_parent_refuses_backup_before_writes() {
    check_protected_irw_parent(true);
}

#[test]
fn successful_verifier_receives_isolated_copy_and_scratch_is_removed() {
    let fixture = Fixture::new();
    let limits = Limits::default();
    let initial = snapshot::inspect_backup(&fixture.backup, &limits)
        .unwrap()
        .1;
    let mut seen = None;
    run_with(fixture.options(), |server, world, scratch, timeout| {
        assert_eq!(
            server,
            fs::canonicalize(std::env::current_exe().unwrap()).unwrap()
        );
        assert_eq!(timeout, Duration::from_secs(10));
        assert_eq!(world, scratch.join("flat.irw"));
        assert_eq!(
            scratch.parent(),
            Some(fs::canonicalize(&fixture.scratch).unwrap().as_path())
        );
        assert_snapshot(scratch, &initial, &limits).unwrap();
        assert_snapshot(&fixture.backup, &initial, &limits).unwrap();
        seen = Some(scratch.to_path_buf());
        Ok(())
    })
    .unwrap();
    let scratch = seen.expect("verifier must be called");
    assert_eq!(
        fs::symlink_metadata(scratch).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(fs::read_dir(&fixture.scratch).unwrap().count(), 0);
    assert_snapshot(&fixture.backup, &initial, &limits).unwrap();
}

#[test]
fn scratch_diagnostic_keeps_path_when_existence_is_unknown() {
    let path = Path::new("synthetic-scratch-path");
    let message = scratch_diagnostic(
        path,
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "synthetic denial",
        )),
    );
    assert!(message.contains("scratch state unknown"), "{message}");
    assert!(message.contains("synthetic-scratch-path"), "{message}");
    let message = scratch_diagnostic(path, Err(io::ErrorKind::NotFound.into()));
    assert!(message.contains("no longer exists"), "{message}");
    assert!(message.contains("synthetic-scratch-path"), "{message}");
}
