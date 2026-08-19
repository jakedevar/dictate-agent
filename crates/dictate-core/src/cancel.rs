//! Cancellation with an explicit **commit point**.
//!
//! A dictation session must be cancellable from every non-terminal state, but
//! one stage of the pipeline is physically irreversible: once text has been
//! handed to the injector it is being typed into somebody's editor and no
//! amount of protocol politeness will un-type it. A plain "cancelled?" flag
//! checked at stage boundaries has a race on exactly that stage — cancel and
//! inject can both win, and the user is left with half a sentence pasted into
//! their terminal *and* a `cancelled` event claiming it never happened.
//!
//! [`CancelToken`] closes that race by making the irreversible stage take the
//! token out of the cancellable set *atomically, before it starts*:
//!
//! ```text
//!            cancel()                 cancel()  -> AlreadyCancelled
//!      Live ──────────► Cancelled ───────────────────────────────►
//!        │
//!        │ enter_commit()  (Some(guard))
//!        ▼
//!   Committed ───────────────────────────────────────────────────►
//!            cancel()  -> TooLate
//! ```
//!
//! Both transitions out of `Live` are a single compare-exchange, so exactly one
//! of them wins. The caller therefore gets a total, honest answer:
//!
//! - [`CancelVerdict::Accepted`] — nothing was injected, and nothing will be.
//! - [`CancelVerdict::TooLate`] — injection had already begun; the session runs
//!   to `done`. Reported to the peer as [`ErrorCode::Conflict`], never as a
//!   successful cancel.
//!
//! [`ErrorCode::Conflict`]: dictate_proto::ErrorCode::Conflict
//!
//! The `TooLate` window is only as wide as the injector call itself (~50–100ms
//! for a clipboard paste), and it is the *only* window in the pipeline where a
//! cancel can be refused. Every other stage — capture, STT, formatting, route
//! dispatch — is interrupted at its next checkpoint.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use tokio::sync::Notify;

const LIVE: u8 = 0;
const CANCELLED: u8 = 1;
const COMMITTED: u8 = 2;

/// What happened to a cancellation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelVerdict {
    /// The token moved `Live -> Cancelled`. The pipeline will stop at its next
    /// checkpoint and nothing irreversible has run.
    Accepted,
    /// The token was already cancelled by an earlier request. Idempotent, and
    /// still a success from the caller's point of view.
    AlreadyCancelled,
    /// The pipeline had already entered its irreversible stage. The session
    /// will complete; the caller must not be told the cancel succeeded.
    TooLate,
}

impl CancelVerdict {
    /// Whether the session is going to stop as a result of this request.
    ///
    /// `AlreadyCancelled` counts: the caller's intent ("this session must not
    /// complete") holds either way, so a duplicate cancel is not an error.
    #[must_use]
    pub fn is_cancelled(self) -> bool {
        matches!(self, Self::Accepted | Self::AlreadyCancelled)
    }
}

/// The error returned by a checkpoint that observed a cancellation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("session cancelled")
    }
}

impl std::error::Error for Cancelled {}

#[derive(Debug)]
struct Inner {
    state: AtomicU8,
    notify: Notify,
}

/// A cheap, clonable cancellation handle shared by the engine and the pipeline
/// task running one session.
#[derive(Debug, Clone)]
pub struct CancelToken {
    inner: Arc<Inner>,
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelToken {
    /// A fresh, live token.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                state: AtomicU8::new(LIVE),
                notify: Notify::new(),
            }),
        }
    }

    /// Request cancellation.
    ///
    /// Safe to call from any task and any number of times; the verdict tells
    /// the caller which of the three things actually happened.
    pub fn cancel(&self) -> CancelVerdict {
        match self.inner.state.compare_exchange(
            LIVE,
            CANCELLED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                // `notify_waiters` only wakes tasks already parked, so the
                // state store above must be visible first — it is, by AcqRel.
                self.inner.notify.notify_waiters();
                CancelVerdict::Accepted
            }
            Err(CANCELLED) => CancelVerdict::AlreadyCancelled,
            Err(_) => CancelVerdict::TooLate,
        }
    }

    /// Whether cancellation has been requested and won.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.inner.state.load(Ordering::Acquire) == CANCELLED
    }

    /// Whether the irreversible stage has been entered.
    #[must_use]
    pub fn is_committed(&self) -> bool {
        self.inner.state.load(Ordering::Acquire) == COMMITTED
    }

    /// A stage boundary: `Err(Cancelled)` if the session must stop here.
    ///
    /// # Errors
    ///
    /// Returns [`Cancelled`] when [`cancel`](Self::cancel) has already won.
    pub fn checkpoint(&self) -> Result<(), Cancelled> {
        if self.is_cancelled() {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }

    /// Take the session out of the cancellable set for an irreversible stage.
    ///
    /// Returns `None` when the session was already cancelled — the caller must
    /// then *not* perform the irreversible work. A returned guard is proof
    /// that no concurrent `cancel()` can succeed for the rest of the session.
    #[must_use]
    pub fn enter_commit(&self) -> Option<CommitGuard> {
        match self.inner.state.compare_exchange(
            LIVE,
            COMMITTED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Some(CommitGuard { _private: () }),
            Err(_) => None,
        }
    }

    /// Resolve when cancellation is requested.
    ///
    /// Used with `tokio::select!` to interrupt a stage that is `await`ing
    /// rather than polling checkpoints — STT on a long utterance, say.
    ///
    /// # The lost-wakeup this is written around
    ///
    /// `notify_waiters` only wakes tasks *already registered*. A `Notified`
    /// future does not register when it is created — it registers on its first
    /// poll — so the obvious spelling
    ///
    /// ```text
    /// let waiter = notify.notified();     // not registered yet!
    /// if cancelled { return }             // <-- cancel() can land here
    /// waiter.await;                       // ...and this parks forever
    /// ```
    ///
    /// has a window where a cancellation is dropped on the floor and the
    /// session hangs until the daemon restarts. `enable()` performs the
    /// registration up front, which closes it: any `cancel()` from that point
    /// on is guaranteed to wake this future.
    pub async fn cancelled(&self) {
        loop {
            let mut waiter = std::pin::pin!(self.inner.notify.notified());
            // Register *before* the check, not merely before the await.
            waiter.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            waiter.await;
            if self.is_cancelled() {
                return;
            }
        }
    }
}

