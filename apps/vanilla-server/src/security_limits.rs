//! Bounded per-IP pre-login admission; no new thread is spawned on rejection.
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_ACTIVE_PER_IP: usize = 4;
const MAX_CONNECTS_PER_MINUTE: usize = 24;
const MAX_TRACKED_IPS: usize = 4096;
const WINDOW: Duration = Duration::from_secs(60);

#[derive(Default)]
struct Bucket {
    active: usize,
    attempts: VecDeque<Instant>,
}
#[derive(Default)]
pub struct ConnectionLimiter {
    buckets: Mutex<HashMap<IpAddr, Bucket>>,
}
pub struct Permit {
    guard: Arc<ConnectionLimiter>,
    address: IpAddr,
}
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(addr) => addr.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(addr)),
        other => other,
    }
}
impl ConnectionLimiter {
    /// The caller must retain this permit until the TCP handler exits.
    pub fn acquire(self: &Arc<Self>, ip: IpAddr) -> Option<Permit> {
        let ip = canonical(ip);
        let now = Instant::now();
        let mut buckets = self.buckets.lock().ok()?;
        buckets.retain(|_, bucket| {
            while bucket.attempts.front().is_some_and(|stamp| now.duration_since(*stamp) >= WINDOW) {
                bucket.attempts.pop_front();
            }
            bucket.active > 0 || !bucket.attempts.is_empty()
        });
        if !buckets.contains_key(&ip) && buckets.len() >= MAX_TRACKED_IPS {
            return None;
        }
        let bucket = buckets.entry(ip).or_default();
        if bucket.active >= MAX_ACTIVE_PER_IP || bucket.attempts.len() >= MAX_CONNECTS_PER_MINUTE {
            return None;
        }
        bucket.active += 1;
        bucket.attempts.push_back(now);
        Some(Permit { guard: Arc::clone(self), address: ip })
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        if let Ok(mut buckets) = self.guard.buckets.lock() {
            if let Some(bucket) = buckets.get_mut(&self.address) {
                bucket.active = bucket.active.saturating_sub(1);
            }
        }
    }
}
#[derive(Default)]
pub struct ChatLimiter {
    sent: VecDeque<Instant>,
    last_message: Option<String>,
    violations: usize,
}
impl ChatLimiter {
    /// Repeated messages, malformed text and >5 messages/5s are rejected.
    /// Three rejected packets mean the session should be disconnected.
    pub fn check(&mut self, message: &str) -> Result<(), bool> {
        let now = Instant::now();
        while self.sent.front().is_some_and(|stamp| now.duration_since(*stamp) >= Duration::from_secs(5)) {
            self.sent.pop_front();
        }
        let invalid = message.is_empty() || message.len() > 400
            || message.chars().count() > 100
            || message.chars().any(|c| c.is_control() && c != '\t'
                || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'))
            || self.sent.len() >= 5
            || self.last_message.as_deref() == Some(message);
        if invalid {
            self.violations += 1;
            return Err(self.violations >= 3);
        }
        self.sent.push_back(now);
        self.last_message = Some(message.to_owned());
        self.violations = 0;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn per_ip_limits_and_releases_slots() {
        let limiter = Arc::new(ConnectionLimiter::default());
        let ip: IpAddr = "192.0.2.1".parse().unwrap();
        let permits: Vec<_> = (0..4).map(|_| limiter.acquire(ip).unwrap()).collect();
        assert!(limiter.acquire(ip).is_none());
        assert!(limiter.acquire("::ffff:192.0.2.1".parse().unwrap()).is_none());
        drop(permits);
        assert!(limiter.acquire(ip).is_some());
    }
    #[test]
    fn spam_and_controls_are_rejected() {
        let mut guard = ChatLimiter::default();
        assert!(guard.check("hello").is_ok());
        assert!(guard.check("hello").is_err());
        assert!(guard.check("bad\u{202E}text").is_err());
        assert_eq!(guard.check("bad\nline"), Err(true));
    }
}
