//! The protocol bridge: one long-lived connection to `dictated`.
//!
//! # Shape
//!
//! [`Bridge::new`] returns a cheap, cloneable handle and the future that owns
//! the socket. The caller spawns the future on whatever runtime it has (Tauri's
//! in the app, `#[tokio::test]`'s in tests), which keeps this module free of
//! any Tauri type and lets the unit tests drive it against a stub daemon.
//!
//! The connection task owns both halves of the socket and `select!`s over
//! requests, incoming lines, the audio-level flush timer and request
//! deadlines — the same single-owner design `dictated`'s server uses, so
//! writes are sequential by construction and no lock is held across an await.
//!
//! # Contract with the daemon
//!
//! - Handshake as [`ClientKind::DesktopUi`], then subscribe to every event.
//! - Events are forwarded **raw** (the JSON object the daemon sent), never
//!   re-serialized from a parsed [`dictate_proto::Event`]: the crate docs
//!   require a relay to preserve events this build does not understand.
//! - `audio_level` is coalesced to at most one per [`BridgeConfig::level_interval`]
//!   (~30 fps), keeping the loudest sample of each window so a meter never
//!   misses a peak.
//! - While the daemon is unreachable every request fails immediately with
//!   `daemon_unavailable` — never queued, never left hanging — and the
//!   connection is retried with capped exponential backoff.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use dictate_proto::{
    ClientInfo, ClientKind, Command, CommandResult, Features, Hello, Message, RequestId,
};
use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot, watch, Notify};
use tokio::time::Instant;

/// Environment variable that overrides the socket location (shared with the
/// `dictate` CLI).
pub const SOCKET_ENV: &str = "DICTATE_SOCKET";

/// The daemon's control socket, resolved exactly as `dictate` and `dictated`
/// resolve it: `$DICTATE_SOCKET`, else
/// `$XDG_RUNTIME_DIR/dictate-agent/dictated.sock`.
#[must_use]
pub fn socket_path() -> PathBuf {
    if let Some(explicit) = std::env::var_os(SOCKET_ENV) {
        return PathBuf::from(explicit);
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let uid = std::env::var("UID").unwrap_or_else(|_| "user".into());
            PathBuf::from(format!("/tmp/dictate-agent-{uid}"))
        });
    runtime.join("dictate-agent").join("dictated.sock")
}

/// Tunables. The defaults are what the app uses; tests shorten them.
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    /// The daemon's socket.
    pub socket: PathBuf,
    /// First reconnect delay.
    pub backoff_min: Duration,
    /// Ceiling for the reconnect delay.
    pub backoff_max: Duration,
    /// Handshake deadline.
    pub handshake_timeout: Duration,
    /// Default per-request deadline.
    pub request_timeout: Duration,
    /// Minimum spacing of forwarded `audio_level` events.
    pub level_interval: Duration,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            socket: socket_path(),
            backoff_min: Duration::from_millis(250),
            backoff_max: Duration::from_secs(5),
            handshake_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(30),
            level_interval: Duration::from_millis(33),
        }
    }
}

/// The connection as the UI should present it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Connection {
    /// Trying to reach the daemon.
    Connecting {
        /// Consecutive attempts, from 1.
        attempt: u32,
    },
    /// Handshaken and subscribed.
    Connected {
        /// Server name (`dictated`).
        server: String,
        /// Server version.
        version: String,
        /// Negotiated protocol version.
        protocol_version: u16,
        /// What this connection may do.
        features: Box<Features>,
    },
    /// The daemon is not reachable; the bridge will try again.
    Disconnected {
        /// Why, in words a user can act on.
        reason: String,
        /// Delay before the next attempt.
        retry_in_ms: u64,
        /// Consecutive failed attempts.
        attempt: u32,
    },
}

impl Connection {
    /// Whether requests can currently be sent.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected { .. })
    }
}

/// A request that did not produce a result. Serialized to the webview as the
/// rejection value of a Tauri command.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BridgeError {
    /// A protocol [`dictate_proto::ErrorCode`] string, or one of the bridge's
    /// own: `daemon_unavailable`, `timeout`, `connection_lost`.
    pub code: String,
    /// Human-readable.
    pub message: String,
    /// Code-specific detail from the daemon (e.g. `config_invalid`'s path).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
}

