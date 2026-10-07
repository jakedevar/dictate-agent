//! The LOCAL route against a fake Ollama: history records the model that
//! actually answered, which after S21's model ladder is not necessarily
//! `[local].model`.
//!
//! Ollama is a tiny in-process HTTP server; nothing here needs a GPU, a
//! network, or a display.

mod harness;

use std::sync::Arc;

use dictate_core::config::LocalConfig;
use dictate_core::ports::mock::MockStt;
use dictate_proto::{Command, DictationMode, State};
use harness::{Harness, Setup};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A fake Ollama: `/api/tags` lists `installed`, `/api/generate` answers
/// `answer` from whichever model was asked.
async fn fake_ollama(installed: &'static [&'static str], answer: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let request = read_request(&mut sock).await;
                let body = if request.starts_with("GET /api/tags") {
                    serde_json::json!({
                        "models": installed.iter().map(|m| serde_json::json!({
                            "name": m, "model": m,
                            "modified_at": "2026-01-01T00:00:00Z", "size": 1,
                        })).collect::<Vec<_>>()
                    })
                } else if request.starts_with("POST /api/generate") {
                    let json_start = request.find('{').unwrap_or(request.len());
                    let asked: serde_json::Value =
                        serde_json::from_str(&request[json_start..]).unwrap_or_default();
                    serde_json::json!({
                        "model": asked["model"],
                        "created_at": "2026-01-01T00:00:00Z",
                        "response": answer,
                        "done": true,
                    })
                } else {
                    serde_json::json!({"error": "unexpected request"})
                }
                .to_string();
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

/// Read one HTTP request: headers, then `content-length` bytes of body.
async fn read_request(sock: &mut tokio::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let Ok(n) = sock.read(&mut chunk).await else {
            break;
        };
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buf);
        if let Some(end) = text.find("\r\n\r\n") {
            let length = text[..end]
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            if buf.len() >= end + 4 + length {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn recorded_models(h: &Harness) -> Vec<Option<String>> {
    let store = h.history.lock().unwrap();
    let mut stmt = store
        .connection()
        .prepare("SELECT execution_model FROM interactions ORDER BY id")
        .unwrap();
    stmt.query_map([], |row| row.get::<_, Option<String>>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

async fn dictate_once(h: &Harness) {
    let mut client = h.client().await;
    client.subscribe().await;
    client
        .request(Command::StartDictation {
            mode: DictationMode::Toggle,
            options: None,
        })
        .await
        .expect("start");
    client.wait_for_state(State::Recording).await;
    client.request(Command::Stop).await.expect("stop");
    client.wait_for_terminal().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_route_history_records_the_resolved_fallback_model() {
    let ollama = fake_ollama(&["fallback:2b"], "Four.").await;
    let h = Harness::with(
        Setup::default()
            .with_stt(Arc::new(MockStt::returning("easy what is two plus two")))
            .with_local(LocalConfig {
                host: ollama,
                model: "preferred:1b".into(),
                models: vec!["fallback:2b".into()],
                timeout_s: 10.0,
            })
            .with_history(),
    )
    .await;

    dictate_once(&h).await;

    assert_eq!(h.injector.injected(), ["Four."]);
    assert_eq!(
        recorded_models(&h),
        [Some("fallback:2b".to_string())],
        "history names the model that answered, not [local].model"
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn local_route_history_records_no_model_when_none_is_installed() {
    let ollama = fake_ollama(&["unrelated:7b"], "unused").await;
    let h = Harness::with(
        Setup::default()
            .with_stt(Arc::new(MockStt::returning("easy what is two plus two")))
            .with_local(LocalConfig {
                host: ollama,
                model: "preferred:1b".into(),
                models: vec!["fallback:2b".into()],
                timeout_s: 10.0,
            })
            .with_history(),
    )
    .await;

    dictate_once(&h).await;

    assert!(h.injector.injected().is_empty());
    assert_eq!(recorded_models(&h), [None], "no model was asked");
    h.stop().await;
}
