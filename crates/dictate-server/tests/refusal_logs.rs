//! PREAUTH_GUARD_REJECTIONS_BYPASS_THROTTLE: requests refused before
//! authentication (a foreign `Host`, any `Origin`) stay refused, cost the
//! peer a connection each, and write a bounded number of warnings — a peer
//! that cannot get one request admitted cannot fill the journal either.
//!
//! Its own test binary because it installs the process-wide subscriber (a
//! thread-scoped one races tracing's per-callsite interest cache against the
//! other tests' threads). Ephemeral ports on 127.0.0.1 only.

use std::io::Write;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use dictate_proto::{Command, CommandResult, ErrorCode, Event, Message, ProtoError};
use dictate_server::{ApiConfig, ApiServer, Backend, BoxFuture, Hangup, Session};
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
                .with_ansi(false)
                .with_writer(move || writer.clone())
                .finish(),
        )
        .expect("the only subscriber in this test binary");
        capture
    })
}

/// Lines at `level` that contain `needle`.
fn count(logs: &str, level: &str, needle: &str) -> usize {
    logs.lines()
        .filter(|l| l.split_whitespace().nth(1) == Some(level) && l.contains(needle))
        .count()
}

/// A daemon that must never be reached.
struct Unreachable(Arc<AtomicUsize>);

struct Never;

impl Backend for Unreachable {
    fn open(&self) -> Box<dyn Session> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::new(Never)
    }
    fn max_message_bytes(&self) -> usize {
        64 * 1024
    }
}

impl Session for Never {
    fn handle_text<'a>(&'a mut self, _: &'a str, _: Hangup) -> BoxFuture<'a, Message> {
        Box::pin(async {
            Message::event(Event::Error {
                session_id: None,
                error: ProtoError::new(ErrorCode::Internal, "unreachable"),
            })
        })
    }
    fn max_message_bytes(&self) -> usize {
        64 * 1024
    }
    fn execute(
        &mut self,
        _: Command,
        _: Hangup,
    ) -> BoxFuture<'_, Result<CommandResult, ProtoError>> {
        Box::pin(async { Err(ProtoError::new(ErrorCode::Internal, "unreachable")) })
    }
    fn next_event(&mut self) -> BoxFuture<'_, Option<Event>> {
        Box::pin(std::future::pending())
    }
}

/// Write `request` on a fresh connection and read until the server closes.
async fn exchange(addr: SocketAddr, request: &[u8]) -> String {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut tcp = TcpStream::connect(addr).await.unwrap();
        tcp.write_all(request).await.unwrap();
        let mut raw = Vec::new();
        let _ = tcp.read_to_end(&mut raw).await;
        String::from_utf8_lossy(&raw).into_owned()
    })
    .await
    .expect("the server answers and closes")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_flood_of_refused_requests_writes_a_bounded_number_of_warnings() {
    let capture = logs();
    let dir = std::env::temp_dir().join(format!("dictate-server-refusals-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let token_file = dir.join("api-token");
    let (token, _) = dictate_server::token::ensure(&token_file).unwrap();
    let opened = Arc::new(AtomicUsize::new(0));
    let server = ApiServer::start(
        &ApiConfig {
            enabled: true,
            bind: "127.0.0.1:0".into(),
            token_file: token_file.to_string_lossy().into_owned(),
            ..ApiConfig::default()
        },
        Arc::new(Unreachable(opened.clone())),
    )
    .await
    .unwrap()
    .unwrap();
    let addr = server.local_addr();

    // No `Connection: close` from the client: the server must close on its
    // own after every refusal.
    let total = 120;
    for i in 0..total {
        let request = if i % 2 == 0 {
            format!(
                "GET /v1/status HTTP/1.1\r\nHost: evil.example\r\nAuthorization: Bearer {token}\r\n\r\n"
            )
        } else {
            format!(
                "GET /v1/status HTTP/1.1\r\nHost: {addr}\r\nOrigin: http://evil.example\r\nAuthorization: Bearer {token}\r\n\r\n"
            )
        };
        let reply = exchange(addr, request.as_bytes()).await;
        assert!(
            reply.starts_with("HTTP/1.1 421") || reply.starts_with("HTTP/1.1 403"),
            "{reply}"
        );
        assert!(
            reply.to_ascii_lowercase().contains("connection: close"),
            "{reply}"
        );
    }

    // Two requests on one keep-alive connection: one answer, then the close.
    let twice = "GET /v1/status HTTP/1.1\r\nHost: evil.example\r\n\r\n".repeat(2);
    let reply = exchange(addr, twice.as_bytes()).await;
    assert_eq!(reply.matches("HTTP/1.1 421").count(), 1, "{reply}");

    server.shutdown().await;
    let _ = std::fs::remove_dir_all(dir);

    let logs = String::from_utf8_lossy(&capture.0.lock().unwrap()).into_owned();
    let warned = count(&logs, "WARN", "network API refused");
    let quiet = count(&logs, "DEBUG", "network API refused");
    assert_eq!(
        warned + quiet,
        total + 1,
        "every refusal is still recorded ({warned} warnings)"
    );
    assert!(warned <= 25, "{warned} warnings for {} refusals", total + 1);
    assert!(warned >= 1, "the budget admits warnings at all");
    assert!(!logs.contains(&token), "the token never reaches the logs");
    assert_eq!(
        opened.load(Ordering::SeqCst),
        0,
        "the daemon was never reached"
    );
}