impl BridgeError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            detail: None,
        }
    }

    fn unavailable(reason: &str) -> Self {
        Self::new("daemon_unavailable", format!("dictated is not running: {reason}"))
    }
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for BridgeError {}

/// Where the bridge delivers what the daemon says.
pub trait EventSink: Send + Sync + 'static {
    /// The connection changed state.
    fn connection(&self, state: &Connection);
    /// A daemon event, exactly as the daemon sent it (`audio_level` already
    /// coalesced).
    fn event(&self, event: &Value);
}

struct Pending {
    command: Command,
    timeout: Duration,
    reply: oneshot::Sender<Result<Value, BridgeError>>,
}

/// A handle to the bridge. Cheap to clone.
#[derive(Clone)]
pub struct Bridge {
    tx: mpsc::Sender<Pending>,
    state: watch::Receiver<Connection>,
    wake: Arc<Notify>,
    default_timeout: Duration,
}

impl Bridge {
    /// Create the bridge. Spawn the returned future to run it; it ends when
    /// every [`Bridge`] handle has been dropped.
    pub fn new(
        config: BridgeConfig,
        sink: Arc<dyn EventSink>,
    ) -> (Self, impl std::future::Future<Output = ()> + Send + 'static) {
        let (tx, rx) = mpsc::channel(64);
        let (state_tx, state_rx) = watch::channel(Connection::Connecting { attempt: 1 });
        let wake = Arc::new(Notify::new());
        let bridge = Self {
            tx,
            state: state_rx,
            wake: wake.clone(),
            default_timeout: config.request_timeout,
        };
        let task = run(config, sink, rx, state_tx, wake);
        (bridge, task)
    }

    /// The connection's current state.
    #[must_use]
    pub fn connection(&self) -> Connection {
        self.state.borrow().clone()
    }

    /// Skip the rest of the current backoff and try to connect now.
    pub fn reconnect_now(&self) {
        self.wake.notify_one();
    }

    /// Send a command and return the daemon's result as raw JSON.
    ///
    /// # Errors
    ///
    /// The daemon's own error, or `daemon_unavailable` / `timeout` /
    /// `connection_lost`.
    pub async fn request(&self, command: Command) -> Result<Value, BridgeError> {
        self.request_within(command, self.default_timeout).await
    }

    /// As [`Bridge::request`], with an explicit deadline (e.g. a full
    /// `diagnose`, which hashes the model file).
    ///
    /// # Errors
    ///
    /// As [`Bridge::request`].
    pub async fn request_within(
        &self,
        command: Command,
        timeout: Duration,
    ) -> Result<Value, BridgeError> {
        let (reply, answer) = oneshot::channel();
        self.tx
            .send(Pending {
                command,
                timeout,
                reply,
            })
            .await
            .map_err(|_| BridgeError::new("connection_lost", "the bridge has shut down"))?;
        answer
            .await
            .map_err(|_| BridgeError::new("connection_lost", "the bridge has shut down"))?
    }

    /// As [`Bridge::request`], decoded into a typed result for Rust callers.
    ///
    /// # Errors
    ///
    /// As [`Bridge::request`], or `malformed_response`.
    pub async fn request_typed(&self, command: Command) -> Result<CommandResult, BridgeError> {
        let value = self.request(command).await?;
        serde_json::from_value(value)
            .map_err(|e| BridgeError::new("malformed_response", e.to_string()))
    }
}

/// Coalesces `audio_level` events to a fixed maximum rate.
///
/// Pure (time is passed in), so its behavior is unit-tested without a clock.
#[derive(Debug)]
pub struct LevelThrottle {
    interval: Duration,
    last_emit: Option<Instant>,
    pending: Option<Value>,
}

