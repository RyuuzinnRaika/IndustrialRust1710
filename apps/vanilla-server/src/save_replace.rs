//! Bounded retries for publishing an already synced save image.
//!
//! Callers keep their save locks, validation, durability and cleanup rules.
//! Never unlink the published destination to work around a sharing denial.
use std::io;
use std::path::Path;
use std::time::Duration;

const RETRY_DELAYS_MS: [u64; 5] = [20, 40, 80, 160, 200];

fn retryable(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::Interrupted
        // Access denied can also be permanent; the finite retry budget still
        // returns that error without changing permissions or deleting files.
        || (cfg!(windows) && matches!(error.raw_os_error(), Some(5 | 32 | 33)))
}

fn retry(
    mut operation: impl FnMut() -> io::Result<()>,
    mut wait: impl FnMut(Duration),
) -> io::Result<()> {
    for delay_ms in RETRY_DELAYS_MS {
        match operation() {
            Ok(()) => return Ok(()),
            Err(error) if retryable(&error) => wait(Duration::from_millis(delay_ms)),
            Err(error) => return Err(error),
        }
    }
    // Six attempts, with at most 500 ms of requested sleep in total. Retain
    // the final OS error so callers can distinguish a persistent denial.
    operation()
}

pub(crate) fn rename_with_retry(
    source: &Path,
    destination: &Path,
    mut rename: impl FnMut(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    retry(|| rename(source, destination), std::thread::sleep)
}

/// Publish a create-only staged file with the same bounded retry policy.
/// A successful link stops the retry loop immediately; no destination is removed.
pub(crate) fn hard_link_with_retry(
    source: &Path,
    destination: &Path,
    mut hard_link: impl FnMut(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    retry(|| hard_link(source, destination), std::thread::sleep)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_publication_can_finish_without_rewriting_the_image() {
        let mut attempts = 0;
        let mut delays = Vec::new();
        retry(
            || {
                attempts += 1;
                if attempts < 4 {
                    Err(io::Error::from(io::ErrorKind::Interrupted))
                } else {
                    Ok(())
                }
            },
            |delay| delays.push(delay),
        )
        .unwrap();
        assert_eq!(attempts, 4);
        assert_eq!(delays.len(), 3);
    }

    #[test]
    fn permanent_interruption_is_bounded_and_returns_the_last_error() {
        let mut attempts = 0;
        let mut waited = Duration::ZERO;
        let error = retry(
            || {
                attempts += 1;
                Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    format!("attempt {attempts}"),
                ))
            },
            |delay| waited += delay,
        )
        .unwrap_err();
        assert_eq!(attempts, 6);
        assert_eq!(waited, Duration::from_millis(500));
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(error.to_string(), "attempt 6");
    }

    #[test]
    fn invalid_path_and_unclassified_permission_errors_are_not_retried() {
        for kind in [io::ErrorKind::InvalidInput, io::ErrorKind::PermissionDenied] {
            let mut attempts = 0;
            let error = retry(
                || {
                    attempts += 1;
                    Err(io::Error::new(kind, "permanent failure"))
                },
                |_| panic!("non-retryable error must not wait"),
            )
            .unwrap_err();
            assert_eq!(attempts, 1);
            assert_eq!(error.kind(), kind);
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_sharing_errors_are_bounded_without_losing_the_os_code() {
        for code in [5, 32, 33] {
            let mut attempts = 0;
            let error = retry(
                || {
                    attempts += 1;
                    Err(io::Error::from_raw_os_error(code))
                },
                |_| {},
            )
            .unwrap_err();
            assert_eq!(attempts, 6);
            assert_eq!(error.raw_os_error(), Some(code));
        }
        for code in [3, 80, 112] {
            assert!(!retryable(&io::Error::from_raw_os_error(code)));
        }
    }
}
