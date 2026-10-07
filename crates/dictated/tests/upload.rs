//! `transcribe_audio` over the local control socket, end to end.
//!
//! A real daemon, real socket, real engine and real decoder; only the four
//! things CI cannot have (microphone, GPU, Ollama, a desktop) are doubles. What
//! these tests pin, by the risk each covers:
//!
//! 1. **the happy path** — a WAV goes in, the response *is* the transcript
//! 2. **side effects** — an upload never touches the microphone, media player or
//!    earcons, and never types unless it asks *and* is allowed
//! 3. **request errors** — oversized, malformed and unsupported audio are precise
//!    protocol errors that never occupy the engine's session slot
//! 4. **concurrency** — busy, cancel, and a caller that hangs up mid-request
//! 5. **privacy** — an upload honors the same no-persistence rules as dictation
//! 6. **audio-less mode** — no input device, no recording, uploads still work

mod harness;

use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use dictate_core::ports::mock::{
    ActiveMedia, Gate, MockAudio, MockInjector, MockStt, MockVad, RecordingEarcons,
};
use dictate_core::ports::{AudioSource, DisabledAudioSource};
use dictate_core::ports::{GateDecision, TrailingSilenceTracker, VoiceActivityGate};
use dictate_proto::{
    AudioEncoding, AudioFormat, AudioSource as Upload, Capabilities, Command, CommandResult,
    ErrorCode, Event, InjectionOutcome, Message, RequestId, SessionOptions, StageTiming, State,
    Transcript,
};
use harness::{Client, Harness, Setup};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A WAV of `secs` seconds of a quiet tone at `rate`/`channels`, 16-bit PCM.
fn wav(rate: u32, channels: u16, secs: f64) -> Vec<u8> {
    let spec = hound::WavSpec {
        channels,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut buf = Cursor::new(Vec::new());
    {
        let mut w = hound::WavWriter::new(&mut buf, spec).unwrap();
        let frames = (f64::from(rate) * secs) as usize;
        for i in 0..frames {
            let s = ((i as f64 * 440.0 * std::f64::consts::TAU / f64::from(rate)).sin() * 3000.0)
                as i16;
            for _ in 0..channels {
                w.write_sample(s).unwrap();
            }
        }
        w.finalize().unwrap();
    }
    buf.into_inner()
}

fn upload_wav(bytes: Vec<u8>) -> Upload {
    Upload::Inline {
        format: AudioFormat::wav(),
        data: bytes,
    }
}

fn transcribe(bytes: Vec<u8>) -> Command {
    Command::TranscribeAudio {
        audio: upload_wav(bytes),
        options: None,
    }
}

fn transcribe_with(bytes: Vec<u8>, options: SessionOptions) -> Command {
    Command::TranscribeAudio {
        audio: upload_wav(bytes),
        options: Some(options),
    }
}

fn expect_transcript(result: CommandResult) -> Transcript {
    match result {
        CommandResult::Transcript(t) => *t,
        other => panic!("expected a transcript, got {other:?}"),
    }
}

/// Drain events until the daemon returns to idle, keeping the `audio_activity`
/// ones.
async fn audio_activity_until_idle(client: &mut Client) -> Vec<Event> {
    let mut activity = Vec::new();
    loop {
        match client.next_event().await {
            Event::StateChanged {
                to: State::Idle, ..
            } => return activity,
            e @ Event::AudioActivity { .. } => activity.push(e),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// 1. The happy path
// ---------------------------------------------------------------------------

/// Approximate the real CPU VAD + GPU STT stage times without either model.
struct DelayedVad(MockVad);

impl VoiceActivityGate for DelayedVad {
    fn gate(&self, samples: &[f32]) -> anyhow::Result<GateDecision> {
        std::thread::sleep(Duration::from_millis(20));
        self.0.gate(samples)
    }

    fn trailing_silence_tracker(&self) -> anyhow::Result<Box<dyn TrailingSilenceTracker>> {
        self.0.trailing_silence_tracker()
    }

    fn enabled(&self) -> bool {
        self.0.enabled()
    }

    fn poll_interval_ms(&self) -> u32 {
        self.0.poll_interval_ms()
    }
}

fn delayed_upload_setup() -> (Setup, Arc<DelayedVad>) {
    let vad = Arc::new(DelayedVad(MockVad::returning(GateDecision::Speech {
        samples: vec![0.1; 16_000],
        leading_trimmed_ms: 0.0,
        trailing_trimmed_ms: 0.0,
    })));
    let setup = Setup::default()
        .with_stt(Arc::new(
            MockStt::returning("hello there").with_delay(Duration::from_millis(130)),
        ))
        .with_vad(vad.clone());
    (setup, vad)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeated_uploads_survive_events_between_request_fragments() {
    let (setup, vad) = delayed_upload_setup();
    let h = Harness::with(setup).await;
    let mut client = h.client().await;
    // Receiving the marker below proves the event arm interrupted the reader.
    client.subscribe().await;
    let audio = wav(16_000, 1, 8.0);
    for n in 0..6 {
        // Like e2e_real: one connection, wait for each transcript, then repeat.
        expect_transcript(client.request(transcribe(audio.clone())).await.unwrap());
        let id = RequestId::Text(format!("fragmented-{n}"));
        let line = Message::request(id.clone(), transcribe(audio.clone()))
            .to_ndjson_line()
            .unwrap();
        // Larger than the socket send buffer: finishing this write requires
        // the server to consume an incomplete frame before the event arrives.
        let split = line.len() - 1024;
        client.write_raw(&line.as_bytes()[..split]).await;
        h.daemon.engine().bus().publish(Event::Error {
            session_id: None,
            error: dictate_proto::ProtoError::new(ErrorCode::Internal, "framing marker"),
        });
        loop {
            if matches!(client.next_event().await, Event::Error { error, .. }
                if error.message == "framing marker")
            {
                break;
            }
        }
        client.write_raw(&line.as_bytes()[split..]).await;
        assert_eq!(client.read_response_ids(1).await, [id]);
    }
    assert_eq!(vad.0.gate_count(), 12);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_response_to_an_upload_is_its_transcript() {
    let h = Harness::start().await;
    let mut client = h.client().await;

    // 48 kHz stereo on purpose: the decoder, not the fixture, must do the
    // conversion to what Whisper consumes.
    let t = expect_transcript(
        client
            .request(transcribe(wav(48_000, 2, 1.0)))
            .await
            .unwrap(),
    );

    assert_eq!(t.text.as_str(), "Hello there");
    assert_eq!(t.route, dictate_proto::Route::Type);
    assert_eq!(t.word_count, Some(2));
    assert_eq!(
        t.injection,
        InjectionOutcome::Delivered,
        "an upload hands text back; it does not type it"
    );
    assert!(h.injector.injected().is_empty());

    // The stages an upload really ran are `ran`; the audio-acquisition stage
    // reports the decode + resample cost rather than a fabricated zero.
    assert!(
        matches!(t.timings.capture, StageTiming::Ran { .. }),
        "{:?}",
        t.timings.capture
    );
    assert!(matches!(t.timings.vad, StageTiming::Ran { .. }));
    assert!(matches!(t.timings.stt, StageTiming::Ran { .. }));
    // The documented invariant: the measured total is at least the sum of the
    // stages (the gap is scheduling). Decoding happens before the session
    // starts, so the total must include the `capture` stage explicitly.
    let total = t.timings.total_ms.expect("total is reported");
    assert!(
        total >= t.timings.measured_ms() - 1e-6,
        "total {total} ms is less than the stages' {} ms",
        t.timings.measured_ms()
    );
    assert!(
        t.timings
            .audio_ms
            .is_some_and(|ms| (ms - 1000.0).abs() < 5.0),
        "{:?}",
        t.timings.audio_ms
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_upload_walks_the_pipeline_without_a_recording_state() {
    let h = Harness::start().await;
    let mut watcher = h.client().await;
    watcher.subscribe().await;
    let mut client = h.client().await;

    client
        .request(transcribe(wav(16_000, 1, 1.0)))
        .await
        .unwrap();
    watcher.wait_for_state(State::Idle).await;

    watcher.assert_walked(&[
        State::Transcribing,
        State::Formatting,
        State::Injecting,
        State::Done,
        State::Idle,
    ]);
    assert!(
        !watcher.seen_states().contains(&State::Recording),
        "no microphone was involved, so no recording state: {:?}",
        watcher.seen_states()
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn raw_pcm_uploads_work_at_the_declared_layout() {
    let h = Harness::start().await;
    let mut client = h.client().await;
    let bytes: Vec<u8> = (0..16_000i16)
        .flat_map(|i| (i % 100).to_le_bytes())
        .collect();
    let t = expect_transcript(
        client
            .request(Command::TranscribeAudio {
                audio: Upload::Inline {
                    format: AudioFormat {
                        encoding: AudioEncoding::PcmS16Le,
                        sample_rate_hz: Some(16_000),
                        channels: Some(1),
                    },
                    data: bytes,
                },
                options: None,
            })
            .await
            .unwrap(),
    );
    assert_eq!(t.text.as_str(), "Hello there");
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn back_to_back_uploads_do_not_race_the_slot_release() {
    // The engine frees its slot *before* answering, so a client that sends its
    // next request the instant it has a transcript must never see `busy`.
    let (setup, vad) = delayed_upload_setup();
    let h = Harness::with(setup).await;
    let mut client = h.client().await;
    // Mirror e2e_real's loop: three sizes, one warm-up plus eight repeats,
    // on one connection without subscribing to events.
    for secs in [1.0, 4.0, 8.0] {
        let audio = wav(16_000, 1, secs);
        for _ in 0..9 {
            let transcript =
                expect_transcript(client.request(transcribe(audio.clone())).await.unwrap());
            assert_eq!(transcript.text.as_str(), "Hello there");
        }
    }
    assert_eq!(vad.0.gate_count(), 27);
    h.stop().await;
}

// ---------------------------------------------------------------------------
// 2. Side effects
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_upload_never_touches_the_microphone_media_or_earcons() {
    let audio = Arc::new(MockAudio::with_seconds(1.0));
    let h = Harness::with(
        Setup::default()
            .with_audio(audio.clone())
            .with_audio_side_effects(Arc::new(ActiveMedia), Arc::new(RecordingEarcons)),
    )
    .await;
    let mut watcher = h.client().await;
    watcher.subscribe().await;
    let mut client = h.client().await;

    client
        .request(transcribe(wav(16_000, 1, 1.0)))
        .await
        .unwrap();

    let activity = audio_activity_until_idle(&mut watcher).await;
    assert!(
        activity.is_empty(),
        "ActiveMedia and RecordingEarcons would have reported activity for a live \
         dictation; an upload must produce none: {activity:?}"
    );
    assert_eq!(
        audio.cancel_count(),
        0,
        "the capture device was never touched"
    );
    assert!(!audio.is_recording());
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn omitted_inject_returns_text_even_on_a_connection_that_may_type() {
    // A live dictation with no `inject` option types (that is what a hotkey is
    // for). An upload is a request/response call: silence means "give me the
    // text", never "type it into whatever has focus".
    let h = Harness::start().await;
    let mut client = h.client().await;
    assert!(
        client
            .hello
            .as_ref()
            .unwrap()
            .capabilities
            .features
            .text_injection,
        "precondition: this connection is allowed to inject"
    );

    for options in [None, Some(SessionOptions::default())] {
        let t = expect_transcript(
            client
                .request(Command::TranscribeAudio {
                    audio: upload_wav(wav(16_000, 1, 1.0)),
                    options,
                })
                .await
                .unwrap(),
        );
        assert_eq!(t.injection, InjectionOutcome::Delivered);
    }
    assert!(h.injector.injected().is_empty());
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inject_true_types_when_the_connection_may() {
    let h = Harness::start().await;
    let mut client = h.client().await;
    let t = expect_transcript(
        client
            .request(transcribe_with(
                wav(16_000, 1, 1.0),
                SessionOptions {
                    inject: Some(true),
                    ..Default::default()
                },
            ))
            .await
            .unwrap(),
    );
    assert!(
        matches!(t.injection, InjectionOutcome::Injected { .. }),
        "{:?}",
        t.injection
    );
    assert_eq!(h.injector.injected(), vec!["Hello there".to_string()]);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn asking_to_inject_without_the_capability_is_forbidden_not_downgraded() {
    let mut caps = Capabilities::local_trusted();
    caps.features.text_injection = false;
    let h = Harness::with(Setup::default().with_capabilities(caps)).await;
    let mut client = h.client().await;

    let err = client
        .request(transcribe_with(
            wav(16_000, 1, 1.0),
            SessionOptions {
                inject: Some(true),
                ..Default::default()
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Forbidden);
    assert!(h.injector.injected().is_empty());

    // Refusing was free: the engine is not stuck, and the same connection can
    // still take delivery.
    expect_transcript(
        client
            .request(transcribe(wav(16_000, 1, 1.0)))
            .await
            .unwrap(),
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_connection_without_the_upload_capability_is_forbidden() {
    let mut caps = Capabilities::local_trusted();
    caps.features.transcribe_upload = false;
    let h = Harness::with(Setup::default().with_capabilities(caps)).await;
    let mut client = h.client().await;
    let err = client
        .request(transcribe(wav(16_000, 1, 1.0)))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Forbidden);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_remote_style_connection_can_transcribe_but_not_type_or_record() {
    let h = Harness::with(
        Setup::default().with_capabilities(Capabilities::remote_transcription_only()),
    )
    .await;
    let mut client = h.client().await;

    let t = expect_transcript(
        client
            .request(transcribe(wav(16_000, 1, 1.0)))
            .await
            .unwrap(),
    );
    assert_eq!(t.injection, InjectionOutcome::Delivered);

    let err = client
        .request(Command::StartDictation {
            mode: dictate_proto::DictationMode::Toggle,
            options: None,
        })
        .await
        .unwrap_err();
    assert_eq!(
        err.code,
        ErrorCode::Forbidden,
        "uploading is not permission to record"
    );
    h.stop().await;
}

// ---------------------------------------------------------------------------
// 3. Request errors
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_over_long_clip_is_payload_too_large_and_leaves_the_slot_free() {
    let mut caps = Capabilities::local_trusted();
    caps.limits.max_audio_ms = Some(2_000);
    let h = Harness::with(Setup::default().with_capabilities(caps)).await;
    let mut client = h.client().await;

    let err = client
        .request(transcribe(wav(16_000, 1, 3.0)))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::PayloadTooLarge);
    assert_eq!(err.detail().unwrap()["limit_ms"], 2_000);
    assert_eq!(err.detail().unwrap()["actual_ms"], 3_000);

    // Exactly at the limit is fine, and the daemon is not wedged.
    expect_transcript(
        client
            .request(transcribe(wav(16_000, 1, 2.0)))
            .await
            .unwrap(),
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_request_larger_than_the_message_limit_is_answered_then_the_connection_closes() {
    let limit: usize = 64 * 1024;
    let mut caps = Capabilities::local_trusted();
    caps.limits.max_message_bytes = limit as u32;
    let h = Harness::with(Setup::default().with_capabilities(caps)).await;
    let mut client = h.client().await;

    // The start of a real upload request, padded to exactly one byte past the
    // limit with no newline: the framing layer must answer before anything is
    // parsed. Sending *only* enough to cross the limit keeps this
    // deterministic — every byte written is one the daemon reads before it
    // decides, so the write can never race the close into a broken pipe.
    let mut line = br#"{"id":7,"command":{"type":"transcribe_audio","audio":{"data":""#.to_vec();
    line.resize(limit + 1, b'A');
    client.write_raw(&line).await;
    match client.next_event().await {
        Event::Error { error, .. } => assert_eq!(error.code, ErrorCode::PayloadTooLarge),
        other => panic!("expected a payload_too_large error event, got {other:?}"),
    }
    // The stream is out of sync after an oversized line, so it is closed.
    client.expect_closed().await;

    // A fresh connection is unaffected: the daemon did not fall over.
    let mut fresh = h.client().await;
    expect_transcript(
        fresh
            .request(transcribe(wav(16_000, 1, 1.0)))
            .await
            .unwrap(),
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_malformed_wav_is_audio_format_unsupported() {
    let h = Harness::start().await;
    let mut client = h.client().await;

    let err = client
        .request(transcribe(b"RIFF....WAVEnot really".to_vec()))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::AudioFormatUnsupported);

    let mut truncated = wav(16_000, 1, 1.0);
    truncated.truncate(truncated.len() / 2);
    let err = client.request(transcribe(truncated)).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::AudioFormatUnsupported, "{err}");

    expect_transcript(
        client
            .request(transcribe(wav(16_000, 1, 1.0)))
            .await
            .unwrap(),
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn other_bad_requests_get_specific_errors() {
    let h = Harness::start().await;
    let mut client = h.client().await;

    // Empty payload.
    let err = client.request(transcribe(Vec::new())).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidParams);

    // Raw PCM that does not say what it is.
    let err = client
        .request(Command::TranscribeAudio {
            audio: Upload::Inline {
                format: AudioFormat {
                    encoding: AudioEncoding::PcmS16Le,
                    sample_rate_hz: None,
                    channels: Some(1),
                },
                data: vec![0; 64],
            },
            options: None,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidParams);

    // An encoding this build has never heard of.
    let err = client
        .request(Command::TranscribeAudio {
            audio: Upload::Inline {
                format: AudioFormat {
                    encoding: AudioEncoding::Unknown("opus".into()),
                    sample_rate_hz: Some(48_000),
                    channels: Some(1),
                },
                data: vec![1, 2, 3],
            },
            options: None,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::AudioFormatUnsupported);

    // Network-transport sources are not available on the local socket.
    for source in [
        Upload::Stream { stream_id: 1 },
        Upload::Body {
            format: AudioFormat::wav(),
        },
    ] {
        let err = client
            .request(Command::TranscribeAudio {
                audio: source,
                options: None,
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::CapabilityUnavailable);
    }
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn silence_is_a_done_session_with_no_text_not_an_error() {
    // The mock STT always returns text, so use one that reports no speech.
    let h = Harness::with(Setup::default().with_stt(Arc::new(MockStt::silent()))).await;
    let mut client = h.client().await;
    let t = expect_transcript(
        client
            .request(transcribe(wav(16_000, 1, 1.0)))
            .await
            .unwrap(),
    );
    assert_eq!(t.text.as_str(), "");
    assert_eq!(t.word_count, Some(0));
    assert!(matches!(t.injection, InjectionOutcome::Skipped { .. }));
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_transcription_failure_is_the_stt_error_not_a_hang() {
    let h = Harness::with(Setup::default().with_stt(Arc::new(MockStt::failing("model exploded"))))
        .await;
    let mut client = h.client().await;
    let err = client
        .request(transcribe(wav(16_000, 1, 1.0)))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::SttFailed);
    assert!(err.message.contains("model exploded"));
    h.stop().await;
}

// ---------------------------------------------------------------------------
// 4. Concurrency
// ---------------------------------------------------------------------------

/// Start an upload on its own connection and leave it parked in STT.
async fn park_an_upload(
    h: &Harness,
    gate: &Arc<Gate>,
    options: Option<SessionOptions>,
) -> tokio::task::JoinHandle<Result<CommandResult, dictate_proto::ProtoError>> {
    let mut client = h.client().await;
    let task = tokio::spawn(async move {
        client
            .request(Command::TranscribeAudio {
                audio: upload_wav(wav(16_000, 1, 1.0)),
                options,
            })
            .await
    });
    gate.wait_entered().await;
    task
}

fn gated_setup(gate: &Arc<Gate>) -> Setup {
    Setup::default().with_stt(Arc::new(
        MockStt::returning("hello there").with_gate(gate.clone()),
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_busy_engine_answers_busy_to_uploads_and_to_recordings() {
    let gate = Gate::closed();
    let h = Harness::with(gated_setup(&gate)).await;
    let first = park_an_upload(&h, &gate, None).await;

    let mut second = h.client().await;
    let err = second
        .request(transcribe(wav(16_000, 1, 1.0)))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Busy);
    assert!(err.retry_after_ms.is_some());

    let err = second.request(Command::Toggle).await.unwrap_err();
    assert_eq!(
        err.code,
        ErrorCode::Busy,
        "a hotkey press must not steal an upload's slot"
    );

    // While it runs, status reports it honestly.
    let status = match second.request(Command::GetStatus).await.unwrap() {
        CommandResult::Status(s) => s,
        other => panic!("{other:?}"),
    };
    assert_eq!(status.state, State::Transcribing);

    gate.open();
    expect_transcript(first.await.unwrap().unwrap());

    // ...and once it is done the slot is free again.
    expect_transcript(
        second
            .request(transcribe(wav(16_000, 1, 1.0)))
            .await
            .unwrap(),
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_on_an_upload_is_invalid_state_rather_than_a_silent_no_op() {
    let gate = Gate::closed();
    let h = Harness::with(gated_setup(&gate)).await;
    let first = park_an_upload(&h, &gate, None).await;

    let mut other = h.client().await;
    let err = other.request(Command::Stop).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidState);

    gate.open();
    expect_transcript(first.await.unwrap().unwrap());
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_from_another_terminal_aborts_an_upload_and_types_nothing() {
    let gate = Gate::closed();
    let h = Harness::with(gated_setup(&gate)).await;
    let mut watcher = h.client().await;
    watcher.subscribe().await;
    let first = park_an_upload(
        &h,
        &gate,
        Some(SessionOptions {
            inject: Some(true),
            ..Default::default()
        }),
    )
    .await;

    // `dictate cancel` in a second terminal.
    let mut canceller = h.client().await;
    assert!(matches!(
        canceller.request(Command::Cancel).await.unwrap(),
        CommandResult::SessionCancelled { .. }
    ));

    let err = first.await.unwrap().unwrap_err();
    assert_eq!(err.code, ErrorCode::Cancelled);
    assert_eq!(watcher.wait_for_terminal().await, State::Cancelled);

    gate.open();
    assert!(
        h.injector.injected().is_empty(),
        "a cancelled upload must never type"
    );

    // The daemon is immediately usable again.
    let mut client = h.client().await;
    let status = match client.request(Command::GetStatus).await.unwrap() {
        CommandResult::Status(s) => s,
        other => panic!("{other:?}"),
    };
    assert_eq!(status.state, State::Idle);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_uploader_that_hangs_up_takes_its_session_with_it() {
    // `dictate transcribe --inject`, interrupted with Ctrl-C: the text must not
    // appear in the focused window a second later.
    let gate = Gate::closed();
    let injector = Arc::new(MockInjector::new());
    let h = Harness::with(gated_setup(&gate).with_injector(injector.clone())).await;

    let mut uploader = h.client().await;
    uploader
        .fire(Command::TranscribeAudio {
            audio: upload_wav(wav(16_000, 1, 1.0)),
            options: Some(SessionOptions {
                inject: Some(true),
                ..Default::default()
            }),
        })
        .await;
    gate.wait_entered().await;
    uploader.disconnect().await;

    // The engine cancels the abandoned session and frees its slot.
    let mut probe = h.client().await;
    harness::within("the daemon to return to idle", async {
        loop {
            if let CommandResult::Status(s) = probe.request(Command::GetStatus).await.unwrap() {
                if s.state == State::Idle {
                    return;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;

    gate.open();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        injector.injected().is_empty(),
        "text for a caller that already hung up must never be typed"
    );
    // ...and the slot really is free.
    expect_transcript(
        probe
            .request(transcribe(wav(16_000, 1, 1.0)))
            .await
            .unwrap(),
    );
    h.stop().await;
}

/// An uploader that pipelined more requests behind its upload, then hung up.
async fn pipelined_hangup_injects_nothing(pipelined: Vec<u8>) {
    let gate = Gate::closed();
    let injector = Arc::new(MockInjector::new());
    let mut caps = Capabilities::local_trusted();
    caps.limits.max_message_bytes = 256 * 1024;
    let h = Harness::with(
        gated_setup(&gate)
            .with_injector(injector.clone())
            .with_capabilities(caps),
    )
    .await;

    let mut uploader = h.client().await;
    uploader
        .fire(Command::TranscribeAudio {
            audio: upload_wav(wav(16_000, 1, 1.0)),
            options: Some(SessionOptions {
                inject: Some(true),
                ..Default::default()
            }),
        })
        .await;
    uploader.write_raw(&pipelined).await;
    gate.wait_entered().await;
    uploader.disconnect().await;

    let mut probe = h.client().await;
    harness::within("the daemon to return to idle", async {
        loop {
            if let CommandResult::Status(s) = probe.request(Command::GetStatus).await.unwrap() {
                if s.state == State::Idle {
                    return;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    gate.open();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        injector.injected().is_empty(),
        "an upload whose connection is gone must never type, whatever was pipelined behind it"
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_uploader_that_pipelined_a_request_then_hung_up_injects_nothing() {
    pipelined_hangup_injects_nothing(
        b"{\"id\":99,\"command\":{\"type\":\"get_status\"}}\n".to_vec(),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_uploader_that_pipelined_past_the_read_ahead_bound_then_hung_up_injects_nothing() {
    // More than the connection's message limit, with no newline: the daemon
    // stops reading at its bound and must still notice the close.
    pipelined_hangup_injects_nothing(vec![b' '; 300 * 1024]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn requests_pipelined_behind_an_upload_are_answered_in_order() {
    // Must-not-change: reading ahead to watch for a hang-up keeps what it read.
    let gate = Gate::closed();
    let h = Harness::with(gated_setup(&gate)).await;
    let mut client = h.client().await;
    client.fire(transcribe(wav(16_000, 1, 1.0))).await;
    client
        .write_raw(b"{\"id\":\"after\",\"command\":{\"type\":\"get_status\"}}\n")
        .await;
    gate.wait_entered().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    gate.open();
    let ids = client.read_response_ids(2).await;
    assert!(
        matches!(ids[0], RequestId::Number(_)) && ids[1] == RequestId::Text("after".into()),
        "pipelined requests are answered after the upload, in order: {ids:?}"
    );
    h.stop().await;
}

// ---------------------------------------------------------------------------
// 5. Privacy
// ---------------------------------------------------------------------------

fn row_count(h: &Harness) -> i64 {
    h.history
        .lock()
        .unwrap()
        .connection()
        .query_row("SELECT COUNT(*) FROM interactions", [], |row| row.get(0))
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_private_upload_persists_nothing_and_a_normal_one_is_recorded() {
    let h = Harness::with(Setup::default().with_history()).await;
    let mut client = h.client().await;

    let t = expect_transcript(
        client
            .request(transcribe_with(
                wav(16_000, 1, 1.0),
                SessionOptions {
                    privacy: Some(true),
                    ..Default::default()
                },
            ))
            .await
            .unwrap(),
    );
    assert_eq!(
        t.text.as_str(),
        "Hello there",
        "the caller still gets its text"
    );
    assert_eq!(
        t.raw_text, None,
        "and the raw transcript is withheld in privacy mode"
    );
    assert_eq!(
        row_count(&h),
        0,
        "privacy must skip the history insert entirely"
    );

    expect_transcript(
        client
            .request(transcribe(wav(16_000, 1, 1.0)))
            .await
            .unwrap(),
    );
    assert_eq!(row_count(&h), 1, "control: a non-private upload is logged");
    h.stop().await;
}

// ---------------------------------------------------------------------------
// 6. Audio-less mode
// ---------------------------------------------------------------------------

fn audio_less_setup() -> Setup {
    let source: Arc<dyn AudioSource> = Arc::new(DisabledAudioSource::new(
        &dictate_core::config::AudioConfig {
            capture: false,
            pre_roll_ms: 0,
            ..Default::default()
        },
    ));
    Setup::default().with_audio_source(source)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn in_audio_less_mode_recording_fails_clearly_and_uploads_still_work() {
    let h = Harness::with(audio_less_setup()).await;
    let mut client = h.client().await;

    for command in [
        Command::StartDictation {
            mode: dictate_proto::DictationMode::Toggle,
            options: None,
        },
        Command::Toggle,
    ] {
        let name = command.name();
        let err = client.request(command).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::CapabilityUnavailable, "{name}");
        assert!(
            err.message.contains("capture = false") && err.message.contains("dictate transcribe"),
            "the error must say why and what to do instead: {}",
            err.message
        );
    }
    // A hotkey press has no terminal: the desktop is told, too.
    assert!(
        h.notifier
            .notices()
            .iter()
            .any(|n| matches!(n, dictate_core::ports::Notice::Error(m) if m.contains("capture"))),
        "{:?}",
        h.notifier.notices()
    );

    expect_transcript(
        client
            .request(transcribe(wav(16_000, 1, 1.0)))
            .await
            .unwrap(),
    );

    // The daemon says so in its status, for a UI to grey the button.
    let status = match client.request(Command::GetStatus).await.unwrap() {
        CommandResult::Status(s) => s,
        other => panic!("{other:?}"),
    };
    let audio = status.audio.expect("audio status is reported");
    assert!(!audio.capture_enabled);
    assert!(!audio.input_open);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_socket_enforces_upload_max_bytes_on_decoded_audio() {
    let clip = wav(16_000, 1, 1.0);
    let max = clip.len() as u64;
    let mut caps = Capabilities::local_trusted();
    caps.limits = dictate_core::config::UploadConfig {
        max_bytes: max,
        ..Default::default()
    }
    .limits();
    let h = Harness::with(Setup::default().with_capabilities(caps)).await;
    let mut client = h.client().await;

    // Exactly at the limit is accepted.
    expect_transcript(client.request(transcribe(clip.clone())).await.unwrap());

    let mut over = clip;
    over.push(0);
    let err = client.request(transcribe(over)).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::PayloadTooLarge);
    assert_eq!(err.detail().unwrap()["limit_bytes"], max);
    h.stop().await;
}
