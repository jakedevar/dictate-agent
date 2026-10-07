//! Per-peer request throttling and the authentication-failure lockout
//! (design §9).
//!
//! Keyed on the TCP peer address only. No forwarding header is trusted: behind
//! a reverse proxy every client shares one bucket, which fails conservative.
//! The table is bounded, so a scan from many addresses cannot grow it without
//! limit.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Failures within [`FAILURE_WINDOW`] that lock a peer out.
pub const MAX_AUTH_FAILURES: u32 = 5;
/// The window authentication failures are counted over.
pub const FAILURE_WINDOW: Duration = Duration::from_secs(60);
/// How long a locked-out peer is refused.
pub const LOCKOUT: Duration = Duration::from_secs(60);
/// Peers tracked before idle entries are pruned.
const TABLE_CAP: usize = 1024;
/// An entry untouched this long is dropped when the table is full.
const IDLE: Duration = Duration::from_secs(600);

/// Why a request was throttled, with how long to wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Throttled {
    /// The peer is over its request rate.
    Rate(Duration),
    /// The peer failed authentication too often.
    LockedOut(Duration),
}

impl Throttled {
    /// Seconds to put in `Retry-After` (at least 1).
    #[must_use]
    pub fn retry_after_secs(&self) -> u64 {
        let d = match self {
            Self::Rate(d) | Self::LockedOut(d) => *d,
        };
        (d.as_secs() + u64::from(d.subsec_nanos() > 0)).max(1)
    }
}

#[derive(Debug)]
struct Peer {
    tokens: f64,
    refilled: Instant,
    failures: u32,
    window_start: Instant,
    locked_until: Option<Instant>,
    seen: Instant,
}

/// Token-bucket rate limit plus failure lockout, per peer IP.
#[derive(Debug)]
pub struct Throttle {
    per_second: f64,
    burst: f64,
    peers: Mutex<HashMap<IpAddr, Peer>>,
}

impl Throttle {
    /// `requests_per_minute` sustained, `burst` at once.
    #[must_use]
    pub fn new(requests_per_minute: u32, burst: u32) -> Self {
        Self {
            per_second: f64::from(requests_per_minute.max(1)) / 60.0,
            burst: f64::from(burst.max(1)),
            peers: Mutex::new(HashMap::new()),
        }
    }

    /// Charge one request to `ip`.
    ///
    /// # Errors
    ///
    /// [`Throttled`] when the peer is locked out or over its rate.
    pub fn check(&self, ip: IpAddr, now: Instant) -> Result<(), Throttled> {
        let Ok(mut peers) = self.peers.lock() else {
            // A poisoned table must not become an open door.
            return Err(Throttled::Rate(Duration::from_secs(1)));
        };
        self.make_room(&mut peers, now);
        let burst = self.burst;
        let peer = peers.entry(ip).or_insert_with(|| Peer {
            tokens: burst,
            refilled: now,
            failures: 0,
            window_start: now,
            locked_until: None,
            seen: now,
        });
        peer.seen = now;
        if let Some(until) = peer.locked_until {
            if now < until {
                return Err(Throttled::LockedOut(until - now));
            }
            peer.locked_until = None;
            peer.failures = 0;
            peer.window_start = now;
        }
        let elapsed = now.saturating_duration_since(peer.refilled).as_secs_f64();
        peer.tokens = (peer.tokens + elapsed * self.per_second).min(self.burst);
        peer.refilled = now;
        if peer.tokens >= 1.0 {
            peer.tokens -= 1.0;
            Ok(())
        } else {
            let wait = (1.0 - peer.tokens) / self.per_second;
            Err(Throttled::Rate(Duration::from_secs_f64(wait)))
        }
    }

    /// Record an authentication failure from `ip`; returns whether the peer
    /// is now locked out.
    pub fn record_failure(&self, ip: IpAddr, now: Instant) -> bool {
        let Ok(mut peers) = self.peers.lock() else {
            return true;
        };
        let burst = self.burst;
        let peer = peers.entry(ip).or_insert_with(|| Peer {
            tokens: burst,
            refilled: now,
            failures: 0,
            window_start: now,
            locked_until: None,
            seen: now,
        });
        peer.seen = now;
        if now.saturating_duration_since(peer.window_start) > FAILURE_WINDOW {
            peer.window_start = now;
            peer.failures = 0;
        }
        peer.failures += 1;
        if peer.failures >= MAX_AUTH_FAILURES {
            peer.locked_until = Some(now + LOCKOUT);
            true
        } else {
            false
        }
    }

