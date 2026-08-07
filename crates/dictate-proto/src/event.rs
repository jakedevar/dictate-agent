//! Server → client events.

use serde::{Deserialize, Serialize};

use crate::error::ProtoError;
use crate::result::Transcript;
use crate::state::{InjectionOutcome, SessionId, State};

/// Text that has been through the full pipeline and is safe to inject.
///
/// Distinct from [`Hypothesis`] at the type level: a function that injects text
/// takes a `FinalText`, so a partial transcript cannot reach it by accident.
/// See the [`Hypothesis`] docs for why this matters.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FinalText(pub String);

impl FinalText {
    /// Borrow the text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for FinalText {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for FinalText {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl core::fmt::Display for FinalText {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

/// An in-progress guess at what the user is saying.
///
/// # A hypothesis is never injectable text
///
/// A partial is a **HUD affordance only**. It exists so the overlay can show
/// something while the user is still speaking. It has not been through the
/// corrections layer, the personal dictionary, snippet expansion, the
/// formatting rules, or the LLM pass, and it will be contradicted by the
/// [`Event::Final`] that follows. Injecting one would type text the user never
/// finished saying.
///
/// That rule is enforced in two places rather than in a comment:
///
/// - **In Rust**, this is a distinct type from [`FinalText`], so it cannot be
///   passed where injectable text is expected.
/// - **On the wire**, [`Event::Partial`] carries a field named `hypothesis`
///   while [`Event::Final`] carries one named `text`. A TypeScript or Swift
///   client reading `.text` off an event object gets `undefined` for a partial
///   — the mistake fails immediately and visibly instead of silently typing a
///   half-finished sentence.
///
/// Research spike R2 returned DEFER on live partials, so nothing emits these
/// today; [`Features::partial_transcripts`](crate::Features::partial_transcripts)
/// is `false`. The variant is specified now precisely so that turning them on
/// later is an additive change rather than a breaking one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Hypothesis(pub String);

impl Hypothesis {
    /// Borrow the text of the guess.
    ///
    /// Deliberately **not** named `as_str`, and deliberately not convertible to
    /// [`FinalText`]: promoting a hypothesis to injectable text is not a thing
    /// a caller should be able to do fluently.
    #[must_use]
    pub fn display_text(&self) -> &str {
        &self.0
    }
}

impl From<String> for Hypothesis {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for Hypothesis {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Something the server is telling the client about.
///
/// Unlike [`Command`](crate::Command), this enum is **open**: an unrecognized
/// event deserializes to [`Event::Unknown`] rather than failing, so a UI built
/// against protocol v1 keeps working against a daemon that has learned new
/// tricks. A relay must forward raw bytes rather than re-serializing a parsed
/// `Event`, since `Unknown` does not preserve the payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// The session moved between states.
    StateChanged {
        /// The session this concerns.
        session_id: SessionId,
        /// State being left.
        from: State,
        /// State being entered.
        to: State,
        /// When, in milliseconds since the Unix epoch.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at_ms: Option<u64>,
    },

    /// A revised guess at the utterance so far. **Not injectable** — see
    /// [`Hypothesis`].
    Partial {
        /// The session this concerns.
        session_id: SessionId,
        /// Position in the sequence of guesses, from zero. Each partial
        /// *replaces* the previous one for this session; they are not appended.
        seq: u32,
        /// The current guess.
        hypothesis: Hypothesis,
        /// When, in milliseconds since the Unix epoch.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at_ms: Option<u64>,
    },

    /// The pipeline finished and produced a transcript.
    ///
    /// The [`Transcript`] fields are flattened alongside `type` and
    /// `session_id`, so this event carries `text`, `route`, and `timings`
    /// directly.
    ///
    /// Note that [`Transcript::injection`] may be
    /// [`InjectionOutcome::AwaitingConsent`], in which case the session is
    /// **not** finished: an [`Event::InjectionResolved`] follows.
    Final {
        /// The session this concerns.
        session_id: SessionId,
        /// The result.
        ///
        /// Boxed purely for layout: a [`Transcript`] is ~400 bytes, mostly
        /// [`StageTimings`](crate::StageTimings), and leaving it inline would
        /// make *every* `Event` that large — including [`Event::AudioLevel`],
        /// which is broadcast ~30 times a second to every subscriber. The box
        /// is invisible on the wire.
        #[serde(flatten)]
        transcript: Box<Transcript>,
    },

    /// A pending consent-gated injection settled.
    ///
    /// Exists because R3 found that Wayland portals gate synthetic input behind
    /// an asynchronous user prompt, so [`Event::Final`] cannot always carry a
    /// terminal injection outcome. On X11 this event is emitted at most once
    /// per session and carries the same outcome the `Final` already reported;
    /// clients should treat it as idempotent.
    InjectionResolved {
        /// The session this concerns.
        session_id: SessionId,
        /// The settled outcome. Never
        /// [`InjectionOutcome::AwaitingConsent`].
        outcome: InjectionOutcome,
    },

