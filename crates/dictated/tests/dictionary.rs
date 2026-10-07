mod harness;
use dictate_core::ports::mock::MockStt;
use dictate_core::ports::{BoxFuture, SttProvider, SttRequest};
use dictate_proto::{Command, CommandResult, DictionaryEntry, ErrorCode, SessionOptions};
use harness::{Harness, Setup};
use std::sync::{Arc, Mutex};
fn entry() -> DictionaryEntry {
    let mut e = DictionaryEntry::new("Kubernetes");
    e.sounds_like = vec!["kubernetties".into()];
    e
}

#[tokio::test]
async fn dictionary_crud_validation_and_live_snapshot() {
    let h = Harness::start().await;
    let mut c = h.client().await;
    let CommandResult::DictionaryEntry { mut entry } = c
        .request(Command::UpsertDictionaryEntry { entry: entry() })
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(entry.id.is_some());
    let id = entry.id.unwrap();
    assert_eq!(entry.hit_count, Some(0));
    assert_eq!(h.dictionary.apply("kubernetties", None).text, "Kubernetes");
    let CommandResult::Dictionary { entries } = c
        .request(Command::ListDictionary {
            query: Some("kuber".into()),
            limit: Some(10),
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(entries.len(), 1);
    let err = c
        .request(Command::UpsertDictionaryEntry {
            entry: DictionaryEntry::new("kubernetes"),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    let err = c
        .request(Command::UpsertDictionaryEntry {
            entry: DictionaryEntry::new(" "),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidParams);
    entry.enabled = false;
    c.request(Command::UpsertDictionaryEntry { entry })
        .await
        .unwrap();
    assert_eq!(
        h.dictionary.apply("kubernetties", None).text,
        "kubernetties"
    );
    assert_eq!(
        c.request(Command::DeleteDictionaryEntry { id })
            .await
            .unwrap(),
        CommandResult::Deleted { id }
    );
    assert_eq!(
        c.request(Command::DeleteDictionaryEntry { id })
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    assert_eq!(
        c.request(Command::DeleteDictionaryEntry { id: 0 })
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidParams
    );
    h.stop().await;
}
#[tokio::test]
async fn dictionary_capabilities_deny_reads_writes_and_suggestion_history() {
    let mut caps = dictated::server::local_capabilities(true);
    caps.features.dictionary_read = false;
    caps.features.dictionary_write = false;
    let h = Harness::with(Setup::default().with_capabilities(caps)).await;
    let mut c = h.client().await;
    for cmd in [
        Command::ListDictionary {
            query: None,
            limit: None,
        },
        Command::UpsertDictionaryEntry { entry: entry() },
        Command::DeleteDictionaryEntry { id: 1 },
        Command::ListDictionarySuggestions { limit: None },
    ] {
        assert_eq!(c.request(cmd).await.unwrap_err().code, ErrorCode::Forbidden);
    }
    assert!(h.dictionary.list(None, None).is_empty());
    h.stop().await;
    let mut caps = dictated::server::local_capabilities(true);
    caps.features.history_read = false;
    let h = Harness::with(Setup::default().with_capabilities(caps)).await;
    let mut c = h.client().await;
    assert_eq!(
        c.request(Command::ListDictionarySuggestions { limit: None })
            .await
            .unwrap_err()
            .code,
        ErrorCode::Forbidden
    );
    h.stop().await;
}
#[tokio::test]
async fn suggestions_are_readonly_until_explicit_acceptance() {
    let h = Harness::with(Setup::default().with_history()).await;
    {
        let store = h.history.lock().unwrap();
        for day in [1, 1, 2] {
            let mut i = store.begin();
            i.timestamp = format!("2026-09-{day:02}T12:00:00+00:00");
            i.grammar_input = Some("deploy kubernetties".into());
            i.grammar_output = Some("deploy Kubernetes".into());
            i.corrected_transcription = i.grammar_output.clone();
            i.completed = true;
            store.commit(&i);
        }
    }
    let mut c = h.client().await;
    let CommandResult::DictionarySuggestions { suggestions } = c
        .request(Command::ListDictionarySuggestions { limit: None })
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(suggestions.len(), 1);
    assert_eq!(suggestions[0].count, 3);
    assert_eq!(suggestions[0].days, 2);
    assert!(h.dictionary.list(None, None).is_empty());
    c.request(Command::UpsertDictionaryEntry {
        entry: suggestions[0].entry.clone(),
    })
    .await
    .unwrap();
    assert_eq!(h.dictionary.apply("kubernetties", None).text, "Kubernetes");
    h.stop().await;
}
struct PromptStt {
    requests: Mutex<Vec<SttRequest>>,
    inner: MockStt,
}
impl SttProvider for PromptStt {
    fn transcribe<'a>(
        &'a self,
        samples: &'a [f32],
        request: SttRequest,
    ) -> BoxFuture<'a, anyhow::Result<Option<dictate_core::ports::Transcription>>> {
        self.requests.lock().unwrap().push(request.clone());
        self.inner.transcribe(samples, request)
    }
    fn model(&self) -> dictate_core::ports::ModelInfo {
        self.inner.model()
    }
}
#[tokio::test]
async fn recognizer_bias_scope_opt_out_and_private_hit_counts() {
    for (app, use_dictionary, privacy, expected, prompt, hits, global_privacy) in [
        (Some("slack"), true, false, "Kubernetes", true, 1, false),
        // Unreplaced text still passes through S20's rules (sentence casing).
        (None, true, false, "Kubernetties", false, 0, false),
        (Some("slack"), false, false, "Kubernetties", false, 0, false),
        (Some("slack"), true, true, "Kubernetes", true, 0, false),
        (Some("slack"), true, false, "Kubernetes", true, 0, true),
    ] {
        let stt = Arc::new(PromptStt {
            requests: Mutex::new(Vec::new()),
            inner: MockStt::returning("kubernetties"),
        });
        let h = Harness::with(Setup::default().with_stt(stt.clone()).with_history()).await;
        if global_privacy {
            *h.history.lock().unwrap() =
                dictate_history::HistoryStore::new(&dictate_history::HistoryConfig {
                    db_path: h
                        .socket
                        .parent()
                        .unwrap()
                        .join("private-history.db")
                        .to_string_lossy()
                        .into_owned(),
                    privacy_mode: true,
                    ..Default::default()
                })
                .unwrap();
        }
        let mut e = entry();
        e.apps = vec!["slack".into()];
        h.dictionary.upsert(e).unwrap();
        let mut c = h.client().await;
        c.subscribe().await;
        c.request(Command::StartDictation {
            mode: Default::default(),
            options: Some(SessionOptions {
                app: app.map(str::to_string),
                use_dictionary: Some(use_dictionary),
                privacy: Some(privacy),
                inject: Some(false),
                ..Default::default()
            }),
        })
        .await
        .unwrap();
        c.request(Command::Stop).await.unwrap();
        let transcript = c.wait_for_final().await;
        assert_eq!(transcript.text.as_str(), expected);
        assert_eq!(
            stt.requests.lock().unwrap()[0].initial_prompt.is_some(),
            prompt
        );
        if !privacy {
            assert_eq!(transcript.raw_text.as_deref(), Some("kubernetties"));
        }
        h.dictionary.flush_hits().unwrap();
        assert_eq!(h.dictionary.list(None, None)[0].hit_count, Some(hits));
        h.stop().await;
    }
}

// ---- a dictionary that cannot open must not stop the daemon ------------------

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("dictated-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn config_with_dictionary_at(
    path: &std::path::Path,
    enabled: bool,
) -> dictate_core::config::Config {
    let mut config = dictate_core::config::Config::default();
    config.dictionary.enabled = enabled;
    config.dictionary.db_path = path.to_string_lossy().into_owned();
    config
}

#[test]
fn an_unopenable_dictionary_degrades_to_no_dictionary() {
    let dir = scratch("dict-blocked");
    // A directory where the database file should be: SQLite cannot open it.
    let blocked = dir.join("dictionary.db");
    std::fs::create_dir(&blocked).unwrap();
    let config = config_with_dictionary_at(&blocked, true);
    assert!(dictated::open_dictionary(&config).is_none());
}

#[test]
fn a_healthy_dictionary_opens_and_a_disabled_one_is_never_opened() {
    let dir = scratch("dict-open");
    let path = dir.join("dictionary.db");
    assert!(dictated::open_dictionary(&config_with_dictionary_at(&path, true)).is_some());
    assert!(path.exists());

    let off = dir.join("never.db");
    assert!(dictated::open_dictionary(&config_with_dictionary_at(&off, false)).is_none());
    assert!(
        !off.exists(),
        "a disabled dictionary must not create its database"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_a_dictionary_dictation_works_and_the_capabilities_are_withdrawn() {
    let h = Harness::with(Setup::default().without_dictionary()).await;
    let mut c = h.raw_client().await;
    let hello = c.handshake().await;
    assert!(!hello.capabilities.features.dictionary_read);
    assert!(!hello.capabilities.features.dictionary_write);
    let err = c
        .request(Command::ListDictionary {
            query: None,
            limit: None,
        })
        .await
        .unwrap_err();
    assert_ne!(err.code, ErrorCode::Internal);
    h.stop().await;
}
