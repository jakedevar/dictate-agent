//! The `dictate dict` subcommands, run for real against the real daemon.
//!
//! Unlike `cli.rs` (a protocol stub), this starts an in-process `dictated`
//! with mock hardware, a synthetic history and a private dictionary database,
//! and runs the built `dictate` binary against it: add / disable / enable /
//! export / rm / import / suggest / accept, plus a rejected bad import.
//! Port of the former `scripts/test-dictionary-cli.py`; all data is synthetic.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use dictate_core::{
    ports::mock::{
        MockAudio, MockFormatter, MockInjector, MockStt, NullEarcons, NullMedia, RecordingNotifier,
    },
    Pipeline,
};
use dictate_history::{HistoryConfig, HistoryStore};
use dictated::{paths::RuntimePaths, Daemon};
use serde_json::Value;

/// `sockaddr_un` caps a path at 108 bytes and sandboxed runners export deep
/// `TMPDIR`s, so fall back to `/tmp` when the temp dir leaves too little room.
fn fixture_root() -> PathBuf {
    let tmp = std::env::temp_dir();
    let base = if tmp.as_os_str().len() <= 40 {
        tmp
    } else {
        PathBuf::from("/tmp")
    };
    let root = base.join(format!("dict-rt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

async fn start_daemon(root: &Path) -> Daemon {
    let history = Arc::new(Mutex::new(
        HistoryStore::new(&HistoryConfig {
            db_path: root.join("history.db").to_string_lossy().into_owned(),
            ..Default::default()
        })
        .unwrap(),
    ));
    {
        // The same phrase corrected on two different days, three times: the
        // evidence behind the single "Tauri" suggestion.
        let store = history.lock().unwrap();
        for day in [1, 1, 2] {
            let mut interaction = store.begin();
            interaction.timestamp = format!("2026-09-{day:02}T12:00:00+00:00");
            interaction.grammar_input = Some("use tow ree today".into());
            interaction.grammar_output = Some("use Tauri today".into());
            interaction.corrected_transcription = interaction.grammar_output.clone();
            interaction.completed = true;
            store.commit(&interaction);
        }
    }
    let dictionary = Arc::new(
        dictate_dict::Dictionary::open(
            dictate_dict::DictionaryConfig {
                db_path: root.join("dictionary.db").to_string_lossy().into_owned(),
                ..Default::default()
            },
            None,
        )
        .unwrap(),
    );
    let pipeline = Arc::new(Pipeline {
        // Never read the real desktop from a test.
        context: Arc::new(dictate_core::ContextEngine::disabled()),
        text_chain: Arc::new(dictate_fmt::TextChain::default()),
        audio: Arc::new(MockAudio::with_seconds(1.0)),
        stt: Arc::new(MockStt::returning("kubernetties")),
        dictionary: Some(dictionary),
        vad: Arc::new(
            dictate_vad::SileroVad::new(dictate_vad::VadConfig {
                enabled: false,
                ..Default::default()
            })
            .unwrap(),
        ),
        formatter: Arc::new(MockFormatter::disabled()),
        injector: Arc::new(MockInjector::unavailable()),
        notifier: Arc::new(RecordingNotifier::default()),
        media: Arc::new(NullMedia),
        earcons: Arc::new(NullEarcons),
        history: history.clone(),
        local: Arc::new(dictate_core::local_executor::LocalExecutor::new(
            &Default::default(),
        )),
        timer: Arc::new(dictate_core::timer::TimerExecutor::new(&Default::default())),
        editor: Arc::new(dictate_core::edit_executor::EditExecutor::new(
            &Default::default(),
            &Default::default(),
            &Default::default(),
        )),
        local_model: "synthetic".into(),
    });
    Daemon::start(
        pipeline,
        history,
        &RuntimePaths::under(root),
        dictated::server::local_capabilities(false),
        None,
    )
    .await
    .unwrap()
}

/// Run `dictate dict <args>` against `socket`; returns (success, stdout, stderr).
async fn dict(root: &Path, socket: &Path, args: &[&str]) -> (bool, String, String) {
    let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_dictate"))
        .arg("dict")
        .args(args)
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_RUNTIME_DIR", root.join("runtime"))
        .env("DICTATE_SOCKET", socket)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(std::time::Duration::from_secs(10), child)
        .await
        .unwrap_or_else(|_| panic!("`dictate dict {args:?}` timed out"))
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

async fn ok(root: &Path, socket: &Path, args: &[&str]) -> String {
    let (success, stdout, stderr) = dict(root, socket, args).await;
    assert!(success, "`dictate dict {args:?}` failed: {stderr}");
    stdout
}

async fn entries(root: &Path, socket: &Path) -> Vec<Value> {
    let json: Value = serde_json::from_str(&ok(root, socket, &["list", "--json"]).await).unwrap();
    json["entries"].as_array().unwrap().clone()
}

fn phrases(list: &[Value]) -> Vec<&str> {
    let mut p: Vec<&str> = list.iter().map(|e| e["phrase"].as_str().unwrap()).collect();
    p.sort();
    p
}

#[tokio::test]
async fn dict_cli_round_trip_against_the_real_daemon() {
    let root = fixture_root();
    let daemon = start_daemon(&root).await;
    let socket = daemon.socket().to_path_buf();
    let (r, s) = (root.as_path(), socket.as_path());

    assert!(entries(r, s).await.is_empty());

    ok(
        r,
        s,
        &[
            "add",
            "Kubernetes",
            "--sounds-like",
            "cube ernetties,kubernetties",
            "--app",
            "slack",
        ],
    )
    .await;
    let first = entries(r, s).await.remove(0);
    assert_eq!(first["phrase"], "Kubernetes");
    assert_eq!(first["apps"], serde_json::json!(["slack"]));
    assert_eq!(
        first["sounds_like"],
        serde_json::json!(["cube ernetties", "kubernetties"])
    );

    ok(r, s, &["disable", &first["id"].to_string()]).await;
    assert_eq!(entries(r, s).await[0]["enabled"], false);
    ok(r, s, &["enable", "Kubernetes"]).await;
    assert_eq!(entries(r, s).await[0]["enabled"], true);

    let exported = root.join("dictionary.jsonl");
    ok(r, s, &["export", exported.to_str().unwrap()]).await;
    let line: Value = serde_json::from_str(
        std::fs::read_to_string(&exported)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(line["phrase"], "Kubernetes");

    ok(r, s, &["rm", "Kubernetes"]).await;
    assert!(entries(r, s).await.is_empty());
    ok(r, s, &["import", exported.to_str().unwrap()]).await;
    assert_eq!(entries(r, s).await[0]["phrase"], "Kubernetes");

    // Suggestions come from the synthetic history and are read-only until accepted.
    let suggest: Value = serde_json::from_str(&ok(r, s, &["suggest", "--json"]).await).unwrap();
    let proposals = suggest["suggestions"].as_array().unwrap();
    assert_eq!(proposals.len(), 1);
    assert_eq!(proposals[0]["entry"]["phrase"], "Tauri");
    assert_eq!(proposals[0]["count"], 3);
    assert_eq!(proposals[0]["days"], 2);
    ok(r, s, &["accept", "Tauri"]).await;
    assert_eq!(phrases(&entries(r, s).await), ["Kubernetes", "Tauri"]);

    // Re-importing updates by phrase rather than duplicating.
    ok(r, s, &["import", exported.to_str().unwrap()]).await;
    assert_eq!(entries(r, s).await.len(), 2);

    // A file with one bad line installs nothing.
    let bad = root.join("bad.jsonl");
    std::fs::write(&bad, "{\"phrase\":\"ShouldNotInstall\"}\ninvalid json\n").unwrap();
    let (success, _, _) = dict(r, s, &["import", bad.to_str().unwrap()]).await;
    assert!(!success, "a malformed import must fail");
    assert_eq!(entries(r, s).await.len(), 2);

    daemon.shutdown().await;
    let _ = std::fs::remove_dir_all(&root);
}
