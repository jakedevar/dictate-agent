//! Synthetic context decisions exercised through the real control plane.
mod harness;
use dictate_core::ports::{
    mock::{Gate, MockAudio, MockInjector},
    AudioSource,
};
use dictate_core::{ContextConfig, ContextEngine, ContextProvider, TestContext, WindowInfo};
use dictate_proto::{
    AppCategory, Capabilities, Command, CommandResult, DictationMode, ErrorCode, Event,
    InjectMethod, InjectionOutcome, SessionOptions, SkipReason, StageTiming, State,
};
use harness::{Harness, Setup};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn window(class: &str, title: &str) -> WindowInfo {
    WindowInfo {
        class: Some(class.into()),
        instance: Some("synthetic-instance".into()),
        title: Some(title.into()),
        ..Default::default()
    }
}
fn setup(config: ContextConfig, provider: Arc<dyn ContextProvider>) -> Setup {
    Setup {
        context: Arc::new(ContextEngine::new(config, provider)),
        capabilities: Capabilities::local_trusted(),
        ..Default::default()
    }
}
fn config(text: &str) -> ContextConfig {
    toml::from_str(text).unwrap()
}

struct BeforeAudio {
    audio: Arc<MockAudio>,
    focus: Arc<TestContext>,
    calls: Arc<AtomicUsize>,
}
impl ContextProvider for BeforeAudio {
    fn capture(&self) -> Option<WindowInfo> {
        assert!(
            !self.audio.is_recording(),
            "context must be captured before audio.start's first await"
        );
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.focus.capture()
    }
}

#[tokio::test]
async fn snapshot_precedes_audio_is_immutable_and_event_precedes_transcribing() {
    let audio = Arc::new(MockAudio::with_seconds(0.1));
    let focus = Arc::new(TestContext::new(Some(window(
        "com.mitchellh.ghostty",
        "Synthetic Claude fixture",
    ))));
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(BeforeAudio {
        audio: audio.clone(),
        focus: focus.clone(),
        calls: calls.clone(),
    });
    let mut s = setup(
        config(
            r#"
[[profiles]]
name = "coding"
match = { class = "com.mitchellh.ghostty", title = "(?i)claude" }
inject = "type"
[[profiles]]
name = "later-chat"
match = { class = "slack" }
inject = "off"
"#,
        ),
        provider,
    );
    s.audio = audio;
    let h = Harness::with(s.with_history()).await;
    let mut client = h.client().await;
    client.subscribe().await;
    client
        .request(Command::StartDictation {
            mode: DictationMode::Toggle,
            options: None,
        })
        .await
        .unwrap();
    // Moving focus must not replace the session's original decision.
    focus.set(Some(window("slack", "Synthetic later focus")));
    client.request(Command::Stop).await.unwrap();
    let mut context_seen = false;
    loop {
        match client.next_event().await {
            Event::ContextResolved { context, .. } => {
                assert!(!context_seen, "exactly one resolution per session");
                let ctx = context.unwrap();
                assert_eq!(ctx.app, "com.mitchellh.ghostty");
                assert_eq!(ctx.profile.as_deref(), Some("coding"));
                assert_eq!(ctx.category, AppCategory::Terminal);
                context_seen = true;
            }
            Event::StateChanged {
                to: State::Transcribing,
                ..
            } => assert!(context_seen),
            Event::Final { transcript, .. } => {
                assert!(matches!(
                    transcript.injection,
                    InjectionOutcome::Injected {
                        method: InjectMethod::Keystroke,
                        ..
                    }
                ));
            }
            Event::StateChanged {
                to: State::Done, ..
            } => break,
            _ => {}
        }
    }
    assert!(context_seen);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        h.injector.policies(),
        vec![Some(dictate_core::InjectionPolicy::Type)]
    );
    let app: Option<String> = h
        .history
        .lock()
        .unwrap()
        .connection()
        .query_row("SELECT app_context FROM interactions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        app.as_deref(),
        Some("com.mitchellh.ghostty"),
        "history stores only the ID, never the title"
    );
    h.stop().await;
}

