//! The transport's admission rules, limits and lifecycle, against a fake
//! daemon. Ephemeral ports on 127.0.0.1 only.
//!
//! The real daemon behind the same transport (WAV upload → text, capability
//! enforcement, event scoping) is exercised in `dictated/tests/network_api.rs`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dictate_proto::{
    Capabilities, Command, CommandResult, ErrorCode, Event, Message, ProtoError, ServerHello,
    ServerInfo, Transcript,
};
use dictate_server::{ApiConfig, ApiServer, Backend, BoxFuture, Hangup, Session, StartError};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;

const TIMEOUT: Duration = Duration::from_secs(10);

async fn within<T>(what: &str, fut: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(TIMEOUT, fut)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

// --- a fake daemon -----------------------------------------------------------

#[derive(Default)]
struct Calls {
    commands: Mutex<Vec<String>>,
    opened: AtomicUsize,
    dropped: AtomicUsize,
}

/// The socket's pre-handshake ceiling, as `dictated` applies it.
const PRE_HANDSHAKE: usize = 64 * 1024;

struct FakeBackend {
    calls: Arc<Calls>,
    /// The negotiated message limit after a handshake.
    granted: usize,
}

struct FakeSession {
    calls: Arc<Calls>,
    handshaken: bool,
    granted: usize,
}

impl Drop for FakeSession {
    fn drop(&mut self) {
        self.calls.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

impl Backend for FakeBackend {
    fn open(&self) -> Box<dyn Session> {
        self.calls.opened.fetch_add(1, Ordering::SeqCst);
        Box::new(FakeSession {
            calls: self.calls.clone(),
            handshaken: false,
            granted: self.granted,
        })
    }

    fn max_message_bytes(&self) -> usize {
        PRE_HANDSHAKE.max(self.granted)
    }
}

impl FakeSession {
    async fn run(&mut self, command: Command, hangup: Hangup) -> Result<CommandResult, ProtoError> {
        self.calls
            .commands
            .lock()
            .unwrap()
            .push(command.name().to_string());
        match command {
            Command::Handshake(_) => {
                self.handshaken = true;
                Ok(CommandResult::Handshake(Box::new(ServerHello {
                    protocol_version: 1,
                    supported_versions: vec![1],
                    server: ServerInfo::new("fake", "0"),
                    capabilities: ApiConfig::default().grant(),
                })))
            }
            _ if !self.handshaken => Err(ProtoError::new(ErrorCode::HandshakeRequired, "x")),
            Command::TranscribeAudio { audio, options } => {
                // `app = "block"` simulates a long transcription: it only ends
                // when the peer hangs up (or the future is dropped).
                if options.as_ref().and_then(|o| o.app.as_deref()) == Some("block") {
                    hangup.await;
                    return Err(ProtoError::new(ErrorCode::Cancelled, "hung up"));
                }
                // `app = "stall"`: never ends, and ignores the hang-up too.
                if options.as_ref().and_then(|o| o.app.as_deref()) == Some("stall") {
                    drop(hangup);
                    std::future::pending::<()>().await;
                }
                let len = match audio {
                    dictate_proto::AudioSource::Inline { data, .. } => data.len(),
                    _ => 0,
                };
                Ok(CommandResult::Transcript(Box::new(Transcript::delivered(
                    format!("{len} bytes"),
                ))))
            }
            Command::GetStatus => Ok(CommandResult::Ack),
            other => Err(ProtoError::new(
                ErrorCode::Forbidden,
                format!("not permitted: {}", other.name()),
            )),
        }
    }
}

impl Session for FakeSession {
    fn handle_text<'a>(&'a mut self, text: &'a str, hangup: Hangup) -> BoxFuture<'a, Message> {
        Box::pin(async move {
            match Message::parse(text) {
                Ok(Message::Request(r)) => match self.run(r.command, hangup).await {
                    Ok(result) => Message::ok(r.id, result),
                    Err(e) => Message::err(r.id, e),
                },
                Ok(_) => Message::event(Event::Error {
                    session_id: None,
                    error: ProtoError::new(ErrorCode::MalformedRequest, "expected a request"),
                }),
                Err(e) => Message::event(Event::Error {
                    session_id: None,
                    error: e,
                }),
            }
        })
    }

    fn max_message_bytes(&self) -> usize {
        if self.handshaken {
            self.granted
        } else {
            PRE_HANDSHAKE
        }
    }

    fn execute(
        &mut self,
        command: Command,
        hangup: Hangup,
    ) -> BoxFuture<'_, Result<CommandResult, ProtoError>> {
        Box::pin(self.run(command, hangup))
    }

    fn next_event(&mut self) -> BoxFuture<'_, Option<Event>> {
        Box::pin(std::future::pending())
    }
}

// --- fixtures ------------------------------------------------------------------

fn temp_dir() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "dictate-server-it-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Running {
    server: Option<ApiServer>,
    addr: SocketAddr,
    token: String,
    token_file: PathBuf,
    calls: Arc<Calls>,
    dir: PathBuf,
}

