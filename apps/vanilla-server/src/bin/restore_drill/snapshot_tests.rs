use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    parent: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        // Use the Cargo-built test binary's directory in the operator-chosen
        // target tree; a user-supplied TEMP might be inside an actual backup.
        // The target tree is assumed private and not concurrently replaced.
        let binary = std::env::current_exe().unwrap();
        let target = fs::canonicalize(binary.parent().unwrap()).unwrap();
        let parent = target.join("restore-drill-test-fixtures");
        ensure_real_dir(&parent);
        let parent = fs::canonicalize(parent).unwrap();
        assert_eq!(parent.parent(), Some(target.as_path()));
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for _ in 0..100 {
            let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let root = parent.join(format!("snapshot-{}-{stamp}-{id}", std::process::id()));
            match fs::create_dir(&root) {
                Ok(()) => return Self { root, parent },
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot create test fixture: {error}"),
            }
        }
        panic!("could not create a unique fixture");
    }

    fn write(&self, name: &str, data: &[u8]) {
        fs::write(self.root.join(name), data).unwrap();
    }

    fn with_marker_and_irw() -> Self {
        let fixture = Self::new();
        fixture.write(MARKER_NAME, MARKER);
        fixture.write("flat.irw", b"abc");
        fixture
    }
}

fn ensure_real_dir(path: &Path) {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
        Err(error) => panic!("cannot create fixture directory: {error}"),
    }
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(no_reparse(&metadata) && metadata.file_type().is_dir());
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Only remove the direct child created by this fixture after verifying
        // its resolved parent and type; leave anything unexpected untouched.
        let is_owned = fs::symlink_metadata(&self.root).is_ok_and(|meta| {
            no_reparse(&meta)
                && meta.file_type().is_dir()
                && self
                    .root
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("snapshot-"))
                && fs::canonicalize(&self.root)
                    .is_ok_and(|resolved| resolved.parent() == Some(self.parent.as_path()))
        });
        let result = if is_owned {
            fs::remove_dir_all(&self.root).and_then(|()| match fs::symlink_metadata(&self.root) {
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
                Ok(_) => Err(invalid("fixture remains after cleanup")),
                Err(error) => Err(error),
            })
        } else {
            Err(invalid("fixture ownership could not be confirmed"))
        };
        if let Err(error) = result {
            if std::thread::panicking() {
                eprintln!("fixture cleanup failed: {error}");
            } else {
                panic!("fixture cleanup failed: {error}");
            }
        }
    }
}

#[test]
fn snapshot_records_sha256_file_names_and_empty_directories_without_writes() {
    let fixture = Fixture::with_marker_and_irw();
    fs::create_dir(fixture.root.join("empty")).unwrap();
    let before = fs::metadata(fixture.root.join("flat.irw"))
        .unwrap()
        .modified()
        .unwrap();
    let (layout, snapshot) = inspect_backup(&fixture.root, &Limits::default()).unwrap();
    assert_eq!(layout, WorldLayout::Irw(OsString::from("flat.irw")));
    assert_eq!(snapshot.files, 2);
    assert_eq!(snapshot.bytes, MARKER.len() as u64 + 3);
    assert_eq!(
        snapshot.entries.get(Path::new("empty")),
        Some(&Entry::Directory)
    );
    assert_eq!(
        snapshot.entries.get(Path::new("flat.irw")),
        Some(&Entry::File {
            bytes: 3,
            sha256: [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad,
            ],
        })
    );
    assert_eq!(fs::read(fixture.root.join("flat.irw")).unwrap(), b"abc");
    assert_eq!(
        fs::metadata(fixture.root.join("flat.irw"))
            .unwrap()
            .modified()
            .unwrap(),
        before
    );
}

#[test]
fn streaming_hash_handles_short_reads_and_propagates_read_errors() {
    struct OneByteAtATime(std::io::Cursor<Vec<u8>>);
    impl Read for OneByteAtATime {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let count = buffer.len().min(1);
            self.0.read(&mut buffer[..count])
        }
    }
    let mut short = OneByteAtATime(std::io::Cursor::new(b"abc".to_vec()));
    let (bytes, hash) = hash_reader(&mut short, 3, 3).unwrap();
    assert_eq!(bytes, 3);
    assert_eq!(
        hash,
        [
            0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
            0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
            0xf2, 0x00, 0x15, 0xad,
        ]
    );

    struct FailedRead;
    impl Read for FailedRead {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::from(ErrorKind::Interrupted))
        }
    }
    assert_eq!(
        hash_reader(&mut FailedRead, 1, 1).unwrap_err().kind(),
        ErrorKind::Interrupted
    );
    assert!(hash_reader(&mut std::io::Cursor::new(b"abc"), 2, 3).is_err());
    assert!(hash_reader(&mut std::io::Cursor::new(b"abc"), 4, 4).is_err());
    struct MustNotRead;
    impl Read for MustNotRead {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            panic!("byte limit must be enforced before reading");
        }
    }
    assert!(hash_reader(&mut MustNotRead, 3, 2).is_err());
}

