//! Forward- and backward-compatibility tests.
//!
//! Each test here simulates a **version skew** — a v1 peer meeting a payload
//! from a hypothetical v1.1 that added something, or a v1.1 peer meeting a
//! payload from v1 that omits something. Together they are the executable
//! statement of the compatibility rule in the crate docs.
//!
//! The rule, restated:
//!
//! - unknown **fields** are ignored, everywhere;
//! - unknown **open-enum values** round-trip losslessly;
//! - unknown **events and results** degrade to `Unknown` rather than failing;
//! - unknown **commands** fail loudly, because a dropped request strands the
//!   caller;
//! - omitted optional fields take defaults that preserve the old behavior, and
//!   for capabilities that default is always "not permitted".

use dictate_proto::*;

// ---------------------------------------------------------------------------
// Unknown fields are ignored everywhere
// ---------------------------------------------------------------------------

/// A v1.1 daemon adds fields to a message a v1 client already understands. The
/// client must read the fields it knows and ignore the rest.
#[test]
fn newer_peer_may_add_fields_to_any_existing_message() {
    let cases = [
        r#"{"type":"stop","force":true}"#,
        r#"{"type":"start_dictation","mode":"toggle","priority":"high"}"#,
        r#"{"type":"get_config","path":"a.b","include_defaults":true}"#,
    ];
    for json in cases {
        serde_json::from_str::<Command>(json)
            .unwrap_or_else(|e| panic!("must tolerate added fields in {json}: {e}"));
    }

    // ...and the same on the event and result paths.
    let e: Event = serde_json::from_str(
        r#"{"type":"state_changed","session_id":"s","from":"idle","to":"recording",
            "reason":"hotkey","device":"hw:1"}"#,
    )
    .unwrap();
    assert!(matches!(
        e,
        Event::StateChanged { ref to, .. } if *to == State::Recording
    ));

    let r: CommandResult = serde_json::from_str(
        r#"{"type":"session_started","session_id":"s","queue_position":0}"#,
    )
    .unwrap();
    assert_eq!(
        r,
        CommandResult::SessionStarted {
            session_id: "s".into()
        }
    );
}

/// No type in this crate may use `deny_unknown_fields`; a nested addition deep
/// in the tree must be tolerated too.
#[test]
fn added_fields_are_tolerated_at_every_nesting_depth() {
    let json = r#"{
        "type": "final",
        "session_id": "s1",
        "text": "hi",
        "route": "type",
        "injection": {"status": "delivered", "latency_ms": 4},
        "timings": {
            "stt": {"status": "ran", "ms": 300.0, "gpu_util": 0.8},
            "diarization": {"status": "ran", "ms": 12.0}
        },
        "confidence": 0.97
    }"#;
    let e: Event = serde_json::from_str(json).unwrap();
    let Event::Final { transcript, .. } = e else {
        panic!("expected a final event");
    };
    assert_eq!(transcript.text.as_str(), "hi");
    assert_eq!(transcript.timings.stt, StageTiming::ran(300.0));
    // The stage this build has never heard of is simply not represented.
    assert_eq!(transcript.timings.fmt_llm, StageTiming::NotReported);
    assert_eq!(transcript.injection, InjectionOutcome::Delivered);
}

// ---------------------------------------------------------------------------
// Unknown open-enum values round-trip losslessly
// ---------------------------------------------------------------------------

