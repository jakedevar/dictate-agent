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
//! are never cached across transports. A unix-socket connection gets
//! [`Capabilities::local_trusted`] adjusted for what the host can actually do;
//! a network connection (S33, [`NetworkConnection`]) is offered the network
//! grant instead, through this same dispatch code.
//!
//! # Network connections (S33)
//!
//! `dictate-server` owns the TCP transport; it executes every command through
//! [`NetworkConnection`], which is a [`Conn`] driven by the same `process` and
//! `dispatch` the socket uses. Three things differ, all decided here:
//!
//! - the handshake offers the network grant, and the connection is never the
//!   owner, so [`restrict_to_owner`] withdraws config authority a second time;
//! - events are forwarded only for sessions the connection started — a phone
//!   must never receive the text of a dictation made at the desk;
//! - `get_status` drops host detail (pid, audio device, formatter host).
//!
//! `raw_text` is redacted by capability on both transports.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use dictate_core::engine::{resolve_options, resolve_upload_options};
use dictate_core::ports::BoxFuture;
use dictate_core::session::{Actor, ClientId, ClientIdGen};
use dictate_core::upload::decode_upload;
use dictate_core::EngineHandle;
use dictate_history::HistoryStore;
use dictate_proto::{
    Capabilities, ClientKind, Command, CommandResult, DiagnosticsReport, ErrorCode, Event,
    Features, Message, ProtoError, RequestId, ServerHello, ServerInfo, SessionId, State, Status,
    Transcript,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, error, info, warn};

/// Message ceiling applied before a connection has negotiated its own limits.
const PRE_HANDSHAKE_MAX_BYTES: usize = 64 * 1024;

/// Sessions a network connection remembers starting, for event scoping.
const OWNED_SESSIONS: usize = 16;

/// Answers `diagnose`. A trait so the server does not depend on the daemon's
/// configuration, and so tests can substitute a fixed report.
pub trait DiagnosticsProvider: Send + Sync + 'static {
    /// Run the checks. `quick` skips the slow ones (hashing the model file).
    fn diagnose(&self, quick: bool) -> BoxFuture<'_, DiagnosticsReport>;
}

/// Shared state a connection needs.
pub struct ServerDeps {
    /// The engine every command is routed to.
    pub engine: EngineHandle,
    /// The interaction log, for `query_history`.
    pub history: Arc<Mutex<HistoryStore>>,
    /// Shared with the transcription pipeline; edits publish a new snapshot.
    pub dictionary: Option<Arc<dictate_dict::Dictionary>>,
    /// Source of per-connection identity.
    pub ids: Arc<ClientIdGen>,
    /// Capabilities handed to a connection on this transport.
    pub capabilities: Capabilities,
    /// The `diagnose` implementation, when this daemon has one.
    pub diagnostics: Option<Arc<dyn DiagnosticsProvider>>,
    /// The `get_config` / `set_config` implementation, when this daemon has
    /// one.
    pub config: Option<Arc<crate::config_rpc::ConfigService>>,
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
    let headless =
        std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none();
    caps.features.headless = headless;
    caps.features.text_injection = injection_available && !headless;
    // Partials are specified but never emitted; advertising them would make a
    // HUD wait for events that are not coming.
    caps.features.partial_transcripts = false;
    // Not implemented in this slice — see `unsupported_command` in `dispatch`.
    caps.features.dictionary_read = true;
    caps.features.dictionary_write = true;
    caps.features.snippets_read = false;
    caps.features.snippets_write = false;
    // S32. Local only: `remote_transcription_only` never grants these, and
    // `Daemon::start_with` withdraws them when no config service is wired.
    caps.features.config_read = true;
    caps.features.config_write = true;
    caps.features.wake_word = false;
    // `routes` is populated explicitly: deny-by-default means an omitted list
    // permits nothing at all.
    caps.routes = dictate_core::engine::local_routes();
    // Upload limits: the defaults here match `[upload]` in the config; the
    // daemon overrides them from the file it actually loaded.
    caps.limits = dictate_core::config::UploadConfig::default().limits();
    caps
}

/// Whether the process on the other end of `stream` runs as the same effective
/// user as this daemon (`SO_PEERCRED`). Fails closed: an unreadable credential
/// is "not the owner".
fn peer_is_owner(stream: &UnixStream) -> bool {
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let own = unsafe { libc::geteuid() };
    is_owner(stream.peer_cred().ok().map(|c| c.uid()), own)
}

