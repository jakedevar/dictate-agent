//! Client → server commands.

use serde::{Deserialize, Serialize};

use crate::audio::{AudioFormat, AudioSource};
use crate::capability::{Features, Hello};
use crate::records::{ConfigEntry, DictionaryEntry, HistoryQuery, Snippet};
use crate::state::{DictationMode, Route};

/// A request for the server to do something.
///
/// # This enum is deliberately closed
///
/// Unlike [`Event`](crate::Event) and [`CommandResult`](crate::CommandResult),
/// `Command` has no `Unknown` catch-all. An unrecognized command **fails to
/// deserialize**, and the server is required to answer
/// [`ErrorCode::UnsupportedCommand`](crate::ErrorCode::UnsupportedCommand).
///
/// The asymmetry is the whole point. A client that receives an event it does
/// not understand can safely ignore it and carry on. A server that receives a
/// command it does not understand must **not** carry on: the caller is waiting
/// for an effect, and silently dropping the request would leave it waiting
/// forever for text that will never arrive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    /// Open the connection and negotiate version and capabilities. A server may
    /// require this before any other command
    /// ([`ErrorCode::HandshakeRequired`](crate::ErrorCode::HandshakeRequired)).
    Handshake(Hello),

    /// Begin capturing audio on the host.
    StartDictation {
        /// How the session was triggered, which determines how it ends.
        #[serde(default)]
        mode: DictationMode,
        /// Per-session overrides.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        options: Option<SessionOptions>,
    },

    /// Atomically start a toggle-mode session when idle, or stop the active
    /// recording session. The daemon resolves the branch in its single-writer
    /// engine, so clients do not need a racy `get_status` followed by
    /// `start_dictation` or `stop` sequence.
    Toggle,

    /// Stop capturing and run the rest of the pipeline.
    Stop,

    /// Abandon the active session and discard its audio and transcript.
    ///
    /// Legal in every non-terminal state; see
    /// [`State::can_transition_to`](crate::State::can_transition_to).
    Cancel,

    /// Report the current state, capabilities, and daemon information.
    GetStatus,

    /// Begin receiving events on this connection.
    Subscribe {
        /// Event type names to receive; empty means all of them.
        ///
        /// Strings rather than a typed enum so a client can subscribe to an
        /// event this build has never heard of — required for a UI to work
        /// against a newer daemon.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        events: Vec<String>,
    },

    /// Stop receiving events on this connection.
    Unsubscribe,

    /// Read configuration.
    GetConfig {
        /// Dotted path to read; absent reads the whole tree.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },

    /// Write configuration. Applied atomically: either every entry validates
    /// and is written, or none is.
    SetConfig {
        /// The settings to write.
        entries: Vec<ConfigEntry>,
    },

    /// List personal-dictionary entries.
    ListDictionary {
        /// Substring filter over phrases.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        query: Option<String>,
        /// Maximum entries to return.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },

    /// Create or update a dictionary entry. Creates when
    /// [`DictionaryEntry::id`] is absent.
    UpsertDictionaryEntry {
        /// The entry.
        entry: DictionaryEntry,
    },

    /// Delete a dictionary entry.
    DeleteDictionaryEntry {
        /// Identifier of the entry to remove.
        id: i64,
    },

    /// List snippets.
    ListSnippets {
        /// Substring filter over triggers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        query: Option<String>,
        /// Maximum entries to return.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },

    /// Create or update a snippet. Creates when [`Snippet::id`] is absent.
    UpsertSnippet {
        /// The snippet.
        snippet: Snippet,
    },

    /// Delete a snippet.
    DeleteSnippet {
        /// Identifier of the snippet to remove.
        id: i64,
    },

    /// Search recorded dictations.
    QueryHistory {
        /// Filters.
        #[serde(default)]
        query: HistoryQuery,
    },

    /// Transcribe caller-supplied audio through the full pipeline.
    ///
    /// The thin-client entry point. Backs `POST /v1/transcribe`, where the
    /// audio arrives as the HTTP body ([`AudioSource::Body`]) and the response
    /// is a [`Transcript`](crate::Transcript). Nothing is injected unless
    /// [`SessionOptions::inject`] asks for it *and* the connection's
    /// [`Features::text_injection`] permits it.
    TranscribeAudio {
        /// Where the audio comes from.
        audio: AudioSource,
        /// Per-request overrides.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        options: Option<SessionOptions>,
    },

    /// Open a binary audio stream. The server replies with a stream id to put
    /// in each [`AudioFrame`](crate::AudioFrame).
    BeginAudioStream {
        /// Layout of the samples that will follow. Declared once here rather
        /// than repeated in every frame header.
        #[serde(default)]
        format: AudioFormat,
        /// Per-session overrides.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        options: Option<SessionOptions>,
    },

    /// Close a binary audio stream and transcribe what was received.
    ///
    /// Equivalent to sending a [`FrameKind::End`](crate::FrameKind::End) frame;
    /// provided for clients that would rather not construct a binary frame just
    /// to say "done".
    EndAudioStream {
        /// The stream to close.
        stream_id: u32,
    },
}

impl Command {
    /// The command's wire tag, for logs and error messages.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Handshake(_) => "handshake",
            Self::StartDictation { .. } => "start_dictation",
            Self::Toggle => "toggle",
            Self::Stop => "stop",
            Self::Cancel => "cancel",
            Self::GetStatus => "get_status",
            Self::Subscribe { .. } => "subscribe",
            Self::Unsubscribe => "unsubscribe",
            Self::GetConfig { .. } => "get_config",
            Self::SetConfig { .. } => "set_config",
            Self::ListDictionary { .. } => "list_dictionary",
            Self::UpsertDictionaryEntry { .. } => "upsert_dictionary_entry",
            Self::DeleteDictionaryEntry { .. } => "delete_dictionary_entry",
            Self::ListSnippets { .. } => "list_snippets",
            Self::UpsertSnippet { .. } => "upsert_snippet",
            Self::DeleteSnippet { .. } => "delete_snippet",
            Self::QueryHistory { .. } => "query_history",
            Self::TranscribeAudio { .. } => "transcribe_audio",
            Self::BeginAudioStream { .. } => "begin_audio_stream",
            Self::EndAudioStream { .. } => "end_audio_stream",
        }
    }

    /// Whether this command changes persistent state.
    ///
    /// Useful for audit logging and for refusing writes on a read-only
    /// connection.
    #[must_use]
    pub fn mutates(&self) -> bool {
        matches!(
            self,
            Self::SetConfig { .. }
                | Self::UpsertDictionaryEntry { .. }
                | Self::DeleteDictionaryEntry { .. }
                | Self::UpsertSnippet { .. }
                | Self::DeleteSnippet { .. }
        )
    }

    /// Whether the connection's negotiated capabilities permit this command.
    ///
    /// Centralized here, rather than in the daemon and again in the network
    /// server, so the local and LAN paths cannot drift apart — a capability
    /// enforced on one transport but forgotten on the other is exactly the bug
    /// this crate exists to prevent. A server MUST answer
    /// [`ErrorCode::Forbidden`](crate::ErrorCode::Forbidden) when this returns
    /// `false`.
    ///
    /// This is an authorization *check*, not authentication: whether the caller
    /// is who they claim is S33's problem, and must be settled before the
    /// resulting [`Features`] are handed here.
    #[must_use]
    pub fn is_permitted(&self, features: &Features) -> bool {
        match self {
            // Always available: they are how a client discovers everything else.
            Self::Handshake(_) | Self::GetStatus | Self::Subscribe { .. } | Self::Unsubscribe => {
                true
            }

            // Driving the host's microphone.
            Self::StartDictation { .. } | Self::Toggle => features.host_capture,

            // Stop and Cancel apply to whatever session this connection owns —
            // including one it started by uploading audio — so they are gated
            // on being able to have started anything at all.
            Self::Stop | Self::Cancel => {
                features.host_capture || features.transcribe_upload || features.streaming_audio
            }

            Self::TranscribeAudio { .. } => features.transcribe_upload,
            Self::BeginAudioStream { .. } | Self::EndAudioStream { .. } => features.streaming_audio,

            Self::GetConfig { .. } => features.config_read,
            Self::SetConfig { .. } => features.config_write,

            Self::ListDictionary { .. } => features.dictionary_read,
            Self::UpsertDictionaryEntry { .. } | Self::DeleteDictionaryEntry { .. } => {
                features.dictionary_write
            }

            Self::ListSnippets { .. } => features.snippets_read,
            Self::UpsertSnippet { .. } | Self::DeleteSnippet { .. } => features.snippets_write,

            Self::QueryHistory { .. } => features.history_read,
        }
    }

    /// The per-session overrides attached to this command, if any.
    #[must_use]
    pub fn options(&self) -> Option<&SessionOptions> {
        match self {
            Self::StartDictation { options, .. }
            | Self::TranscribeAudio { options, .. }
            | Self::BeginAudioStream { options, .. } => options.as_ref(),
            _ => None,
        }
    }
}

