//! `get_config` / `set_config` against a real daemon on a real socket (S32).
//!
//! Every fixture is a synthetic file in the harness's private directory; no
//! test reads or writes the user's configuration.

mod harness;

use dictate_proto::{
    Capabilities, Command, CommandResult, ConfigEntry, ConfigSnapshot, ErrorCode, ProtoError, State,
};
use harness::{Harness, Setup};
use serde_json::{json, Value};

/// A Python-era file: comments, a legacy section, an unknown key, and a
/// value with an inline comment. Everything here must survive an edit.
const FIXTURE: &str = r#"# dictate-agent config (synthetic fixture)

[grammar]
# the formatter
enabled = false # off until the model is pulled
model = "example-model:1b"

[router]
ollama_model = "example-local:1b"

[editor]
mode = "vim"

[history]
enabled = true
"#;

fn snapshot(result: Result<CommandResult, ProtoError>) -> ConfigSnapshot {
    match result.expect("a config result") {
        CommandResult::Config(s) => s,
        other => panic!("expected a config snapshot, got {other:?}"),
    }
}

fn set(entries: Vec<ConfigEntry>) -> Command {
    Command::SetConfig {
        entries,
        document: None,
        dry_run: false,
    }
}

fn at<'a>(v: &'a Value, path: &str) -> &'a Value {
    path.split('.')
        .try_fold(v, |node, seg| node.get(seg))
        .unwrap_or_else(|| panic!("{path} missing from {v}"))
}

