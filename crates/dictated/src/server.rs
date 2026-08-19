//! The unix-socket control plane: NDJSON framing, handshake, dispatch, and
//! event fan-out.
//!
//! # One task per connection, reading and writing in the same select
//!
//! A connection has two things happening to it — requests arriving and events
//! being broadcast — and both write to the same socket. Splitting that into a
//! reader task and a writer task would need a channel and a story about
//! interleaving; instead one task owns both halves and `select!`s over them, so
//! writes are sequential by construction and the ordering guarantee the
//! protocol's reference flow depends on falls out for free:
//!
//! ```text
//! → start_dictation
//! ← response: session_started        (written by the request arm)
//! ← event:    state_changed          (written by the event arm, next loop)
//! ```
//!
//! The state change is *published* while the engine is still handling the
//! request, but it is only *forwarded* after the response has been written,
//! because this task cannot be in two arms at once. A client that assumes a
//! session id arrives before events about that session is correct.
//!
//! # Capabilities are per connection
//!
//! Every connection negotiates its own [`Capabilities`] at handshake and they
//! are never cached across transports. Today every connection is a local unix
//! socket and gets [`Capabilities::local_trusted`] adjusted for what the host
//! can actually do; S33's TCP listener will hand out
//! `remote_transcription_only` from the same dispatch code.

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use dictate_core::engine::resolve_options;
use dictate_core::session::{Actor, ClientId, ClientIdGen};
use dictate_core::EngineHandle;
use dictate_history::HistoryStore;
use dictate_proto::{
    Capabilities, ClientKind, Command, CommandResult, ErrorCode, Event, Features, Message,
    ProtoError, RequestId, ServerHello, ServerInfo,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, error, info, warn};

/// Message ceiling applied before a connection has negotiated its own limits.
const PRE_HANDSHAKE_MAX_BYTES: usize = 64 * 1024;

/// Shared state a connection needs.
pub struct ServerDeps {
    /// The engine every command is routed to.
    pub engine: EngineHandle,
    /// The interaction log, for `query_history`.
    pub history: Arc<Mutex<HistoryStore>>,
    /// Source of per-connection identity.
    pub ids: Arc<ClientIdGen>,
    /// Capabilities handed to a connection on this transport.
    pub capabilities: Capabilities,
}

/// Capabilities for a trusted local connection, adjusted for what this host
/// can actually do.
///
/// Starts from [`Capabilities::local_trusted`] and *removes* what is not true
/// here, never the other way round — the fail-safe direction, matching
/// `Features`' deny-by-default shape. A daemon with no display server
/// advertises `headless` and withdraws `text_injection`, so a client is told
/// up front rather than discovering it from a failed injection.
#[must_use]
pub fn local_capabilities(injection_available: bool) -> Capabilities {
    let mut caps = Capabilities::local_trusted();
    let headless = std::env::var_os("DISPLAY").is_none()
        && std::env::var_os("WAYLAND_DISPLAY").is_none();
    caps.features.headless = headless;
    caps.features.text_injection = injection_available && !headless;
    // Partials are specified but never emitted; advertising them would make a
    // HUD wait for events that are not coming.
    caps.features.partial_transcripts = false;
    // Not implemented in this slice — see `unsupported_command` in `dispatch`.
    caps.features.dictionary_read = false;
    caps.features.dictionary_write = false;
    caps.features.snippets_read = false;
    caps.features.snippets_write = false;
    caps.features.config_read = false;
    caps.features.config_write = false;
    caps.features.wake_word = false;
    // `routes` is populated explicitly: deny-by-default means an omitted list
    // permits nothing at all.
    caps.routes = dictate_core::engine::local_routes();
    caps
}

/// The bound listener.
pub struct Server {
    listener: UnixListener,
    path: std::path::PathBuf,
}