impl Running {
    async fn start(tweak: impl FnOnce(&mut ApiConfig)) -> Self {
        Self::start_with(
            tweak,
            ApiConfig::default().limits().max_message_bytes as usize,
        )
        .await
    }

    /// As [`Running::start`], with the message limit the fake daemon grants
    /// at handshake.
    async fn start_with(tweak: impl FnOnce(&mut ApiConfig), granted: usize) -> Self {
        let dir = temp_dir();
        let token_file = dir.join("api-token");
        let (token, _) = dictate_server::token::ensure(&token_file).unwrap();
        let mut config = ApiConfig {
            enabled: true,
            bind: "127.0.0.1:0".into(),
            token_file: token_file.to_string_lossy().into_owned(),
            ..ApiConfig::default()
        };
        tweak(&mut config);
        let calls = Arc::new(Calls::default());
        let server = ApiServer::start(
            &config,
            Arc::new(FakeBackend {
                calls: calls.clone(),
                granted,
            }),
        )
        .await
        .expect("the API starts")
        .expect("the API is enabled");
        Self {
            addr: server.local_addr(),
            server: Some(server),
            token,
            token_file,
            calls,
            dir,
        }
    }

    fn host(&self) -> String {
        self.addr.to_string()
    }

    async fn stop(mut self) {
        if let Some(server) = self.server.take() {
            server.shutdown().await;
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null)
    }
    fn code(&self) -> String {
        self.json()["code"].as_str().unwrap_or_default().to_string()
    }
}

/// One raw HTTP/1.1 exchange. The server closes after each response.
async fn exchange(addr: SocketAddr, request: Vec<u8>) -> Reply {
    within("an HTTP exchange", async {
        let mut tcp = TcpStream::connect(addr).await.unwrap();
        tcp.write_all(&request).await.unwrap();
        let mut raw = Vec::new();
        let _ = tcp.read_to_end(&mut raw).await;
        parse(&raw)
    })
    .await
}

fn parse(raw: &[u8]) -> Reply {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("a complete response head");
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .unwrap()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_string(), v.trim().to_string()))
        .collect();
    Reply {
        status,
        headers,
        body: raw[split + 4..].to_vec(),
    }
}

