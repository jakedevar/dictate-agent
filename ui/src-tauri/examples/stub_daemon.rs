//! A scriptable stand-in for `dictated`, for driving the Flow bar in the X11
//! test and for screenshots. Commands arrive one per line on stdin:
//!
//! ```text
//! state <from> <to>        emit state_changed
//! level <rms>              emit one audio_level
//! speak <seconds>          emit a synthetic speech envelope at 60 Hz
//! final <text…>            emit a final transcript
//! error <message…>         emit an error event
//! drop                     close client connections
//! quit                     exit
//! ```
//!
//! Usage: `cargo run --example stub_daemon -- <socket>`

#[path = "../tests/common/stub.rs"]
mod stub;

use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::main]
async fn main() {
    let socket = std::env::args()
        .nth(1)
        .expect("usage: stub_daemon <socket>");
    let daemon = stub::StubDaemon::start(std::path::Path::new(&socket)).await;
    eprintln!("stub-daemon listening on {socket}");
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let mut words = line.split_whitespace();
        match words.next() {
            Some("state") => {
                let from = words.next().unwrap_or("idle");
                let to = words.next().unwrap_or("idle");
                daemon.state(from, to);
            }
            Some("level") => {
                let rms: f64 = words.next().and_then(|w| w.parse().ok()).unwrap_or(0.2);
                daemon.emit(json!({"type": "audio_level", "session_id": "stub-1", "rms": rms}));
            }
            Some("speak") => {
                let secs: f64 = words.next().and_then(|w| w.parse().ok()).unwrap_or(1.0);
                let frames = (secs * 60.0) as u32;
                for i in 0..frames {
                    let t = f64::from(i) / 60.0;
                    // Syllable-rate modulation over a slower phrase envelope.
                    let rms = (0.05
                        + 0.35 * ((t * 7.0).sin().abs()) * (0.6 + 0.4 * (t * 1.3).sin()))
                    .clamp(0.0, 1.0);
                    daemon.emit(json!({"type": "audio_level", "session_id": "stub-1", "rms": rms}));
                    tokio::time::sleep(Duration::from_millis(16)).await;
                }
            }
            Some("final") => {
                let text: Vec<&str> = words.collect();
                daemon.emit(json!({
                    "type": "final", "session_id": "stub-1", "text": text.join(" "),
                    "route": "type", "timings": {}, "injection": "delivered",
                    "word_count": text.len()
                }));
            }
            Some("error") => {
                let message: Vec<&str> = words.collect();
                daemon.emit(json!({
                    "type": "error", "session_id": "stub-1",
                    "error": {"code": "stt_failed", "message": message.join(" ")}
                }));
            }
            Some("drop") => daemon.drop_connections(),
            Some("quit") | None => break,
            Some(other) => eprintln!("stub-daemon: unknown command {other:?}"),
        }
    }
    daemon.stop();
}