/// The property that lets a v1 relay or logger sit between two v1.1 peers
/// without corrupting their traffic.
#[test]
fn unknown_open_enum_values_survive_a_round_trip_through_this_build() {
    let cases = [
        (r#""dictating""#, "State"),
        (r#""telepathy""#, "Route"),
        (r#""hum""#, "DictationMode"),
        (r#""opus""#, "AudioEncoding"),
    ];
    // Each parsed as its own type, re-emitted, and compared byte for byte.
    let s: State = serde_json::from_str(cases[0].0).unwrap();
    assert_eq!(serde_json::to_string(&s).unwrap(), cases[0].0);
    assert!(!s.is_known());

    let r: Route = serde_json::from_str(cases[1].0).unwrap();
    assert_eq!(serde_json::to_string(&r).unwrap(), cases[1].0);

    let m: DictationMode = serde_json::from_str(cases[2].0).unwrap();
    assert_eq!(serde_json::to_string(&m).unwrap(), cases[2].0);

    let a: AudioEncoding = serde_json::from_str(cases[3].0).unwrap();
    assert_eq!(serde_json::to_string(&a).unwrap(), cases[3].0);
    assert_eq!(a.bytes_per_sample(), None, "must not guess a sample width");
}

/// A whole message carrying several unknown enum values must survive intact —
/// this is the realistic relay case.
#[test]
fn a_message_full_of_unknown_enum_values_relays_unchanged() {
    let original = r#"{"kind":"event","v":1,"event":{"type":"state_changed","session_id":"s","from":"resampling","to":"diarizing"}}"#;
    let m = Message::parse(original).unwrap();
    assert_eq!(serde_json::to_string(&m).unwrap(), original);
}

#[test]
fn an_unknown_error_code_is_still_actionable() {
    let e: ProtoError =
        serde_json::from_str(r#"{"code":"gpu_fell_over","message":"oh no"}"#).unwrap();
    assert!(!e.code.is_known());
    // A client can still show the message, log the code, and pick a status.
    assert_eq!(e.to_string(), "[gpu_fell_over] oh no");
    assert_eq!(e.code.http_status(), 500);
    assert!(!e.is_retryable(), "unknown codes must not be retried blindly");
}

// ---------------------------------------------------------------------------
// The command/event asymmetry
// ---------------------------------------------------------------------------

/// The asymmetry, asserted side by side: the same unknown type string degrades
/// on the event path and fails on the command path.
#[test]
fn unknown_degrades_on_the_event_path_and_fails_on_the_command_path() {
    let unknown_type = r#"{"type":"summon_kraken","tentacles":8}"#;

    // Event path: a client must survive a newer daemon.
    let e: Event = serde_json::from_str(unknown_type)
        .expect("an unknown event must degrade, not fail");
    assert_eq!(e, Event::Unknown);

    // Result path: likewise.
    let r: CommandResult = serde_json::from_str(unknown_type)
        .expect("an unknown result must degrade, not fail");
    assert_eq!(r, CommandResult::Unknown);

    // Command path: a server must NOT silently drop a request, because the
    // caller is waiting for an effect that would never happen.
    assert!(
        serde_json::from_str::<Command>(unknown_type).is_err(),
        "an unknown command must fail so the server can answer UnsupportedCommand"
    );

    // And the error the server owes the caller is well-defined.
    let err = ProtoError::unsupported_command("summon_kraken");
    assert_eq!(err.code, ErrorCode::UnsupportedCommand);
    assert_eq!(err.code.http_status(), 501);
}

/// An unknown command inside an envelope must surface as a protocol error the
/// server can turn into a response, not as a dropped message.
#[test]
fn an_unknown_command_in_an_envelope_produces_a_reportable_error() {
    let e = Message::parse(
        r#"{"kind":"request","v":1,"id":7,"command":{"type":"summon_kraken"}}"#,
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::MalformedRequest);
    // The server still knows the id from the raw JSON and can answer id 7.
    let raw: serde_json::Value = serde_json::from_str(
        r#"{"kind":"request","v":1,"id":7,"command":{"type":"summon_kraken"}}"#,
    )
    .unwrap();
    assert_eq!(raw["id"], 7);
}

// ---------------------------------------------------------------------------
// Omitted fields take behavior-preserving defaults
// ---------------------------------------------------------------------------

/// A v1 peer's payload, read by a build that has since added optional fields:
/// every default must preserve the old behavior.
#[test]
fn omitted_optional_fields_default_to_the_previous_behavior() {
    // A minimal session start still means "toggle", as it always did.
    let c: Command = serde_json::from_str(r#"{"type":"start_dictation"}"#).unwrap();
    assert_eq!(
        c,
        Command::StartDictation {
            mode: DictationMode::Toggle,
            options: None
        }
    );

    // A dictionary entry with no `enabled` is enabled, not silently disabled.
    let d: DictionaryEntry = serde_json::from_str(r#"{"phrase":"Kubernetes"}"#).unwrap();
    assert!(d.enabled);

    // A history query with no order is newest-first.
    let q: HistoryQuery = serde_json::from_str("{}").unwrap();
    assert_eq!(q.order, SortOrder::Descending);

    // Session options omit rather than assert: None means "server default",
    // never "off".
    let o: SessionOptions = serde_json::from_str("{}").unwrap();
    assert_eq!(o.format_llm, None, "None must mean 'use the configured default'");
    assert_eq!(o.inject, None);
}

/// The one direction that must never be permissive: an unspecified capability
/// is a *denied* capability.
#[test]
fn omitted_capabilities_fail_safe_to_denied() {
    let caps: Capabilities = serde_json::from_str("{}").unwrap();
    assert!(!caps.features.text_injection);
    assert!(!caps.features.config_write);
    assert!(!caps.features.host_capture);

    // A newer daemon advertising unknown capabilities must not accidentally
    // grant the ones this build checks.
    let caps: Capabilities = serde_json::from_str(
        r#"{"features":{"mind_reading":true,"quantum_entanglement":true}}"#,
    )
    .unwrap();
    assert!(!caps.features.text_injection);
    assert!(
        !Command::SetConfig { entries: vec![] }.is_permitted(&caps.features),
        "an unknown capability must never imply a known one"
    );
}

/// Timings must never fabricate a zero for a stage nobody reported.
#[test]
fn omitted_timings_read_as_unreported_not_as_zero() {
    let t: StageTimings = serde_json::from_str("{}").unwrap();
    for (name, stage) in t.stages() {
        assert_eq!(stage, &StageTiming::NotReported, "{name}");
        assert_eq!(stage.elapsed_ms(), None, "{name} must not report 0ms");
    }
}

// ---------------------------------------------------------------------------
// Version negotiation
// ---------------------------------------------------------------------------

#[test]
fn a_newer_client_is_refused_with_a_specific_diagnosable_code() {
    let e = Message::parse(r#"{"kind":"request","v":2,"id":1,"command":{"type":"stop"}}"#)
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::UnsupportedVersion);
    assert_eq!(e.code.http_status(), 501);
    assert!(
        e.message.contains('2'),
        "the client deserves to be told which version was refused: {}",
        e.message
    );
}

#[test]
fn a_client_offering_several_versions_negotiates_down() {
    // A v1.1 client that can also speak v1 must be accepted at v1.
    assert_eq!(negotiate_version(&[1, 2, 3]), Some(1));
    // A client that cannot speak v1 at all is refused, not guessed at.
    assert_eq!(negotiate_version(&[7, 8]), None);
}

#[test]
fn a_hello_without_a_version_list_still_negotiates() {
    let h: Hello =
        serde_json::from_str(r#"{"protocol_version":1,"client":{"name":"phone"}}"#).unwrap();
    assert_eq!(negotiate_version(&h.versions()), Some(1));
    assert_eq!(h.client.kind, ClientKind::Automation, "the safe default");
}

// ---------------------------------------------------------------------------
// Binary frames
// ---------------------------------------------------------------------------

/// The binary analog of the open enum: an explicit length lets a decoder skip a
/// frame kind it does not understand without losing sync.
#[test]
fn an_unknown_frame_kind_does_not_desynchronize_the_stream() {
    let mut buf = Vec::new();
    let mut exotic = AudioFrame::audio(1, 0, vec![0xAA; 16]);
    exotic.kind = FrameKind::Unknown(200);
    exotic.encode_into(&mut buf);
    AudioFrame::audio(1, 1, vec![0xBB; 8]).encode_into(&mut buf);
    AudioFrame::end(1, 2).encode_into(&mut buf);

    let (frames, used) = AudioFrame::decode_all(&buf).unwrap();
    assert_eq!(used, buf.len(), "the whole buffer must be consumed");
    assert_eq!(frames.len(), 3);
    assert!(!frames[0].kind.is_known());
    assert_eq!(frames[1].payload, vec![0xBB; 8], "sync was preserved");
    assert!(frames[2].kind.is_terminal());
}

#[test]
fn a_future_frame_version_is_refused_rather_than_misparsed() {
    let mut b = AudioFrame::audio(1, 0, vec![1, 2, 3]).encode();
    b[4] = 2;
    let err = AudioFrame::decode(&b).unwrap_err();
    assert_eq!(err, FrameError::UnsupportedVersion { found: 2 });
    // ...and it maps onto a protocol error the server can send back.
    let pe: ProtoError = err.into();
    assert_eq!(pe.code, ErrorCode::UnsupportedVersion);
}
