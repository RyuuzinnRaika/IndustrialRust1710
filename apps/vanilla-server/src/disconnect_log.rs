//! Shared stderr budget for selected Protocol 5 receive diagnostics.
//!
//! A fresh TCP session may fail during Handshake or Login before it has a
//! player identity. Logging every malformed UTF-8 frame unconditionally lets
//! unauthenticated traffic flood stderr, even when packet decoding is safe.
use std::io::Write;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(60);
const MAX_INVALID_PACKET_LOGS: u32 = 16;

#[cfg(test)]
static DIAGNOSTIC_WRITES: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static DIAGNOSTIC_WRITE_ERRORS: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
pub(crate) fn diagnostic_write_counts() -> (usize, usize) {
    (
        DIAGNOSTIC_WRITES.load(Ordering::Relaxed),
        DIAGNOSTIC_WRITE_ERRORS.load(Ordering::Relaxed),
    )
}

#[derive(Default)]
struct InvalidPacketLogBudget {
    window_start: Option<Instant>,
    emitted: u32,
}

impl InvalidPacketLogBudget {
    fn permits(&mut self, now: Instant) -> bool {
        let expired = match self.window_start {
            Some(start) => now.saturating_duration_since(start) >= WINDOW,
            None => true,
        };
        if expired {
            self.window_start = Some(now);
            self.emitted = 0;
        }
        if self.emitted >= MAX_INVALID_PACKET_LOGS {
            return false;
        }
        self.emitted += 1;
        true
    }
}

#[derive(Clone, Copy)]
enum LogPolicy {
    Silent,
    Always,
    Budgeted,
}

fn policy(category: &str) -> LogPolicy {
    match category {
        "peer_closed" => LogPolicy::Silent,
        "invalid_packet_or_state" | "truncated_frame_eof" => LogPolicy::Budgeted,
        _ => LogPolicy::Always,
    }
}

fn decide(policy: LogPolicy, budget: &mut InvalidPacketLogBudget, now: Instant) -> bool {
    match policy {
        LogPolicy::Silent => false,
        LogPolicy::Always => true,
        LogPolicy::Budgeted => budget.permits(now),
    }
}

/// A diagnostic write failure must not replace the normal receive-EOF result;
/// the caller still needs to save and release the player.
pub(crate) fn log_truncated_frame<W: Write>(writer: &mut W, entity_id: i32) {
    #[cfg(test)]
    DIAGNOSTIC_WRITES.fetch_add(1, Ordering::Relaxed);
    let result = writeln!(
        writer,
        "client_receive_end category=truncated_frame_eof phase=play entity_id={entity_id}"
    );
    #[cfg(test)]
    if result.is_err() {
        DIAGNOSTIC_WRITE_ERRORS.fetch_add(1, Ordering::Relaxed);
    }
    let _ = result;
}

/// InvalidData / InvalidInput and incomplete-frame EOF diagnostics share a
/// global per-minute budget.
/// Other non-peer-close failures remain visible for operational diagnostics.
/// A poisoned lock fails closed for budgeted diagnostics.
pub(crate) fn should_log(category: &str) -> bool {
    let policy = policy(category);
    match policy {
        LogPolicy::Silent => return false,
        LogPolicy::Always => return true,
        LogPolicy::Budgeted => {}
    }
    static BUDGET: OnceLock<Mutex<InvalidPacketLogBudget>> = OnceLock::new();
    let budget = BUDGET.get_or_init(|| Mutex::new(InvalidPacketLogBudget::default()));
    budget
        .lock()
        .map(|mut guard| decide(policy, &mut guard, Instant::now()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    struct BrokenWriter {
        calls: usize,
        partial_first: bool,
    }

    impl Write for BrokenWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.calls += 1;
            if self.partial_first && self.calls == 1 && !bytes.is_empty() {
                return Ok(1);
            }
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "synthetic failure",
            ))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn failed_and_partial_diagnostic_writes_preserve_success_result() {
        for partial_first in [false, true] {
            let mut writer = BrokenWriter {
                calls: 0,
                partial_first,
            };
            log_truncated_frame(&mut writer, 42);
            assert!(writer.calls > if partial_first { 1 } else { 0 });
        }
        let mut output = Vec::new();
        log_truncated_frame(&mut output, 42);
        assert_eq!(
            output,
            b"client_receive_end category=truncated_frame_eof phase=play entity_id=42\n"
        );
    }

    #[test]
    fn invalid_packet_error_output_has_a_fixed_per_minute_budget() {
        let start = Instant::now();
        let mut budget = InvalidPacketLogBudget::default();
        for _ in 0..MAX_INVALID_PACKET_LOGS {
            assert!(budget.permits(start));
        }
        for _ in 0..1_000 {
            assert!(!budget.permits(start + WINDOW - Duration::from_nanos(1)));
        }
        assert!(budget.permits(start + WINDOW));
        for _ in 1..MAX_INVALID_PACKET_LOGS {
            assert!(budget.permits(start + WINDOW));
        }
        assert!(!budget.permits(start + WINDOW));
    }

    #[test]
    fn peer_close_is_silent_and_other_failure_categories_remain_visible() {
        assert!(!should_log("peer_closed"));
        assert!(should_log("io_timeout"));
        assert!(should_log("server_io_or_state"));
    }

    #[test]
    fn incomplete_and_invalid_diagnostics_share_one_budget() {
        let start = Instant::now();
        let mut budget = InvalidPacketLogBudget::default();
        assert!(!decide(policy("peer_closed"), &mut budget, start));
        assert!(decide(policy("io_timeout"), &mut budget, start));
        for index in 0..MAX_INVALID_PACKET_LOGS {
            let category = if index % 2 == 0 {
                "invalid_packet_or_state"
            } else {
                "truncated_frame_eof"
            };
            assert!(decide(policy(category), &mut budget, start));
        }
        assert!(!decide(
            policy("invalid_packet_or_state"),
            &mut budget,
            start
        ));
        assert!(!decide(policy("truncated_frame_eof"), &mut budget, start));
        assert!(decide(policy("server_io_or_state"), &mut budget, start));
        assert!(!decide(policy("peer_closed"), &mut budget, start));
        assert!(decide(
            policy("truncated_frame_eof"),
            &mut budget,
            start + WINDOW
        ));
        assert!(decide(
            policy("invalid_packet_or_state"),
            &mut budget,
            start + WINDOW
        ));
    }
}
