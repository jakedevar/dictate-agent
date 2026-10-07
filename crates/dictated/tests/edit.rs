//! S25 through the actual daemon/socket, with synthetic selection and LLM ports.
mod harness;

use dictate_core::edit_executor::{EditConfig, EditExecutor};
use dictate_core::ports::mock::{Gate, MockInjector, MockStt};
use dictate_core::ports::{AudioSource, Notice};
use dictate_fmt::llm::client::{
    BackendError, BoxFuture, ChatRequest, ChatResponse, InstalledModel,
};
use dictate_fmt::llm::{ChatBackend, LlmConfig};
use dictate_proto::{
    Command, DictationMode, InjectionOutcome, Route, SessionOptions, StageTiming, State,
};
use harness::{within, Harness, Setup};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct Fake {
    reply: Result<String, BackendError>,
    calls: AtomicUsize,
    requests: Mutex<Vec<ChatRequest>>,
    entered: tokio::sync::Notify,
    stall: bool,
    change: Option<(Arc<MockInjector>, String, u32)>,
}
impl Fake {
    fn reply(text: &str) -> Self {
        Self {
            reply: Ok(text.into()),
            calls: AtomicUsize::new(0),
            requests: Mutex::new(vec![]),
            entered: tokio::sync::Notify::new(),
            stall: false,
            change: None,
        }
    }
}
impl ChatBackend for Fake {
    fn chat<'a>(
        &'a self,
        request: &'a ChatRequest,
    ) -> BoxFuture<'a, Result<ChatResponse, BackendError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.requests.lock().unwrap().push(request.clone());
            if let Some((injector, text, window)) = &self.change {
                injector.select(text, *window);
            }
            self.entered.notify_one();
            if self.stall {
                std::future::pending::<()>().await;
            }
            self.reply.clone().map(|content| ChatResponse {
                content,
                done_reason: Some("stop".into()),
                ..Default::default()
            })
        })
    }
    fn list_models(&self) -> BoxFuture<'_, Result<Vec<InstalledModel>, BackendError>> {
        Box::pin(async {
            Ok(vec![InstalledModel {
                name: "synthetic:1b".into(),
                family: "test".into(),
                size: 0,
            }])
        })
    }
    fn load<'a>(&'a self, _: &'a str, _: &'a str) -> BoxFuture<'a, Result<Duration, BackendError>> {
        Box::pin(async { Ok(Duration::ZERO) })
    }
}
fn setup(fake: Arc<Fake>, preview: bool) -> Setup {
    let mut setup =
        Setup::default().with_stt(Arc::new(MockStt::returning("edit make this formal")));
    setup.injector.select("send the synthetic report", 1);
    setup.editor = Some(Arc::new(EditExecutor::with_backend(
        &dictate_core::config::LocalConfig {
            model: "synthetic:1b".into(),
            models: vec![],
            ..Default::default()
        },
        &LlmConfig::default(),
        &EditConfig {
            preview_only: preview,
        },
        fake,
    )));
    setup
}
async fn start(h: &Harness, options: Option<SessionOptions>) -> harness::Client {
    let mut client = h.client().await;
    client.subscribe().await;
    client
        .request(Command::StartDictation {
            mode: DictationMode::Toggle,
            options,
        })
        .await
        .unwrap();
    client.wait_for_state(State::Recording).await;
    client
}
async fn run(h: &Harness, options: Option<SessionOptions>) -> dictate_proto::Transcript {
    let mut client = start(h, options).await;
    client.request(Command::Stop).await.unwrap();
    client.wait_for_final().await
}

#[tokio::test]
async fn edit_rewrites_only_the_selection_once_and_accounts_for_the_llm() {
    let fake = Arc::new(Fake::reply("Please send the synthetic report."));
    let h = Harness::with(setup(fake.clone(), false)).await;
    let transcript = run(&h, None).await;
    assert_eq!(transcript.route, Route::Edit);
    assert_eq!(
        transcript.text.as_str(),
        "Please send the synthetic report."
    );
    assert!(transcript.injection.did_inject());
    assert!(matches!(
        transcript.timings.fmt_llm,
        StageTiming::Ran { .. }
    ));
    assert_eq!(
        h.injector.replacements(),
        ["Please send the synthetic report."]
    );
    assert!(
        h.injector.injected().is_empty(),
        "EDIT must never use ordinary typing"
    );
    assert_eq!(fake.calls.load(Ordering::SeqCst), 1);
    h.stop().await;
}