impl Server {
    /// Bind the control socket, clearing a stale one if no daemon answers.
    ///
    /// # Errors
    ///
    /// If another daemon is listening on this path, or the bind fails.
    pub async fn bind(path: &Path) -> Result<Self> {
        if path.exists() {
            // Distinguish "a daemon is running" from "a daemon died without
            // cleaning up". Only the second is safe to clear, and connecting
            // is the only way to tell them apart.
            match UnixStream::connect(path).await {
                Ok(_) => anyhow::bail!(
                    "another daemon is already listening on {}",
                    path.display()
                ),
                Err(_) => {
                    debug!("clearing stale socket at {}", path.display());
                    std::fs::remove_file(path)
                        .with_context(|| format!("removing stale socket {}", path.display()))?;
                }
            }
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let listener = UnixListener::bind(path)
            .with_context(|| format!("binding {}", path.display()))?;
        info!("listening on {}", path.display());
        Ok(Self {
            listener,
            path: path.to_path_buf(),
        })
    }

    /// The socket path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Accept connections until `shutdown` resolves.
    pub async fn serve(self, deps: Arc<ServerDeps>, shutdown: impl std::future::Future<Output = ()>) {
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                () = &mut shutdown => break,
                accepted = self.listener.accept() => match accepted {
                    Ok((stream, _)) => {
                        let deps = deps.clone();
                        let id = deps.ids.next();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream, id, deps).await {
                                debug!(%id, "connection ended: {e}");
                            }
                        });
                    }
                    Err(e) => {
                        error!("accept failed: {e}");
                        break;
                    }
                },
            }
        }
        let _ = std::fs::remove_file(&self.path);
        info!("control socket closed");
    }
}

/// Per-connection negotiated state.
struct Conn {
    id: ClientId,
    /// `None` until the handshake completes. Every other command is refused
    /// with `handshake_required` until then — a client must learn what it is
    /// allowed to do before doing it.
    capabilities: Option<Capabilities>,
    /// `None` when not subscribed; `Some(filter)` when subscribed, where an
    /// empty filter means every event.
    subscription: Option<Vec<String>>,
}

impl Conn {
    fn features(&self) -> Features {
        self.capabilities
            .as_ref()
            .map(|c| c.features.clone())
            .unwrap_or_default()
    }

    fn actor(&self) -> Actor {
        Actor::Connection {
            id: self.id,
            host_capture: self.features().host_capture,
        }
    }

    fn max_message_bytes(&self) -> usize {
        self.capabilities
            .as_ref()
            .map_or(PRE_HANDSHAKE_MAX_BYTES, |c| {
                c.limits.max_message_bytes as usize
            })
    }

    fn wants(&self, event: &Event) -> bool {
        self.subscription
            .as_ref()
            .is_some_and(|filter| event.matches_filter(filter))
    }
}

async fn handle_connection(stream: UnixStream, id: ClientId, deps: Arc<ServerDeps>) -> Result<()> {
    info!(%id, "control connection opened");
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut events = deps.engine.subscribe();
    let mut conn = Conn {
        id,
        capabilities: None,
        subscription: None,
    };

    loop {
        tokio::select! {
            line = read_line_bounded(&mut reader, conn.max_message_bytes()) => {
                match line? {
                    Framed::Eof => break,
                    Framed::TooLarge(limit) => {
                        // Answered rather than silently truncated, then the
                        // connection is closed: the stream is no longer in
                        // sync, since the rest of the oversized line would be
                        // read as a fresh message.
                        let err = ProtoError::new(
                            ErrorCode::PayloadTooLarge,
                            format!("message exceeds the {limit}-byte limit for this connection"),
                        );
                        write(&mut write_half, &Message::event(Event::Error {
                            session_id: None,
                            error: err,
                        })).await?;
                        break;
                    }
                    Framed::Line(line) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        let reply = process(&line, &mut conn, &deps).await;
                        write(&mut write_half, &reply).await?;
                    }
                }
            }
            event = events.recv() => match event {
                Ok(event) => {
                    if conn.wants(&event) {
                        write(&mut write_half, &Message::event(event)).await?;
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    // Documented and tolerated: `audio_level` is decoration.
                    // Logged because losing a `state_changed` is not, and this
                    // is the only place it would show up.
                    warn!(%id, "subscriber lagged; dropped {n} events");
                }
                Err(RecvError::Closed) => break,
            },
        }
    }

    // Tell the engine before returning: a session this connection owns has to
    // be orphaned or cancelled, and nobody else can notice the socket closed.
    deps.engine
        .disconnected(id, conn.features().host_capture)
        .await;
    info!(%id, "control connection closed");
    Ok(())
}