fn is_owner(peer_uid: Option<u32>, own_uid: u32) -> bool {
    peer_uid == Some(own_uid)
}

/// Withdraw what only the owning user may do. Config is the sensitive part:
/// `set_config` rewrites a file the daemon trusts, so a connection that is not
/// provably this user's (the socket mode and directory already keep others
/// out; this is the second lock) neither reads nor writes it.
fn restrict_to_owner(mut caps: Capabilities, owner: bool) -> Capabilities {
    if !owner {
        caps.features.config_read = false;
        caps.features.config_write = false;
    }
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
                Ok(_) => anyhow::bail!("another daemon is already listening on {}", path.display()),
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
        let listener =
            UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
        // Owner-only, whatever the umask said: a second layer under the
        // private runtime directory (`RuntimePaths::ensure_dirs`).
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .with_context(|| format!("restricting {}", path.display()))?;
        }
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
    pub async fn serve(
        self,
        deps: Arc<ServerDeps>,
        shutdown: impl std::future::Future<Output = ()>,
    ) {
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
    /// The peer's effective uid matches the daemon's (`SO_PEERCRED`). Always
    /// `false` for a network connection.
    owner: bool,
    /// `None` when not subscribed; `Some(filter)` when subscribed, where an
    /// empty filter means every event.
    subscription: Option<Vec<String>>,
    /// Arrived over the network API (S33): events are scoped to sessions this
    /// connection started, and `get_status` is scrubbed of host detail.
    network: bool,
    /// Capabilities offered at handshake. `None` is the socket's default
    /// (`ServerDeps::capabilities`); a network connection carries its grant.
    offered: Option<Capabilities>,
    /// Sessions this connection started, oldest first, at most
    /// [`OWNED_SESSIONS`].
    owned: VecDeque<SessionId>,
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
        if matches!(event, Event::ContextResolved { .. }) && !self.features().context_read {
            return false;
        }
        // A network peer sees its own sessions and nothing else — not the
        // desk's dictations, and not session-less bus errors.
        if self.network && !event.session_id().is_some_and(|id| self.owned.contains(id)) {
            return false;
        }
        self.subscription
            .as_ref()
            .is_some_and(|filter| event.matches_filter(filter))
    }

    /// The event as this connection may see it, or `None` if it may not.
    fn visible(&self, mut event: Event) -> Option<Event> {
        if !self.wants(&event) {
            return None;
        }
        if let Event::Final { transcript, .. } = &mut event {
            redact_transcript(transcript, &self.features());
        }
        Some(event)
    }

    /// Remember a session this connection started.
    fn record_owned(&mut self, session_id: SessionId) {
        if self.owned.len() == OWNED_SESSIONS {
            self.owned.pop_front();
        }
        self.owned.push_back(session_id);
    }
}

/// Strip what the connection may not see from a transcript. `raw_text` is the
/// recognizer's unformatted output — the most sensitive field a transcript
/// carries — and is shown only to connections holding `Features::raw_text`.
fn redact_transcript(transcript: &mut Transcript, features: &Features) {
    if !features.raw_text {
        transcript.raw_text = None;
    }
}

/// Drop host detail a network peer has no use for: the pid (for signalling a
/// local process), the audio device, the formatter's host and model, and any
/// session it did not start. `state` stays: a peer needs it to avoid `busy`.
fn scrub_for_network(status: &mut Status, owned: &VecDeque<SessionId>) {
    status.daemon.pid = None;
    status.audio = None;
    status.formatter = None;
    if status
        .session
        .as_ref()
        .is_some_and(|s| !owned.contains(&s.session_id))
    {
        status.session = None;
    }
}

async fn handle_connection(stream: UnixStream, id: ClientId, deps: Arc<ServerDeps>) -> Result<()> {
    let owner = peer_is_owner(&stream);
    if !owner {
        warn!(%id, "control connection from a different user (or unreadable credentials)");
    }
    info!(%id, "control connection opened");
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = ConnReader::new(read_half);
    let mut events = deps.engine.subscribe();
    let mut conn = Conn {
        id,
        capabilities: None,
        owner,
        subscription: None,
        network: false,
        offered: None,
        owned: VecDeque::new(),
    };

    let result = connection_loop(&mut reader, &mut write_half, &mut events, &mut conn, &deps).await;

    // Tell the engine before returning — on *every* exit path, including a
    // failed write to a peer that has already gone. A session this connection
    // owns has to be orphaned or cancelled, and nobody else can notice the
    // socket closed.
    deps.engine
        .disconnected(id, conn.features().host_capture)
        .await;
    info!(%id, "control connection closed");
    result
}

