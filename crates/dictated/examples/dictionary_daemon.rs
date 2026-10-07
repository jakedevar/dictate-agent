//! Isolated real control-plane fixture for CLI round trips. Uses mock hardware,
//! synthetic history, and exclusively the root supplied on the command line.
use dictate_core::{
    ports::mock::{
        MockAudio, MockFormatter, MockInjector, MockStt, NullEarcons, NullMedia, RecordingNotifier,
    },
    Pipeline,
};
use dictate_history::{HistoryConfig, HistoryStore};
use dictated::{paths::RuntimePaths, Daemon};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let root = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("temporary fixture root required"),
    );
    std::fs::create_dir_all(&root)?;
    let history = Arc::new(Mutex::new(HistoryStore::new(&HistoryConfig {
        db_path: root.join("history.db").to_string_lossy().into_owned(),
        ..Default::default()
    })?));
    {
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
    let dictionary = Arc::new(dictate_dict::Dictionary::open(
        dictate_dict::DictionaryConfig {
            db_path: root.join("dictionary.db").to_string_lossy().into_owned(),
            ..Default::default()
        },
        None,
    )?);
    let pipeline = Arc::new(Pipeline {
        // Never read the real desktop from an example.
        context: Arc::new(dictate_core::ContextEngine::disabled()),
        text_chain: Arc::new(dictate_fmt::TextChain::default()),
        audio: Arc::new(MockAudio::with_seconds(1.0)),
        stt: Arc::new(MockStt::returning("kubernetties")),
        dictionary: Some(dictionary),
        vad: Arc::new(dictate_vad::SileroVad::new(dictate_vad::VadConfig {
            enabled: false,
            ..Default::default()
        })?),
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
    let daemon = Daemon::start(
        pipeline,
        history,
        &RuntimePaths::under(&root),
        dictated::server::local_capabilities(false),
        None,
    )
    .await?;
    println!("ready {}", daemon.socket().display());
    use tokio::io::AsyncBufReadExt;
    let mut line = String::new();
    tokio::io::BufReader::new(tokio::io::stdin())
        .read_line(&mut line)
        .await?;
    daemon.shutdown().await;
    Ok(())
}