    /// Something went wrong.
    Error {
        /// The session this concerns, or absent for a connection-level failure
        /// that belongs to no session.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<SessionId>,
        /// What went wrong.
        error: ProtoError,
    },

    /// Input amplitude, for level meters and the HUD.
    ///
    /// High-frequency: R1 recommends the emitter throttle to ~30/s. A client
    /// must tolerate these being dropped or coalesced — they are decoration,
    /// and no correctness depends on receiving every one.
    AudioLevel {
        /// The session this concerns.
        session_id: SessionId,
        /// Root-mean-square amplitude, normalized to `0.0..=1.0`.
        rms: f32,
        /// Peak amplitude since the previous event, normalized to `0.0..=1.0`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        peak: Option<f32>,
        /// When, in milliseconds since the Unix epoch.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at_ms: Option<u64>,
    },

    /// An event type this build does not recognize. See the crate-level
    /// compatibility rule.
    #[serde(other)]
    Unknown,
}

impl Event {
    /// The event's wire tag, matching the names used by
    /// [`Command::Subscribe`](crate::Command::Subscribe).
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::StateChanged { .. } => "state_changed",
            Self::Partial { .. } => "partial",
            Self::Final { .. } => "final",
            Self::InjectionResolved { .. } => "injection_resolved",
            Self::Error { .. } => "error",
            Self::AudioLevel { .. } => "audio_level",
            Self::Unknown => "unknown",
        }
    }

    /// The session this event concerns, when it concerns one.
    #[must_use]
    pub fn session_id(&self) -> Option<&SessionId> {
        match self {
            Self::StateChanged { session_id, .. }
            | Self::Partial { session_id, .. }
            | Self::Final { session_id, .. }
            | Self::InjectionResolved { session_id, .. }
            | Self::AudioLevel { session_id, .. } => Some(session_id),
            Self::Error { session_id, .. } => session_id.as_ref(),
            Self::Unknown => None,
        }
    }

    /// Whether a subscription for `events` should receive this event.
    ///
    /// An empty filter accepts everything.
    #[must_use]
    pub fn matches_filter(&self, events: &[String]) -> bool {
        events.is_empty() || events.iter().any(|e| e == self.name())
    }

