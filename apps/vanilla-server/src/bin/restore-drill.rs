use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[path = "restore_drill/process.rs"]
mod process;
#[path = "restore_drill/snapshot.rs"]
mod snapshot;

use snapshot::{Entry, Limits, Snapshot, WorldLayout};

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_owned())
}

struct Options {
    server: PathBuf,
    backup: PathBuf,
    scratch_root: PathBuf,
    protected: Vec<PathBuf>,
    timeout: Duration,
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> io::Result<Options> {
    let mut server = None;
    let mut backup = None;
    let mut scratch_root = None;
    let mut timeout = None;
    let mut protected = Vec::new();
    let mut args = args.into_iter();
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| invalid("option requires a value"))?;
        let slot = match flag.to_str() {
            Some("--server") => &mut server,
            Some("--backup") => &mut backup,
            Some("--scratch-root") => &mut scratch_root,
            Some("--protect-world") => {
                protected.push(PathBuf::from(value));
                continue;
            }
            Some("--timeout-seconds") => {
                if timeout.is_some() {
                    return Err(invalid("duplicate timeout"));
                }
                let seconds = value
                    .to_str()
                    .and_then(|v| v.parse::<u64>().ok())
                    .filter(|seconds| (1..=86400).contains(seconds))
                    .ok_or_else(|| invalid("timeout must be 1 to 86400 seconds"))?;
                timeout = Some(Duration::from_secs(seconds));
                continue;
            }
            _ => return Err(invalid("unknown option")),
        };
        if slot.replace(PathBuf::from(value)).is_some() {
            return Err(invalid("duplicate option"));
        }
    }
    Ok(Options {
        server: server.ok_or_else(|| invalid("missing --server"))?,
        backup: backup.ok_or_else(|| invalid("missing --backup"))?,
        scratch_root: scratch_root.ok_or_else(|| invalid("missing --scratch-root"))?,
        protected,
        timeout: timeout.unwrap_or(Duration::from_secs(120)),
    })
}

fn canonical_directory(path: &Path) -> io::Result<PathBuf> {
    snapshot::ensure_no_reparse_ancestors(path)?;
    fs::canonicalize(path)
}

fn overlap(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

fn server_path(path: &Path) -> io::Result<PathBuf> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    canonical_directory(parent)?;
    let meta = fs::symlink_metadata(path)?;
    if !snapshot::is_plain_file(&meta) {
        return Err(invalid("server must be a regular file without links"));
    }
    fs::canonicalize(path)
}

fn protected_path(path: &Path) -> io::Result<PathBuf> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_dir() {
        canonical_directory(path)
    } else {
        let file = server_path(path)?;
        Ok(file
            .parent()
            .ok_or_else(|| invalid("protected file has no parent"))?
            .to_path_buf())
    }
}

fn scratch_diagnostic(path: &Path, state: io::Result<fs::Metadata>) -> String {
    match state {
        Ok(_) => format!(
            "scratch path present; inspect before handling: {}",
            path.display()
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            format!("scratch path no longer exists: {}", path.display())
        }
        Err(error) => format!("scratch state unknown at {}: {error}", path.display()),
    }
}

