//! Session identity, ownership, and the one place a state transition happens.
//!
//! # Why ownership is enforced here and not in the protocol
//!
//! `Stop` and `Cancel` deliberately carry no session id (S01 item 3, accepted
//! by the Epic-lead). That keeps the wire simple — there is only ever one
//! dictation session — but it moves the entire authorization question into the
//! daemon: *whose* session is "the" session?
//!
//! Today there is one local client and the answer looks obvious. It stops
//! being obvious the moment S33 exposes this over the LAN, where a phone must
//! be able to transcribe its own audio and must **not** be able to cancel the
//! dictation Jake is in the middle of at his desk. Getting the rule right now
//! costs nothing; retrofitting it after a phone client exists is a security
//! change under time pressure.
//!
//! The rule is capability-derived rather than transport-derived, so S33
//! inherits it for free:
//!
//! | Actor | May control a session owned by… |
//! |---|---|
//! | [`Actor::Signal`] (SIGUSR1/2, hotkey) | anything — it is the host's physical control |
//! | connection with `host_capture` | itself, or an unowned host-local session |
//! | connection without `host_capture` | only itself |
//!
//! `host_capture` is the right discriminator because it is exactly the
//! permission to switch on *this host's* microphone. A peer that may do that is
//! by definition trusted with this host's dictation; a peer that may not (every
//! remote client, per `Capabilities::remote_transcription_only`) has no
//! business stopping it.
//!
//! # Why a disconnect orphans rather than cancels
//!
//! `dictate toggle` is a keybinding. It connects, sends one command, and exits
//! — so the connection that *starts* a session is almost always gone before
//! the session ends, and the connection that stops it is a different process
//! entirely. If a disconnect cancelled the session, the CLI could never work.
//!
//! So a host-local session **survives** its owner disconnecting and becomes
//! [`SessionOwner::Host`], controllable by any trusted-local actor. A session
//! owned by a connection that lacks `host_capture` has no such fallback — no
//! one else can take delivery of its result — so it is cancelled. That branch
//! is unreachable today (starting a session *requires* `host_capture`) and
//! exists for S33's upload sessions.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use dictate_proto::{DictationMode, Event, Features, SessionId, State};

use crate::cancel::CancelToken;
use crate::event_bus::EventBus;

/// Identifies one control-plane connection for the lifetime of that connection.
///
/// Monotonic and never reused, so a reconnecting client is a *different* owner
/// and cannot inherit the previous connection's session by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClientId(pub u64);

impl std::fmt::Display for ClientId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "client-{}", self.0)
    }
}

/// Hands out process-unique [`ClientId`]s.
#[derive(Debug, Default)]
pub struct ClientIdGen(AtomicU64);

impl ClientIdGen {
    /// The next never-before-used id.
    pub fn next(&self) -> ClientId {
        ClientId(self.0.fetch_add(1, Ordering::Relaxed))
    }
}

/// Who is asking the engine to do something.
#[derive(Debug, Clone)]
pub enum Actor {
    /// A control-plane connection, carrying the capabilities negotiated for it.
    Connection {
        /// Which connection.
        id: ClientId,
        /// Whether that connection may drive this host's microphone. This is
        /// the trust discriminator described in the module docs.
        host_capture: bool,
    },
    /// A POSIX signal (SIGUSR1/SIGUSR2) — the host's physical control surface.
    Signal,
}

impl Actor {
    /// Build an actor from a connection's negotiated capabilities.
    #[must_use]
    pub fn connection(id: ClientId, features: &Features) -> Self {
        Self::Connection {
            id,
            host_capture: features.host_capture,
        }
    }

    /// Whether this actor is trusted with this host's dictation.
    #[must_use]
    pub fn is_host_trusted(&self) -> bool {
        match self {
            Self::Signal => true,
            Self::Connection { host_capture, .. } => *host_capture,
        }
    }

    /// The owner recorded for a session this actor starts.
    #[must_use]
    pub fn as_owner(&self) -> SessionOwner {
        match self {
            Self::Signal => SessionOwner::Host,
            Self::Connection { id, .. } => SessionOwner::Connection(*id),
        }
    }
}

/// Who is entitled to stop or cancel a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionOwner {
    /// Started by, and belonging to, one connection.
    Connection(ClientId),
    /// Belongs to the host rather than to any connection — started by a signal,
    /// or orphaned when its owning connection went away.
    Host,
}

impl SessionOwner {
    /// Whether `actor` may stop or cancel a session with this owner.
    ///
    /// See the table in the module docs. Deny-by-default in shape: every arm
    /// that returns `true` names a specific reason to allow.
    #[must_use]
    pub fn may_control(&self, actor: &Actor) -> bool {
        match (self, actor) {
            // The physical hotkey outranks everything on this host.
            (_, Actor::Signal) => true,
            // Your own session is always yours.
            (Self::Connection(owner), Actor::Connection { id, .. }) => owner == id,
            // An unowned host session may be driven by any trusted-local peer
            // — this is what makes two separate `dictate` invocations work.
            (Self::Host, Actor::Connection { host_capture, .. }) => *host_capture,
        }
    }