fn detail_path(e: &ProtoError) -> Option<String> {
    e.detail
        .as_ref()
        .and_then(|d| d.get("path"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

async fn harness_with_file(contents: Option<&str>) -> (Harness, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "dictated-config-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    if let Some(text) = contents {
        std::fs::write(&path, text).unwrap();
    }
    let h = Harness::with(
        Setup::default()
            .with_history()
            .with_config_file(path.clone()),
    )
    .await;
    (h, path)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_returns_the_effective_config_and_the_users_file() {
    let (h, path) = harness_with_file(Some(FIXTURE)).await;
    let mut c = h.client().await;
    let hello = c.hello.clone().unwrap();
    assert!(hello.capabilities.features.config_read);
    assert!(hello.capabilities.features.config_write);

    let s = snapshot(c.request(Command::GetConfig { path: None }).await);
    // Set in the file, legacy-mapped, and defaulted, all in one tree.
    assert_eq!(at(&s.values, "grammar.model"), "example-model:1b");
    assert_eq!(at(&s.values, "local.model"), "example-local:1b");
    assert_eq!(at(&s.values, "vad.enabled"), true);
    let file = s.file.expect("whole-tree reads carry the file");
    assert_eq!(file.document, FIXTURE);
    assert!(file.exists);
    assert_eq!(std::path::Path::new(&file.path), path);
    assert!(
        s.warnings.iter().any(|w| w.contains("[editor]")),
        "the loader's warnings are shown, not swallowed: {:?}",
        s.warnings
    );
    assert!(s.restart_required.is_empty(), "{:?}", s.restart_required);
    assert!(s.errors.is_empty());

    // A subtree read.
    let s = snapshot(
        c.request(Command::GetConfig {
            path: Some("grammar".into()),
        })
        .await,
    );
    assert_eq!(s.path.as_deref(), Some("grammar"));
    assert_eq!(s.values["enabled"], false);
    assert!(s.file.is_none());

    let err = c
        .request(Command::GetConfig {
            path: Some("grammar.nope".into()),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_round_trips_and_preserves_comments_and_unknown_keys() {
    let (h, path) = harness_with_file(Some(FIXTURE)).await;
    let mut c = h.client().await;

    let s = snapshot(
        c.request(set(vec![
            ConfigEntry::new("grammar.enabled", json!(true)),
            ConfigEntry::new("grammar.timeout_s", json!(4)),
            ConfigEntry::new("vad", json!({"trailing_silence_ms": 1200})),
        ]))
        .await,
    );
    assert_eq!(
        s.applied,
        vec![
            "grammar.enabled",
            "grammar.timeout_s",
            "vad.trailing_silence_ms"
        ]
    );
    // None of these is applied live, and the daemon says so.
    assert_eq!(s.restart_required, s.applied);
    assert!(!s.dry_run);

    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.starts_with("# dictate-agent config (synthetic fixture)\n"));
    assert!(
        written.contains("# the formatter\nenabled = true # off until the model is pulled\n"),
        "comments around a replaced value survive:\n{written}"
    );
    assert!(
        written.contains("timeout_s = 4.0"),
        "floats stay floats:\n{written}"
    );
    assert!(written.contains("[router]\nollama_model = \"example-local:1b\""));
    assert!(
        written.contains("[editor]\nmode = \"vim\""),
        "unknown sections survive"
    );
    assert!(written.contains("[vad]\ntrailing_silence_ms = 1200"));
    assert_eq!(s.file.unwrap().document, written);

    // One backup, holding exactly what was replaced.
    let backup = path.with_file_name("config.toml.bak");
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), FIXTURE);
    // No temp files left behind.
    let stray: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
        .collect();
    assert!(stray.is_empty(), "{stray:?}");

    // The next read sees the write, and still owes a restart for it.
    let s = snapshot(c.request(Command::GetConfig { path: None }).await);
    assert_eq!(at(&s.values, "grammar.enabled"), true);
    assert_eq!(at(&s.values, "grammar.timeout_s"), 4.0);
    assert!(s.restart_required.contains(&"grammar.enabled".to_string()));

    // `null` removes the key, so its default applies again.
    let s = snapshot(
        c.request(set(vec![ConfigEntry::new(
            "grammar.timeout_s",
            Value::Null,
        )]))
        .await,
    );
    assert_eq!(s.applied, vec!["grammar.timeout_s"]);
    assert_eq!(at(&s.values, "grammar.timeout_s"), 10.0);
    assert!(!std::fs::read_to_string(&path)
        .unwrap()
        .contains("timeout_s"));
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invalid_writes_are_config_invalid_with_the_offending_path_and_change_nothing() {
    let (h, path) = harness_with_file(Some(FIXTURE)).await;
    let mut c = h.client().await;

    for (entries, expected_path) in [
        // Fails the loader's value validation.
        (
            vec![
                ConfigEntry::new("grammar.enabled", json!(true)),
                ConfigEntry::new("grammar.timeout_s", json!(0)),
            ],
            "grammar.timeout_s",
        ),
        // Fails deserialization (wrong type).
        (
            vec![ConfigEntry::new("grammar.enabled", json!("yes"))],
            "grammar.enabled",
        ),
        (
            vec![ConfigEntry::new("whisper.device", json!("tpu"))],
            "whisper.device",
        ),
        // A key the daemon would ignore is a typo, not a setting.
        (
            vec![ConfigEntry::new("grammar.modle", json!("x"))],
            "grammar.modle",
        ),
        (vec![ConfigEntry::new("grammar", json!(true))], "grammar"),
        (
            vec![ConfigEntry::new("grammar..model", json!("x"))],
            "grammar..model",
        ),
    ] {
        let err = c.request(set(entries.clone())).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfigInvalid, "{entries:?}: {err}");
        assert_eq!(
            detail_path(&err).as_deref(),
            Some(expected_path),
            "{entries:?}: {err:?}"
        );
        assert!(err.message.contains(expected_path), "{}", err.message);
    }

    // Whole-document writes: bad TOML, and good TOML the daemon would refuse.
    let err = c
        .request(Command::SetConfig {
            entries: vec![],
            document: Some("[grammar\nenabled = true\n".into()),
            dry_run: false,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::ConfigInvalid);
    let err = c
        .request(Command::SetConfig {
            entries: vec![],
            document: Some("[grammar]\ntimeout_s = -1.0\n".into()),
            dry_run: false,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::ConfigInvalid);
    assert_eq!(detail_path(&err).as_deref(), Some("grammar.timeout_s"));

    // Malformed requests are not config errors.
    let err = c.request(set(vec![])).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidParams);
    let err = c
        .request(Command::SetConfig {
            entries: vec![ConfigEntry::new("grammar.enabled", json!(true))],
            document: Some(String::new()),
            dry_run: false,
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidParams);

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        FIXTURE,
        "nothing was written"
    );
    assert!(!path.with_file_name("config.toml.bak").exists());
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_whole_document_write_and_a_dry_run() {
    let (h, path) = harness_with_file(Some(FIXTURE)).await;
    let mut c = h.client().await;

    let proposed = "# rewritten by hand\n[grammar]\nenabled = true\n";
    let s = snapshot(
        c.request(Command::SetConfig {
            entries: vec![],
            document: Some(proposed.into()),
            dry_run: true,
        })
        .await,
    );
    assert!(s.dry_run);
    assert!(s.applied.contains(&"grammar.enabled".to_string()));
    assert!(
        s.applied.contains(&"local.model".to_string()),
        "{:?}",
        s.applied
    );
    assert_eq!(s.file.unwrap().document, proposed);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        FIXTURE,
        "a dry run writes nothing"
    );

    let s = snapshot(
        c.request(Command::SetConfig {
            entries: vec![],
            document: Some(proposed.into()),
            dry_run: false,
        })
        .await,
    );
    assert!(!s.dry_run);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        proposed,
        "written verbatim"
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_missing_file_reads_as_defaults_and_a_write_creates_it() {
    let (h, path) = harness_with_file(None).await;
    let mut c = h.client().await;
    let s = snapshot(c.request(Command::GetConfig { path: None }).await);
    assert!(!s.file.as_ref().unwrap().exists);
    assert_eq!(s.file.unwrap().document, "");
    let defaults = dictate_core::config::Config::default();
    assert_eq!(at(&s.values, "grammar.enabled"), defaults.grammar.enabled);
    assert_eq!(
        at(&s.values, "grammar.model"),
        defaults.grammar.model.as_str()
    );

    snapshot(
        c.request(set(vec![ConfigEntry::new(
            "notifications.enabled",
            json!(false),
        )]))
        .await,
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[notifications]\nenabled = false\n"
    );
    assert!(
        !path.with_file_name("config.toml.bak").exists(),
        "nothing to back up"
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_symlinked_config_is_written_through_to_its_target() {
    let (h, path) = harness_with_file(None).await;
    let target = path.with_file_name("dotfiles-config.toml");
    std::fs::write(&target, FIXTURE).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    let mut c = h.client().await;
    snapshot(
        c.request(set(vec![ConfigEntry::new("grammar.enabled", json!(true))]))
            .await,
    );
    assert!(
        std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the link must not be replaced by a regular file"
    );
    assert!(std::fs::read_to_string(&target)
        .unwrap()
        .contains("enabled = true # off until the model is pulled"));
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn privacy_mode_applies_live_and_is_not_reported_as_needing_a_restart() {
    let (h, _path) = harness_with_file(Some(FIXTURE)).await;
    let mut c = h.client().await;
    c.subscribe().await;

    let s = snapshot(
        c.request(set(vec![ConfigEntry::new(
            "history.privacy_mode",
            json!(true),
        )]))
        .await,
    );
    assert_eq!(s.applied, vec!["history.privacy_mode"]);
    assert!(s.restart_required.is_empty(), "{:?}", s.restart_required);

    let CommandResult::Status(status) = c.request(Command::GetStatus).await.unwrap() else {
        panic!("expected status");
    };
    assert!(status.capabilities.features.privacy_mode);

    c.request(Command::StartDictation {
        mode: dictate_proto::DictationMode::Toggle,
        options: None,
    })
    .await
    .expect("start");
    c.wait_for_state(State::Recording).await;
    c.request(Command::Stop).await.expect("stop");
    c.wait_for_state(State::Done).await;
    let rows: i64 = h
        .history
        .lock()
        .unwrap()
        .connection()
        .query_row("SELECT COUNT(*) FROM interactions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        rows, 0,
        "privacy mode switched on over the socket must store nothing"
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn config_access_is_capability_gated() {
    // A connection whose capabilities withhold config writes may read only.
    let mut read_only = dictated::server::local_capabilities(true);
    read_only.features.config_write = false;
    let dir = std::env::temp_dir().join(format!("dictated-config-cap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(&path, FIXTURE).unwrap();
    let h = Harness::with(
        Setup::default()
            .with_capabilities(read_only)
            .with_config_file(path.clone()),
    )
    .await;
    let mut c = h.client().await;
    snapshot(c.request(Command::GetConfig { path: None }).await);
    let err = c
        .request(set(vec![ConfigEntry::new("grammar.enabled", json!(true))]))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Forbidden);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), FIXTURE);
    h.stop().await;

    // A remote-shaped connection gets neither.
    let h = Harness::with(
        Setup::default()
            .with_capabilities(Capabilities::remote_transcription_only())
            .with_config_file(path.clone()),
    )
    .await;
    let mut c = h.client().await;
    let err = c
        .request(Command::GetConfig { path: None })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Forbidden);
    h.stop().await;

    // A daemon with no config service withdraws the capability up front.
    let h = Harness::start().await;
    let mut c = h.client().await;
    assert!(!c.hello.clone().unwrap().capabilities.features.config_read);
    let err = c
        .request(Command::GetConfig { path: None })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Forbidden);
    h.stop().await;
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn context_profiles_read_and_write_back_in_their_configured_shape() {
    let (h, path) = harness_with_file(Some(FIXTURE)).await;
    let mut c = h.client().await;
    let profile = json!({
        "name": "terminal",
        "match": {"class": "example-term*"},
        "category": "terminal",
        "llm_format": false
    });
    let s = snapshot(
        c.request(set(vec![ConfigEntry::new(
            "context.profiles",
            json!([profile.clone()]),
        )]))
        .await,
    );
    assert_eq!(s.applied, vec!["context.profiles"]);
    assert_eq!(at(&s.values, "context.profiles"), &json!([profile]));
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(
        written.contains("[[context.profiles]]\nname = \"terminal\""),
        "written the way a person would write it:\n{written}"
    );
    // And the daemon's own loader accepts what was written.
    let (_, report) = dictate_core::config::parse_config(&written).unwrap();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    h.stop().await;
}