impl LevelThrottle {
    /// At most one event per `interval`.
    #[must_use]
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last_emit: None,
            pending: None,
        }
    }

    /// Offer an event; returns the event to forward now, if any.
    pub fn offer(&mut self, now: Instant, event: Value) -> Option<Value> {
        let merged = match self.pending.take() {
            Some(held) => louder(held, event),
            None => event,
        };
        if self
            .last_emit
            .is_none_or(|last| now.duration_since(last) >= self.interval)
        {
            self.last_emit = Some(now);
            Some(merged)
        } else {
            self.pending = Some(merged);
            None
        }
    }

    /// When a held event becomes due, if one is held.
    #[must_use]
    pub fn due_at(&self) -> Option<Instant> {
        self.pending.as_ref()?;
        Some(self.last_emit.map_or_else(Instant::now, |t| t + self.interval))
    }

    /// Release the held event if it is due.
    pub fn flush(&mut self, now: Instant) -> Option<Value> {
        if self.due_at().is_some_and(|due| now >= due) {
            self.last_emit = Some(now);
            return self.pending.take();
        }
        None
    }

    /// Drop a held event (the session moved on; a stale level is noise).
    pub fn discard(&mut self) {
        self.pending = None;
    }
}

/// Keep the newer event's identity with the louder of the two readings.
fn louder(held: Value, mut newer: Value) -> Value {
    for key in ["rms", "peak"] {
        let a = held.get(key).and_then(Value::as_f64);
        let b = newer.get(key).and_then(Value::as_f64);
        if let (Some(a), Some(obj)) = (a, newer.as_object_mut()) {
            if b.is_none_or(|b| a > b) {
                obj.insert(key.into(), held[key].clone());
            }
        }
    }
    newer
}

/// Capped exponential backoff: `min · 2^(attempt-1)`, at most `max`.
#[must_use]
pub fn backoff(attempt: u32, min: Duration, max: Duration) -> Duration {
    let factor = 1u32 << attempt.saturating_sub(1).min(16);
    min.saturating_mul(factor).min(max)
}

enum Ended {
    /// Every handle was dropped: stop for good.
    Shutdown,
    /// The connection failed; reconnect.
    Lost(String),
}

async fn run(
    config: BridgeConfig,
    sink: Arc<dyn EventSink>,
    mut rx: mpsc::Receiver<Pending>,
    state: watch::Sender<Connection>,
    wake: Arc<Notify>,
) {
    let publish = |s: Connection| {
        sink.connection(&s);
        let _ = state.send(s);
    };
    let mut failures: u32 = 0;
    loop {
        publish(Connection::Connecting {
            attempt: failures + 1,
        });
        let reason = match connect(&config).await {
            Ok((stream, hello)) => {
                failures = 0;
                publish(Connection::Connected {
                    server: hello.server.name.clone(),
                    version: hello.server.version.clone(),
                    protocol_version: hello.protocol_version,
                    features: Box::new(hello.capabilities.features.clone()),
                });
                match serve(stream, &mut rx, sink.as_ref(), &config).await {
                    Ended::Shutdown => return,
                    Ended::Lost(reason) => reason,
                }
            }
            Err(reason) => reason,
        };
        failures += 1;
        let delay = backoff(failures, config.backoff_min, config.backoff_max);
        publish(Connection::Disconnected {
            reason: reason.clone(),
            retry_in_ms: u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
            attempt: failures,
        });
        let sleep = tokio::time::sleep(delay);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                () = &mut sleep => break,
                () = wake.notified() => break,
                pending = rx.recv() => match pending {
                    None => return,
                    Some(p) => {
                        let _ = p.reply.send(Err(BridgeError::unavailable(&reason)));
                    }
                },
            }
        }
    }
}

type Reader = tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>;
type Writer = tokio::net::unix::OwnedWriteHalf;

async fn connect(
    config: &BridgeConfig,
) -> Result<((Reader, Writer), dictate_proto::ServerHello), String> {
    let stream = UnixStream::connect(&config.socket).await.map_err(|e| {
        match e.kind() {
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
                format!("no daemon listening on {}", config.socket.display())
            }
            _ => format!("cannot reach {}: {e}", config.socket.display()),
        }
    })?;
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();

    let mut info = ClientInfo::new("dictate-ui", ClientKind::DesktopUi);
    info.version = Some(env!("CARGO_PKG_VERSION").into());
    let handshake = async {
        send(&mut write, 0, Command::Handshake(Hello::new(info))).await?;
        let hello = match await_response(&mut lines, 0).await? {
            Ok(CommandResult::Handshake(hello)) => *hello,
            Ok(other) => return Err(format!("unexpected handshake answer: {other:?}")),
            Err(e) => return Err(format!("handshake refused: {e}")),
        };
        send(&mut write, 1, Command::Subscribe { events: Vec::new() }).await?;
        match await_response(&mut lines, 1).await? {
            Ok(_) => Ok(hello),
            Err(e) => Err(format!("subscribe refused: {e}")),
        }
    };
    let hello = tokio::time::timeout(config.handshake_timeout, handshake)
        .await
        .map_err(|_| "the daemon did not answer the handshake".to_string())??;
    Ok(((lines, write), hello))
}

