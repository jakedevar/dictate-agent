//! Golden wire-format tests.
//!
//! These pin the **exact on-the-wire representation** of the protocol. They are
//! the mechanical enforcement of the compatibility rule in the crate docs: a
//! rename, a retype, a re-tagging, or a change of nesting fails here with a
//! diff of the actual bytes.
//!
//! # If a test in this file fails
//!
//! Do not "fix" it by updating the expected value. Decide which happened:
//!
//! - **You added an optional field** (`#[serde(default)]` +
//!   `skip_serializing_if`) and a golden that sets it now shows it. That is
//!   additive and permitted; update the golden.
//! - **You renamed, removed, retyped, or re-nested something.** That is a
//!   breaking change. Either revert it, or bump
//!   [`PROTOCOL_VERSION`](dictate_proto::PROTOCOL_VERSION) and update every
//!   consumer — S02, S32, and S33 all parse these bytes.
//!
//! Each case is checked in both directions: serializing the value must produce
//! the pinned JSON, and deserializing the pinned JSON must reproduce the value.
//! One-directional pinning would miss a field that is written but not read.

use dictate_proto::*;
use serde_json::json;

/// Assert a value's wire form is exactly `expected`, and that it survives the
/// round trip back.
fn pin<T>(label: &str, value: T, expected: serde_json::Value)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let actual = serde_json::to_value(&value)
        .unwrap_or_else(|e| panic!("{label}: serialization failed: {e}"));
    assert_eq!(
        actual, expected,
        "\n{label}: wire format changed.\n  actual:   {actual}\n  expected: {expected}\n\
         If this was deliberate, read the header of tests/golden.rs before editing.\n"
    );
    let back: T = serde_json::from_value(expected)
        .unwrap_or_else(|e| panic!("{label}: deserialization failed: {e}"));
    assert_eq!(back, value, "{label}: value did not survive the round trip");
}

fn full_timings() -> StageTimings {
    StageTimings {
        capture: StageTiming::ran(82.5),
        vad: StageTiming::ran(18.0),
        stt: StageTiming::ran(412.0),
        fmt_rules: StageTiming::ran(0.4),
        fmt_llm: StageTiming::skipped(SkipReason::BelowMinWords),
        inject: StageTiming::ran(61.0),
        total_ms: Some(598.0),
        audio_ms: Some(3400.0),
    }
}

fn full_timings_json() -> serde_json::Value {
    json!({
        "capture":   {"status": "ran", "ms": 82.5},
        "vad":       {"status": "ran", "ms": 18.0},
        "stt":       {"status": "ran", "ms": 412.0},
        "fmt_rules": {"status": "ran", "ms": 0.4},
        "fmt_llm":   {"status": "skipped", "reason": "below_min_words"},
        "inject":    {"status": "ran", "ms": 61.0},
        "total_ms":  598.0,
        "audio_ms":  3400.0
    })
}

fn transcript() -> Transcript {
    Transcript {
        text: "Hello there.".into(),
        raw_text: Some("hello there".into()),
        route: Route::Type,
        timings: full_timings(),
        injection: InjectionOutcome::Injected {
            method: InjectMethod::Paste,
            chars: 12,
        },
        word_count: Some(2),
        model: Some("large-v3-turbo".into()),
    }
}

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

#[test]
fn golden_envelope_request() {
    pin(
        "request",
        Message::request(1u64, Command::Stop),
        json!({"kind": "request", "v": 1, "id": 1, "command": {"type": "stop"}}),
    );
}

#[test]
fn golden_envelope_response_ok() {
    pin(
        "response/ok",
        Message::ok(1u64, CommandResult::Ack),
        json!({"kind": "response", "v": 1, "id": 1, "result": {"type": "ack"}}),
    );
}

#[test]
fn golden_envelope_response_error() {
    pin(
        "response/error",
        Message::err(
            "req-7",
            ProtoError::new(ErrorCode::Busy, "a session is already running")
                .with_retry_after_ms(500),
        ),
        json!({
            "kind": "response", "v": 1, "id": "req-7",
            "error": {
                "code": "busy",
                "message": "a session is already running",
                "retry_after_ms": 500
            }
        }),
    );
}

