//! Synthetic context decisions exercised through the real control plane.
mod harness;
use dictate_core::ports::{
    mock::{Gate, MockAudio, MockInjector, MockVad},
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
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        assert_eq!(
            self.audio.is_recording(),
            call > 0,
            "start capture precedes audio; stop capture happens while recording"
        );
        self.focus.capture()
    }
}

#[tokio::test]
async fn stop_time_app_replaces_start_profile_and_event_precedes_transcribing() {
    stop_time_app_for_mode(DictationMode::Toggle).await;
}

#[tokio::test]
async fn hold_release_uses_stop_time_app() {
    stop_time_app_for_mode(DictationMode::PushToTalk).await;
}

async fn stop_time_app_for_mode(mode: DictationMode) {
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
inject = "paste"
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
            mode,
            options: None,
        })
        .await
        .unwrap();
    // Stop is the user's destination decision, even if recording began elsewhere.
    focus.set(Some(window("slack", "Synthetic later focus")));
    client.request(Command::Stop).await.unwrap();
    let mut contexts = Vec::new();
    loop {
        match client.next_event().await {
            Event::ContextResolved { context, .. } => {
                contexts.push(context.unwrap());
            }
            Event::StateChanged {
                to: State::Transcribing,
                ..
            } => {
                assert_eq!(contexts.len(), 2);
                assert_eq!(contexts[0].app, "com.mitchellh.ghostty");
                assert_eq!(contexts[0].profile.as_deref(), Some("coding"));
                assert_eq!(contexts[0].category, AppCategory::Terminal);
                assert_eq!(contexts[1].app, "slack");
                assert_eq!(contexts[1].profile.as_deref(), Some("later-chat"));
            }
            Event::Final { transcript, .. } => {
                assert!(matches!(
                    transcript.injection,
                    InjectionOutcome::Injected {
                        method: InjectMethod::Paste,
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
    assert_eq!(contexts.len(), 2);
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert_eq!(
        h.injector.policies(),
        vec![Some(dictate_core::InjectionPolicy::Paste)]
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
        Some("slack"),
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
    assert_eq!(
        p.llm_format, None,
        "terminals get the verbatim LLM policy, not an opt-out"
    );
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

/// `capture_context = context_read && host_capture`, end to end: a connection
/// that holds `host_capture` but not `context_read` starts and finishes a
/// dictation without the daemon ever reading the host's focus, and without a
/// `context_resolved` event.
#[tokio::test]
async fn a_host_capture_connection_without_context_read_never_reads_focus() {
    struct CountingFocus(Arc<AtomicUsize>);
    impl ContextProvider for CountingFocus {
        fn capture(&self) -> Option<WindowInfo> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Some(window("slack", "Synthetic title"))
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let mut s = setup(
        ContextConfig::default(),
        Arc::new(CountingFocus(calls.clone())),
    );
    s.capabilities.features.host_capture = true;
    s.capabilities.features.context_read = false;
    let h = Harness::with(s).await;
    let mut client = h.client().await;
    client.subscribe().await;
    client
        .request(Command::StartDictation {
            mode: DictationMode::Toggle,
            options: Some(SessionOptions {
                inject: Some(false),
                ..Default::default()
            }),
        })
        .await
        .unwrap();
    client.request(Command::Stop).await.unwrap();
    loop {
        match client.next_event().await {
            Event::ContextResolved { .. } => panic!("context event without context_read"),
            Event::StateChanged {
                to: State::Done, ..
            } => break,
            _ => {}
        }
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "host focus was read for a connection without context_read"
    );
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

#[tokio::test]
async fn vad_auto_stop_refreshes_the_app_for_one_shot_and_wake_word() {
    struct MovingFocus(AtomicUsize);
    impl ContextProvider for MovingFocus {
        fn capture(&self) -> Option<WindowInfo> {
            Some(if self.0.fetch_add(1, Ordering::Relaxed) == 0 {
                window("ghostty", "Synthetic start")
            } else {
                window("slack", "Synthetic stop")
            })
        }
    }
    for mode in [DictationMode::OneShot, DictationMode::WakeWord] {
        let focus = Arc::new(MovingFocus(AtomicUsize::new(0)));
        let vad = Arc::new(
            MockVad::returning(dictate_vad::GateDecision::Speech {
                samples: vec![0.2; 512],
                leading_trimmed_ms: 0.0,
                trailing_trimmed_ms: 0.0,
            })
            .auto_stopping(),
        );
        let h = Harness::with(
            setup(
                config("[[profiles]]\nname='stop'\nmatch={class='slack'}\ninject='off'"),
                focus.clone(),
            )
            .with_vad(vad),
        )
        .await;
        let mut client = h.client().await;
        client.subscribe().await;
        client
            .request(Command::StartDictation {
                mode,
                options: None,
            })
            .await
            .unwrap();
        let mut apps = Vec::new();
        loop {
            match client.next_event().await {
                Event::ContextResolved { context, .. } => apps.push(context.unwrap().app),
                Event::Final { transcript, .. } => assert_eq!(
                    transcript.injection,
                    InjectionOutcome::Skipped {
                        reason: SkipReason::Disabled
                    }
                ),
                Event::StateChanged {
                    to: State::Done, ..
                } => break,
                _ => {}
            }
        }
        assert_eq!(apps, ["ghostty", "slack"]);
        assert_eq!(focus.0.load(Ordering::Relaxed), 2);
        assert!(h.injector.injected().is_empty());
        h.stop().await;
    }
}

#[tokio::test]
async fn same_app_keeps_start_profile_and_publishes_no_second_context() {
    let focus = Arc::new(TestContext::new(Some(window("ghostty", "Synthetic start"))));
    let h = Harness::with(setup(
        config(
            r#"
[[profiles]]
name='start'
match={class='ghostty', title='start'}
inject='type'
[[profiles]]
name='later'
match={class='ghostty', title='later'}
inject='off'
"#,
        ),
        focus.clone(),
    ))
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
    focus.set(Some(window("ghostty", "Synthetic later")));
    client.request(Command::Stop).await.unwrap();
    let mut contexts = 0;
    loop {
        match client.next_event().await {
            Event::ContextResolved { context, .. } => {
                contexts += 1;
                assert_eq!(context.unwrap().profile.as_deref(), Some("start"));
            }
            Event::StateChanged {
                to: State::Done, ..
            } => break,
            _ => {}
        }
    }
    assert_eq!(contexts, 1);
    assert_eq!(
        h.injector.policies(),
        [Some(dictate_core::InjectionPolicy::Type)]
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_stop_captures_focus_before_a_delayed_pipeline_wakes() {
    use std::sync::{Condvar, Mutex};
    struct SlowMedia {
        entered: AtomicUsize,
        released: Mutex<bool>,
        wake: Condvar,
    }
    impl dictate_core::ports::MediaController for SlowMedia {
        fn pause_if_playing(&self) -> bool {
            self.entered.store(1, Ordering::Release);
            let mut released = self.released.lock().unwrap();
            while !*released {
                released = self.wake.wait(released).unwrap();
            }
            false
        }
        fn resume_if_needed(&self) -> bool {
            false
        }
    }
    let media = Arc::new(SlowMedia {
        entered: AtomicUsize::new(0),
        released: Mutex::new(false),
        wake: Condvar::new(),
    });
    let focus = Arc::new(TestContext::new(Some(window("ghostty", "Synthetic start"))));
    let mut s = setup(
        config("[[profiles]]\nname='stop'\nmatch={class='slack'}\ninject='off'"),
        focus.clone(),
    );
    s.media = media.clone();
    let h = Harness::with(s).await;
    let mut client = h.client().await;
    client.subscribe().await;
    client
        .request(Command::StartDictation {
            mode: DictationMode::Toggle,
            options: None,
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while media.entered.load(Ordering::Acquire) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    focus.set(Some(window("slack", "Synthetic intended destination")));
    client.request(Command::Stop).await.unwrap();
    focus.set(Some(window("google-chrome", "Synthetic later focus")));
    *media.released.lock().unwrap() = true;
    media.wake.notify_one();
    let mut apps = Vec::new();
    loop {
        match client.next_event().await {
            Event::ContextResolved { context, .. } => apps.push(context.unwrap().app),
            Event::Final { transcript, .. } => assert_eq!(
                transcript.injection,
                InjectionOutcome::Skipped {
                    reason: SkipReason::Disabled
                }
            ),
            Event::StateChanged {
                to: State::Done, ..
            } => break,
            _ => {}
        }
    }
    assert_eq!(apps, ["ghostty", "slack"]);
    assert!(h.injector.injected().is_empty());
    h.stop().await;
}
