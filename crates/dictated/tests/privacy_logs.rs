//! Privacy mode keeps transcript text out of the logs, not just the database.
//!
//! Logs reach the systemd journal, so a private session that wrote its text
//! there would defeat privacy mode as surely as a history row. This binary
//! installs one global subscriber that captures everything at INFO and runs
//! sessions through the real daemon: the private ones must leave no trace of
//! their words; a normal one must (which proves the capture works).

mod harness;

use std::io::Write;
use std::sync::{Arc, Mutex, OnceLock};

use dictate_core::ports::mock::{MockFormatter, MockStt};
use dictate_proto::{Command, SessionOptions, State, Transcript};
use harness::{Harness, Setup};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn logs() -> &'static Capture {
    static CAPTURE: OnceLock<Capture> = OnceLock::new();
    CAPTURE.get_or_init(|| {
        let capture = Capture::default();
        let writer = capture.clone();
        tracing::subscriber::set_global_default(
            tracing_subscriber::fmt()
                .with_max_level(tracing::Level::DEBUG)
                .with_writer(move || writer.clone())
                .finish(),
        )
        .expect("the only subscriber in this test binary");
        capture
    })
}

fn captured() -> String {
    String::from_utf8_lossy(&logs().0.lock().unwrap()).into_owned()
}

async fn dictate(h: &Harness, options: SessionOptions) -> Transcript {
    let mut client = h.client().await;
    client.subscribe().await;
    client
        .request(Command::StartDictation {
            mode: dictate_proto::DictationMode::Toggle,
            options: Some(options),
        })
        .await
        .expect("start");
    client.wait_for_state(State::Recording).await;
    client.request(Command::Stop).await.expect("stop");
    client.wait_for_final().await
}

fn setup(words: &str, global_privacy: bool) -> Setup {
    let mut s = Setup::default()
        .with_stt(Arc::new(MockStt::returning(words)))
        // A formatter that changes the text exercises every text-bearing log
        // site: transcribed, formatted, routed and grammar-corrected.
        .with_formatter(Arc::new(MockFormatter::appending(" zz9plural")))
        .with_history();
    s.history_privacy = global_privacy;
    s
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn private_sessions_log_no_transcript_text() {
    let _ = logs();

    // Per-session privacy.
    let h = Harness::with(setup("um the quokka ledger migrates at dawn", false)).await;
    let t = dictate(
        &h,
        SessionOptions {
            privacy: Some(true),
            ..SessionOptions::default()
        },
    )
    .await;
    assert!(
        t.text.as_str().contains("quokka"),
        "the caller still gets its text"
    );
    h.stop().await;

    // Global privacy mode.
    let h = Harness::with(setup("um the axolotl invoice settles at noon", true)).await;
    let t = dictate(&h, SessionOptions::default()).await;
    assert!(t.text.as_str().contains("axolotl"));
    h.stop().await;

    // A normal session, so an empty capture cannot pass by accident.
    let h = Harness::with(setup("um the pangolin archive opens at dusk", false)).await;
    dictate(&h, SessionOptions::default()).await;
    h.stop().await;

    let logs = captured();
    assert!(
        logs.contains("pangolin"),
        "the capture must see a normal session's text at INFO"
    );
    for secret in ["quokka", "ledger", "axolotl", "invoice"] {
        assert!(
            !logs.contains(secret),
            "private transcript text {secret:?} reached the logs"
        );
    }
}