async fn connection_loop(
    reader: &mut ConnReader,
    write_half: &mut tokio::net::unix::OwnedWriteHalf,
    events: &mut tokio::sync::broadcast::Receiver<Event>,
    conn: &mut Conn,
    deps: &Arc<ServerDeps>,
) -> Result<()> {
    let id = conn.id;
    loop {
        tokio::select! {
            line = read_line_bounded(reader, conn.max_message_bytes()) => {
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
                        write(write_half, &Message::event(Event::Error {
                            session_id: None,
                            error: err,
                        })).await?;
                        break;
                    }
                    Framed::Line(line) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        // While a request is being answered, watch for the
                        // peer hanging up: a long request (an upload) whose
                        // caller has gone away must be cancelled, not finished.
                        let limit = conn.max_message_bytes();
                        let mut hangup: Hangup<'_> = Box::pin(watch_for_hangup(reader, limit));
                        let reply = process(&line, conn, deps, &mut hangup).await;
                        drop(hangup);
                        write(write_half, &reply).await?;
                    }
                }
            }
            event = events.recv() => match event {
                Ok(event) => {
                    if let Some(event) = conn.visible(event) {
                        write(write_half, &Message::event(event)).await?;
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
    Ok(())
}

/// Resolves when the peer closes its end of the connection; pending forever
/// while it is merely idle.
type Hangup<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>;

/// How often a connection whose read-ahead is full is checked for a hang-up.
const HANGUP_POLL: std::time::Duration = std::time::Duration::from_millis(20);

/// Watch for the peer hanging up while a request is being answered.
///
/// A client may pipeline more requests behind a long one. Those bytes cannot
/// simply be left unread — the end of the stream sits behind them, so a
/// hang-up would go unnoticed and an abandoned upload would still be typed
/// into a window. So they are read ahead into the connection's carry buffer
/// (kept, in order, for the next frame) until either the stream ends — a
/// hang-up — or `limit` bytes are held. Past that bound nothing more is read;
/// the socket's read-closed readiness (`EPOLLRDHUP`) still reports the close.
async fn watch_for_hangup(reader: &mut ConnReader, limit: usize) {
    use tokio::io::AsyncReadExt;
    reader.absorb_buffered();
    let mut chunk = [0u8; 8 * 1024];
    while reader.carried() < limit {
        let room = (limit - reader.carried()).min(chunk.len());
        // `read` is cancel-safe: if the request finishes first and this future
        // is dropped, no byte has been taken from the socket and lost.
        match reader.inner.get_mut().read(&mut chunk[..room]).await {
            // EOF or a socket error: either way, nobody is waiting for an answer.
            Ok(0) | Err(_) => return,
            Ok(n) => reader.carry(&chunk[..n]),
        }
    }
    loop {
        match reader
            .inner
            .get_ref()
            .ready(tokio::io::Interest::READABLE)
            .await
        {
            Ok(ready) if ready.is_read_closed() => return,
            Ok(_) => tokio::time::sleep(HANGUP_POLL).await,
            Err(_) => return,
        }
    }
}

/// A connection's read side: the socket's buffered reader, preceded by any
/// bytes [`watch_for_hangup`] read ahead while a request was in flight.
///
/// Invariant: while `carry` holds unread bytes, `inner`'s own buffer is empty
/// — the read-ahead absorbs it first — so bytes are always delivered in the
/// order they arrived.
struct ConnReader {
    inner: BufReader<tokio::net::unix::OwnedReadHalf>,
    carry: Vec<u8>,
    pos: usize,
}

impl ConnReader {
    fn new(read_half: tokio::net::unix::OwnedReadHalf) -> Self {
        Self {
            inner: BufReader::new(read_half),
            carry: Vec::new(),
            pos: 0,
        }
    }

    /// Unread bytes held in the carry buffer.
    fn carried(&self) -> usize {
        self.carry.len() - self.pos
    }

    /// Append read-ahead bytes, compacting what was already consumed.
    fn carry(&mut self, bytes: &[u8]) {
        if self.pos > 0 {
            self.carry.drain(..self.pos);
            self.pos = 0;
        }
        self.carry.extend_from_slice(bytes);
    }

    /// Move the buffered reader's unread bytes to the carry buffer, so bytes
    /// read from the socket afterwards land behind them.
    fn absorb_buffered(&mut self) {
        let buffered = self.inner.buffer().to_vec();
        if !buffered.is_empty() {
            self.inner.consume(buffered.len());
            self.carry(&buffered);
        }
    }
}

impl tokio::io::AsyncRead for ConnReader {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.carried() > 0 {
            let n = this.carried().min(buf.remaining());
            buf.put_slice(&this.carry[this.pos..this.pos + n]);
            tokio::io::AsyncBufRead::consume(std::pin::Pin::new(this), n);
            return std::task::Poll::Ready(Ok(()));
        }
        std::pin::Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

impl tokio::io::AsyncBufRead for ConnReader {
    fn poll_fill_buf(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<&[u8]>> {
        let this = self.get_mut();
        if this.pos < this.carry.len() {
            return std::task::Poll::Ready(Ok(&this.carry[this.pos..]));
        }
        std::pin::Pin::new(&mut this.inner).poll_fill_buf(cx)
    }

    fn consume(self: std::pin::Pin<&mut Self>, amt: usize) {
        let this = self.get_mut();
        if this.pos < this.carry.len() {
            this.pos = (this.pos + amt).min(this.carry.len());
            if this.pos == this.carry.len() {
                this.carry.clear();
                this.pos = 0;
            }
        } else {
            std::pin::Pin::new(&mut this.inner).consume(amt);
        }
    }
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
async fn process(
    line: &str,
    conn: &mut Conn,
    deps: &ServerDeps,
    hangup: &mut Hangup<'_>,
) -> Message {
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
    match dispatch(request.command, conn, deps, hangup).await {
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
    hangup: &mut Hangup<'_>,
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
            let session_id = deps.engine.start(conn.actor(), mode, resolved).await?;
            Ok(CommandResult::SessionStarted { session_id })
        }

        Command::Toggle => {
            let outcome = deps
                .engine
                .toggle(conn.actor(), resolve_options(None, &capabilities)?)
                .await?;
            Ok(match outcome {
                dictate_core::ToggleOutcome::Started(session_id) => {
                    CommandResult::SessionStarted { session_id }
                }
                dictate_core::ToggleOutcome::Stopped(session_id) => {
                    CommandResult::SessionStopped { session_id }
                }
            })
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
            let mut capabilities = capabilities;
            // Privacy mode can be switched live through `set_config`; report
            // what is in force, not what was true at handshake.
            if let Some(on) = deps.config.as_ref().and_then(|c| c.running_privacy_mode()) {
                capabilities.features.privacy_mode = on;
            }
            let mut status = deps.engine.status(capabilities).await?;
            if conn.network {
                scrub_for_network(&mut status, &conn.owned);
            }
            Ok(CommandResult::Status(status))
        }

        Command::GetConfig { path } => {
            let service = config_service(deps)?;
            let snapshot = tokio::task::spawn_blocking(move || service.read(path.as_deref()))
                .await
                .map_err(|e| ProtoError::new(ErrorCode::Internal, e.to_string()))??;
            Ok(CommandResult::Config(Box::new(snapshot)))
        }

        Command::SetConfig {
            entries,
            document,
            dry_run,
            base_revision,
        } => {
            let service = config_service(deps)?;
            let snapshot = tokio::task::spawn_blocking(move || {
                service.write(entries, document, dry_run, base_revision.as_deref())
            })
            .await
            .map_err(|e| ProtoError::new(ErrorCode::Internal, e.to_string()))??;
            Ok(CommandResult::Config(Box::new(snapshot)))
        }

        Command::GetContext => Ok(CommandResult::Context(Box::new(
            deps.engine.get_context().await?,
        ))),

        Command::Subscribe { events } => {
            conn.subscription = Some(events);
            Ok(CommandResult::Ack)
        }

        Command::Unsubscribe => {
            conn.subscription = None;
            Ok(CommandResult::Ack)
        }

        // S22: the generic permission gate above enforces per-connection read
        // and write capabilities before any store access or history mining.
        Command::ListDictionary { query, limit } => {
            let dictionary = dictionary(deps)?;
            Ok(CommandResult::Dictionary {
                entries: dictionary.list(query.as_deref(), limit),
            })
        }
        Command::UpsertDictionaryEntry { entry } => {
            let dictionary = dictionary(deps)?.clone();
            let entry = tokio::task::spawn_blocking(move || dictionary.upsert(entry))
                .await
                .map_err(|e| ProtoError::new(ErrorCode::Internal, e.to_string()))??;
            Ok(CommandResult::DictionaryEntry { entry })
        }
        Command::DeleteDictionaryEntry { id } => {
            let dictionary = dictionary(deps)?.clone();
            tokio::task::spawn_blocking(move || dictionary.delete(id))
                .await
                .map_err(|e| ProtoError::new(ErrorCode::Internal, e.to_string()))??;
            Ok(CommandResult::Deleted { id })
        }
        Command::ListDictionarySuggestions { limit } => {
            let known = dictionary(deps)?.list(None, None);
            let history = deps.history.clone();
            let suggestions = tokio::task::spawn_blocking(move || {
                let store = history
                    .lock()
                    .map_err(|_| "history store is poisoned".to_string())?;
                dictate_dict::suggestions::mine(store.connection(), &known)
                    .map_err(|e| e.to_string())
            })
            .await
            .map_err(|e| ProtoError::new(ErrorCode::Internal, e.to_string()))?
            .map_err(|e| ProtoError::new(ErrorCode::HistoryError, e))?;
            Ok(CommandResult::DictionarySuggestions {
                suggestions: suggestions
                    .into_iter()
                    .take(limit.unwrap_or(500).min(500) as usize)
                    .collect(),
            })
        }

        Command::TranscribeAudio { audio, options } => {
            transcribe_audio(audio, options, conn, &capabilities, deps, hangup).await
        }

        Command::Diagnose { quick } => match &deps.diagnostics {
            Some(provider) => Ok(CommandResult::Diagnostics(Box::new(
                provider.diagnose(quick).await,
            ))),
            None => Err(ProtoError::capability_unavailable(
                "diagnostics are not configured on this daemon",
            )),
        },

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

        Command::GetHistoryAnalytics => {
            let history = deps.history.clone();
            let analytics = tokio::task::spawn_blocking(move || {
                let store = history.lock().map_err(|_| "history store is poisoned")?;
                store.analytics().map_err(|e| e.to_string())
            })
            .await
            .map_err(|e| ProtoError::new(ErrorCode::Internal, e.to_string()))?
            .map_err(|e| ProtoError::new(ErrorCode::HistoryError, e))?;
            Ok(CommandResult::HistoryAnalytics(analytics))
        }

        Command::PurgeHistory => {
            let history = deps.history.clone();
            tokio::task::spawn_blocking(move || {
                let store = history.lock().map_err(|_| "history store is poisoned")?;
                store.purge().map_err(|e| e.to_string())
            })
            .await
            .map_err(|e| ProtoError::new(ErrorCode::Internal, e.to_string()))?
            .map_err(|e| ProtoError::new(ErrorCode::HistoryError, e))?;
            Ok(CommandResult::Ack)
        }

        // Unreachable: `is_implemented` has already refused everything that
        // does not have an arm above. Kept total rather than `unreachable!()`
        // so a command added to the protocol without being wired up here is
        // refused rather than panicking the connection task.
        other => Err(ProtoError::unsupported_command(other.name())),
    }
}

/// Run an uploaded clip through the pipeline and answer with its transcript.
///
/// The order is deliberate: cheap refusals first (capabilities, options), then
/// the CPU-bound decode on the blocking pool — *before* the engine's slot is
/// touched, so a malformed or oversized upload is a request error and never
/// makes a good dictation answer `busy` — then the session itself.
async fn transcribe_audio(
    audio: dictate_proto::AudioSource,
    options: Option<dictate_proto::SessionOptions>,
    conn: &mut Conn,
    capabilities: &Capabilities,
    deps: &ServerDeps,
    hangup: &mut Hangup<'_>,
) -> Result<CommandResult, ProtoError> {
    let resolved = resolve_upload_options(options.as_ref(), capabilities)?;

    let limits = capabilities.limits.clone();
    let supplied = tokio::task::spawn_blocking(move || decode_upload(&audio, &limits))
        .await
        .map_err(|e| ProtoError::new(ErrorCode::Internal, format!("decoder failed: {e}")))??;

    let ticket = deps
        .engine
        .transcribe(conn.actor(), supplied, resolved)
        .await?;
    // Recorded before the outcome is awaited, so the session's events — which
    // are forwarded only after this request is answered — are recognized as
    // this connection's.
    conn.record_owned(ticket.session_id.clone());

    let outcome = tokio::select! {
        biased;
        outcome = ticket.outcome => outcome.map_err(|_| {
            ProtoError::new(ErrorCode::Internal, "the session ended without an outcome")
        })?,
        () = hangup => {
            // The caller is gone. Cancel rather than finish: a transcript
            // nobody will read must not be typed into a window, and the slot
            // should free for the next request.
            debug!(session = %ticket.session_id.as_str(), "uploader hung up; cancelling");
            let _ = deps.engine.cancel(conn.actor()).await;
            return Err(ProtoError::new(ErrorCode::Cancelled, "the client disconnected"));
        }
    };

    match (outcome.state, outcome.transcript, outcome.error) {
        (State::Done, Some(mut transcript), _) => {
            redact_transcript(&mut transcript, &capabilities.features);
            Ok(CommandResult::Transcript(Box::new(transcript)))
        }
        (_, _, Some(error)) => Err(error),
        (State::Cancelled, ..) => Err(ProtoError::new(
            ErrorCode::Cancelled,
            "the transcription was cancelled",
        )),
        (state, ..) => Err(ProtoError::new(
            ErrorCode::Internal,
            format!("the session ended {} without a transcript", state.as_str()),
        )),
    }
}

/// Whether this build can actually perform a command, as distinct from whether
/// this connection is allowed to ask for it.
///
/// The unimplemented set is owned by later slices: S24 brings snippets and S33
/// audio streaming (S32 implemented config). Until then the honest answer is
/// `unsupported_command` — never a stub that returns an empty list, which a
/// client would reasonably read as "you have no dictionary entries".
fn is_implemented(command: &Command) -> bool {
    !matches!(
        command,
        Command::ListSnippets { .. }
            | Command::UpsertSnippet { .. }
            | Command::DeleteSnippet { .. }
            | Command::BeginAudioStream { .. }
            | Command::EndAudioStream { .. }
    )
}

fn config_service(deps: &ServerDeps) -> Result<Arc<crate::config_rpc::ConfigService>, ProtoError> {
    deps.config.clone().ok_or_else(|| {
        ProtoError::capability_unavailable("configuration is not managed by this daemon")
    })
}

fn dictionary(deps: &ServerDeps) -> Result<&Arc<dictate_dict::Dictionary>, ProtoError> {
    deps.dictionary.as_ref().ok_or_else(|| {
        ProtoError::new(
            ErrorCode::CapabilityUnavailable,
            "dictionary store is unavailable",
        )
    })
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
    if kind == ClientKind::Remote && !conn.network {
        debug!(%conn.id, "client self-describes as remote on a local socket");
    }
    info!(
        %conn.id,
        client = %hello.client.name,
        version = %hello.client.version.clone().unwrap_or_else(|| "?".into()),
        transport = if conn.network { "network" } else { "socket" },
        "handshake"
    );

    let offered = conn
        .offered
        .clone()
        .unwrap_or_else(|| deps.capabilities.clone());
    let granted = restrict_to_owner(offered, conn.owner);
    conn.capabilities = Some(granted.clone());
    Ok(CommandResult::Handshake(Box::new(ServerHello {
        protocol_version: negotiated,
        supported_versions: dictate_proto::SUPPORTED_PROTOCOL_VERSIONS.to_vec(),
        server: ServerInfo::new("dictated", env!("CARGO_PKG_VERSION")),
        capabilities: granted,
    })))
}

/// A control connection that arrived over the network API (S33) instead of the
/// unix socket.
///
/// It is a [`Conn`] driven by the socket's own [`process`] and [`dispatch`]:
/// same parser, same handshake, same `is_implemented` / `is_permitted` gates,
/// same option resolution, upload decoding and session ownership. What makes
/// it a *network* connection is decided at [`NetworkConnection::open`]: the
/// grant it is offered, `owner = false`, and session-scoped events.
///
/// Dropping it is the disconnect: the engine is told, and an upload this
/// connection still owns is cancelled — the same rule as a socket peer hanging
/// up.
pub struct NetworkConnection {
    conn: Conn,
    deps: Arc<ServerDeps>,
    events: tokio::sync::broadcast::Receiver<Event>,
}

impl NetworkConnection {
    /// A fresh, not-yet-handshaken connection offered `grant`.
    ///
    /// The id comes from the daemon's one [`ClientIdGen`], so a network peer
    /// can never be mistaken for a socket peer's session owner.
    #[must_use]
    pub fn open(deps: Arc<ServerDeps>, grant: Capabilities) -> Self {
        let id = deps.ids.next();
        info!(%id, "network control connection opened");
        let events = deps.engine.subscribe();
        Self {
            conn: Conn {
                id,
                capabilities: None,
                owner: false,
                subscription: None,
                network: true,
                offered: Some(grant),
                owned: VecDeque::new(),
            },
            deps,
            events,
        }
    }

    /// Answer one envelope given as text, exactly as the socket answers one
    /// line. `hangup` resolves if the peer goes away mid-request.
    pub async fn handle_line(
        &mut self,
        line: &str,
        hangup: impl std::future::Future<Output = ()> + Send,
    ) -> Message {
        let mut hangup: Hangup<'_> = Box::pin(hangup);
        process(line, &mut self.conn, &self.deps, &mut hangup).await
    }

    /// Execute one typed command.
    ///
    /// # Errors
    ///
    /// Whatever the socket would answer: `handshake_required`, `forbidden`,
    /// `unsupported_command`, or the command's own failure.
    pub async fn execute(
        &mut self,
        command: Command,
        hangup: impl std::future::Future<Output = ()> + Send,
    ) -> Result<CommandResult, ProtoError> {
        let mut hangup: Hangup<'_> = Box::pin(hangup);
        dispatch(command, &mut self.conn, &self.deps, &mut hangup).await
    }

    /// The next event this connection may see: subscribed, about one of its
    /// own sessions, and redacted to its capabilities. `None` once the bus
    /// closes. Cancel-safe.
    pub async fn next_event(&mut self) -> Option<Event> {
        loop {
            match self.events.recv().await {
                Ok(event) => {
                    if let Some(event) = self.conn.visible(event) {
                        return Some(event);
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    warn!(id = %self.conn.id, "network subscriber lagged; dropped {n} events");
                }
                Err(RecvError::Closed) => return None,
            }
        }
    }
}

impl Drop for NetworkConnection {
    fn drop(&mut self) {
        let engine = self.deps.engine.clone();
        let id = self.conn.id;
        // A network connection never has `host_capture`, so the engine
        // cancels (rather than orphans) anything it owns.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move { engine.disconnected(id, false).await });
        }
        info!(%id, "network control connection closed");
    }
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
            assert!(
                caps.allows_route(route),
                "{} must be allowed",
                route.as_str()
            );
        }
    }

    #[test]
    fn unimplemented_features_are_advertised_as_absent() {
        let caps = local_capabilities(true);
        assert!(caps.features.dictionary_read);
        assert!(caps.features.dictionary_write);
        assert!(!caps.features.snippets_read);
        // S32 implements configuration over the socket, for local peers.
        assert!(caps.features.config_read);
        assert!(caps.features.config_write);
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
        assert!(is_implemented(&Command::ListDictionary {
            query: None,
            limit: None
        }));
        assert!(is_implemented(&Command::GetConfig { path: None }));
        assert!(!is_implemented(&Command::ListSnippets {
            query: None,
            limit: None
        }));
        // Implemented by this slice.
        assert!(is_implemented(&Command::GetStatus));
        assert!(is_implemented(&Command::Toggle));
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
            owner: true,
            subscription: None,
            network: false,
            offered: None,
            owned: VecDeque::new(),
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
            owner: true,
            subscription: Some(Vec::new()),
            network: false,
            offered: None,
            owned: VecDeque::new(),
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
            owner: true,
            subscription: Some(vec!["state_changed".into()]),
            network: false,
            offered: None,
            owned: VecDeque::new(),
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

    #[test]
    fn only_the_owning_uid_keeps_config_authority() {
        let caps = local_capabilities(true);
        let own = restrict_to_owner(caps.clone(), is_owner(Some(1000), 1000));
        assert!(own.features.config_read && own.features.config_write);

        for peer in [Some(1001), Some(0), None] {
            let other = restrict_to_owner(caps.clone(), is_owner(peer, 1000));
            assert!(!other.features.config_read, "peer {peer:?}");
            assert!(!other.features.config_write, "peer {peer:?}");
            // Everything else is untouched: only config is owner-gated here.
            assert_eq!(
                other.features.dictionary_read,
                caps.features.dictionary_read
            );
        }
    }

    #[tokio::test]
    async fn a_unix_peer_of_this_process_is_the_owner() {
        let (a, b) = UnixStream::pair().unwrap();
        assert!(peer_is_owner(&a));
        assert!(peer_is_owner(&b));
    }

    #[tokio::test]
    async fn the_bound_socket_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        // Short on purpose: a socket path is limited to ~100 bytes and the
        // sandbox's TMPDIR is long.
        let dir = std::path::PathBuf::from(format!("/tmp/ds-mode-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("dictated.sock");
        let server = Server::bind(&sock).await.unwrap();
        let mode = std::fs::metadata(server.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn network_conn(owned: &[&str]) -> Conn {
        Conn {
            id: ClientId(9),
            capabilities: Some(Capabilities::remote_transcription_only()),
            owner: false,
            subscription: Some(Vec::new()),
            network: true,
            offered: None,
            owned: owned.iter().map(|s| SessionId((*s).into())).collect(),
        }
    }

    fn final_event(session: &str) -> Event {
        let mut transcript = Transcript::delivered("Hello there.");
        transcript.raw_text = Some("hello there".into());
        Event::Final {
            session_id: SessionId(session.into()),
            transcript: Box::new(transcript),
        }
    }

    #[test]
    fn a_network_subscriber_sees_only_its_own_sessions() {
        let conn = network_conn(&["mine"]);
        assert!(conn.visible(final_event("mine")).is_some());
        assert!(
            conn.visible(final_event("the-desk")).is_none(),
            "a phone must never receive a dictation made at the desk"
        );
        // Session-less bus errors belong to no one it owns.
        assert!(conn
            .visible(Event::Error {
                session_id: None,
                error: ProtoError::new(ErrorCode::Internal, "x"),
            })
            .is_none());
        // A socket subscriber is unchanged: it sees everything it asked for.
        let local = Conn {
            network: false,
            capabilities: Some(Capabilities::local_trusted()),
            ..network_conn(&[])
        };
        assert!(local.visible(final_event("the-desk")).is_some());
    }

    #[test]
    fn raw_text_follows_the_capability_on_every_transport() {
        let conn = network_conn(&["mine"]);
        match conn.visible(final_event("mine")) {
            Some(Event::Final { transcript, .. }) => {
                assert_eq!(transcript.raw_text, None);
                assert_eq!(transcript.text.as_str(), "Hello there.");
            }
            other => panic!("expected a final event, got {other:?}"),
        }
        let mut granted = network_conn(&["mine"]);
        granted.capabilities.as_mut().unwrap().features.raw_text = true;
        match granted.visible(final_event("mine")) {
            Some(Event::Final { transcript, .. }) => {
                assert_eq!(transcript.raw_text.as_deref(), Some("hello there"));
            }
            other => panic!("expected a final event, got {other:?}"),
        }
        // Local connections hold the flag: unchanged behavior.
        assert!(local_capabilities(true).features.raw_text);
    }

    #[test]
    fn owned_sessions_are_bounded() {
        let mut conn = network_conn(&[]);
        for i in 0..(OWNED_SESSIONS + 5) {
            conn.record_owned(SessionId(format!("s{i}")));
        }
        assert_eq!(conn.owned.len(), OWNED_SESSIONS);
        assert!(conn
            .owned
            .contains(&SessionId(format!("s{}", OWNED_SESSIONS + 4))));
        assert!(!conn.owned.contains(&SessionId("s0".into())));
    }

    #[test]
    fn network_status_drops_host_detail_and_foreign_sessions() {
        let status_json = serde_json::json!({
            "state": "recording",
            "session": {"session_id": "the-desk", "state": "recording", "mode": "toggle"},
            "daemon": {"name": "dictated", "version": "0", "protocol_version": 1, "pid": 42},
            "capabilities": {},
            "audio": {"capture_enabled": true, "input_open": true},
        });
        let mut status: Status = serde_json::from_value(status_json).unwrap();
        scrub_for_network(&mut status, &VecDeque::new());
        assert_eq!(status.daemon.pid, None);
        assert!(status.audio.is_none());
        assert!(status.formatter.is_none());
        assert!(status.session.is_none());
        assert_eq!(
            status.state,
            State::Recording,
            "state stays: it explains busy"
        );

        let mut own: Status = serde_json::from_value(serde_json::json!({
            "session": {"session_id": "mine", "state": "transcribing"},
            "daemon": {"name": "dictated", "version": "0", "protocol_version": 1},
        }))
        .unwrap();
        scrub_for_network(&mut own, &[SessionId("mine".into())].into_iter().collect());
        assert!(own.session.is_some(), "its own session is its business");
    }
}
