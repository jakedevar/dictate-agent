//! S35 scratchpad through the real daemon/socket: dictate a note, nothing is
//! typed, then list, search and delete it over the protocol.
mod harness;

use dictate_core::ports::mock::MockStt;
use dictate_core::ports::Notice;
use dictate_proto::{
    Command, CommandResult, DictationMode, ErrorCode, InjectionOutcome, Note, Route,
    SessionOptions, SkipReason, State,
};
use harness::{Harness, Setup};
use std::sync::Arc;

fn setup(said: &str) -> Setup {
    Setup::default()
        .with_history()
        .with_stt(Arc::new(MockStt::returning(said)))
}

async fn run(h: &Harness, options: Option<SessionOptions>) -> dictate_proto::Transcript {
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
    client.request(Command::Stop).await.unwrap();
    client.wait_for_final().await
}

async fn notes(h: &Harness, query: Option<&str>) -> Vec<Note> {
    let mut c = h.client().await;
    match c
        .request(Command::ListNotes {
            query: query.map(Into::into),
            limit: None,
            id: None,
        })
        .await
        .unwrap()
    {
        CommandResult::Notes { notes } => notes,
        other => panic!("expected notes, got {}", other.name()),
    }
}

fn note_route() -> Option<SessionOptions> {
    Some(SessionOptions {
        route: Some(Route::Note),
        ..Default::default()
    })
}

#[tokio::test]
async fn a_spoken_note_is_stored_and_never_typed() {
    let h = Harness::with(setup("note to self: buy milk and eggs")).await;
    let transcript = run(&h, None).await;

    assert_eq!(transcript.route, Route::Note);
    assert!(matches!(
        transcript.injection,
        InjectionOutcome::Skipped {
            reason: SkipReason::RouteNotEligible
        }
    ));
    assert!(
        h.injector.injected().is_empty() && h.injector.replacements().is_empty(),
        "a note must never reach the focused window"
    );

    let stored = notes(&h, None).await;
    assert_eq!(stored.len(), 1);
    // The trigger phrase is not part of the note.
    assert!(
        stored[0].text.to_lowercase().starts_with("buy milk"),
        "{:?}",
        stored[0].text
    );
    assert_eq!(transcript.text.as_str(), stored[0].text);
    assert_eq!(stored[0].word_count, 4);
    assert!(h
        .notifier
        .notices()
        .iter()
        .any(|n| matches!(n, Notice::NoteSaved(t) if *t == stored[0].text)));
    h.stop().await;
}

#[tokio::test]
async fn the_dictation_is_logged_as_a_note_route_in_history() {
    let h = Harness::with(setup("note: call the dentist")).await;
    run(&h, None).await;
    let route: String = h
        .history
        .lock()
        .unwrap()
        .connection()
        .query_row("SELECT route_type FROM interactions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(route, "note");
    h.stop().await;
}

#[tokio::test]
async fn a_forced_note_route_keeps_the_whole_utterance() {
    let h = Harness::with(setup("remember to water the plants")).await;
    let transcript = run(&h, note_route()).await;
    assert_eq!(transcript.route, Route::Note);
    let stored = notes(&h, None).await;
    assert_eq!(stored.len(), 1);
    assert!(stored[0].text.contains("water the plants"));
    assert!(h.injector.injected().is_empty());
    h.stop().await;
}

#[tokio::test]
async fn ordinary_prose_that_mentions_note_is_still_typed() {
    for said in [
        "note that the deadline moved",
        "I made a note of it",
        "Take a notebook",
    ] {
        let h = Harness::with(setup(said)).await;
        let transcript = run(&h, None).await;
        assert_eq!(transcript.route, Route::Type, "{said}");
        assert_eq!(h.injector.injected().len(), 1, "{said} must be typed");
        assert!(notes(&h, None).await.is_empty(), "{said} must not be noted");
        h.stop().await;
    }
}

#[tokio::test]
async fn privacy_mode_saves_nothing_and_says_so() {
    for (global, session) in [(true, None), (false, Some(true))] {
        let mut setup = setup("note: the private thing");
        setup.history_privacy = global;
        let h = Harness::with(setup).await;
        let transcript = run(
            &h,
            session.map(|privacy| SessionOptions {
                privacy: Some(privacy),
                ..Default::default()
            }),
        )
        .await;
        assert_eq!(transcript.route, Route::Note);
        assert!(matches!(
            transcript.injection,
            InjectionOutcome::Skipped {
                reason: SkipReason::NotPermitted
            }
        ));
        assert!(
            transcript.text.as_str().is_empty(),
            "nothing was saved, so no text is reported"
        );
        assert!(notes(&h, None).await.is_empty());
        assert!(h.injector.injected().is_empty(), "and it is not typed");
        assert!(h
            .notifier
            .notices()
            .iter()
            .any(|n| matches!(n, Notice::Error(m) if m.contains("privacy"))));
        // No trace of the words anywhere in the history database either.
        let leaked: i64 = h
            .history
            .lock()
            .unwrap()
            .connection()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM notes) + (SELECT COUNT(*) FROM interactions \
                 WHERE corrected_transcription LIKE '%private thing%')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(leaked, 0);
        h.stop().await;
    }
}