async fn send(write: &mut Writer, id: u64, command: Command) -> Result<(), String> {
    let line = Message::request(RequestId::Number(id), command)
        .to_ndjson_line()
        .map_err(|e| e.to_string())?;
    write
        .write_all(line.as_bytes())
        .await
        .map_err(|e| format!("write failed: {e}"))
}

/// Read until the response to `id` arrives. Only used during the handshake,
/// before the subscription exists, so nothing else can arrive meanwhile.
async fn await_response(
    lines: &mut Reader,
    id: u64,
) -> Result<Result<CommandResult, dictate_proto::ProtoError>, String> {
    loop {
        let line = lines
            .next_line()
            .await
            .map_err(|e| e.to_string())?
            .ok_or("the daemon closed the connection during the handshake")?;
        if let Ok(Message::Response(r)) = Message::parse(&line) {
            if r.id == RequestId::Number(id) {
                return Ok(match r.outcome {
                    dictate_proto::Outcome::Result(v) => Ok(v),
                    dictate_proto::Outcome::Error(e) => Err(e),
                });
            }
        }
    }
}

struct InFlight {
    reply: oneshot::Sender<Result<Value, BridgeError>>,
    deadline: Instant,
}

async fn serve(
    (mut lines, mut write): (Reader, Writer),
    rx: &mut mpsc::Receiver<Pending>,
    sink: &dyn EventSink,
    config: &BridgeConfig,
) -> Ended {
    let mut in_flight: HashMap<u64, InFlight> = HashMap::new();
    let mut next_id: u64 = 2;
    let mut level = LevelThrottle::new(config.level_interval);
    let far = || Instant::now() + Duration::from_secs(3600);

    let ended = loop {
        let level_due = level.due_at().unwrap_or_else(far);
        let request_due = in_flight
            .values()
            .map(|f| f.deadline)
            .min()
            .unwrap_or_else(far);
        tokio::select! {
            pending = rx.recv() => {
                let Some(p) = pending else { break Ended::Shutdown };
                let id = next_id;
                next_id += 1;
                if let Err(e) = send(&mut write, id, p.command).await {
                    let _ = p.reply.send(Err(BridgeError::new("connection_lost", e.clone())));
                    break Ended::Lost(e);
                }
                in_flight.insert(id, InFlight { reply: p.reply, deadline: Instant::now() + p.timeout });
            }
            line = lines.next_line() => match line {
                Ok(Some(line)) => route(&line, &mut in_flight, &mut level, sink),
                Ok(None) => break Ended::Lost("the daemon closed the connection".into()),
                Err(e) => break Ended::Lost(format!("read failed: {e}")),
            },
            () = tokio::time::sleep_until(level_due) => {
                if let Some(event) = level.flush(Instant::now()) {
                    sink.event(&event);
                }
            }
            () = tokio::time::sleep_until(request_due) => {
                let now = Instant::now();
                let expired: Vec<u64> = in_flight
                    .iter()
                    .filter(|(_, f)| f.deadline <= now)
                    .map(|(id, _)| *id)
                    .collect();
                for id in expired {
                    if let Some(f) = in_flight.remove(&id) {
                        let _ = f.reply.send(Err(BridgeError::new(
                            "timeout",
                            "the daemon did not answer in time",
                        )));
                    }
                }
            }
        }
    };
    for (_, f) in in_flight.drain() {
        let _ = f.reply.send(Err(BridgeError::new(
            "connection_lost",
            "the connection to dictated was lost before it answered",
        )));
    }
    ended
}