/// Proof that the irreversible stage is running and cancellation can no longer
/// win. Held for the duration of the injector call.
///
/// Only constructible by [`CancelToken::enter_commit`], so a caller cannot
/// reach the injector without having first taken the session out of the
/// cancellable set. Dropping it does **not** reopen that window — the token
/// stays `Committed` — because the injection it guarded has already happened.
#[derive(Debug)]
#[must_use = "holding the guard is what marks the irreversible region"]
pub struct CommitGuard {
    _private: (),
}

impl Drop for CommitGuard {
    fn drop(&mut self) {
        // The commit region is the only place a cancel can be refused, so
        // knowing exactly how long it lasted is worth a trace line when
        // diagnosing a `conflict` a user did not expect.
        tracing::trace!("injection commit region ended");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_is_accepted_once_then_idempotent() {
        let t = CancelToken::new();
        assert!(!t.is_cancelled());
        assert_eq!(t.cancel(), CancelVerdict::Accepted);
        assert!(t.is_cancelled());
        assert_eq!(t.cancel(), CancelVerdict::AlreadyCancelled);
        assert!(t.cancel().is_cancelled());
    }

    #[test]
    fn checkpoint_fails_after_cancel() {
        let t = CancelToken::new();
        assert!(t.checkpoint().is_ok());
        t.cancel();
        assert_eq!(t.checkpoint(), Err(Cancelled));
    }

    #[test]
    fn commit_blocks_later_cancel() {
        let t = CancelToken::new();
        let guard = t.enter_commit().expect("live token must commit");
        assert!(t.is_committed());
        assert_eq!(t.cancel(), CancelVerdict::TooLate);
        assert!(!t.cancel().is_cancelled(), "TooLate must not read as cancelled");
        drop(guard);
        // Committed is terminal for the token: dropping the guard does not
        // re-open the cancellation window on an injection that already ran.
        assert_eq!(t.cancel(), CancelVerdict::TooLate);
    }

    #[test]
    fn cancel_blocks_later_commit() {
        let t = CancelToken::new();
        assert_eq!(t.cancel(), CancelVerdict::Accepted);
        assert!(
            t.enter_commit().is_none(),
            "a cancelled session must never reach the injector"
        );
    }

    #[test]
    fn commit_is_exclusive() {
        let t = CancelToken::new();
        assert!(t.enter_commit().is_some());
        assert!(t.enter_commit().is_none(), "commit must be entered at most once");
    }

    #[tokio::test]
    async fn cancelled_resolves_when_already_cancelled() {
        let t = CancelToken::new();
        t.cancel();
        // Must not hang: the pre-check runs before parking.
        tokio::time::timeout(std::time::Duration::from_secs(1), t.cancelled())
            .await
            .expect("cancelled() must resolve for an already-cancelled token");
    }

    #[tokio::test]
    async fn cancelled_wakes_a_parked_waiter() {
        let t = CancelToken::new();
        let waiter = t.clone();
        let h = tokio::spawn(async move { waiter.cancelled().await });
        tokio::task::yield_now().await;
        t.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(1), h)
            .await
            .expect("waiter must wake")
            .expect("waiter task must not panic");
    }

    /// The property the whole type exists for: under contention, `cancel` and
    /// `enter_commit` can never both succeed.
    #[test]
    fn cancel_and_commit_are_mutually_exclusive_under_contention() {
        for _ in 0..2_000 {
            let t = CancelToken::new();
            let a = t.clone();
            let b = t.clone();
            let h1 = std::thread::spawn(move || a.cancel());
            let h2 = std::thread::spawn(move || b.enter_commit().is_some());
            let verdict = h1.join().unwrap();
            let committed = h2.join().unwrap();
            assert!(
                !(verdict == CancelVerdict::Accepted && committed),
                "cancel accepted AND injection committed — the half-injected-result bug"
            );
            assert!(
                verdict == CancelVerdict::Accepted || committed,
                "one of the two must win; neither did"
            );
        }
    }
}
