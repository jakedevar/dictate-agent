//! Session lifecycle: the state machine, routes, and injection outcomes.

use serde::{Deserialize, Serialize};

use crate::error::ProtoError;
use crate::timings::SkipReason;

/// An opaque session identifier, minted by the daemon.
///
/// Deliberately a string rather than a `uuid::Uuid`: no consumer of this crate
/// — least of all a phone client — should be forced to adopt our UUID library
/// to read a correlation key it only ever echoes back.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub String);

impl SessionId {
    /// Borrow the identifier as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for SessionId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for SessionId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl core::fmt::Display for SessionId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

open_str_enum! {
    /// The dictation session state machine, shared verbatim with the daemon
    /// (S02).
    ///
    /// The happy path is a strictly forward chain:
    ///
    /// ```text
    /// Idle -> Recording -> Transcribing -> Formatting -> Injecting -> Done
    ///           |              |              |             |
    ///           +--------------+--------------+-------------+---> Error
    ///           |              |              |             |
    ///           +--------------+--------------+-------------+---> Cancelled
    /// ```
    ///
    /// Stages may be **skipped forward** — VAD can be disabled, the LLM pass is
    /// skipped by the skip-rules, and a remote client's session ends at
    /// `Formatting` because it takes delivery of text instead of having it
    /// injected. Moving *backward* is not legal within a session.
    ///
    /// `Error` and `Cancelled` are reachable from every non-terminal state; see
    /// [`State::can_transition_to`].
    pub enum State {
        /// No session in flight; the daemon is waiting for a trigger.
        Idle => "idle",
        /// Capturing audio.
        Recording => "recording",
        /// Running speech-to-text over the captured audio.
        Transcribing => "transcribing",
        /// Applying the rules layer and (optionally) the LLM formatting pass.
        Formatting => "formatting",
        /// Delivering text to the target surface.
        Injecting => "injecting",
        /// Terminal: the session completed successfully.
        Done => "done",
        /// Terminal: the session failed. The accompanying
        /// [`Event::Error`](crate::Event::Error) carries the reason.
        Error => "error",
        /// Terminal: the session was cancelled by the user or a client.
        Cancelled => "cancelled",
    }
    default = Idle;
}

impl State {
    /// Position in the forward pipeline, or `None` for terminal/unknown states.
    fn stage_index(&self) -> Option<u8> {
        match self {
            Self::Idle => Some(0),
            Self::Recording => Some(1),
            Self::Transcribing => Some(2),
            Self::Formatting => Some(3),
            Self::Injecting => Some(4),
            Self::Done | Self::Error | Self::Cancelled | Self::Unknown(_) => None,
        }
    }

    /// Whether this state ends the session.
    ///
    /// An unknown state is *not* treated as terminal: a newer daemon may have
    /// introduced an intermediate stage, and assuming it is terminal would make
    /// an older client hang up on a live session.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Done | Self::Error | Self::Cancelled)
    }

    /// Whether a session is in flight and therefore cancellable.
    #[must_use]
    pub fn is_active(&self) -> bool {
        !self.is_terminal() && !matches!(self, Self::Idle)
    }

    /// Whether `self -> next` is a legal transition.
    ///
    /// Rules, in order:
    /// 1. Any non-terminal state may move to any terminal state — this is what
    ///    makes cancellation reachable from everywhere.
    /// 2. A terminal state may only reset to [`State::Idle`].
    /// 3. Otherwise the move must go strictly forward in the pipeline, which
    ///    permits skipping a stage but never revisiting one.
    /// 4. Transitions involving an [`State::Unknown`] state are permitted, so
    ///    an older client does not reject a newer daemon's pipeline as
    ///    malformed.
    #[must_use]
    pub fn can_transition_to(&self, next: &State) -> bool {
        if matches!(self, Self::Unknown(_)) || matches!(next, Self::Unknown(_)) {
            return true;
        }
        if self.is_terminal() {
            return matches!(next, Self::Idle);
        }
        if next.is_terminal() {
            return true;
        }
        match (self.stage_index(), next.stage_index()) {
            (Some(a), Some(b)) => b > a,
            _ => false,
        }
    }
}

