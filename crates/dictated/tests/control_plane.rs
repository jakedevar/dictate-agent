//! The control plane, end to end over a real unix socket.
//!
//! Organized by the risk each group covers:
//!
//! 1. **the happy path** — a full dictation drives the protocol's state machine
//! 2. **cancellation** — reachable from every non-terminal state, and never
//!    leaving half-injected text or a live capture stream
//! 3. **concurrent commands** — racing starts, cancel-vs-inject, disconnects
//! 4. **session ownership** — the S33 property, enforced today
//! 5. **the signal shim** — SIGUSR1/2 produce identical transitions
//! 6. **protocol conformance** — handshake, capabilities, unknown commands
//! 7. **timings** — honest per-stage accounting

mod harness;

use std::sync::Arc;
use std::time::Duration;

use dictate_core::ports::mock::{
    ActiveMedia, Gate, MockAudio, MockFormatter, MockInjector, MockStt, MockVad, RecordingEarcons,
};
use dictate_core::ports::{AudioSource, GateDecision, Notice};
use dictate_proto::{
    Command, CommandResult, ErrorCode, InjectionOutcome, SessionOptions, SkipReason, StageTiming,
    State,
};
use harness::{eventually, Harness, Setup};

// ---------------------------------------------------------------------------
// 1. The happy path
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_dictation_runs_the_protocols_state_machine_over_the_socket() {
    let h = Harness::start().await;
    let mut client = h.client().await;
    client.subscribe().await;

    let session_id = match client
        .request(Command::StartDictation {
            mode: dictate_proto::DictationMode::Toggle,
            options: None,
        })
        .await
        .expect("start")
    {
        CommandResult::SessionStarted { session_id } => session_id,
        other => panic!("expected session_started, got {other:?}"),
    };

    // The response arrives before any event about the session it created.
    assert_eq!(
        client.wait_for_state(State::Recording).await,
        vec![State::Recording]
    );

    match client.request(Command::Stop).await.expect("stop") {
        CommandResult::SessionStopped { session_id: id } => assert_eq!(id, session_id),
        other => panic!("expected session_stopped, got {other:?}"),
    }

    let transcript = client.wait_for_final().await;
    assert_eq!(transcript.text.as_str(), "hello there");
    assert_eq!(transcript.route, dictate_proto::Route::Type);
    assert!(matches!(
        transcript.injection,
        InjectionOutcome::Injected { .. }
    ));
    assert_eq!(transcript.word_count, Some(2));

    assert_eq!(
        h.injector.injected(),
        vec!["hello there".to_string()],
        "the transcript must reach the injector exactly once"
    );

    // Every stage was visited, in the protocol's order. Asserted against the
    // connection's whole recorded history, because `wait_for_final` above has
    // already consumed some of these events.
    client.wait_for_state(State::Done).await;
    client.assert_walked(&[
        State::Recording,
        State::Transcribing,
        State::Formatting,
        State::Injecting,
        State::Done,
    ]);

    // A terminal state resets to idle, so a HUD can clear itself.
    assert_eq!(client.wait_for_state(State::Idle).await, vec![State::Idle]);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn audio_side_effects_are_visible_in_the_daemon_event_stream() {
    let h = Harness::with(
        Setup::default().with_audio_side_effects(Arc::new(ActiveMedia), Arc::new(RecordingEarcons)),
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

    let mut activities = Vec::new();
    loop {
        match client.next_event().await {
            dictate_proto::Event::AudioActivity { activity, .. } => activities.push(activity),
            dictate_proto::Event::StateChanged {
                to: State::Idle, ..
            } => break,
            _ => {}
        }
    }
    assert!(activities.contains(&dictate_proto::AudioActivity::EarconStart));
    assert!(activities.contains(&dictate_proto::AudioActivity::EarconStop));
    assert!(activities.contains(&dictate_proto::AudioActivity::MediaPaused));
    assert!(activities.contains(&dictate_proto::AudioActivity::MediaResumed));
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_with_no_audio_finishes_done_with_empty_text() {
    let h = Harness::with(Setup::default().with_audio(Arc::new(MockAudio::empty()))).await;
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

    let transcript = client.wait_for_final().await;
    assert_eq!(transcript.text.as_str(), "");
    assert_eq!(
        transcript.raw_text,
        Some(String::new()),
        "empty means nothing was said; absent would mean it is being withheld"
    );
    assert!(matches!(
        transcript.injection,
        InjectionOutcome::Skipped {
            reason: SkipReason::NoSpeechDetected
        }
    ));
    assert_eq!(
        transcript.timings.vad,
        StageTiming::skipped(SkipReason::NoSpeechDetected)
    );
    assert!(h.injector.injected().is_empty());
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vad_no_speech_gate_skips_stt_entirely() {
    let vad = Arc::new(MockVad::returning(GateDecision::NoSpeech));
    let h = Harness::with(Setup::default().with_vad(vad.clone()).with_stt(Arc::new(
        MockStt::failing("STT must not run after VAD gate"),
    )))
    .await;
    let mut client = h.client().await;
    client.subscribe().await;
    client
        .request(Command::StartDictation {
            mode: dictate_proto::DictationMode::Toggle,
            options: None,
        })
        .await
        .unwrap();
    client.wait_for_state(State::Recording).await;
    client.request(Command::Stop).await.unwrap();
    let transcript = client.wait_for_final().await;
    assert_eq!(transcript.text.as_str(), "");
    assert!(matches!(transcript.timings.vad, StageTiming::Ran { .. }));
    assert!(matches!(
        transcript.timings.stt,
        StageTiming::Skipped {
            reason: SkipReason::NoSpeechDetected
        }
    ));
    assert_eq!(vad.gate_count(), 1);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_shot_session_auto_stops_after_vad_trailing_silence() {
    let vad = Arc::new(
        MockVad::returning(GateDecision::Speech {
            samples: vec![0.2; 512],
            leading_trimmed_ms: 0.0,
            trailing_trimmed_ms: 0.0,
        })
        .auto_stopping(),
    );
    let h = Harness::with(Setup::default().with_vad(vad)).await;
    let mut client = h.client().await;
    client.subscribe().await;
    client
        .request(Command::StartDictation {
            mode: dictate_proto::DictationMode::OneShot,
            options: None,
        })
        .await
        .unwrap();
    assert_eq!(client.wait_for_terminal().await, State::Done);
    assert_eq!(h.injector.injected(), vec!["hello there".to_string()]);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_shot_snapshot_failure_falls_back_to_explicit_stop() {
    let audio = Arc::new(MockAudio::with_seconds(1.5).without_snapshots());
    let vad = Arc::new(
        MockVad::returning(GateDecision::Speech {
            samples: vec![0.2; 512],
            leading_trimmed_ms: 0.0,
            trailing_trimmed_ms: 0.0,
        })
        .auto_stopping(),
    );
    let h = Harness::with(Setup::default().with_audio(audio).with_vad(vad)).await;
    let mut client = h.client().await;
    client.subscribe().await;
    client
        .request(Command::StartDictation {
            mode: dictate_proto::DictationMode::OneShot,
            options: None,
        })
        .await
        .unwrap();
    client.wait_for_state(State::Recording).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    client
        .request(Command::Stop)
        .await
        .expect("explicit stop remains available");
    assert_eq!(client.wait_for_terminal().await, State::Done);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_transcription_failure_ends_the_session_in_error() {
    let h = Harness::with(
        Setup::default().with_stt(Arc::new(MockStt::failing("CUDA device fell off the bus"))),
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
    assert!(
        h.injector.injected().is_empty(),
        "a failed session must not inject anything"
    );
    h.stop().await;
}

// ---------------------------------------------------------------------------
// 2. Cancellation, from every non-terminal state
// ---------------------------------------------------------------------------

/// Assert the invariants every cancellation must satisfy, whichever state it
/// arrived in: nothing typed, and the capture stream released.
async fn assert_cancelled_cleanly(h: &Harness) {
    assert!(
        h.injector.injected().is_empty(),
        "a cancelled session must never leave injected text"
    );
    eventually("the capture stream to be released", || {
        !h.audio.is_recording() && h.audio.cancel_count() > 0
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_from_recording() {
    let h = Harness::start().await;
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

    assert!(matches!(
        client.request(Command::Cancel).await.expect("cancel"),
        CommandResult::SessionCancelled { .. }
    ));
    assert_eq!(client.wait_for_terminal().await, State::Cancelled);
    assert_cancelled_cleanly(&h).await;
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_from_transcribing() {
    let gate = Gate::closed();
    let h = Harness::with(Setup::default().with_stt(Arc::new(
        MockStt::returning("hello there").with_gate(gate.clone()),
    )))
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
    client.wait_for_state(State::Transcribing).await;

    // The pipeline is now parked inside the recognizer.
    client.request(Command::Cancel).await.expect("cancel");
    gate.open();

    assert_eq!(client.wait_for_terminal().await, State::Cancelled);
    assert_cancelled_cleanly(&h).await;
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_from_formatting() {
    let h = Harness::with(Setup::default().with_formatter(Arc::new(
        MockFormatter::default().with_delay(Duration::from_secs(30)),
    )))
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
    client.wait_for_state(State::Formatting).await;

    client.request(Command::Cancel).await.expect("cancel");
    assert_eq!(client.wait_for_terminal().await, State::Cancelled);
    assert_cancelled_cleanly(&h).await;
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_from_injecting_before_the_commit_point_types_nothing() {
    // The availability probe runs inside `injecting` but before the commit
    // point, so this parks the pipeline exactly where a cancel must still win.
    let gate = Gate::closed();
    let injector = Arc::new(MockInjector::new().with_availability_gate(gate.clone()));
    let h = Harness::with(Setup::default().with_injector(injector)).await;
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
    client.wait_for_state(State::Injecting).await;

    client
        .request(Command::Cancel)
        .await
        .expect("cancel must be accepted while injecting, pre-commit");
    gate.open();

    assert_eq!(client.wait_for_terminal().await, State::Cancelled);
    assert_cancelled_cleanly(&h).await;
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_after_the_commit_point_is_refused_rather_than_lying() {
    // The other side of the same race: injection has begun and cannot be
    // undone, so the honest answer is `conflict`, not a successful cancel.
    let gate = Gate::closed();
    let injector = Arc::new(MockInjector::new().with_gate(gate.clone()));
    let h = Harness::with(Setup::default().with_injector(injector)).await;
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
    client.wait_for_state(State::Injecting).await;

    // Wait until the injector is genuinely inside `inject` — that is, past the
    // commit point. The gate's arrival counter makes this exact rather than a
    // sleep long enough to "probably" be right.
    gate.wait_entered().await;

    let err = client
        .request(Command::Cancel)
        .await
        .expect_err("a committed injection must not be cancellable");
    assert_eq!(err.code, ErrorCode::Conflict);
    assert!(
        err.message.contains("committed"),
        "the caller must be told why: {}",
        err.message
    );

    gate.open();
    assert_eq!(client.wait_for_terminal().await, State::Done);
    assert_eq!(
        h.injector.injected(),
        vec!["hello there".to_string()],
        "the injection that beat the cancel must have completed, not been torn in half"
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_with_no_session_reports_no_active_session() {
    let h = Harness::start().await;
    let mut client = h.client().await;
    let err = client.request(Command::Cancel).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::NoActiveSession);

    let err = client.request(Command::Stop).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::NoActiveSession);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancelled_session_leaves_the_daemon_ready_for_the_next_one() {
    let h = Harness::start().await;
    let mut client = h.client().await;
    client.subscribe().await;

    for _ in 0..3 {
        client
            .request(Command::StartDictation {
                mode: dictate_proto::DictationMode::Toggle,
                options: None,
            })
            .await
            .expect("start");
        client.wait_for_state(State::Recording).await;
        client.request(Command::Cancel).await.expect("cancel");
        assert_eq!(client.wait_for_terminal().await, State::Cancelled);
        client.wait_for_state(State::Idle).await;
    }
    h.stop().await;
}

// ---------------------------------------------------------------------------
// 3. Concurrent-command safety
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_racing_starts_produce_exactly_one_session() {
    let h = Harness::start().await;
    let mut a = h.client().await;
    let mut b = h.client().await;

    let start = || Command::StartDictation {
        mode: dictate_proto::DictationMode::Toggle,
        options: None,
    };
    let (ra, rb) = tokio::join!(a.request(start()), b.request(start()));

    let outcomes = [ra, rb];
    let started = outcomes.iter().filter(|r| r.is_ok()).count();
    let busy = outcomes
        .iter()
        .filter_map(|r| r.as_ref().err())
        .filter(|e| e.code == ErrorCode::Busy)
        .count();

    assert_eq!(started, 1, "exactly one racing start may win");
    assert_eq!(
        busy, 1,
        "the loser must be told `busy`, not silently dropped"
    );

    let loser = outcomes
        .iter()
        .find_map(|r| r.as_ref().err())
        .expect("a busy error");
    assert_eq!(
        loser.retry_after_ms,
        Some(500),
        "`busy` is retryable and must say when"
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_racing_protocol_toggles_start_exactly_one_session() {
    // Each request is one atomic mailbox operation. One sees idle and starts;
    // the other sees that foreign client's recording session and is refused by
    // session ownership. A status-then-act client could instead race into a
    // second start attempt after observing stale idle state.
    let h = Harness::start().await;
    let mut a = h.client().await;
    let mut b = h.client().await;
    let (ra, rb) = tokio::join!(a.request(Command::Toggle), b.request(Command::Toggle));
    let outcomes = [ra, rb];
    assert_eq!(
        outcomes.iter().filter(|result| result.is_ok()).count(),
        1,
        "exactly one racing toggle may create a session"
    );
    assert_eq!(
        outcomes
            .iter()
            .filter_map(|result| result.as_ref().err())
            .filter(|error| error.code == ErrorCode::Forbidden)
            .count(),
        1,
        "the other client's toggle must not stop a session it does not own"
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_concurrent_starts_still_produce_exactly_one_session() {
    let h = Harness::start().await;

    // Eight independent connections, each on its own task, all firing at once.
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let socket = h.socket.clone();
        tasks.push(tokio::spawn(async move {
            let mut client = harness::Client::connect(&socket).await;
            client.handshake().await;
            client
                .request(Command::StartDictation {
                    mode: dictate_proto::DictationMode::Toggle,
                    options: None,
                })
                .await
        }));
    }

    let mut started = 0;
    let mut busy = 0;
    for task in tasks {
        match task.await.expect("task must not panic") {
            Ok(_) => started += 1,
            Err(e) if e.code == ErrorCode::Busy => busy += 1,
            Err(e) => panic!("unexpected error {:?}: {}", e.code, e.message),
        }
    }

    assert_eq!(
        started, 1,
        "the engine's single mailbox must serialize all eight into one winner"
    );
    assert_eq!(
        busy, 7,
        "every loser must be told `busy`, not silently dropped"
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_arriving_immediately_after_start_is_not_lost() {
    // Regression: the session task parks on the stop signal only *after*
    // raising the recording notice and pausing media. A `Stop` that lands in
    // that window was dropped by `notify_waiters`, and the session hung in
    // `recording` until the daemon was restarted. A hotkey-driven
    // push-to-talk tap is exactly this timing.
    for _ in 0..25 {
        let h = Harness::start().await;
        let mut client = h.client().await;
        client.subscribe().await;

        client
            .request(Command::StartDictation {
                mode: dictate_proto::DictationMode::PushToTalk,
                options: None,
            })
            .await
            .expect("start");
        // Deliberately no wait for `recording`: stop as fast as the socket
        // will carry it.
        client.request(Command::Stop).await.expect("stop");

        assert_eq!(
            client.wait_for_terminal().await,
            State::Done,
            "a stop racing the session task's first park must still be seen"
        );
        h.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancel_arriving_immediately_after_start_is_not_lost() {
    // The same window, for the other verb.
    for _ in 0..25 {
        let h = Harness::start().await;
        let mut client = h.client().await;
        client.subscribe().await;

        client
            .request(Command::StartDictation {
                mode: dictate_proto::DictationMode::Toggle,
                options: None,
            })
            .await
            .expect("start");
        client.request(Command::Cancel).await.expect("cancel");

        assert_eq!(client.wait_for_terminal().await, State::Cancelled);
        assert!(h.injector.injected().is_empty());
        h.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_and_a_cancel_racing_resolve_as_cancelled() {
    // Both are pending; the user asked for the audio to be thrown away, so
    // cancellation must win rather than the pipeline transcribing it anyway.
    for _ in 0..25 {
        let h = Harness::start().await;
        let mut owner = h.client().await;
        owner.subscribe().await;
        owner
            .request(Command::StartDictation {
                mode: dictate_proto::DictationMode::Toggle,
                options: None,
            })
            .await
            .expect("start");

        let (stop, cancel) = tokio::join!(
            async {
                let mut c = harness::Client::connect(&h.socket).await;
                c.handshake().await;
                c.request(Command::Cancel).await
            },
            owner.request(Command::Stop),
        );
        let _ = (stop, cancel);

        let terminal = owner.wait_for_terminal().await;
        assert!(
            matches!(terminal, State::Cancelled | State::Done),
            "a stop/cancel race must resolve to a defined terminal state, got {terminal:?}"
        );
        if terminal == State::Cancelled {
            assert!(
                h.injector.injected().is_empty(),
                "a cancelled session must not have injected"
            );
        }
        h.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_stop_is_invalid_state_rather_than_a_silent_no_op() {
    let gate = Gate::closed();
    let h = Harness::with(Setup::default().with_stt(Arc::new(
        MockStt::returning("hello there").with_gate(gate.clone()),
    )))
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
    client.request(Command::Stop).await.expect("first stop");

    let err = client.request(Command::Stop).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidState);

    gate.open();
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_disconnect_orphans_the_session_so_the_next_invocation_can_finish_it() {
    // This is what makes `dictate toggle` work: the process that starts the
    // session exits immediately, and a *different* process stops it.
    let h = Harness::start().await;

    let mut starter = h.client().await;
    starter
        .request(Command::StartDictation {
            mode: dictate_proto::DictationMode::Toggle,
            options: None,
        })
        .await
        .expect("start");
    starter.disconnect().await;

    let mut stopper = h.client().await;
    stopper.subscribe().await;
    stopper
        .request(Command::Stop)
        .await
        .expect("an orphaned session must be stoppable by the next trusted client");

    let transcript = stopper.wait_for_final().await;
    assert_eq!(transcript.text.as_str(), "hello there");
    assert_eq!(h.injector.injected(), vec!["hello there".to_string()]);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_disconnect_mid_pipeline_does_not_disturb_the_running_session() {
    let gate = Gate::closed();
    let h = Harness::with(Setup::default().with_stt(Arc::new(
        MockStt::returning("hello there").with_gate(gate.clone()),
    )))
    .await;

    let mut watcher = h.client().await;
    watcher.subscribe().await;

    let mut starter = h.client().await;
    starter
        .request(Command::StartDictation {
            mode: dictate_proto::DictationMode::Toggle,
            options: None,
        })
        .await
        .expect("start");
    watcher.wait_for_state(State::Recording).await;
    starter.request(Command::Stop).await.expect("stop");
    watcher.wait_for_state(State::Transcribing).await;

    // The owner vanishes while the pipeline is mid-flight.
    starter.disconnect().await;
    gate.open();

    assert_eq!(
        watcher.wait_for_terminal().await,
        State::Done,
        "an orphaned session must run to completion, not be cancelled"
    );
    h.stop().await;
}

// ---------------------------------------------------------------------------
// 4. Session ownership
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_client_cannot_cancel_the_first_clients_session() {
    let h = Harness::start().await;
    let mut owner = h.client().await;
    let mut intruder = h.client().await;

    owner
        .request(Command::StartDictation {
            mode: dictate_proto::DictationMode::Toggle,
            options: None,
        })
        .await
        .expect("start");

    let err = intruder.request(Command::Cancel).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Forbidden);
    assert!(err.message.contains("another client"));

    let err = intruder.request(Command::Stop).await.unwrap_err();
    assert_eq!(
        err.code,
        ErrorCode::Forbidden,
        "stop is as dangerous as cancel and must be gated the same way"
    );

    // The owner is unaffected.
    owner
        .request(Command::Cancel)
        .await
        .expect("the owner may cancel");
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_owner_can_stop_its_own_session_while_others_cannot() {
    let h = Harness::start().await;
    let mut owner = h.client().await;
    owner.subscribe().await;
    let mut intruder = h.client().await;

    owner
        .request(Command::StartDictation {
            mode: dictate_proto::DictationMode::Toggle,
            options: None,
        })
        .await
        .expect("start");
    owner.wait_for_state(State::Recording).await;

    assert_eq!(
        intruder.request(Command::Stop).await.unwrap_err().code,
        ErrorCode::Forbidden
    );
    owner
        .request(Command::Stop)
        .await
        .expect("the owner may stop");
    assert_eq!(owner.wait_for_terminal().await, State::Done);
    h.stop().await;
}

// ---------------------------------------------------------------------------
// 5. The signal shim
// ---------------------------------------------------------------------------

/// Drive a whole dictation through the socket and report the states seen.
async fn states_via_protocol(h: &Harness) -> Vec<State> {
    let mut client = h.client().await;
    client.subscribe().await;
    client.request(Command::Toggle).await.expect("toggle start");
    client.wait_for_state(State::Recording).await;
    client.request(Command::Toggle).await.expect("toggle stop");
    let mut seen = vec![State::Recording];
    seen.extend(client.wait_for_state(State::Done).await);
    seen
}

/// The same dictation, driven by SIGUSR1 twice.
async fn states_via_signal(h: &Harness) -> Vec<State> {
    use dictate_core::ResolvedOptions;
    use dictated::signals::{apply, classify, SignalAction};

    let mut client = h.client().await;
    client.subscribe().await;
    let engine = h.daemon.engine().clone();
    let options = ResolvedOptions::default();

    assert_eq!(
        classify(signal_hook::consts::signal::SIGUSR1),
        SignalAction::Toggle
    );
    apply(SignalAction::Toggle, &engine, &options).await;
    client.wait_for_state(State::Recording).await;
    apply(SignalAction::Toggle, &engine, &options).await;

    let mut seen = vec![State::Recording];
    seen.extend(client.wait_for_state(State::Done).await);
    seen
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sigusr1_produces_the_same_state_transitions_as_the_protocol_commands() {
    let via_protocol = {
        let h = Harness::start().await;
        let states = states_via_protocol(&h).await;
        h.stop().await;
        states
    };
    let via_signal = {
        let h = Harness::start().await;
        let states = states_via_signal(&h).await;
        h.stop().await;
        states
    };

    assert_eq!(
        via_protocol, via_signal,
        "the signal shim must be the same code path, not a parallel implementation"
    );
    assert!(via_signal.contains(&State::Done));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sigusr2_cancels_exactly_as_the_cancel_command_does() {
    use dictate_core::ResolvedOptions;
    use dictated::signals::{apply, classify, SignalAction};

    let h = Harness::start().await;
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

    assert_eq!(
        classify(signal_hook::consts::signal::SIGUSR2),
        SignalAction::Cancel
    );
    apply(
        SignalAction::Cancel,
        h.daemon.engine(),
        &ResolvedOptions::default(),
    )
    .await;

    assert_eq!(client.wait_for_terminal().await, State::Cancelled);
    assert_cancelled_cleanly(&h).await;
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_signal_may_cancel_a_session_a_connected_client_owns() {
    // The hotkey is the physical control surface of this machine. A client
    // holding a session must not be able to veto it.
    use dictate_core::ResolvedOptions;
    use dictated::signals::{apply, SignalAction};

    let h = Harness::start().await;
    let mut owner = h.client().await;
    owner.subscribe().await;
    owner
        .request(Command::StartDictation {
            mode: dictate_proto::DictationMode::Toggle,
            options: None,
        })
        .await
        .expect("start");
    owner.wait_for_state(State::Recording).await;

    apply(
        SignalAction::Cancel,
        h.daemon.engine(),
        &ResolvedOptions::default(),
    )
    .await;
    assert_eq!(owner.wait_for_terminal().await, State::Cancelled);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_real_sigusr1_delivered_to_this_process_starts_a_session() {
    // The unit tests prove the mapping; this proves the wiring — that a signal
    // actually delivered by the kernel reaches the engine, which is what
    // `scripts/dictate-toggle` depends on.
    use dictate_core::ResolvedOptions;

    let h = Harness::start().await;
    let mut client = h.client().await;
    client.subscribe().await;

    let engine = h.daemon.engine().clone();
    let listener = tokio::spawn(async move {
        let _ = dictated::signals::listen(engine, ResolvedOptions::default()).await;
    });
    // Let the handler install before raising, or the default action for
    // SIGUSR1 (terminate) would take down the test binary.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // SAFETY: `raise` delivers a signal to this process. The handler for
    // SIGUSR1 is installed above.
    unsafe {
        libc::raise(libc::SIGUSR1);
    }
    assert_eq!(
        client.wait_for_state(State::Recording).await,
        vec![State::Recording],
        "a kernel-delivered SIGUSR1 must start recording"
    );

    // SAFETY: as above; SIGUSR2 is handled by the same listener.
    unsafe {
        libc::raise(libc::SIGUSR2);
    }
    assert_eq!(client.wait_for_terminal().await, State::Cancelled);

    // SAFETY: SIGTERM ends the listener task cleanly.
    unsafe {
        libc::raise(libc::SIGTERM);
    }
    let _ = tokio::time::timeout(Duration::from_secs(5), listener).await;
    h.stop().await;
}

// ---------------------------------------------------------------------------
// 6. Protocol conformance
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn commands_before_the_handshake_are_refused() {
    let h = Harness::start().await;
    let mut client = h.raw_client().await;
    let err = client.request(Command::GetStatus).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::HandshakeRequired);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_handshake_advertises_local_trusted_capabilities_with_explicit_routes() {
    let h = Harness::start().await;
    let mut client = h.raw_client().await;
    let hello = client.handshake().await;

    assert_eq!(hello.protocol_version, dictate_proto::PROTOCOL_VERSION);
    assert_eq!(hello.server.name, "dictated");
    assert!(hello.capabilities.features.host_capture);
    assert!(
        !hello.capabilities.routes.is_empty(),
        "routes are deny-by-default; an empty list would refuse everything"
    );
    assert!(hello.capabilities.allows_route(&dictate_proto::Route::Type));
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unknown_command_is_answered_rather_than_dropped() {
    // The asymmetry the protocol is built on: a caller must never be left
    // waiting for an effect that will never happen.
    let h = Harness::start().await;
    let mut client = h.client().await;

    // Bypass the typed client: `Command` cannot represent an unknown command.
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let stream = tokio::net::UnixStream::connect(&h.socket).await.unwrap();
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);

    let hello = r#"{"kind":"request","v":1,"id":1,"command":{"type":"handshake","protocol_version":1,"supported_versions":[1],"client":{"name":"raw","kind":"cli"}}}"#;
    write
        .write_all(format!("{hello}\n").as_bytes())
        .await
        .unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    write
        .write_all(b"{\"kind\":\"request\",\"v\":1,\"id\":2,\"command\":{\"type\":\"teleport\"}}\n")
        .await
        .unwrap();
    line.clear();
    reader.read_line(&mut line).await.unwrap();
    let msg: dictate_proto::Message = serde_json::from_str(&line).unwrap();
    match msg {
        dictate_proto::Message::Response(r) => {
            assert_eq!(r.id, dictate_proto::RequestId::Number(2));
            let e = r.outcome.error().expect("an error");
            assert_eq!(e.code, ErrorCode::UnsupportedCommand);
        }
        other => panic!("expected a response, got {other:?}"),
    }

    // The daemon is still healthy afterwards.
    assert!(client.request(Command::GetStatus).await.is_ok());
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn commands_whose_backing_feature_does_not_exist_answer_unsupported_command() {
    let h = Harness::start().await;
    let mut client = h.client().await;

    for command in [
        Command::ListDictionary {
            query: None,
            limit: None,
        },
        Command::ListSnippets {
            query: None,
            limit: None,
        },
        Command::GetConfig { path: None },
    ] {
        let name = command.name();
        let err = client.request(command).await.unwrap_err();
        assert_eq!(
            err.code,
            ErrorCode::UnsupportedCommand,
            "{name} must say this build cannot do it, not that permission is \
             missing — a client told `forbidden` goes looking for a setting"
        );
    }
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn get_status_tracks_the_session_and_reports_this_connections_capabilities() {
    let h = Harness::start().await;
    let mut client = h.client().await;
    client.subscribe().await;

    let status = match client.request(Command::GetStatus).await.expect("status") {
        CommandResult::Status(s) => s,
        other => panic!("expected status, got {other:?}"),
    };
    assert_eq!(status.state, State::Idle);
    assert!(status.session.is_none());
    assert_eq!(status.daemon.name, "dictated");
    assert_eq!(
        status.daemon.protocol_version,
        dictate_proto::PROTOCOL_VERSION
    );
    assert_eq!(status.daemon.pid, Some(std::process::id()));
    assert!(status.capabilities.features.host_capture);
    assert_eq!(status.model.as_ref().map(|m| m.name.as_str()), Some("mock"));

    client
        .request(Command::StartDictation {
            mode: dictate_proto::DictationMode::PushToTalk,
            options: None,
        })
        .await
        .expect("start");
    client.wait_for_state(State::Recording).await;

    let status = match client.request(Command::GetStatus).await.expect("status") {
        CommandResult::Status(s) => s,
        other => panic!("expected status, got {other:?}"),
    };
    assert_eq!(status.state, State::Recording);
    let session = status.session.expect("a session summary");
    assert_eq!(session.state, State::Recording);
    assert_eq!(session.mode, dictate_proto::DictationMode::PushToTalk);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_subscribers_both_see_the_whole_session() {
    let h = Harness::start().await;
    let mut a = h.client().await;
    let mut b = h.client().await;
    a.subscribe().await;
    b.subscribe().await;

    a.request(Command::StartDictation {
        mode: dictate_proto::DictationMode::Toggle,
        options: None,
    })
    .await
    .expect("start");
    a.wait_for_state(State::Recording).await;
    b.wait_for_state(State::Recording).await;

    a.request(Command::Stop).await.expect("stop");
    assert_eq!(a.wait_for_final().await.text.as_str(), "hello there");
    assert_eq!(
        b.wait_for_final().await.text.as_str(),
        "hello there",
        "a second subscriber must receive the same final event"
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unsubscribed_client_receives_no_events_but_still_gets_responses() {
    let h = Harness::start().await;
    let mut client = h.client().await;
    // Deliberately no subscribe.
    client
        .request(Command::StartDictation {
            mode: dictate_proto::DictationMode::Toggle,
            options: None,
        })
        .await
        .expect("start");
    client.request(Command::Cancel).await.expect("cancel");
    assert!(
        client.request(Command::GetStatus).await.is_ok(),
        "an unsubscribed connection must stay fully usable"
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn asking_to_inject_without_the_capability_is_forbidden_not_downgraded() {
    let mut caps = dictated::server::local_capabilities(true);
    caps.features.text_injection = false;
    let h = Harness::with(Setup::default().with_capabilities(caps)).await;
    let mut client = h.client().await;

    let err = client
        .request(Command::StartDictation {
            mode: dictate_proto::DictationMode::Toggle,
            options: Some(SessionOptions {
                inject: Some(true),
                ..SessionOptions::default()
            }),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Forbidden);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_that_takes_delivery_gets_text_and_the_host_types_nothing() {
    let h = Harness::start().await;
    let mut client = h.client().await;
    client.subscribe().await;

    client
        .request(Command::StartDictation {
            mode: dictate_proto::DictationMode::Toggle,
            options: Some(SessionOptions {
                inject: Some(false),
                ..SessionOptions::default()
            }),
        })
        .await
        .expect("start");
    client.wait_for_state(State::Recording).await;
    client.request(Command::Stop).await.expect("stop");

    let transcript = client.wait_for_final().await;
    assert_eq!(transcript.text.as_str(), "hello there");
    assert_eq!(
        transcript.injection,
        InjectionOutcome::Delivered,
        "taking delivery is a success, not a skip and not a failure"
    );
    assert!(
        h.injector.injected().is_empty(),
        "nothing may be typed on the host"
    );
    h.stop().await;
}

// ---------------------------------------------------------------------------
// 7. Honest timings
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn per_stage_timings_distinguish_ran_skipped_and_absent() {
    let h = Harness::with(
        // "hi" is below the formatter's minimum word count in the real
        // corrector; the mock is configured off so the skip is explicit.
        Setup::default().with_formatter(Arc::new(MockFormatter::disabled())),
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

    let t = client.wait_for_final().await.timings;

    assert!(
        matches!(t.capture, StageTiming::Ran { .. }),
        "capture ran: {:?}",
        t.capture
    );
    assert!(
        matches!(t.stt, StageTiming::Ran { .. }),
        "stt ran: {:?}",
        t.stt
    );
    assert!(
        matches!(t.inject, StageTiming::Ran { .. }),
        "inject ran: {:?}",
        t.inject
    );
    assert!(
        matches!(t.vad, StageTiming::Ran { .. }),
        "the configured VAD pass-through still executes and must report a measured stage: {:?}",
        t.vad
    );
    assert_eq!(
        t.fmt_llm,
        StageTiming::skipped(SkipReason::Disabled),
        "a stage skipped by a rule must say `skipped`, never `ran{{0.0}}`"
    );
    assert_eq!(
        t.fmt_rules,
        StageTiming::NotReported,
        "the rules pass is accounted inside `stt` and has no separate measurement"
    );

    let total = t.total_ms.expect("total_ms is reported, not derived");
    assert!(total > 0.0);
    assert!(
        total >= t.measured_ms() - 1.0,
        "total ({total}) must cover the stage sum ({})",
        t.measured_ms()
    );
    assert!(t.audio_ms.expect("audio_ms enables a real-time factor") > 0.0);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_formatting_pass_that_burns_time_and_fails_reports_failed_not_ran() {
    let h = Harness::with(
        Setup::default().with_formatter(Arc::new(MockFormatter::failing_after(
            Duration::from_millis(20),
            "ollama refused the connection",
        ))),
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

    let transcript = client.wait_for_final().await;
    match &transcript.timings.fmt_llm {
        StageTiming::Failed { ms, error } => {
            assert!(*ms >= 20.0, "the burned time must stay in the budget");
            assert_eq!(error.as_deref(), Some("ollama refused the connection"));
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert_eq!(
        transcript.text.as_str(),
        "hello there",
        "the pass fails open: the text survives"
    );
    h.stop().await;
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn query_history_returns_the_session_that_just_ran() {
    let h = Harness::with(Setup::default().with_history()).await;
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
    client.wait_for_state(State::Done).await;

    let page = match client
        .request(Command::QueryHistory {
            query: dictate_proto::HistoryQuery::default(),
        })
        .await
        .expect("history")
    {
        CommandResult::History(p) => p,
        other => panic!("expected history, got {other:?}"),
    };

    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].text.as_deref(), Some("hello there"));
    assert_eq!(page.items[0].route, dictate_proto::Route::Type);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_private_daemon_session_leaves_zero_history_rows() {
    let h = Harness::with(Setup::default().with_history()).await;
    let mut client = h.client().await;
    client.subscribe().await;
    client
        .request(Command::StartDictation {
            mode: dictate_proto::DictationMode::Toggle,
            options: Some(SessionOptions {
                privacy: Some(true),
                ..Default::default()
            }),
        })
        .await
        .expect("start private dictation");
    client.wait_for_state(State::Recording).await;
    client.request(Command::Stop).await.expect("stop");
    client.wait_for_state(State::Done).await;

    let count: i64 = h.history.lock().unwrap().connection().query_row(
        "SELECT COUNT(*) FROM interactions", [], |row| row.get(0),
    ).unwrap();
    assert_eq!(count, 0, "privacy must skip the history insert entirely");
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn history_analytics_and_purge_are_available_over_the_socket() {
    let h = Harness::with(Setup::default().with_history()).await;
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
    client.wait_for_state(State::Done).await;
    let analytics = match client.request(Command::GetHistoryAnalytics).await.expect("analytics") {
        CommandResult::HistoryAnalytics(analytics) => analytics,
        other => panic!("expected history analytics, got {other:?}"),
    };
    assert_eq!(analytics.words_today, 2);
    assert!(analytics.overall_wpm.is_some());
    assert!(matches!(client.request(Command::PurgeHistory).await, Ok(CommandResult::Ack)));
    let count: i64 = h.history.lock().unwrap().connection().query_row(
        "SELECT COUNT(*) FROM interactions", [], |row| row.get(0),
    ).unwrap();
    assert_eq!(count, 0);
    h.stop().await;
}

// ---------------------------------------------------------------------------
// Notifications (the user-visible side effects the pipeline still owes)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancelled_session_tells_the_user_it_was_cancelled() {
    let h = Harness::start().await;
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
    client.request(Command::Cancel).await.expect("cancel");
    client.wait_for_terminal().await;

    eventually("a cancellation notice", || {
        h.notifier.notices().contains(&Notice::Cancelled)
    })
    .await;
    h.stop().await;
}