fn request(method: &str, path: &str, host: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    // `Connection: close` so `exchange` can read to EOF.
    let mut out = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    for (n, v) in headers {
        out.push_str(&format!("{n}: {v}\r\n"));
    }
    if !body.is_empty() || method == "POST" {
        out.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    out.push_str("\r\n");
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

// --- authentication ------------------------------------------------------------

#[tokio::test]
async fn a_valid_token_is_admitted() {
    let api = Running::start(|_| {}).await;
    let auth = bearer(&api.token);
    let reply = exchange(
        api.addr,
        request(
            "GET",
            "/v1/status",
            &api.host(),
            &[("Authorization", &auth)],
            b"",
        ),
    )
    .await;
    assert_eq!(
        reply.status,
        200,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    assert_eq!(reply.header("cache-control"), Some("no-store"));
    assert!(
        reply
            .headers
            .iter()
            .all(|(n, _)| !n.to_ascii_lowercase().starts_with("access-control-")),
        "no CORS header, ever"
    );
    api.stop().await;
}

#[tokio::test]
async fn every_bad_credential_is_401_and_reaches_nothing() {
    let wrong = bearer(&dictate_server::token::generate().unwrap());
    let cases: Vec<Vec<(&str, &str)>> = vec![
        vec![],
        vec![("Authorization", "Basic dXNlcjpwYXNz")],
        vec![("Authorization", "Bearer")],
        vec![("Authorization", wrong.as_str())],
    ];
    // A server per case, so the failures stay under the lockout threshold
    // and every answer is the authentication verdict itself.
    for headers in &cases {
        let api = Running::start(|_| {}).await;
        for path in ["/v1/status", "/v1/ws", "/nope"] {
            let reply = exchange(api.addr, request("GET", path, &api.host(), headers, b"")).await;
            assert_eq!(reply.status, 401, "{path} {headers:?}");
            assert_eq!(reply.header("www-authenticate"), Some("Bearer"));
            assert_eq!(reply.code(), "unauthorized");
        }
        let upload = exchange(
            api.addr,
            request("POST", "/v1/transcribe", &api.host(), headers, b"RIFF...."),
        )
        .await;
        assert_eq!(upload.status, 401, "{headers:?}");
        assert_eq!(
            api.calls.opened.load(Ordering::SeqCst),
            0,
            "no unauthenticated request may reach the daemon"
        );
        api.stop().await;
    }
}

#[tokio::test]
async fn a_token_in_the_query_string_is_not_a_credential() {
    let api = Running::start(|_| {}).await;
    let path = format!("/v1/status?token={}", api.token);
    let reply = exchange(api.addr, request("GET", &path, &api.host(), &[], b"")).await;
    assert_eq!(reply.status, 401);
    api.stop().await;
}

#[tokio::test]
async fn repeated_failures_lock_the_peer_out_even_with_the_right_token_after() {
    let api = Running::start(|c| {
        c.requests_per_minute = 6000;
        c.burst = 100;
    })
    .await;
    for _ in 0..dictate_server::limit::MAX_AUTH_FAILURES {
        let reply = exchange(
            api.addr,
            request(
                "GET",
                "/v1/status",
                &api.host(),
                &[("Authorization", "Bearer dct1_nope")],
                b"",
            ),
        )
        .await;
        assert_eq!(reply.status, 401);
    }
    let auth = bearer(&api.token);
    let reply = exchange(
        api.addr,
        request(
            "GET",
            "/v1/status",
            &api.host(),
            &[("Authorization", &auth)],
            b"",
        ),
    )
    .await;
    assert_eq!(reply.status, 429);
    assert_eq!(reply.code(), "rate_limited");
    assert!(reply.header("retry-after").is_some());
    api.stop().await;
}

// --- browsers ------------------------------------------------------------------

#[tokio::test]
async fn any_origin_is_refused_before_authentication() {
    let api = Running::start(|_| {}).await;
    let auth = bearer(&api.token);
    for origin in [
        "http://evil.example",
        "null",
        &format!("http://{}", api.host()),
    ] {
        let reply = exchange(
            api.addr,
            request(
                "GET",
                "/v1/status",
                &api.host(),
                &[("Authorization", &auth), ("Origin", origin)],
                b"",
            ),
        )
        .await;
        assert_eq!(reply.status, 403, "{origin}");
        assert_eq!(reply.code(), "forbidden");
    }
    // A CORS preflight gets nothing that would let a page proceed.
    let preflight = exchange(
        api.addr,
        request(
            "OPTIONS",
            "/v1/transcribe",
            &api.host(),
            &[
                ("Origin", "http://evil.example"),
                ("Access-Control-Request-Method", "POST"),
            ],
            b"",
        ),
    )
    .await;
    assert_eq!(preflight.status, 403);
    assert!(preflight.header("access-control-allow-origin").is_none());
    assert_eq!(api.calls.opened.load(Ordering::SeqCst), 0);
    api.stop().await;
}

#[tokio::test]
async fn a_dns_rebinding_host_is_refused() {
    let api = Running::start(|_| {}).await;
    let auth = bearer(&api.token);
    let port = api.addr.port();
    for host in [
        format!("evil.example:{port}"),
        "evil.example".into(),
        String::new(),
    ] {
        let reply = exchange(
            api.addr,
            request("GET", "/v1/status", &host, &[("Authorization", &auth)], b""),
        )
        .await;
        assert_eq!(reply.status, 421, "{host:?}");
    }
    let named = exchange(
        api.addr,
        request(
            "GET",
            "/v1/status",
            &format!("localhost:{port}"),
            &[("Authorization", &auth)],
            b"",
        ),
    )
    .await;
    assert_eq!(named.status, 200, "localhost is a loopback bind's own name");
    api.stop().await;
}

// --- limits --------------------------------------------------------------------

#[tokio::test]
async fn an_oversized_upload_is_413_declared_or_streamed() {
    let api = Running::start(|c| c.max_upload_bytes = 1_000).await;
    let auth = bearer(&api.token);
    let declared = exchange(
        api.addr,
        request(
            "POST",
            "/v1/transcribe",
            &api.host(),
            &[("Authorization", &auth)],
            &vec![0u8; 1_001],
        ),
    )
    .await;
    assert_eq!(declared.status, 413);
    assert_eq!(declared.code(), "payload_too_large");

    // Chunked, so no Content-Length to refuse early: the cap applies while
    // reading.
    let mut chunked = format!(
        "POST /v1/transcribe HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nAuthorization: {auth}\r\nTransfer-Encoding: chunked\r\n\r\n",
        api.host()
    )
    .into_bytes();
    for _ in 0..3 {
        chunked.extend_from_slice(b"200\r\n");
        chunked.extend_from_slice(&[0u8; 0x200]);
        chunked.extend_from_slice(b"\r\n");
    }
    chunked.extend_from_slice(b"0\r\n\r\n");
    let streamed = exchange(api.addr, chunked).await;
    assert_eq!(streamed.status, 413);

    let fits = exchange(
        api.addr,
        request(
            "POST",
            "/v1/transcribe",
            &api.host(),
            &[("Authorization", &auth)],
            &vec![0u8; 1_000],
        ),
    )
    .await;
    assert_eq!(fits.status, 200);
    assert_eq!(fits.json()["text"], "1000 bytes");
    api.stop().await;
}

#[tokio::test]
async fn a_peer_over_its_rate_gets_429_with_retry_after() {
    let api = Running::start(|c| {
        c.requests_per_minute = 1;
        c.burst = 2;
    })
    .await;
    let auth = bearer(&api.token);
    let get = || {
        request(
            "GET",
            "/v1/status",
            &api.host(),
            &[("Authorization", &auth)],
            b"",
        )
    };
    assert_eq!(exchange(api.addr, get()).await.status, 200);
    assert_eq!(exchange(api.addr, get()).await.status, 200);
    let third = exchange(api.addr, get()).await;
    assert_eq!(third.status, 429);
    assert!(third.header("retry-after").is_some());
    api.stop().await;
}

#[tokio::test]
async fn unsupported_content_types_and_bad_params_are_request_errors() {
    let api = Running::start(|_| {}).await;
    let auth = bearer(&api.token);
    let ogg = exchange(
        api.addr,
        request(
            "POST",
            "/v1/transcribe",
            &api.host(),
            &[("Authorization", &auth), ("Content-Type", "audio/ogg")],
            b"OggS",
        ),
    )
    .await;
    assert_eq!(ogg.code(), "audio_format_unsupported");
    assert_eq!(
        ogg.status,
        ErrorCode::AudioFormatUnsupported.http_status(),
        "the protocol's status for the code"
    );
    let bad = exchange(
        api.addr,
        request(
            "POST",
            "/v1/transcribe?privacy=maybe",
            &api.host(),
            &[("Authorization", &auth)],
            b"x",
        ),
    )
    .await;
    assert_eq!(bad.status, 400);
    assert_eq!(bad.code(), "invalid_params");
    api.stop().await;
}

#[tokio::test]
async fn a_client_that_hangs_up_mid_upload_drops_its_daemon_connection() {
    let api = Running::start(|_| {}).await;
    let auth = bearer(&api.token);
    let mut tcp = TcpStream::connect(api.addr).await.unwrap();
    tcp.write_all(&request(
        "POST",
        "/v1/transcribe?app=block",
        &api.host(),
        &[("Authorization", &auth)],
        b"RIFF",
    ))
    .await
    .unwrap();
    within("the daemon connection to open", async {
        while api.calls.opened.load(Ordering::SeqCst) == 0
            || !api
                .calls
                .commands
                .lock()
                .unwrap()
                .contains(&"transcribe_audio".to_string())
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    drop(tcp);
    within("the daemon connection to be dropped", async {
        while api.calls.dropped.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    api.stop().await;
}

// --- startup refusals ----------------------------------------------------------

#[tokio::test]
async fn startup_refuses_a_lan_bind_without_the_opt_in() {
    let dir = temp_dir();
    let token_file = dir.join("api-token");
    dictate_server::token::ensure(&token_file).unwrap();
    let backend = || -> Arc<dyn Backend> {
        Arc::new(FakeBackend {
            calls: Arc::new(Calls::default()),
            granted: PRE_HANDSHAKE,
        })
    };
    for config in [
        ApiConfig {
            enabled: true,
            bind: "192.0.2.10:0".into(),
            token_file: token_file.to_string_lossy().into_owned(),
            ..ApiConfig::default()
        },
        ApiConfig {
            enabled: true,
            bind: "192.0.2.10:0".into(),
            allow_lan: true,
            token_file: token_file.to_string_lossy().into_owned(),
            ..ApiConfig::default()
        },
        ApiConfig {
            enabled: true,
            bind: "0.0.0.0:0".into(),
            allow_lan: true,
            allow_plaintext_lan: true,
            token_file: token_file.to_string_lossy().into_owned(),
            ..ApiConfig::default()
        },
    ] {
        match ApiServer::start(&config, backend()).await {
            Err(StartError::Refused(_)) => {}
            Err(other) => panic!("{}: expected a refusal, got {other}", config.bind),
            Ok(_) => panic!("{}: must not start", config.bind),
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn startup_refuses_without_a_usable_token() {
    use std::os::unix::fs::PermissionsExt;
    let dir = temp_dir();
    let token_file = dir.join("api-token");
    let config = ApiConfig {
        enabled: true,
        bind: "127.0.0.1:0".into(),
        token_file: token_file.to_string_lossy().into_owned(),
        ..ApiConfig::default()
    };
    let backend: Arc<dyn Backend> = Arc::new(FakeBackend {
        calls: Arc::new(Calls::default()),
        granted: PRE_HANDSHAKE,
    });
    assert!(matches!(
        ApiServer::start(&config, backend.clone()).await,
        Err(StartError::Token(_))
    ));
    assert!(!token_file.exists(), "startup never mints a token");
    dictate_server::token::ensure(&token_file).unwrap();
    std::fs::set_permissions(&token_file, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        ApiServer::start(&config, backend.clone()).await,
        Err(StartError::Token(_))
    ));
    // A disabled API needs nothing at all.
    let disabled = ApiConfig {
        enabled: false,
        ..config
    };
    assert!(ApiServer::start(&disabled, backend)
        .await
        .unwrap()
        .is_none());
    let _ = std::fs::remove_dir_all(dir);
}

// --- WebSocket -----------------------------------------------------------------

async fn ws_connect(
    api: &Running,
    headers: &[(&str, &str)],
) -> Result<tokio_tungstenite::WebSocketStream<TcpStream>, tokio_tungstenite::tungstenite::Error> {
    let mut req = format!("ws://{}/v1/ws", api.host())
        .into_client_request()
        .unwrap();
    for (n, v) in headers {
        req.headers_mut()
            .insert(axum_name(n), v.parse().expect("a header value"));
    }
    let tcp = TcpStream::connect(api.addr).await.unwrap();
    within(
        "the WebSocket handshake",
        tokio_tungstenite::client_async(req, tcp),
    )
    .await
    .map(|(ws, _)| ws)
}

fn axum_name(name: &str) -> tokio_tungstenite::tungstenite::http::HeaderName {
    tokio_tungstenite::tungstenite::http::HeaderName::from_bytes(name.as_bytes()).unwrap()
}

fn request_line(id: u64, command: serde_json::Value) -> String {
    serde_json::json!({"kind":"request","v":1,"id":id,"command":command}).to_string()
}

async fn next_text(ws: &mut tokio_tungstenite::WebSocketStream<TcpStream>) -> serde_json::Value {
    loop {
        match within("a WebSocket message", ws.next()).await {
            Some(Ok(WsMessage::Text(t))) => return serde_json::from_str(&t).unwrap(),
            Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_))) => {}
            other => panic!("expected a text message, got {other:?}"),
        }
    }
}

async fn close_code(ws: &mut tokio_tungstenite::WebSocketStream<TcpStream>) -> u16 {
    loop {
        match within("the close frame", ws.next()).await {
            Some(Ok(WsMessage::Close(Some(frame)))) => return frame.code.into(),
            Some(Ok(WsMessage::Text(_) | WsMessage::Ping(_) | WsMessage::Pong(_))) => {}
            other => panic!("expected a close frame, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn the_websocket_upgrade_is_gated_like_every_request() {
    let api = Running::start(|_| {}).await;
    let status = |e: tokio_tungstenite::tungstenite::Error| match e {
        tokio_tungstenite::tungstenite::Error::Http(r) => r.status().as_u16(),
        other => panic!("expected an HTTP refusal, got {other}"),
    };
    assert_eq!(status(ws_connect(&api, &[]).await.unwrap_err()), 401);
    let auth = bearer(&api.token);
    // Cross-site WebSocket hijacking: a browser always sends Origin.
    assert_eq!(
        status(
            ws_connect(
                &api,
                &[("Authorization", &auth), ("Origin", "http://evil.example")]
            )
            .await
            .unwrap_err()
        ),
        403
    );
    assert_eq!(api.calls.opened.load(Ordering::SeqCst), 0);
    api.stop().await;
}

#[tokio::test]
async fn a_websocket_session_speaks_envelopes() {
    let api = Running::start(|_| {}).await;
    let auth = bearer(&api.token);
    let mut ws = ws_connect(&api, &[("Authorization", &auth)]).await.unwrap();
    ws.send(WsMessage::Text(
        request_line(1, serde_json::json!({"type":"get_status"})).into(),
    ))
    .await
    .unwrap();
    let reply = next_text(&mut ws).await;
    assert_eq!(reply["error"]["code"], "handshake_required", "{reply}");

    ws.send(WsMessage::Text(
        request_line(
            2,
            serde_json::json!({"type":"handshake","protocol_version":1,"client":{"name":"t","kind":"cli"}}),
        )
        .into(),
    ))
    .await
    .unwrap();
    let reply = next_text(&mut ws).await;
    assert_eq!(reply["id"], 2);
    assert_eq!(reply["result"]["type"], "handshake");

    ws.close(None).await.unwrap();
    within("the daemon connection to be dropped", async {
        while api.calls.dropped.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    api.stop().await;
}

#[tokio::test]
async fn binary_frames_close_the_session() {
    let api = Running::start(|_| {}).await;
    let auth = bearer(&api.token);
    let mut ws = ws_connect(&api, &[("Authorization", &auth)]).await.unwrap();
    ws.send(WsMessage::Binary(vec![b'D', b'C', b'T', b'A'].into()))
        .await
        .unwrap();
    assert_eq!(close_code(&mut ws).await, 1003);
    api.stop().await;
}

#[tokio::test]
async fn pipelining_past_the_queue_closes_the_session_and_cancels_its_work() {
    let api = Running::start(|c| {
        c.requests_per_minute = 6000;
        c.burst = 100;
    })
    .await;
    let auth = bearer(&api.token);
    let mut ws = ws_connect(&api, &[("Authorization", &auth)]).await.unwrap();
    ws.send(WsMessage::Text(
        request_line(
            1,
            serde_json::json!({"type":"handshake","protocol_version":1,"client":{"name":"t"}}),
        )
        .into(),
    ))
    .await
    .unwrap();
    let _ = next_text(&mut ws).await;
    // One request that never finishes on its own...
    ws.send(WsMessage::Text(
        request_line(
            2,
            serde_json::json!({"type":"transcribe_audio","audio":{"source":"inline","format":{"encoding":"wav"},"data":"UklGRg=="},"options":{"app":"block"}}),
        )
        .into(),
    ))
    .await
    .unwrap();
    // ...and more queued behind it than the session will hold.
    for id in 3..(3 + 8) {
        ws.send(WsMessage::Text(
            request_line(id, serde_json::json!({"type":"get_status"})).into(),
        ))
        .await
        .unwrap();
    }
    assert_eq!(close_code(&mut ws).await, 1008);
    api.stop().await;
}

#[tokio::test]
async fn shutdown_closes_open_websocket_sessions() {
    let api = Running::start(|_| {}).await;
    let auth = bearer(&api.token);
    let mut ws = ws_connect(&api, &[("Authorization", &auth)]).await.unwrap();
    let calls = api.calls.clone();
    within("the session to open", async {
        while calls.opened.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    api.stop().await;
    assert_eq!(close_code(&mut ws).await, 1001);
}

// --- every WebSocket request is admitted again ---------------------------------

async fn ws_send(ws: &mut tokio_tungstenite::WebSocketStream<TcpStream>, line: String) {
    ws.send(WsMessage::Text(line.into())).await.unwrap();
}

fn handshake_line(id: u64) -> String {
    request_line(
        id,
        serde_json::json!({"type":"handshake","protocol_version":1,"client":{"name":"t"}}),
    )
}

fn commands(calls: &Calls) -> Vec<String> {
    calls.commands.lock().unwrap().clone()
}

async fn authed_ws(api: &Running) -> tokio_tungstenite::WebSocketStream<TcpStream> {
    let auth = bearer(&api.token);
    ws_connect(api, &[("Authorization", &auth)]).await.unwrap()
}

/// Wait for the server to end the session, however it does it.
async fn ended(ws: &mut tokio_tungstenite::WebSocketStream<TcpStream>) -> Option<u16> {
    loop {
        match within("the session to end", ws.next()).await {
            Some(Ok(WsMessage::Close(frame))) => return frame.map(|f| f.code.into()),
            Some(Ok(_)) => {}
            Some(Err(_)) | None => return None,
        }
    }
}

/// WS_TOKEN_REVOCATION_BYPASS: rotating the token ends every session opened
/// with the old one; the next command does not run.
#[tokio::test]
async fn rotating_the_token_closes_open_websocket_sessions() {
    let api = Running::start(|_| {}).await;
    let mut ws = authed_ws(&api).await;
    ws_send(&mut ws, handshake_line(1)).await;
    assert_eq!(next_text(&mut ws).await["id"], 1);

    let rotated = dictate_server::token::rotate(&api.token_file).unwrap();
    ws_send(
        &mut ws,
        request_line(2, serde_json::json!({"type":"get_status"})),
    )
    .await;
    assert_eq!(close_code(&mut ws).await, 1008);
    assert_eq!(
        commands(&api.calls),
        vec!["handshake"],
        "nothing ran on the revoked token"
    );

    // The rotated token opens a session as usual.
    let auth = bearer(&rotated);
    assert!(ws_connect(&api, &[("Authorization", &auth)]).await.is_ok());
    api.stop().await;
}

/// A session that sends nothing is closed too, when the token file is
/// deleted or made readable by others.
#[tokio::test]
async fn revoking_the_token_closes_an_idle_websocket_session() {
    use std::os::unix::fs::PermissionsExt;
    for revoke in ["delete", "chmod 644"] {
        let api = Running::start(|_| {}).await;
        let mut ws = authed_ws(&api).await;
        ws_send(&mut ws, handshake_line(1)).await;
        let _ = next_text(&mut ws).await;
        if revoke == "delete" {
            std::fs::remove_file(&api.token_file).unwrap();
        } else {
            std::fs::set_permissions(&api.token_file, std::fs::Permissions::from_mode(0o644))
                .unwrap();
        }
        assert_eq!(close_code(&mut ws).await, 1008, "{revoke}");
        api.stop().await;
    }
}

/// WS_THROTTLE_FAIL_OPEN: a request the peer's rate will not admit within
/// the bounded wait is answered `rate_limited` and never runs.
#[tokio::test]
async fn an_over_rate_websocket_request_is_refused_not_run() {
    let api = Running::start(|c| {
        c.requests_per_minute = 1;
        c.burst = 1;
    })
    .await;
    // The upgrade spends the only token; the next one is a minute away.
    let mut ws = authed_ws(&api).await;
    let started = std::time::Instant::now();
    ws_send(&mut ws, handshake_line(1)).await;
    let reply = next_text(&mut ws).await;
    assert_eq!(reply["id"], 1, "{reply}");
    assert_eq!(reply["error"]["code"], "rate_limited", "{reply}");
    assert!(reply["error"]["detail"]["retry_after_s"].as_u64().unwrap() > 5);
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "refused at once, not after a wait that ends in running it"
    );
    assert!(commands(&api.calls).is_empty(), "nothing ran");
    api.stop().await;
}

/// A short wait is waited out, then the request runs on a token it holds.
#[tokio::test]
async fn a_websocket_request_waits_out_a_short_refill() {
    let api = Running::start(|c| {
        c.requests_per_minute = 600;
        c.burst = 1;
    })
    .await;
    let mut ws = authed_ws(&api).await;
    ws_send(&mut ws, handshake_line(1)).await;
    let reply = next_text(&mut ws).await;
    assert_eq!(reply["result"]["type"], "handshake", "{reply}");
    assert_eq!(commands(&api.calls), vec!["handshake"]);
    api.stop().await;
}

/// A peer locked out over HTTP is locked out on the WebSocket it already
/// holds: its commands are refused for the lockout, not run after a pause.
#[tokio::test]
async fn a_locked_out_peer_runs_nothing_on_its_open_websocket() {
    let api = Running::start(|c| {
        c.requests_per_minute = 6000;
        c.burst = 100;
    })
    .await;
    let mut ws = authed_ws(&api).await;
    ws_send(&mut ws, handshake_line(1)).await;
    let _ = next_text(&mut ws).await;
    let wrong = bearer(&dictate_server::token::generate().unwrap());
    for _ in 0..dictate_server::limit::MAX_AUTH_FAILURES {
        let reply = exchange(
            api.addr,
            request(
                "GET",
                "/v1/status",
                &api.host(),
                &[("Authorization", &wrong)],
                b"",
            ),
        )
        .await;
        assert_eq!(reply.status, 401);
    }
    let started = std::time::Instant::now();
    ws_send(
        &mut ws,
        request_line(2, serde_json::json!({"type":"get_status"})),
    )
    .await;
    let reply = next_text(&mut ws).await;
    assert_eq!(reply["id"], 2, "{reply}");
    assert_eq!(reply["error"]["code"], "rate_limited", "{reply}");
    assert!(started.elapsed() < Duration::from_secs(4));
    assert_eq!(commands(&api.calls), vec!["handshake"]);
    api.stop().await;
}

/// WS_NEGOTIATED_MESSAGE_LIMIT_BYPASS: after the handshake a message is held
/// to the grant's limit, not the API's encoded upload size.
#[tokio::test]
async fn a_websocket_message_over_the_negotiated_limit_is_refused_unread() {
    let api = Running::start_with(|_| {}, 1024).await;
    let mut ws = authed_ws(&api).await;
    ws_send(&mut ws, handshake_line(1)).await;
    let _ = next_text(&mut ws).await;
    let padded = serde_json::json!({
        "kind": "request", "v": 1, "id": 2,
        "command": {"type": "get_status"},
        "padding": "x".repeat(4096),
    })
    .to_string();
    ws_send(&mut ws, padded).await;
    let reply = next_text(&mut ws).await;
    assert_eq!(reply["kind"], "event", "{reply}");
    assert_eq!(
        reply["event"]["error"]["code"], "payload_too_large",
        "{reply}"
    );
    assert_eq!(close_code(&mut ws).await, 1009);
    assert_eq!(commands(&api.calls), vec!["handshake"]);

    // The transport buffers no more than the connection could ever accept
    // (here the 64 KiB pre-handshake ceiling), whatever [api] allows uploads.
    let mut ws = authed_ws(&api).await;
    let huge = serde_json::json!({
        "kind": "request", "v": 1, "id": 3,
        "command": {"type": "handshake", "protocol_version": 1, "client": {"name": "t"}},
        "padding": "x".repeat(128 * 1024),
    })
    .to_string();
    let _ = ws.send(WsMessage::Text(huge.into())).await;
    let _ = ended(&mut ws).await;
    assert_eq!(
        commands(&api.calls),
        vec!["handshake"],
        "the huge one never ran"
    );
    api.stop().await;
}

/// Before the handshake, the socket's 64 KiB ceiling applies, however large
/// the grant the handshake would bring.
#[tokio::test]
async fn before_the_handshake_a_websocket_message_is_held_to_64_kib() {
    let api = Running::start(|_| {}).await;
    let mut ws = authed_ws(&api).await;
    let padded = serde_json::json!({
        "kind": "request", "v": 1, "id": 1,
        "command": {"type": "handshake", "protocol_version": 1, "client": {"name": "t"}},
        "padding": "x".repeat(70 * 1024),
    })
    .to_string();
    ws_send(&mut ws, padded).await;
    let reply = next_text(&mut ws).await;
    assert_eq!(
        reply["event"]["error"]["code"], "payload_too_large",
        "{reply}"
    );
    assert_eq!(close_code(&mut ws).await, 1009);
    assert!(commands(&api.calls).is_empty());
    api.stop().await;
}

/// WS_IDLE_SHUTDOWN_NOT_ENFORCED_IN_FLIGHT: shutdown interrupts a request in
/// flight, closes the session, and returns only once its daemon connection
/// has been dropped (which is what cancels an upload).
#[tokio::test]
async fn shutdown_interrupts_a_websocket_request_in_flight() {
    let api = Running::start(|_| {}).await;
    let calls = api.calls.clone();
    let mut ws = authed_ws(&api).await;
    ws_send(&mut ws, handshake_line(1)).await;
    let _ = next_text(&mut ws).await;
    ws_send(
        &mut ws,
        request_line(
            2,
            serde_json::json!({"type":"transcribe_audio","audio":{"source":"inline","format":{"encoding":"wav"},"data":"UklGRg=="},"options":{"app":"block"}}),
        ),
    )
    .await;
    within("the upload to start", async {
        while !commands(&calls).contains(&"transcribe_audio".to_string()) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    within("shutdown", api.stop()).await;
    assert_eq!(calls.opened.load(Ordering::SeqCst), 1);
    assert_eq!(
        calls.dropped.load(Ordering::SeqCst),
        1,
        "the daemon connection is dropped before shutdown returns"
    );
    assert_eq!(close_code(&mut ws).await, 1001);
}

/// WS_IDLE_SHUTDOWN_NOT_ENFORCED_IN_FLIGHT: a peer that disconnects mid-request
/// frees its session at once, even when the command in flight does not watch
/// for the hang-up itself.
#[tokio::test]
async fn a_disconnect_mid_request_frees_the_session_even_if_the_command_ignores_it() {
    let api = Running::start(|_| {}).await;
    let calls = api.calls.clone();
    let mut ws = authed_ws(&api).await;
    ws_send(&mut ws, handshake_line(1)).await;
    let _ = next_text(&mut ws).await;
    ws_send(
        &mut ws,
        request_line(
            2,
            serde_json::json!({"type":"transcribe_audio","audio":{"source":"inline","format":{"encoding":"wav"},"data":"UklGRg=="},"options":{"app":"stall"}}),
        ),
    )
    .await;
    within("the request to start", async {
        while !commands(&calls).contains(&"transcribe_audio".to_string()) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    drop(ws);
    within("the daemon connection to be dropped", async {
        while calls.dropped.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    api.stop().await;
}

// --- TLS -----------------------------------------------------------------------

#[tokio::test]
async fn tls_serves_https_and_reports_the_pinnable_fingerprint() {
    use tokio_rustls::rustls;

    let dir = temp_dir();
    let generated =
        rcgen::generate_simple_self_signed(vec!["127.0.0.1".into(), "localhost".into()]).unwrap();
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");
    std::fs::write(&cert_path, generated.cert.pem()).unwrap();
    std::fs::write(&key_path, generated.signing_key.serialize_pem()).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let api = Running::start(|c| {
        c.tls_cert = cert_path.to_string_lossy().into_owned();
        c.tls_key = key_path.to_string_lossy().into_owned();
    })
    .await;
    let expected = dictate_server::tls::certificate_fingerprint(&cert_path).unwrap();
    assert_eq!(
        api.server.as_ref().unwrap().tls_fingerprint(),
        Some(expected.as_str())
    );

    let mut roots = rustls::RootCertStore::empty();
    roots.add(generated.cert.der().clone()).unwrap();
    let client = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(client));
    let tcp = TcpStream::connect(api.addr).await.unwrap();
    let name = rustls::pki_types::ServerName::try_from("127.0.0.1").unwrap();
    let mut tls = within("the TLS handshake", connector.connect(name, tcp))
        .await
        .unwrap();
    let auth = bearer(&api.token);
    tls.write_all(&request(
        "GET",
        "/v1/status",
        &api.host(),
        &[("Authorization", &auth)],
        b"",
    ))
    .await
    .unwrap();
    let mut raw = Vec::new();
    let _ = within("the HTTPS response", tls.read_to_end(&mut raw)).await;
    assert_eq!(parse(&raw).status, 200);

    // Plain HTTP to the TLS port gets no HTTP answer.
    let mut plain = TcpStream::connect(api.addr).await.unwrap();
    plain
        .write_all(&request(
            "GET",
            "/v1/status",
            &api.host(),
            &[("Authorization", &auth)],
            b"",
        ))
        .await
        .unwrap();
    let mut raw = Vec::new();
    let _ = within("the plaintext attempt to end", plain.read_to_end(&mut raw)).await;
    assert!(!raw.starts_with(b"HTTP/1.1 200"));
    api.stop().await;
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_grant_offered_to_peers_holds_no_local_authority() {
    let grant: Capabilities = ApiConfig::default().grant();
    assert!(!grant.features.config_write);
    assert!(!grant.features.text_injection);
}