#[test]
fn backup_path_with_parent_traversal_is_rejected() {
    let fixture = Fixture::with_marker_and_irw();
    let name = fixture.root.file_name().unwrap();
    // PathBuf::push may normalize traversal when the root is verbatim on
    // Windows, so construct the input without push/join.
    let mut traversal = fixture.root.as_os_str().to_os_string();
    traversal.push(std::path::MAIN_SEPARATOR.to_string());
    traversal.push("..");
    traversal.push(std::path::MAIN_SEPARATOR.to_string());
    traversal.push(name);
    let traversal = PathBuf::from(traversal);
    assert!(inspect_backup(&traversal, &Limits::default()).is_err());
    assert_eq!(fs::read(fixture.root.join("flat.irw")).unwrap(), b"abc");
}

#[cfg(windows)]
#[test]
fn drive_relative_and_root_relative_directory_paths_are_rejected() {
    assert!(ensure_no_reparse_ancestors(Path::new(r"C:relative")).is_err());
    assert!(ensure_no_reparse_ancestors(Path::new(r"\root-relative")).is_err());
}

#[test]
fn world_layout_accepts_nonflat_irw_or_anvil_and_rejects_ambiguity() {
    let fixture = Fixture::new();
    fixture.write(MARKER_NAME, MARKER);
    assert!(inspect_backup(&fixture.root, &Limits::default()).is_err());
    fixture.write("other.irw", b"irw");
    assert_eq!(
        inspect_backup(&fixture.root, &Limits::default()).unwrap().0,
        WorldLayout::Irw(OsString::from("other.irw"))
    );
    fixture.write("second.irw", b"irw");
    assert!(inspect_backup(&fixture.root, &Limits::default()).is_err());
    fs::remove_file(fixture.root.join("second.irw")).unwrap();
    fixture.write("level.dat", b"anvil");
    assert!(inspect_backup(&fixture.root, &Limits::default()).is_err());
    fs::remove_file(fixture.root.join("other.irw")).unwrap();
    assert_eq!(
        inspect_backup(&fixture.root, &Limits::default()).unwrap().0,
        WorldLayout::Anvil
    );
}

#[test]
fn marker_must_be_exact_and_regular() {
    let fixture = Fixture::with_marker_and_irw();
    fixture.write(MARKER_NAME, b"not the ownership marker");
    assert!(inspect_backup(&fixture.root, &Limits::default()).is_err());
    let mut oversized = MARKER.to_vec();
    oversized.push(0);
    fixture.write(MARKER_NAME, &oversized);
    assert!(inspect_backup(&fixture.root, &Limits::default()).is_err());
    fs::remove_file(fixture.root.join(MARKER_NAME)).unwrap();
    fs::create_dir(fixture.root.join(MARKER_NAME)).unwrap();
    assert!(inspect_backup(&fixture.root, &Limits::default()).is_err());
}

#[test]
fn file_byte_and_entry_limits_allow_exact_boundary_and_reject_one_less() {
    let fixture = Fixture::with_marker_and_irw();
    let mut limits = Limits {
        max_files: 2,
        max_bytes: MARKER.len() as u64 + 3,
        max_entries: 2,
        ..Limits::default()
    };
    assert!(inspect_backup(&fixture.root, &limits).is_ok());
    limits.max_files = 1;
    assert!(inspect_backup(&fixture.root, &limits).is_err());
    limits.max_files = 2;
    limits.max_bytes -= 1;
    assert!(inspect_backup(&fixture.root, &limits).is_err());
    limits.max_bytes += 1;
    limits.max_entries = 1;
    assert!(inspect_backup(&fixture.root, &limits).is_err());
}

#[test]
fn depth_and_path_storage_limits_fail_closed() {
    let fixture = Fixture::with_marker_and_irw();
    fs::create_dir(fixture.root.join("one")).unwrap();
    fs::create_dir(fixture.root.join("one").join("two")).unwrap();
    let mut limits = Limits {
        max_depth: 1,
        ..Limits::default()
    };
    assert!(inspect_backup(&fixture.root, &limits).is_err());
    limits.max_depth = 2;
    assert!(inspect_backup(&fixture.root, &limits).is_ok());
    limits.max_path_bytes = MARKER_NAME.len() - 1;
    assert!(inspect_backup(&fixture.root, &limits).is_err());
    limits.max_path_bytes = Limits::default().max_path_bytes;
    limits.max_total_path_bytes = 1;
    assert!(inspect_backup(&fixture.root, &limits).is_err());
}

#[cfg(unix)]
#[test]
fn symbolic_links_are_rejected_even_when_the_target_is_a_regular_file() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::with_marker_and_irw();
    symlink(fixture.root.join("flat.irw"), fixture.root.join("link")).unwrap();
    assert!(inspect_backup(&fixture.root, &Limits::default()).is_err());
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_in_the_backup_parent_path_is_rejected() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let parent = fixture.root.join("real-parent");
    let backup = parent.join("backup");
    fs::create_dir(&parent).unwrap();
    fs::create_dir(&backup).unwrap();
    fs::write(backup.join(MARKER_NAME), MARKER).unwrap();
    fs::write(backup.join("flat.irw"), b"abc").unwrap();
    let link = fixture.root.join("parent-link");
    symlink(&parent, &link).unwrap();
    assert!(inspect_backup(&link.join("backup"), &Limits::default()).is_err());
    assert_eq!(fs::read(backup.join("flat.irw")).unwrap(), b"abc");
}

