//! Run only the dedicated offline verifier child; never signal other processes.

use std::fmt;
use std::io;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub(crate) struct VerifyProcessError {
    message: String,
    child_reaped: bool,
}

impl VerifyProcessError {
    /// False means the caller must leave the scratch tree in place.
    pub(crate) fn child_reaped(&self) -> bool {
        self.child_reaped
    }

    fn new(message: String, child_reaped: bool) -> Self {
        Self {
            message,
            child_reaped,
        }
    }

    #[cfg(test)]
    pub(crate) fn unreaped_for_test() -> Self {
        Self::new(
            "synthetic child termination not confirmed".to_owned(),
            false,
        )
    }
}

impl fmt::Display for VerifyProcessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for VerifyProcessError {}

/// Launch the native read-only check against the isolated world copy.
pub(crate) fn verify_world(
    server: &Path,
    world: &Path,
    cwd: &Path,
    timeout: Duration,
) -> Result<(), VerifyProcessError> {
    let mut command = Command::new(server);
    command.arg("--verify-world").arg(world).current_dir(cwd);
    run_command(command, timeout)
}

trait OwnedChild {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>>;
    fn kill(&mut self) -> io::Result<()>;
    fn wait(&mut self) -> io::Result<ExitStatus>;
}

impl OwnedChild for Child {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        Child::try_wait(self)
    }
    fn kill(&mut self) -> io::Result<()> {
        Child::kill(self)
    }
    fn wait(&mut self) -> io::Result<ExitStatus> {
        Child::wait(self)
    }
}

fn stop_and_reap(child: &mut impl OwnedChild, reason: &str) -> VerifyProcessError {
    match child.kill() {
        Ok(()) => match child.wait() {
            Ok(_) => VerifyProcessError::new(reason.to_owned(), true),
            Err(error) => VerifyProcessError::new(format!("{reason}; wait failed: {error}"), false),
        },
        Err(kill_error) => {
            // A child may already have exited between try_wait and kill. Do not
            // block forever in wait if kill failed while the child remains alive.
            match child.try_wait() {
                Ok(Some(_)) => {
                    VerifyProcessError::new(format!("{reason}; kill failed: {kill_error}"), true)
                }
                Ok(None) => VerifyProcessError::new(
                    format!("{reason}; kill failed: {kill_error}; child may still be running"),
                    false,
                ),
                Err(wait_error) => VerifyProcessError::new(
                    format!("{reason}; kill failed: {kill_error}; try_wait failed: {wait_error}"),
                    false,
                ),
            }
        }
    }
}

fn monitor(child: &mut impl OwnedChild, deadline: Instant) -> Result<(), VerifyProcessError> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(VerifyProcessError::new(
                    format!("native verifier exited unsuccessfully: {status}"),
                    true,
                ))
            }
            Ok(None) => {}
            Err(error) => {
                return Err(stop_and_reap(
                    child,
                    &format!("native verifier polling failed: {error}"),
                ))
            }
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(stop_and_reap(child, "native verifier timed out"));
        }
        thread::sleep(
            deadline
                .saturating_duration_since(now)
                .min(Duration::from_millis(20)),
        );
    }
}

fn run_command(mut command: Command, timeout: Duration) -> Result<(), VerifyProcessError> {
    let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
        VerifyProcessError::new("native verifier timeout is too large".to_owned(), true)
    })?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| {
            VerifyProcessError::new(format!("cannot spawn native verifier: {error}"), true)
        })?;
    monitor(&mut child, deadline)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .arg("restore_drill_process_fixture")
            .arg("--test-threads=1")
            .env("INDUSTRIAL_RESTORE_TEST_PROCESS_MODE", mode);
        command
    }

    #[test]
    fn restore_drill_process_fixture() {
        match std::env::var("INDUSTRIAL_RESTORE_TEST_PROCESS_MODE").as_deref() {
            Ok("success") => {}
            Ok("failure") => std::process::exit(12),
            Ok("sleep") => thread::sleep(Duration::from_secs(30)),
            Ok(_) => panic!("unexpected fixture mode"),
            Err(_) => {}
        }
    }

    #[test]
    fn real_child_success_failure_timeout_and_spawn_failure() {
        assert!(run_command(fixture("success"), Duration::from_secs(10)).is_ok());
        let failure = run_command(fixture("failure"), Duration::from_secs(10)).unwrap_err();
        assert!(failure.child_reaped());
        assert!(failure.to_string().contains("unsuccessfully"));
        let timeout = run_command(fixture("sleep"), Duration::from_millis(200)).unwrap_err();
        assert!(timeout.child_reaped());
        assert!(timeout.to_string().contains("timed out"));
        // An impossible child of the executable file does not create a file.
        let nonexistent = std::env::current_exe().unwrap().join("missing-executable");
        let failure = run_command(Command::new(nonexistent), Duration::from_secs(1)).unwrap_err();
        assert!(failure.child_reaped());
        assert!(failure.to_string().contains("spawn"));
    }

    #[test]
    fn unconfirmed_termination_requires_retaining_scratch() {
        struct Stub {
            fail_kill: bool,
            waited: bool,
        }
        impl OwnedChild for Stub {
            fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
                Ok(None)
            }
            fn kill(&mut self) -> io::Result<()> {
                if self.fail_kill {
                    Err(io::Error::other("kill denied"))
                } else {
                    Ok(())
                }
            }
            fn wait(&mut self) -> io::Result<ExitStatus> {
                self.waited = true;
                Err(io::Error::other("wait failed"))
            }
        }
        let mut denied = Stub {
            fail_kill: true,
            waited: false,
        };
        let error = stop_and_reap(&mut denied, "timeout");
        assert!(!error.child_reaped());
        assert!(!denied.waited);
        assert!(error.to_string().contains("kill failed"));
        let mut wait_failed = Stub {
            fail_kill: false,
            waited: false,
        };
        let error = stop_and_reap(&mut wait_failed, "timeout");
        assert!(!error.child_reaped());
        assert!(wait_failed.waited);
        assert!(error.to_string().contains("wait failed"));
    }
}