/// Per-session overrides.
///
/// Every field is `Option`, and `None` means "use the daemon's configured
/// default" — never "off". This matters: a client that omits a field must not
/// accidentally disable formatting.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SessionOptions {
    /// Force a route instead of consulting the router.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<Route>,

    /// Whether to inject the result into the focused application.
    ///
    /// `None` lets the server decide from the connection's capabilities, which
    /// is the correct default for both a local client (inject) and a remote one
    /// (do not). Requesting `Some(true)` without
    /// [`Features::text_injection`] is
    /// [`ErrorCode::Forbidden`](crate::ErrorCode::Forbidden), not a silent
    /// downgrade — a client that asked to type into an editor deserves to know
    /// it did not happen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inject: Option<bool>,

    /// Run the LLM formatting pass. `None` applies the configured skip-rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format_llm: Option<bool>,

    /// Apply the personal dictionary and snippets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_dictionary: Option<bool>,

    /// BCP-47 language hint for the recognizer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,

    /// Target application, for per-app formatting profiles (S23). A remote
    /// client can supply this to get the tone it wants without the host being
    /// able to detect focus on its behalf.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,

    /// Do not persist this session to history, regardless of configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub privacy: Option<bool>,
}

impl SessionOptions {
    /// Options for a caller that wants text back and no side effects.
    #[must_use]
    pub fn transcription_only() -> Self {
        Self {
            inject: Some(false),
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{ClientInfo, ClientKind};

    #[test]
    fn unit_commands_serialize_as_a_bare_tag() {
        assert_eq!(
            serde_json::to_string(&Command::Stop).unwrap(),
            r#"{"type":"stop"}"#
        );
        assert_eq!(
            serde_json::to_string(&Command::Cancel).unwrap(),
            r#"{"type":"cancel"}"#
        );
    }

    /// The closed-enum half of the compatibility rule: a command from a newer
    /// peer must fail loudly rather than be silently dropped.
    #[test]
    fn unknown_command_fails_to_deserialize() {
        let err = serde_json::from_str::<Command>(r#"{"type":"summon_kraken"}"#);
        assert!(
            err.is_err(),
            "an unknown command must not deserialize; the server owes the \
             caller an UnsupportedCommand error"
        );
    }

    /// ...while unknown *fields* on a known command are still ignored.
    #[test]
    fn unknown_fields_on_a_known_command_are_ignored() {
        let c: Command =
            serde_json::from_str(r#"{"type":"start_dictation","mode":"toggle","warp":9}"#).unwrap();
        assert_eq!(
            c,
            Command::StartDictation {
                mode: DictationMode::Toggle,
                options: None
            }
        );
    }

    #[test]
    fn start_dictation_defaults_to_toggle() {
        let c: Command = serde_json::from_str(r#"{"type":"start_dictation"}"#).unwrap();
        assert_eq!(
            c,
            Command::StartDictation {
                mode: DictationMode::Toggle,
                options: None
            }
        );
    }

    #[test]
    fn handshake_flattens_hello_alongside_the_tag() {
        let c = Command::Handshake(Hello::new(ClientInfo::new("dictate-cli", ClientKind::Cli)));
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["type"], "handshake");
        assert_eq!(v["protocol_version"], 1);
        assert_eq!(v["client"]["name"], "dictate-cli");
        assert_eq!(serde_json::from_value::<Command>(v).unwrap(), c);
    }

    /// The scenario the capability model exists for.
    #[test]
    fn a_remote_client_may_transcribe_but_not_drive_the_host() {
        let remote = Features::remote_transcription_only();

        assert!(Command::TranscribeAudio {
            audio: AudioSource::Body {
                format: AudioFormat::wav()
            },
            options: None,
        }
        .is_permitted(&remote));
        assert!(Command::BeginAudioStream {
            format: AudioFormat::whisper_native(),
            options: None
        }
        .is_permitted(&remote));

        // ...but it cannot switch on the host's microphone,
        assert!(!Command::StartDictation {
            mode: DictationMode::Toggle,
            options: None
        }
        .is_permitted(&remote));
        assert!(!Command::Toggle.is_permitted(&remote));
        // ...rewrite the host's config,
        assert!(!Command::SetConfig { entries: vec![] }.is_permitted(&remote));
        // ...or read the host's dictation history.
        assert!(!Command::QueryHistory {
            query: HistoryQuery::default()
        }
        .is_permitted(&remote));
    }

    #[test]
    fn a_local_client_may_do_everything() {
        let local = Features::local_trusted();
        let all = [
            Command::StartDictation {
                mode: DictationMode::PushToTalk,
                options: None,
            },
            Command::Toggle,
            Command::Stop,
            Command::Cancel,
            Command::GetStatus,
            Command::SetConfig { entries: vec![] },
            Command::QueryHistory {
                query: HistoryQuery::default(),
            },
            Command::UpsertSnippet {
                snippet: Snippet::new("sig", "— Jake"),
            },
            Command::DeleteDictionaryEntry { id: 1 },
        ];
        for c in all {
            assert!(c.is_permitted(&local), "{} must be permitted", c.name());
        }
    }

    /// Discovery commands must work before anything is negotiated, or a client
    /// could never learn why it is being refused.
    #[test]
    fn discovery_commands_need_no_capabilities() {
        let none = Features::default();
        assert!(Command::GetStatus.is_permitted(&none));
        assert!(Command::Subscribe { events: vec![] }.is_permitted(&none));
        assert!(Command::Unsubscribe.is_permitted(&none));
        assert!(Command::Handshake(Hello::new(ClientInfo::new(
            "x",
            ClientKind::Remote
        )))
        .is_permitted(&none));
    }

    #[test]
    fn mutating_commands_are_identified() {
        assert!(Command::SetConfig { entries: vec![] }.mutates());
        assert!(Command::DeleteSnippet { id: 3 }.mutates());
        assert!(!Command::GetStatus.mutates());
        assert!(!Command::QueryHistory {
            query: HistoryQuery::default()
        }
        .mutates());
        // Starting a session is an action, not a persistent mutation.
        assert!(!Command::Stop.mutates());
    }

    #[test]
    fn every_command_name_is_its_wire_tag() {
        let samples = [
            Command::Toggle,
            Command::Stop,
            Command::Cancel,
            Command::GetStatus,
            Command::Unsubscribe,
            Command::GetConfig { path: None },
            Command::SetConfig { entries: vec![] },
            Command::DeleteSnippet { id: 1 },
            Command::EndAudioStream { stream_id: 1 },
        ];
        for c in samples {
            let v = serde_json::to_value(&c).unwrap();
            assert_eq!(v["type"], c.name());
        }
    }

    #[test]
    fn session_options_omit_unset_fields_entirely() {
        let c = Command::StartDictation {
            mode: DictationMode::PushToTalk,
            options: Some(SessionOptions::transcription_only()),
        };
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(
            json,
            r#"{"type":"start_dictation","mode":"push_to_talk","options":{"inject":false}}"#
        );
        assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), c);
    }

    #[test]
    fn options_accessor_reaches_every_carrying_variant() {
        let o = SessionOptions::transcription_only();
        assert!(Command::StartDictation {
            mode: DictationMode::Toggle,
            options: Some(o.clone())
        }
        .options()
        .is_some());
        assert!(Command::TranscribeAudio {
            audio: AudioSource::Stream { stream_id: 1 },
            options: Some(o.clone())
        }
        .options()
        .is_some());
        assert!(Command::BeginAudioStream {
            format: AudioFormat::default(),
            options: Some(o)
        }
        .options()
        .is_some());
        assert!(Command::Stop.options().is_none());
    }
}
