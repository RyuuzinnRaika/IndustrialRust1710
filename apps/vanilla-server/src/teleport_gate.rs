//! Protocol 5 has no teleport transaction ID. A server-issued S08 position
//! update is acknowledged by the next C04/C06 reporting that destination.
//! Until then, old-location world actions must never be processed.
use crate::player::PlayerPosition;
use std::time::{Duration, Instant};

const ACK_TOLERANCE: f64 = 0.25;
const RESEND_INTERVAL: Duration = Duration::from_secs(1);
const ACK_TIMEOUT: Duration = Duration::from_secs(12);

pub struct TeleportGate {
    target: PlayerPosition,
    started: Instant,
    last_sent: Instant,
}

impl TeleportGate {
    pub fn new(target: PlayerPosition, now: Instant) -> Self {
        Self {
            target,
            started: now,
            last_sent: now,
        }
    }

    /// Only a finite, near-exact C04/C06 position can acknowledge S08.
    /// C03 (onGround) and C05 (look) cannot prove arrival.
    pub fn acknowledges(&self, received: PlayerPosition) -> bool {
        received.is_valid()
            && (received.x - self.target.x).abs() <= ACK_TOLERANCE
            && (received.y - self.target.y).abs() <= ACK_TOLERANCE
            && (received.z - self.target.z).abs() <= ACK_TOLERANCE
    }

    pub fn timed_out(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.started) >= ACK_TIMEOUT
    }

    pub fn should_resend(&mut self, now: Instant) -> bool {
        if now.saturating_duration_since(self.last_sent) < RESEND_INTERVAL {
            return false;
        }
        self.last_sent = now;
        true
    }

    /// C02 entity interaction, C07 digging/dropping, C08 placement/use
    /// and C12 sign edits all mutate world state at a client-chosen location.
    pub fn blocks_world_action(packet_id: u32) -> bool {
        matches!(packet_id, 2 | 7 | 8 | 18)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f64, y: f64, z: f64) -> PlayerPosition {
        PlayerPosition { x, y, z }
    }

    #[test]
    fn add014_only_destination_position_acknowledges_teleport() {
        let now = Instant::now();
        let gate = TeleportGate::new(at(80.5, 70.0, -40.5), now);
        assert!(!gate.acknowledges(at(0.5, 65.0, 0.5)));
        assert!(!gate.acknowledges(at(80.5, 70.5, -40.5)));
        assert!(!gate.acknowledges(at(f64::NAN, 70.0, -40.5)));
        assert!(gate.acknowledges(at(80.5, 70.0, -40.5)));
        assert!(gate.acknowledges(at(80.55, 70.05, -40.55)));
    }

    #[test]
    fn add014_retries_are_bounded_and_missing_ack_expires() {
        let now = Instant::now();
        let mut gate = TeleportGate::new(at(50.5, 65.0, 0.5), now);
        assert!(!gate.should_resend(now + Duration::from_millis(999)));
        assert!(gate.should_resend(now + Duration::from_secs(1)));
        assert!(!gate.should_resend(now + Duration::from_millis(1100)));
        assert!(!gate.timed_out(now + Duration::from_secs(11)));
        assert!(gate.timed_out(now + Duration::from_secs(12)));
    }

    #[test]
    fn add014_blocks_old_world_mutations_but_not_position_ack_or_keepalive() {
        for packet in [2, 7, 8, 18] {
            assert!(TeleportGate::blocks_world_action(packet));
        }
        for packet in [0, 3, 4, 5, 6] {
            assert!(!TeleportGate::blocks_world_action(packet));
        }
    }
}