/// One NDJSON frame.
enum Framed {
    Line(String),
    TooLarge(usize),
    Eof,
}

/// Read one newline-delimited message, refusing to buffer more than `limit`.
///
/// The bound is applied *while* reading rather than after, so a peer cannot
/// make the daemon allocate an arbitrary amount of memory by omitting a
/// newline — the same rule the binary frame decoder applies to `payload_len`.
async fn read_line_bounded<R>(reader: &mut R, limit: usize) -> Result<Framed>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let mut buf = Vec::new();
    let read = {
        let mut limited = tokio::io::AsyncReadExt::take(reader, limit as u64 + 1);
        limited.read_until(b'\n', &mut buf).await?
    };
    if read == 0 {
        return Ok(Framed::Eof);
    }
    if buf.len() > limit {
        return Ok(Framed::TooLarge(limit));
    }
    Ok(Framed::Line(String::from_utf8_lossy(&buf).into_owned()))
}

async fn write(sink: &mut (impl AsyncWriteExt + Unpin), message: &Message) -> Result<()> {
    let line = message.to_ndjson_line()?;
    sink.write_all(line.as_bytes()).await?;
    sink.flush().await?;
    Ok(())
}

/// Parse one line and produce the message to send back.
async fn process(line: &str, conn: &mut Conn, deps: &ServerDeps) -> Message {
    let parsed = match Message::parse(line) {
        Ok(m) => m,
        Err(e) => return recover_parse_failure(line, e),
    };

    let request = match parsed {
        Message::Request(r) => r,
        // A server receives requests. A client sending us a response or an
        // event is confused, and has no request id for us to answer against —
        // so this is reported as a connection-level error event.
        other => {
            return Message::event(Event::Error {
                session_id: None,
                error: ProtoError::new(
                    ErrorCode::MalformedRequest,
                    format!(
                        "expected a request; received a {}",
                        match other {
                            Message::Response(_) => "response",
                            Message::Event(_) => "event",
                            Message::Request(_) => unreachable!(),
                        }
                    ),
                ),
            })
        }
    };

    let id = request.id.clone();
    match dispatch(request.command, conn, deps).await {
        Ok(result) => Message::ok(id, result),
        Err(error) => Message::err(id, error),
    }
}

/// Turn an unparseable line into the most useful answer available.
///
/// An unknown command **must** be answered rather than dropped, so the id and
/// the command name are dug out of the raw JSON when the typed parse fails.
/// A caller left waiting forever for an effect that will never happen is the
/// failure mode the protocol's command/event asymmetry exists to prevent.
fn recover_parse_failure(line: &str, error: ProtoError) -> Message {
    let raw: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        // Not even JSON: no id to answer against.
        Err(_) => {
            return Message::event(Event::Error {
                session_id: None,
                error,
            })
        }
    };

    let id = match raw.get("id") {
        Some(serde_json::Value::Number(n)) => n.as_u64().map(RequestId::Number),
        Some(serde_json::Value::String(s)) => Some(RequestId::Text(s.clone())),
        _ => None,
    };

    let error = match raw.pointer("/command/type").and_then(|v| v.as_str()) {
        // `Command` is a closed enum: a name we do not know fails to
        // deserialize, and that is exactly `unsupported_command`.
        Some(name) if error.code == ErrorCode::MalformedRequest => {
            ProtoError::unsupported_command(name)
        }
        _ => error,
    };

    match id {
        Some(id) => Message::err(id, error),
        None => Message::event(Event::Error {
            session_id: None,
            error,
        }),
    }
}

