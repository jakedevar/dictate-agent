//! S33 logging privacy (design §10): the network API never writes the bearer
//! token, a rejected credential, a query string, or a private session's words
//! to the logs — which reach the systemd journal.
//!
//! Its own test binary because it installs the process-wide subscriber.

mod harness;

use std::io::Write;
use std::sync::{Arc, Mutex, OnceLock};

use dictate_core::ports::mock::MockStt;
use dictate_server::ApiConfig;
use harness::{within, Harness, Setup};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

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

async fn run(words: &str, query: &str) -> (String, String) {
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
    assert_eq!(
        post(addr, &format!("Bearer {token}"), query, &wav()).await,
        200
    );
    h.stop().await;
    (token, wrong)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_network_api_logs_no_credentials_and_no_private_words() {
    let _ = logs();

    let (token_a, wrong_a) = run(
        "the okapi manifest ships friday",
        "?privacy=true&app=secret-app-hint",
    )
    .await;
    // A normal session, so an empty capture cannot pass by accident.
    let (token_b, wrong_b) = run("the tapir ledger closes monday", "").await;

    let logs = captured();
    assert!(
        logs.contains("tapir"),
        "the capture must see a normal session's text at INFO"
    );
    assert!(
        logs.contains("network API authentication failed"),
        "the rejected request is logged, as a class"
    );
    for secret in [
        token_a.as_str(),
        token_b.as_str(),
        wrong_a.as_str(),
        wrong_b.as_str(),
        "Bearer dct1_",
        "okapi",
        "manifest",
        "secret-app-hint",
        "privacy=true",
    ] {
        assert!(!logs.contains(secret), "{secret:?} reached the logs");
    }
}
