//! Per-peer request throttling, the authentication-failure lockout, and the
//! budget for warnings about refused requests (design §9).
//!
//! Keyed on the TCP peer address only. No forwarding header is trusted: behind
//! a reverse proxy every client shares one bucket, which fails conservative.
//!
//! The table has a hard cap ([`TABLE_CAP`]), enforced under the one lock that
//! every admission and every failure update takes. Idle peers are dropped
//! first, then the least recently seen peer that is not locked out. A live
//! lockout is never evicted to make room: when every entry is one, a peer the
//! table does not already know is refused ([`Throttled::Saturated`]) until the
//! earliest lockout expires. So a scan from many source addresses can neither
//! grow the table nor wash a lockout out of it.

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
/// The most peers ever tracked.
pub const TABLE_CAP: usize = 1024;
/// An entry untouched this long is dropped when the table is full.
const IDLE: Duration = Duration::from_secs(600);

/// Why a request was throttled, with how long to wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Throttled {
    /// The peer is over its request rate.
    Rate(Duration),
    /// The peer failed authentication too often.
    LockedOut(Duration),
    /// The table is full of live lockouts and this peer is not in it: refused
    /// until the earliest lockout expires (fail closed).
    Saturated(Duration),
}

impl Throttled {
    /// How long to wait.
    #[must_use]
    pub fn wait(&self) -> Duration {
        match self {
            Self::Rate(d) | Self::LockedOut(d) | Self::Saturated(d) => *d,
        }
    }

    /// Seconds to put in `Retry-After` (at least 1).
    #[must_use]
    pub fn retry_after_secs(&self) -> u64 {
        let d = self.wait();
        (d.as_secs() + u64::from(d.subsec_nanos() > 0)).max(1)
    }

    /// A short class name for logs.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Rate(_) => "rate",
            Self::LockedOut(_) => "lockout",
            Self::Saturated(_) => "saturated",
        }
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

impl Peer {
    fn new(burst: f64, now: Instant) -> Self {
        Self {
            tokens: burst,
            refilled: now,
            failures: 0,
            window_start: now,
            locked_until: None,
            seen: now,
        }
    }

    fn locked(&self, now: Instant) -> bool {
        self.locked_until.is_some_and(|until| until > now)
    }
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

    /// Charge one request to `ip`. Only an admitted request spends a token,
    /// so a caller that waits [`Throttled::wait`] and asks again is charged
    /// once, and two callers can never spend the same refill.
    ///
    /// # Errors
    ///
    /// [`Throttled`] when the peer is locked out or over its rate, or when it
    /// cannot be tracked because the table is full of live lockouts.
    pub fn check(&self, ip: IpAddr, now: Instant) -> Result<(), Throttled> {
        let Ok(mut peers) = self.peers.lock() else {
            // A poisoned table must not become an open door.
            return Err(Throttled::Rate(Duration::from_secs(1)));
        };
        let peer = self
            .slot(&mut peers, ip, now)
            .map_err(Throttled::Saturated)?;
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
    /// is now locked out. A peer the full table cannot take is reported as
    /// locked out (fail closed).
    pub fn record_failure(&self, ip: IpAddr, now: Instant) -> bool {
        let Ok(mut peers) = self.peers.lock() else {
            return true;
        };
        let Ok(peer) = self.slot(&mut peers, ip, now) else {
            return true;
        };
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

    /// `ip`'s entry, inserting one if the table has (or can make) room.
    ///
    /// # Errors
    ///
    /// The time until the earliest lockout expires, when `ip` is not tracked
    /// and every entry is a live lockout.
    fn slot<'a>(
        &self,
        peers: &'a mut HashMap<IpAddr, Peer>,
        ip: IpAddr,
        now: Instant,
    ) -> Result<&'a mut Peer, Duration> {
        if !peers.contains_key(&ip) {
            make_room(peers, now)?;
        }
        Ok(peers
            .entry(ip)
            .or_insert_with(|| Peer::new(self.burst, now)))
    }
}

/// Free one slot if the table is full: drop idle peers, then the least
/// recently seen peer that is not locked out. Never a live lockout.
fn make_room(peers: &mut HashMap<IpAddr, Peer>, now: Instant) -> Result<(), Duration> {
    if peers.len() < TABLE_CAP {
        return Ok(());
    }
    peers.retain(|_, p| now.saturating_duration_since(p.seen) < IDLE || p.locked(now));
    if peers.len() < TABLE_CAP {
        return Ok(());
    }
    let oldest = peers
        .iter()
        .filter(|(_, p)| !p.locked(now))
        .min_by_key(|(_, p)| p.seen)
        .map(|(ip, _)| *ip);
    if let Some(ip) = oldest {
        peers.remove(&ip);
        return Ok(());
    }
    // Every entry is a live lockout.
    let earliest = peers
        .values()
        .filter_map(|p| p.locked_until)
        .min()
        .map_or(LOCKOUT, |until| until.saturating_duration_since(now));
    Err(earliest.max(Duration::from_secs(1)))
}

/// Lines per minute, and at once, for warnings about refused requests.
const LOG_PER_MINUTE: u32 = 30;
const LOG_BURST: u32 = 20;

/// A process-wide budget for the warnings written about refused requests
/// (foreign `Host`, any `Origin`, failed authentication).
///
/// Those refusals happen before a peer has proved anything, so their log
/// lines must cost an attacker something too: a peer that cannot get a single
/// request admitted must not be able to fill the journal. Past the budget a
/// refusal is still refused, and logged at DEBUG only; the next warning
/// written says how many were suppressed.
#[derive(Debug)]
pub struct LogBudget {
    per_second: f64,
    burst: f64,
    state: Mutex<LogState>,
}

