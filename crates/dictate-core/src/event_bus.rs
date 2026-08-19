//! Fan-out of [`Event`]s to every subscribed connection.
//!
//! One dictation session is watched by several clients at once — a `dictate
//! tail` in a terminal, the tray icon, and later the Tauri HUD — so events are
//! broadcast rather than addressed. A late subscriber does not get history:
//! events describe transitions, and replaying a transition that already
//! happened would make a HUD render a session that has long since finished.
//!
//! # Dropping is a feature, not a failure
//!
//! `audio_level` fires ~30×/second and is a level meter. `state_changed` fires
//! a handful of times per session and is the state machine. Both go down the
//! same channel, so a slow consumer will eventually lag.
//!
//! [`tokio::sync::broadcast`] handles this by dropping the *oldest* messages
//! and telling the receiver how many it missed, which is the right trade for
//! the level meter and the wrong one for the state machine. The mitigation is
//! capacity plus honesty: the buffer is sized for a burst of level events, and
//! a lagging subscriber is logged at `warn`. Clients are required by the
//! protocol to tolerate dropped `audio_level` events, and a client that needs
//! ground truth can always ask for it with `get_status`.

use dictate_proto::Event;
use tokio::sync::broadcast;

/// Default channel depth: roughly two seconds of throttled `audio_level`
/// events plus the state changes interleaved with them, so a subscriber that
/// stalls briefly (a terminal being resized, a UI repaint) does not lag.
pub const DEFAULT_CAPACITY: usize = 256;

/// A clonable publish handle.
#[derive(Debug, Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl EventBus {
    /// Create a bus with the given channel depth.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Publish an event to all current subscribers.
    ///
    /// Having no subscribers is normal (a headless daemon nobody is watching),
    /// so the "no receivers" error is deliberately discarded rather than
    /// logged — it would fire on every event of every session.
    pub fn publish(&self, event: Event) {
        let _ = self.tx.send(event);
    }

    /// Subscribe. The receiver sees events published from this point on.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    /// How many subscribers are currently attached.
    #[must_use]
    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dictate_proto::{SessionId, State};

    fn state_changed(to: State) -> Event {
        Event::StateChanged {
            session_id: SessionId("s1".into()),
            from: State::Idle,
            to,
            at_ms: None,
        }
    }

    #[test]
    fn publishing_without_subscribers_is_not_an_error() {
        let bus = EventBus::new(4);
        bus.publish(state_changed(State::Recording));
        assert_eq!(bus.subscriber_count(), 0);
    }

    #[tokio::test]
    async fn every_subscriber_sees_every_event() {
        let bus = EventBus::new(8);
        let mut a = bus.subscribe();
        let mut b = bus.subscribe();
        assert_eq!(bus.subscriber_count(), 2);

        bus.publish(state_changed(State::Recording));

        for rx in [&mut a, &mut b] {
            match rx.recv().await.unwrap() {
                Event::StateChanged { to, .. } => assert_eq!(to, State::Recording),
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn a_late_subscriber_does_not_replay_history() {
        let bus = EventBus::new(8);
        bus.publish(state_changed(State::Recording));
        let mut late = bus.subscribe();
        assert!(
            late.try_recv().is_err(),
            "a subscriber must not receive events published before it attached"
        );
        bus.publish(state_changed(State::Transcribing));
        assert!(late.recv().await.is_ok());
    }

    #[tokio::test]
    async fn a_lagging_subscriber_reports_lag_rather_than_corrupting() {
        let bus = EventBus::new(2);
        let mut rx = bus.subscribe();
        for _ in 0..8 {
            bus.publish(state_changed(State::Recording));
        }
        assert!(
            matches!(rx.recv().await, Err(broadcast::error::RecvError::Lagged(_))),
            "overflow must surface as Lagged, not as silently reordered events"
        );
        // Still usable afterwards — this is why a lag is logged and tolerated
        // rather than treated as a reason to drop the client. The receiver
        // resumes at the oldest message still buffered.
        assert!(
            rx.recv().await.is_ok(),
            "a lagged subscriber must recover, not stay broken"
        );
    }
}
