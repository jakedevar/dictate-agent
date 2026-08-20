//! Successful command results.

use serde::{Deserialize, Serialize};

use crate::capability::{Capabilities, ServerHello};
use crate::event::FinalText;
use crate::records::{ConfigSnapshot, DictionaryEntry, HistoryAnalytics, HistoryPage, Snippet};
use crate::state::{DictationMode, InjectionOutcome, Route, SessionId, State};
use crate::timings::StageTimings;

/// A finished transcript and everything measured about producing it.
///
/// # One type, two delivery paths
///
/// This same payload is what [`Event::Final`](crate::Event::Final) flattens for
/// a streaming client and what [`CommandResult::Transcript`] returns for a
/// one-shot `POST /v1/transcribe`. Sharing it is deliberate: it is why S33 is
/// cheap. If the HTTP response and the WebSocket event were separate types they
/// would drift, and the phone client and the desktop UI would end up parsing
/// different shapes of the same fact.
///
/// The difference between the two paths shows up in exactly one field:
/// [`Transcript::injection`] is [`InjectionOutcome::Delivered`] when the caller
/// takes the text, and [`InjectionOutcome::Injected`] when it was typed into
/// the host's focused application.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    /// The formatted text, ready to inject. See [`FinalText`].
    pub text: FinalText,

    /// The raw speech-to-text output before corrections, dictionary, snippets,
    /// and formatting.
    ///
    /// Present when the connection is permitted to see it; it is the ground
    /// truth the formatting layers are evaluated against, and it is also the
    /// most privacy-sensitive field here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_text: Option<String>,

    /// Where the text was dispatched.
    #[serde(default)]
    pub route: Route,

    /// Per-stage latency. Required, not optional: the ≤1.0s p50 budget is
    /// measured from this field, and S12 records real turbo-CUDA numbers
    /// through it.
    #[serde(default)]
    pub timings: StageTimings,

    /// What became of the text.
    ///
    /// May be [`InjectionOutcome::AwaitingConsent`], in which case the session
    /// is not over — see [`Event::InjectionResolved`](crate::Event::InjectionResolved).
    pub injection: InjectionOutcome,

    /// Words in [`Transcript::text`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word_count: Option<u32>,

    /// The speech-to-text model that produced this, e.g. `"large-v3-turbo"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl Transcript {
    /// A transcript handed back to a caller rather than injected anywhere.
    pub fn delivered(text: impl Into<FinalText>) -> Self {
        Self {
            text: text.into(),
            raw_text: None,
            route: Route::Type,
            timings: StageTimings::default(),
            injection: InjectionOutcome::Delivered,
            word_count: None,
            model: None,
        }
    }

    /// Whether the pipeline has fully settled.
    ///
    /// `false` while a consent-gated injection is still pending.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        self.injection.is_settled()
    }
}

/// The successful answer to a [`Command`](crate::Command).
///
/// Open, like [`Event`](crate::Event): an unrecognized result deserializes to
/// [`CommandResult::Unknown`] rather than failing the connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CommandResult {
    /// The command succeeded and has nothing to report.
    Ack,

    /// Answer to [`Command::Handshake`](crate::Command::Handshake).
    ///
    /// Boxed for layout only; invisible on the wire.
    Handshake(Box<ServerHello>),

    /// Answer to [`Command::GetStatus`](crate::Command::GetStatus).
    ///
    /// Boxed for layout only; invisible on the wire.
    Status(Box<Status>),

    /// A session was created.
    SessionStarted {
        /// The new session.
        session_id: SessionId,
    },

    /// A session was stopped; its transcript arrives as
    /// [`Event::Final`](crate::Event::Final).
    SessionStopped {
        /// The session that was stopped.
        session_id: SessionId,
    },

    /// A session was cancelled and its audio discarded.
    SessionCancelled {
        /// The session that was cancelled.
        session_id: SessionId,
    },

    /// Configuration, after a read or a write.
    Config(ConfigSnapshot),

    /// A list of dictionary entries.
    Dictionary {
        /// The entries.
        entries: Vec<DictionaryEntry>,
    },

    /// A single dictionary entry after creation or update, carrying the
    /// server-assigned [`DictionaryEntry::id`].
    DictionaryEntry {
        /// The stored entry.
        entry: DictionaryEntry,
    },

    /// A list of snippets.
    Snippets {
        /// The snippets.
        snippets: Vec<Snippet>,
    },

    /// A single snippet after creation or update.
    Snippet {
        /// The stored snippet.
        snippet: Snippet,
    },

    /// A record was removed.
    Deleted {
        /// Identifier of the removed record.
        id: i64,
    },

    /// A page of history.
    History(HistoryPage),

    /// Aggregate history metrics.
    HistoryAnalytics(HistoryAnalytics),

    /// A finished transcript, for
    /// [`Command::TranscribeAudio`](crate::Command::TranscribeAudio).
    ///
    /// Boxed for layout only; invisible on the wire.
    Transcript(Box<Transcript>),

    /// A binary audio stream was opened.
    AudioStreamOpened {
        /// Put this in every [`AudioFrame::stream_id`](crate::AudioFrame::stream_id).
        stream_id: u32,
        /// The session the stream feeds.
        session_id: SessionId,
    },

    /// A result type this build does not recognize.
    #[serde(other)]
    Unknown,
}

