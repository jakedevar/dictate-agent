//! S33: the network API in front of a real daemon — real engine, real dispatch,
//! real socket alongside — with mock hardware. Ephemeral ports on 127.0.0.1
//! only.
//!
//! What these pin, from the security design
//! (`thoughts/shared/plans/2026-10-07-s33-network-api-security-design.md`):
//! a WAV upload comes back as text and types nothing; a network peer holds the
//! transcription grant and nothing else, whatever the socket beside it holds;
//! `raw_text` is opt-in; events are scoped to the peer's own sessions; a peer
//! cannot touch the desk's session; hanging up cancels the peer's upload; and
//! the API refuses to start rather than expose more than it opted into.

mod harness;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use dictate_core::ports::mock::MockStt;
use dictate_proto::{
    AudioFormat, AudioSource, Command, CommandResult, ErrorCode, Event, Message, Outcome,
    ProtoError, SessionId, SessionOptions, State,
};
use dictate_server::{ApiConfig, StartError};
use futures_util::{SinkExt, StreamExt};
use harness::{within, Harness, Setup};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;

// --- fixtures ------------------------------------------------------------------

/// A WAV of `secs` seconds of a quiet tone, 16 kHz mono 16-bit.
fn wav(secs: f64) -> Vec<u8> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut w = hound::WavWriter::new(&mut buf, spec).unwrap();
        for i in 0..((16_000.0 * secs) as usize) {
            let s = ((i as f64 * 440.0 * std::f64::consts::TAU / 16_000.0).sin() * 3000.0) as i16;
            w.write_sample(s).unwrap();
        }
        w.finalize().unwrap();
    }
    buf.into_inner()
}

struct Api {
    addr: SocketAddr,
    token: String,
}

fn api_config(h: &Harness, tweak: impl FnOnce(&mut ApiConfig)) -> ApiConfig {
    let mut config = ApiConfig {
        enabled: true,
        bind: "127.0.0.1:0".into(),
        token_file: h.dir().join("api-token").to_string_lossy().into_owned(),
        // Generous: these tests exercise the daemon, not the throttle.
        requests_per_minute: 6_000,
        burst: 200,
        ..ApiConfig::default()
    };
    tweak(&mut config);
    config
}

/// Issue a token (the operator's `dictated --api-token`) and start the API.
async fn start_api(h: &mut Harness, tweak: impl FnOnce(&mut ApiConfig)) -> Api {
    let config = api_config(h, tweak);
    let (token, _) = dictate_server::token::ensure(&config.token_path()).unwrap();
    let addr = h
        .daemon
        .start_network_api(&config)
        .await
        .expect("the API starts")
        .expect("the API is enabled");
    Api { addr, token }
}

struct Reply {
    status: u16,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null)
    }
}