#[tokio::test]
async fn profile_off_skips_probe_and_commit_and_reports_disabled() {
    // A probe would hang at this unopened gate, so completion proves Off was
    // honored before even touching the backend.
    let injector = Arc::new(MockInjector::unavailable().with_availability_gate(Gate::closed()));
    let c = config("[[profiles]]\nname='off'\nmatch={class='slack'}\ninject='off'");
    let h = Harness::with(
        setup(
            c,
            Arc::new(TestContext::new(Some(window("slack", "Synthetic")))),
        )
        .with_injector(injector),
    )
    .await;
    let mut client = h.client().await;
    client.subscribe().await;
    client
        .request(Command::StartDictation {
            mode: DictationMode::Toggle,
            options: None,
        })
        .await
        .unwrap();
    client.request(Command::Stop).await.unwrap();
    let transcript = client.wait_for_final().await;
    assert_eq!(
        transcript.injection,
        InjectionOutcome::Skipped {
            reason: SkipReason::Disabled
        }
    );
    assert_eq!(
        transcript.timings.inject,
        StageTiming::skipped(SkipReason::Disabled)
    );
    assert!(h.injector.injected().is_empty());
    assert!(h.injector.policies().is_empty());
    h.stop().await;
}

#[tokio::test]
async fn caller_delivery_and_app_hint_never_query_host_focus() {
    struct ForbiddenFocus;
    impl ContextProvider for ForbiddenFocus {
        fn capture(&self) -> Option<WindowInfo> {
            panic!("caller-supplied app must not query host focus")
        }
    }
    let c = config("[[profiles]]\nname='off'\nmatch={class='slack'}\ninject='off'");
    let h = Harness::with(setup(c, Arc::new(ForbiddenFocus))).await;
    let mut client = h.client().await;
    client.subscribe().await;
    client
        .request(Command::StartDictation {
            mode: DictationMode::Toggle,
            options: Some(SessionOptions {
                app: Some("slack".into()),
                inject: Some(false),
                ..Default::default()
            }),
        })
        .await
        .unwrap();
    client.request(Command::Stop).await.unwrap();
    assert_eq!(
        client.wait_for_final().await.injection,
        InjectionOutcome::Delivered
    );
    assert!(h.injector.injected().is_empty());
    h.stop().await;
}