open_str_enum! {
    /// Where a finished transcript is dispatched.
    ///
    /// `Timer` (systemd-run) and `Local` (Ollama) are load-bearing features of
    /// the existing daemon and are explicitly preserved through the rebuild —
    /// they are not legacy behavior awaiting removal.
    pub enum Route {
        /// Inject the formatted text into the focused application.
        Type => "type",
        /// Set a system timer via `systemd-run`.
        Timer => "timer",
        /// Answer the utterance with a local LLM instead of typing it.
        Local => "local",
        /// Rewrite the current selection.
        Edit => "edit",
        /// Interpret the utterance as a command rather than as prose.
        Command => "command",
    }
    default = Type;
}

open_str_enum! {
    /// How a dictation session was triggered, which determines how it ends.
    pub enum DictationMode {
        /// Start now; a subsequent [`Command::Stop`](crate::Command::Stop)
        /// ends the session.
        Toggle => "toggle",
        /// Record while a key is physically held; release ends the session.
        PushToTalk => "push_to_talk",
        /// Record until VAD detects end-of-speech, then stop automatically.
        OneShot => "one_shot",
        /// Started hands-free by the wake word.
        WakeWord => "wake_word",
    }
    default = Toggle;
}

open_str_enum! {
    /// The mechanism used to place text into the target application.
    pub enum InjectMethod {
        /// Write to the clipboard and synthesize a paste, restoring the prior
        /// clipboard contents afterwards. The default.
        Paste => "paste",
        /// Synthesize individual keystrokes, for paste-hostile applications.
        Keystroke => "keystroke",
        /// Handed to the compositor's input-method protocol.
        InputMethod => "input_method",
    }
}

/// The outcome of the injection stage.
///
/// Deliberately **not** a boolean. Research spike R3 found that on GNOME/KDE
/// Wayland, synthetic input goes through an asynchronous, user-consent-gated
/// portal, so "succeeded or failed" is not a complete outcome space — a request
/// can sit pending on a dialog the user has not answered yet. X11 ships first
/// and never produces [`InjectionOutcome::AwaitingConsent`], so modeling it
/// today costs one enum and saves a breaking version bump later.
///
/// [`InjectionOutcome::Delivered`] covers the other non-binary case: a remote
/// client asked for a transcript and received one, and nothing was injected
/// anywhere. That is a success, not a skip and not a failure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum InjectionOutcome {
    /// Text was placed into the focused application.
    Injected {
        /// The mechanism that was used.
        method: InjectMethod,
        /// Number of characters injected.
        chars: u32,
    },

    /// The backend has requested user consent and is waiting for an answer.
    ///
    /// **This is not terminal.** The session's
    /// [`Event::Final`](crate::Event::Final) may carry this outcome, with the
    /// resolution arriving later as
    /// [`Event::InjectionResolved`](crate::Event::InjectionResolved) carrying
    /// the same `session_id`. A client must not report success or failure while
    /// this outcome stands.
    AwaitingConsent {
        /// The injection backend awaiting consent, e.g. `"portal"`, `"libei"`.
        backend: String,
        /// Backend-specific handle for the pending consent request, when the
        /// backend exposes one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        consent_id: Option<String>,
    },

    /// The user was asked for consent and refused.
    ConsentDenied {
        /// The injection backend that requested consent.
        backend: String,
        /// Human-readable detail, when the backend supplies one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },

    /// No injection backend is available on this platform or connection.
    ///
    /// Distinct from a failure: nothing went wrong, the capability simply does
    /// not exist here — a headless daemon, an unsupported compositor, or a LAN
    /// connection whose [`Features::text_injection`](crate::Features::text_injection)
    /// is `false`.
    Unavailable {
        /// The backend that was attempted, or `"none"` if none is configured.
        backend: String,
        /// Why it is unavailable.
        reason: String,
    },

    /// The transcript was returned to the caller instead of being injected.
    ///
    /// The normal terminal outcome for `POST /v1/transcribe` and for any client
    /// that asked for text rather than for an effect.
    Delivered,

    /// Injection was deliberately not attempted.
    Skipped {
        /// Why the stage was skipped.
        reason: SkipReason,
    },

    /// Injection was attempted and failed.
    Failed {
        /// The failure.
        error: ProtoError,
    },

    /// An outcome introduced by a newer peer. See the crate-level
    /// compatibility rule.
    #[serde(other)]
    Unknown,
}

