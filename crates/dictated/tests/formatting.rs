//! The formatting section of a real session (S20), over the real socket.
//!
//! Pins the post-STT order from the Wave 2 integration contract: the
//! deterministic chain runs first and is timed as `fmt_rules`; routing runs on
//! its output; only `type` reaches the LLM pass; and an LLM output that alters
//! a protected span is rejected in favor of the rules output.

mod harness;

use std::sync::Arc;

use dictate_core::ports::mock::{MockFormatter, MockStt};
use dictate_fmt::{FormatConfig, FormatContext};
use dictate_proto::{
    AppContext, Command, InjectionOutcome, Route, SessionOptions, SkipReason, StageTiming, State,
    Tone, Transcript,
};
use harness::{Harness, Setup};

async fn dictate(h: &Harness, options: Option<SessionOptions>) -> Transcript {
    let mut client = h.client().await;
    client.subscribe().await;
    client
        .request(Command::StartDictation {
            mode: dictate_proto::DictationMode::Toggle,
            options,
        })
        .await
        .expect("start");
    client.wait_for_state(State::Recording).await;
    client.request(Command::Stop).await.expect("stop");
    client.wait_for_final().await
}

/// One column of the only interactions row.
fn history_text(h: &Harness, column: &str) -> Option<String> {
    h.history
        .lock()
        .unwrap()
        .connection()
        .query_row(&format!("SELECT {column} FROM interactions"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_rules_chain_is_a_measured_stage_and_its_output_is_what_gets_typed() {
    let h = Harness::with(Setup::default().with_stt(Arc::new(MockStt::returning(
        "um so the the config is broken",
    ))))
    .await;
    let t = dictate(&h, None).await;

    assert_eq!(t.text.as_str(), "So the config is broken.");
    assert_eq!(
        h.injector.injected(),
        vec!["So the config is broken.".to_string()]
    );
    assert_eq!(
        t.raw_text.as_deref(),
        Some("um so the the config is broken"),
        "raw_text is Whisper's, untouched"
    );
    assert!(
        matches!(t.timings.fmt_rules, StageTiming::Ran { ms } if ms >= 0.0),
        "fmt_rules is measured: {:?}",
        t.timings.fmt_rules
    );
    assert!(matches!(t.timings.fmt_llm, StageTiming::Ran { .. }));
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn format_disabled_is_reported_as_skipped_and_the_raw_text_goes_through() {
    let h = Harness::with(
        Setup::default()
            .with_stt(Arc::new(MockStt::returning(
                "um so the the config is broken",
            )))
            .with_format(FormatConfig {
                enabled: false,
                ..FormatConfig::default()
            }),
    )
    .await;
    let t = dictate(&h, None).await;

    assert_eq!(
        t.timings.fmt_rules,
        StageTiming::skipped(SkipReason::Disabled),
        "a disabled chain says `skipped`, never `ran{{0.0}}`"
    );
    assert_eq!(t.text.as_str(), "um so the the config is broken");
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_llm_pass_gets_the_rules_output_and_the_resolved_context() {
    let formatter = Arc::new(MockFormatter::appending(" [llm]"));
    let h = Harness::with(
        Setup::default()
            .with_stt(Arc::new(MockStt::returning("uh the tests are flaky again")))
            .with_formatter(formatter.clone()),
    )
    .await;
    let t = dictate(
        &h,
        Some(SessionOptions {
            app: Some("ghostty".into()),
            ..SessionOptions::default()
        }),
    )
    .await;

    assert_eq!(
        t.text.as_str(),
        "The tests are flaky again. [llm]",
        "rules first, then the LLM on the rules output"
    );
    assert_eq!(formatter.calls(), 1);
    let ctx = formatter
        .last_context()
        .expect("the LLM pass saw a context");
    assert_eq!(
        ctx,
        FormatContext {
            app: Some(AppContext::new("ghostty")),
            tone: Tone::Neutral,
            route: Route::Type,
            language: Some("en".into()),
            vocabulary: Vec::new(),
            use_dictionary: true,
            // The harness's history store is disabled, which S30 reports as
            // privacy mode: nothing about this session may persist.
            persist: false,
            spoken_punctuation: None,
            spoken_line_breaks: None,
            format_llm: None,
            // Nothing in this sentence is protected.
            protected: Vec::new(),
        }
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_non_type_route_skips_the_llm_as_route_not_eligible() {
    let formatter = Arc::new(MockFormatter::appending(" [llm]"));
    let h = Harness::with(
        Setup::default()
            .with_stt(Arc::new(MockStt::returning("edit: make this more formal")))
            .with_formatter(formatter.clone()),
    )
    .await;
    let t = dictate(&h, None).await;

    assert_eq!(
        t.route,
        Route::Edit,
        "`edit:` kept its colon through the rules"
    );
    assert_eq!(
        t.timings.fmt_llm,
        StageTiming::skipped(SkipReason::RouteNotEligible)
    );
    assert_eq!(formatter.calls(), 0, "the LLM pass never ran");
    assert!(matches!(t.timings.fmt_rules, StageTiming::Ran { .. }));
    assert!(matches!(t.injection, InjectionOutcome::Skipped { .. }));
    assert!(h.injector.injected().is_empty());
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn routing_runs_on_the_rules_output_before_the_llm() {
    // "um timer ten minutes" only routes to `timer` once the filler is gone.
    // The timer route is withheld from this connection so the test never runs
    // `systemd-run` on the host; the route decision is still recorded.
    let formatter = Arc::new(MockFormatter::appending(" [llm]"));
    let mut caps = dictated::server::local_capabilities(true);
    caps.routes = vec![Route::Type];
    let h = Harness::with(
        Setup::default()
            .with_stt(Arc::new(MockStt::returning("um timer ten minutes")))
            .with_formatter(formatter.clone())
            .with_capabilities(caps)
            .with_history(),
    )
    .await;
    let mut client = h.client().await;
    client.subscribe().await;
    client
        .request(Command::StartDictation {
            mode: dictate_proto::DictationMode::Toggle,
            options: None,
        })
        .await
        .expect("start");
    client.wait_for_state(State::Recording).await;
    client.request(Command::Stop).await.expect("stop");
    assert_eq!(client.wait_for_terminal().await, State::Error);

    assert_eq!(
        formatter.calls(),
        0,
        "a timer utterance never reaches the LLM"
    );
    assert_eq!(history_text(&h, "route_type").as_deref(), Some("timer"));
    assert_eq!(
        history_text(&h, "grammar_input").as_deref(),
        Some("Timer 10 minutes"),
        "the router saw the rules output"
    );
    assert_eq!(
        history_text(&h, "raw_transcription").as_deref(),
        Some("um timer ten minutes")
    );
    assert!(h.injector.injected().is_empty());
    h.stop().await;
}

fn strip_slash(text: &str) -> String {
    // The production failure: the model dropped the `/` of a slash command.
    text.replace("/research_codebase", "research_codebase")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_llm_that_strips_a_slash_command_is_rejected_and_the_rules_output_kept() {
    let formatter = Arc::new(MockFormatter::rewriting(strip_slash));
    let h = Harness::with(
        Setup::default()
            .with_stt(Arc::new(MockStt::returning(
                "research code base for the auth module",
            )))
            .with_formatter(formatter.clone())
            .with_history(),
    )
    .await;
    let t = dictate(&h, None).await;

    assert_eq!(
        t.text.as_str(),
        "/research_codebase for the auth module.",
        "the slash command survived the LLM"
    );
    assert_eq!(
        h.injector.injected(),
        vec!["/research_codebase for the auth module.".to_string()]
    );
    match &t.timings.fmt_llm {
        StageTiming::Failed { error, .. } => {
            let error = error.as_deref().unwrap_or_default();
            assert!(error.contains("formatter output rejected"), "{error}");
            assert!(error.contains("`/research_codebase`"), "{error}");
        }
        other => panic!("a rejected LLM output is `failed`, got {other:?}"),
    }
    assert_eq!(formatter.calls(), 1);

    // History keeps both sides for S21's evaluation.
    assert_eq!(
        history_text(&h, "grammar_input").as_deref(),
        Some("/research_codebase for the auth module.")
    );
    assert_eq!(
        history_text(&h, "grammar_output").as_deref(),
        Some("research_codebase for the auth module."),
        "the rejected output is recorded, not delivered"
    );
    assert!(
        history_text(&h, "grammar_error").is_some_and(|e| e.contains("formatter output rejected"))
    );
    assert_eq!(
        history_text(&h, "corrected_transcription").as_deref(),
        Some("/research_codebase for the auth module.")
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn history_records_raw_rules_llm_and_final_text_separately() {
    let h = Harness::with(
        Setup::default()
            .with_stt(Arc::new(MockStt::returning("um check the the logs please")))
            .with_formatter(Arc::new(MockFormatter::appending(" Thanks!")))
            .with_history(),
    )
    .await;
    let t = dictate(&h, None).await;

    assert_eq!(t.text.as_str(), "Check the logs please. Thanks!");
    assert_eq!(
        history_text(&h, "raw_transcription").as_deref(),
        Some("um check the the logs please")
    );
    assert_eq!(
        history_text(&h, "grammar_input").as_deref(),
        Some("Check the logs please.")
    );
    assert_eq!(
        history_text(&h, "grammar_output").as_deref(),
        Some("Check the logs please. Thanks!")
    );
    assert_eq!(
        history_text(&h, "corrected_transcription").as_deref(),
        Some("Check the logs please. Thanks!")
    );
    let rules_ms: Option<f64> = h
        .history
        .lock()
        .unwrap()
        .connection()
        .query_row(
            "SELECT fmt_rules_duration_ms FROM interactions",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(rules_ms.is_some(), "the measured rules time is persisted");
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rules_output_that_is_empty_types_nothing() {
    // Whisper's classic silence hallucination, scrubbed to nothing.
    let h =
        Harness::with(Setup::default().with_stt(Arc::new(MockStt::returning("Thank you.")))).await;
    let t = dictate(&h, None).await;

    assert_eq!(t.text.as_str(), "");
    assert!(h.injector.injected().is_empty());
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_or_app_profile_opt_out_skips_the_llm_as_disabled() {
    // The session asks for rules-only text.
    let formatter = Arc::new(MockFormatter::appending(" [llm]"));
    let h = Harness::with(
        Setup::default()
            .with_stt(Arc::new(MockStt::returning("the tests are flaky again")))
            .with_formatter(formatter.clone()),
    )
    .await;
    let t = dictate(
        &h,
        Some(SessionOptions {
            format_llm: Some(false),
            ..SessionOptions::default()
        }),
    )
    .await;
    assert_eq!(t.text.as_str(), "The tests are flaky again.");
    assert_eq!(
        t.timings.fmt_llm,
        StageTiming::skipped(SkipReason::Disabled)
    );
    assert_eq!(formatter.calls(), 0);
    h.stop().await;

    // An app profile asks for rules-only text for its app.
    let context: dictate_core::ContextConfig =
        toml::from_str("[[profiles]]\nname='plain'\nmatch={class='plainapp'}\nllm_format=false")
            .unwrap();
    let formatter = Arc::new(MockFormatter::appending(" [llm]"));
    let h = Harness::with(Setup {
        context: Arc::new(dictate_core::ContextEngine::new(
            context,
            Arc::new(dictate_core::NoContext),
        )),
        ..Setup::default()
            .with_stt(Arc::new(MockStt::returning("the tests are flaky again")))
            .with_formatter(formatter.clone())
    })
    .await;
    let t = dictate(
        &h,
        Some(SessionOptions {
            app: Some("plainapp".into()),
            ..SessionOptions::default()
        }),
    )
    .await;
    assert_eq!(t.text.as_str(), "The tests are flaky again.");
    assert_eq!(
        t.timings.fmt_llm,
        StageTiming::skipped(SkipReason::Disabled)
    );
    assert_eq!(formatter.calls(), 0);

    // Another app still gets the pass.
    let t = dictate(
        &h,
        Some(SessionOptions {
            app: Some("otherapp".into()),
            ..SessionOptions::default()
        }),
    )
    .await;
    assert_eq!(t.text.as_str(), "The tests are flaky again. [llm]");
    assert_eq!(formatter.calls(), 1);
    h.stop().await;
}