    /// The injectable text this event carries, if any.
    ///
    /// Only [`Event::Final`] ever returns `Some`. [`Event::Partial`] returns
    /// `None` by construction — this accessor is the safe way to ask "is there
    /// text to type here?" without pattern-matching and getting it wrong.
    #[must_use]
    pub fn injectable_text(&self) -> Option<&FinalText> {
        match self {
            Self::Final { transcript, .. } => Some(&transcript.text),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Route;
    use crate::timings::{SkipReason, StageTiming, StageTimings};

    fn transcript() -> Transcript {
        Transcript {
            text: FinalText::from("Hello there."),
            raw_text: Some("hello there".into()),
            route: Route::Type,
            timings: StageTimings {
                stt: StageTiming::ran(410.0),
                fmt_llm: StageTiming::skipped(SkipReason::BelowMinWords),
                ..Default::default()
            },
            injection: InjectionOutcome::Delivered,
            word_count: Some(2),
            model: None,
        }
    }

    /// Constraint: a Partial must not be mistakable for injectable text, in
    /// Rust *or* from a language without a type system to lean on.
    #[test]
    fn a_partial_carries_no_text_field_on_the_wire() {
        let p = Event::Partial {
            session_id: "s1".into(),
            seq: 3,
            hypothesis: Hypothesis::from("hello ther"),
            at_ms: None,
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["type"], "partial");
        assert_eq!(v["hypothesis"], "hello ther");
        assert!(
            v.get("text").is_none(),
            "a TS client reading .text off a partial must get undefined, not a \
             half-finished sentence: {v}"
        );
        assert!(p.injectable_text().is_none());
    }

    #[test]
    fn a_final_carries_text_and_no_hypothesis_field() {
        let f = Event::Final {
            session_id: "s1".into(),
            transcript: Box::new(transcript()),
        };
        let v = serde_json::to_value(&f).unwrap();
        assert_eq!(v["type"], "final");
        assert_eq!(v["text"], "Hello there.");
        assert!(v.get("hypothesis").is_none());
        assert_eq!(
            f.injectable_text().map(FinalText::as_str),
            Some("Hello there.")
        );
    }

    /// The plan specifies Final{text, route, timings}; flattening the
    /// transcript is what produces that shape rather than a nested object.
    #[test]
    fn final_flattens_route_and_timings_to_the_top_level() {
        let f = Event::Final {
            session_id: "s1".into(),
            transcript: Box::new(transcript()),
        };
        let v = serde_json::to_value(&f).unwrap();
        assert_eq!(v["route"], "type");
        assert_eq!(v["timings"]["stt"]["status"], "ran");
        assert_eq!(v["timings"]["stt"]["ms"], 410.0);
        assert_eq!(v["timings"]["fmt_llm"]["status"], "skipped");
        assert_eq!(v["timings"]["fmt_llm"]["reason"], "below_min_words");
        assert_eq!(serde_json::from_value::<Event>(v).unwrap(), f);
    }

    #[test]
    fn every_event_round_trips() {
        let events = vec![
            Event::StateChanged {
                session_id: "s".into(),
                from: State::Recording,
                to: State::Transcribing,
                at_ms: Some(1_700_000_000_000),
            },
            Event::Partial {
                session_id: "s".into(),
                seq: 0,
                hypothesis: "part".into(),
                at_ms: None,
            },
            Event::Final {
                session_id: "s".into(),
                transcript: Box::new(transcript()),
            },
            Event::InjectionResolved {
                session_id: "s".into(),
                outcome: InjectionOutcome::ConsentDenied {
                    backend: "portal".into(),
                    detail: None,
                },
            },
            Event::Error {
                session_id: None,
                error: ProtoError::new(crate::ErrorCode::Internal, "boom"),
            },
            Event::AudioLevel {
                session_id: "s".into(),
                rms: 0.25,
                peak: Some(0.9),
                at_ms: None,
            },
        ];
        for e in events {
            let json = serde_json::to_string(&e).unwrap();
            assert_eq!(
                serde_json::from_str::<Event>(&json).unwrap(),
                e,
                "round trip failed for {}",
                e.name()
            );
        }
    }

    /// The open half of the compatibility rule: a v1 client must survive a
    /// newer daemon's event.
    #[test]
    fn unknown_event_degrades_instead_of_failing() {
        let e: Event =
            serde_json::from_str(r#"{"type":"wake_word_detected","keyword":"hey flow"}"#).unwrap();
        assert_eq!(e, Event::Unknown);
        assert_eq!(e.name(), "unknown");
        assert!(e.session_id().is_none());
        assert!(e.injectable_text().is_none());
    }

    #[test]
    fn unknown_fields_on_a_known_event_are_ignored() {
        let e: Event = serde_json::from_str(
            r#"{"type":"audio_level","session_id":"s","rms":0.5,"spectrum":[1,2,3]}"#,
        )
        .unwrap();
        assert_eq!(
            e,
            Event::AudioLevel {
                session_id: "s".into(),
                rms: 0.5,
                peak: None,
                at_ms: None,
            }
        );
    }

    #[test]
    fn subscription_filter_matches_by_name() {
        let e = Event::AudioLevel {
            session_id: "s".into(),
            rms: 0.1,
            peak: None,
            at_ms: None,
        };
        assert!(e.matches_filter(&[]), "an empty filter accepts everything");
        assert!(e.matches_filter(&["audio_level".to_string()]));
        assert!(!e.matches_filter(&["final".to_string()]));
    }

    #[test]
    fn session_id_accessor_covers_every_variant() {
        let e = Event::Error {
            session_id: Some("s9".into()),
            error: ProtoError::new(crate::ErrorCode::SttFailed, "x"),
        };
        assert_eq!(e.session_id().map(SessionId::as_str), Some("s9"));

        let connection_level = Event::Error {
            session_id: None,
            error: ProtoError::new(crate::ErrorCode::Unauthorized, "x"),
        };
        assert!(connection_level.session_id().is_none());
    }

    /// R3 forward-compat: the pending-consent flow must be expressible today.
    #[test]
    fn consent_gated_injection_has_a_full_event_flow() {
        let pending = Transcript {
            injection: InjectionOutcome::AwaitingConsent {
                backend: "portal".into(),
                consent_id: Some("req-7".into()),
            },
            ..transcript()
        };
        assert!(!pending.injection.is_settled());

        let f = Event::Final {
            session_id: "s".into(),
            transcript: Box::new(pending),
        };
        let json = serde_json::to_string(&f).unwrap();
        assert_eq!(serde_json::from_str::<Event>(&json).unwrap(), f);

        let resolved = Event::InjectionResolved {
            session_id: "s".into(),
            outcome: InjectionOutcome::Injected {
                method: crate::InjectMethod::Paste,
                chars: 12,
            },
        };
        assert!(matches!(
            &resolved,
            Event::InjectionResolved { outcome, .. } if outcome.is_settled()
        ));
    }
}