/// Dispatch one line from the daemon.
fn route(
    line: &str,
    in_flight: &mut HashMap<u64, InFlight>,
    level: &mut LevelThrottle,
    sink: &dyn EventSink,
) {
    let Ok(mut message) = serde_json::from_str::<Value>(line) else {
        return;
    };
    match message.get("kind").and_then(Value::as_str) {
        Some("response") => {
            let Some(id) = message.get("id").and_then(Value::as_u64) else {
                return;
            };
            let Some(f) = in_flight.remove(&id) else {
                return;
            };
            let outcome = if let Some(result) = message.get_mut("result") {
                Ok(result.take())
            } else {
                let error = message.get("error").cloned().unwrap_or(Value::Null);
                Err(BridgeError {
                    code: error
                        .get("code")
                        .and_then(Value::as_str)
                        .unwrap_or("internal")
                        .into(),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("the daemon returned an error without a message")
                        .into(),
                    detail: error.get("detail").cloned(),
                })
            };
            let _ = f.reply.send(outcome);
        }
        Some("event") => {
            let Some(event) = message.get_mut("event").map(Value::take) else {
                return;
            };
            if event.get("type").and_then(Value::as_str) == Some("audio_level") {
                if let Some(event) = level.offer(Instant::now(), event) {
                    sink.event(&event);
                }
            } else {
                level.discard();
                sink.event(&event);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lvl(rms: f64) -> Value {
        json!({"type": "audio_level", "session_id": "s", "rms": rms})
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let (min, max) = (Duration::from_millis(250), Duration::from_secs(5));
        assert_eq!(backoff(1, min, max), Duration::from_millis(250));
        assert_eq!(backoff(2, min, max), Duration::from_millis(500));
        assert_eq!(backoff(4, min, max), Duration::from_secs(2));
        assert_eq!(backoff(9, min, max), max);
        assert_eq!(backoff(u32::MAX, min, max), max, "no overflow");
    }

    #[test]
    fn the_first_level_passes_and_a_burst_is_coalesced_to_its_loudest() {
        let t0 = Instant::now();
        let mut th = LevelThrottle::new(Duration::from_millis(33));
        assert_eq!(th.offer(t0, lvl(0.1)), Some(lvl(0.1)));
        // Three more inside the window: none forwarded yet.
        assert_eq!(th.offer(t0 + Duration::from_millis(5), lvl(0.2)), None);
        assert_eq!(th.offer(t0 + Duration::from_millis(10), lvl(0.9)), None);
        assert_eq!(th.offer(t0 + Duration::from_millis(15), lvl(0.3)), None);
        assert_eq!(th.due_at(), Some(t0 + Duration::from_millis(33)));
        assert_eq!(th.flush(t0 + Duration::from_millis(20)), None, "not due yet");
        let out = th.flush(t0 + Duration::from_millis(33)).expect("due");
        assert_eq!(out["rms"], json!(0.9), "the peak of the window survives");
        assert_eq!(th.due_at(), None);
    }

    #[test]
    fn levels_never_exceed_the_rate() {
        let t0 = Instant::now();
        let mut th = LevelThrottle::new(Duration::from_millis(33));
        let mut forwarded = 0;
        // 1 s of events at 200 Hz.
        for i in 0..200u64 {
            let now = t0 + Duration::from_millis(i * 5);
            forwarded += usize::from(th.flush(now).is_some());
            forwarded += usize::from(th.offer(now, lvl(0.5)).is_some());
        }
        assert!((28..=31).contains(&forwarded), "{forwarded} per second");
    }

    #[test]
    fn a_discarded_level_is_never_forwarded() {
        let t0 = Instant::now();
        let mut th = LevelThrottle::new(Duration::from_millis(33));
        th.offer(t0, lvl(0.1));
        th.offer(t0 + Duration::from_millis(1), lvl(0.2));
        th.discard();
        assert_eq!(th.flush(t0 + Duration::from_secs(1)), None);
    }

    #[test]
    fn the_socket_path_honors_the_override() {
        // Serialized with other env tests by being the only one in this crate.
        std::env::set_var(SOCKET_ENV, "/tmp/x/custom.sock");
        assert_eq!(socket_path(), PathBuf::from("/tmp/x/custom.sock"));
        std::env::remove_var(SOCKET_ENV);
        std::env::set_var("XDG_RUNTIME_DIR", "/run/user/4242");
        assert_eq!(
            socket_path(),
            PathBuf::from("/run/user/4242/dictate-agent/dictated.sock")
        );
    }
}