/// One HTTP/1.1 request with the API's token; the connection closes after.
async fn http(api: &Api, method: &str, path_and_query: &str, body: &[u8]) -> Reply {
    let mut head = format!(
        "{method} {path_and_query} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nAuthorization: Bearer {}\r\n",
        api.addr, api.token
    );
    if method == "POST" {
        head.push_str("Content-Type: audio/wav\r\n");
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");
    let mut request = head.into_bytes();
    request.extend_from_slice(body);
    within("an HTTP exchange", async {
        let mut tcp = TcpStream::connect(api.addr).await.unwrap();
        tcp.write_all(&request).await.unwrap();
        let mut raw = Vec::new();
        let _ = tcp.read_to_end(&mut raw).await;
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("a response head");
        let status = String::from_utf8_lossy(&raw[..split])
            .split(' ')
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        Reply {
            status,
            body: raw[split + 4..].to_vec(),
        }
    })
    .await
}

/// A network peer on the WebSocket.
struct Ws {
    ws: tokio_tungstenite::WebSocketStream<TcpStream>,
    next_id: u64,
    events: Vec<Event>,
}

impl Ws {
    async fn connect(api: &Api) -> Self {
        let mut req = format!("ws://{}/v1/ws", api.addr)
            .into_client_request()
            .unwrap();
        req.headers_mut().insert(
            "authorization",
            format!("Bearer {}", api.token).parse().unwrap(),
        );
        let tcp = TcpStream::connect(api.addr).await.unwrap();
        let (ws, _) = within(
            "the WebSocket handshake",
            tokio_tungstenite::client_async(req, tcp),
        )
        .await
        .expect("the upgrade is accepted");
        Self {
            ws,
            next_id: 1,
            events: Vec::new(),
        }
    }

    async fn read(&mut self) -> Option<Message> {
        loop {
            match self.ws.next().await? {
                Ok(WsMessage::Text(text)) => return Some(Message::parse(&text).unwrap()),
                Ok(WsMessage::Close(_)) | Err(_) => return None,
                Ok(_) => {}
            }
        }
    }

    async fn request(&mut self, command: Command) -> Result<CommandResult, ProtoError> {
        let id = self.next_id;
        self.next_id += 1;
        let line = serde_json::to_string(&Message::request(id, command)).unwrap();
        self.ws.send(WsMessage::Text(line.into())).await.unwrap();
        within("a WebSocket response", async {
            loop {
                match self.read().await.expect("the session stays open") {
                    Message::Response(r) if r.id == id.into() => {
                        return match r.outcome {
                            Outcome::Result(v) => Ok(v),
                            Outcome::Error(e) => Err(e),
                        }
                    }
                    Message::Event(e) => self.events.push(e.event),
                    other => panic!("unexpected {other:?}"),
                }
            }
        })
        .await
    }

    async fn handshake(&mut self) -> dictate_proto::Capabilities {
        let hello = dictate_proto::Hello::new(dictate_proto::ClientInfo::new(
            "phone",
            // Self-described as the local CLI: the grant must not care.
            dictate_proto::ClientKind::Cli,
        ));
        match self.request(Command::Handshake(hello)).await.unwrap() {
            CommandResult::Handshake(h) => h.capabilities,
            other => panic!("expected a handshake, got {other:?}"),
        }
    }

    /// Events that arrive within `grace`, with nothing asked.
    async fn drain(&mut self, grace: Duration) -> Vec<Event> {
        let mut seen = std::mem::take(&mut self.events);
        while let Ok(Some(message)) = tokio::time::timeout(grace, self.read()).await {
            if let Message::Event(e) = message {
                seen.push(e.event);
            }
        }
        seen
    }
}

fn inline_wav(secs: f64) -> AudioSource {
    AudioSource::Inline {
        format: AudioFormat::wav(),
        data: wav(secs),
    }
}

fn expect_code(result: Result<CommandResult, ProtoError>, code: ErrorCode, what: &str) {
    match result {
        Err(e) => assert_eq!(e.code, code, "{what}: {}", e.message),
        Ok(r) => panic!("{what}: expected {code:?}, got {r:?}"),
    }
}

// --- HTTP ------------------------------------------------------------------------

#[tokio::test]
async fn a_wav_uploaded_over_http_comes_back_as_text_and_types_nothing() {
    let mut h = Harness::start().await;
    let api = start_api(&mut h, |_| {}).await;

    let reply = http(&api, "POST", "/v1/transcribe", &wav(1.0)).await;
    assert_eq!(
        reply.status,
        200,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    let body = reply.json();
    assert_eq!(body["type"], "transcript");
    assert_eq!(body["text"], "Hello there");
    assert_eq!(body["route"], "type");
    assert_eq!(body["injection"]["status"], "delivered");
    assert!(
        body.get("raw_text").is_none(),
        "raw_text is opt-in for network peers: {body}"
    );
    assert!(h.injector.injected().is_empty(), "nothing was typed");
    h.stop().await;
}

#[tokio::test]
async fn raw_text_reaches_a_network_peer_only_when_the_operator_opts_in() {
    let mut h = Harness::start().await;
    let api = start_api(&mut h, |c| c.expose_raw_text = true).await;
    let body = http(&api, "POST", "/v1/transcribe", &wav(1.0)).await.json();
    assert_eq!(body["raw_text"], "hello there", "{body}");
    h.stop().await;
}

#[tokio::test]
async fn a_network_peer_cannot_type_or_run_host_routes() {
    let mut h = Harness::start().await;
    let api = start_api(&mut h, |_| {}).await;
    for query in ["inject=true", "route=timer", "route=local", "route=command"] {
        let reply = http(&api, "POST", &format!("/v1/transcribe?{query}"), &wav(0.5)).await;
        assert_eq!(
            reply.status,
            403,
            "{query}: {}",
            String::from_utf8_lossy(&reply.body)
        );
        assert_eq!(reply.json()["code"], "forbidden", "{query}");
    }
    assert!(h.injector.injected().is_empty());
    h.stop().await;
}

#[tokio::test]
async fn network_status_is_scrubbed_and_reports_the_network_grant() {
    let mut h = Harness::start().await;
    let api = start_api(&mut h, |_| {}).await;
    let reply = http(&api, "GET", "/v1/status", b"").await;
    assert_eq!(reply.status, 200);
    let body = reply.json();
    assert_eq!(body["type"], "status");
    assert!(body["daemon"].get("pid").is_none(), "{body}");
    assert!(body.get("audio").is_none(), "{body}");
    assert!(body.get("formatter").is_none(), "{body}");
    let features = &body["capabilities"]["features"];
    assert_eq!(features["transcribe_upload"], true);
    for flag in [
        "text_injection",
        "host_capture",
        "context_read",
        "config_read",
        "config_write",
        "history_read",
        "dictionary_read",
        "diagnostics",
        "raw_text",
    ] {
        assert_ne!(features[flag], true, "{flag} must not reach a network peer");
    }
    assert_eq!(body["capabilities"]["routes"], serde_json::json!(["type"]));
    h.stop().await;
}

// --- WebSocket -------------------------------------------------------------------

#[tokio::test]
async fn a_websocket_session_transcribes_and_sees_only_its_own_events() {
    let mut h = Harness::start().await;
    let api = start_api(&mut h, |_| {}).await;
    let mut ws = Ws::connect(&api).await;

    let caps = ws.handshake().await;
    assert!(caps.features.transcribe_upload);
    assert!(
        !caps.features.text_injection,
        "a self-described CLI is still remote"
    );
    assert!(!caps.features.config_write);
    assert!(!caps.features.raw_text);
    assert!(matches!(
        ws.request(Command::Subscribe { events: Vec::new() })
            .await
            .unwrap(),
        CommandResult::Ack
    ));

    let transcript = match ws
        .request(Command::TranscribeAudio {
            audio: inline_wav(1.0),
            options: None,
        })
        .await
        .unwrap()
    {
        CommandResult::Transcript(t) => t,
        other => panic!("expected a transcript, got {other:?}"),
    };
    assert_eq!(transcript.text.as_str(), "Hello there");
    assert_eq!(transcript.raw_text, None);

    // Its own session's events follow the response, all about one session.
    let events = ws.drain(Duration::from_millis(500)).await;
    let finals: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::Final { transcript, .. } => Some(transcript),
            _ => None,
        })
        .collect();
    assert_eq!(finals.len(), 1, "{events:?}");
    assert_eq!(
        finals[0].raw_text, None,
        "raw_text is redacted in events too"
    );
    let sessions: std::collections::HashSet<SessionId> = events
        .iter()
        .filter_map(|e| e.session_id().cloned())
        .collect();
    assert_eq!(sessions.len(), 1, "{events:?}");
    assert!(events.iter().any(|e| matches!(
        e,
        Event::StateChanged {
            to: State::Done,
            ..
        }
    )));
    assert!(h.injector.injected().is_empty());
    h.stop().await;
}