impl CommandResult {
    /// The result's wire tag.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Ack => "ack",
            Self::Handshake(_) => "handshake",
            Self::Status(_) => "status",
            Self::SessionStarted { .. } => "session_started",
            Self::SessionStopped { .. } => "session_stopped",
            Self::SessionCancelled { .. } => "session_cancelled",
            Self::Config(_) => "config",
            Self::Dictionary { .. } => "dictionary",
            Self::DictionaryEntry { .. } => "dictionary_entry",
            Self::Snippets { .. } => "snippets",
            Self::Snippet { .. } => "snippet",
            Self::Deleted { .. } => "deleted",
            Self::History(_) => "history",
            Self::HistoryAnalytics(_) => "history_analytics",
            Self::Transcript(_) => "transcript",
            Self::AudioStreamOpened { .. } => "audio_stream_opened",
            Self::Unknown => "unknown",
        }
    }
}

/// A snapshot of what the daemon is doing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Status {
    /// Current pipeline state.
    #[serde(default)]
    pub state: State,

    /// The active session, when one is in flight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionSummary>,

    /// About the daemon.
    pub daemon: DaemonInfo,

    /// The speech-to-text model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelStatus>,

    /// What **this connection** is permitted to do. Re-sent here so a client
    /// that skipped the handshake can still discover its own limits.
    #[serde(default)]
    pub capabilities: Capabilities,
}

/// Daemon identity and health.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonInfo {
    /// Product name, e.g. `"dictated"`.
    pub name: String,
    /// Build version.
    pub version: String,
    /// The protocol version in force on this connection.
    pub protocol_version: u16,
    /// Process id, for local clients that want to signal it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// How long the daemon has been running, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uptime_ms: Option<u64>,
}

/// Speech-to-text model state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelStatus {
    /// Model name, e.g. `"large-v3-turbo"`.
    pub name: String,
    /// Whether it is resident and ready. A `false` here explains a slow first
    /// transcription rather than leaving it looking like a regression.
    #[serde(default)]
    pub loaded: bool,
    /// Inference backend, e.g. `"cuda"` or `"cpu"`. Load-bearing for the
    /// latency budget: a CPU-fallback number is not comparable to a CUDA one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
}

