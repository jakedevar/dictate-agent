//! `diagnose` against a real daemon: every dependency gets a named check with a
//! verdict and a one-line fix, and the failure the June binary hid for six
//! weeks — a formatter whose model is not installed — is loud.
//!
//! Ollama is a tiny in-process HTTP server; the speech model is a file in a
//! temp directory. Nothing here needs a GPU, a network, or a display.

mod harness;

use std::sync::Arc;

use dictate_core::config::{Config, ConfigReport};
use dictate_core::ports::mock::{MockFormatter, MockStt};
use dictate_proto::{
    CheckStatus, Command, CommandResult, DiagnosticsReport, ErrorCode, FormatterHealth,
};
use harness::{Harness, Setup};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A fake Ollama that answers every request with `models` as its `/api/tags`.
async fn fake_ollama(models: &[&str]) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let body = serde_json::json!({
        "models": models.iter().map(|m| serde_json::json!({
            "name": m, "modified_at": "2026-01-01T00:00:00Z", "size": 1,
        })).collect::<Vec<_>>()
    })
    .to_string();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let body = body.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}")
}

/// A config whose paths all point into a private temp directory.
fn config_in(dir: &std::path::Path, ollama: &str, grammar_model: &str) -> Config {
    let mut c = Config::default();
    c.whisper.model_path = dir.join("no-such-model.bin").to_string_lossy().into_owned();
    c.whisper.device = "cpu".into();
    c.grammar.host = ollama.to_string();
    c.grammar.model = grammar_model.to_string();
    c.local.host = ollama.to_string();
    c.local.model = "local-model:1b".into();
    c.notifications.enabled = false;
    c
}