/// Execute one command.
async fn dispatch(
    command: Command,
    conn: &mut Conn,
    deps: &ServerDeps,
) -> Result<CommandResult, ProtoError> {
    // The handshake is the only thing a fresh connection may do. Everything
    // else needs capabilities, and capabilities are what the handshake
    // produces.
    if conn.capabilities.is_none() {
        if let Command::Handshake(hello) = command {
            return handshake(hello, conn, deps);
        }
        return Err(ProtoError::new(
            ErrorCode::HandshakeRequired,
            "send a handshake before any other command",
        ));
    }

    // "This build cannot do that" is checked before "you may not do that".
    //
    // Both are honest refusals, but they mean different things to a client:
    // `forbidden` invites asking for permission, while `unsupported_command`
    // says no amount of permission will help. Since the capability flags for
    // these features are advertised as `false`, the capability gate below
    // would otherwise answer `forbidden` and send S32's UI looking for a
    // setting that does not exist.
    if !is_implemented(&command) {
        return Err(ProtoError::unsupported_command(command.name()));
    }

    // Answer `forbidden` rather than silently doing less than was asked.
    if !command.is_permitted(&conn.features()) {
        return Err(ProtoError::new(
            ErrorCode::Forbidden,
            format!(
                "this connection is not permitted to use '{}'",
                command.name()
            ),
        ));
    }

    let capabilities = conn
        .capabilities
        .clone()
        .expect("capabilities checked above");

    match command {
        Command::Handshake(hello) => handshake(hello, conn, deps),

        Command::StartDictation { mode, options } => {
            let resolved = resolve_options(options.as_ref(), &capabilities)?;
            let session_id = deps
                .engine
                .start(conn.actor(), mode, resolved)
                .await?;
            Ok(CommandResult::SessionStarted { session_id })
        }

        Command::Stop => {
            let session_id = deps.engine.stop(conn.actor()).await?;
            Ok(CommandResult::SessionStopped { session_id })
        }

        Command::Cancel => {
            let session_id = deps.engine.cancel(conn.actor()).await?;
            Ok(CommandResult::SessionCancelled { session_id })
        }

        Command::GetStatus => {
            let status = deps.engine.status(capabilities).await?;
            Ok(CommandResult::Status(status))
        }

        Command::Subscribe { events } => {
            conn.subscription = Some(events);
            Ok(CommandResult::Ack)
        }

        Command::Unsubscribe => {
            conn.subscription = None;
            Ok(CommandResult::Ack)
        }

        Command::QueryHistory { query } => {
            let history = deps.history.clone();
            // SQLite is blocking; a slow query must not stall the runtime.
            let page = tokio::task::spawn_blocking(move || {
                let store = history.lock().map_err(|_| "history store is poisoned")?;
                store.query(&query).map_err(|e| e.to_string())
            })
            .await
            .map_err(|e| ProtoError::new(ErrorCode::Internal, e.to_string()))?
            .map_err(|e| ProtoError::new(ErrorCode::HistoryError, e))?;
            Ok(CommandResult::History(page))
        }

        // Unreachable: `is_implemented` has already refused everything that
        // does not have an arm above. Kept total rather than `unreachable!()`
        // so a command added to the protocol without being wired up here is
        // refused rather than panicking the connection task.
        other => Err(ProtoError::unsupported_command(other.name())),
    }
}

/// Whether this build can actually perform a command, as distinct from whether
/// this connection is allowed to ask for it.
///
/// The unimplemented set is owned by later slices: S22 brings the dictionary,
/// S24 snippets, S33 audio upload and streaming, and config CRUD needs the
/// validation layer that arrives with them. Until then the honest answer is
/// `unsupported_command` — never a stub that returns an empty list, which a
/// client would reasonably read as "you have no dictionary entries".
fn is_implemented(command: &Command) -> bool {
    !matches!(
        command,
        Command::GetConfig { .. }
            | Command::SetConfig { .. }
            | Command::ListDictionary { .. }
            | Command::UpsertDictionaryEntry { .. }
            | Command::DeleteDictionaryEntry { .. }
            | Command::ListSnippets { .. }
            | Command::UpsertSnippet { .. }
            | Command::DeleteSnippet { .. }
            | Command::TranscribeAudio { .. }
            | Command::BeginAudioStream { .. }
            | Command::EndAudioStream { .. }
    )
}