#[tokio::test]
async fn explicit_edit_route_treats_the_whole_utterance_as_an_instruction() {
    let fake = Arc::new(Fake::reply("Please send the synthetic report."));
    let setup =
        setup(fake.clone(), false).with_stt(Arc::new(MockStt::returning("make this formal")));
    let h = Harness::with(setup).await;
    let transcript = run(
        &h,
        Some(SessionOptions {
            route: Some(Route::Edit),
            ..Default::default()
        }),
    )
    .await;
    assert!(transcript.injection.did_inject());
    let request = fake.requests.lock().unwrap()[0].clone();
    let data: serde_json::Value = serde_json::from_str(&request.messages[1].content).unwrap();
    assert_eq!(data["instruction"], "Make this formal");
    h.stop().await;
}

#[tokio::test]
async fn missing_selection_and_empty_instruction_never_call_the_model_or_type() {
    for empty_instruction in [false, true] {
        let fake = Arc::new(Fake::reply("unused"));
        let mut setup = setup(fake.clone(), false);
        if empty_instruction {
            setup.stt = Arc::new(MockStt::returning("edit:"));
        } else {
            setup.injector.clear_selection();
        }
        let h = Harness::with(setup).await;
        let transcript = run(&h, None).await;
        assert!(matches!(
            transcript.injection,
            InjectionOutcome::Failed { .. }
        ));
        assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
        assert!(h.injector.replacements().is_empty());
        assert!(h.injector.injected().is_empty());
        h.stop().await;
    }
}

#[tokio::test]
async fn backend_error_and_invalid_reply_leave_the_selection_untouched() {
    for error in [false, true] {
        let mut fake = Fake::reply("Sure! Here is your replacement.");
        if error {
            fake.reply = Err(BackendError::Unreachable("synthetic failure".into()));
        }
        let h = Harness::with(setup(Arc::new(fake), false)).await;
        let transcript = run(&h, None).await;
        assert!(matches!(
            transcript.injection,
            InjectionOutcome::Failed { .. }
        ));
        assert!(matches!(
            transcript.timings.fmt_llm,
            StageTiming::Failed { .. }
        ));
        assert!(h.injector.replacements().is_empty());
        assert!(h
            .notifier
            .notices()
            .iter()
            .any(|n| matches!(n, Notice::EditError(_))));
        h.stop().await;
    }
}

#[tokio::test]
async fn changed_selection_or_window_discards_the_rewrite() {
    for (text, window) in [
        ("a different synthetic selection", 1),
        ("send the synthetic report", 2),
    ] {
        let injector = Arc::new(MockInjector::new());
        let mut fake = Fake::reply("Please send the synthetic report.");
        fake.change = Some((injector.clone(), text.into(), window));
        let setup = setup(Arc::new(fake), false).with_injector(injector.clone());
        injector.select("send the synthetic report", 1);
        let h = Harness::with(setup).await;
        let transcript = run(&h, None).await;
        assert!(matches!(
            transcript.injection,
            InjectionOutcome::Failed { .. }
        ));
        assert!(h.injector.replacements().is_empty());
        assert!(h.injector.injected().is_empty());
        h.stop().await;
    }
}

#[tokio::test]
async fn cancel_during_rewrite_never_reaches_replacement() {
    let mut fake = Fake::reply("unused");
    fake.stall = true;
    let fake = Arc::new(fake);
    let h = Harness::with(setup(fake.clone(), false)).await;
    let mut client = start(&h, None).await;
    client.request(Command::Stop).await.unwrap();
    within("edit model entered", fake.entered.notified()).await;
    client.request(Command::Cancel).await.unwrap();
    client.wait_for_state(State::Cancelled).await;
    assert!(h.injector.replacements().is_empty());
    assert!(h.injector.injected().is_empty());
    assert!(!h.audio.is_recording());
    h.stop().await;
}

#[tokio::test]
async fn preview_never_replaces_and_privacy_suppresses_the_notification_and_history() {
    for privacy in [false, true] {
        let mut setup = setup(
            Arc::new(Fake::reply("Please send the synthetic report.")),
            true,
        );
        setup.history_enabled = true;
        let h = Harness::with(setup).await;
        let transcript = run(
            &h,
            Some(SessionOptions {
                privacy: Some(privacy),
                ..Default::default()
            }),
        )
        .await;
        assert_eq!(
            transcript.text.as_str(),
            "Please send the synthetic report."
        );
        assert!(!transcript.injection.did_inject());
        assert!(h.injector.replacements().is_empty());
        assert_eq!(
            h.notifier
                .notices()
                .iter()
                .any(|n| matches!(n, Notice::EditPreview(_))),
            !privacy
        );
        if privacy {
            assert_eq!(
                h.history
                    .lock()
                    .unwrap()
                    .query(&Default::default())
                    .unwrap()
                    .total,
                Some(0)
            );
        }
        h.stop().await;
    }
}

