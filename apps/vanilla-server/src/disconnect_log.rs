//! Shared stderr budget for malformed Protocol 5 connections.
//!
//! A fresh TCP session may fail during Handshake or Login before it has a
//! player identity. Logging every malformed UTF-8 frame unconditionally lets
//! unauthenticated traffic flood stderr, even when packet decoding is safe.
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(60);
const MAX_INVALID_PACKET_LOGS: u32 = 16;

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

/// InvalidData / InvalidInput disconnects are capped globally per minute.
/// Other non-peer-close failures remain visible for operational diagnostics.
/// A poisoned lock fails closed (silences malformed-packet diagnostics).
pub(crate) fn should_log(category: &str) -> bool {
    if category == "peer_closed" {
        return false;
    }
    if category != "invalid_packet_or_state" {
        return true;
    }
    static BUDGET: OnceLock<Mutex<InvalidPacketLogBudget>> = OnceLock::new();
    let budget = BUDGET.get_or_init(|| Mutex::new(InvalidPacketLogBudget::default()));
    budget
        .lock()
        .map(|mut guard| guard.permits(Instant::now()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