    /// What should happen to this session when `client` disconnects.
    #[must_use]
    pub fn on_disconnect(&self, client: ClientId, host_capture: bool) -> DisconnectAction {
        match self {
            Self::Connection(owner) if *owner == client => {
                if host_capture {
                    DisconnectAction::Orphan
                } else {
                    DisconnectAction::Cancel
                }
            }
            _ => DisconnectAction::Ignore,
        }
    }
}

/// The engine's response to an owning connection going away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisconnectAction {
    /// Not this connection's session; leave it alone.
    Ignore,
    /// Hand the session to the host so another trusted-local client can finish
    /// it. The pipeline keeps running.
    Orphan,
    /// Nobody can take delivery; stop the session.
    Cancel,
}

/// Generate an opaque, collision-resistant session id.
#[must_use]
pub fn new_session_id() -> SessionId {
    SessionId(uuid::Uuid::new_v4().to_string()[..12].to_string())
}

/// Milliseconds since the Unix epoch, the protocol's timestamp unit.
#[must_use]
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// A live session's shared state: the handle the pipeline task drives and the
/// engine observes.
///
/// Every state change in the daemon goes through [`SessionHandle::advance`].
/// There is no other writer, so the protocol's transition rules are enforced
/// in exactly one place and the `StateChanged` event can never disagree with
/// the state a `GetStatus` would report.
#[derive(Debug, Clone)]
pub struct SessionHandle {
    id: SessionId,
    mode: DictationMode,
    started_ms: u64,
    state: Arc<Mutex<State>>,
    token: CancelToken,
    bus: EventBus,
}

impl SessionHandle {
    /// Create a handle in [`State::Idle`], ready to advance to `Recording`.
    #[must_use]
    pub fn new(id: SessionId, mode: DictationMode, token: CancelToken, bus: EventBus) -> Self {
        Self {
            id,
            mode,
            started_ms: now_ms(),
            state: Arc::new(Mutex::new(State::Idle)),
            token,
            bus,
        }
    }

    /// This session's id.
    #[must_use]
    pub fn id(&self) -> &SessionId {
        &self.id
    }

    /// The mode it was started in.
    #[must_use]
    pub fn mode(&self) -> DictationMode {
        self.mode.clone()
    }

    /// When it started, in epoch milliseconds.
    #[must_use]
    pub fn started_ms(&self) -> u64 {
        self.started_ms
    }

    /// Its cancellation token.
    #[must_use]
    pub fn token(&self) -> &CancelToken {
        &self.token
    }

    /// The current state.
    #[must_use]
    pub fn state(&self) -> State {
        self.state.lock().expect("session state mutex poisoned").clone()
    }

    /// Whether the session has reached a terminal state.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.state().is_terminal()
    }

    /// Move to `next`, emitting the `StateChanged` event that proves it.
    ///
    /// Rejects a transition the protocol forbids (backwards, or out of a
    /// terminal state) rather than emitting an event that would teach clients
    /// a state machine we do not actually implement. A rejected transition is
    /// a daemon bug, so it is logged loudly and the state is left untouched.
    pub fn advance(&self, next: State) -> Result<(), InvalidTransition> {
        let mut guard = self.state.lock().expect("session state mutex poisoned");
        let from = guard.clone();
        if from == next {
            return Ok(());
        }
        if !from.can_transition_to(&next) {
            tracing::error!(
                session = %self.id.as_str(),
                from = %from.as_str(),
                to = %next.as_str(),
                "rejected an illegal state transition"
            );
            return Err(InvalidTransition {
                from,
                to: next,
            });
        }
        *guard = next.clone();
        drop(guard);

        self.bus.publish(Event::StateChanged {
            session_id: self.id.clone(),
            from,
            to: next,
            at_ms: Some(now_ms()),
        });
        Ok(())
    }

    /// Publish a session event without changing state.
    ///
    /// Used for the payload events — `final`, `error`, `audio_level` — that
    /// accompany a session but are not transitions of it.
    pub fn publish(&self, event: Event) {
        self.bus.publish(event);
    }

    /// A stage boundary: advance to `next`, or stop because the session was
    /// cancelled.
    ///
    /// This is the pipeline's normal way to move forward — it makes the
    /// cancellation check impossible to forget, because you cannot enter the
    /// next stage without passing through it.
    ///
    /// # Errors
    ///
    /// [`crate::cancel::Cancelled`] when cancellation has already won.
    pub fn advance_checked(&self, next: State) -> Result<(), crate::cancel::Cancelled> {
        self.token.checkpoint()?;
        // A rejected transition must not be reported as a cancellation; log
        // and continue, since the state machine is already the authority.
        let _ = self.advance(next);
        self.token.checkpoint()
    }
}

/// A transition the protocol's state machine forbids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidTransition {
    /// The state the session was in.
    pub from: State,
    /// The state that was requested.
    pub to: State,
}

impl std::fmt::Display for InvalidTransition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "illegal state transition {} -> {}",
            self.from.as_str(),
            self.to.as_str()
        )
    }
}

