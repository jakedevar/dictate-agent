//! S33 logging privacy (design §10): the network API never writes the bearer
//! token, a rejected credential, a query string, or a network session's words
//! to the logs — which reach the systemd journal.
//!
//! NETWORK_TRANSCRIPTS_LOGGED (manager ruling): transcript text is never
//! logged at INFO or above for any session, and never at all for a network
//! or privacy session — with no opt-in from the client. A local session's
//! words at DEBUG prove the capture would have seen them.
//!
//! Its own test binary because it installs the process-wide subscriber.

mod harness;

use std::io::Write;
use std::sync::{Arc, Mutex, OnceLock};

use base64::Engine as _;
use dictate_core::ports::mock::MockStt;
use dictate_proto::{AudioFormat, AudioSource, Command, CommandResult};
use dictate_server::ApiConfig;
use futures_util::{SinkExt, StreamExt};
use harness::{within, Harness, Setup};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;

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
                .with_ansi(false)
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

fn wav() -> Vec<u8> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut w = hound::WavWriter::new(&mut buf, spec).unwrap();
        for i in 0..16_000 {
            w.write_sample(((i as f64 * 0.17).sin() * 3000.0) as i16)
                .unwrap();
        }
        w.finalize().unwrap();
    }
    buf.into_inner()
}

async fn post(addr: std::net::SocketAddr, auth: &str, query: &str, body: &[u8]) -> u16 {
    let head = format!(
        "POST /v1/transcribe{query} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nAuthorization: {auth}\r\nContent-Type: audio/wav\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    within("an upload", async {
        let mut tcp = TcpStream::connect(addr).await.unwrap();
        tcp.write_all(head.as_bytes()).await.unwrap();
        tcp.write_all(body).await.unwrap();
        let mut raw = Vec::new();
        let _ = tcp.read_to_end(&mut raw).await;
        String::from_utf8_lossy(&raw)
            .split(' ')
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    })
    .await
}

/// One `transcribe_audio` over the WebSocket, with no options at all.
async fn ws_upload(addr: std::net::SocketAddr, token: &str) {
    let mut req = format!("ws://{addr}/v1/ws").into_client_request().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    let tcp = TcpStream::connect(addr).await.unwrap();
    let (mut ws, _) = within("the upgrade", tokio_tungstenite::client_async(req, tcp))
        .await
        .unwrap();
    let data = base64::engine::general_purpose::STANDARD.encode(wav());
    for line in [
        serde_json::json!({"kind":"request","v":1,"id":1,"command":{"type":"handshake","protocol_version":1,"client":{"name":"phone","kind":"remote"}}}),
        serde_json::json!({"kind":"request","v":1,"id":2,"command":{"type":"transcribe_audio","audio":{"source":"inline","format":{"encoding":"wav"},"data":data}}}),
    ] {
        let id = line["id"].clone();
        ws.send(WsMessage::Text(line.to_string().into()))
            .await
            .unwrap();
        within("a WebSocket answer", async {
            loop {
                match ws.next().await {
                    Some(Ok(WsMessage::Text(t))) => {
                        let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                        if v["id"] == id {
                            assert!(v.get("error").is_none(), "{v}");
                            return;
                        }
                    }
                    Some(Ok(_)) => {}
                    other => panic!("the session ended: {other:?}"),
                }
            }
        })
        .await;
    }
    let _ = ws.close(None).await;
}

enum Via {
    /// `POST /v1/transcribe` with this query string.
    Http(&'static str),
    /// `transcribe_audio` over `GET /v1/ws`.
    Ws,
    /// `transcribe_audio` over the unix socket.
    Socket,
}

async fn run(words: &str, via: Via) -> (String, String) {
    let mut h = Harness::with(
        Setup::default()
            .with_stt(Arc::new(MockStt::returning(words)))
            .with_history(),
    )
    .await;
    let config = ApiConfig {
        enabled: true,
        bind: "127.0.0.1:0".into(),
        token_file: h.dir().join("api-token").to_string_lossy().into_owned(),
        ..ApiConfig::default()
    };
    let (token, _) = dictate_server::token::ensure(&config.token_path()).unwrap();
    let addr = h.daemon.start_network_api(&config).await.unwrap().unwrap();
    let wrong = dictate_server::token::generate().unwrap();
    assert_eq!(
        post(addr, &format!("Bearer {wrong}"), "", &wav()).await,
        401
    );
    match via {
        Via::Http(query) => assert_eq!(
            post(addr, &format!("Bearer {token}"), query, &wav()).await,
            200
        ),
        Via::Ws => ws_upload(addr, &token).await,
        Via::Socket => {
            let mut client = h.client().await;
            let result = client
                .request(Command::TranscribeAudio {
                    audio: AudioSource::Inline {
                        format: AudioFormat::wav(),
                        data: wav(),
                    },
                    options: None,
                })
                .await
                .unwrap();
            assert!(matches!(result, CommandResult::Transcript(_)));
        }
    }
    h.stop().await;
    (token, wrong)
}

fn level(line: &str) -> Option<&str> {
    line.split_whitespace().nth(1)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_network_api_logs_no_credentials_and_no_network_words() {
    let _ = logs();

    // A network session that asked for privacy...
    let (token_a, wrong_a) = run(
        "the okapi manifest ships friday",
        Via::Http("?privacy=true&app=secret-app-hint"),
    )
    .await;
    // ...and ordinary ones that asked for nothing, on both transports.
    let (token_b, wrong_b) = run("the tapir ledger closes monday", Via::Http("")).await;
    let (token_c, wrong_c) = run("the wombat roster opens sunday", Via::Ws).await;
    // A local session, so a capture that missed DEBUG cannot pass by accident.
    let (token_d, wrong_d) = run("the numbat archive settles tuesday", Via::Socket).await;

    let logs = captured();
    assert!(
        logs.contains("numbat"),
        "the capture sees a local session's words at DEBUG"
    );
    for line in logs.lines().filter(|l| l.contains("numbat")) {
        assert_eq!(
            level(line),
            Some("DEBUG"),
            "transcript text above DEBUG: {line}"
        );
    }
    let lengths = logs
        .lines()
        .filter(|l| level(l) == Some("INFO") && l.contains("transcribed chars="))
        .count();
    assert_eq!(
        lengths, 4,
        "every session logs its length, network ones included"
    );
    assert!(
        logs.contains("network API authentication failed"),
        "the rejected request is logged, as a class"
    );
    for secret in [
        token_a.as_str(),
        token_b.as_str(),
        token_c.as_str(),
        token_d.as_str(),
        wrong_a.as_str(),
        wrong_b.as_str(),
        wrong_c.as_str(),
        wrong_d.as_str(),
        "Bearer dct1_",
        "okapi",
        "manifest",
        "tapir",
        "ledger",
        "wombat",
        "roster",
        "secret-app-hint",
        "privacy=true",
    ] {
        assert!(!logs.contains(secret), "{secret:?} reached the logs");
    }
}