    /// Peers currently tracked (for tests and diagnostics).
    #[must_use]
    pub fn tracked(&self) -> usize {
        self.peers.lock().map(|p| p.len()).unwrap_or(0)
    }

    /// Keep the table bounded: drop idle peers, and if that is not enough,
    /// drop the least recently seen ones that are not locked out.
    fn make_room(&self, peers: &mut HashMap<IpAddr, Peer>, now: Instant) {
        if peers.len() < TABLE_CAP {
            return;
        }
        peers.retain(|_, p| {
            now.saturating_duration_since(p.seen) < IDLE || p.locked_until.is_some_and(|u| u > now)
        });
        if peers.len() < TABLE_CAP {
            return;
        }
        let mut by_age: Vec<(IpAddr, Instant)> = peers
            .iter()
            .filter(|(_, p)| p.locked_until.is_none_or(|u| u <= now))
            .map(|(ip, p)| (*ip, p.seen))
            .collect();
        by_age.sort_by_key(|(_, seen)| *seen);
        let excess = peers.len() + 1 - TABLE_CAP;
        for (ip, _) in by_age.into_iter().take(excess) {
            peers.remove(&ip);
        }
        // If every entry is a live lockout the table may exceed the cap by
        // the number of attacking addresses still locked; they expire within
        // LOCKOUT, and each costs a few dozen bytes.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ip(n: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(192, 0, 2, n))
    }

    #[test]
    fn a_burst_is_allowed_then_the_rate_applies() {
        let t = Throttle::new(60, 3);
        let now = Instant::now();
        for _ in 0..3 {
            assert!(t.check(ip(1), now).is_ok());
        }
        let Err(Throttled::Rate(wait)) = t.check(ip(1), now) else {
            panic!("the fourth request in the same instant must be throttled");
        };
        assert!(wait <= Duration::from_secs(1));
        // One per second refills.
        assert!(t.check(ip(1), now + Duration::from_secs(1)).is_ok());
        // Other peers are unaffected.
        assert!(t.check(ip(2), now).is_ok());
    }

    #[test]
    fn repeated_auth_failures_lock_a_peer_out_and_it_expires() {
        let t = Throttle::new(6000, 100);
        let now = Instant::now();
        for i in 1..MAX_AUTH_FAILURES {
            assert!(!t.record_failure(ip(1), now), "failure {i}");
        }
        assert!(t.record_failure(ip(1), now));
        assert!(matches!(t.check(ip(1), now), Err(Throttled::LockedOut(_))));
        assert!(t.check(ip(2), now).is_ok(), "only the failing peer");
        assert!(t.check(ip(1), now + LOCKOUT).is_ok(), "the lockout ends");
    }

    #[test]
    fn failures_outside_the_window_do_not_accumulate() {
        let t = Throttle::new(6000, 100);
        let mut now = Instant::now();
        for _ in 0..(MAX_AUTH_FAILURES * 3) {
            assert!(!t.record_failure(ip(1), now));
            now += FAILURE_WINDOW / (MAX_AUTH_FAILURES - 1) + Duration::from_secs(1);
        }
    }

    #[test]
    fn the_table_stays_bounded() {
        let t = Throttle::new(60, 5);
        let now = Instant::now();
        for i in 0..(TABLE_CAP as u32 + 500) {
            let addr = IpAddr::V4(Ipv4Addr::from(0x0A00_0000 + i));
            let _ = t.check(addr, now);
        }
        assert!(t.tracked() <= TABLE_CAP, "{}", t.tracked());
    }

    #[test]
    fn retry_after_rounds_up_to_whole_seconds() {
        assert_eq!(
            Throttled::Rate(Duration::from_millis(10)).retry_after_secs(),
            1
        );
        assert_eq!(
            Throttled::Rate(Duration::from_millis(1500)).retry_after_secs(),
            2
        );
        assert_eq!(
            Throttled::LockedOut(Duration::from_secs(60)).retry_after_secs(),
            60
        );
    }
}