#[cfg(windows)]
fn powershell_junction_path(path: &Path) -> PathBuf {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let input: Vec<u16> = path.as_os_str().encode_wide().collect();
    let unc: Vec<u16> = r"\\?\UNC\".encode_utf16().collect();
    let verbatim: Vec<u16> = r"\\?\".encode_utf16().collect();
    let output = if let Some(rest) = input.strip_prefix(unc.as_slice()) {
        let mut result: Vec<u16> = r"\\".encode_utf16().collect();
        result.extend_from_slice(rest);
        result
    } else if let Some(rest) = input.strip_prefix(verbatim.as_slice()) {
        rest.to_vec()
    } else {
        input
    };
    PathBuf::from(OsString::from_wide(&output))
}

#[cfg(windows)]
#[test]
fn junction_test_path_preserves_unc_root() {
    assert_eq!(
        powershell_junction_path(Path::new(r"\\?\UNC\server\share\dir")),
        PathBuf::from(r"\\server\share\dir")
    );
    assert_eq!(
        powershell_junction_path(Path::new(r"\\?\C:\dir")),
        PathBuf::from(r"C:\dir")
    );
}

#[cfg(windows)]
#[test]
#[ignore = "requires permission to create synthetic Windows junctions; run explicitly"]
fn windows_junctions_are_rejected_at_root_and_inside_backup() {
    use std::os::windows::fs::MetadataExt;
    use std::process::{Command, Stdio};

    let fixture = Fixture::new();
    let backup = fixture.root.join("backup");
    let outside = fixture.root.join("outside");
    fs::create_dir(&backup).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(backup.join(MARKER_NAME), MARKER).unwrap();
    fs::write(backup.join("flat.irw"), b"abc").unwrap();
    fs::write(outside.join("sentinel"), b"untouched").unwrap();

    let root_link = fixture.root.join("root-junction");
    let inner_link = backup.join("inside-junction");
    let parent_link = fixture.root.join("parent-junction");
    let powershell = PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot is required"))
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    let create_junction = |link: &Path, target: &Path| {
        // Values are supplied through dedicated environment variables rather
        // than interpolated into a shell command. Both paths are inside this
        // fixture under the private Cargo target directory.
        // Windows PowerShell's junction provider rejects verbatim paths.
        // Preserve the leading UNC root when translating such a path.
        let link = powershell_junction_path(link);
        let target = powershell_junction_path(target);
        let status = Command::new(&powershell)
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$ErrorActionPreference='Stop'; New-Item -ItemType Junction -Path $env:IR1710_TEST_JUNCTION -Target $env:IR1710_TEST_JUNCTION_TARGET | Out-Null",
            ])
            .env("IR1710_TEST_JUNCTION", &link)
            .env("IR1710_TEST_JUNCTION_TARGET", &target)
            .stdin(Stdio::null())
            .status()
            .expect("PowerShell is needed to create the test-only junction");
        assert!(status.success(), "could not create test-only junction");
        let metadata = fs::symlink_metadata(&link).unwrap();
        assert_ne!(metadata.file_attributes() & 0x400, 0);
    };

    create_junction(&root_link, &backup);
    assert!(inspect_backup(&root_link, &Limits::default()).is_err());
    create_junction(&inner_link, &outside);
    assert!(inspect_backup(&backup, &Limits::default()).is_err());
    assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"untouched");
    fs::remove_dir(&inner_link).unwrap();
    fs::remove_dir(&root_link).unwrap();
    assert_eq!(
        fs::symlink_metadata(&inner_link).unwrap_err().kind(),
        ErrorKind::NotFound
    );
    assert_eq!(
        fs::symlink_metadata(&root_link).unwrap_err().kind(),
        ErrorKind::NotFound
    );
    assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"untouched");

    let real_parent = fixture.root.join("real-parent");
    let nested_backup = real_parent.join("backup");
    fs::create_dir(&real_parent).unwrap();
    fs::create_dir(&nested_backup).unwrap();
    fs::write(nested_backup.join(MARKER_NAME), MARKER).unwrap();
    fs::write(nested_backup.join("flat.irw"), b"abc").unwrap();
    create_junction(&parent_link, &real_parent);
    assert!(inspect_backup(&parent_link.join("backup"), &Limits::default()).is_err());
    assert_eq!(fs::read(nested_backup.join("flat.irw")).unwrap(), b"abc");
    fs::remove_dir(&parent_link).unwrap();
    assert_eq!(
        fs::symlink_metadata(&parent_link).unwrap_err().kind(),
        ErrorKind::NotFound
    );
}