#[tokio::test]
async fn with_history_disabled_a_note_is_refused_not_typed() {
    let h =
        Harness::with(Setup::default().with_stt(Arc::new(MockStt::returning("note: x y")))).await;
    let transcript = run(&h, None).await;
    assert!(matches!(
        transcript.injection,
        InjectionOutcome::Skipped {
            reason: SkipReason::NotPermitted
        }
    ));
    assert!(h.injector.injected().is_empty());
    assert!(notes(&h, None).await.is_empty());
    h.stop().await;
}

#[tokio::test]
async fn a_trigger_with_nothing_after_it_is_not_a_note() {
    let h = Harness::with(setup("note:")).await;
    let transcript = run(&h, None).await;
    assert!(matches!(
        transcript.injection,
        InjectionOutcome::Skipped {
            reason: SkipReason::NoSpeechDetected
        }
    ));
    assert!(notes(&h, None).await.is_empty());
    assert!(h.injector.injected().is_empty());
    h.stop().await;
}

#[tokio::test]
async fn list_search_show_and_delete_over_the_protocol() {
    let h = Harness::with(setup("unused")).await;
    {
        let store = h.history.lock().unwrap();
        store.add_note("Buy oat milk").unwrap();
        store.add_note("Email the landlord").unwrap();
    }
    let all = notes(&h, None).await;
    assert_eq!(
        all.iter().map(|n| n.text.as_str()).collect::<Vec<_>>(),
        ["Email the landlord", "Buy oat milk"],
        "newest first"
    );
    assert_eq!(notes(&h, Some("MILK")).await.len(), 1);
    assert!(notes(&h, Some("zebra")).await.is_empty());

    let mut c = h.client().await;
    let one = match c
        .request(Command::ListNotes {
            query: None,
            limit: None,
            id: Some(all[1].id),
        })
        .await
        .unwrap()
    {
        CommandResult::Notes { notes } => notes,
        other => panic!("{}", other.name()),
    };
    assert_eq!(one, vec![all[1].clone()]);

    match c
        .request(Command::DeleteNote { id: all[1].id })
        .await
        .unwrap()
    {
        CommandResult::Deleted { id } => assert_eq!(id, all[1].id),
        other => panic!("{}", other.name()),
    }
    assert_eq!(notes(&h, None).await.len(), 1);
    assert_eq!(
        c.request(Command::DeleteNote { id: all[1].id })
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    h.stop().await;
}

#[tokio::test]
async fn a_connection_without_history_access_cannot_read_or_delete_notes() {
    let mut caps = dictated::server::local_capabilities(true);
    caps.features.history_read = false;
    caps.features.history_write = false;
    let h = Harness::with(setup("unused").with_capabilities(caps)).await;
    h.history.lock().unwrap().add_note("secret errand").unwrap();
    let mut c = h.client().await;
    for cmd in [
        Command::ListNotes {
            query: None,
            limit: None,
            id: None,
        },
        Command::DeleteNote { id: 1 },
    ] {
        assert_eq!(c.request(cmd).await.unwrap_err().code, ErrorCode::Forbidden);
    }
    assert_eq!(h.history.lock().unwrap().note_count().unwrap(), 1);
    h.stop().await;
}

#[tokio::test]
async fn a_connection_not_granted_the_note_route_cannot_store_one() {
    // Deny-by-default routes: a remote-style grant of `type` only must not be
    // able to write to the local scratchpad by saying the trigger.
    let mut caps = dictated::server::local_capabilities(true);
    caps.routes = vec![Route::Type];
    let h = Harness::with(setup("note: sneaky").with_capabilities(caps)).await;
    let mut client = h.client().await;
    client.subscribe().await;
    client
        .request(Command::StartDictation {
            mode: DictationMode::Toggle,
            options: None,
        })
        .await
        .unwrap();
    client.wait_for_state(State::Recording).await;
    client.request(Command::Stop).await.unwrap();
    client.wait_for_state(State::Error).await;
    assert!(h.history.lock().unwrap().note_count().unwrap() == 0);
    assert!(h.injector.injected().is_empty());
    h.stop().await;
}

#[tokio::test]
async fn a_cancelled_session_stores_no_note() {
    let h = Harness::with(setup("note: never mind")).await;
    let mut client = h.client().await;
    client.subscribe().await;
    client
        .request(Command::StartDictation {
            mode: DictationMode::Toggle,
            options: None,
        })
        .await
        .unwrap();
    client.wait_for_state(State::Recording).await;
    client.request(Command::Cancel).await.unwrap();
    client.wait_for_state(State::Cancelled).await;
    assert!(notes(&h, None).await.is_empty());
    h.stop().await;
}