#[test]
fn golden_envelope_event() {
    pin(
        "event",
        Message::event(Event::StateChanged {
            session_id: "s1".into(),
            from: State::Idle,
            to: State::Recording,
            at_ms: Some(1_700_000_000_000u64),
        }),
        json!({
            "kind": "event", "v": 1,
            "event": {
                "type": "state_changed",
                "session_id": "s1",
                "from": "idle",
                "to": "recording",
                "at_ms": 1_700_000_000_000u64
            }
        }),
    );
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[test]
fn golden_command_lifecycle() {
    pin("toggle", Command::Toggle, json!({"type": "toggle"}));
    pin("stop", Command::Stop, json!({"type": "stop"}));
    pin("cancel", Command::Cancel, json!({"type": "cancel"}));
    pin("get_status", Command::GetStatus, json!({"type": "get_status"}));
    pin(
        "get_history_analytics",
        Command::GetHistoryAnalytics,
        json!({"type": "get_history_analytics"}),
    );
    pin(
        "purge_history",
        Command::PurgeHistory,
        json!({"type": "purge_history"}),
    );
    pin(
        "unsubscribe",
        Command::Unsubscribe,
        json!({"type": "unsubscribe"}),
    );
    pin(
        "subscribe",
        Command::Subscribe {
            events: vec!["final".into(), "state_changed".into()],
        },
        json!({"type": "subscribe", "events": ["final", "state_changed"]}),
    );
}

#[test]
fn golden_command_start_dictation() {
    pin(
        "start_dictation",
        Command::StartDictation {
            mode: DictationMode::PushToTalk,
            options: Some(SessionOptions {
                route: Some(Route::Type),
                inject: Some(true),
                format_llm: None,
                use_dictionary: None,
                language: Some("en".into()),
                app: Some("code".into()),
                privacy: None,
            }),
        },
        json!({
            "type": "start_dictation",
            "mode": "push_to_talk",
            "options": {"route": "type", "inject": true, "language": "en", "app": "code"}
        }),
    );
}

#[test]
fn golden_command_handshake() {
    pin(
        "handshake",
        Command::Handshake(Hello {
            protocol_version: 1,
            supported_versions: vec![1],
            client: ClientInfo {
                name: "dictate-cli".into(),
                version: Some("0.2.0".into()),
                kind: ClientKind::Cli,
            },
        }),
        json!({
            "type": "handshake",
            "protocol_version": 1,
            "supported_versions": [1],
            "client": {"name": "dictate-cli", "version": "0.2.0", "kind": "cli"}
        }),
    );
}

#[test]
fn golden_command_transcribe_audio_inline() {
    pin(
        "transcribe_audio/inline",
        Command::TranscribeAudio {
            audio: AudioSource::Inline {
                format: AudioFormat::whisper_native(),
                data: vec![0, 1, 2, 3],
            },
            options: None,
        },
        json!({
            "type": "transcribe_audio",
            "audio": {
                "source": "inline",
                "format": {"encoding": "pcm_f32le", "sample_rate_hz": 16000, "channels": 1},
                "data": "AAECAw=="
            }
        }),
    );
}

#[test]
fn golden_command_transcribe_audio_body() {
    // The `POST /v1/transcribe` shape: audio is the HTTP body, so the JSON
    // carries parameters only.
    pin(
        "transcribe_audio/body",
        Command::TranscribeAudio {
            audio: AudioSource::Body {
                format: AudioFormat::wav(),
            },
            options: Some(SessionOptions::transcription_only()),
        },
        json!({
            "type": "transcribe_audio",
            "audio": {"source": "body", "format": {"encoding": "wav"}},
            "options": {"inject": false}
        }),
    );
}

#[test]
fn golden_command_audio_stream() {
    pin(
        "begin_audio_stream",
        Command::BeginAudioStream {
            format: AudioFormat::whisper_native(),
            options: None,
        },
        json!({
            "type": "begin_audio_stream",
            "format": {"encoding": "pcm_f32le", "sample_rate_hz": 16000, "channels": 1}
        }),
    );
    pin(
        "end_audio_stream",
        Command::EndAudioStream { stream_id: 3 },
        json!({"type": "end_audio_stream", "stream_id": 3}),
    );
}

#[test]
fn golden_command_crud() {
    pin(
        "get_config",
        Command::GetConfig {
            path: Some("whisper.model".into()),
        },
        json!({"type": "get_config", "path": "whisper.model"}),
    );
    pin(
        "set_config",
        Command::SetConfig {
            entries: vec![ConfigEntry::new("whisper.model", json!("large-v3-turbo"))],
        },
        json!({
            "type": "set_config",
            "entries": [{"path": "whisper.model", "value": "large-v3-turbo"}]
        }),
    );
    pin(
        "upsert_dictionary_entry",
        Command::UpsertDictionaryEntry {
            entry: DictionaryEntry {
                id: Some(4),
                phrase: "Kubernetes".into(),
                sounds_like: vec!["kubernetties".into()],
                case_sensitive: false,
                enabled: true,
                source: EntrySource::Manual,
                hit_count: Some(12),
            },
        },
        json!({
            "type": "upsert_dictionary_entry",
            "entry": {
                "id": 4,
                "phrase": "Kubernetes",
                "sounds_like": ["kubernetties"],
                "case_sensitive": false,
                "enabled": true,
                "source": "manual",
                "hit_count": 12
            }
        }),
    );
    pin(
        "delete_snippet",
        Command::DeleteSnippet { id: 9 },
        json!({"type": "delete_snippet", "id": 9}),
    );
    pin(
        "query_history",
        Command::QueryHistory {
            query: HistoryQuery {
                text: Some("kubernetes".into()),
                limit: Some(50),
                ..Default::default()
            },
        },
        json!({
            "type": "query_history",
            "query": {"text": "kubernetes", "limit": 50, "order": "desc"}
        }),
    );
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// The plan specifies `Final{text, route, timings}`; this pins that those are
/// top-level fields rather than nested under a `transcript` object.
#[test]
fn golden_event_final() {
    pin(
        "event/final",
        Event::Final {
            session_id: "s1".into(),
            transcript: Box::new(transcript()),
        },
        json!({
            "type": "final",
            "session_id": "s1",
            "text": "Hello there.",
            "raw_text": "hello there",
            "route": "type",
            "timings": full_timings_json(),
            "injection": {"status": "injected", "method": "paste", "chars": 12},
            "word_count": 2,
            "model": "large-v3-turbo"
        }),
    );
}

/// Pins the field name `hypothesis`. Renaming it to `text` would make a partial
/// indistinguishable from an injectable final in every non-Rust client, so this
/// assertion is a correctness guard, not a style preference.
#[test]
fn golden_event_partial() {
    pin(
        "event/partial",
        Event::Partial {
            session_id: "s1".into(),
            seq: 2,
            hypothesis: "hello ther".into(),
            at_ms: None,
        },
        json!({
            "type": "partial",
            "session_id": "s1",
            "seq": 2,
            "hypothesis": "hello ther"
        }),
    );
}

#[test]
fn golden_event_injection_resolved() {
    pin(
        "event/injection_resolved",
        Event::InjectionResolved {
            session_id: "s1".into(),
            outcome: InjectionOutcome::ConsentDenied {
                backend: "portal".into(),
                detail: Some("user dismissed the dialog".into()),
            },
        },
        json!({
            "type": "injection_resolved",
            "session_id": "s1",
            "outcome": {
                "status": "consent_denied",
                "backend": "portal",
                "detail": "user dismissed the dialog"
            }
        }),
    );
}

#[test]
fn golden_event_error_and_audio_level() {
    pin(
        "event/error",
        Event::Error {
            session_id: Some("s1".into()),
            error: ProtoError::new(ErrorCode::SttFailed, "model returned no segments"),
        },
        json!({
            "type": "error",
            "session_id": "s1",
            "error": {"code": "stt_failed", "message": "model returned no segments"}
        }),
    );
    pin(
        "event/audio_level",
        Event::AudioLevel {
            session_id: "s1".into(),
            rms: 0.25,
            peak: Some(0.75),
            at_ms: None,
        },
        json!({"type": "audio_level", "session_id": "s1", "rms": 0.25, "peak": 0.75}),
    );
}

// ---------------------------------------------------------------------------
// Injection outcomes — the R3 forward-compat surface
// ---------------------------------------------------------------------------

#[test]
fn golden_injection_outcomes() {
    pin(
        "injection/injected",
        InjectionOutcome::Injected {
            method: InjectMethod::Keystroke,
            chars: 40,
        },
        json!({"status": "injected", "method": "keystroke", "chars": 40}),
    );
    pin(
        "injection/awaiting_consent",
        InjectionOutcome::AwaitingConsent {
            backend: "portal".into(),
            consent_id: Some("req-7".into()),
        },
        json!({"status": "awaiting_consent", "backend": "portal", "consent_id": "req-7"}),
    );
    pin(
        "injection/unavailable",
        InjectionOutcome::Unavailable {
            backend: "none".into(),
            reason: "no display server".into(),
        },
        json!({"status": "unavailable", "backend": "none", "reason": "no display server"}),
    );
    pin(
        "injection/delivered",
        InjectionOutcome::Delivered,
        json!({"status": "delivered"}),
    );
    pin(
        "injection/skipped",
        InjectionOutcome::Skipped {
            reason: SkipReason::NotPermitted,
        },
        json!({"status": "skipped", "reason": "not_permitted"}),
    );
}

// ---------------------------------------------------------------------------
// Timings
// ---------------------------------------------------------------------------

#[test]
fn golden_stage_timings() {
    pin("timings/full", full_timings(), full_timings_json());
    pin(
        "timings/ran_zero",
        StageTiming::ran(0.0),
        json!({"status": "ran", "ms": 0.0}),
    );
    pin(
        "timings/not_reported",
        StageTiming::NotReported,
        json!({"status": "not_reported"}),
    );
    pin(
        "timings/failed",
        StageTiming::Failed {
            ms: 3000.0,
            error: Some("connection refused".into()),
        },
        json!({"status": "failed", "ms": 3000.0, "error": "connection refused"}),
    );
}

// ---------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------

#[test]
fn golden_result_transcript() {
    pin(
        "result/transcript",
        CommandResult::Transcript(Box::new(transcript())),
        json!({
            "type": "transcript",
            "text": "Hello there.",
            "raw_text": "hello there",
            "route": "type",
            "timings": full_timings_json(),
            "injection": {"status": "injected", "method": "paste", "chars": 12},
            "word_count": 2,
            "model": "large-v3-turbo"
        }),
    );
}

#[test]
fn golden_result_handshake_capabilities() {
    pin(
        "result/handshake",
        CommandResult::Handshake(Box::new(ServerHello {
            protocol_version: 1,
            supported_versions: vec![1],
            server: ServerInfo::new("dictated", "0.2.0"),
            capabilities: Capabilities::remote_transcription_only(),
        })),
        json!({
            "type": "handshake",
            "protocol_version": 1,
            "supported_versions": [1],
            "server": {"name": "dictated", "version": "0.2.0"},
            "capabilities": {
                "features": {
                    "text_injection": false,
                    "partial_transcripts": false,
                    "audio_level_events": true,
                    "streaming_audio": true,
                    "transcribe_upload": true,
                    "host_capture": false,
                    "history_read": false,
                    "history_write": false,
                    "dictionary_read": false,
                    "dictionary_write": false,
                    "snippets_read": false,
                    "snippets_write": false,
                    "config_read": false,
                    "config_write": false,
                    "wake_word": false,
                    "headless": false,
                    "privacy_mode": false
                },
                "audio_formats": ["pcm_f32le", "pcm_s16le", "wav"],
                "routes": ["type"],
                "limits": {
                    "max_message_bytes": 1_048_576,
                    "max_audio_frame_bytes": 1_048_576,
                    "max_concurrent_sessions": 1
                }
            }
        }),
    );
}

#[test]
fn golden_result_simple_variants() {
    pin("result/ack", CommandResult::Ack, json!({"type": "ack"}));
    pin(
        "result/session_started",
        CommandResult::SessionStarted {
            session_id: "s1".into(),
        },
        json!({"type": "session_started", "session_id": "s1"}),
    );
    pin(
        "result/deleted",
        CommandResult::Deleted { id: 9 },
        json!({"type": "deleted", "id": 9}),
    );
    pin(
        "result/audio_stream_opened",
        CommandResult::AudioStreamOpened {
            stream_id: 3,
            session_id: "s1".into(),
        },
        json!({"type": "audio_stream_opened", "stream_id": 3, "session_id": "s1"}),
    );
}

// ---------------------------------------------------------------------------
// Enum vocabularies
// ---------------------------------------------------------------------------

/// Pins every wire string of every open enum. These strings are the vocabulary
/// S32 and S33 hard-code; changing one silently breaks them.
#[test]
fn golden_enum_vocabularies() {
    let cases: Vec<(&str, Vec<String>, &[&str])> = vec![
        (
            "State",
            State::known().iter().map(State::to_string).collect(),
            &[
                "idle",
                "recording",
                "transcribing",
                "formatting",
                "injecting",
                "done",
                "error",
                "cancelled",
            ],
        ),
        (
            "Route",
            Route::known().iter().map(Route::to_string).collect(),
            &["type", "timer", "local", "edit", "command"],
        ),
        (
            "DictationMode",
            DictationMode::known()
                .iter()
                .map(DictationMode::to_string)
                .collect(),
            &["toggle", "push_to_talk", "one_shot", "wake_word"],
        ),
        (
            "InjectMethod",
            InjectMethod::known()
                .iter()
                .map(InjectMethod::to_string)
                .collect(),
            &["paste", "keystroke", "input_method"],
        ),
        (
            "AudioEncoding",
            AudioEncoding::known()
                .iter()
                .map(AudioEncoding::to_string)
                .collect(),
            &["pcm_f32le", "pcm_s16le", "wav"],
        ),
        (
            "SkipReason",
            SkipReason::known()
                .iter()
                .map(SkipReason::to_string)
                .collect(),
            &[
                "disabled",
                "below_min_words",
                "route_not_eligible",
                "not_supported",
                "no_speech_detected",
                "not_permitted",
                "dependency_unavailable",
                "cancelled",
            ],
        ),
        (
            "EntrySource",
            EntrySource::known()
                .iter()
                .map(EntrySource::to_string)
                .collect(),
            &["manual", "auto_learned", "builtin", "synced"],
        ),
        (
            "SortOrder",
            SortOrder::known().iter().map(SortOrder::to_string).collect(),
            &["desc", "asc"],
        ),
        (
            "ClientKind",
            ClientKind::known()
                .iter()
                .map(ClientKind::to_string)
                .collect(),
            &["cli", "desktop_ui", "remote", "automation"],
        ),
    ];

    for (name, actual, expected) in cases {
        assert_eq!(
            actual, expected,
            "{name}: wire vocabulary changed — this breaks S32/S33"
        );
    }
}

/// Every error code's wire string, pinned. S33 maps these to HTTP statuses.
#[test]
fn golden_error_code_vocabulary() {
    let actual: Vec<String> = ErrorCode::known().iter().map(ErrorCode::to_string).collect();
    assert_eq!(
        actual,
        [
            "unsupported_version",
            "unsupported_command",
            "malformed_request",
            "invalid_params",
            "handshake_required",
            "unauthorized",
            "forbidden",
            "rate_limited",
            "payload_too_large",
            "capability_unavailable",
            "consent_required",
            "consent_denied",
            "busy",
            "invalid_state",
            "no_active_session",
            "cancelled",
            "timeout",
            "audio_device_error",
            "audio_format_unsupported",
            "stt_failed",
            "model_unavailable",
            "formatting_failed",
            "injection_failed",
            "history_error",
            "config_invalid",
            "not_found",
            "conflict",
            "internal",
        ]
    );
}

/// The binary frame layout, pinned byte for byte. Changing it requires a
/// [`FRAME_VERSION`](dictate_proto::frame::FRAME_VERSION) bump, not an edit
/// here.
#[test]
fn golden_binary_frame_layout() {
    let frame = AudioFrame {
        kind: FrameKind::Audio,
        flags: 0,
        stream_id: 1,
        seq: 0,
        payload: vec![0x11, 0x22, 0x33, 0x44],
    };
    assert_eq!(
        frame.encode(),
        vec![
            b'D', b'C', b'T', b'A', // magic
            1,    // frame version
            1,    // kind = audio
            0x00, 0x00, // flags
            0x01, 0x00, 0x00, 0x00, // stream_id = 1 (LE)
            0x00, 0x00, 0x00, 0x00, // seq = 0 (LE)
            0x04, 0x00, 0x00, 0x00, // payload_len = 4 (LE)
            0x11, 0x22, 0x33, 0x44, // payload
        ],
        "binary frame layout changed — bump FRAME_VERSION"
    );
    assert_eq!(dictate_proto::frame::FRAME_HEADER_LEN, 20);
    assert_eq!(dictate_proto::frame::FRAME_VERSION, 1);
    assert_eq!(PROTOCOL_VERSION, 1);
}