#[derive(Debug)]
struct LogState {
    tokens: f64,
    refilled: Instant,
    suppressed: u64,
}

impl Default for LogBudget {
    fn default() -> Self {
        Self::new(LOG_PER_MINUTE, LOG_BURST)
    }
}

impl LogBudget {
    /// `per_minute` lines sustained, `burst` at once.
    #[must_use]
    pub fn new(per_minute: u32, burst: u32) -> Self {
        let burst = f64::from(burst.max(1));
        Self {
            per_second: f64::from(per_minute.max(1)) / 60.0,
            burst,
            state: Mutex::new(LogState {
                tokens: burst,
                refilled: Instant::now(),
                suppressed: 0,
            }),
        }
    }

    /// `Some(n)` when a warning may be written now, `n` being how many were
    /// suppressed since the last one; `None` when this one is suppressed.
    pub fn admit(&self, now: Instant) -> Option<u64> {
        let mut state = self.state.lock().ok()?;
        let elapsed = now.saturating_duration_since(state.refilled).as_secs_f64();
        state.tokens = (state.tokens + elapsed * self.per_second).min(self.burst);
        state.refilled = now;
        if state.tokens >= 1.0 {
            state.tokens -= 1.0;
            Some(std::mem::take(&mut state.suppressed))
        } else {
            state.suppressed += 1;
            None
        }
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

    fn addr(n: u32) -> IpAddr {
        IpAddr::V4(Ipv4Addr::from(0x0A00_0000 + n))
    }

    /// The review's interleaving (THROTTLE_TABLE_UNBOUNDED): every attacking
    /// address is admitted before any of its failures is recorded, so the
    /// lockouts land in slots the admissions already filled. The table must
    /// still never exceed its cap, keep every lockout it holds, and refuse a
    /// new address rather than grow.
    #[test]
    fn live_lockouts_never_push_the_table_past_its_cap() {
        let t = Throttle::new(6000, 100);
        let now = Instant::now();
        let attackers: Vec<IpAddr> = (0..TABLE_CAP as u32 + 64).map(addr).collect();
        for a in &attackers {
            let _ = t.check(*a, now);
            assert!(t.tracked() <= TABLE_CAP);
        }
        for a in &attackers {
            for _ in 0..MAX_AUTH_FAILURES {
                let _ = t.record_failure(*a, now);
                assert!(t.tracked() <= TABLE_CAP);
            }
        }
        assert_eq!(t.tracked(), TABLE_CAP);
        let locked = attackers
            .iter()
            .filter(|a| matches!(t.check(**a, now), Err(Throttled::LockedOut(_))))
            .count();
        assert_eq!(locked, TABLE_CAP, "every tracked lockout is still enforced");

        // A scan from fresh addresses: refused, and the table does not grow.
        for i in 0..4096 {
            let fresh = IpAddr::V4(Ipv4Addr::from(0xAC10_0000 + i));
            match t.check(fresh, now) {
                Err(Throttled::Saturated(wait)) => assert!(wait <= LOCKOUT),
                other => panic!("a new peer on a table full of lockouts got {other:?}"),
            }
            assert!(t.record_failure(fresh, now), "fails closed");
        }
        assert_eq!(t.tracked(), TABLE_CAP);

        // Once the lockouts expire, new peers are admitted again.
        assert!(t.check(ip(1), now + LOCKOUT).is_ok());
        assert!(t.tracked() <= TABLE_CAP);
    }

    /// The cap holds under real concurrency, admissions and failure updates
    /// interleaving freely on many threads.
    #[test]
    fn the_cap_holds_under_concurrent_admission_and_failures() {
        let t = std::sync::Arc::new(Throttle::new(6000, 100));
        let now = Instant::now();
        let threads: Vec<_> = (0..8u32)
            .map(|n| {
                let t = t.clone();
                std::thread::spawn(move || {
                    for i in 0..600u32 {
                        let a = addr(n * 10_000 + i);
                        let _ = t.check(a, now);
                        for _ in 0..MAX_AUTH_FAILURES {
                            let _ = t.record_failure(a, now);
                        }
                        assert!(t.tracked() <= TABLE_CAP, "{}", t.tracked());
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert!(t.tracked() <= TABLE_CAP, "{}", t.tracked());
    }

    /// WS_THROTTLE_FAIL_OPEN: a refused check spends nothing, and one refill
    /// admits exactly one caller however many ask for it.
    #[test]
    fn a_refill_admits_exactly_one_caller() {
        let t = Throttle::new(60, 1);
        let now = Instant::now();
        assert!(t.check(ip(1), now).is_ok());
        let Err(Throttled::Rate(wait)) = t.check(ip(1), now) else {
            panic!("the burst is spent");
        };
        let later = now + wait;
        let admitted = (0..4).filter(|_| t.check(ip(1), later).is_ok()).count();
        assert_eq!(admitted, 1);
    }

    #[test]
    fn the_log_budget_suppresses_a_flood_and_reports_it() {
        let budget = LogBudget::new(60, 3);
        let now = Instant::now();
        let written: Vec<_> = (0..100).filter_map(|_| budget.admit(now)).collect();
        assert_eq!(written, vec![0, 0, 0], "the burst, then silence");
        // One line per second refills; the next line counts what was dropped.
        assert_eq!(budget.admit(now + Duration::from_secs(1)), Some(97));
        assert_eq!(budget.admit(now + Duration::from_secs(1)), None);
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