#[tokio::test]
async fn no_injection_permission_never_reads_selection_or_calls_the_model() {
    let fake = Arc::new(Fake::reply("unused"));
    let h = Harness::with(setup(fake.clone(), false)).await;
    let transcript = run(
        &h,
        Some(SessionOptions {
            inject: Some(false),
            ..Default::default()
        }),
    )
    .await;
    assert!(!transcript.injection.did_inject());
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    assert!(h.injector.replacements().is_empty());
    h.stop().await;
}

#[tokio::test]
async fn focus_changed_during_stt_cannot_capture_a_new_destination() {
    let gate = Gate::closed();
    let stt = Arc::new(MockStt::returning("edit make this formal").with_gate(gate.clone()));
    let fake = Arc::new(Fake::reply("unused"));
    let h = Harness::with(setup(fake.clone(), false).with_stt(stt)).await;
    let mut client = start(&h, None).await;
    client.request(Command::Stop).await.unwrap();
    within("recognizer entered", gate.wait_entered()).await;
    h.injector.select("another app's selection", 2);
    gate.open();
    let transcript = client.wait_for_final().await;
    assert!(matches!(
        transcript.injection,
        InjectionOutcome::Failed { .. }
    ));
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    assert!(h.injector.replacements().is_empty());
    h.stop().await;
}

#[tokio::test]
async fn terminal_and_nonpaste_profiles_never_attempt_a_rewrite() {
    for policy in ["terminal", "off", "type"] {
        let terminal = policy == "terminal";
        let fake = Arc::new(Fake::reply("unused"));
        let mut setup = setup(fake.clone(), false);
        let config = if terminal {
            dictate_core::ContextConfig::default()
        } else {
            serde_json::from_value(serde_json::json!({"enabled": true, "profiles": [{"name": "synthetic-off", "match": {"class": "synthetic-editor"}, "inject": policy}]})).unwrap()
        };
        let provider = Arc::new(dictate_core::TestContext::new(Some(
            dictate_core::WindowInfo {
                class: Some(
                    if terminal {
                        "XTerm"
                    } else {
                        "synthetic-editor"
                    }
                    .into(),
                ),
                ..Default::default()
            },
        )));
        setup.context = Arc::new(dictate_core::ContextEngine::new(config, provider));
        let h = Harness::with(setup).await;
        let transcript = run(&h, None).await;
        assert!(!transcript.injection.did_inject());
        assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
        assert!(h.injector.replacements().is_empty());
        assert!(h.injector.injected().is_empty());
        h.stop().await;
    }
}

#[tokio::test]
async fn history_privacy_suppresses_edit_previews_without_a_session_override() {
    let mut setup = setup(
        Arc::new(Fake::reply("Please send the synthetic report.")),
        true,
    );
    setup.history_enabled = true;
    setup.history_privacy = true;
    let h = Harness::with(setup).await;
    let transcript = run(&h, None).await;
    assert!(!transcript.injection.did_inject());
    assert!(!h
        .notifier
        .notices()
        .iter()
        .any(|n| matches!(n, Notice::EditPreview(_))));
    assert!(h
        .history
        .lock()
        .unwrap()
        .query(&Default::default())
        .unwrap()
        .items
        .is_empty());
    h.stop().await;
}

#[tokio::test]
async fn ungranted_edit_route_is_denied_before_selection_and_llm_work() {
    let fake = Arc::new(Fake::reply("unused"));
    let mut setup = setup(fake.clone(), false);
    setup.capabilities.routes = vec![Route::Type];
    let h = Harness::with(setup).await;
    let mut client = start(&h, None).await;
    client.request(Command::Stop).await.unwrap();
    client.wait_for_state(State::Error).await;
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    assert!(h.injector.replacements().is_empty());
    h.stop().await;
}

#[tokio::test]
async fn cancellation_after_replacement_commit_is_too_late_and_pastes_once() {
    let gate = Gate::closed();
    let injector = Arc::new(MockInjector::new().with_gate(gate.clone()));
    let setup = setup(
        Arc::new(Fake::reply("Please send the synthetic report.")),
        false,
    )
    .with_injector(injector.clone());
    injector.select("send the synthetic report", 1);
    let h = Harness::with(setup).await;
    let mut client = start(&h, None).await;
    client.request(Command::Stop).await.unwrap();
    within("selection replacement entered", gate.wait_entered()).await;
    let error = client.request(Command::Cancel).await.unwrap_err();
    assert_eq!(error.code, dictate_proto::ErrorCode::Conflict);
    gate.open();
    assert!(client.wait_for_final().await.injection.did_inject());
    assert_eq!(
        h.injector.replacements(),
        ["Please send the synthetic report."]
    );
    assert!(h.injector.injected().is_empty());
    h.stop().await;
}