#[tokio::test]
async fn private_context_sessions_leave_no_history_rows() {
    for global in [false, true] {
        let mut s = setup(
            ContextConfig::default(),
            Arc::new(TestContext::new(Some(window(
                "slack",
                "Synthetic private fixture",
            )))),
        )
        .with_history();
        s.history_privacy = global;
        let h = Harness::with(s).await;
        let mut client = h.client().await;
        client.subscribe().await;
        client
            .request(Command::StartDictation {
                mode: DictationMode::Toggle,
                options: Some(SessionOptions {
                    privacy: Some(!global),
                    ..Default::default()
                }),
            })
            .await
            .unwrap();
        client.request(Command::Stop).await.unwrap();
        client.wait_for_state(State::Done).await;
        let count: i64 = h
            .history
            .lock()
            .unwrap()
            .connection()
            .query_row("SELECT COUNT(*) FROM interactions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
        h.stop().await;
    }
}

#[tokio::test]
async fn get_context_returns_resolved_profile_and_none_has_defined_defaults() {
    let focus = Arc::new(TestContext::new(Some(window(
        "Ghostty",
        "Synthetic fixture",
    ))));
    let h = Harness::with(setup(ContextConfig::default(), focus.clone())).await;
    let mut client = h.client().await;
    assert!(
        client
            .hello
            .as_ref()
            .unwrap()
            .capabilities
            .features
            .context_read
    );
    let CommandResult::Context(p) = client.request(Command::GetContext).await.unwrap() else {
        panic!("expected context result")
    };
    assert_eq!(p.context.as_ref().unwrap().app, "ghostty");
    assert_eq!(p.llm_format, None, "terminals get the verbatim LLM policy, not an opt-out");
    assert_eq!(p.spoken_punctuation, Some(false));
    assert_eq!(p.inject, None, "ghostty preserves global Ctrl+V");
    focus.set(None);
    let CommandResult::Context(p) = client.request(Command::GetContext).await.unwrap() else {
        panic!("expected context result")
    };
    assert_eq!(*p, Default::default());
    h.stop().await;
}

#[tokio::test]
async fn remote_context_discovery_and_sensitive_events_are_denied() {
    let mut s = setup(
        ContextConfig::default(),
        Arc::new(TestContext::new(Some(window(
            "slack",
            "Synthetic private title",
        )))),
    );
    s.capabilities = Capabilities::remote_transcription_only();
    let h = Harness::with(s).await;
    let mut client = h.client().await;
    client.subscribe().await;
    assert!(
        !client
            .hello
            .as_ref()
            .unwrap()
            .capabilities
            .features
            .context_read
    );
    assert_eq!(
        client.request(Command::GetContext).await.unwrap_err().code,
        ErrorCode::Forbidden
    );
    // A simultaneous local hotkey session publishes a sensitive context event
    // to the shared bus. A remote subscriber must not receive it.
    h.daemon
        .engine()
        .start(
            dictate_core::Actor::Signal,
            DictationMode::Toggle,
            dictate_core::ResolvedOptions {
                inject: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    h.daemon
        .engine()
        .stop(dictate_core::Actor::Signal)
        .await
        .unwrap();
    loop {
        match client.next_event().await {
            Event::ContextResolved { .. } => panic!("remote subscriber received host focus"),
            Event::StateChanged {
                to: State::Done, ..
            } => break,
            _ => {}
        }
    }
    h.stop().await;
}

/// A one-second 16 kHz mono WAV of a quiet tone (the mock VAD and STT decide
/// what it "says"; only the decoder reads the samples).
fn tone_wav() -> Vec<u8> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut w = hound::WavWriter::new(&mut buf, spec).unwrap();
        for i in 0..16_000u32 {
            let s =
                ((f64::from(i) * 440.0 * std::f64::consts::TAU / 16_000.0).sin() * 3000.0) as i16;
            w.write_sample(s).unwrap();
        }
        w.finalize().unwrap();
    }
    buf.into_inner()
}

#[tokio::test]
async fn uploads_resolve_a_named_app_without_ever_reading_host_focus() {
    struct ForbiddenFocus;
    impl ContextProvider for ForbiddenFocus {
        fn capture(&self) -> Option<WindowInfo> {
            panic!("an upload must never query host focus")
        }
    }
    let c = config("[[profiles]]\nname='chat'\nmatch={class='slack'}\ncategory='chat'");
    let h = Harness::with(setup(c, Arc::new(ForbiddenFocus))).await;
    let mut client = h.client().await;
    client.subscribe().await;

    for (app, expected) in [(Some("Slack"), Some(("slack", "chat"))), (None, None)] {
        let result = client
            .request(Command::TranscribeAudio {
                audio: dictate_proto::AudioSource::Inline {
                    format: dictate_proto::AudioFormat::wav(),
                    data: tone_wav(),
                },
                options: Some(SessionOptions {
                    app: app.map(str::to_string),
                    ..Default::default()
                }),
            })
            .await
            .unwrap();
        assert!(matches!(result, CommandResult::Transcript(_)), "{result:?}");

        let context = loop {
            match client.next_event().await {
                Event::ContextResolved { context, .. } => break context,
                _ => continue,
            }
        };
        match expected {
            Some((app, profile)) => {
                let context = context.expect("a named app resolves a context");
                assert_eq!(context.app, app);
                assert_eq!(context.profile.as_deref(), Some(profile));
                assert_eq!(context.category, AppCategory::Chat);
                assert_eq!(context.title, None, "uploads never carry host titles");
            }
            None => assert_eq!(context, None, "no app and no host focus: no context"),
        }
    }
    assert!(h.injector.injected().is_empty());
    h.stop().await;
}
