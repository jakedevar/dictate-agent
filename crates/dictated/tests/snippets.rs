//! S24 through a real daemon: control-plane CRUD, expansion in a live
//! dictation, route safety, privacy and the upload gate for host variables.
mod harness;
use dictate_core::ports::mock::MockStt;
use dictate_proto::{
    AudioFormat, AudioSource, Command, CommandResult, ErrorCode, SessionOptions, Snippet,
};
use harness::{Harness, Setup};
use std::sync::Arc;

fn snippet(trigger: &str, expansion: &str) -> Snippet {
    Snippet::new(trigger, expansion)
}

async fn dictate(h: &Harness, options: SessionOptions) -> dictate_proto::Transcript {
    let mut c = h.client().await;
    c.subscribe().await;
    c.request(Command::StartDictation {
        mode: Default::default(),
        options: Some(options),
    })
    .await
    .unwrap();
    c.request(Command::Stop).await.unwrap();
    c.wait_for_final().await
}

#[tokio::test]
async fn snippet_crud_validation_and_live_snapshot() {
    let h = Harness::start().await;
    let mut c = h.client().await;
    let CommandResult::Snippet { snippet: saved } = c
        .request(Command::UpsertSnippet {
            snippet: snippet("work email", "jake@example.com"),
        })
        .await
        .unwrap()
    else {
        panic!("expected a snippet")
    };
    let id = saved.id.expect("server-assigned id");
    assert_eq!(saved.hit_count, Some(0));
    // The matcher snapshot is live the moment the command returns.
    assert_eq!(h.dictionary.find_snippets("work email", None).len(), 1);

    let CommandResult::Snippets { snippets } = c
        .request(Command::ListSnippets {
            query: Some("WORK".into()),
            limit: Some(5),
        })
        .await
        .unwrap()
    else {
        panic!("expected snippets")
    };
    assert_eq!(snippets, vec![saved.clone()]);

    for (bad, code) in [
        (snippet("Work  Email", "x"), ErrorCode::Conflict),
        (snippet(" ", "x"), ErrorCode::InvalidParams),
        (snippet("fine", ""), ErrorCode::InvalidParams),
    ] {
        let err = c
            .request(Command::UpsertSnippet { snippet: bad })
            .await
            .unwrap_err();
        assert_eq!(err.code, code);
    }
    let mut off = saved.clone();
    off.enabled = false;
    c.request(Command::UpsertSnippet { snippet: off })
        .await
        .unwrap();
    assert!(h.dictionary.find_snippets("work email", None).is_empty());

    assert_eq!(
        c.request(Command::DeleteSnippet { id }).await.unwrap(),
        CommandResult::Deleted { id }
    );
    assert_eq!(
        c.request(Command::DeleteSnippet { id })
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    assert_eq!(
        c.request(Command::DeleteSnippet { id: 0 })
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidParams
    );
    h.stop().await;
}

#[tokio::test]
async fn snippet_capabilities_deny_reads_and_writes() {
    let mut caps = dictated::server::host_capabilities(true, false);
    caps.features.snippets_read = false;
    caps.features.snippets_write = false;
    let h = Harness::with(Setup::default().with_capabilities(caps)).await;
    let mut c = h.client().await;
    for cmd in [
        Command::ListSnippets {
            query: None,
            limit: None,
        },
        Command::UpsertSnippet {
            snippet: snippet("sig", "S"),
        },
        Command::DeleteSnippet { id: 1 },
    ] {
        assert_eq!(c.request(cmd).await.unwrap_err().code, ErrorCode::Forbidden);
    }
    assert!(h.dictionary.list_snippets(None, None).is_empty());
    h.stop().await;
}

#[tokio::test]
async fn a_spoken_trigger_is_typed_as_its_expansion_verbatim() {
    let h = Harness::with(
        Setup::default()
            .with_stt(Arc::new(MockStt::returning("Insert work email.")))
            .with_history(),
    )
    .await;
    h.dictionary
        .upsert_snippet(snippet("insert work email", "  jake@example.com\n"))
        .unwrap();
    let t = dictate(&h, SessionOptions::default()).await;
    assert_eq!(t.text.as_str(), "  jake@example.com\n");
    assert_eq!(t.raw_text.as_deref(), Some("Insert work email."));
    assert_eq!(
        h.injector.injected(),
        vec!["  jake@example.com\n".to_string()]
    );
    h.stop().await;
}

#[tokio::test]
async fn an_expansion_that_looks_like_a_route_trigger_is_typed_not_routed() {
    for expansion in [
        "timer ten minutes",
        "edit: make it formal",
        "easy what is rust",
    ] {
        let h = Harness::with(Setup::default().with_stt(Arc::new(MockStt::returning("sig")))).await;
        h.dictionary
            .upsert_snippet(snippet("sig", expansion))
            .unwrap();
        let t = dictate(&h, SessionOptions::default()).await;
        assert_eq!(t.text.as_str(), expansion);
        assert_eq!(
            h.injector.injected(),
            vec![expansion.to_string()],
            "{expansion}"
        );
        h.stop().await;
    }
}

#[tokio::test]
async fn variables_resolve_in_a_live_session_and_hits_follow_privacy() {
    for (privacy, hits) in [(false, 1), (true, 0)] {
        let h = Harness::with(
            Setup::default()
                .with_stt(Arc::new(MockStt::returning("stamp")))
                .with_history(),
        )
        .await;
        h.dictionary
            .upsert_snippet(snippet("stamp", "Date: {date}"))
            .unwrap();
        let t = dictate(
            &h,
            SessionOptions {
                privacy: Some(privacy),
                ..Default::default()
            },
        )
        .await;
        let date = t.text.as_str().strip_prefix("Date: ").expect("prefix kept");
        assert_eq!(date.len(), 10, "{date}");
        assert!(date.chars().enumerate().all(|(i, c)| if i == 4 || i == 7 {
            c == '-'
        } else {
            c.is_ascii_digit()
        }));
        h.dictionary.flush_hits().unwrap();
        assert_eq!(
            h.dictionary.list_snippets(None, None)[0].hit_count,
            Some(hits)
        );
        h.stop().await;
    }
}

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
async fn an_upload_never_reads_the_clipboard_for_a_snippet_variable() {
    let h = Harness::with(Setup::default().with_stt(Arc::new(MockStt::returning("paste")))).await;
    h.dictionary
        .upsert_snippet(snippet("paste", "<{clipboard}|{selection}>"))
        .unwrap();
    let mut c = h.client().await;
    let CommandResult::Transcript(t) = c
        .request(Command::TranscribeAudio {
            audio: AudioSource::Inline {
                format: AudioFormat::wav(),
                data: tone_wav(),
            },
            options: None,
        })
        .await
        .unwrap()
    else {
        panic!("expected a transcript")
    };
    assert_eq!(
        t.text.as_str(),
        "<{clipboard}|{selection}>",
        "an upload has no user at this desktop; host state stays unread"
    );
    assert!(h.injector.injected().is_empty());
    h.stop().await;
}