async fn diagnose(h: &Harness, quick: bool) -> DiagnosticsReport {
    let mut client = h.client().await;
    match client.request(Command::Diagnose { quick }).await.unwrap() {
        CommandResult::Diagnostics(report) => *report,
        other => panic!("expected diagnostics, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_daemon_without_a_doctor_says_so() {
    let h = Harness::start().await;
    let mut client = h.client().await;
    let err = client
        .request(Command::Diagnose { quick: true })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::CapabilityUnavailable);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_missing_formatter_model_is_a_failure_that_names_what_is_installed() {
    let ollama = fake_ollama(&["gemma4:12b", "qwen3.6:27b"]).await;
    let dir = std::env::temp_dir();
    let h = Harness::with(Setup::default().with_doctor(
        // The model the June config names; not installed.
        config_in(&dir, &ollama, "qwen3:14b"),
        ConfigReport::default(),
    ))
    .await;

    let report = diagnose(&h, true).await;

    let model = report.check("grammar_model").expect("grammar_model check");
    assert_eq!(model.status, CheckStatus::Fail);
    assert!(model.detail.contains("qwen3:14b"), "{}", model.detail);
    assert!(model.detail.contains("fails open on every dictation"));
    let fix = model.fix.as_deref().expect("every failure has a fix");
    assert!(fix.contains("ollama pull qwen3:14b"), "{fix}");
    assert!(
        fix.contains("gemma4:12b") && fix.contains("qwen3.6:27b"),
        "the fix must offer the installed alternatives: {fix}"
    );

    // The server itself is fine: the problem is precisely the model.
    assert_eq!(report.check("ollama").unwrap().status, CheckStatus::Ok);
    assert!(!report.is_healthy());
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_installed_formatter_model_passes() {
    let ollama = fake_ollama(&["gemma4:12b"]).await;
    let h = Harness::with(Setup::default().with_doctor(
        config_in(&std::env::temp_dir(), &ollama, "gemma4:12b"),
        ConfigReport::default(),
    ))
    .await;
    let report = diagnose(&h, true).await;
    assert_eq!(
        report.check("grammar_model").unwrap().status,
        CheckStatus::Ok
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_local_route_model_is_checked_separately_and_only_warns() {
    let ollama = fake_ollama(&["gemma4:12b"]).await;
    let h = Harness::with(Setup::default().with_doctor(
        config_in(&std::env::temp_dir(), &ollama, "gemma4:12b"),
        ConfigReport::default(),
    ))
    .await;
    let local = diagnose(&h, true)
        .await
        .check("local_model")
        .unwrap()
        .clone();
    assert_eq!(
        local.status,
        CheckStatus::Warn,
        "only the `local` route needs it"
    );
    assert!(local.fix.unwrap().contains("local.model"));
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unreachable_ollama_is_a_failure_when_the_formatter_needs_it() {
    let dir = std::env::temp_dir();
    // Port 1 on loopback is never an Ollama server.
    let h = Harness::with(Setup::default().with_doctor(
        config_in(&dir, "http://127.0.0.1:1", "gemma4:12b"),
        ConfigReport::default(),
    ))
    .await;
    let report = diagnose(&h, true).await;
    let ollama = report.check("ollama").unwrap();
    assert_eq!(ollama.status, CheckStatus::Fail);
    assert!(ollama.fix.as_deref().unwrap().contains("ollama serve"));
    // With no server the installed models are unknowable — say so rather than
    // claiming the model is missing.
    assert_eq!(
        report.check("grammar_model").unwrap().status,
        CheckStatus::Skipped
    );

    // ...and merely a warning when nothing depends on it.
    let mut config = config_in(&dir, "http://127.0.0.1:1", "gemma4:12b");
    config.grammar.enabled = false;
    let h2 = Harness::with(Setup::default().with_doctor(config, ConfigReport::default())).await;
    let report = diagnose(&h2, true).await;
    assert_eq!(report.check("ollama").unwrap().status, CheckStatus::Warn);
    assert_eq!(
        report.check("grammar_model").unwrap().status,
        CheckStatus::Skipped
    );
    h.stop().await;
    h2.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_missing_speech_model_says_how_to_get_it() {
    let ollama = fake_ollama(&[]).await;
    let dir = std::env::temp_dir();
    let mut config = config_in(&dir, &ollama, "m");
    config.whisper.model = "large-v3-turbo".into();
    let h = Harness::with(Setup::default().with_doctor(config, ConfigReport::default())).await;
    let stt = diagnose(&h, false)
        .await
        .check("stt_model")
        .unwrap()
        .clone();
    assert_eq!(stt.status, CheckStatus::Fail);
    assert_eq!(
        stt.fix.as_deref(),
        Some("run `dictate model pull large-v3-turbo`")
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_model_of_the_wrong_size_is_caught_even_on_a_quick_run() {
    let ollama = fake_ollama(&[]).await;
    let dir = std::env::temp_dir().join(format!("doctor-model-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let spec = dictate_stt::catalog_model("tiny.en").unwrap();
    let path = dir.join(spec.filename);
    std::fs::write(&path, b"a truncated download").unwrap();

    let mut config = config_in(&dir, &ollama, "m");
    config.whisper.model = "tiny.en".into();
    config.whisper.model_path = path.to_string_lossy().into_owned();
    let h = Harness::with(Setup::default().with_doctor(config, ConfigReport::default())).await;

    let stt = diagnose(&h, true).await.check("stt_model").unwrap().clone();
    assert_eq!(stt.status, CheckStatus::Fail);
    assert!(stt.detail.contains("truncated"), "{}", stt.detail);
    assert!(stt.fix.unwrap().contains("dictate model pull tiny.en"));

    h.stop().await;
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_custom_model_is_reported_as_present_but_unverified() {
    let ollama = fake_ollama(&[]).await;
    let dir = std::env::temp_dir().join(format!("doctor-custom-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("my-finetune.bin");
    std::fs::write(&path, b"whatever").unwrap();

    let mut config = config_in(&dir, &ollama, "m");
    config.whisper.model = "my-finetune".into();
    config.whisper.model_path = path.to_string_lossy().into_owned();
    let h = Harness::with(Setup::default().with_doctor(config, ConfigReport::default())).await;
    let stt = diagnose(&h, false)
        .await
        .check("stt_model")
        .unwrap()
        .clone();
    assert_eq!(stt.status, CheckStatus::Ok);
    assert!(stt.detail.contains("not verified"), "{}", stt.detail);
    h.stop().await;
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn config_warnings_are_surfaced_with_the_pointer_to_the_full_list() {
    let ollama = fake_ollama(&[]).await;
    let report = ConfigReport {
        path: "/x/config.toml".into(),
        existed: true,
        warnings: vec![
            "unknown section [editor] ignored".into(),
            "unknown key 'output.typing_delay_ms' ignored".into(),
        ],
        errors: vec![],
    };
    let h = Harness::with(
        Setup::default().with_doctor(config_in(&std::env::temp_dir(), &ollama, "m"), report),
    )
    .await;
    let config = diagnose(&h, true).await.check("config").unwrap().clone();
    assert_eq!(config.status, CheckStatus::Warn);
    assert!(config.detail.contains("[editor]") && config.detail.contains("2 warning"));
    assert!(config.fix.unwrap().contains("--check-config"));
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_backend_check_catches_a_device_mismatch() {
    let ollama = fake_ollama(&[]).await;
    let mut config = config_in(&std::env::temp_dir(), &ollama, "m");
    config.whisper.device = "cuda".into();
    // MockStt reports backend "mock" — never what a cuda config asked for.
    let h = Harness::with(Setup::default().with_doctor(config, ConfigReport::default())).await;
    let backend = diagnose(&h, true)
        .await
        .check("stt_backend")
        .unwrap()
        .clone();
    assert_eq!(backend.status, CheckStatus::Fail);
    assert!(backend.detail.contains("cuda") && backend.detail.contains("mock"));
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hotkey_permission_problems_name_the_unreadable_device_and_the_fix() {
    let ollama = fake_ollama(&[]).await;
    let mut config = config_in(&std::env::temp_dir(), &ollama, "m");
    config.hotkey.enabled = true;
    config.hotkey.devices = vec!["/dev/input/event-that-does-not-exist".into()];
    let h = Harness::with(Setup::default().with_doctor(config, ConfigReport::default())).await;
    let report = diagnose(&h, true).await;
    let hotkeys = report.check("hotkeys").unwrap();
    assert_eq!(hotkeys.status, CheckStatus::Fail);
    assert!(hotkeys.detail.contains("event-that-does-not-exist"));
    assert!(hotkeys.fix.as_deref().unwrap().contains("input"));

    // Disabled hotkeys are skipped, not failed.
    let h2 = Harness::with(Setup::default().with_doctor(
        config_in(&std::env::temp_dir(), &ollama, "m"),
        ConfigReport::default(),
    ))
    .await;
    assert_eq!(
        diagnose(&h2, true).await.check("hotkeys").unwrap().status,
        CheckStatus::Skipped
    );
    h.stop().await;
    h2.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_legacy_pid_holder_is_named_because_it_owns_the_toggle_signal() {
    let ollama = fake_ollama(&[]).await;
    let h = Harness::with(Setup::default().with_doctor(
        config_in(&std::env::temp_dir(), &ollama, "m"),
        ConfigReport::default(),
    ))
    .await;
    let legacy = h.dir().join("dictate.pid");

    // Nothing there yet.
    assert_eq!(
        diagnose(&h, true).await.check("legacy_pid").unwrap().status,
        CheckStatus::Ok
    );

    // Held by another live process of this user, standing in for the old
    // daemon (`sleep`, so its name is checkable too).
    let mut other = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    std::fs::write(&legacy, other.id().to_string()).unwrap();
    let check = diagnose(&h, true)
        .await
        .check("legacy_pid")
        .unwrap()
        .clone();
    let _ = other.kill();
    let _ = other.wait();
    assert_eq!(check.status, CheckStatus::Warn);
    assert!(
        check.detail.contains("sleep") && check.detail.contains("THAT process"),
        "{}",
        check.detail
    );

    // A stale one is called out as stale, and the doctor does not delete it.
    std::fs::write(&legacy, "4194304").unwrap();
    let check = diagnose(&h, true)
        .await
        .check("legacy_pid")
        .unwrap()
        .clone();
    assert_eq!(check.status, CheckStatus::Warn);
    assert!(check.detail.contains("stale"), "{}", check.detail);
    assert!(legacy.exists(), "diagnosis is read-only");
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_broken_formatter_shows_up_in_get_status_and_in_the_report() {
    // A formatter double that reports the June failure.
    struct Broken;
    impl dictate_core::ports::Formatter for Broken {
        fn format<'a>(
            &'a self,
            text: &'a str,
            _ctx: &'a dictate_core::ports::FormatContext,
        ) -> dictate_core::ports::BoxFuture<'a, dictate_core::ports::Formatted> {
            Box::pin(async move {
                dictate_core::ports::Formatted {
                    text: text.to_string(),
                    changed: false,
                    error: Some("model 'qwen3:14b' not found".into()),
                    duration_s: 0.0,
                }
            })
        }
        fn plan(
            &self,
            _: &str,
            _: &dictate_core::ports::FormatContext,
        ) -> dictate_core::ports::FormatPlan {
            dictate_core::ports::FormatPlan::Run
        }
        fn status(&self) -> Option<dictate_proto::FormatterStatus> {
            Some(dictate_proto::FormatterStatus {
                enabled: true,
                model: Some("qwen3:14b".into()),
                health: FormatterHealth::ModelMissing,
                detail: Some("model 'qwen3:14b' is not installed in Ollama".into()),
            })
        }
    }
    let ollama = fake_ollama(&["gemma4:12b"]).await;
    let h = Harness::with(
        Setup::default()
            .with_formatter(Arc::new(Broken))
            .with_doctor(
                config_in(&std::env::temp_dir(), &ollama, "qwen3:14b"),
                ConfigReport::default(),
            ),
    )
    .await;

    let mut client = h.client().await;
    let status = match client.request(Command::GetStatus).await.unwrap() {
        CommandResult::Status(s) => s,
        other => panic!("{other:?}"),
    };
    let formatter = status.formatter.expect("get_status reports the formatter");
    assert_eq!(formatter.health, FormatterHealth::ModelMissing);

    let report = diagnose(&h, true).await;
    let check = report.check("formatter").unwrap();
    assert_eq!(check.status, CheckStatus::Fail);
    assert!(check
        .fix
        .as_deref()
        .unwrap()
        .contains("ollama pull qwen3:14b"));
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_healthy_mock_daemon_produces_the_full_named_vocabulary() {
    let ollama = fake_ollama(&["gemma4:12b"]).await;
    let mut config = config_in(&std::env::temp_dir(), &ollama, "gemma4:12b");
    config.local.model = "gemma4:12b".into();
    let h = Harness::with(
        Setup::default()
            .with_stt(Arc::new(MockStt::returning("x")))
            .with_formatter(Arc::new(MockFormatter::default()))
            .with_doctor(config, ConfigReport::default()),
    )
    .await;
    let report = diagnose(&h, true).await;
    for id in [
        "config",
        "stt_model",
        "stt_backend",
        "formatter",
        "ollama",
        "grammar_model",
        "local_model",
        "injection",
        "hotkeys",
        "audio_input",
        "notifications",
        "tools",
        "legacy_pid",
    ] {
        assert!(report.check(id).is_some(), "missing check `{id}`");
    }
    // Every warning and failure carries a fix; every check has a title.
    for c in &report.checks {
        assert!(!c.title.is_empty());
        if matches!(c.status, CheckStatus::Warn | CheckStatus::Fail) {
            assert!(
                c.fix.is_some(),
                "check `{}` is {:?} without a fix",
                c.id,
                c.status
            );
        }
    }
    h.stop().await;
}