impl std::error::Error for InvalidTransition {}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(id: u64, host_capture: bool) -> Actor {
        Actor::Connection {
            id: ClientId(id),
            host_capture,
        }
    }

    #[test]
    fn a_client_may_control_its_own_session() {
        let owner = SessionOwner::Connection(ClientId(1));
        assert!(owner.may_control(&conn(1, true)));
    }

    #[test]
    fn a_second_client_may_not_control_the_first_clients_session() {
        let owner = SessionOwner::Connection(ClientId(1));
        assert!(
            !owner.may_control(&conn(2, true)),
            "trusted-local is not enough to hijack a session another connection owns"
        );
        assert!(!owner.may_control(&conn(2, false)));
    }

    #[test]
    fn a_remote_client_may_not_control_a_host_session() {
        // The S33 property: a phone on the LAN cannot cancel the desktop's
        // in-flight dictation.
        assert!(!SessionOwner::Host.may_control(&conn(9, false)));
    }

    #[test]
    fn a_trusted_local_client_may_control_a_host_session() {
        // The CLI property: `dictate toggle` twice is two connections.
        assert!(SessionOwner::Host.may_control(&conn(9, true)));
    }

    #[test]
    fn signals_may_control_anything() {
        assert!(SessionOwner::Host.may_control(&Actor::Signal));
        assert!(SessionOwner::Connection(ClientId(7)).may_control(&Actor::Signal));
    }

    #[test]
    fn a_signal_started_session_is_host_owned() {
        assert_eq!(Actor::Signal.as_owner(), SessionOwner::Host);
        assert_eq!(
            conn(3, true).as_owner(),
            SessionOwner::Connection(ClientId(3))
        );
    }

    #[test]
    fn disconnect_orphans_a_trusted_local_session() {
        let owner = SessionOwner::Connection(ClientId(1));
        assert_eq!(
            owner.on_disconnect(ClientId(1), true),
            DisconnectAction::Orphan
        );
    }

    #[test]
    fn disconnect_cancels_an_untrusted_session() {
        let owner = SessionOwner::Connection(ClientId(1));
        assert_eq!(
            owner.on_disconnect(ClientId(1), false),
            DisconnectAction::Cancel
        );
    }

    #[test]
    fn an_unrelated_disconnect_is_ignored() {
        let owner = SessionOwner::Connection(ClientId(1));
        assert_eq!(
            owner.on_disconnect(ClientId(2), true),
            DisconnectAction::Ignore
        );
        assert_eq!(
            SessionOwner::Host.on_disconnect(ClientId(2), true),
            DisconnectAction::Ignore
        );
    }

    #[test]
    fn client_ids_are_never_reused() {
        let gen = ClientIdGen::default();
        let a = gen.next();
        let b = gen.next();
        assert_ne!(a, b);
    }

    #[test]
    fn advance_emits_and_records() {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();
        let h = SessionHandle::new(
            new_session_id(),
            DictationMode::Toggle,
            CancelToken::new(),
            bus,
        );
        h.advance(State::Recording).unwrap();
        assert_eq!(h.state(), State::Recording);
        match rx.try_recv().expect("a state_changed event") {
            Event::StateChanged { from, to, .. } => {
                assert_eq!(from, State::Idle);
                assert_eq!(to, State::Recording);
            }
            other => panic!("expected state_changed, got {other:?}"),
        }
    }

    #[test]
    fn advance_rejects_a_backwards_transition() {
        let bus = EventBus::new(16);
        let h = SessionHandle::new(
            new_session_id(),
            DictationMode::Toggle,
            CancelToken::new(),
            bus,
        );
        h.advance(State::Transcribing).unwrap();
        let err = h.advance(State::Recording).unwrap_err();
        assert_eq!(err.from, State::Transcribing);
        assert_eq!(
            h.state(),
            State::Transcribing,
            "a rejected transition must not mutate state"
        );
    }

    #[test]
    fn every_non_terminal_state_can_reach_cancelled() {
        // The protocol's rule 1, asserted against our own state holder rather
        // than only against the proto crate's unit tests.
        for state in [
            State::Idle,
            State::Recording,
            State::Transcribing,
            State::Formatting,
            State::Injecting,
        ] {
            let bus = EventBus::new(16);
            let h = SessionHandle::new(
                new_session_id(),
                DictationMode::Toggle,
                CancelToken::new(),
                bus,
            );
            // Walk to the state under test.
            if state != State::Idle {
                h.advance(state.clone()).unwrap();
            }
            assert!(
                h.advance(State::Cancelled).is_ok(),
                "cancellation must be reachable from {}",
                state.as_str()
            );
        }
    }

    #[test]
    fn advance_checked_stops_on_cancel() {
        let bus = EventBus::new(16);
        let token = CancelToken::new();
        let h = SessionHandle::new(
            new_session_id(),
            DictationMode::Toggle,
            token.clone(),
            bus,
        );
        h.advance_checked(State::Recording).unwrap();
        token.cancel();
        assert!(h.advance_checked(State::Transcribing).is_err());
        assert_eq!(
            h.state(),
            State::Recording,
            "a cancelled pipeline must not advance past its checkpoint"
        );
    }
}