fn make_scratch(root: &Path) -> io::Result<PathBuf> {
    for _ in 0..32 {
        let path = root.join(format!("restore-drill-{:032x}", rand::random::<u128>()));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(invalid("could not allocate unique scratch directory"))
}

fn copy_snapshot(source: &Path, target: &Path, snapshot: &Snapshot) -> io::Result<()> {
    for (relative, entry) in &snapshot.entries {
        let src = source.join(relative);
        let dest = target.join(relative);
        match entry {
            Entry::Directory => fs::create_dir(dest)?,
            Entry::File { bytes, .. } => {
                let before = fs::symlink_metadata(&src)?;
                if !snapshot::is_plain_file(&before) || before.len() != *bytes {
                    return Err(invalid("source changed before copy"));
                }
                let mut input = File::open(&src)?;
                if !input.metadata()?.is_file() || input.metadata()?.len() != *bytes {
                    return Err(invalid("source changed when opened"));
                }
                let mut output = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&dest)?;
                let mut buf = [0u8; 65536];
                let mut count = 0u64;
                loop {
                    let n = input.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    count = count
                        .checked_add(n as u64)
                        .ok_or_else(|| invalid("copy length overflow"))?;
                    if count > *bytes {
                        return Err(invalid("source grew during copy"));
                    }
                    output.write_all(&buf[..n])?;
                }
                output.flush()?;
                let after = fs::symlink_metadata(&src)?;
                if count != *bytes
                    || input.metadata()?.len() != *bytes
                    || !snapshot::is_plain_file(&after)
                    || after.len() != *bytes
                {
                    return Err(invalid("source changed during copy"));
                }
            }
        }
    }
    Ok(())
}

fn assert_snapshot(path: &Path, expected: &Snapshot, limits: &Limits) -> io::Result<()> {
    if snapshot::snapshot_tree(path, limits)? != *expected {
        return Err(invalid("tree changed during verification"));
    }
    Ok(())
}

fn run(options: Options) -> io::Result<()> {
    run_with(options, process::verify_world)
}

fn run_with(
    options: Options,
    verifier: impl FnOnce(&Path, &Path, &Path, Duration) -> Result<(), process::VerifyProcessError>,
) -> io::Result<()> {
    let backup = canonical_directory(&options.backup)?;
    let scratch_root = canonical_directory(&options.scratch_root)?;
    if overlap(&backup, &scratch_root) {
        return Err(invalid("backup and scratch root overlap"));
    }
    for world in &options.protected {
        let world = protected_path(world)?;
        if overlap(&world, &backup) || overlap(&world, &scratch_root) {
            return Err(invalid("protected world overlaps backup or scratch root"));
        }
    }
    let server = server_path(&options.server)?;
    if server.starts_with(&backup) || server.starts_with(&scratch_root) {
        return Err(invalid("server cannot be inside backup or scratch root"));
    }
    let limits = Limits::default();
    let (layout, initial) = snapshot::inspect_backup(&backup, &limits)?;
    let scratch = make_scratch(&scratch_root)?;
    let result = (|| -> io::Result<()> {
        copy_snapshot(&backup, &scratch, &initial)?;
        assert_snapshot(&scratch, &initial, &limits)?;
        assert_snapshot(&backup, &initial, &limits)?;
        let target = match layout {
            WorldLayout::Anvil => scratch.clone(),
            WorldLayout::Irw(name) => scratch.join(name),
        };
        verifier(&server, &target, &scratch, options.timeout).map_err(|error| {
            invalid(&format!(
                "native verification failed: {error}; child reaped: {}",
                error.child_reaped()
            ))
        })?;
        assert_snapshot(&scratch, &initial, &limits)?;
        assert_snapshot(&backup, &initial, &limits)?;
        if canonical_directory(&scratch)? != scratch
            || scratch.parent() != Some(scratch_root.as_path())
        {
            return Err(invalid("scratch ownership could not be confirmed"));
        }
        fs::remove_dir_all(&scratch)?;
        match fs::symlink_metadata(&scratch) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Ok(_) => Err(invalid("scratch remains after cleanup")),
            Err(error) => Err(error),
        }
    })();
    if result.is_err() {
        eprintln!(
            "{}",
            scratch_diagnostic(&scratch, fs::symlink_metadata(&scratch))
        );
    }
    result
}

fn main() {
    if let Err(error) = parse_args(std::env::args_os().skip(1)).and_then(run) {
        eprintln!("restore drill failed: {error}");
        std::process::exit(1);
    }
    println!("offline restore verification succeeded");
}

#[cfg(test)]
#[path = "restore_drill/cli_tests.rs"]
mod tests;