/// The session currently in flight.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSummary {
    /// Which session.
    pub session_id: SessionId,
    /// Its state.
    pub state: State,
    /// How it was triggered.
    #[serde(default)]
    pub mode: DictationMode,
    /// When it started, in milliseconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_ms: Option<u64>,
    /// The route chosen, once routing has happened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<Route>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{ServerInfo, Features};
    use crate::timings::StageTiming;

    fn status() -> Status {
        Status {
            state: State::Idle,
            session: None,
            daemon: DaemonInfo {
                name: "dictated".into(),
                version: "0.2.0".into(),
                protocol_version: crate::PROTOCOL_VERSION,
                pid: Some(4242),
                uptime_ms: Some(90_000),
            },
            model: Some(ModelStatus {
                name: "large-v3-turbo".into(),
                loaded: true,
                backend: Some("cuda".into()),
            }),
            capabilities: Capabilities::local_trusted(),
        }
    }

    #[test]
    fn every_result_round_trips() {
        let results = vec![
            CommandResult::Ack,
            CommandResult::Handshake(Box::new(ServerHello {
                protocol_version: 1,
                supported_versions: vec![1],
                server: ServerInfo::new("dictated", "0.2.0"),
                capabilities: Capabilities::remote_transcription_only(),
            })),
            CommandResult::Status(Box::new(status())),
            CommandResult::SessionStarted {
                session_id: "s1".into(),
            },
            CommandResult::SessionStopped {
                session_id: "s1".into(),
            },
            CommandResult::SessionCancelled {
                session_id: "s1".into(),
            },
            CommandResult::Config(ConfigSnapshot {
                values: serde_json::json!({"whisper": {"model": "large-v3-turbo"}}),
                path: None,
                applied: vec![],
                restart_required: vec![],
            }),
            CommandResult::Dictionary {
                entries: vec![DictionaryEntry::new("Kubernetes")],
            },
            CommandResult::DictionaryEntry {
                entry: DictionaryEntry::new("Kubernetes"),
            },
            CommandResult::Snippets {
                snippets: vec![Snippet::new("sig", "— Jake")],
            },
            CommandResult::Snippet {
                snippet: Snippet::new("sig", "— Jake"),
            },
            CommandResult::Deleted { id: 9 },
            CommandResult::History(HistoryPage::default()),
            CommandResult::Transcript(Box::new(Transcript::delivered("hello"))),
            CommandResult::AudioStreamOpened {
                stream_id: 3,
                session_id: "s1".into(),
            },
        ];
        for r in results {
            let json = serde_json::to_string(&r).unwrap();
            assert_eq!(
                serde_json::from_str::<CommandResult>(&json).unwrap(),
                r,
                "round trip failed for {}",
                r.name()
            );
            assert_eq!(
                serde_json::to_value(&r).unwrap()["type"],
                r.name(),
                "name() must match the wire tag"
            );
        }
    }

    #[test]
    fn unknown_result_degrades_instead_of_failing() {
        let r: CommandResult =
            serde_json::from_str(r#"{"type":"model_downloaded","name":"x"}"#).unwrap();
        assert_eq!(r, CommandResult::Unknown);
    }

    /// The shared-payload property that makes S33 cheap.
    #[test]
    fn the_http_and_websocket_paths_carry_the_same_transcript_shape() {
        let t = Transcript {
            text: "Hello there.".into(),
            raw_text: Some("hello there".into()),
            route: Route::Type,
            timings: StageTimings {
                stt: StageTiming::ran(410.0),
                ..Default::default()
            },
            injection: InjectionOutcome::Delivered,
            word_count: Some(2),
            model: Some("large-v3-turbo".into()),
        };

        // One-shot HTTP: the transcript is the result.
        let http = serde_json::to_value(CommandResult::Transcript(Box::new(t.clone()))).unwrap();
        // Streaming WS: the same fields, flattened into the event.
        let ws = serde_json::to_value(crate::Event::Final {
            session_id: "s1".into(),
            transcript: Box::new(t),
        })
        .unwrap();

        for field in ["text", "raw_text", "route", "timings", "injection"] {
            assert_eq!(
                http[field], ws[field],
                "field `{field}` must be identical on both paths"
            );
        }
    }

    #[test]
    fn delivered_transcript_is_settled_and_injected_nothing() {
        let t = Transcript::delivered("hi");
        assert!(t.is_settled());
        assert!(!t.injection.did_inject());
        assert_eq!(t.route, Route::Type);
    }

    #[test]
    fn transcript_requires_timings_even_when_empty() {
        // `timings` has a default, so an omitting peer still parses...
        let t: Transcript =
            serde_json::from_str(r#"{"text":"hi","injection":{"status":"delivered"}}"#).unwrap();
        // ...but reads as unreported rather than as zeros.
        assert_eq!(t.timings, StageTimings::default());
        assert!(!t.timings.stt.did_run());
    }

    #[test]
    fn status_reports_backend_so_latency_numbers_are_comparable() {
        let s = status();
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["model"]["backend"], "cuda");
        assert_eq!(v["state"], "idle");
        assert_eq!(v["capabilities"]["features"]["text_injection"], true);
        assert_eq!(serde_json::from_value::<Status>(v).unwrap(), s);
    }

    #[test]
    fn status_capabilities_default_when_omitted() {
        let s: Status = serde_json::from_str(
            r#"{"daemon":{"name":"dictated","version":"0.2.0","protocol_version":1}}"#,
        )
        .unwrap();
        assert_eq!(s.state, State::Idle);
        assert_eq!(s.capabilities.features, Features::default());
        assert!(!s.capabilities.features.text_injection);
    }
}