fn handshake(
    hello: dictate_proto::Hello,
    conn: &mut Conn,
    deps: &ServerDeps,
) -> Result<CommandResult, ProtoError> {
    let versions = hello.versions();
    let Some(negotiated) = dictate_proto::negotiate_version(&versions) else {
        // Reported rather than closing the connection, so an older client can
        // tell the user *why* it was refused.
        return Err(ProtoError::new(
            ErrorCode::UnsupportedVersion,
            format!(
                "no common protocol version; client speaks {versions:?}, this daemon speaks {:?}",
                dictate_proto::SUPPORTED_PROTOCOL_VERSIONS
            ),
        ));
    };

    // `client.kind` is a hint for logging. Authorization comes from the
    // transport this connection arrived on, never from what it calls itself:
    // a client claiming `kind: "cli"` over a LAN socket is still remote.
    let kind = hello.client.kind.clone();
    if kind == ClientKind::Remote {
        debug!(%conn.id, "client self-describes as remote on a local socket");
    }
    info!(
        %conn.id,
        client = %hello.client.name,
        version = %hello.client.version.clone().unwrap_or_else(|| "?".into()),
        "handshake"
    );

    conn.capabilities = Some(deps.capabilities.clone());
    Ok(CommandResult::Handshake(Box::new(ServerHello {
        protocol_version: negotiated,
        supported_versions: dictate_proto::SUPPORTED_PROTOCOL_VERSIONS.to_vec(),
        server: ServerInfo::new("dictated", env!("CARGO_PKG_VERSION")),
        capabilities: deps.capabilities.clone(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertised_routes_are_explicit_never_empty() {
        // Deny-by-default: an empty list permits nothing, so a daemon that
        // forgot to populate it would refuse every route it can actually run.
        let caps = local_capabilities(true);
        assert!(!caps.routes.is_empty());
        for route in dictate_proto::Route::known() {
            assert!(caps.allows_route(route), "{} must be allowed", route.as_str());
        }
    }

    #[test]
    fn unimplemented_features_are_advertised_as_absent() {
        let caps = local_capabilities(true);
        assert!(!caps.features.dictionary_read);
        assert!(!caps.features.snippets_read);
        assert!(!caps.features.config_read);
        assert!(
            !caps.features.partial_transcripts,
            "advertising partials would make a HUD wait for events that never come"
        );
        // What this slice *does* implement stays on.
        assert!(caps.features.history_read);
        assert!(caps.features.host_capture);
    }

    #[test]
    fn an_unavailable_injector_withdraws_the_injection_capability() {
        let caps = local_capabilities(false);
        assert!(!caps.features.text_injection);
    }

    #[test]
    fn features_this_build_implements_are_separated_from_features_it_permits() {
        // The distinction a client acts on: `unsupported_command` means no
        // amount of permission will help, `forbidden` means ask for it.
        assert!(!is_implemented(&Command::ListDictionary {
            query: None,
            limit: None
        }));
        assert!(!is_implemented(&Command::GetConfig { path: None }));
        assert!(!is_implemented(&Command::ListSnippets {
            query: None,
            limit: None
        }));
        // Implemented by this slice.
        assert!(is_implemented(&Command::GetStatus));
        assert!(is_implemented(&Command::Stop));
        assert!(is_implemented(&Command::Cancel));
        assert!(is_implemented(&Command::Unsubscribe));
        assert!(is_implemented(&Command::QueryHistory {
            query: dictate_proto::HistoryQuery::default()
        }));
    }

    #[test]
    fn a_conn_without_a_subscription_receives_nothing() {
        let conn = Conn {
            id: ClientId(1),
            capabilities: None,
            subscription: None,
        };
        let event = Event::Error {
            session_id: None,
            error: ProtoError::new(ErrorCode::Internal, "x"),
        };
        assert!(!conn.wants(&event));
    }

    #[test]
    fn an_empty_filter_subscribes_to_everything() {
        let conn = Conn {
            id: ClientId(1),
            capabilities: None,
            subscription: Some(Vec::new()),
        };
        assert!(conn.wants(&Event::Error {
            session_id: None,
            error: ProtoError::new(ErrorCode::Internal, "x"),
        }));
    }

    #[test]
    fn a_named_filter_selects_only_those_events() {
        let conn = Conn {
            id: ClientId(1),
            capabilities: None,
            subscription: Some(vec!["state_changed".into()]),
        };
        assert!(conn.wants(&Event::StateChanged {
            session_id: dictate_proto::SessionId("s".into()),
            from: dictate_proto::State::Idle,
            to: dictate_proto::State::Recording,
            at_ms: None,
        }));
        assert!(!conn.wants(&Event::Error {
            session_id: None,
            error: ProtoError::new(ErrorCode::Internal, "x"),
        }));
    }

    #[test]
    fn an_unknown_command_is_reported_with_its_name_and_id() {
        let line = r#"{"kind":"request","v":1,"id":7,"command":{"type":"teleport"}}"#;
        let err = Message::parse(line).unwrap_err();
        let msg = recover_parse_failure(line, err);
        match msg {
            Message::Response(r) => {
                assert_eq!(r.id, RequestId::Number(7));
                let e = r.outcome.error().expect("an error outcome");
                assert_eq!(e.code, ErrorCode::UnsupportedCommand);
                assert!(
                    e.message.contains("teleport"),
                    "the caller must be told which command was refused: {}",
                    e.message
                );
            }
            other => panic!("expected a response, got {other:?}"),
        }
    }

    #[test]
    fn a_string_request_id_survives_recovery() {
        let line = r#"{"kind":"request","v":1,"id":"abc","command":{"type":"nope"}}"#;
        let err = Message::parse(line).unwrap_err();
        match recover_parse_failure(line, err) {
            Message::Response(r) => assert_eq!(r.id, RequestId::Text("abc".into())),
            other => panic!("expected a response, got {other:?}"),
        }
    }

    #[test]
    fn unparseable_input_degrades_to_a_connection_level_error() {
        let line = "this is not json";
        let err = Message::parse(line).unwrap_err();
        match recover_parse_failure(line, err) {
            Message::Event(e) => match e.event {
                Event::Error { session_id, error } => {
                    assert!(session_id.is_none());
                    assert_eq!(error.code, ErrorCode::MalformedRequest);
                }
                other => panic!("expected an error event, got {other:?}"),
            },
            other => panic!("expected an event, got {other:?}"),
        }
    }

    #[test]
    fn a_version_mismatch_is_not_downgraded_to_unsupported_command() {
        let line = r#"{"kind":"request","v":99,"id":1,"command":{"type":"stop"}}"#;
        let err = Message::parse(line).unwrap_err();
        assert_eq!(err.code, ErrorCode::UnsupportedVersion);
        match recover_parse_failure(line, err) {
            Message::Response(r) => {
                assert_eq!(
                    r.outcome.error().unwrap().code,
                    ErrorCode::UnsupportedVersion
                );
            }
            other => panic!("expected a response, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn read_line_bounded_returns_whole_lines_then_eof() {
        let input = b"{\"a\":1}\n{\"b\":2}\n".to_vec();
        let mut reader = BufReader::new(std::io::Cursor::new(input));
        match read_line_bounded(&mut reader, 1024).await.unwrap() {
            Framed::Line(l) => assert_eq!(l.trim(), r#"{"a":1}"#),
            _ => panic!("expected a line"),
        }
        match read_line_bounded(&mut reader, 1024).await.unwrap() {
            Framed::Line(l) => assert_eq!(l.trim(), r#"{"b":2}"#),
            _ => panic!("expected a line"),
        }
        assert!(matches!(
            read_line_bounded(&mut reader, 1024).await.unwrap(),
            Framed::Eof
        ));
    }

    #[tokio::test]
    async fn an_oversized_line_is_refused_before_it_is_buffered() {
        let huge = format!("{}\n", "x".repeat(4096));
        let mut reader = BufReader::new(std::io::Cursor::new(huge.into_bytes()));
        match read_line_bounded(&mut reader, 64).await.unwrap() {
            Framed::TooLarge(limit) => assert_eq!(limit, 64),
            _ => panic!("expected the read to be refused"),
        }
    }

    #[tokio::test]
    async fn a_line_exactly_at_the_limit_is_accepted() {
        let line = format!("{}\n", "x".repeat(63));
        let mut reader = BufReader::new(std::io::Cursor::new(line.into_bytes()));
        assert!(matches!(
            read_line_bounded(&mut reader, 64).await.unwrap(),
            Framed::Line(_)
        ));
    }
}