impl InjectionOutcome {
    /// Whether this outcome is settled.
    ///
    /// [`InjectionOutcome::AwaitingConsent`] is the only unsettled outcome; a
    /// client must wait for
    /// [`Event::InjectionResolved`](crate::Event::InjectionResolved) before
    /// reporting a result to the user.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        !matches!(self, Self::AwaitingConsent { .. })
    }

    /// Whether text actually reached the user's target application.
    ///
    /// [`InjectionOutcome::Delivered`] is **not** counted: the caller received
    /// text, but nothing was injected anywhere.
    #[must_use]
    pub fn did_inject(&self) -> bool {
        matches!(self, Self::Injected { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn non_terminal() -> Vec<State> {
        vec![
            State::Idle,
            State::Recording,
            State::Transcribing,
            State::Formatting,
            State::Injecting,
        ]
    }

    /// Constraint: cancellation must be reachable from every non-terminal state.
    #[test]
    fn cancel_reachable_from_every_non_terminal_state() {
        for s in non_terminal() {
            assert!(
                s.can_transition_to(&State::Cancelled),
                "{s} must be cancellable"
            );
            assert!(
                s.can_transition_to(&State::Error),
                "{s} must be able to fail"
            );
        }
    }

    #[test]
    fn happy_path_chain_is_legal() {
        let chain = [
            State::Idle,
            State::Recording,
            State::Transcribing,
            State::Formatting,
            State::Injecting,
            State::Done,
        ];
        for pair in chain.windows(2) {
            assert!(
                pair[0].can_transition_to(&pair[1]),
                "{} -> {} must be legal",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn stages_may_be_skipped_forward_but_never_revisited() {
        // A remote client formats and then takes delivery: no Injecting stage.
        assert!(State::Formatting.can_transition_to(&State::Done));
        // VAD disabled: Recording straight to Transcribing is already covered;
        // skipping the LLM pass means Transcribing -> Injecting.
        assert!(State::Transcribing.can_transition_to(&State::Injecting));
        // Backward moves are illegal.
        assert!(!State::Formatting.can_transition_to(&State::Recording));
        assert!(!State::Injecting.can_transition_to(&State::Transcribing));
        // Self-transitions are illegal.
        assert!(!State::Recording.can_transition_to(&State::Recording));
    }

    #[test]
    fn terminal_states_only_reset_to_idle() {
        for t in [State::Done, State::Error, State::Cancelled] {
            assert!(t.is_terminal());
            assert!(t.can_transition_to(&State::Idle));
            assert!(!t.can_transition_to(&State::Recording));
            assert!(!t.can_transition_to(&State::Done));
        }
    }

    #[test]
    fn unknown_state_is_not_terminal_and_transitions_freely() {
        let u = State::Unknown("buffering".into());
        assert!(!u.is_terminal());
        assert!(u.can_transition_to(&State::Done));
        assert!(State::Recording.can_transition_to(&u));
    }

    #[test]
    fn is_active_excludes_idle_and_terminals() {
        assert!(!State::Idle.is_active());
        assert!(State::Recording.is_active());
        assert!(State::Injecting.is_active());
        assert!(!State::Done.is_active());
        assert!(!State::Cancelled.is_active());
    }

    #[test]
    fn awaiting_consent_is_the_only_unsettled_outcome() {
        assert!(!InjectionOutcome::AwaitingConsent {
            backend: "portal".into(),
            consent_id: None,
        }
        .is_settled());
        assert!(InjectionOutcome::Delivered.is_settled());
        assert!(InjectionOutcome::Injected {
            method: InjectMethod::Paste,
            chars: 3,
        }
        .is_settled());
    }

    #[test]
    fn delivered_is_not_an_injection() {
        assert!(!InjectionOutcome::Delivered.did_inject());
        assert!(InjectionOutcome::Injected {
            method: InjectMethod::Paste,
            chars: 1,
        }
        .did_inject());
    }

    #[test]
    fn unknown_route_round_trips_losslessly() {
        let json = "\"telepathy\"";
        let r: Route = serde_json::from_str(json).unwrap();
        assert_eq!(r, Route::Unknown("telepathy".into()));
        assert!(!r.is_known());
        assert_eq!(serde_json::to_string(&r).unwrap(), json);
    }

    #[test]
    fn known_routes_cover_the_preserved_set() {
        let names: Vec<&str> = Route::known().iter().map(Route::as_str).collect();
        assert_eq!(names, ["type", "timer", "local", "edit", "command"]);
    }
}