#[tokio::test]
async fn every_command_beyond_transcription_is_refused_over_the_network() {
    // A daemon whose socket *does* serve config, so a refusal on the network
    // is the network's doing.
    let dir = std::env::temp_dir().join(format!("dictated-net-cfg-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let config_file: PathBuf = dir.join("config.toml");
    let _ = std::fs::remove_file(&config_file);
    let mut h = Harness::with(Setup::default().with_config_file(config_file.clone())).await;
    let local = h.client().await;
    let local_caps = local.hello.as_ref().unwrap().capabilities.clone();
    assert!(
        local_caps.features.config_write,
        "the socket may write config"
    );

    let api = start_api(&mut h, |_| {}).await;
    let mut ws = Ws::connect(&api).await;
    ws.handshake().await;

    let forbidden = [
        Command::StartDictation {
            mode: dictate_proto::DictationMode::Toggle,
            options: None,
        },
        Command::Toggle,
        Command::GetContext,
        Command::GetConfig { path: None },
        Command::SetConfig {
            entries: vec![dictate_proto::ConfigEntry {
                path: "local.host".into(),
                value: serde_json::json!("http://198.51.100.7:11434"),
            }],
            document: None,
            dry_run: false,
            base_revision: None,
        },
        Command::SetConfig {
            entries: Vec::new(),
            document: Some("[local]\nhost = \"http://198.51.100.7:11434\"\n".into()),
            dry_run: false,
            base_revision: None,
        },
        Command::ListDictionary {
            query: None,
            limit: None,
        },
        Command::ListDictionarySuggestions { limit: None },
        Command::UpsertDictionaryEntry {
            entry: dictate_proto::DictionaryEntry::new("Kubernetes"),
        },
        Command::DeleteDictionaryEntry { id: 1 },
        Command::QueryHistory {
            query: dictate_proto::HistoryQuery::default(),
        },
        Command::GetHistoryAnalytics,
        Command::PurgeHistory,
        Command::Diagnose { quick: true },
    ];
    for command in forbidden {
        let name = command.name();
        expect_code(ws.request(command).await, ErrorCode::Forbidden, name);
    }
    for command in [
        Command::BeginAudioStream {
            format: AudioFormat::whisper_native(),
            options: None,
        },
        Command::EndAudioStream { stream_id: 1 },
        Command::ListSnippets {
            query: None,
            limit: None,
        },
    ] {
        let name = command.name();
        expect_code(
            ws.request(command).await,
            ErrorCode::UnsupportedCommand,
            name,
        );
    }
    // Uploads may not ask to type, nor for a host route.
    expect_code(
        ws.request(Command::TranscribeAudio {
            audio: inline_wav(0.5),
            options: Some(SessionOptions {
                inject: Some(true),
                ..SessionOptions::default()
            }),
        })
        .await,
        ErrorCode::Forbidden,
        "inject",
    );
    assert!(!config_file.exists(), "nothing wrote the config file");
    assert!(h.injector.injected().is_empty());
    drop(local);
    h.stop().await;
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_network_subscriber_never_receives_a_dictation_made_at_the_desk() {
    let mut h = Harness::start().await;
    let api = start_api(&mut h, |_| {}).await;
    let mut phone = Ws::connect(&api).await;
    phone.handshake().await;
    phone
        .request(Command::Subscribe { events: Vec::new() })
        .await
        .unwrap();

    let mut desk = h.client().await;
    desk.subscribe().await;
    desk.request(Command::Toggle).await.unwrap();
    desk.wait_for_state(State::Recording).await;
    desk.request(Command::Toggle).await.unwrap();
    let text = desk.wait_for_final().await;
    assert_eq!(text.text.as_str(), "Hello there", "the desk dictated");

    // Anything the phone was sent arrives before the answer to a request it
    // makes now, or shortly after.
    phone.request(Command::GetStatus).await.unwrap();
    let leaked = phone.drain(Duration::from_millis(300)).await;
    assert!(
        leaked.is_empty(),
        "a network peer received the desk's events: {leaked:?}"
    );
    h.stop().await;
}

#[tokio::test]
async fn a_network_peer_cannot_stop_or_cancel_the_desks_session() {
    let mut h = Harness::start().await;
    let api = start_api(&mut h, |_| {}).await;
    let mut desk = h.client().await;
    desk.subscribe().await;
    desk.request(Command::StartDictation {
        mode: dictate_proto::DictationMode::Toggle,
        options: None,
    })
    .await
    .unwrap();
    desk.wait_for_state(State::Recording).await;

    let mut phone = Ws::connect(&api).await;
    phone.handshake().await;
    for command in [Command::Cancel, Command::Stop] {
        let name = command.name();
        assert!(
            phone.request(command).await.is_err(),
            "{name} on the desk's session must be refused"
        );
    }
    match desk.request(Command::GetStatus).await.unwrap() {
        CommandResult::Status(s) => assert_eq!(s.state, State::Recording, "still recording"),
        other => panic!("expected status, got {other:?}"),
    }
    desk.request(Command::Stop).await.unwrap();
    assert_eq!(desk.wait_for_final().await.text.as_str(), "Hello there");
    h.stop().await;
}

#[tokio::test]
async fn hanging_up_mid_upload_cancels_the_network_session() {
    let mut h = Harness::with(Setup::default().with_stt(Arc::new(
        MockStt::returning("too late").with_delay(Duration::from_secs(3)),
    )))
    .await;
    let api = start_api(&mut h, |_| {}).await;
    let mut desk = h.client().await;
    desk.subscribe().await;

    let body = wav(1.0);
    let mut tcp = TcpStream::connect(api.addr).await.unwrap();
    let head = format!(
        "POST /v1/transcribe HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Type: audio/wav\r\nContent-Length: {}\r\n\r\n",
        api.addr,
        api.token,
        body.len()
    );
    tcp.write_all(head.as_bytes()).await.unwrap();
    tcp.write_all(&body).await.unwrap();
    desk.wait_for_state(State::Transcribing).await;
    drop(tcp);

    let walked = within("the abandoned upload to end", desk.wait_for_terminal()).await;
    assert_eq!(
        walked,
        State::Cancelled,
        "a peer that hung up gets nothing finished for it"
    );
    assert!(h.injector.injected().is_empty());
    h.stop().await;
}

// --- startup -----------------------------------------------------------------------

#[tokio::test]
async fn the_api_refuses_to_start_beyond_loopback_or_without_a_token() {
    let mut h = Harness::start().await;
    let lan = api_config(&h, |c| c.bind = "192.0.2.10:0".into());
    dictate_server::token::ensure(&lan.token_path()).unwrap();
    assert!(matches!(
        h.daemon.start_network_api(&lan).await,
        Err(StartError::Refused(_))
    ));
    let opted_in_plaintext = api_config(&h, |c| {
        c.bind = "192.0.2.10:0".into();
        c.allow_lan = true;
    });
    assert!(matches!(
        h.daemon.start_network_api(&opted_in_plaintext).await,
        Err(StartError::Refused(_))
    ));
    let no_token = api_config(&h, |c| {
        c.token_file = h.dir().join("no-such-token").to_string_lossy().into_owned();
    });
    assert!(matches!(
        h.daemon.start_network_api(&no_token).await,
        Err(StartError::Token(_))
    ));
    assert!(
        h.daemon.network_api_addr().is_none(),
        "nothing is listening"
    );
    // Disabled is not an error and opens nothing. (This test never yields to
    // the daemon's tasks before stopping it, which is what exposed the lost
    // shutdown wakeup fixed in `Daemon::shutdown`.)
    assert_eq!(
        h.daemon
            .start_network_api(&ApiConfig::default())
            .await
            .unwrap(),
        None
    );
    h.stop().await;
}

#[tokio::test]
async fn an_inline_upload_over_the_websocket_honors_the_network_limits() {
    let mut h = Harness::start().await;
    let api = start_api(&mut h, |c| c.max_audio_seconds = 1).await;
    let mut ws = Ws::connect(&api).await;
    ws.handshake().await;
    expect_code(
        ws.request(Command::TranscribeAudio {
            audio: inline_wav(2.0),
            options: None,
        })
        .await,
        ErrorCode::PayloadTooLarge,
        "a 2 s clip over a 1 s limit",
    );
    // The base64 path is the same decoder the socket uses.
    let encoded = base64::engine::general_purpose::STANDARD.encode(wav(0.5));
    let line = serde_json::json!({
        "kind": "request", "v": 1, "id": 77,
        "command": {"type": "transcribe_audio",
                    "audio": {"source": "inline", "format": {"encoding": "wav"}, "data": encoded}}
    })
    .to_string();
    ws.ws.send(WsMessage::Text(line.into())).await.unwrap();
    let reply = within("the inline upload", ws.read()).await.unwrap();
    match reply {
        Message::Response(r) => assert!(r.outcome.is_ok(), "{:?}", r.outcome.error()),
        other => panic!("expected a response, got {other:?}"),
    }
    h.stop().await;
}
